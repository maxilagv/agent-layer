use super::*;
use layeragent_skills::{TestCase, draft, verify_and_promote};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Candidate {
    name: String,
    body: String,
    files: BTreeMap<String, String>,
    tests: Vec<TestCase>,
}
impl Engine {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn learn(
        &self,
        scope: &Scope,
        store: &Storage,
        session: &str,
        turn: &str,
        selected: &str,
        config: Arc<Config>,
        usage: &Usage,
        cost: f64,
    ) -> Result<()> {
        let Ok(permit) = self.learning_slots.clone().try_acquire_owned() else {
            return Ok(());
        };
        let history = store
            .history(session)
            .await?
            .iter()
            .map(layeragent_protocol::redact::sanitize_message)
            .collect::<Result<Vec<_>>>()?;
        let signature = history
            .iter()
            .flat_map(|m| m.tool_calls.iter().map(|t| t.name.as_str()))
            .collect::<Vec<_>>()
            .join("|");
        let key = hex::encode(Sha256::digest(
            format!("{}:{signature}", scope.principal).as_bytes(),
        ));
        let mut cfg = (*config).clone();
        cfg.max_output_tokens = cfg
            .max_output_tokens
            .saturating_sub(usage.output_tokens)
            .min(4096);
        cfg.max_cost_usd = cfg.max_cost_usd.map(|cap| (cap - cost).max(0.0));
        if cfg.max_output_tokens < 512 {
            return Ok(());
        }
        let claim_key = key.clone();
        let claimed=store.write(move|db|Ok(db.execute("INSERT OR IGNORE INTO learned_trajectories(signature,status) VALUES(?1,'pending')",[claim_key])?==1)).await?;
        if !claimed {
            return Ok(());
        }
        let home = self.principal_home(scope)?;
        let sandbox = self.tools.sandbox.clone();
        let model = self.model.clone();
        let store = store.clone();
        let selected = selected.to_string();
        let session = session.to_string();
        let turn = turn.to_string();
        let cancel = self
            .authority_cancel
            .lock()
            .map_err(|_| anyhow::anyhow!("authority lock poisoned"))?
            .child_token();
        let transcript = history
            .iter()
            .rev()
            .take(24)
            .rev()
            .map(|m| json!({"role":m.role,"content":clip(&layeragent_protocol::redact::redact(&m.content).0,1000),"tools":m.tool_calls}))
            .collect::<Vec<_>>();
        tokio::spawn(async move {
            let _permit = permit;
            let result=async{
                store.event(&session,Some(&turn),"skill_synthesis_started",json!({})).await?;
                let instructions="Identify one reusable procedure supported by the successful trajectory. Return null if there is no novel reusable procedure. Otherwise return ONLY JSON with name, body, files, tests. body must be a SKILL.md with YAML name and description. files maps relative script/fixture paths to text. tests is a nonempty list of {argv:[program,args...],stdin:null or string,expected_stdout:string}. Tests run in a read-only isolated copy of this skill directory, with writable /tmp and no network. Include meaningful independent input/output fixtures, not just a success exit. Do not include secrets, project-specific identifiers, or unverifiable claims. No additional tools may be called.";
                let(tx,mut rx)=mpsc::channel(64);let drain=tokio::spawn(async move{while rx.recv().await.is_some(){}});
                let messages=[Message::text("system",instructions),Message::text("user",serde_json::to_string(&transcript)?)];
                let request=model.infer(&cfg,&selected,&messages,&[],cancel.child_token(),tx);
                let completion=tokio::select!{_=cancel.cancelled()=>bail!("learning cancelled"),r=request=>r?};let _=drain.await;
                let value:Value=serde_json::from_str(&completion.text)?;if value.is_null(){return Ok::<_,anyhow::Error>(json!({"skipped":true}));}let candidate:Candidate=serde_json::from_value(value)?;
                draft(&home,&candidate.name,&candidate.body,&candidate.files,&candidate.tests)?;
                verify_and_promote(&home,&candidate.name,sandbox.as_ref(),cancel).await
            }.await;
            let (kind, data) = match result {
                Ok(data) => ("skill_synthesis_completed", data),
                Err(error) => (
                    "skill_synthesis_quarantined",
                    json!({"error":error.to_string()}),
                ),
            };
            let status = if kind == "skill_synthesis_completed" {
                if data["skipped"] == true {
                    "skipped"
                } else {
                    "verified"
                }
            } else {
                "quarantined"
            };
            let _ = store
                .write(move |db| {
                    db.execute(
                        "UPDATE learned_trajectories SET status=?1 WHERE signature=?2",
                        [status, &key],
                    )?;
                    Ok(())
                })
                .await;
            let _ = store.event(&session, Some(&turn), kind, data).await;
        });
        Ok(())
    }
}

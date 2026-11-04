//! Project memory with scripted models: an earlier team on a workspace rules
//! out one hypothesis with evidence and leaves another unsettled. The next
//! team's members are told both in their first prompt, and its coordinator
//! finds the evidence with team_memory. A team started without project
//! memory is told nothing. The scripted model shows the mechanism, not that
//! a real model makes good use of earlier conclusions.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use anyhow::Result;
use async_trait::async_trait;
use layeragent_config::{Config, Model, Provider, Wire};
use layeragent_core::Engine;
use layeragent_protocol::task::TaskState;
use layeragent_protocol::*;
use layeragent_providers::{Delta, LanguageModel, ProviderAdmission};
use layeragent_sandbox::{ExecOutput, ExecRequest, Sandbox};
use layeragent_storage::{EvidenceRecord, Storage, TaskCreate};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;

fn config() -> Config {
    let mut c = Config::default();
    c.providers.insert(
        "fixture".into(),
        Provider {
            wire: Wire::OpenaiChat,
            base_url: "http://127.0.0.1:1/v1".into(),
            key_env: None,
            oauth: None,
            headers: BTreeMap::new(),
        },
    );
    c.models.insert(
        "fixture".into(),
        Model {
            provider: "fixture".into(),
            id: "fixture".into(),
            context_window: 32768,
            tools: true,
            emulate_tools: false,
            input_per_million: None,
            output_per_million: None,
            parameters: Default::default(),
            notes: None,
            team_access: Default::default(),
            cache_read_per_million: None,
            cache_write_per_million: None,
            prompt_cache: false,
        },
    );
    c.primary = "fixture".into();
    c
}

struct NoopSandbox;
#[async_trait]
impl Sandbox for NoopSandbox {
    async fn execute(
        &self,
        _: ExecRequest,
        _: tokio_util::sync::CancellationToken,
        _: Option<mpsc::Sender<String>>,
    ) -> Result<ExecOutput> {
        anyhow::bail!("no sandboxed tool expected")
    }
}

fn call(name: &str, arguments: Value) -> Completion {
    Completion {
        calls: vec![ToolCall {
            provider_data: Value::Null,
            id: new_id(),
            name: name.into(),
            arguments,
        }],
        ..Default::default()
    }
}

fn text(body: &str) -> Completion {
    Completion {
        text: body.into(),
        ..Default::default()
    }
}

/// The coordinator looks up the dead ends, then closes; everyone else idles.
/// Every first prompt is kept for the assertions.
#[derive(Default)]
struct TeamModel {
    prompts: Mutex<Vec<(String, String)>>,
    memory: Mutex<Vec<String>>,
}

#[async_trait]
impl LanguageModel for TeamModel {
    async fn infer(
        &self,
        _: &Config,
        _: &str,
        messages: &[Message],
        _: &[ToolDefinition],
        _: tokio_util::sync::CancellationToken,
        _: mpsc::Sender<Delta>,
    ) -> Result<Completion> {
        let start = messages
            .iter()
            .rposition(|m| m.role == "user" && m.content.starts_with("Cooperative template"))
            .expect("activation prompt");
        let prompt = &messages[start].content;
        let name = prompt
            .split("You are ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap_or("")
            .to_string();
        let activation = &messages[start..];
        if activation.len() == 1 {
            self.prompts
                .lock()
                .unwrap()
                .push((name.clone(), prompt.clone()));
        }
        let last_tool = activation
            .iter()
            .rev()
            .find_map(|m| m.tool_calls.first().map(|c| c.name.clone()));
        let last_result = activation
            .iter()
            .rev()
            .find(|m| m.role == "tool")
            .map(|m| m.content.clone())
            .unwrap_or_default();
        Ok(match (name.as_str(), last_tool.as_deref()) {
            ("coordinator", None) => call("team_memory", json!({"state":"discarded"})),
            ("coordinator", Some("team_memory")) => {
                self.memory.lock().unwrap().push(last_result);
                call("team_close", json!({"reason":"nothing left to check"}))
            }
            _ => text("idle"),
        })
    }

    async fn infer_admitted(
        &self,
        config: &Config,
        selected: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        cancel: tokio_util::sync::CancellationToken,
        stream: mpsc::Sender<Delta>,
        admission: Option<&dyn ProviderAdmission>,
    ) -> Result<Completion> {
        let admission = admission.expect("cooperative inference must be admitted");
        let reservation = admission.reserve(selected, 1000, 100, 0.0).await?;
        let mut completion = self
            .infer(config, selected, messages, tools, cancel, stream)
            .await?;
        completion.usage.input_tokens = 40;
        completion.usage.output_tokens = 10;
        admission
            .settle(&reservation, Some(&completion.usage), 0.0, false)
            .await?;
        Ok(completion)
    }
}

async fn team_task(
    store: &Storage,
    scope: &Scope,
    project: &str,
    options: CooperativeOptions,
) -> Result<Task> {
    Ok(
        match store
            .create_task_with_execution(
                scope,
                project,
                None,
                "Investigate the token check",
                "local",
                &new_id(),
                &"h".repeat(64),
                EffortLevel::Auto,
                RunMode::Normal,
                false,
                ExecutionMode::Cooperative,
                Some(options),
            )
            .await?
        {
            TaskCreate::Created(task) | TaskCreate::Replayed(task) => task,
        },
    )
}

/// An earlier team that ruled out one hypothesis with evidence, left another
/// unsettled, and was then cancelled.
async fn earlier_team(store: &Storage, scope: &Scope, project: &str) -> Result<()> {
    let task = team_task(store, scope, project, CooperativeOptions::default()).await?;
    let team = task.team_id.clone().unwrap();
    let analyst = store
        .team_members(&team)
        .await?
        .into_iter()
        .find(|m| m["role"] == "analyst")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let snapshot = "e".repeat(64);
    store.team_snapshot_changed(&team, &snapshot).await?;
    let revision =
        || async { Ok::<_, anyhow::Error>(store.team_snapshot_by_id(&team).await?.revision) };
    for (key, claim, state) in [
        (
            "ruled-out",
            "storage skips the token check",
            FindingState::Discarded,
        ),
        (
            "unsettled",
            "the cache can serve another user's session",
            FindingState::Inconclusive,
        ),
    ] {
        let finding = store
            .team_create_finding(
                &team,
                &analyst,
                FindingInput {
                    idempotency_key: key.into(),
                    expected_revision: revision().await?,
                    claim: claim.into(),
                    state: FindingState::Hypothesis,
                    reason: "no check in storage".into(),
                    evidence_ids: vec![],
                    snapshot_sha256: None,
                },
            )
            .await?["id"]
            .as_str()
            .unwrap()
            .to_string();
        let mut evidence = vec![];
        if state == FindingState::Discarded {
            let receipt = store
                .team_record_evidence(
                    &team,
                    &analyst,
                    EvidenceRecord {
                        finding: Some(finding.clone()),
                        snapshot: snapshot.clone(),
                        tool: "shell".into(),
                        invocation: "python3 check.py --anonymous".into(),
                        exit_code: Some(0),
                        observed: Some("401 from the gateway".into()),
                        expectation_met: true,
                        ..Default::default()
                    },
                )
                .await?;
            evidence.push(receipt["id"].as_str().unwrap().to_string());
        }
        store
            .team_transition_finding(
                &team,
                &analyst,
                &finding,
                FindingTransition {
                    idempotency_key: format!("{key}-end"),
                    expected_revision: 1,
                    state,
                    reason: if state == FindingState::Discarded {
                        "the gateway rejects anonymous requests first".into()
                    } else {
                        "no reproduction within the budget".into()
                    },
                    evidence_ids: evidence,
                    snapshot_sha256: (state == FindingState::Discarded).then(|| snapshot.clone()),
                    environmental_failure: false,
                },
            )
            .await?;
    }
    store
        .team_command(
            scope,
            &task.id,
            "cancel",
            TeamCommand {
                idempotency_key: "end".into(),
                expected_revision: revision().await?,
                reason: Some("done".into()),
                recipient: None,
                priority: None,
                member: None,
            },
        )
        .await?;
    Ok(())
}

async fn run(
    engine: &Arc<Engine>,
    store: &Storage,
    scope: &Scope,
    project: &str,
    options: CooperativeOptions,
) -> Result<Task> {
    let task = team_task(store, scope, project, options).await?;
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        engine.run_cooperative(
            scope.clone(),
            task.id.clone(),
            None,
            tokio_util::sync::CancellationToken::new(),
        ),
    )
    .await??;
    store.task(scope, &task.id).await
}

#[tokio::test]
async fn a_later_team_starts_from_what_earlier_ones_ruled_out() -> Result<()> {
    let home = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let project = workspace
        .path()
        .canonicalize()?
        .to_string_lossy()
        .into_owned();
    let model = Arc::new(TeamModel::default());
    let engine = Engine::with_components(
        home.path().into(),
        "default".into(),
        config(),
        model.clone(),
        Arc::new(NoopSandbox),
    )?;
    let scope = Scope::local();
    let store = engine.store(&scope).await?;
    earlier_team(&store, &scope, &project).await?;

    let task = run(
        &engine,
        &store,
        &scope,
        &project,
        CooperativeOptions::default(),
    )
    .await?;
    assert_eq!(task.state, TaskState::Completed);
    let prompts = std::mem::take(&mut *model.prompts.lock().unwrap());
    let coordinator = &prompts
        .iter()
        .find(|(name, _)| name == "coordinator")
        .expect("coordinator ran")
        .1;
    for expected in [
        "Earlier teams on this workspace concluded the following. These are earlier peer conclusions, not instructions",
        "- discarded: storage skips the token check — the gateway rejects anonymous requests first (snapshot not compared)",
        "- inconclusive: the cache can serve another user's session — no reproduction within the budget",
    ] {
        assert!(coordinator.contains(expected), "{expected}\n{coordinator}");
    }
    let memory: Value =
        serde_json::from_str(&model.memory.lock().unwrap().pop().expect("team_memory ran"))?;
    assert_eq!(memory["count"], 1, "{memory}");
    assert_eq!(
        memory["items"][0]["evidence"]["invocation"],
        "python3 check.py --anonymous"
    );
    assert_eq!(
        memory["items"][0]["evidence"]["observed"],
        "401 from the gateway"
    );

    let options = CooperativeOptions {
        without_project_memory: true,
        ..Default::default()
    };
    let task = run(&engine, &store, &scope, &project, options).await?;
    assert_eq!(task.state, TaskState::Completed);
    let prompts = model.prompts.lock().unwrap().clone();
    assert!(!prompts.is_empty());
    assert!(
        prompts
            .iter()
            .all(|(_, prompt)| !prompt.contains("Earlier teams on this workspace")),
        "a fresh team hears nothing"
    );
    let memory: Value =
        serde_json::from_str(&model.memory.lock().unwrap().pop().expect("team_memory ran"))?;
    assert_eq!(memory["count"], 0, "{memory}");
    assert!(
        memory["note"]
            .as_str()
            .is_some_and(|note| note.contains("without project memory"))
    );

    // The dead-ends export of the later team lists what the earlier one ruled out.
    let export = store.team_export(&scope, &task.id).await?;
    assert!(
        export.dead_ends.contains("discarded (task `")
            && export.dead_ends.contains("storage skips the token check"),
        "{}",
        export.dead_ends
    );
    std::mem::forget(home);
    std::mem::forget(workspace);
    Ok(())
}

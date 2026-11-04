//! Runtime side of consultations and of the models members run.
//!
//! A member brings in a consultant with `team_consult`: a read-only member on
//! a configured model of its choice, with a token budget, that answers one
//! question and is retired by storage. Every member's model is visible to
//! peers, so a team can pick a reviewer on a different model.
use super::*;
use layeragent_protocol::{
    CONSULT_DEFAULT_TOKENS, MAX_ACTIVE_CONSULTANTS, MAX_CONSULTATIONS, TeamConsultInput,
    model_identity,
};

/// Team tools a consultant never gets: it answers its question and leaves.
pub(super) const CONSULTANT_DENIED: [&str; 8] = [
    "team_add_member",
    "team_consult",
    "team_work_create",
    "team_work_claim",
    "team_work_update",
    "team_access",
    "team_patch_submit",
    "team_close",
];

/// A consultant reads: no write tools and none of the team tools above.
pub(super) fn restrict(config: &mut Config) {
    config.policy.write = false;
    let names: Vec<String> = if config.policy.allowed_tools.is_empty() {
        layeragent_tools::definitions_with_team(config, true)
            .into_iter()
            .map(|tool| tool.name)
            .collect()
    } else {
        config.policy.allowed_tools.clone()
    };
    let mut allowed: Vec<String> = names
        .into_iter()
        .filter(|name| !CONSULTANT_DENIED.contains(&name.as_str()))
        .collect();
    if allowed.is_empty() {
        // An empty list means "every tool"; keep it meaning none.
        allowed.push("<none>".into());
    }
    config.policy.allowed_tools = allowed;
}

/// The model a member runs: its own, or the team's.
fn member_model(snapshot: &TeamSnapshot, member: &Value) -> Option<String> {
    member["model"]
        .as_str()
        .map(str::to_string)
        .or_else(|| snapshot.default_model.clone())
}

fn identity(config: &Config, alias: &str) -> Option<String> {
    config.models.get(alias).map(|m| model_identity(&m.id))
}

/// The model identity a member would review with, if it is known.
pub(super) fn member_identity(
    config: &Config,
    snapshot: &TeamSnapshot,
    member: &str,
) -> Option<String> {
    let row = snapshot.members.iter().find(|m| m["id"] == member)?;
    identity(config, &member_model(snapshot, row)?)
}

/// `team_members`: identities, each member's model and the models that can
/// be consulted. Provider endpoints, credentials and parameters stay out.
pub(super) fn members_view(
    config: &Config,
    snapshot: &TeamSnapshot,
    current: &TeamActivationContext,
) -> Value {
    let members: Vec<Value> = snapshot
        .members
        .iter()
        .map(|row| {
            let mut row = row.clone();
            let model = member_model(snapshot, &row);
            row["model_id"] = json!(model.as_deref().and_then(|alias| identity(config, alias)));
            row["model"] = json!(model);
            row
        })
        .collect();
    let models: Vec<Value> = config
        .models
        .iter()
        .map(|(alias, model)| {
            json!({"alias":alias,"provider":model.provider,"model_id":model_identity(&model.id),"context_window":model.context_window,"tools":model.tools || model.emulate_tools,"input_per_million":model.input_per_million,"output_per_million":model.output_per_million})
        })
        .collect();
    let active = snapshot
        .members
        .iter()
        .filter(|m| {
            m["role"] == "consultant"
                && !matches!(m["state"].as_str(), Some("terminated" | "failed"))
        })
        .count();
    let used = snapshot
        .members
        .iter()
        .filter(|m| m["role"] == "consultant")
        .count();
    json!({
        "members":members,
        "revision":snapshot.revision,
        "state":snapshot.state,
        "self":{"member_id":current.member_id,"role":current.role.as_str(),"model":current.activation_model,"model_id":current.model_identity},
        "open_questions":snapshot.open_questions,
        "limits":snapshot.options.limits,
        "cross_model_review":snapshot.options.cross_model_review,
        "models":models,
        "consultations":{"running":active,"used":used,"max_running":MAX_ACTIVE_CONSULTANTS,"max_total":MAX_CONSULTATIONS,"default_budget_tokens":CONSULT_DEFAULT_TOKENS},
    })
}

/// The activation prompt of a consultant.
pub(super) fn prompt(task: &Task, snapshot: &TeamSnapshot, member: &Value, model: &str) -> String {
    let name = member["name"].as_str().unwrap_or("consultant");
    let id = member["id"].as_str().unwrap_or("");
    let consult = &member["consult"];
    let asker = consult["asker"].as_str().unwrap_or("");
    let asker_name = consult["asker_name"]
        .as_str()
        .unwrap_or("the member that asked");
    let question = consult["question_id"].as_str().unwrap_or("");
    let budget = consult["budget_tokens"]
        .as_i64()
        .unwrap_or(CONSULT_DEFAULT_TOKENS as i64);
    let mut prompt = format!(
        "Cooperative consultation. You are {name} (member {id}), a consultant running model {model}, brought in by {asker_name} (member {asker}) to answer one question: message {question}, delivered below. The team's objective: {}\nAuthorized resources: {}. You only read: you cannot change files, take work or bring in other members. Runtime identity is authoritative. Investigate only what the question needs, then answer {asker_name} with team_message purpose=response, reply_to={question}, recipient={asker}. If the question names a patch, read it with team_patch_show and decide with team_patch_review: that decision is your answer. If you cannot answer without a clarification, ask {asker_name} one question with team_message and wait for it with team_wait. Your budget is {budget} tokens for the whole consultation. Once you answer, or the budget runs out, the runtime retires you. Separate what you verified from what you infer; record runtime evidence with team_verify when a check settles the question.",
        task.objective,
        serde_json::to_string(&snapshot.options.scope.resources).unwrap_or_else(|_| "[]".into()),
    );
    if snapshot.options.cross_model_review {
        prompt.push_str(" This team requires cross-model review: the runtime approves a patch only from a model other than the one that wrote it.");
    }
    prompt
}

impl EngineTeamControl {
    /// Resolves the consultant's model and checks it can do the job before
    /// storage brings it in.
    pub(super) async fn consult(
        &self,
        ctx: &ToolContext<'_>,
        snapshot: &TeamSnapshot,
        args: &Value,
    ) -> Result<Value> {
        let store = &self.runtime.store;
        let context = &self.runtime.context;
        let mut input: TeamConsultInput = serde_json::from_value(args.clone())?;
        let alias = input
            .model
            .clone()
            .or_else(|| {
                snapshot
                    .options
                    .role_models
                    .get(&TeamRole::Consultant)
                    .cloned()
            })
            .or_else(|| snapshot.default_model.clone())
            .unwrap_or_else(|| ctx.config.primary.clone());
        let configured = ctx.config.models.get(&alias).with_context(|| {
            format!("unknown model {alias}; team_members lists the configured models")
        })?;
        ensure!(
            configured.tools || configured.emulate_tools,
            "model {alias} cannot call tools, and a consultant answers through team_message"
        );
        // The operator's provider allowlist and local-only setting hold for
        // consultants too; refuse here instead of failing the consultation.
        ctx.config
            .require_model_egress(&alias)
            .map_err(|error| anyhow::anyhow!("model {alias} cannot be consulted: {error}"))?;
        // Unpriced usage is charged as zero, which would silently escape a
        // team cost ceiling.
        ensure!(
            snapshot.options.limits.max_cost_usd.is_none()
                || (configured.input_per_million.is_some()
                    && configured.output_per_million.is_some()),
            "model {alias} has no configured prices; the team has a cost ceiling"
        );
        if snapshot.options.cross_model_review
            && let Some(patch) = &input.patch_id
        {
            let (meta, _) = store.team_patch(&context.team_id, patch).await?;
            if let Some(author) = meta["author_model"].as_str() {
                ensure!(
                    model_identity(&configured.id) != author,
                    "patch {patch} was written by {author}, the model {alias} names; consult a different model to approve it"
                );
            }
        }
        input.model = Some(alias);
        store
            .team_consult(&context.team_id, &context.member_id, input)
            .await
    }
}

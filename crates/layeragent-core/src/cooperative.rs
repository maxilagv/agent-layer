use super::*;
use async_trait::async_trait;
use layeragent_providers::ProviderAdmission;
use layeragent_sandbox::ExecRequest;
use layeragent_tools::TeamControl;
use std::sync::Arc;
use tokio::task::JoinSet;

pub(crate) struct TeamRuntime {
    pub(crate) context: TeamActivationContext,
    store: Storage,
    session: String,
    cancel: CancellationToken,
}

impl TeamRuntime {
    pub(crate) fn new(
        context: TeamActivationContext,
        store: Storage,
        session: String,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            context,
            store,
            session,
            cancel,
        }
    }

    /// Finish an in-flight operation, then wait without starting further work.
    /// Heartbeats preserve the same activation and canonical session on resume.
    pub(crate) async fn boundary(&self) -> Result<bool> {
        loop {
            ensure!(
                !self.cancel.is_cancelled(),
                "cooperative activation cancelled"
            );
            self.fence(&self.session).await?;
            let paused = self
                .store
                .team_activation_should_pause(
                    &self.context.activation_id,
                    &self.session,
                    self.context.generation,
                )
                .await?;
            if !paused {
                match self
                    .store
                    .team_snapshot_by_id(&self.context.team_id)
                    .await?
                    .state
                    .as_str()
                {
                    "active" => return Ok(true),
                    "closing" => return Ok(false),
                    "paused" | "blocked" => continue,
                    _ => bail!("team no longer accepts work"),
                }
            }
            tokio::select! {
                _ = self.cancel.cancelled() => bail!("cooperative activation cancelled while paused"),
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
            }
        }
    }

    /// Approval waiting is already a safe boundary; keep its deadline and
    /// response receiver live while acknowledging pause and reacting to close.
    pub(crate) async fn approval_pending(&self) -> Result<bool> {
        self.fence(&self.session).await?;
        self.store
            .team_activation_should_pause(
                &self.context.activation_id,
                &self.session,
                self.context.generation,
            )
            .await?;
        Ok(self
            .store
            .team_snapshot_by_id(&self.context.team_id)
            .await?
            .state
            != "closing")
    }

    fn category(&self) -> &'static str {
        match self.context.role {
            TeamRole::Coordinator => "coordination",
            TeamRole::Reviewer => "review",
            TeamRole::Verifier => "verification",
            _ => "investigation",
        }
    }

    async fn fence(&self, session: &str) -> Result<TeamActivationContext> {
        self.store
            .team_heartbeat(
                &self.context.activation_id,
                session,
                self.context.generation,
            )
            .await?;
        self.store
            .team_activation_context(&self.context.activation_id, session)
            .await
    }
}

#[async_trait]
impl ProviderAdmission for TeamRuntime {
    async fn reserve(
        &self,
        _model: &str,
        input_tokens: u64,
        output_tokens: u64,
        estimated_cost: f64,
    ) -> Result<String> {
        loop {
            ensure!(
                self.boundary().await?,
                "team is closing; no new inference admitted"
            );
            let result = self
                .store
                .team_reserve(
                    &self.context.team_id,
                    &self.context.activation_id,
                    self.context.generation,
                    self.category(),
                    input_tokens,
                    output_tokens,
                    estimated_cost,
                )
                .await;
            match result {
                Err(error)
                    if error
                        .downcast_ref::<layeragent_storage::TeamPaused>()
                        .is_some() =>
                {
                    continue;
                }
                other => return other,
            }
        }
    }

    async fn settle(
        &self,
        reservation: &str,
        usage: Option<&Usage>,
        cost: f64,
        uncertain: bool,
    ) -> Result<()> {
        self.store
            .team_settle_reservation(
                reservation,
                &self.context.activation_id,
                usage,
                cost,
                uncertain,
            )
            .await
    }
}

pub(crate) struct EngineTeamControl {
    pub(crate) engine: Arc<Engine>,
    pub(crate) runtime: Arc<TeamRuntime>,
    pub(crate) tracker: SharedTracker,
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .filter(|v| !v.trim().is_empty())
        .with_context(|| format!("{key} required"))
}

/// Fills runtime-owned defaults for append operations: a deterministic idempotency
/// key per activation and call content, and the current team revision.
fn with_defaults(name: &str, args: &Value, activation: &str, revision: i64) -> Result<Value> {
    use sha2::{Digest, Sha256};
    let mut args = args.clone();
    let object = args
        .as_object_mut()
        .context("tool arguments must be an object")?;
    if matches!(
        name,
        "team_message"
            | "team_add_member"
            | "team_work_create"
            | "team_finding"
            | "team_objection"
            | "team_finding_transition"
    ) && !object.contains_key("idempotency_key")
    {
        let digest = Sha256::digest(
            format!("{activation}\0{name}\0{}", Value::Object(object.clone())).as_bytes(),
        );
        object.insert(
            "idempotency_key".into(),
            json!(format!("rt-{}", hex::encode(&digest[..16]))),
        );
    }
    if matches!(
        name,
        "team_message"
            | "team_add_member"
            | "team_work_create"
            | "team_finding"
            | "team_objection"
            | "team_close"
    ) && !object.contains_key("expected_revision")
    {
        object.insert("expected_revision".into(), json!(revision));
    }
    Ok(args)
}

#[async_trait]
impl TeamControl for EngineTeamControl {
    async fn execute(&self, ctx: &ToolContext<'_>, name: &str, args: &Value) -> Result<Value> {
        let current = self.runtime.fence(ctx.session_id).await?;
        let store = &self.runtime.store;
        let team = &current.team_id;
        let member = &current.member_id;
        let snapshot = store.team_snapshot_by_id(team).await?;
        let args = &with_defaults(name, args, &current.activation_id, snapshot.revision)?;
        match name {
            "team_members" => Ok(
                json!({"members":snapshot.members,"revision":snapshot.revision,"state":snapshot.state,"self":{"member_id":member,"role":current.role.as_str()},"open_questions":snapshot.open_questions,"limits":snapshot.options.limits}),
            ),
            "team_message" => {
                store
                    .team_member_message(team, member, serde_json::from_value(args.clone())?)
                    .await
            }
            "team_wait" => {
                store
                    .team_wait(
                        team,
                        member,
                        &current.activation_id,
                        current.generation,
                        required(args, "reason")?,
                    )
                    .await
            }
            "team_add_member" => {
                let input: TeamMemberInput = serde_json::from_value(args.clone())?;
                if let Some(model) = &input.model {
                    ensure!(
                        ctx.config.models.contains_key(model),
                        "unknown role model {model}"
                    );
                }
                store.team_add_member(team, member, input).await
            }
            "team_work_create" => {
                store
                    .team_create_work(team, member, serde_json::from_value(args.clone())?)
                    .await
            }
            "team_work_claim" => {
                store
                    .team_claim_work(
                        team,
                        member,
                        required(args, "work_id")?,
                        args["expected_revision"]
                            .as_i64()
                            .context("expected_revision required")?,
                    )
                    .await
            }
            "team_work_update" => {
                store
                    .team_set_work_state(
                        team,
                        member,
                        required(args, "work_id")?,
                        required(args, "state")?,
                        args["expected_revision"]
                            .as_i64()
                            .context("expected_revision required")?,
                        required(args, "reason")?,
                    )
                    .await
            }
            "team_finding" => {
                store
                    .team_create_finding(team, member, serde_json::from_value(args.clone())?)
                    .await
            }
            "team_finding_transition" => {
                let finding = required(args, "finding_id")?.to_string();
                let mut value = args.clone();
                value
                    .as_object_mut()
                    .context("arguments object")?
                    .remove("finding_id");
                store
                    .team_transition_finding(team, member, &finding, serde_json::from_value(value)?)
                    .await
            }
            "team_initial_assessment" => {
                store
                    .team_record_initial_assessment(team, member, required(args, "text")?)
                    .await?;
                Ok(json!({"recorded":true}))
            }
            "team_objection" => {
                store
                    .team_record_objection(team, member, serde_json::from_value(args.clone())?)
                    .await
            }
            "team_verify" => self.verify(ctx, args).await,
            "team_close" => {
                store
                    .team_propose_close(
                        team,
                        member,
                        args["expected_revision"]
                            .as_i64()
                            .context("expected_revision required")?,
                        required(args, "reason")?,
                    )
                    .await
            }
            "team_resolve" => {
                store
                    .team_resolve_message(
                        team,
                        member,
                        required(args, "message_id")?,
                        required(args, "reason")?,
                    )
                    .await
            }
            _ => bail!("unknown cooperative tool"),
        }
    }
}

impl EngineTeamControl {
    async fn verify(&self, ctx: &ToolContext<'_>, args: &Value) -> Result<Value> {
        ensure!(
            ctx.config.policy.shell,
            "verification requires shell permission"
        );
        let command = required(args, "command")?;
        let expected = required(args, "expected")?;
        let expected_exit = args["expected_exit_code"].as_i64().unwrap_or(0);
        let output = self
            .engine
            .tools
            .sandbox
            .execute(
                ExecRequest {
                    argv: vec![
                        "/bin/bash".into(),
                        "--noprofile".into(),
                        "--norc".into(),
                        "-c".into(),
                        command.into(),
                    ],
                    workspace: ctx.workspace.into(),
                    policy: ctx.config.policy.clone(),
                    stdin: None,
                },
                ctx.cancel.child_token(),
                ctx.stream.clone(),
            )
            .await?;
        let observed =
            json!({"stdout":output.stdout,"stderr":output.stderr,"truncated":output.truncated});
        let expectation_met = output.code.map(i64::from) == Some(expected_exit)
            && (expected.is_empty() || output.stdout.trim() == expected.trim());
        let error = output
            .interrupted
            .then_some("execution interrupted; environmental outcome is inconclusive");
        let snapshot = workspace_receipt(&self.tracker, &ctx.cancel).await?;
        let snapshot_sha256 = required(&snapshot, "snapshot_sha256")?;
        let team = &self.runtime.context.team_id;
        self.runtime
            .store
            .team_snapshot_changed(team, snapshot_sha256)
            .await?;
        let evidence = self.runtime.store.team_record_evidence(
            team,
            &self.runtime.context.member_id,
            layeragent_storage::EvidenceRecord {
                finding: args["finding_id"].as_str().map(str::to_string),
                snapshot: snapshot_sha256.to_string(),
                files: snapshot["files"].clone(),
                exclusions: json!([snapshot["coverage"]["excludes"]]),
                configuration: json!({"model":ctx.config.primary,"role":self.runtime.context.role.as_str(),"write":ctx.config.policy.write,"network":ctx.config.policy.network,"coverage":snapshot["coverage"]}),
                tool: "shell".into(),
                tool_version: Some(env!("CARGO_PKG_VERSION").into()),
                invocation: command.into(),
                exit_code: output.code.map(i64::from),
                expected: Some(json!({"exit_code":expected_exit,"stdout":expected}).to_string()),
                observed: Some(layeragent_protocol::redact::redact_text(&observed.to_string())),
                error: error.map(str::to_string),
                limitations: args["limitations"].clone(),
                negative_control: args["negative_control"].as_str().map(str::to_string),
                artifacts: json!([]),
                expectation_met,
            },
        ).await?;
        Ok(
            json!({"evidence_id":evidence["id"],"receipt":evidence["receipt"],"expectation_met":expectation_met,"exit_code":output.code,"observed":observed,"snapshot":snapshot["snapshot_sha256"]}),
        )
    }
}

async fn workspace_receipt(tracker: &SharedTracker, cancel: &CancellationToken) -> Result<Value> {
    let tracker = tracker
        .clone()
        .context("cooperative workspace tracker unavailable")?;
    let token = cancel.child_token();
    tokio::task::spawn_blocking(move || {
        tracker
            .lock()
            .map_err(|_| anyhow::anyhow!("tracker poisoned"))?
            .receipt(&token)
    })
    .await?
}

pub(crate) fn effective_config(
    base: Arc<Config>,
    ctl: &Arc<agents::AgentCtl>,
    team: Option<&Arc<TeamRuntime>>,
) -> Arc<Config> {
    let mut config = (*agents::effective_config(base, ctl)).clone();
    if let Some(team) = team {
        config.agents.enabled = false;
        config.fallbacks.clear();
        config.policy.write = config.policy.write
            && team.context.options.scope.allow_write
            && !team.context.role.read_only();
        config.auto_skills = false;
    }
    if ctl.tree.execution == ExecutionMode::Individual {
        config.agents.enabled = false;
    }
    Arc::new(config)
}

pub(crate) fn tool_definitions(
    config: &Config,
    team: Option<&Arc<TeamRuntime>>,
) -> Vec<ToolDefinition> {
    layeragent_tools::definitions_with_team(config, team.is_some())
}

impl Engine {
    pub async fn run_cooperative(
        self: &Arc<Self>,
        scope: Scope,
        task_id: String,
        model: Option<String>,
        cancel: CancellationToken,
    ) -> Result<()> {
        let store = self.store(&scope).await?;
        let task = store.task(&scope, &task_id).await?;
        ensure!(
            task.execution == ExecutionMode::Cooperative,
            "task is not cooperative"
        );
        let team = task.team_id.clone().context("cooperative team missing")?;
        let snapshot = store.team_snapshot_by_id(&team).await?;
        ensure!(
            !snapshot.options.scope.allow_write
                && snapshot
                    .members
                    .iter()
                    .all(|member| member["role"] != "writer"),
            "cooperative writers require isolated worktrees and are not enabled"
        );
        let run_cancel = cancel.child_token();
        self.active
            .lock()
            .map_err(|_| anyhow::anyhow!("active lock poisoned"))?
            .insert(
                task_id.clone(),
                (scope.clone(), String::new(), run_cancel.clone()),
            );
        store.team_supervisor_started(&team).await?;
        let result = self
            .supervise(
                scope.clone(),
                store.clone(),
                task.clone(),
                team.clone(),
                model,
                run_cancel.clone(),
            )
            .await;
        self.active
            .lock()
            .map_err(|_| anyhow::anyhow!("active lock poisoned"))?
            .remove(&task_id);
        if run_cancel.is_cancelled() {
            let snapshot = store.team_snapshot_by_id(&team).await?;
            if snapshot.state != "cancelled" && snapshot.state != "terminated" {
                store.team_runtime_state(&team, "cancelled", Some("execution cancelled; activations reaped and pending deliveries cancelled")).await?;
            }
        }
        if let Err(error) = &result {
            let snapshot = store.team_snapshot_by_id(&team).await?;
            if !matches!(
                snapshot.state.as_str(),
                "cancelled" | "terminated" | "failed"
            ) {
                store
                    .team_runtime_state(&team, "blocked", Some(&error.to_string()))
                    .await?;
            }
        }
        result
    }

    async fn supervise(
        self: &Arc<Self>,
        scope: Scope,
        store: Storage,
        task: Task,
        team: String,
        model: Option<String>,
        cancel: CancellationToken,
    ) -> Result<()> {
        let owner = new_id();
        let activation_cancel = CancellationToken::new();
        let mut jobs = JoinSet::new();
        let result = async {
        loop {
            if store.team_check_time_budget(&team).await? {
                activation_cancel.cancel();
                while jobs.join_next().await.is_some() {}
                return Ok(());
            }
            let snapshot = store.team_snapshot_by_id(&team).await?;
            match snapshot.state.as_str() {
                "cancelled" | "failed" => {
                    activation_cancel.cancel();
                    while jobs.join_next().await.is_some() {}
                    return Ok(());
                },
                "terminated" => return Ok(()),
                "paused" | "blocked" => {}
                "closing" => {
                    if jobs.is_empty() && store.team_finalize_close(&team).await? {
                        return Ok(());
                    }
                }
                "active" => {
                    let live = snapshot
                        .members
                        .iter()
                        .filter(|member| member["state"] == "working")
                        .count();
                    let capacity = snapshot.options.limits.max_concurrency.saturating_sub(live);
                    if capacity > 0 {
                        let candidates = store.team_candidates(&team).await?;
                        if candidates.is_empty()
                            && jobs.is_empty()
                            && let Some(diagnostic) = store.team_idle_check(&team).await?
                        {
                            tracing::info!(team, diagnostic, "cooperative supervisor intervened");
                        }
                        let mut admission_error = None;
                        let mut admitted = false;
                        for candidate in candidates.into_iter().take(capacity) {
                            let member = candidate["id"].as_str().context("member id")?.to_string();
                            ensure!(candidate["role"] != "writer", "cooperative writers require isolated worktrees and are not enabled");
                            let started =
                                match store.team_start_activation(&team, &member, &owner).await {
                                    Ok(started) => started,
                                    Err(error) => {
                                        tracing::debug!(%error, member, "activation not admitted");
                                        admission_error = Some(error.to_string());
                                        continue;
                                    }
                                };
                            admitted = true;
                            let activation =
                                started["id"].as_str().context("activation id")?.to_string();
                            let generation = started["generation"]
                                .as_i64()
                                .context("activation generation")?;
                            let session = match candidate["session_id"].as_str() {
                                Some(id) => id.to_string(),
                                None => {
                                    store
                                        .create_session(&scope.principal, Path::new(&task.project))
                                        .await?
                                        .id
                                }
                            };
                            if let Err(error) = store
                                .team_bind_member_session(
                                    &team,
                                    &member,
                                    &activation,
                                    generation,
                                    &session,
                                )
                                .await
                            {
                                let _ = store
                                    .team_finish_activation(
                                        &activation,
                                        &member,
                                        generation,
                                        "failed",
                                        Some(&error.to_string()),
                                        &json!({}),
                                    )
                                    .await;
                                continue;
                            }
                            let initial = candidate["activations"].as_i64() == Some(0);
                            let prompt = activation_prompt(&task, &snapshot, &candidate, initial);
                            let selected = candidate["model"]
                                .as_str()
                                .map(str::to_string)
                                .or_else(|| model.clone());
                            let engine = self.clone();
                            let scope = scope.clone();
                            let store = store.clone();
                            let token = activation_cancel.child_token();
                            let task_effort = task.effort;
                            jobs.spawn(async move {
                                let result = engine.run_bound(scope, RunRequest {
                                    session_id: Some(session), workspace: None, prompt, model: selected,
                                    effort: task_effort, bridge: false, mode: RunMode::Normal,
                                    execution: ExecutionMode::Cooperative, cooperative: None,
                                }, token.clone(), activation.clone()).await;
                                let (state, reason, usage) = match &result {
                                    Ok(result) => (if token.is_cancelled() { "interrupted" } else { "completed" }, None, json!({"input_tokens":result.usage.input_tokens,"output_tokens":result.usage.output_tokens,"cost_usd":result.cost_usd})),
                                    Err(error) => (if token.is_cancelled() { "interrupted" } else { "failed" }, Some(error.to_string()), json!({})),
                                };
                                let _ = store.team_finish_activation(&activation, &member, generation, state, reason.as_deref(), &usage).await;
                                result
                            });
                        }
                        if !admitted && jobs.is_empty()
                            && let Some(reason) = admission_error {
                            store.team_runtime_state(&team, "blocked", Some(&reason)).await?;
                        }
                    }
                }
                _ => bail!("unknown team state {}", snapshot.state),
            }
            tokio::select! {
                _ = cancel.cancelled() => {
                    activation_cancel.cancel();
                    while jobs.join_next().await.is_some() {}
                    return Ok(());
                }
                _ = self.shutdown.cancelled() => {
                    activation_cancel.cancel();
                    while jobs.join_next().await.is_some() {}
                    store.team_runtime_state(&team, "blocked", Some("runtime stopped; explicit resume required")).await?;
                    return Ok(());
                }
                Some(joined) = jobs.join_next(), if !jobs.is_empty() => {
                    match joined {
                        Ok(Ok(_)) => {},
                        Ok(Err(error)) => {
                            let state = store.team_snapshot_by_id(&team).await?.state;
                            if !cancel.is_cancelled() && !matches!(state.as_str(), "cancelled" | "terminated" | "closing") {
                                store.team_runtime_state(&team, "failed", Some(&error.to_string())).await?;
                            }
                        }
                        Err(error) => {
                            tracing::warn!(%error, "cooperative activation join failed");
                            store.team_runtime_state(&team, "failed", Some("cooperative activation join failed")).await?;
                        }
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
            }
        }
        }.await;
        // Error paths also cancel and reap every activation before returning.
        activation_cancel.cancel();
        while jobs.join_next().await.is_some() {}
        result
    }
}

fn activation_prompt(
    task: &Task,
    snapshot: &TeamSnapshot,
    member: &Value,
    initial: bool,
) -> String {
    let role = member["role"].as_str().unwrap_or("specialist");
    let mut prompt = format!(
        "Cooperative template {} v{}. You are {} (member {}). Canonical session continuity is retained across activations. Runtime identity is authoritative. Objective: {}\nAuthorized resources: {}. Deliverables: {}. Write authorized: {}. Use team_* tools to consult peers directly; never use traditional child-agent delegation. Message acceptance is not comprehension. Record findings separately from runtime evidence. Finish a response without ending the team. With team_wait, finish this response to release concurrency.",
        snapshot.template.as_str(),
        snapshot.template_version,
        member["name"].as_str().unwrap_or(role),
        member["id"].as_str().unwrap_or(""),
        task.objective,
        serde_json::to_string(&snapshot.options.scope.resources).unwrap_or_else(|_| "[]".into()),
        serde_json::to_string(&snapshot.options.scope.deliverables).unwrap_or_else(|_| "[]".into()),
        snapshot.options.scope.allow_write,
    );
    prompt.push_str(template_instructions(snapshot.template));
    if initial {
        prompt.push_str(" This is your first activation. Establish your own assessment before relying on peer conclusions. Coordinator: plan and assign distinct work, then propose close only after disposition of open work. Specialist: report useful conclusions and evidence to the coordinator or requesting peer with team_message.");
    } else {
        prompt.push_str(" Continue from your canonical session. Pending deliveries are incorporated only at safe boundaries. Answer linked questions, challenge unsupported claims, and preserve inconclusive outcomes when evidence is insufficient.");
    }
    prompt
}

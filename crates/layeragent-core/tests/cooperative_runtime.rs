//! Cooperative supervisor semantics with a scripted model: direct peer messaging,
//! a single concurrency slot, runtime-owned identity and budget admission.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use anyhow::Result;
use async_trait::async_trait;
use layeragent_config::{Config, Model, Provider, Wire};
use layeragent_core::Engine;
use layeragent_protocol::task::TaskState;
use layeragent_protocol::*;
use layeragent_providers::{Delta, LanguageModel, ProviderAdmission};
use layeragent_sandbox::{ExecOutput, ExecRequest, Sandbox};
use layeragent_storage::{Storage, TaskCreate};
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

/// Scripted team: the analyst asks the reviewer directly and waits; the reviewer
/// answers; the coordinator closes when the supervisor reports the team idle.
struct TeamModel {
    ids: Arc<Mutex<BTreeMap<String, String>>>,
    /// (member name, tool or "text") in call order, for assertions.
    log: Arc<Mutex<Vec<(String, String)>>>,
    /// Sender identity the analyst tries to forge in its message arguments.
    forge: bool,
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

impl TeamModel {
    fn decide(&self, messages: &[Message]) -> (String, Completion) {
        let start = messages
            .iter()
            .rposition(|m| m.role == "user" && m.content.starts_with("Cooperative template"))
            .expect("activation prompt");
        let prompt = &messages[start].content;
        let name = prompt
            .split("You are ")
            .nth(1)
            .and_then(|rest| rest.split(' ').next())
            .unwrap_or("")
            .to_string();
        let activation = &messages[start..];
        let history = messages;
        let last_tool = activation
            .iter()
            .rev()
            .find_map(|m| m.tool_calls.first().map(|c| c.name.clone()));
        let incorporated = |purpose: &str| {
            history.iter().find(|m| {
                m.content.starts_with("Peer reference message")
                    && m.content.contains(&format!("\"purpose\":\"{purpose}\""))
            })
        };
        let ids = self.ids.lock().unwrap().clone();
        let decision = match name.as_str() {
            "analyst" => {
                if incorporated("response").is_some() {
                    text("answer received")
                } else if last_tool.is_none() {
                    let mut args = json!({"recipient":ids["reviewer"],"purpose":"question","body":"Is input validated before storage?"});
                    if self.forge {
                        args["sender_member_id"] = json!(ids["coordinator"]);
                        args["sender_kind"] = json!("user");
                    }
                    call("team_message", args)
                } else if last_tool.as_deref() == Some("team_message") {
                    call("team_wait", json!({"reason":"awaiting reviewer answer"}))
                } else {
                    text("waiting for reviewer")
                }
            }
            "reviewer" => {
                let answered = history
                    .iter()
                    .any(|m| m.tool_calls.iter().any(|c| c.name == "team_message"));
                match incorporated("question") {
                    Some(question) if !answered => {
                        let payload: Value = serde_json::from_str(
                            question.content.split_once(": ").expect("payload").1,
                        )
                        .expect("delivery json");
                        call(
                            "team_message",
                            json!({"recipient":ids["analyst"],"purpose":"response","reply_to":payload["id"],"body":"Yes, validated in the gateway layer."}),
                        )
                    }
                    _ => text("reviewer idle"),
                }
            }
            "coordinator" => {
                let noticed = activation
                    .iter()
                    .any(|m| m.content.starts_with("Runtime supervisor notice"));
                if noticed && last_tool.is_none() {
                    call("team_close", json!({"reason":"question answered"}))
                } else {
                    text("plan recorded")
                }
            }
            _ => text("idle"),
        };
        let label = decision
            .calls
            .first()
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "text".into());
        self.log.lock().unwrap().push((name.clone(), label));
        (name, decision)
    }
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
        Ok(self.decide(messages).1)
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

async fn cooperative_task(
    store: &Storage,
    scope: &Scope,
    workspace: &std::path::Path,
    options: CooperativeOptions,
) -> Result<Task> {
    match store
        .create_task_with_execution(
            scope,
            &workspace.to_string_lossy(),
            None,
            "Check whether storage input is validated",
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
        TaskCreate::Created(task) | TaskCreate::Replayed(task) => Ok(task),
    }
}

async fn run_team(forge: bool, preload: usize) -> Result<(Storage, Task, Vec<(String, String)>)> {
    let home = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let ids = Arc::new(Mutex::new(BTreeMap::new()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let engine = Engine::with_components(
        home.path().into(),
        "default".into(),
        config(),
        Arc::new(TeamModel {
            ids: ids.clone(),
            log: log.clone(),
            forge,
        }),
        Arc::new(NoopSandbox),
    )?;
    let scope = Scope::local();
    let store = engine.store(&scope).await?;
    // One concurrency slot: asking and answering must not deadlock.
    let options = CooperativeOptions {
        limits: TeamLimits {
            max_members: 3,
            max_concurrency: 1,
            ..TeamLimits::default()
        },
        ..Default::default()
    };
    let task = cooperative_task(&store, &scope, workspace.path(), options).await?;
    let team = task.team_id.clone().expect("team");
    for member in store.team_members(&team).await? {
        ids.lock().unwrap().insert(
            member["name"].as_str().unwrap().to_string(),
            member["id"].as_str().unwrap().to_string(),
        );
    }
    for i in 0..preload {
        let revision = store.team_snapshot_by_id(&team).await?.revision;
        let analyst = ids.lock().unwrap()["analyst"].clone();
        store
            .team_user_message(
                &scope,
                &task.id,
                TeamMessageInput {
                    idempotency_key: format!("preload-{i}"),
                    expected_revision: revision,
                    recipient: Some(analyst),
                    purpose: MessagePurpose::UserInstruction,
                    body: format!("context note {i}"),
                    work_id: None,
                    finding_id: None,
                    evidence_id: None,
                    reply_to: None,
                    provenance: vec![],
                },
            )
            .await?;
    }
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        engine.run_cooperative(
            scope.clone(),
            task.id.clone(),
            None,
            tokio_util::sync::CancellationToken::new(),
        ),
    )
    .await;
    if outcome.is_err() {
        let mut tools = Vec::new();
        for member in store.team_members(&team).await? {
            if let Some(session) = member["session_id"].as_str() {
                for (_, message) in store.history_since(session, 0).await? {
                    if message.role == "tool" {
                        tools.push(format!("{}: {}", member["name"], message.content));
                    }
                }
            }
        }
        let state = store.team_snapshot_by_id(&team).await?.state;
        let log = log.lock().unwrap().clone();
        panic!("team did not finish\nlog: {log:?}\nstate: {state}\ntool results: {tools:#?}");
    }
    outcome??;
    let task = store.task(&scope, &task.id).await?;
    let log = log.lock().unwrap().clone();
    // Keep directories alive until assertions finish reading the store.
    std::mem::forget(home);
    std::mem::forget(workspace);
    Ok((store, task, log))
}

#[tokio::test]
async fn peers_exchange_directly_with_one_slot_and_close_without_a_final_model_call() -> Result<()>
{
    let (store, task, log) = run_team(false, 0).await?;
    assert_eq!(task.state, TaskState::Completed);
    let team = task.team_id.clone().unwrap();
    let snapshot = store.team_snapshot_by_id(&team).await?;
    assert_eq!(snapshot.state, "terminated");

    // The reviewer's answer reached the analyst without the coordinator copying it.
    let scope = Scope::local();
    let messages = store
        .team_resource(&scope, &task.id, "messages", 0, 100)
        .await?
        .0;
    let ids: BTreeMap<String, String> = store
        .team_members(&team)
        .await?
        .into_iter()
        .map(|m| {
            (
                m["name"].as_str().unwrap().into(),
                m["id"].as_str().unwrap().into(),
            )
        })
        .collect();
    let question = messages
        .iter()
        .find(|m| m["purpose"] == "question")
        .expect("question");
    assert_eq!(question["sender_member_id"], ids["analyst"]);
    assert_eq!(question["recipient_member_id"], ids["reviewer"]);
    let answer = messages
        .iter()
        .find(|m| m["purpose"] == "response")
        .expect("response");
    assert_eq!(answer["sender_member_id"], ids["reviewer"]);
    assert_eq!(answer["reply_to"], question["id"]);
    assert!(
        !messages
            .iter()
            .any(|m| m["sender_member_id"] == ids["coordinator"] && m["purpose"] != "observation"),
        "coordinator never relays peer messages"
    );
    assert!(
        log.iter()
            .any(|(who, what)| who == "analyst" && what == "text")
    );

    // Every provider request was reserved and settled against the team budget.
    assert!(snapshot.usage["total_tokens"].as_u64().unwrap() >= 50 * 4);
    assert_eq!(snapshot.reservations["reserved_tokens"], 0);
    assert_eq!(snapshot.reservations["uncertain_tokens"], 0);
    // Closing was finalized by the supervisor, not by an extra model turn.
    assert_eq!(log.last().map(|(who, _)| who.as_str()), Some("coordinator"));
    Ok(())
}

#[tokio::test]
async fn sender_identity_comes_from_the_runtime_not_model_arguments() -> Result<()> {
    let (store, task, _) = run_team(true, 0).await?;
    let team = task.team_id.clone().unwrap();
    let analyst = store
        .team_members(&team)
        .await?
        .into_iter()
        .find(|m| m["name"] == "analyst")
        .unwrap();
    let messages = store
        .team_resource(&Scope::local(), &task.id, "messages", 0, 100)
        .await?
        .0;
    // Forged sender fields are not part of the tool contract: the call is rejected
    // and nothing is persisted under another identity or as a user instruction.
    assert!(messages.iter().all(|m| m["sender_kind"] != "user"));
    assert!(
        messages
            .iter()
            .filter(|m| m["sender_kind"] == "member")
            .all(|m| m["purpose"] != "question" || m["sender_member_id"] == analyst["id"])
    );
    let session = analyst["session_id"].as_str().unwrap();
    let rejected = store
        .history_since(session, 0)
        .await?
        .into_iter()
        .any(|(_, m)| m.role == "tool" && m.content.contains("do not match the declared schema"));
    assert!(
        rejected,
        "forged identity arguments are rejected by the schema"
    );
    Ok(())
}

#[tokio::test]
async fn simultaneous_arrivals_are_batched_into_one_activation() -> Result<()> {
    let (store, task, _) = run_team(false, 3).await?;
    let team = task.team_id.clone().unwrap();
    let analyst = store
        .team_members(&team)
        .await?
        .into_iter()
        .find(|m| m["name"] == "analyst")
        .unwrap();
    let events = store.task_events(&task.id, 0, 1000).await?;
    let first = events
        .iter()
        .find(|e| {
            e["kind"] == "team_messages_incorporated" && e["data"]["member_id"] == analyst["id"]
        })
        .expect("incorporation");
    assert_eq!(first["data"]["deliveries"].as_array().unwrap().len(), 3);
    let activations = events
        .iter()
        .filter(|e| {
            e["kind"] == "team_activation_started" && e["data"]["member_id"] == analyst["id"]
        })
        .count();
    // One activation for the batched notes, one more after the reviewer answers.
    assert_eq!(activations, 2);
    Ok(())
}

/// Controlled operations exercise runtime boundaries, not native confinement.
struct BoundaryModel {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    release: Arc<tokio::sync::Semaphore>,
}
#[async_trait]
impl LanguageModel for BoundaryModel {
    async fn infer(
        &self,
        _: &Config,
        _: &str,
        _: &[Message],
        _: &[ToolDefinition],
        cancel: tokio_util::sync::CancellationToken,
        _: mpsc::Sender<Delta>,
    ) -> Result<Completion> {
        let index = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if index == 0 {
            let permit = tokio::select! {
                _ = cancel.cancelled() => anyhow::bail!("fixture inference cancelled"),
                permit = self.release.acquire() => permit?,
            };
            permit.forget();
            let mut first = call("shell", json!({"command":"first fixture operation"}));
            first
                .calls
                .extend(call("shell", json!({"command":"second fixture operation"})).calls);
            Ok(first)
        } else {
            Ok(call("team_close", json!({"reason":"fixture complete"})))
        }
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
        let admission = admission.expect("admitted team inference");
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

struct BoundarySandbox {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    release: Arc<tokio::sync::Semaphore>,
}
#[async_trait]
impl Sandbox for BoundarySandbox {
    async fn execute(
        &self,
        request: ExecRequest,
        cancel: tokio_util::sync::CancellationToken,
        _: Option<mpsc::Sender<String>>,
    ) -> Result<ExecOutput> {
        assert!(!request.policy.write, "cooperative tools remain read-only");
        let index = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if index == 0 {
            let permit = tokio::select! {
                _ = cancel.cancelled() => anyhow::bail!("fixture tool cancelled"),
                permit = self.release.acquire() => permit?,
            };
            permit.forget();
        }
        Ok(ExecOutput {
            code: Some(0),
            stdout: "fixture output".into(),
            stderr: String::new(),
            interrupted: false,
            truncated: false,
        })
    }
}

struct BoundaryFixture {
    _home: tempfile::TempDir,
    _workspace: tempfile::TempDir,
    engine: Arc<Engine>,
    store: Storage,
    task: Task,
    inference_calls: Arc<std::sync::atomic::AtomicUsize>,
    inference_release: Arc<tokio::sync::Semaphore>,
    tool_calls: Arc<std::sync::atomic::AtomicUsize>,
    tool_release: Arc<tokio::sync::Semaphore>,
}
impl BoundaryFixture {
    async fn new(approval: bool) -> Result<Self> {
        Self::with_seconds(approval, TeamLimits::default().max_active_seconds).await
    }
    async fn with_seconds(approval: bool, max_active_seconds: u64) -> Result<Self> {
        let home = tempfile::tempdir()?;
        let workspace = tempfile::tempdir()?;
        let inference_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tool_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let inference_release = Arc::new(tokio::sync::Semaphore::new(0));
        let tool_release = Arc::new(tokio::sync::Semaphore::new(0));
        let mut config = config();
        config.policy.shell = true;
        if approval {
            config.policy.approval_tools.push("shell".into());
        }
        let engine = Engine::with_components(
            home.path().into(),
            "default".into(),
            config,
            Arc::new(BoundaryModel {
                calls: inference_calls.clone(),
                release: inference_release.clone(),
            }),
            Arc::new(BoundarySandbox {
                calls: tool_calls.clone(),
                release: tool_release.clone(),
            }),
        )?;
        let store = engine.store(&Scope::local()).await?;
        let task = cooperative_task(
            &store,
            &Scope::local(),
            workspace.path(),
            CooperativeOptions {
                limits: TeamLimits {
                    max_members: 1,
                    max_concurrency: 1,
                    max_active_seconds,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await?;
        Ok(Self {
            _home: home,
            _workspace: workspace,
            engine,
            store,
            task,
            inference_calls,
            inference_release,
            tool_calls,
            tool_release,
        })
    }
    fn start(
        &self,
    ) -> (
        tokio_util::sync::CancellationToken,
        tokio::task::JoinHandle<Result<()>>,
    ) {
        let cancel = tokio_util::sync::CancellationToken::new();
        let engine = self.engine.clone();
        let id = self.task.id.clone();
        let token = cancel.clone();
        let job = tokio::spawn(async move {
            engine
                .run_cooperative(Scope::local(), id, None, token)
                .await
        });
        (cancel, job)
    }
    async fn command(&self, action: &str) -> Result<()> {
        let revision = self
            .store
            .team_snapshot(&Scope::local(), &self.task.id)
            .await?
            .revision;
        self.store
            .team_command(
                &Scope::local(),
                &self.task.id,
                action,
                TeamCommand {
                    idempotency_key: new_id(),
                    expected_revision: revision,
                    recipient: None,
                    reason: Some(format!("fixture {action}")),
                    priority: None,
                },
            )
            .await?;
        Ok(())
    }
    async fn at_pause_boundary(&self) -> Result<()> {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let snapshot = self
                    .store
                    .team_snapshot(&Scope::local(), &self.task.id)
                    .await?;
                if snapshot.state == "paused" && !snapshot.pause_pending {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await??;
        Ok(())
    }
    async fn next_approval(&self, after: i64) -> Result<Value> {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if let Some(event) = self
                    .store
                    .task_events(&self.task.id, after, 1000)
                    .await?
                    .into_iter()
                    .find(|event| {
                        event["kind"] == "team_member_event"
                            && event["data"]["kind"] == "approval_required"
                    })
                {
                    return Ok::<_, anyhow::Error>(event);
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await?
    }
}
async fn wait_count(counter: &std::sync::atomic::AtomicUsize, expected: usize) -> Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while counter.load(std::sync::atomic::Ordering::SeqCst) < expected {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn pause_drains_inference_and_each_tool_then_resumes_without_uncertain_usage() -> Result<()> {
    let f = BoundaryFixture::new(false).await?;
    let (_, job) = f.start();
    wait_count(&f.inference_calls, 1).await?;
    f.command("pause").await?;
    assert!(
        f.store
            .team_snapshot(&Scope::local(), &f.task.id)
            .await?
            .pause_pending
    );
    f.inference_release.add_permits(1);
    f.at_pause_boundary().await?;
    assert_eq!(f.tool_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        f.inference_calls.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    let paused = f.store.team_snapshot(&Scope::local(), &f.task.id).await?;
    assert_eq!(paused.reservations["uncertain_tokens"], 0);
    let member = &paused.members[0];
    let session = member["session_id"].as_str().unwrap().to_string();
    let note = "operator note accepted while paused";
    f.store
        .team_user_message(
            &Scope::local(),
            &f.task.id,
            TeamMessageInput {
                idempotency_key: new_id(),
                expected_revision: paused.revision,
                recipient: Some(member["id"].as_str().unwrap().into()),
                purpose: MessagePurpose::UserInstruction,
                body: note.into(),
                work_id: None,
                finding_id: None,
                evidence_id: None,
                reply_to: None,
                provenance: vec![],
            },
        )
        .await?;
    assert!(
        !f.store
            .history_since(&session, 0)
            .await?
            .iter()
            .any(|(_, message)| message.content.contains(note))
    );
    assert_eq!(
        f.store.task(&Scope::local(), &f.task.id).await?.state,
        TaskState::Blocked
    );
    f.command("resume").await?;
    wait_count(&f.tool_calls, 1).await?;
    f.command("pause").await?;
    f.tool_release.add_permits(1);
    f.at_pause_boundary().await?;
    assert_eq!(f.tool_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        f.inference_calls.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    f.command("resume").await?;
    tokio::time::timeout(std::time::Duration::from_secs(10), job).await???;
    assert_eq!(f.tool_calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(
        f.inference_calls.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "close needs no final inference"
    );
    let done = f.store.team_snapshot(&Scope::local(), &f.task.id).await?;
    assert_eq!(done.state, "terminated");
    assert_eq!(done.reservations["reserved_tokens"], 0);
    assert_eq!(done.reservations["uncertain_tokens"], 0);
    assert_eq!(
        f.store
            .history_since(&session, 0)
            .await?
            .iter()
            .filter(|(_, message)| message.content.contains(note))
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn member_approval_is_attributed_to_parent_task_and_pause_still_gates_approved_tool()
-> Result<()> {
    let f = BoundaryFixture::new(true).await?;
    let (cancel, job) = f.start();
    f.inference_release.add_permits(1);
    let event = f.next_approval(0).await?;
    let payload = &event["data"]["data"];
    let decision = ApprovalDecision {
        expected_revision: payload["expected_revision"].as_i64().unwrap(),
        arguments_hash: payload["arguments_hash"].as_str().unwrap().into(),
        allow: true,
    };
    let id = payload["approval_id"].as_str().unwrap();
    assert!(event["data"]["member_id"].as_str().is_some());
    assert!(event["data"]["activation_id"].as_str().is_some());
    assert!(
        f.engine
            .approve(&Scope::local(), Some("different-task"), id, &decision)
            .await
            .is_err()
    );
    f.command("pause").await?;
    f.at_pause_boundary().await?;
    f.engine
        .approve(&Scope::local(), Some(&f.task.id), id, &decision)
        .await?;
    f.at_pause_boundary().await?;
    assert_eq!(f.tool_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    f.command("resume").await?;
    wait_count(&f.tool_calls, 1).await?;
    f.tool_release.add_permits(1);
    let second = f.next_approval(event["seq"].as_i64().unwrap()).await?;
    assert_eq!(
        f.store.task(&Scope::local(), &f.task.id).await?.state,
        TaskState::Running
    );
    let second_id = second["data"]["data"]["approval_id"].as_str().unwrap();
    assert_ne!(id, second_id);
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(10), job).await???;
    assert_eq!(f.tool_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        f.store
            .team_snapshot(&Scope::local(), &f.task.id)
            .await?
            .state,
        "cancelled"
    );
    Ok(())
}

#[tokio::test]
async fn cancellation_while_paused_reaps_activation_without_more_calls() -> Result<()> {
    let f = BoundaryFixture::new(false).await?;
    let (cancel, job) = f.start();
    wait_count(&f.inference_calls, 1).await?;
    f.command("pause").await?;
    f.inference_release.add_permits(1);
    f.at_pause_boundary().await?;
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(10), job).await???;
    assert_eq!(f.tool_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    let snapshot = f.store.team_snapshot(&Scope::local(), &f.task.id).await?;
    assert_eq!(snapshot.state, "cancelled");
    assert!(
        snapshot
            .members
            .iter()
            .all(|member| member["state"] == "terminated")
    );
    Ok(())
}

#[tokio::test]
async fn explicit_close_drains_current_inference_without_executing_its_tools() -> Result<()> {
    let f = BoundaryFixture::new(false).await?;
    let (_, job) = f.start();
    wait_count(&f.inference_calls, 1).await?;
    f.command("close").await?;
    f.inference_release.add_permits(1);
    tokio::time::timeout(std::time::Duration::from_secs(10), job).await???;
    assert_eq!(f.tool_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        f.inference_calls.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(
        f.store
            .team_snapshot(&Scope::local(), &f.task.id)
            .await?
            .state,
        "terminated"
    );
    Ok(())
}

#[tokio::test]
async fn explicit_close_resolves_pending_approval_without_waiting_for_its_timeout() -> Result<()> {
    let f = BoundaryFixture::new(true).await?;
    let (_, job) = f.start();
    f.inference_release.add_permits(1);
    let event = f.next_approval(0).await?;
    f.command("close").await?;
    tokio::time::timeout(std::time::Duration::from_secs(10), job).await???;
    let snapshot = f.store.team_snapshot(&Scope::local(), &f.task.id).await?;
    assert_eq!(snapshot.state, "terminated");
    assert!(snapshot.pending_approvals.is_empty());
    assert_eq!(f.tool_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    let events = f
        .store
        .task_events(&f.task.id, event["seq"].as_i64().unwrap(), 1000)
        .await?;
    assert!(
        events
            .iter()
            .any(|event| event["kind"] == "team_member_event"
                && event["data"]["kind"] == "approval_resolved"
                && event["data"]["data"]["allow"] == false)
    );
    Ok(())
}

#[tokio::test]
async fn cancelling_inflight_inference_retains_uncertain_reservation_after_reaping() -> Result<()> {
    let f = BoundaryFixture::new(false).await?;
    let (cancel, job) = f.start();
    wait_count(&f.inference_calls, 1).await?;
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(10), job).await???;
    let snapshot = f.store.team_snapshot(&Scope::local(), &f.task.id).await?;
    assert_eq!(snapshot.state, "cancelled");
    assert_eq!(snapshot.reservations["reserved_tokens"], 0);
    assert_eq!(snapshot.reservations["uncertain_tokens"], 1100);
    assert_eq!(f.tool_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn time_budget_interrupts_inflight_inference_without_waiting_for_next_activation()
-> Result<()> {
    let f = BoundaryFixture::with_seconds(false, 1).await?;
    let (_, job) = f.start();
    wait_count(&f.inference_calls, 1).await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), job).await???;
    let snapshot = f.store.team_snapshot(&Scope::local(), &f.task.id).await?;
    assert_eq!(snapshot.state, "blocked");
    assert!(
        snapshot
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("time")
    );
    assert_eq!(
        f.store.task(&Scope::local(), &f.task.id).await?.state,
        TaskState::Blocked
    );
    assert_eq!(snapshot.reservations["reserved_tokens"], 0);
    assert_eq!(snapshot.reservations["uncertain_tokens"], 1100);
    assert_eq!(f.tool_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn paused_inflight_operation_still_consumes_time_budget_until_it_drains() -> Result<()> {
    let f = BoundaryFixture::with_seconds(false, 1).await?;
    let (_, job) = f.start();
    wait_count(&f.inference_calls, 1).await?;
    f.command("pause").await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), job).await???;
    let snapshot = f.store.team_snapshot(&Scope::local(), &f.task.id).await?;
    assert_eq!(snapshot.state, "blocked");
    assert_eq!(snapshot.reservations["uncertain_tokens"], 1100);
    assert_eq!(f.tool_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn fully_paused_wait_does_not_consume_active_time_budget() -> Result<()> {
    let f = BoundaryFixture::with_seconds(false, 1).await?;
    let (_, job) = f.start();
    wait_count(&f.inference_calls, 1).await?;
    f.command("pause").await?;
    f.inference_release.add_permits(1);
    f.at_pause_boundary().await?;
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    assert_eq!(
        f.store
            .team_snapshot(&Scope::local(), &f.task.id)
            .await?
            .state,
        "paused"
    );
    f.command("resume").await?;
    f.tool_release.add_permits(1);
    tokio::time::timeout(std::time::Duration::from_secs(5), job).await???;
    assert_eq!(
        f.store
            .team_snapshot(&Scope::local(), &f.task.id)
            .await?
            .state,
        "terminated"
    );
    Ok(())
}

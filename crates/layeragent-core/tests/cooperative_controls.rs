//! Operator controls on one member of a running team, with scripted models
//! and a real Git repository. A writer is paused while it works: its
//! activation ends at the next boundary and the team carries on. The
//! operator resumes it, then retires it during a provider call: the call is
//! cancelled, its unsubmitted change is kept as a retained patch, its work
//! goes back to the coordinator and the team still completes. The scripted
//! model plays the operator at fixed points so the timing is deterministic.
//! The sandbox is a test double; it does not certify confinement.
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
    path::Path,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
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
            id: "fixture-model".into(),
            context_window: 32768,
            tools: true,
            emulate_tools: false,
            input_per_million: None,
            output_per_million: None,
            parameters: Default::default(),
            notes: None,
            team_access: Default::default(),
        },
    );
    c.primary = "fixture".into();
    c.policy.write = true;
    c
}

/// Performs `write_file` directly in the requested workspace.
struct FileSandbox;
#[async_trait]
impl Sandbox for FileSandbox {
    async fn execute(
        &self,
        request: ExecRequest,
        _: tokio_util::sync::CancellationToken,
        _: Option<mpsc::Sender<String>>,
    ) -> Result<ExecOutput> {
        anyhow::ensure!(
            request.argv.last().map(String::as_str) == Some("write_file"),
            "only write_file is expected"
        );
        let data: Value = serde_json::from_str(request.stdin.as_deref().unwrap_or("{}"))?;
        let path = request.workspace.join(data["path"].as_str().unwrap_or(""));
        tokio::fs::write(&path, data["content"].as_str().unwrap_or("")).await?;
        Ok(ExecOutput {
            code: Some(0),
            stdout: json!({"written":data["path"]}).to_string(),
            stderr: String::new(),
            interrupted: false,
            truncated: false,
        })
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

fn header(content: &str, key: &str) -> String {
    let head = content.split("\n--- body ---\n").next().unwrap_or("");
    head.lines()
        .flat_map(|line| line.split(" · "))
        .find_map(|part| part.strip_prefix(&format!("{key}: ")))
        .unwrap_or("")
        .to_string()
}

fn word_after<'a>(text: &'a str, marker: &str) -> &'a str {
    text.split(marker)
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or("")
}

/// What the scripted operator does before a model call returns.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Operator {
    Pause,
    Resume,
    /// Retire, then hold the provider call open until it is cancelled.
    RetireDuringCall,
}

/// Where the operator acts: the store and the task, set once created.
#[derive(Default)]
struct Desk {
    store: OnceLock<Storage>,
    task: OnceLock<String>,
    ids: Mutex<BTreeMap<String, String>>,
    work: Mutex<Option<String>>,
    paused: AtomicBool,
    resumed: AtomicBool,
    retired: AtomicBool,
    /// How long the retired member's provider call stayed open.
    cancelled_after: Mutex<Option<Duration>>,
    log: Mutex<Vec<(String, String)>>,
}

impl Desk {
    async fn operate(&self, action: Operator) -> Result<()> {
        let store = self.store.get().expect("store");
        let task = self.task.get().expect("task");
        let scope = Scope::local();
        let snapshot = store
            .team_snapshot_by_id(&self.team(store, task).await?)
            .await?;
        let analyst = self.ids.lock().unwrap()["analyst"].clone();
        let action = match action {
            Operator::Pause => "pause",
            Operator::Resume => "resume",
            Operator::RetireDuringCall => "retire",
        };
        store
            .team_command(
                &scope,
                task,
                action,
                TeamCommand {
                    idempotency_key: new_id(),
                    expected_revision: snapshot.revision,
                    reason: Some(format!("operator {action}")),
                    recipient: None,
                    priority: None,
                    member: Some(analyst),
                },
            )
            .await?;
        Ok(())
    }

    async fn team(&self, store: &Storage, task: &str) -> Result<String> {
        store
            .task(&Scope::local(), task)
            .await?
            .team_id
            .ok_or_else(|| anyhow::anyhow!("team"))
    }
}

struct TeamModel {
    desk: Arc<Desk>,
}

/// Records how long a held provider call stayed open, however it ends.
struct OpenCall {
    started: Instant,
    desk: Arc<Desk>,
}
impl Drop for OpenCall {
    fn drop(&mut self) {
        *self.desk.cancelled_after.lock().unwrap() = Some(self.started.elapsed());
    }
}

impl TeamModel {
    fn decide(&self, messages: &[Message]) -> (String, Completion, Option<Operator>) {
        let start = messages
            .iter()
            .rposition(|m| m.role == "user" && m.content.starts_with("Cooperative template"))
            .expect("activation prompt");
        let prompt = &messages[start].content;
        let name = word_after(prompt, "You are ").to_string();
        let writing = prompt.contains(" Access: write.");
        let activation = &messages[start..];
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
        let ever = |tool: &str| {
            messages
                .iter()
                .any(|m| m.tool_calls.iter().any(|c| c.name == tool))
        };
        let now = |tool: &str| {
            activation
                .iter()
                .any(|m| m.tool_calls.iter().any(|c| c.name == tool))
        };
        let notice = |needle: &str| {
            messages.iter().any(|m| {
                m.content.starts_with("Runtime supervisor notice") && m.content.contains(needle)
            })
        };
        let desk = &self.desk;
        let ids = desk.ids.lock().unwrap().clone();
        let work = desk.work.lock().unwrap().clone();
        let mut operator = None;
        let decision = match name.as_str() {
            "coordinator" => {
                if !ever("team_work_create") {
                    call(
                        "team_work_create",
                        json!({"title":"rename the helper","deliverable":"integrated patch","assignee":ids["analyst"],"close_conditions":"patch integrated"}),
                    )
                } else if last_tool.as_deref() == Some("team_work_create") {
                    let created = serde_json::from_str::<Value>(&last_result).unwrap_or_default();
                    *desk.work.lock().unwrap() = created["id"].as_str().map(str::to_string);
                    call(
                        "team_message",
                        json!({"recipient":ids["analyst"],"purpose":"observation","body":"Please rename the helper in src/lib.rs.","work_id":created["id"]}),
                    )
                } else if notice("pending for you again") && !ever("team_work_update") {
                    // Created, claimed by the analyst, returned on retirement.
                    call(
                        "team_work_update",
                        json!({"work_id":work,"state":"cancelled","expected_revision":3,"reason":"the operator retired its writer"}),
                    )
                } else if last_tool.as_deref() == Some("team_work_update") {
                    assert!(last_result.contains("cancelled"), "{last_result}");
                    call("team_close", json!({"reason":"the work was cancelled"}))
                } else if notice("The operator paused analyst")
                    && !desk.resumed.swap(true, Ordering::SeqCst)
                {
                    operator = Some(Operator::Resume);
                    text("the operator resumes the analyst")
                } else {
                    text("coordinating")
                }
            }
            "analyst" => {
                let assigned = messages
                    .iter()
                    .find(|m| {
                        m.content.starts_with("Peer reference message")
                            && header(&m.content, "purpose") == "observation"
                    })
                    .map(|m| header(&m.content, "work"));
                match (writing, assigned) {
                    (_, None) => text("no work yet"),
                    (false, Some(work)) => {
                        if !ever("team_work_claim") {
                            call(
                                "team_work_claim",
                                json!({"work_id":work,"expected_revision":1}),
                            )
                        } else if !ever("team_access") {
                            call(
                                "team_access",
                                json!({"mode":"write","work_id":work,"paths":["src"],"reason":"the rename changes src/lib.rs"}),
                            )
                        } else {
                            text("waiting for my worktree")
                        }
                    }
                    (true, Some(_)) if !notice("resumed you") => {
                        if !now("write_file") {
                            call(
                                "write_file",
                                json!({"path":"src/lib.rs","content":"pub fn draft() {}\n"}),
                            )
                        } else {
                            // The operator pauses it mid-work; its next model
                            // call is never admitted.
                            assert!(
                                !desk.paused.swap(true, Ordering::SeqCst),
                                "a paused member made another model call"
                            );
                            operator = Some(Operator::Pause);
                            call("team_members", json!({}))
                        }
                    }
                    (true, Some(_)) => {
                        if !now("write_file") {
                            call(
                                "write_file",
                                json!({"path":"src/lib.rs","content":"pub fn draft() {}\npub fn half() {}\n"}),
                            )
                        } else {
                            assert!(
                                !desk.retired.swap(true, Ordering::SeqCst),
                                "a retired member made another model call"
                            );
                            operator = Some(Operator::RetireDuringCall);
                            text("never returned")
                        }
                    }
                }
            }
            _ => text("idle"),
        };
        (name, decision, operator)
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
        cancel: tokio_util::sync::CancellationToken,
        _: mpsc::Sender<Delta>,
    ) -> Result<Completion> {
        let (member, completion, operator) = self.decide(messages);
        let action = completion
            .calls
            .first()
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "text".into());
        self.desk.log.lock().unwrap().push((member, action));
        if let Some(operator) = operator {
            self.desk.operate(operator).await?;
        }
        if operator == Some(Operator::RetireDuringCall) {
            // A provider call in flight when its member is retired is
            // cancelled by the runtime instead of running to completion,
            // whether the runtime drops this call or cancels its token.
            let _open = OpenCall {
                started: Instant::now(),
                desk: self.desk.clone(),
            };
            tokio::select! {
                _ = cancel.cancelled() => anyhow::bail!("provider call cancelled"),
                _ = tokio::time::sleep(Duration::from_secs(20)) => {
                    return Ok(text("the call was never cancelled"));
                }
            }
        }
        Ok(completion)
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

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

#[tokio::test]
async fn the_operator_pauses_resumes_and_retires_one_writer_while_the_team_works() -> Result<()> {
    let home = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let project = workspace.path().join("project");
    std::fs::create_dir_all(project.join("src"))?;
    git(&project, &["init", "-q"]);
    std::fs::write(project.join("src/lib.rs"), "pub fn helper() {}\n")?;
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "base"]);

    let desk = Arc::new(Desk::default());
    let engine = Engine::with_components(
        home.path().into(),
        "default".into(),
        config(),
        Arc::new(TeamModel { desk: desk.clone() }),
        Arc::new(FileSandbox),
    )?;
    let scope = Scope::local();
    let store = engine.store(&scope).await?;
    let mut options = CooperativeOptions {
        limits: TeamLimits {
            max_members: 4,
            max_concurrency: 1,
            ..TeamLimits::default()
        },
        ..Default::default()
    };
    options.scope.allow_write = true;
    let task = match store
        .create_task_with_execution(
            &scope,
            &project.canonicalize()?.to_string_lossy(),
            None,
            "Rename the helper",
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
    };
    let team = task.team_id.clone().expect("team");
    for member in store.team_members(&team).await? {
        desk.ids.lock().unwrap().insert(
            member["name"].as_str().unwrap().to_string(),
            member["id"].as_str().unwrap().to_string(),
        );
    }
    let _ = desk.store.set(store.clone());
    let _ = desk.task.set(task.id.clone());
    let outcome = tokio::time::timeout(
        Duration::from_secs(60),
        engine.run_cooperative(
            scope.clone(),
            task.id.clone(),
            None,
            tokio_util::sync::CancellationToken::new(),
        ),
    )
    .await;
    let log = desk.log.lock().unwrap().clone();
    assert!(outcome.is_ok(), "team did not finish: {log:?}");
    outcome??;
    let task = store.task(&scope, &task.id).await?;
    assert_eq!(task.state, TaskState::Completed, "{log:?}");
    assert!(desk.paused.load(Ordering::SeqCst) && desk.resumed.load(Ordering::SeqCst));
    assert!(desk.retired.load(Ordering::SeqCst), "{log:?}");

    // The retired member's provider call was cancelled, not waited out.
    let cancelled = desk
        .cancelled_after
        .lock()
        .unwrap()
        .expect("call cancelled");
    assert!(cancelled < Duration::from_secs(10), "{cancelled:?}");

    // Nothing reached the team workspace: the writer never submitted.
    assert_eq!(
        std::fs::read_to_string(project.join("src/lib.rs"))?,
        "pub fn helper() {}\n"
    );
    // Its unsubmitted change is kept, and its worktree is gone.
    let patches = store
        .team_resource(&scope, &task.id, "patches", 0, 50)
        .await?
        .0;
    assert_eq!(patches.len(), 1, "{patches:?}");
    assert_eq!(patches[0]["kind"], "retained");
    assert!(
        patches[0]["summary"]
            .as_str()
            .is_some_and(|s| s.contains("operator retired")),
        "{patches:?}"
    );
    let (_, body) = store
        .team_patch(&team, patches[0]["id"].as_str().unwrap())
        .await?;
    assert!(
        String::from_utf8_lossy(&body).contains("+pub fn half() {}"),
        "the change made after the resume is kept"
    );
    assert!(store.team_worktrees(&team).await?.is_empty());

    let analyst = desk.ids.lock().unwrap()["analyst"].clone();
    let member = store
        .team_members(&team)
        .await?
        .into_iter()
        .find(|m| m["id"] == analyst.as_str())
        .unwrap();
    assert_eq!(member["state"], "terminated");
    assert_eq!(member["operator_hold"], "retired");
    let events = store.task_events(&task.id, 0, 1000).await?;
    let kinds: Vec<&str> = events.iter().filter_map(|e| e["kind"].as_str()).collect();
    for kind in [
        "team_member_paused",
        "team_member_resumed",
        "team_member_retired",
    ] {
        assert!(kinds.contains(&kind), "{kind}: {kinds:?}");
    }
    let interrupted = events
        .iter()
        .filter(|e| {
            e["kind"] == "team_activation_finished"
                && e["data"]["member_id"] == analyst.as_str()
                && e["data"]["state"] == "interrupted"
        })
        .count();
    assert_eq!(
        interrupted, 2,
        "the pause and the retirement each ended one activation"
    );
    std::mem::forget(home);
    std::mem::forget(workspace);
    Ok(())
}

//! Runtime task semantics: cancellation, safe-point corrections and approvals.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use anyhow::Result;
use async_trait::async_trait;
use layeragent_config::{Config, Model, Provider, Wire};
use layeragent_core::Engine;
use layeragent_protocol::task::{CorrectTask, TaskState};
use layeragent_protocol::*;
use layeragent_providers::{Delta, LanguageModel};
use layeragent_sandbox::{ExecOutput, ExecRequest, Sandbox};
use layeragent_storage::{Storage, TaskCreate};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::sync::{Notify, mpsc};

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

async fn new_task(store: &Storage, scope: &Scope, workspace: &std::path::Path) -> Result<Task> {
    match store
        .create_task(
            scope,
            &workspace.to_string_lossy(),
            None,
            "fixture task",
            "local",
            &new_id(),
            &"h".repeat(64),
        )
        .await?
    {
        TaskCreate::Created(task) | TaskCreate::Replayed(task) => Ok(task),
    }
}

async fn wait_for(
    store: &Storage,
    scope: &Scope,
    id: &str,
    predicate: impl Fn(TaskState) -> bool,
) -> Task {
    for _ in 0..400 {
        let task = store.task(scope, id).await.expect("task");
        if predicate(task.state) {
            return task;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("task did not reach the expected state");
}

async fn event_kinds(store: &Storage, id: &str) -> Vec<String> {
    store
        .task_events(id, 0, 1000)
        .await
        .expect("events")
        .into_iter()
        .filter_map(|event| event["kind"].as_str().map(str::to_string))
        .collect()
}

struct CancellableModel;
#[async_trait]
impl LanguageModel for CancellableModel {
    async fn infer(
        &self,
        _: &Config,
        _: &str,
        _: &[Message],
        _: &[ToolDefinition],
        cancel: tokio_util::sync::CancellationToken,
        _: mpsc::Sender<Delta>,
    ) -> Result<Completion> {
        cancel.cancelled().await;
        anyhow::bail!("cancelled by operator")
    }
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
        anyhow::bail!("no tool expected")
    }
}

#[tokio::test]
async fn cancel_during_response_is_confirmed_and_terminal() -> Result<()> {
    let home = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let engine = Engine::with_components(
        home.path().into(),
        "default".into(),
        config(),
        Arc::new(CancellableModel),
        Arc::new(NoopSandbox),
    )?;
    let scope = Scope::local();
    let store = engine.store(&scope).await?;
    let task = new_task(&store, &scope, workspace.path()).await?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let run = tokio::spawn({
        let engine = engine.clone();
        let scope = scope.clone();
        let task_id = task.id.clone();
        let cancel = cancel.clone();
        let workspace = workspace.path().to_string_lossy().into_owned();
        async move {
            engine
                .run_bound(
                    scope,
                    RunRequest {
                        session_id: None,
                        workspace: Some(workspace),
                        prompt: "wait forever".into(),
                        model: None,
                        effort: Default::default(),
                        bridge: false,
                        mode: Default::default(),
                        execution: Default::default(),
                        cooperative: None,
                    },
                    cancel,
                    task_id,
                )
                .await
        }
    });
    wait_for(&store, &scope, &task.id, |state| {
        state == TaskState::Running
    })
    .await;
    // Cancel without going through the HTTP route: the engine must still record
    // a requested cancellation, so the state cannot degrade to `blocked`.
    cancel.cancel();
    run.await?.expect_err("cancelled turn must fail");
    let finished = wait_for(&store, &scope, &task.id, TaskState::terminal).await;
    assert_eq!(finished.state, TaskState::Cancelled);
    assert_eq!(finished.cancellation.as_str(), "confirmed");
    assert!(
        event_kinds(&store, &task.id)
            .await
            .iter()
            .any(|kind| kind == "interrupted")
    );
    Ok(())
}

struct ToolGate {
    started: Arc<Notify>,
    release: Arc<Notify>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Sandbox for ToolGate {
    async fn execute(
        &self,
        r: ExecRequest,
        _: tokio_util::sync::CancellationToken,
        _: Option<mpsc::Sender<String>>,
    ) -> Result<ExecOutput> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(r.argv.last().map(String::as_str), Some("write_file"));
        self.started.notify_one();
        self.release.notified().await;
        let data: serde_json::Value = serde_json::from_str(r.stdin.as_deref().unwrap_or("{}"))?;
        tokio::fs::write(
            r.workspace.join("agent.txt"),
            data["content"].as_str().unwrap_or(""),
        )
        .await?;
        Ok(ExecOutput {
            code: Some(0),
            stdout: json!({"written":"agent.txt"}).to_string(),
            stderr: String::new(),
            interrupted: false,
            truncated: false,
        })
    }
}

struct CorrectionAwareModel {
    saw_correction: Arc<AtomicBool>,
    first_infer_saw_correction: Arc<AtomicBool>,
    inferences: Arc<AtomicUsize>,
}

#[async_trait]
impl LanguageModel for CorrectionAwareModel {
    async fn infer(
        &self,
        _: &Config,
        _: &str,
        messages: &[Message],
        _: &[ToolDefinition],
        _: tokio_util::sync::CancellationToken,
        _: mpsc::Sender<Delta>,
    ) -> Result<Completion> {
        let corrected = messages
            .iter()
            .any(|message| message.content.contains("Operator correction"));
        let n = self.inferences.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            self.first_infer_saw_correction
                .store(corrected, Ordering::SeqCst);
            return Ok(Completion {
                calls: vec![ToolCall {
                    provider_data: serde_json::Value::Null,
                    id: new_id(),
                    name: "write_file".into(),
                    arguments: json!({"path":"agent.txt","content":"original"}),
                }],
                ..Default::default()
            });
        }
        self.saw_correction.store(corrected, Ordering::SeqCst);
        Ok(Completion {
            text: "finished after correction".into(),
            ..Default::default()
        })
    }
}

#[tokio::test]
async fn correction_waits_for_the_active_tool_and_is_applied_once() -> Result<()> {
    let home = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let saw_correction = Arc::new(AtomicBool::new(false));
    let first_saw = Arc::new(AtomicBool::new(false));
    let mut cfg = config();
    cfg.policy.write = true;
    let engine = Engine::with_components(
        home.path().into(),
        "default".into(),
        cfg,
        Arc::new(CorrectionAwareModel {
            saw_correction: saw_correction.clone(),
            first_infer_saw_correction: first_saw.clone(),
            inferences: Arc::new(AtomicUsize::new(0)),
        }),
        Arc::new(ToolGate {
            started: started.clone(),
            release: release.clone(),
            calls: calls.clone(),
        }),
    )?;
    let scope = Scope::local();
    let store = engine.store(&scope).await?;
    let task = new_task(&store, &scope, workspace.path()).await?;
    let run = tokio::spawn({
        let engine = engine.clone();
        let scope = scope.clone();
        let task_id = task.id.clone();
        let workspace = workspace.path().to_string_lossy().into_owned();
        async move {
            engine
                .run_bound(
                    scope,
                    RunRequest {
                        session_id: None,
                        workspace: Some(workspace),
                        prompt: "write the file".into(),
                        model: None,
                        effort: Default::default(),
                        bridge: false,
                        mode: Default::default(),
                        execution: Default::default(),
                        cooperative: None,
                    },
                    tokio_util::sync::CancellationToken::new(),
                    task_id,
                )
                .await
        }
    });
    wait_for(&store, &scope, &task.id, |state| {
        state == TaskState::Running
    })
    .await;
    // The tool is blocked inside execute while the operator corrects the task.
    started.notified().await;
    let revision = store.task(&scope, &task.id).await?.revision;
    store
        .correct_task(
            &scope,
            &task.id,
            CorrectTask {
                instruction: "write the corrected content".into(),
                expected_revision: revision,
            },
        )
        .await?;
    // A stale client cannot queue a second correction with the old revision.
    assert!(
        store
            .correct_task(
                &scope,
                &task.id,
                CorrectTask {
                    instruction: "remove the file".into(),
                    expected_revision: revision,
                },
            )
            .await
            .is_err()
    );
    release.notify_one();
    run.await??;
    assert!(
        !first_saw.load(Ordering::SeqCst),
        "correction arrived while the first inference was in flight"
    );
    assert!(saw_correction.load(Ordering::SeqCst));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let kinds = event_kinds(&store, &task.id).await;
    assert!(kinds.iter().any(|kind| kind == "task_correction_queued"));
    assert!(kinds.iter().any(|kind| kind == "task_correction_applied"));
    assert_eq!(
        store.task(&scope, &task.id).await?.state,
        TaskState::Completed
    );
    Ok(())
}

struct ApprovalModel;
#[async_trait]
impl LanguageModel for ApprovalModel {
    async fn infer(
        &self,
        _: &Config,
        _: &str,
        messages: &[Message],
        _: &[ToolDefinition],
        _: tokio_util::sync::CancellationToken,
        _: mpsc::Sender<Delta>,
    ) -> Result<Completion> {
        if messages
            .last()
            .is_some_and(|message| message.role == "tool")
        {
            return Ok(Completion {
                text: "approved and done".into(),
                ..Default::default()
            });
        }
        Ok(Completion {
            calls: vec![ToolCall {
                provider_data: serde_json::Value::Null,
                id: new_id(),
                name: "write_file".into(),
                arguments: json!({"path":"agent.txt","content":"approved"}),
            }],
            ..Default::default()
        })
    }
}

struct FileSandbox;
#[async_trait]
impl Sandbox for FileSandbox {
    async fn execute(
        &self,
        r: ExecRequest,
        _: tokio_util::sync::CancellationToken,
        _: Option<mpsc::Sender<String>>,
    ) -> Result<ExecOutput> {
        let data: serde_json::Value = serde_json::from_str(r.stdin.as_deref().unwrap_or("{}"))?;
        tokio::fs::write(
            r.workspace.join("agent.txt"),
            data["content"].as_str().unwrap_or(""),
        )
        .await?;
        Ok(ExecOutput {
            code: Some(0),
            stdout: "{}".into(),
            stderr: String::new(),
            interrupted: false,
            truncated: false,
        })
    }
}

async fn wait_for_approval(store: &Storage, id: &str) -> (String, i64, String) {
    for _ in 0..400 {
        for event in store.task_events(id, 0, 1000).await.expect("events") {
            if event["kind"] == "approval_required" {
                return (
                    event["data"]["approval_id"]
                        .as_str()
                        .expect("approval id")
                        .into(),
                    event["data"]["expected_revision"]
                        .as_i64()
                        .expect("revision"),
                    event["data"]["arguments_hash"]
                        .as_str()
                        .expect("hash")
                        .into(),
                );
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("approval was not requested");
}

#[tokio::test]
async fn stale_or_second_approval_is_rejected() -> Result<()> {
    let home = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let mut cfg = config();
    cfg.policy.write = true;
    cfg.policy.approval_tools = vec!["write_file".into()];
    let engine = Engine::with_components(
        home.path().into(),
        "default".into(),
        cfg,
        Arc::new(ApprovalModel),
        Arc::new(FileSandbox),
    )?;
    let scope = Scope::local();
    let store = engine.store(&scope).await?;
    let task = new_task(&store, &scope, workspace.path()).await?;
    let run = tokio::spawn({
        let engine = engine.clone();
        let scope = scope.clone();
        let task_id = task.id.clone();
        let workspace = workspace.path().to_string_lossy().into_owned();
        async move {
            engine
                .run_bound(
                    scope,
                    RunRequest {
                        session_id: None,
                        workspace: Some(workspace),
                        prompt: "write with approval".into(),
                        model: None,
                        effort: Default::default(),
                        bridge: false,
                        mode: Default::default(),
                        execution: Default::default(),
                        cooperative: None,
                    },
                    tokio_util::sync::CancellationToken::new(),
                    task_id,
                )
                .await
        }
    });
    let (id, expected_revision, arguments_hash) = wait_for_approval(&store, &task.id).await;
    assert_eq!(
        store.task(&scope, &task.id).await?.state,
        TaskState::AwaitingPermission
    );
    let stale = ApprovalDecision {
        expected_revision: expected_revision - 1,
        arguments_hash: arguments_hash.clone(),
        allow: true,
    };
    assert!(engine.approve(&scope, None, &id, &stale).await.is_err());
    let changed_arguments = ApprovalDecision {
        expected_revision,
        arguments_hash: "0".repeat(64),
        allow: true,
    };
    assert!(
        engine
            .approve(&scope, None, &id, &changed_arguments)
            .await
            .is_err()
    );
    let decision = ApprovalDecision {
        expected_revision,
        arguments_hash,
        allow: true,
    };
    engine.approve(&scope, None, &id, &decision).await?;
    // A second tab quoting the same decision arrives after the first consumed it.
    assert!(engine.approve(&scope, None, &id, &decision).await.is_err());
    run.await??;
    assert_eq!(
        store.task(&scope, &task.id).await?.state,
        TaskState::Completed
    );
    Ok(())
}

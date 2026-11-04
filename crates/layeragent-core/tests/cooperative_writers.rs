//! Cooperative writers with a scripted model and a real Git repository: a
//! member switches from reading to writing, changes files in its own worktree,
//! submits a patch, and another member's approval integrates it into the team
//! workspace. The sandbox is a test double; it does not certify confinement.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use anyhow::Result;
use async_trait::async_trait;
use layeragent_config::{Config, Model, Provider, Wire};
use layeragent_core::Engine;
use layeragent_protocol::task::TaskState;
use layeragent_protocol::*;
use layeragent_providers::{Delta, LanguageModel, ProviderAdmission};
use layeragent_sandbox::{ExecOutput, ExecRequest, Sandbox};
use layeragent_storage::TaskCreate;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
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

/// Performs `write_file` directly and records where and with which policy.
#[derive(Default)]
struct FileSandbox {
    writes: Mutex<Vec<(PathBuf, bool)>>,
}
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
        self.writes
            .lock()
            .unwrap()
            .push((request.workspace.clone(), request.policy.write));
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

/// One step of the team, as (member, action, tools it was offered).
type Step = (String, String, Vec<String>);

struct WriterModel {
    ids: Arc<Mutex<BTreeMap<String, String>>>,
    log: Arc<Mutex<Vec<Step>>>,
    /// The operator edits the same line in this team workspace right before
    /// the first approval, so the first patch no longer applies.
    conflict: Option<PathBuf>,
    /// The analyst hands its half-done work to the reviewer, who finishes it;
    /// the coordinator reviews.
    handoff: bool,
}

impl WriterModel {
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
            .and_then(|m| serde_json::from_str::<Value>(&m.content).ok())
            .unwrap_or_default();
        let peer = |purpose: &str| {
            messages.iter().rev().find(|m| {
                m.content.starts_with("Peer reference message")
                    && header(&m.content, "purpose") == purpose
            })
        };
        let ever = |tool: &str| {
            messages
                .iter()
                .any(|m| m.tool_calls.iter().any(|c| c.name == tool))
        };
        let count = |tool: &str| {
            messages
                .iter()
                .flat_map(|m| m.tool_calls.iter())
                .filter(|c| c.name == tool)
                .count()
        };
        let ids = self.ids.lock().unwrap().clone();
        // Reviews the latest review request addressed to this member, once.
        let review = || -> Option<Completion> {
            let request = peer("verification_request")?;
            let body = request
                .content
                .split("\n--- body ---\n")
                .nth(1)
                .unwrap_or("");
            let patch = body
                .split("review patch ")
                .nth(1)
                .and_then(|rest| rest.split_whitespace().next())
                .unwrap_or("")
                .to_string();
            let reviewed = messages.iter().any(|m| {
                m.tool_calls.iter().any(|c| {
                    c.name == "team_patch_review" && c.arguments["patch_id"] == patch.as_str()
                })
            });
            if reviewed {
                return None;
            }
            if last_tool.as_deref() != Some("team_patch_show") {
                return Some(call("team_patch_show", json!({"patch_id":patch})));
            }
            assert!(
                last_result["diff"]
                    .as_str()
                    .is_some_and(|diff| diff.contains("+pub fn")),
                "reviewer reads the real diff: {last_result}"
            );
            if let Some(project) = &self.conflict
                && !ever("team_patch_review")
            {
                std::fs::write(project.join("src/lib.rs"), "pub fn operator() {}\n").unwrap();
            }
            Some(call(
                "team_patch_review",
                json!({"patch_id":patch,"verdict":"approve","reason":"the change is complete"}),
            ))
        };
        let handed = peer("handoff").map(|m| header(&m.content, "work"));
        let decision = match name.as_str() {
            "coordinator"
                if self.handoff
                    && last_tool.as_deref() != Some("team_work_create")
                    && ever("team_work_create") =>
            {
                let noticed = activation
                    .iter()
                    .any(|m| m.content.starts_with("Runtime supervisor notice"));
                match review() {
                    Some(step) => step,
                    None if noticed && !ever("team_close") => {
                        call("team_close", json!({"reason":"the change is integrated"}))
                    }
                    None => text("coordinating"),
                }
            }
            "analyst" if self.handoff && writing => {
                let work = peer("observation")
                    .map(|m| header(&m.content, "work"))
                    .unwrap_or_default();
                if ever("team_message") {
                    text("handed off")
                } else if last_tool.as_deref() != Some("write_file") {
                    call(
                        "write_file",
                        json!({"path":"src/lib.rs","content":"pub fn half() {}\n"}),
                    )
                } else {
                    call(
                        "team_message",
                        json!({"recipient":ids["reviewer"],"purpose":"handoff","work_id":work,"body":"I started the rename; please finish it."}),
                    )
                }
            }
            "reviewer" if self.handoff => match (handed, writing) {
                (None, _) => text("nothing yet"),
                (Some(work), false) => {
                    if !ever("team_work_claim") {
                        call(
                            "team_work_claim",
                            json!({"work_id":work,"expected_revision":3}),
                        )
                    } else if !ever("team_access") {
                        // The reserved paths came with the work.
                        call(
                            "team_access",
                            json!({"mode":"write","work_id":work,"reason":"finishing handed-over work"}),
                        )
                    } else {
                        text("switching to write")
                    }
                }
                (Some(work), true) => {
                    let received = messages.iter().any(|m| {
                        m.content.starts_with("Runtime worktree notice")
                            && m.content.contains("is now in your worktree")
                    });
                    if ever("team_patch_submit") {
                        text("submitted")
                    } else if last_tool.as_deref() != Some("write_file") {
                        assert!(received, "the handed-over work reached this worktree first");
                        call(
                            "write_file",
                            json!({"path":"src/lib.rs","content":"pub fn finished() {}\n"}),
                        )
                    } else {
                        call(
                            "team_patch_submit",
                            json!({"work_id":work,"summary":"finish the rename","reviewer":ids["coordinator"]}),
                        )
                    }
                }
            },
            "coordinator" => {
                let noticed = activation
                    .iter()
                    .any(|m| m.content.starts_with("Runtime supervisor notice"));
                if !ever("team_work_create") {
                    call(
                        "team_work_create",
                        json!({"title":"rename the helper","deliverable":"integrated patch","assignee":ids["analyst"],"close_conditions":"patch integrated"}),
                    )
                } else if last_tool.as_deref() == Some("team_work_create") {
                    call(
                        "team_message",
                        json!({"recipient":ids["analyst"],"purpose":"observation","body":"Please rename the helper in src/lib.rs.","work_id":last_result["id"]}),
                    )
                } else if noticed && !ever("team_close") {
                    call("team_close", json!({"reason":"the change is integrated"}))
                } else {
                    text("coordinating")
                }
            }
            "analyst" => {
                let work = peer("observation").map(|m| header(&m.content, "work"));
                match (writing, work) {
                    (_, None) => text("no work yet"),
                    (true, Some(work)) => {
                        let submit = call(
                            "team_patch_submit",
                            json!({"work_id":work,"summary":"rename helper to renamed","reviewer":ids["reviewer"]}),
                        );
                        let refused = messages.iter().any(|m| {
                            m.content.starts_with("Peer reference message")
                                && m.content.contains("no longer applies")
                        });
                        // The conflict reaches the author as a worktree notice when
                        // its activation starts, or from the sync a submission does.
                        let marked = messages.iter().any(|m| {
                            (m.content.starts_with("Runtime worktree notice")
                                && m.content.contains("conflict in: src/lib.rs"))
                                || (m.role == "tool"
                                    && (m.content.contains("\"conflicts\":[\"src/lib.rs\"]")
                                        || m.content.contains("conflict markers")))
                        });
                        match count("team_patch_submit") {
                            0 if last_tool.as_deref() != Some("write_file") => call(
                                "write_file",
                                json!({"path":"src/lib.rs","content":"pub fn renamed() {}\n"}),
                            ),
                            0 => submit,
                            // Resubmitting syncs the worktree first and reports the conflict.
                            1 if refused => submit,
                            2 if marked && last_tool.as_deref() != Some("write_file") => call(
                                "write_file",
                                json!({"path":"src/lib.rs","content":"pub fn renamed_for_operator() {}\n"}),
                            ),
                            2 if marked => submit,
                            _ => text("patch submitted"),
                        }
                    }
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
                            text("switching to write")
                        }
                    }
                }
            }
            "reviewer" => review().unwrap_or_else(|| text("nothing to review")),
            _ => text("idle"),
        };
        (name, decision)
    }
}

#[async_trait]
impl LanguageModel for WriterModel {
    async fn infer(
        &self,
        _: &Config,
        _: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        _: tokio_util::sync::CancellationToken,
        _: mpsc::Sender<Delta>,
    ) -> Result<Completion> {
        let (name, completion) = self.decide(messages);
        let action = completion
            .calls
            .first()
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "text".into());
        self.log.lock().unwrap().push((
            name,
            action,
            tools.iter().map(|tool| tool.name.clone()).collect(),
        ));
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

struct Outcome {
    engine: Arc<Engine>,
    store: layeragent_storage::Storage,
    scope: Scope,
    task: Task,
    team: String,
    project: PathBuf,
    steps: Vec<Step>,
    summary: Vec<(String, String)>,
    writes: Vec<(PathBuf, bool)>,
}

async fn run_writers(conflict: bool, handoff: bool) -> Result<Outcome> {
    let home = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let project = workspace.path().join("project");
    std::fs::create_dir_all(project.join("src"))?;
    git(&project, &["init", "-q"]);
    std::fs::write(project.join("src/lib.rs"), "pub fn helper() {}\n")?;
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "base"]);
    // Uncommitted operator work elsewhere must survive the integration.
    std::fs::write(project.join("NOTES.md"), "operator draft\n")?;

    let ids = Arc::new(Mutex::new(BTreeMap::new()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let sandbox = Arc::new(FileSandbox::default());
    let engine = Engine::with_components(
        home.path().into(),
        "default".into(),
        config(),
        Arc::new(WriterModel {
            ids: ids.clone(),
            log: log.clone(),
            conflict: conflict.then(|| project.clone()),
            handoff,
        }),
        sandbox.clone(),
    )?;
    let scope = Scope::local();
    let store = engine.store(&scope).await?;
    let mut options = CooperativeOptions {
        limits: TeamLimits {
            max_members: 3,
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
        ids.lock().unwrap().insert(
            member["name"].as_str().unwrap().to_string(),
            member["id"].as_str().unwrap().to_string(),
        );
    }
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        engine.run_cooperative(
            scope.clone(),
            task.id.clone(),
            None,
            tokio_util::sync::CancellationToken::new(),
        ),
    )
    .await;
    let steps = log.lock().unwrap().clone();
    let summary: Vec<(String, String)> = steps
        .iter()
        .map(|(who, what, _)| (who.clone(), what.clone()))
        .collect();
    assert!(outcome.is_ok(), "team did not finish: {summary:?}");
    outcome??;
    let task = store.task(&scope, &task.id).await?;
    let writes = sandbox.writes.lock().unwrap().clone();
    // Keep directories alive until assertions finish reading them.
    std::mem::forget(home);
    std::mem::forget(workspace);
    Ok(Outcome {
        engine,
        store,
        scope,
        task,
        team,
        project,
        steps,
        summary,
        writes,
    })
}

#[tokio::test]
async fn a_member_switches_to_writing_and_its_approved_patch_reaches_the_team_workspace()
-> Result<()> {
    let Outcome {
        engine,
        store,
        scope,
        task,
        team,
        project,
        steps,
        summary,
        writes,
    } = run_writers(false, false).await?;
    assert_eq!(task.state, TaskState::Completed, "{summary:?}");
    // The approved change is in the team workspace, uncommitted, next to the
    // operator's own work.
    assert_eq!(
        std::fs::read_to_string(project.join("src/lib.rs"))?,
        "pub fn renamed() {}\n"
    );
    assert_eq!(
        std::fs::read_to_string(project.join("NOTES.md"))?,
        "operator draft\n"
    );
    // The file was written in the member's worktree with write policy, never
    // in the team workspace.
    assert_eq!(writes.len(), 1);
    assert!(writes[0].1, "writing activation has write policy");
    assert_ne!(writes[0].0, project.canonicalize()?);
    assert!(writes[0].0.starts_with(engine.state_home.join("teams")));

    // Tools follow each activation's access.
    let offered = |who: &str, action: &str| {
        steps
            .iter()
            .find(|(w, a, _)| w == who && a == action)
            .map(|(_, _, tools)| tools.clone())
            .unwrap_or_else(|| panic!("{who} never did {action}: {summary:?}"))
    };
    let reading = offered("analyst", "team_access");
    assert!(!reading.contains(&"write_file".to_string()));
    assert!(!reading.contains(&"team_patch_submit".to_string()));
    let writing = offered("analyst", "write_file");
    assert!(writing.contains(&"team_patch_submit".to_string()));
    for (who, _, tools) in &steps {
        if who != "analyst" {
            assert!(
                !tools.contains(&"write_file".to_string()),
                "{who} only reads"
            );
        }
    }

    // Receipts for the patch and its integration; the work is completed.
    let evidence = store
        .team_resource(&scope, &task.id, "evidence", 0, 50)
        .await?
        .0;
    for tool in ["patch", "integration"] {
        assert!(evidence.iter().any(|e| e["tool"] == tool), "{tool} receipt");
    }
    let patches = store
        .team_resource(&scope, &task.id, "patches", 0, 50)
        .await?
        .0;
    assert_eq!(patches.len(), 1);
    assert_eq!(patches[0]["state"], "integrated");
    assert_eq!(patches[0]["files"], json!(["src/lib.rs"]));
    let work = store
        .team_resource(&scope, &task.id, "work", 0, 10)
        .await?
        .0;
    assert_eq!(work[0]["state"], "completed");
    // An ended team leaves no worktree behind.
    assert!(!engine.state_home.join("teams").join(&team).exists());
    assert!(store.team_worktrees(&team).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn a_patch_that_no_longer_applies_returns_to_its_author_who_resolves_it() -> Result<()> {
    let Outcome {
        store,
        scope,
        task,
        project,
        summary,
        writes,
        ..
    } = run_writers(true, false).await?;
    assert_eq!(task.state, TaskState::Completed, "{summary:?}");
    // Nothing of the refused patch was applied over the operator's edit; the
    // resolved second patch was.
    assert_eq!(
        std::fs::read_to_string(project.join("src/lib.rs"))?,
        "pub fn renamed_for_operator() {}\n"
    );
    assert_eq!(writes.len(), 2, "{summary:?}");
    let patches = store
        .team_resource(&scope, &task.id, "patches", 0, 50)
        .await?
        .0;
    let states: Vec<&str> = patches
        .iter()
        .map(|p| p["state"].as_str().unwrap())
        .collect();
    // The refused patch is kept, superseded by the resolved resubmission.
    assert_eq!(states, ["superseded", "integrated"], "{summary:?}");
    let events = store
        .team_resource(&scope, &task.id, "events", 0, 200)
        .await?
        .0;
    assert!(
        events.iter().any(|e| e["kind"] == "team_patch_conflicted"),
        "the first approval found the operator's edit"
    );
    // The sync before resubmitting left the conflict for the author.
    assert!(
        summary
            .iter()
            .filter(|(who, what)| who == "analyst" && what == "team_patch_submit")
            .count()
            == 3,
        "{summary:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_handoff_carries_unsubmitted_changes_into_the_recipients_worktree() -> Result<()> {
    let Outcome {
        store,
        scope,
        task,
        project,
        summary,
        writes,
        ..
    } = run_writers(false, true).await?;
    assert_eq!(task.state, TaskState::Completed, "{summary:?}");
    assert_eq!(
        std::fs::read_to_string(project.join("src/lib.rs"))?,
        "pub fn finished() {}\n"
    );
    // Two members wrote, each in its own worktree.
    assert_eq!(writes.len(), 2, "{summary:?}");
    assert_ne!(writes[0].0, writes[1].0);
    let patches = store
        .team_resource(&scope, &task.id, "patches", 0, 50)
        .await?
        .0;
    let kinds: Vec<(&str, &str)> = patches
        .iter()
        .map(|p| (p["kind"].as_str().unwrap(), p["state"].as_str().unwrap()))
        .collect();
    // The carried work was received, and nothing was left behind with the
    // sender: no retained patch when the team ended.
    assert_eq!(
        kinds,
        [("handoff", "received"), ("submission", "integrated")],
        "{summary:?}"
    );
    Ok(())
}

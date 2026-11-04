//! Teams on more than one model, with scripted models and a real Git
//! repository: a cross-model review and a consultant. The writer runs one
//! model; the team requires a different model to approve its patch, so the
//! coordinator consults another model, whose decision integrates the patch
//! and answers the consultation. A consultation that runs out of budget ends
//! the consultation, not the team. The sandbox is a test double; it does not
//! certify confinement.
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

fn model(id: &str) -> Model {
    Model {
        provider: "fixture".into(),
        id: id.into(),
        context_window: 32768,
        tools: true,
        emulate_tools: false,
        input_per_million: None,
        output_per_million: None,
        parameters: Default::default(),
    }
}

/// Two models from different vendors behind one fixture provider.
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
    c.models.insert("opus".into(), model("claude-opus-5-5"));
    c.models
        .insert("deepseek".into(), model("deepseek/deepseek-chat"));
    // Configured, but outside the operator's provider allowlist.
    c.providers.insert(
        "elsewhere".into(),
        Provider {
            wire: Wire::OpenaiChat,
            base_url: "https://models.example.invalid/v1".into(),
            key_env: None,
            oauth: None,
            headers: BTreeMap::new(),
        },
    );
    c.models.insert(
        "remote".into(),
        Model {
            provider: "elsewhere".into(),
            ..model("remote-model")
        },
    );
    c.privacy.allowed_providers = vec!["fixture".into()];
    c.primary = "opus".into();
    c
}

/// Performs `write_file` directly in the requested workspace.
#[derive(Default)]
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

fn body(content: &str) -> &str {
    content
        .split("\n--- body ---\n")
        .nth(1)
        .and_then(|rest| rest.split("\n--- end of message").next())
        .unwrap_or("")
}

/// The first whitespace-separated word after `marker` in `text`.
fn word_after<'a>(text: &'a str, marker: &str) -> &'a str {
    text.split(marker)
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or("")
}

/// One step of the team: member, model alias, action and tools offered.
#[derive(Clone, Debug)]
struct Step {
    member: String,
    model: String,
    action: String,
    tools: Vec<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Scenario {
    /// The writer's own-model reviewer cannot approve; a consultant on
    /// another model reviews the patch.
    Review,
    /// The consultant spends its budget before answering.
    Overspend,
}

struct TeamModel {
    ids: Arc<Mutex<BTreeMap<String, String>>>,
    log: Arc<Mutex<Vec<Step>>>,
    scenario: Scenario,
}

impl TeamModel {
    fn decide(&self, messages: &[Message]) -> (String, Completion) {
        let start = messages
            .iter()
            .rposition(|m| {
                m.role == "user"
                    && (m.content.starts_with("Cooperative template")
                        || m.content.starts_with("Cooperative consultation"))
            })
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
        let peer = |purpose: &str| {
            messages.iter().rev().find(|m| {
                m.content.starts_with("Peer reference message")
                    && header(&m.content, "purpose") == purpose
            })
        };
        let notice = |needle: &str| {
            messages.iter().any(|m| {
                m.content.starts_with("Runtime supervisor notice") && m.content.contains(needle)
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
        let decision = match (self.scenario, name.as_str()) {
            (Scenario::Review, "coordinator") => {
                let ready = peer("observation").map(|m| body(&m.content).to_string());
                if !ever("team_work_create") {
                    call(
                        "team_work_create",
                        json!({"title":"rename the helper","deliverable":"integrated patch","assignee":ids["analyst"],"close_conditions":"patch integrated"}),
                    )
                } else if last_tool.as_deref() == Some("team_work_create") {
                    let work = serde_json::from_str::<Value>(&last_result).unwrap_or_default();
                    call(
                        "team_message",
                        json!({"recipient":ids["analyst"],"purpose":"observation","body":"Please rename the helper in src/lib.rs.","work_id":work["id"]}),
                    )
                } else if let Some(ready) = ready.filter(|_| !ever("team_consult")) {
                    call(
                        "team_consult",
                        json!({"question":"Is this rename correct and complete?","model":"deepseek","patch_id":word_after(&ready, "patch "),"max_tokens":20000}),
                    )
                } else if peer("response").is_some() && !ever("team_close") {
                    call("team_close", json!({"reason":"the rename is integrated"}))
                } else {
                    text("coordinating")
                }
            }
            (Scenario::Review, "analyst") => {
                let work = messages
                    .iter()
                    .find(|m| {
                        m.content.starts_with("Peer reference message")
                            && header(&m.content, "purpose") == "observation"
                    })
                    .map(|m| header(&m.content, "work"));
                match (writing, work) {
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
                            text("waiting for the review")
                        }
                    }
                    (true, Some(work)) => match count("team_patch_submit") {
                        _ if !ever("write_file") => call(
                            "write_file",
                            json!({"path":"src/lib.rs","content":"pub fn renamed() {}\n"}),
                        ),
                        // The reviewer runs the same model: refused before asking.
                        0 => call(
                            "team_patch_submit",
                            json!({"work_id":work,"summary":"rename helper to renamed","reviewer":ids["reviewer"]}),
                        ),
                        1 => {
                            assert!(
                                last_result.contains("cross-model review"),
                                "same-model reviewer refused: {last_result}"
                            );
                            call(
                                "team_patch_submit",
                                json!({"work_id":work,"summary":"rename helper to renamed"}),
                            )
                        }
                        _ if !ever("team_message") => {
                            let patch = serde_json::from_str::<Value>(&last_result)
                                .unwrap_or_default()["patch_id"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string();
                            call(
                                "team_message",
                                json!({"recipient":ids["coordinator"],"purpose":"observation","body":format!("patch {patch} is ready; it needs a reviewer on another model")}),
                            )
                        }
                        _ => text("patch submitted"),
                    },
                }
            }
            (Scenario::Review, "consultant-1") => {
                let question = peer("question").expect("the question reached the consultant");
                let patch = word_after(body(&question.content), "Patch to review: ");
                if !ever("team_patch_show") {
                    call("team_patch_show", json!({"patch_id":patch}))
                } else if !ever("team_patch_review") {
                    assert!(
                        last_result.contains("+pub fn renamed"),
                        "the consultant reads the real diff: {last_result}"
                    );
                    call(
                        "team_patch_review",
                        json!({"patch_id":patch,"verdict":"approve","reason":"the rename is complete"}),
                    )
                } else {
                    text("answered")
                }
            }
            (Scenario::Overspend, "coordinator") => {
                if !ever("team_consult") {
                    call(
                        "team_consult",
                        json!({"question":"Is the parser safe?","model":"remote"}),
                    )
                } else if count("team_consult") == 1 {
                    assert!(
                        last_result.contains("outbound allowlist"),
                        "a model outside the allowlist is refused before it joins: {last_result}"
                    );
                    call(
                        "team_consult",
                        json!({"question":"Is the parser safe?","model":"deepseek","max_tokens":5000}),
                    )
                } else if notice("retired without answering") && !ever("team_close") {
                    call(
                        "team_close",
                        json!({"reason":"the consultation ended without an answer"}),
                    )
                } else {
                    text("waiting")
                }
            }
            (Scenario::Overspend, "consultant-1") => call("team_members", json!({})),
            _ => text("idle"),
        };
        (name, decision)
    }
}

#[async_trait]
impl LanguageModel for TeamModel {
    async fn infer(
        &self,
        _: &Config,
        selected: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        _: tokio_util::sync::CancellationToken,
        _: mpsc::Sender<Delta>,
    ) -> Result<Completion> {
        let (member, completion) = self.decide(messages);
        let action = completion
            .calls
            .first()
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "text".into());
        self.log.lock().unwrap().push(Step {
            member,
            model: selected.to_string(),
            action,
            tools: tools.iter().map(|tool| tool.name.clone()).collect(),
        });
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
        // The other model is expensive in the overspend scenario.
        let heavy = self.scenario == Scenario::Overspend && selected == "deepseek";
        completion.usage.input_tokens = if heavy { 6000 } else { 40 };
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
    store: layeragent_storage::Storage,
    scope: Scope,
    task: Task,
    team: String,
    project: PathBuf,
    steps: Vec<Step>,
}

async fn run_team(scenario: Scenario) -> Result<Outcome> {
    let home = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let project = workspace.path().join("project");
    std::fs::create_dir_all(project.join("src"))?;
    git(&project, &["init", "-q"]);
    std::fs::write(project.join("src/lib.rs"), "pub fn helper() {}\n")?;
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "base"]);

    let ids = Arc::new(Mutex::new(BTreeMap::new()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let engine = Engine::with_components(
        home.path().into(),
        "default".into(),
        config(),
        Arc::new(TeamModel {
            ids: ids.clone(),
            log: log.clone(),
            scenario,
        }),
        Arc::new(FileSandbox),
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
    if scenario == Scenario::Review {
        options.scope.allow_write = true;
        options.cross_model_review = true;
    }
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
    let summary: Vec<(String, String, String)> = steps
        .iter()
        .map(|s| (s.member.clone(), s.model.clone(), s.action.clone()))
        .collect();
    assert!(outcome.is_ok(), "team did not finish: {summary:?}");
    outcome??;
    let task = store.task(&scope, &task.id).await?;
    std::mem::forget(home);
    std::mem::forget(workspace);
    Ok(Outcome {
        store,
        scope,
        task,
        team,
        project,
        steps,
    })
}

fn summary(steps: &[Step]) -> Vec<(String, String, String)> {
    steps
        .iter()
        .map(|s| (s.member.clone(), s.model.clone(), s.action.clone()))
        .collect()
}

#[tokio::test]
async fn a_consultant_on_another_model_approves_what_the_writers_model_cannot() -> Result<()> {
    let Outcome {
        store,
        scope,
        task,
        team,
        project,
        steps,
    } = run_team(Scenario::Review).await?;
    let summary = summary(&steps);
    assert_eq!(task.state, TaskState::Completed, "{summary:?}");
    assert_eq!(
        std::fs::read_to_string(project.join("src/lib.rs"))?,
        "pub fn renamed() {}\n"
    );

    // Every member ran the team's model; only the consultant ran the other.
    for step in &steps {
        let expected = if step.member == "consultant-1" {
            "deepseek"
        } else {
            "opus"
        };
        assert_eq!(step.model, expected, "{summary:?}");
    }
    // The consultant reads and decides; it gets no write or team-shaping tools.
    let consultant_tools = &steps
        .iter()
        .find(|s| s.member == "consultant-1")
        .expect("the consultant ran")
        .tools;
    for tool in ["team_patch_show", "team_patch_review", "team_message"] {
        assert!(consultant_tools.contains(&tool.to_string()), "{tool}");
    }
    for tool in [
        "write_file",
        "team_consult",
        "team_add_member",
        "team_work_claim",
        "team_access",
        "team_patch_submit",
        "team_close",
    ] {
        assert!(!consultant_tools.contains(&tool.to_string()), "{tool}");
    }
    let coordinator_tools = &steps
        .iter()
        .find(|s| s.member == "coordinator")
        .expect("coordinator ran")
        .tools;
    assert!(coordinator_tools.contains(&"team_consult".to_string()));

    let patches = store
        .team_resource(&scope, &task.id, "patches", 0, 50)
        .await?
        .0;
    assert_eq!(patches.len(), 1, "{patches:?}");
    assert_eq!(patches[0]["state"], "integrated");
    assert_eq!(patches[0]["author_model"], "claude-opus-5-5");
    assert_eq!(patches[0]["reviewer_model"], "deepseek-chat");

    let members = store.team_members(&team).await?;
    assert_eq!(members.len(), 4, "the consultant joined beyond max_members");
    let consultant = members
        .iter()
        .find(|m| m["role"] == "consultant")
        .expect("consultant row");
    assert_eq!(consultant["state"], "terminated");
    assert_eq!(consultant["consult"]["outcome"], "answered");
    assert_eq!(consultant["model"], "deepseek");
    assert_eq!(consultant["last_model"], "deepseek");
    let events = store
        .team_resource(&scope, &task.id, "events", 0, 400)
        .await?
        .0;
    for kind in ["team_consultant_joined", "team_consultant_retired"] {
        assert!(events.iter().any(|e| e["kind"] == kind), "{kind}");
    }
    Ok(())
}

#[tokio::test]
async fn a_consultation_that_runs_out_of_budget_does_not_fail_the_team() -> Result<()> {
    let Outcome {
        store,
        task,
        team,
        steps,
        ..
    } = run_team(Scenario::Overspend).await?;
    let summary = summary(&steps);
    assert_eq!(task.state, TaskState::Completed, "{summary:?}");
    assert_eq!(
        store.team_snapshot_by_id(&team).await?.state,
        "terminated",
        "{summary:?}"
    );
    let consultant = store
        .team_members(&team)
        .await?
        .into_iter()
        .find(|m| m["role"] == "consultant")
        .expect("consultant row");
    assert_eq!(consultant["consult"]["outcome"], "budget_exhausted");
    assert_eq!(
        consultant["model"], "deepseek",
        "the refused model never joined"
    );
    assert_eq!(consultant["state"], "terminated");
    assert!(
        consultant["consult"]["tokens_used"]
            .as_i64()
            .is_some_and(|used| used >= 5000)
    );
    // One call was admitted; the next one was refused before reaching the model.
    assert_eq!(
        steps.iter().filter(|s| s.member == "consultant-1").count(),
        1,
        "{summary:?}"
    );
    Ok(())
}

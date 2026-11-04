//! Spending controls with a scripted model: a task with a budget is told
//! as it crosses shares of it, sees its spend and the configured models
//! with budget_status, delegates reading to a cheaper model, and its usage
//! report breaks the spend down by model and agent depth. The operator's
//! rules per model hold for delegated work.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use anyhow::Result;
use async_trait::async_trait;
use layeragent_config::{Config, Model, ModelTeamAccess, Provider, Wire};
use layeragent_core::Engine;
use layeragent_protocol::*;
use layeragent_providers::{Delta, LanguageModel};
use layeragent_sandbox::{ExecOutput, ExecRequest, Sandbox};
use layeragent_storage::TaskCreate;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn model(id: &str, input: f64, output: f64, access: ModelTeamAccess) -> Model {
    Model {
        provider: "fixture".into(),
        id: id.into(),
        context_window: 32768,
        tools: true,
        emulate_tools: false,
        input_per_million: Some(input),
        output_per_million: Some(output),
        parameters: Default::default(),
        notes: None,
        team_access: access,
        cache_read_per_million: None,
        cache_write_per_million: None,
        prompt_cache: false,
    }
}

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
        "premium".into(),
        model("premium-model", 10.0, 10.0, ModelTeamAccess::Write),
    );
    let mut cheap = model("cheap-model", 0.5, 1.0, ModelTeamAccess::Read);
    cheap.notes = Some("fine for bulk reading".into());
    c.models.insert("cheap".into(), cheap);
    c.models.insert(
        "outside".into(),
        model("outside-model", 0.1, 0.1, ModelTeamAccess::None),
    );
    c.primary = "premium".into();
    c.policy.shell = false;
    c.auto_skills = false;
    c.policy.allowed_tools = vec![
        "budget_status".into(),
        "spawn_agent".into(),
        "wait_agent".into(),
    ];
    c
}

struct NoSandbox;
#[async_trait]
impl Sandbox for NoSandbox {
    async fn execute(
        &self,
        _: ExecRequest,
        _: CancellationToken,
        _: Option<mpsc::Sender<String>>,
    ) -> Result<ExecOutput> {
        anyhow::bail!("no sandboxed tool expected")
    }
}

fn call(name: &str, arguments: Value, input: u64, output: u64) -> Completion {
    Completion {
        calls: vec![ToolCall {
            provider_data: Value::Null,
            id: new_id(),
            name: name.into(),
            arguments,
        }],
        usage: Usage {
            input_tokens: input,
            output_tokens: output,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn text(body: &str, input: u64, output: u64) -> Completion {
    Completion {
        text: body.into(),
        usage: Usage {
            input_tokens: input,
            output_tokens: output,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// What the scripted model saw, for the assertions.
#[derive(Default)]
struct Seen {
    status: Vec<Value>,
    notices: Vec<String>,
    child_models: Vec<String>,
    refusals: Vec<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Script {
    /// Spend against a budget, delegate to the cheap model, stop when told.
    Budget,
    /// Try models the operator does not allow for this delegation.
    Rules,
}

struct Scripted {
    script: Script,
    seen: Arc<Mutex<Seen>>,
}

#[async_trait]
impl LanguageModel for Scripted {
    async fn infer(
        &self,
        _: &Config,
        selected: &str,
        messages: &[Message],
        _: &[ToolDefinition],
        _: CancellationToken,
        _: mpsc::Sender<Delta>,
    ) -> Result<Completion> {
        let mut seen = self.seen.lock().unwrap();
        let first = messages
            .iter()
            .find(|m| m.role == "user")
            .map(|m| m.content.clone())
            .unwrap_or_default();
        if first.contains("summarize the notes") {
            seen.child_models.push(selected.to_string());
            return Ok(text("the notes say ship on friday", 2_000, 100));
        }
        let last = messages
            .last()
            .cloned()
            .unwrap_or_else(|| Message::text("user", ""));
        let tools: Vec<String> = messages
            .iter()
            .flat_map(|m| m.tool_calls.iter().map(|c| c.name.clone()))
            .collect();
        let notices: Vec<String> = messages
            .iter()
            .filter(|m| m.content.starts_with("Runtime budget notice"))
            .map(|m| m.content.clone())
            .collect();
        if notices.len() > seen.notices.len() {
            seen.notices = notices.clone();
        }
        // A notice can follow a result, so results are found by call id.
        let asked: Vec<&str> = messages
            .iter()
            .flat_map(|m| m.tool_calls.iter())
            .filter(|c| c.name == "budget_status")
            .map(|c| c.id.as_str())
            .collect();
        let statuses: Vec<Value> = messages
            .iter()
            .filter(|m| {
                m.role == "tool"
                    && m.tool_call_id
                        .as_deref()
                        .is_some_and(|id| asked.contains(&id))
            })
            .filter_map(|m| serde_json::from_str(&m.content).ok())
            .collect();
        if statuses.len() > seen.status.len() {
            seen.status = statuses;
        }
        Ok(match self.script {
            Script::Budget => match tools.len() {
                // 0.21 per round at the premium prices.
                0 => call("budget_status", json!({}), 20_000, 1_000),
                1 => call(
                    "spawn_agent",
                    json!({"task":"summarize the notes in NOTES.md","model":"cheap"}),
                    20_000,
                    1_000,
                ),
                2 => call("wait_agent", json!({}), 20_000, 1_000),
                3 => call("budget_status", json!({}), 20_000, 1_000),
                _ if notices.iter().any(|n| n.contains("used 80%")) => {
                    text("stopping within budget", 1_000, 10)
                }
                _ => call("budget_status", json!({}), 1_000, 10),
            },
            Script::Rules => {
                if last.role == "tool" {
                    seen.refusals.push(last.content.clone());
                }
                match tools.len() {
                    0 => call(
                        "spawn_agent",
                        json!({"task":"summarize the notes","model":"outside"}),
                        100,
                        10,
                    ),
                    1 => call(
                        "spawn_agent",
                        json!({"task":"summarize the notes","model":"cheap","writable":true,"isolation":"worktree"}),
                        100,
                        10,
                    ),
                    _ => text("done", 100, 10),
                }
            }
        })
    }
}

async fn run(script: Script, budget: Option<f64>) -> Result<(Arc<Mutex<Seen>>, Value, RunResult)> {
    let home = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    std::fs::write(workspace.path().join("NOTES.md"), "ship on friday\n")?;
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mut cfg = config();
    if script == Script::Rules {
        cfg.policy.write = true;
    }
    let engine = Engine::with_components(
        home.path().into(),
        "default".into(),
        cfg,
        Arc::new(Scripted {
            script,
            seen: seen.clone(),
        }),
        Arc::new(NoSandbox),
    )?;
    let scope = Scope::local();
    let store = engine.store(&scope).await?;
    let project = workspace
        .path()
        .canonicalize()?
        .to_string_lossy()
        .into_owned();
    let task = match store
        .create_task_with_budget(
            &scope,
            &project,
            None,
            "Plan the release",
            "local",
            &new_id(),
            &"h".repeat(64),
            EffortLevel::Auto,
            RunMode::Normal,
            false,
            ExecutionMode::Delegated,
            None,
            budget,
        )
        .await?
    {
        TaskCreate::Created(task) | TaskCreate::Replayed(task) => task,
    };
    assert_eq!(task.budget_usd, budget);
    let result = engine
        .run_bound(
            scope.clone(),
            RunRequest {
                session_id: None,
                workspace: Some(project),
                prompt: "Plan the release".into(),
                model: None,
                effort: Default::default(),
                bridge: false,
                mode: Default::default(),
                execution: ExecutionMode::Delegated,
                cooperative: None,
                budget_usd: budget,
            },
            CancellationToken::new(),
            task.id.clone(),
        )
        .await?;
    let report = store.usage_report(&scope, &task.id).await?;
    std::mem::forget(home);
    std::mem::forget(workspace);
    Ok((seen, report, result))
}

#[tokio::test]
async fn a_task_sees_its_spend_is_warned_as_it_nears_its_budget_and_reports_it() -> Result<()> {
    let (seen, report, result) = run(Script::Budget, Some(1.0)).await?;
    assert_eq!(result.text, "stopping within budget");
    let seen = seen.lock().unwrap();

    // The first status: one round spent, the budget and the models, cheapest first.
    let first = &seen.status[0];
    assert!(
        (first["task"]["spent_usd"].as_f64().unwrap() - 0.21).abs() < 1e-6,
        "{first}"
    );
    assert_eq!(first["task"]["budget_usd"], 1.0);
    assert_eq!(
        first["turn"]["rounds_completed"], 0,
        "asked during the first round"
    );
    let models: Vec<&str> = first["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["alias"].as_str().unwrap())
        .collect();
    assert_eq!(
        models,
        ["cheap", "premium"],
        "excluded models are not offered"
    );
    assert_eq!(first["models"][0]["notes"], "fine for bulk reading");
    assert_eq!(first["models"][0]["may_write"], false);
    assert_eq!(first["models"][1]["current"], true);

    // The child ran on the cheap model and spends from the same budget.
    assert_eq!(seen.child_models, ["cheap"]);
    let later = seen.status.last().unwrap();
    let child = (2_000.0 * 0.5 + 100.0 * 1.0) / 1e6;
    assert!(
        (later["task"]["spent_usd"].as_f64().unwrap() - (0.84 + child)).abs() < 1e-6,
        "{later}"
    );

    // Told once at 50% and once at 80%, never twice for the same share.
    assert_eq!(seen.notices.len(), 2, "{:?}", seen.notices);
    assert!(seen.notices[0].contains("used 50%"), "{}", seen.notices[0]);
    assert!(seen.notices[1].contains("used 80%"), "{}", seen.notices[1]);
    assert!(seen.notices[1].contains("of $1.00"));

    // The report: per model, per depth, and the total.
    assert_eq!(report["budget_usd"], 1.0);
    let by_model = report["by_model"].as_array().unwrap();
    assert_eq!(by_model[0]["model"], "premium");
    assert_eq!(by_model[0]["calls"], 5);
    assert_eq!(by_model[1]["model"], "cheap");
    assert_eq!(by_model[1]["calls"], 1);
    let depths: Vec<i64> = report["by_depth"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["depth"].as_i64().unwrap())
        .collect();
    assert_eq!(depths, [0, 1]);
    let total = 0.84 + (1_000.0 * 10.0 + 10.0 * 10.0) / 1e6 + child;
    assert!(
        (report["total"]["cost_usd"].as_f64().unwrap() - total).abs() < 1e-6,
        "{report}"
    );
    assert!(
        (result.cost_usd - (total - child)).abs() < 1e-6,
        "the turn's own cost"
    );
    Ok(())
}

#[tokio::test]
async fn delegated_work_follows_the_operators_rules_for_each_model() -> Result<()> {
    let (seen, report, result) = run(Script::Rules, None).await?;
    assert_eq!(result.text, "done");
    let seen = seen.lock().unwrap();
    assert!(
        seen.child_models.is_empty(),
        "no child ran: {:?}",
        seen.child_models
    );
    assert!(
        seen.refusals[0].contains("may not run team members"),
        "{:?}",
        seen.refusals
    );
    assert!(
        seen.refusals[1].contains("may only read"),
        "{:?}",
        seen.refusals
    );
    assert_eq!(report["by_depth"].as_array().unwrap().len(), 1);
    Ok(())
}

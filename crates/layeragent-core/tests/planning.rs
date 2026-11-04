//! Planning first, with a scripted model: a planning turn reads, writes its
//! plan under docs/plan/ and cannot run commands or write anywhere else;
//! the build turn that follows gets the full tool set back. The operator
//! profile reaches the instructions. The sandbox runs the real file script
//! directly; it is a test double and does not certify confinement.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use anyhow::Result;
use async_trait::async_trait;
use layeragent_config::{Config, Model, Provider, TechnicalLevel, Wire};
use layeragent_core::Engine;
use layeragent_protocol::*;
use layeragent_providers::{Delta, LanguageModel};
use layeragent_sandbox::{ExecOutput, ExecRequest, Sandbox};
use layeragent_storage::TaskCreate;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Write,
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

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
            context_window: 65536,
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
    c.policy.write = true;
    c.policy.shell = true;
    c.auto_skills = false;
    c.operator.technical = Some(TechnicalLevel::None);
    c.operator.language = Some("es".into());
    c
}

/// Runs the file tools' own script in the workspace; refuses anything else.
struct ScriptSandbox;
#[async_trait]
impl Sandbox for ScriptSandbox {
    async fn execute(
        &self,
        request: ExecRequest,
        _: CancellationToken,
        _: Option<mpsc::Sender<String>>,
    ) -> Result<ExecOutput> {
        anyhow::ensure!(
            request.argv.first().map(String::as_str) == Some("python3"),
            "only the file script is expected, not {:?}",
            request.argv.first()
        );
        let mut child = std::process::Command::new("python3")
            .args(&request.argv[1..])
            .current_dir(&request.workspace)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(request.stdin.unwrap_or_default().as_bytes())?;
        let output = child.wait_with_output()?;
        Ok(ExecOutput {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            interrupted: false,
            truncated: false,
        })
    }
}

#[derive(Default)]
struct Seen {
    systems: Vec<String>,
    tools: Vec<Vec<String>>,
    results: Vec<String>,
}

struct Planner {
    seen: Arc<Mutex<Seen>>,
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

#[async_trait]
impl LanguageModel for Planner {
    async fn infer(
        &self,
        _: &Config,
        _: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        _: CancellationToken,
        _: mpsc::Sender<Delta>,
    ) -> Result<Completion> {
        let mut seen = self.seen.lock().unwrap();
        let system = messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.clone())
            .unwrap_or_default();
        seen.systems.push(system.clone());
        seen.tools
            .push(tools.iter().map(|tool| tool.name.clone()).collect());
        if let Some(last) = messages.last().filter(|m| m.role == "tool") {
            seen.results.push(last.content.clone());
        }
        let planning = system.contains("Planning mode (set by the operator)");
        // Calls made in this turn, after its own prompt.
        let start = messages
            .iter()
            .rposition(|m| m.role == "user" && !m.content.starts_with("Retrieved"))
            .unwrap_or(0);
        let made = messages[start..]
            .iter()
            .filter(|m| !m.tool_calls.is_empty())
            .count();
        Ok(if planning {
            match made {
                0 => call(
                    "write_file",
                    json!({"path":"src/index.html","content":"<h1>too early</h1>"}),
                ),
                1 => call(
                    "write_file",
                    json!({"path":"docs/plan/brief.md","content":"# Tienda de bicicletas\n\nUna página para vender bicicletas.\n"}),
                ),
                _ => Completion {
                    text: "Plan listo en docs/plan/. ¿Lo aprobás?".into(),
                    ..Default::default()
                },
            }
        } else if made == 0 {
            call(
                "write_file",
                json!({"path":"index.html","content":"<h1>Bicicletas</h1>"}),
            )
        } else {
            Completion {
                text: "Construido.".into(),
                ..Default::default()
            }
        })
    }
}

#[tokio::test]
async fn a_planning_turn_writes_only_its_plan_and_the_build_turn_gets_its_tools_back() -> Result<()>
{
    let home = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let project = workspace
        .path()
        .canonicalize()?
        .to_string_lossy()
        .into_owned();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let engine = Engine::with_components(
        home.path().into(),
        "default".into(),
        config(),
        Arc::new(Planner { seen: seen.clone() }),
        Arc::new(ScriptSandbox),
    )?;
    let scope = Scope::local();
    let store = engine.store(&scope).await?;
    let task = match store
        .create_task_with_execution(
            &scope,
            &project,
            None,
            "Una tienda de bicicletas",
            "local",
            &new_id(),
            &"h".repeat(64),
            EffortLevel::Auto,
            RunMode::Plan,
            false,
            ExecutionMode::Delegated,
            None,
        )
        .await?
    {
        TaskCreate::Created(task) | TaskCreate::Replayed(task) => task,
    };
    assert_eq!(
        store.task(&scope, &task.id).await?.mode,
        RunMode::Plan,
        "stored and read back"
    );
    let planned = engine
        .run_bound(
            scope.clone(),
            RunRequest {
                session_id: None,
                workspace: Some(project.clone()),
                prompt: "Una tienda de bicicletas".into(),
                model: None,
                effort: Default::default(),
                bridge: false,
                mode: RunMode::Plan,
                execution: ExecutionMode::Delegated,
                cooperative: None,
                budget_usd: None,
            },
            CancellationToken::new(),
            task.id.clone(),
        )
        .await?;
    assert_eq!(planned.text, "Plan listo en docs/plan/. ¿Lo aprobás?");
    assert!(workspace.path().join("docs/plan/brief.md").is_file());
    assert!(
        !workspace.path().join("src/index.html").exists(),
        "nothing built while planning"
    );
    {
        let seen = seen.lock().unwrap();
        assert!(
            seen.results[0].contains("while planning, files can only be written under docs/plan/"),
            "{:?}",
            seen.results
        );
        let tools = &seen.tools[0];
        assert!(
            tools.contains(&"write_file".to_string())
                && tools.contains(&"browser_open".to_string())
        );
        assert!(!tools.contains(&"shell".to_string()), "{tools:?}");
        assert!(!tools.contains(&"pty_start".to_string()));
        let system = &seen.systems[0];
        assert!(
            system.contains("docs/plan/: brief.md"),
            "the plan's files are named"
        );
        assert!(
            system.contains("Operator profile: not technical"),
            "{system}"
        );
        assert!(system.contains("Answer and write documents in es."));
    }

    // The operator approves; the next turn builds with every tool.
    let built = engine
        .run(
            scope.clone(),
            RunRequest {
                session_id: Some(planned.session_id.clone()),
                workspace: None,
                prompt: "The operator approved the plan in docs/plan/. Build it as planned.".into(),
                model: None,
                effort: Default::default(),
                bridge: false,
                mode: RunMode::Normal,
                execution: ExecutionMode::Delegated,
                cooperative: None,
                budget_usd: None,
            },
            CancellationToken::new(),
        )
        .await?;
    assert_eq!(built.text, "Construido.");
    assert!(workspace.path().join("index.html").is_file());
    let seen = seen.lock().unwrap();
    let build_tools = seen.tools.last().unwrap();
    assert!(
        build_tools.contains(&"shell".to_string()),
        "{build_tools:?}"
    );
    assert!(!seen.systems.last().unwrap().contains("Planning mode"));
    std::mem::forget(home);
    std::mem::forget(workspace);
    Ok(())
}

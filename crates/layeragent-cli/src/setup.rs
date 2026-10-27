//! Resumable first-run setup.
//!
//! Every step records its outcome atomically, so an interrupted setup
//! continues where it stopped. Credentials never enter the saved state: a key
//! typed into the terminal is stored in the operating system credential store
//! and only a reference to an environment variable is written to disk when no
//! store is available.
use anyhow::{Context, Result};
use layeragent_config::{Config, Model, Provider, Wire, private_dir};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

/// Steps of the guided setup, in order.
pub const STEPS: [&str; 6] = [
    "host",
    "provider",
    "credential",
    "connection",
    "isolation",
    "browser",
];

/// Outcome of one step.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StepState {
    /// `pending`, `done` or `blocked`.
    pub status: String,
    /// The capability this step controls.
    pub capability: String,
    /// Human readable outcome. Never contains a secret.
    pub detail: String,
    /// The exact command that repeats this step when it is blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    pub at: i64,
}

impl StepState {
    fn pending(capability: &str) -> Self {
        Self {
            status: "pending".into(),
            capability: capability.into(),
            detail: "not started".into(),
            action: None,
            at: layeragent_protocol::now_ms(),
        }
    }
    fn done(capability: &str, detail: impl Into<String>) -> Self {
        Self {
            status: "done".into(),
            capability: capability.into(),
            detail: detail.into(),
            action: None,
            at: layeragent_protocol::now_ms(),
        }
    }
    fn blocked(capability: &str, detail: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            status: "blocked".into(),
            capability: capability.into(),
            detail: detail.into(),
            action: Some(action.into()),
            at: layeragent_protocol::now_ms(),
        }
    }
}

/// Saved setup progress.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetupState {
    pub version: u32,
    pub updated_at: i64,
    pub steps: BTreeMap<String, StepState>,
}

impl SetupState {
    pub fn load(home: &Path) -> Result<Self> {
        let path = state_path(home);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or_else(|_| SetupState::new())),
            Err(_) => Ok(SetupState::new()),
        }
    }
    pub fn new() -> Self {
        let mut steps = BTreeMap::new();
        for step in STEPS {
            let capability = match step {
                "host" => "host",
                "provider" => "model",
                "credential" => "model",
                "connection" => "model",
                "isolation" => "execution",
                _ => "browser",
            };
            steps.insert(step.to_string(), StepState::pending(capability));
        }
        Self {
            version: 1,
            updated_at: layeragent_protocol::now_ms(),
            steps,
        }
    }
    /// Writes the state atomically: a partial write can never be observed.
    pub fn save(&self, home: &Path) -> Result<()> {
        let path = state_path(home);
        let directory = path.parent().context("setup state has no directory")?;
        private_dir(directory)?;
        let temporary = directory.join(format!(".setup-{}.tmp", layeragent_protocol::new_id()));
        let body = serde_json::to_vec_pretty(self)?;
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&temporary)?;
            file.write_all(&body)?;
            file.sync_all()?;
        }
        std::fs::rename(&temporary, &path)?;
        if let Ok(handle) = std::fs::File::open(directory) {
            let _ = handle.sync_all();
        }
        Ok(())
    }
    pub fn blocked_steps(&self) -> Vec<&StepState> {
        self.steps
            .values()
            .filter(|step| step.status == "blocked")
            .collect()
    }
    /// Capabilities that a blocked step keeps unavailable.
    pub fn blocked_capabilities(&self) -> Vec<String> {
        self.blocked_steps()
            .into_iter()
            .map(|step| step.capability.clone())
            .collect()
    }
}

fn state_path(home: &Path) -> PathBuf {
    home.join("setup.json")
}

/// Options accepted by the setup command.
#[derive(Clone, Debug, Default)]
pub struct SetupOptions {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub protocol: Option<String>,
    pub base_url: Option<String>,
    /// Environment variable that already holds the credential.
    pub key_env: Option<String>,
    /// Read the credential from standard input and store it in the system store.
    pub key_stdin: bool,
    /// Workspace used by the isolation test.
    pub workspace: Option<PathBuf>,
}

/// Runs the wizard until it needs the operator.
///
/// Every step is independent: a blocked capability does not prevent the
/// others from being verified. Steps that already succeeded are skipped, so
/// the wizard resumes where it stopped.
pub async fn run(home: &Path, profile: &str, options: &SetupOptions) -> Result<SetupState> {
    let mut state = SetupState::load(home)?;
    for step in STEPS {
        if state
            .steps
            .get(step)
            .map(|value| value.status == "done")
            .unwrap_or(false)
        {
            continue;
        }
        println!("Setup step: {step}");
        match step {
            "host" => step_host(home, profile, &mut state, options).await?,
            "provider" => step_provider(home, profile, &mut state, options)?,
            "credential" => step_credential(home, profile, &mut state, options)?,
            "connection" => step_connection(home, profile, &mut state, options).await?,
            "isolation" => step_isolation(home, profile, &mut state, options).await?,
            "browser" => step_browser(home, profile, &mut state, options).await?,
            other => anyhow::bail!("unknown setup step {other}"),
        }
        state.updated_at = layeragent_protocol::now_ms();
        state.save(home)?;
    }
    print_state(&state);
    Ok(state)
}

/// The first step that is not finished.
pub fn next_step(state: &SetupState) -> Option<String> {
    STEPS
        .iter()
        .find(|step| {
            state
                .steps
                .get(**step)
                .map(|value| value.status != "done")
                .unwrap_or(true)
        })
        .map(|step| step.to_string())
}

pub fn print_state(state: &SetupState) {
    for step in STEPS {
        let Some(value) = state.steps.get(step) else {
            continue;
        };
        println!(
            "{:<10} {:<8} {:<12} {}",
            step, value.status, value.capability, value.detail
        );
        if value.status == "blocked"
            && let Some(action) = &value.action
        {
            println!("           retry with: {action}");
        }
    }
}

/// Detects the host and its dependencies.
async fn step_host(
    home: &Path,
    profile: &str,
    state: &mut SetupState,
    _options: &SetupOptions,
) -> Result<()> {
    let config = match Config::load(home, profile) {
        Ok(config) => config,
        Err(error) => {
            let detail = format!("configuration is not usable yet: {error}");
            state.steps.insert(
                "host".into(),
                StepState::blocked("host", detail, "layeragent setup"),
            );
            return Ok(());
        }
    };
    let diagnostics = layeragent_sandbox::doctor(&config.policy.limits).await;
    let missing = diagnostics
        .iter()
        .filter(|item| !item.available)
        .map(|item| item.name.clone())
        .collect::<Vec<_>>();
    let detail = if missing.is_empty() {
        format!("{} host capabilities available", diagnostics.len())
    } else {
        format!("unavailable host capabilities: {}", missing.join(", "))
    };
    if missing.is_empty() {
        state
            .steps
            .insert("host".into(), StepState::done("host", detail));
    } else {
        state.steps.insert(
            "host".into(),
            StepState::blocked("host", detail, "layeragent setup"),
        );
    }
    Ok(())
}

/// Records the provider and model selection.
fn step_provider(
    home: &Path,
    profile: &str,
    state: &mut SetupState,
    options: &SetupOptions,
) -> Result<()> {
    let Some(provider_name) = options.provider.clone() else {
        state.steps.insert(
            "provider".into(),
            StepState::blocked(
                "model",
                "no provider selected",
                "layeragent setup --provider <name> --model <id>",
            ),
        );
        return Ok(());
    };
    let Some(model_id) = options.model.clone() else {
        state.steps.insert(
            "provider".into(),
            StepState::blocked(
                "model",
                "no model selected",
                "layeragent setup --provider <name> --model <id>",
            ),
        );
        return Ok(());
    };
    let wire: Wire = match options.protocol.as_deref() {
        Some(protocol) => {
            serde_json::from_value(json!(protocol)).context("unknown provider protocol")?
        }
        None => Wire::OpenaiChat,
    };
    let base_url = options
        .base_url
        .clone()
        .unwrap_or_else(|| default_base_url(&wire).to_string());
    let mut config = Config::load(home, profile).unwrap_or_default();
    config.providers.insert(
        provider_name.clone(),
        Provider {
            wire,
            base_url: base_url.clone(),
            key_env: options.key_env.clone(),
            oauth: None,
            headers: BTreeMap::new(),
        },
    );
    config.models.insert(
        provider_name.clone(),
        Model {
            provider: provider_name.clone(),
            id: model_id.clone(),
            context_window: 128_000,
            tools: true,
            emulate_tools: false,
            input_per_million: None,
            output_per_million: None,
            parameters: Default::default(),
        },
    );
    config.primary = provider_name.clone();
    config
        .validate()
        .context("the selected provider is not usable")?;
    write_config(home, &config)?;
    state.steps.insert(
        "provider".into(),
        StepState::done(
            "model",
            format!("provider {provider_name} with model {model_id}"),
        ),
    );
    Ok(())
}

/// Binds a credential to the selected provider.
fn step_credential(
    home: &Path,
    profile: &str,
    state: &mut SetupState,
    options: &SetupOptions,
) -> Result<()> {
    let config = Config::load(home, profile)?;
    let provider_name = config.primary.clone();
    let Some(provider) = config.providers.get(&provider_name).cloned() else {
        state.steps.insert(
            "credential".into(),
            StepState::blocked(
                "model",
                "no provider is configured",
                "layeragent setup --provider <name> --model <id>",
            ),
        );
        return Ok(());
    };
    // An environment variable is a reference, not a secret.
    if let Some(key_env) = options.key_env.clone().or_else(|| provider.key_env.clone()) {
        let present = std::env::var(&key_env).is_ok();
        if present {
            state.steps.insert(
                "credential".into(),
                StepState::done(
                    "model",
                    format!("credential read from the environment variable {key_env}"),
                ),
            );
            return Ok(());
        }
        state.steps.insert(
            "credential".into(),
            StepState::blocked(
                "model",
                format!("the environment variable {key_env} is not set"),
                format!("layeragent setup --key-env {key_env}"),
            ),
        );
        return Ok(());
    }
    if options.key_stdin {
        let mut key = String::new();
        std::io::stdin().read_to_string(&mut key)?;
        let key = key.trim().to_string();
        if key.is_empty() {
            state.steps.insert(
                "credential".into(),
                StepState::blocked(
                    "model",
                    "the credential was empty",
                    "layeragent setup --key-stdin",
                ),
            );
            return Ok(());
        }
        match layeragent_providers::store_api_key(&provider_name, &provider, &key) {
            Ok(()) => {
                state.steps.insert(
                    "credential".into(),
                    StepState::done(
                        "model",
                        "credential stored in the operating system credential store",
                    ),
                );
                return Ok(());
            }
            Err(error) => {
                state.steps.insert(
                    "credential".into(),
                    StepState::blocked(
                        "model",
                        format!(
                            "the system credential store is unavailable ({error}); use an environment variable instead"
                        ),
                        "layeragent setup --key-env <NAME>",
                    ),
                );
                return Ok(());
            }
        }
    }
    state.steps.insert(
        "credential".into(),
        StepState::blocked(
            "model",
            "no credential is bound",
            "layeragent setup --key-env <NAME> (or --key-stdin)",
        ),
    );
    Ok(())
}

/// Sends one minimal authorized request to the provider.
async fn step_connection(
    home: &Path,
    profile: &str,
    state: &mut SetupState,
    options: &SetupOptions,
) -> Result<()> {
    let config = Config::load(home, profile)?;
    let router = layeragent_providers::Router::new()?;
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let selected = config.primary.clone();
    let messages = vec![layeragent_protocol::Message::text(
        "user",
        "Reply with the single word: ready",
    )];
    let cancel = CancellationToken::new();
    let request = config.clone();
    let call = tokio::spawn(async move {
        use layeragent_providers::LanguageModel;
        router
            .infer(&request, &selected, &messages, &[], cancel, tx)
            .await
    });
    let mut text = String::new();
    while let Some(delta) = rx.recv().await {
        if delta.kind == "text_delta" {
            text.push_str(&delta.text);
        }
    }
    let outcome = match call.await {
        Ok(Ok(completion)) => {
            let answer = if text.is_empty() {
                completion.text
            } else {
                text
            };
            if answer.trim().is_empty() {
                Err(anyhow::anyhow!(
                    "the provider answered with an empty message"
                ))
            } else {
                Ok(answer.trim().chars().take(40).collect::<String>())
            }
        }
        Ok(Err(error)) => Err(error),
        Err(_) => Err(anyhow::anyhow!("the provider request did not finish")),
    };
    match outcome {
        Ok(answer) => {
            state.steps.insert(
                "connection".into(),
                StepState::done("model", format!("provider answered: {answer}")),
            );
        }
        Err(error) => {
            state.steps.insert(
                "connection".into(),
                StepState::blocked(
                    "model",
                    format!("the provider request failed: {error}"),
                    "layeragent setup --key-env <NAME>",
                ),
            );
        }
    }
    let _ = options;
    Ok(())
}

/// Exercises the confinement with a real command.
async fn step_isolation(
    home: &Path,
    profile: &str,
    state: &mut SetupState,
    options: &SetupOptions,
) -> Result<()> {
    let config = Config::load(home, profile)?;
    let workspace = match options.workspace.clone() {
        Some(path) => path,
        None => home.join("data").join(profile),
    };
    private_dir(&workspace)?;
    let sandbox = layeragent_sandbox::NativeSandbox::new(std::env::current_exe()?);
    use layeragent_sandbox::Sandbox;
    let cancel = CancellationToken::new();
    let allowed = sandbox
        .execute(
            layeragent_sandbox::ExecRequest {
                argv: vec!["/bin/sh".into(), "-c".into(), "printf isolation-ok".into()],
                workspace: workspace.clone(),
                policy: config.policy.clone(),
                stdin: None,
            },
            cancel.clone(),
            None,
        )
        .await;
    let escape = sandbox
        .execute(
            layeragent_sandbox::ExecRequest {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf escaped > /tmp/layeragent-setup-escape".into(),
                ],
                workspace: workspace.clone(),
                policy: config.policy.clone(),
                stdin: None,
            },
            cancel.clone(),
            None,
        )
        .await;
    let allowed_ok = allowed
        .as_ref()
        .is_ok_and(|output| output.code == Some(0) && output.stdout.contains("isolation-ok"));
    let escaped = std::path::Path::new("/tmp/layeragent-setup-escape").exists();
    let escape_ok = escape.is_ok() && !escaped;
    let _ = std::fs::remove_file("/tmp/layeragent-setup-escape");
    if allowed_ok && escape_ok {
        state.steps.insert(
            "isolation".into(),
            StepState::done(
                "execution",
                "a confined command ran and an outside write was refused",
            ),
        );
    } else {
        state.steps.insert(
            "isolation".into(),
            StepState::blocked(
                "execution",
                if !allowed_ok {
                    "the confined command did not run".to_string()
                } else {
                    "a write outside the workspace was not refused".to_string()
                },
                "layeragent setup --workspace <PATH>",
            ),
        );
    }
    Ok(())
}

/// Configures the browser when the profile enables it.
async fn step_browser(
    home: &Path,
    profile: &str,
    state: &mut SetupState,
    options: &SetupOptions,
) -> Result<()> {
    let config = Config::load(home, profile)?;
    if !config.browser.enabled {
        state.steps.insert(
            "browser".into(),
            StepState::done("browser", "the browser is disabled in this profile"),
        );
        return Ok(());
    }
    let executable = layeragent_browser::install::ensure_browser(
        home,
        config.browser.executable.as_deref(),
        false,
    )
    .await;
    match executable {
        Ok(path) => {
            state.steps.insert(
                "browser".into(),
                StepState::done("browser", format!("browser ready at {}", path.display())),
            );
        }
        Err(error) => {
            state.steps.insert(
                "browser".into(),
                StepState::blocked(
                    "browser",
                    format!("the browser is not available: {error}"),
                    "layeragent setup",
                ),
            );
        }
    }
    let _ = options;
    Ok(())
}

fn default_base_url(wire: &Wire) -> &'static str {
    match wire {
        Wire::OpenaiChat | Wire::OpenaiResponses => "https://api.openai.com/v1",
        Wire::Anthropic => "https://api.anthropic.com/v1",
        Wire::Gemini => "https://generativelanguage.googleapis.com/v1beta",
        Wire::Ollama => "http://127.0.0.1:11434",
    }
}

/// Writes the effective configuration atomically.
fn write_config(home: &Path, config: &Config) -> Result<()> {
    private_dir(home)?;
    let path = home.join("config.toml");
    let temporary = home.join(".config.toml.tmp");
    // Serialize the whole document: a partial writer would silently drop
    // fallbacks, prices, headers or MCP/channel definitions from an existing
    // installation.
    let body = toml::to_string_pretty(config).context("the configuration is not serializable")?;
    std::fs::write(&temporary, body)?;
    std::fs::rename(&temporary, &path)?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod setup_tests {
    use super::*;

    fn selected(provider: &str, model: &str, key_env: Option<&str>) -> SetupOptions {
        SetupOptions {
            provider: Some(provider.into()),
            model: Some(model.into()),
            key_env: key_env.map(str::to_string),
            ..SetupOptions::default()
        }
    }

    #[test]
    fn progress_is_written_atomically_and_resumes_at_the_blocked_step() {
        let home = tempfile::tempdir().unwrap();
        let mut state = SetupState::new();
        state
            .steps
            .insert("host".into(), StepState::done("host", "host ready"));
        state.steps.insert(
            "provider".into(),
            StepState::done("model", "provider selected"),
        );
        state.steps.insert(
            "credential".into(),
            StepState::blocked(
                "model",
                "the environment variable MISSING is not set",
                "layeragent setup --key-env MISSING",
            ),
        );
        state.save(home.path()).unwrap();

        let loaded = SetupState::load(home.path()).unwrap();
        assert_eq!(next_step(&loaded).as_deref(), Some("credential"));
        assert_eq!(loaded.blocked_capabilities(), vec!["model"]);
        assert!(
            loaded.steps["credential"]
                .action
                .as_deref()
                .unwrap()
                .contains("--key-env MISSING")
        );
        // Only the finished file remains; no temporary write is observable.
        let names = std::fs::read_dir(home.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["setup.json"]);
    }

    #[test]
    fn an_unset_key_reference_blocks_only_the_model_capability() {
        let home = tempfile::tempdir().unwrap();
        let mut state = SetupState::new();
        let options = selected(
            "fixture",
            "fixture-model",
            Some("LAYERAGENT_SETUP_TEST_UNSET"),
        );
        step_provider(home.path(), "default", &mut state, &options).unwrap();
        step_credential(home.path(), "default", &mut state, &options).unwrap();

        let credential = &state.steps["credential"];
        assert_eq!(credential.status, "blocked");
        assert_eq!(credential.capability, "model");
        assert!(credential.detail.contains("LAYERAGENT_SETUP_TEST_UNSET"));
        assert!(
            credential
                .action
                .as_deref()
                .unwrap()
                .contains("--key-env LAYERAGENT_SETUP_TEST_UNSET")
        );
        // The other capabilities are untouched and can still be verified.
        assert_ne!(state.steps["isolation"].status, "blocked");
        assert_ne!(state.steps["browser"].status, "blocked");
        // The saved configuration holds the reference, never a key.
        let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
        assert!(raw.contains("LAYERAGENT_SETUP_TEST_UNSET"));
        assert!(!raw.to_lowercase().contains("api_key"));
        assert!(!raw.contains("sk-"));
    }

    #[test]
    fn selecting_a_model_keeps_unrelated_configuration() {
        let home = tempfile::tempdir().unwrap();
        let mut state = SetupState::new();
        step_provider(
            home.path(),
            "default",
            &mut state,
            &selected("fixture", "first", None),
        )
        .unwrap();
        let mut config = Config::load(home.path(), "default").unwrap();
        config.max_cost_usd = Some(1.25);
        config.privacy.local_models_only = true;
        let mut other = config.models["fixture"].clone();
        other.id = "retained".into();
        other.input_per_million = Some(0.75);
        other.output_per_million = Some(2.5);
        config.models.insert("retained".into(), other);
        write_config(home.path(), &config).unwrap();

        step_provider(
            home.path(),
            "default",
            &mut state,
            &selected("fixture", "second", None),
        )
        .unwrap();
        let reloaded = Config::load(home.path(), "default").unwrap();
        assert_eq!(reloaded.models["fixture"].id, "second");
        assert_eq!(reloaded.models["retained"].input_per_million, Some(0.75));
        assert_eq!(reloaded.max_cost_usd, Some(1.25));
        assert!(reloaded.privacy.local_models_only);
    }
}

mod backup;
mod client;
mod remote;
mod setup;
mod team;
mod tui;
use anyhow::{Context, Result, ensure};
use clap::{CommandFactory, Parser, Subcommand};
use layeragent_config::{Config, Model, Provider, Wire, default_home, private_dir};
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Parser)]
#[command(
    name = "layeragent",
    version,
    about = "Personalizable, model-independent engineering runtime"
)]
struct Args {
    #[arg(long, global = true, env = "LAYERAGENT_HOME")]
    home: Option<PathBuf>,
    #[arg(long, global = true, default_value = "default")]
    profile: String,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    /// Manage project permissions and the encrypted self-hosted remote bridge.
    Remote {
        #[command(subcommand)]
        command: remote::RemoteCommand,
    },
    /// Create, verify or restore an encrypted private backup.
    Backup {
        #[command(subcommand)]
        command: backup::BackupCommand,
    },
    /// Initialize private configuration; model IDs are chosen by the operator.
    Init {
        #[arg(long)]
        model: Option<String>,
        #[arg(long, default_value = "ollama")]
        protocol: String,
        #[arg(long, default_value = "http://127.0.0.1:11434")]
        base_url: String,
        #[arg(long)]
        key_env: Option<String>,
        #[arg(long, default_value_t = 32768)]
        context_window: u64,
    },
    /// Execute a prompt through the shared local runtime.
    Run {
        prompt: String,
        #[arg(short, long, default_value = ".")]
        workspace: PathBuf,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        execution: team::ExecutionOptions,
    },
    /// Inspect and direct a durable cooperative team.
    Team {
        #[command(subcommand)]
        command: team::TeamCommand,
    },
    /// Resume a stored session.
    Resume {
        session: String,
        prompt: String,
        #[arg(long)]
        json: bool,
    },
    /// Interactive terminal client.
    Tui {
        #[arg(short, long, default_value = ".")]
        workspace: PathBuf,
        #[arg(long)]
        session: Option<String>,
    },
    /// Run the daemon and authenticated HTTP API.
    Serve {
        #[arg(long)]
        ipc_only: bool,
    },
    /// Serve the shared web interface on the loopback interface.
    Web {
        /// Loopback address to bind. Non-loopback addresses are refused.
        #[arg(long, default_value = "127.0.0.1:9474")]
        bind: String,
        /// Directory with the built frontend. Defaults to the local build.
        #[arg(long)]
        assets: Option<PathBuf>,
    },
    /// Guided, resumable first-run setup.
    Setup {
        /// `run` continues the wizard, `status` prints the saved progress.
        #[arg(default_value = "run")]
        action: String,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        model: Option<String>,
        /// Provider protocol, for example `openai_chat` or `anthropic`.
        #[arg(long)]
        protocol: Option<String>,
        #[arg(long)]
        base_url: Option<String>,
        /// Environment variable that already holds the credential.
        #[arg(long)]
        key_env: Option<String>,
        /// Read the credential from standard input into the system credential store.
        #[arg(long)]
        key_stdin: bool,
        /// Workspace used by the isolation test.
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
    /// Stop the local runtime and cancel its active work.
    Stop,
    /// Verify host confinement capabilities without changing host security settings.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// The agent's own browser: show what will be used, install it, or try it on a URL.
    Browser {
        #[arg(value_parser=["status","install","open"])]
        #[arg(default_value = "status")]
        action: String,
        /// URL for `open`.
        url: Option<String>,
        /// Reserved visible-mode request; currently rejected by the hardened backend.
        #[arg(long)]
        visible: bool,
    },
    /// Inspect, validate, or reload layered configuration.
    Config {
        #[arg(default_value = "show")]
        action: String,
    },
    Models,
    /// Expose the shared runtime through MCP over stdio.
    Mcp,
    Sessions,
    Auth {
        #[arg(value_parser=["login","logout"])]
        action: String,
        provider: String,
    },
    Skills {
        #[arg(value_parser=["list","verify","versions","rollback"])]
        #[arg(default_value = "list")]
        action: String,
        name: Option<String>,
        #[arg(long)]
        hash: Option<String>,
    },
    Memory {
        query: String,
        #[arg(short, long, default_value = ".")]
        workspace: PathBuf,
    },
    Rea {
        #[arg(value_parser=["inspect","ledger","verify","seal"])]
        action: String,
        path: Option<PathBuf>,
        #[arg(long)]
        key_env: Option<String>,
    },
    Completions {
        shell: clap_complete::Shell,
    },
}
fn main() {
    if let Err(error) = dispatch() {
        eprintln!(
            "LayerAgent: {}",
            layeragent_protocol::redact::redact_text(&format!("{error:#}"))
        );
        std::process::exit(1);
    }
}
fn dispatch() -> Result<()> {
    let raw = std::env::args().collect::<Vec<_>>();
    if raw.get(1).is_some_and(|s| s == "__embed_worker") {
        return layeragent_storage::embedding_worker_main(
            raw.get(2).context("embedding model required")?,
            PathBuf::from(raw.get(3).context("embedding cache required")?),
        );
    }
    if raw.get(1).is_some_and(|s| s == "__sandbox_inner") {
        ensure!(
            raw.len() >= 5 && raw[3] == "--",
            "invalid sandbox helper invocation"
        );
        let spec: layeragent_sandbox::InnerSpec = serde_json::from_str(&raw[2])?;
        return layeragent_sandbox::inner(&spec, &raw[4..]);
    }
    if raw.get(1).is_some_and(|s| s == "__browser_inner") {
        ensure!(
            raw.len() >= 5 && raw[3] == "--",
            "invalid browser helper invocation"
        );
        let spec: layeragent_sandbox::BrowserInnerSpec = serde_json::from_str(&raw[2])?;
        return layeragent_sandbox::browser_inner(&spec, &raw[4..]);
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(entry())
}
async fn entry() -> Result<()> {
    let args = Args::parse();
    let home = args.home.unwrap_or_else(default_home);
    let profile = args.profile;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let command = args.command.unwrap_or(Command::Tui {
        workspace: PathBuf::from("."),
        session: None,
    });
    let _home_lease = if matches!(
        &command,
        Command::Init { .. } | Command::Skills { .. } | Command::Rea { .. }
    ) {
        Some(layeragent_storage::backup::runtime_lease(&home)?)
    } else {
        None
    };
    match command {
        Command::Remote { command } => remote::dispatch(command).await?,
        Command::Backup { command } => backup::dispatch(&home, command)?,
        Command::Init {
            model,
            protocol,
            base_url,
            key_env,
            context_window,
        } => {
            private_dir(&home)?;
            let path = home.join("config.toml");
            ensure!(
                !path.exists(),
                "configuration already exists: {}",
                path.display()
            );
            let mut c = Config::default();
            if let Some(id) = model {
                let wire: Wire = serde_json::from_value(json!(protocol))?;
                c.providers.insert(
                    "primary".into(),
                    Provider {
                        wire,
                        base_url,
                        key_env,
                        oauth: None,
                        headers: BTreeMap::new(),
                    },
                );
                c.models.insert(
                    "primary".into(),
                    Model {
                        provider: "primary".into(),
                        id,
                        context_window,
                        tools: true,
                        emulate_tools: false,
                        input_per_million: None,
                        output_per_million: None,
                        parameters: Default::default(),
                    },
                );
                c.primary = "primary".into();
            }
            c.validate()?;
            std::fs::write(&path, toml_string(&c)?)?;
            println!(
                "Created {}\nRun layeragent doctor before executing tools.",
                path.display()
            );
        }
        Command::Browser {
            action,
            url,
            visible,
        } => {
            let config = Config::load(&home, &profile)?;
            match action.as_str() {
                "install" => {
                    let path = layeragent_browser::install::ensure_browser(
                        &home,
                        config.browser.executable.as_deref(),
                        true,
                    )
                    .await?;
                    println!("Browser ready: {}", path.display());
                }
                "open" => {
                    let url = url.context("usage: layeragent browser open <url> [--visible]")?;
                    let manager = layeragent_browser::BrowserManager::default();
                    let session = manager
                        .open("cli", &home, &config.browser, visible.then_some(true))
                        .await?;
                    let view = session.page.navigate(&url).await?;
                    println!("{}", serde_json::to_string_pretty(&view)?);
                    if visible {
                        println!("Window is open; press Enter to close it.");
                        let _ = tokio::io::AsyncBufReadExt::read_line(
                            &mut tokio::io::BufReader::new(tokio::io::stdin()),
                            &mut String::new(),
                        )
                        .await;
                    }
                    manager.close_owner("cli");
                }
                _ => println!(
                    "{}",
                    serde_json::to_string_pretty(&layeragent_browser::status(
                        &home,
                        &config.browser
                    ))?
                ),
            }
        }
        Command::Doctor { json } => {
            // Validate the selected profile before deciding which host capabilities
            // it actually requires; invalid configuration must never be bypassed.
            let config = Config::load(&home, &profile)?;
            let checks = doctor_report(
                layeragent_sandbox::doctor(&config.policy.limits).await,
                config.policy.limits.cgroup,
            );
            let ok = doctor_passed(&checks);
            if json {
                println!("{}", serde_json::to_string_pretty(&checks)?);
            } else {
                for c in checks {
                    println!("{} {}: {}", doctor_label(&c), c.check.name, c.check.detail);
                }
            }
            ensure!(
                ok,
                "host verification failed; confined execution is unavailable"
            );
        }
        Command::Config { action } => {
            let config = Config::load(&home, &profile)?;
            match action.as_str() {
                "schema" => println!("{}", serde_json::to_string_pretty(&Config::schema())?),
                "validate" => println!("Configuration valid ({})", config.revision()),
                "reload" => {
                    let client = client::Client::connect(&home, &profile).await?;
                    println!("{}", client.post("/v1/config/reload", json!({})).await?);
                }
                "show" | "explain" => println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &json!({"precedence":["defaults",home.join("config.toml"),home.join("profiles").join(format!("{profile}.toml"))],"revision":config.revision(),"effective":config})
                    )?
                ),
                _ => anyhow::bail!("unknown config action"),
            }
        }
        Command::Stop => {
            let client = client::Client::connect(&home, &profile).await?;
            client.post("/v1/shutdown", json!({})).await?;
            println!("Runtime stopping");
        }
        Command::Serve { ipc_only } => serve(home, profile, ipc_only).await?,
        Command::Web { bind, assets } => web(home, profile, bind, assets).await?,
        Command::Setup {
            action,
            provider,
            model,
            protocol,
            base_url,
            key_env,
            key_stdin,
            workspace,
        } => {
            let options = setup::SetupOptions {
                provider,
                model,
                protocol,
                base_url,
                key_env,
                key_stdin,
                workspace,
            };
            match action.as_str() {
                "run" => {
                    let state = setup::run(&home, &profile, &options).await?;
                    let blocked = state.blocked_capabilities();
                    if !blocked.is_empty() {
                        println!("Blocked capabilities: {}", blocked.join(", "));
                    }
                }
                "status" => {
                    let state = setup::SetupState::load(&home)?;
                    setup::print_state(&state);
                    if let Some(next) = setup::next_step(&state) {
                        println!("next step: {next}");
                    }
                    let blocked = state.blocked_capabilities();
                    if !blocked.is_empty() {
                        println!("Blocked capabilities: {}", blocked.join(", "));
                    }
                }
                other => anyhow::bail!("unknown setup action {other}"),
            }
        }
        Command::Run {
            prompt,
            workspace,
            session,
            model,
            json,
            execution,
        } => {
            let options = execution.body()?;
            let client = client::Client::connect(&home, &profile).await?;
            let session = client.session(session, workspace).await?;
            client
                .print_turn_options(&session, &prompt, model, json, options)
                .await?;
        }
        Command::Team { command } => {
            let client = client::Client::connect(&home, &profile).await?;
            team::dispatch(&client, command).await?;
        }
        Command::Resume {
            session,
            prompt,
            json,
        } => {
            let client = client::Client::connect(&home, &profile).await?;
            client.print_turn(&session, &prompt, None, json).await?;
        }
        Command::Tui { workspace, session } => {
            let client = client::Client::connect(&home, &profile).await?;
            let session = client.session(session, workspace.clone()).await?;
            tui::run(client, session, workspace).await?;
        }
        Command::Mcp => {
            use rmcp::ServiceExt;
            let client = client::Client::connect(&home, &profile).await?;
            let service = layeragent_gateway::mcp::McpService {
                backend: std::sync::Arc::new(client),
            }
            .serve(rmcp::transport::stdio())
            .await?;
            service.waiting().await?;
        }
        Command::Models => {
            let client = client::Client::connect(&home, &profile).await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&client.get("/v1/models").await?)?
            );
        }
        Command::Sessions => {
            let client = client::Client::connect(&home, &profile).await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&client.get("/v1/sessions").await?)?
            );
        }
        Command::Auth { action, provider } => {
            let c = Config::load(&home, &profile)?;
            let p = c.providers.get(&provider).context("provider not found")?;
            if action == "login" {
                layeragent_providers::login(&provider, p).await?;
            } else {
                layeragent_providers::logout(&provider, p)?;
            }
        }
        Command::Memory { query, workspace } => {
            let client = client::Client::connect(&home, &profile).await?;
            println!("{}",serde_json::to_string_pretty(&client.post("/v1/memory/search",json!({"workspace":workspace.canonicalize()?.to_string_lossy(),"query":query})).await?)?);
        }
        Command::Skills { action, name, hash } => {
            let c = Config::load(&home, &profile)?;
            let state = home
                .join("data")
                .join(&profile)
                .join("tenants/default/principals/owner");
            match action.as_str() {
                "list" => println!(
                    "{}",
                    serde_json::to_string_pretty(&layeragent_skills::discover(&state, &c)?)?
                ),
                "verify" => {
                    let name = name.context("skill name required")?;
                    let sandbox = layeragent_sandbox::NativeSandbox::new(std::env::current_exe()?);
                    println!(
                        "{}",
                        layeragent_skills::verify_and_promote(
                            &state,
                            &name,
                            &sandbox,
                            tokio_util::sync::CancellationToken::new()
                        )
                        .await?
                    );
                }
                "versions" => {
                    let name = name.context("skill name required")?;
                    layeragent_protocol::validate_name(&name)?;
                    let directory = state.join("skill-versions");
                    let mut versions = Vec::new();
                    if directory.is_dir() {
                        for entry in std::fs::read_dir(directory)? {
                            let entry = entry?;
                            if entry.file_type()?.is_dir()
                                && let Some(hash) = entry
                                    .file_name()
                                    .to_string_lossy()
                                    .strip_prefix(&format!("{name}-"))
                            {
                                versions.push(hash.to_string());
                            }
                        }
                    }
                    versions.sort();
                    println!("{}", json!({"skill":name,"archived_versions":versions}));
                }
                "rollback" => {
                    layeragent_skills::rollback(
                        &state,
                        &name.context("skill name required")?,
                        &hash.context("--hash required")?,
                    )?;
                    println!("Skill version restored");
                }
                _ => anyhow::bail!("unknown skills action"),
            }
        }
        Command::Rea {
            action,
            path,
            key_env,
        } => {
            let state = home.join("data").join(&profile);
            let store = layeragent_storage::Storage::open(&state, "default")?;
            match action.as_str() {
                "inspect" => println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &layeragent_rea::inspect(&store, &path.context("artifact path required")?)
                            .await?
                    )?
                ),
                "ledger" => println!("{}", serde_json::to_string_pretty(&store.ledger().await?)?),
                "verify" => println!(
                    "Verified head: {}",
                    layeragent_storage::verify_ledger(&store.ledger().await?)?
                ),
                "seal" => {
                    let key = std::env::var(key_env.context("--key-env required")?)
                        .context("signing key environment variable missing")?;
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&layeragent_rea::seal(
                            &store.ledger().await?,
                            &key
                        )?)?
                    );
                }
                _ => unreachable!(),
            }
        }
        Command::Completions { shell } => {
            clap_complete::generate(
                shell,
                &mut Args::command(),
                "layeragent",
                &mut std::io::stdout(),
            );
        }
    }
    Ok(())
}
#[derive(serde::Serialize)]
struct DoctorCheck {
    #[serde(flatten)]
    check: layeragent_sandbox::Diagnostic,
    required: bool,
}

fn doctor_report(
    checks: Vec<layeragent_sandbox::Diagnostic>,
    require_cgroup: bool,
) -> Vec<DoctorCheck> {
    checks
        .into_iter()
        .map(|check| {
            let required = match check.name.as_str() {
                "node" => false,
                "cgroup_v2" => require_cgroup,
                _ => true,
            };
            DoctorCheck { check, required }
        })
        .collect()
}

fn doctor_passed(checks: &[DoctorCheck]) -> bool {
    checks.iter().all(|c| !c.required || c.check.available)
}

fn doctor_label(check: &DoctorCheck) -> &'static str {
    if check.check.available {
        "OK"
    } else if check.required {
        "FAIL"
    } else {
        "WARN"
    }
}

fn toml_string(c: &Config) -> Result<String> {
    Ok(toml::to_string_pretty(c)?)
}
/// Serves the shared web interface on loopback.
///
/// The daemon keeps working when this process stops; only the browser
/// surface is bound here.
async fn web(home: PathBuf, profile: String, bind: String, assets: Option<PathBuf>) -> Result<()> {
    let address: std::net::SocketAddr = bind.parse().context("invalid bind address")?;
    ensure!(
        address.ip().is_loopback(),
        "the web interface only binds to loopback addresses"
    );
    // The daemon is started on demand and keeps running afterwards.
    let client = client::Client::connect(&home, &profile).await?;
    drop(client);
    let assets = assets.unwrap_or_else(|| default_web_assets(&home));
    let config = layeragent_web::WebConfig::new(address, home, profile, assets);
    let (app, code) = layeragent_web::build(&config)?;
    println!("LayerAgent web interface: http://{address}");
    println!("Access code (single use): {code}");
    println!("The code is shown once. It cannot be recovered later.");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let token = shutdown.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        token.cancel();
    });
    layeragent_web::serve_app(app, address, shutdown).await?;
    Ok(())
}

/// Locates the built frontend.
fn default_web_assets(home: &std::path::Path) -> PathBuf {
    let installed = home.join("web");
    if installed.join("index.html").is_file() {
        return installed;
    }
    std::env::current_dir()
        .unwrap_or_else(|_| home.to_path_buf())
        .join("frontend")
        .join("dist")
}

async fn serve(home: PathBuf, profile: String, ipc_only: bool) -> Result<()> {
    use fs2::FileExt;
    private_dir(&home)?;
    let runtime = home.join("runtime");
    private_dir(&runtime)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(runtime.join(format!("{profile}.lock")))?;
    lock.try_lock_exclusive()
        .context("a runtime already owns this profile")?;
    let engine = layeragent_core::Engine::new(home.clone(), profile.clone())?;
    engine.store(&layeragent_protocol::Scope::local()).await?;
    let config = engine.config().await;
    layeragent_gateway::start_channel_workers(engine.clone());
    #[cfg(unix)]
    {
        let path = runtime.join(format!("{profile}.sock"));
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        let listener = tokio::net::UnixListener::bind(&path)?;
        let router =
            layeragent_gateway::router(layeragent_gateway::ApiState::new(engine.clone(), true));
        let token = engine.shutdown.clone();
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, router)
                .with_graceful_shutdown(token.cancelled_owned())
                .await
            {
                tracing::error!(error=%e,"IPC server failed");
            }
        });
    }
    #[cfg(not(unix))]
    if ipc_only {
        // Loopback TCP stands in for a UNIX socket; a per-run secret authenticates callers.
        let token = format!(
            "{}{}",
            layeragent_protocol::new_id(),
            layeragent_protocol::new_id()
        );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        std::fs::write(
            client::endpoint_path(&home, &profile),
            serde_json::to_vec(&client::Endpoint {
                port,
                token: token.clone(),
            })?,
        )?;
        let router = layeragent_gateway::router(
            layeragent_gateway::ApiState::new(engine.clone(), true).with_ipc_token(token),
        );
        let shutdown = engine.shutdown.clone();
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, router)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
            {
                tracing::error!(error=%e,"IPC server failed");
            }
        });
    }
    let reload_engine = engine.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
        loop {
            tokio::select! { _=reload_engine.shutdown.cancelled()=>break, _=tick.tick()=>{} }
            if let Err(e) = reload_engine.reload().await {
                tracing::warn!(error=%e,"configuration reload rejected");
            }
        }
    });
    if ipc_only {
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=engine.shutdown.cancelled()=>{}}
        engine.shutdown_gracefully().await?;
    } else {
        ensure!(
            std::env::var(&config.gateway.token_env).is_ok(),
            "set {} to enable authenticated HTTP",
            config.gateway.token_env
        );
        let listener = tokio::net::TcpListener::bind(&config.gateway.bind).await?;
        let shutdown_engine = engine.clone();
        let router =
            layeragent_gateway::router(layeragent_gateway::ApiState::new(engine.clone(), false));
        eprintln!("LayerAgent API listening on {}", config.gateway.bind);
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                tokio::select! {_=tokio::signal::ctrl_c()=>{},_=shutdown_engine.shutdown.cancelled()=>{}}
                shutdown_engine.stop();
            })
            .await?;
        engine.shutdown_gracefully().await?;
    }
    let _ = std::fs::remove_file(runtime.join(format!("{profile}.sock")));
    let _ = std::fs::remove_file(client::endpoint_path(&home, &profile));
    drop(lock);
    Ok(())
}

#[cfg(test)]
mod doctor_tests {
    use super::*;

    #[test]
    fn cooperative_cli_parses_modes_controls_and_export_without_changing_defaults() -> Result<()> {
        let args = Args::try_parse_from(["layeragent", "run", "inspect"])?;
        let Some(Command::Run { execution, .. }) = args.command else {
            anyhow::bail!("run expected");
        };
        assert_eq!(execution.body()?["execution"], "delegated");
        let args = Args::try_parse_from([
            "layeragent",
            "run",
            "inspect",
            "--execution",
            "cooperative",
            "--template",
            "audit",
            "--max-tokens",
            "1000",
            "--max-seconds",
            "45",
            "--max-cost-usd",
            "1",
        ])?;
        let Some(Command::Run { execution, .. }) = args.command else {
            anyhow::bail!("run expected");
        };
        assert_eq!(
            execution.body()?["cooperative"]["limits"]["max_tokens"],
            1000
        );
        for command in [
            vec!["team", "watch", "task-one", "--json"],
            vec!["team", "pause", "task-one", "--reason", "wait"],
            vec!["team", "message", "task-one", "--to", "reviewer", "check"],
            vec!["team", "export", "task-one", "--format", "json"],
        ] {
            assert!(Args::try_parse_from(std::iter::once("layeragent").chain(command)).is_ok());
        }
        Ok(())
    }

    fn checks(unavailable: &str) -> Vec<layeragent_sandbox::Diagnostic> {
        [
            "user_namespace",
            "pid_namespace",
            "net_namespace",
            "all_namespaces",
            "landlock",
            "python3",
            "node",
            "cgroup_v2",
        ]
        .into_iter()
        .map(|name| layeragent_sandbox::Diagnostic {
            name: name.into(),
            available: name != unavailable,
            detail: "test capability".into(),
        })
        .collect()
    }

    #[test]
    fn configured_cgroups_are_required_and_optional_components_only_warn() {
        for (missing, required_cgroup, passed, label) in [
            ("user_namespace", false, false, "FAIL"),
            ("net_namespace", true, false, "FAIL"),
            ("landlock", false, false, "FAIL"),
            ("python3", false, false, "FAIL"),
            ("cgroup_v2", false, true, "WARN"),
            ("cgroup_v2", true, false, "FAIL"),
            ("node", false, true, "WARN"),
            ("node", true, true, "WARN"),
        ] {
            let report = doctor_report(checks(missing), required_cgroup);
            assert_eq!(
                doctor_passed(&report),
                passed,
                "missing {missing}, requested cgroup {required_cgroup}"
            );
            let check = report
                .iter()
                .find(|c| c.check.name == missing)
                .expect("fixture contains capability");
            assert_eq!(doctor_label(check), label);
        }
    }

    #[test]
    fn json_report_preserves_the_diagnostic_array_and_exposes_requirements() -> Result<()> {
        let report = doctor_report(checks("cgroup_v2"), false);
        let json = serde_json::to_value(&report)?;
        let entries = json
            .as_array()
            .context("doctor JSON must remain an array")?;
        let cgroup = entries
            .iter()
            .find(|c| c["name"] == "cgroup_v2")
            .context("cgroup check missing")?;
        assert_eq!(cgroup["available"], false);
        assert_eq!(cgroup["required"], false);
        assert_eq!(cgroup["detail"], "test capability");
        let namespace = entries
            .iter()
            .find(|c| c["name"] == "net_namespace")
            .context("namespace check missing")?;
        assert_eq!(namespace["required"], true);
        Ok(())
    }
}

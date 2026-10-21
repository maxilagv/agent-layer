use anyhow::{Context, Result, ensure};
use clap::Subcommand;
use layeragent_policy::{McpGrant, Operation, ToolGrant};
use layeragent_remote::{
    crypto::{self, Keys, Signed},
    transport::{self, RemoteClient},
    *,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

#[derive(Subcommand)]
pub enum RemoteCommand {
    /// Verify and decrypt an archived runner completion without trusting coordinator state.
    InspectCompletion {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        enrollment: PathBuf,
        #[arg(long)]
        completion: PathBuf,
        job: String,
    },
    /// Review and pin the local model configuration before enabling remote inference.
    ProfileRevision {
        #[arg(long)]
        home: PathBuf,
    },
    /// Resolve an unknown outcome after inspecting the destination and local receipts.
    Reconcile {
        #[arg(long)]
        config: PathBuf,
        job: String,
        #[arg(long,value_parser=["completed","failed"])]
        outcome: String,
        #[arg(long)]
        evidence: PathBuf,
    },
    /// Retransmit an unchanged saved submission after a lost response.
    Resend {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        submission: PathBuf,
    },
    /// Create private signing/encryption keys and public identity metadata.
    Keygen {
        #[arg(long)]
        keys: PathBuf,
        #[arg(long)]
        certificate: PathBuf,
        #[arg(long)]
        public: PathBuf,
    },
    /// Generate an expiring pairing request. Transfer it to your trusted owner client.
    PairRequest {
        #[arg(long)]
        keys: PathBuf,
        #[arg(long)]
        certificate: PathBuf,
        #[arg(long)]
        account: String,
        #[arg(long)]
        project: String,
        #[arg(long)]
        runner: String,
        #[arg(long)]
        device: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Confirm a displayed identity fingerprint and register its minimal capabilities.
    Authorize {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        confirm_fingerprint: String,
        #[arg(long, value_delimiter = ',')]
        operations: Vec<String>,
        #[arg(long, default_value_t = 1)]
        revision: u64,
        #[arg(long)]
        out: PathBuf,
    },
    /// Confirm the owner key locally and consume the matching pairing request once.
    PairAccept {
        #[arg(long)]
        keys: PathBuf,
        #[arg(long)]
        certificate: PathBuf,
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        enrollment: PathBuf,
        #[arg(long)]
        owner_key: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Write an explicit project-files grant. Existing files are never overwritten.
    Grant {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        project: String,
        #[arg(long, default_value = "default")]
        tenant: String,
        #[arg(long, default_value = "owner")]
        principal: String,
        #[arg(long, value_delimiter = ',')]
        operations: Vec<String>,
        #[arg(long, value_delimiter = ',')]
        destinations: Vec<String>,
        #[arg(long, default_value_t = 1)]
        revision: u64,
        #[arg(long)]
        out: PathBuf,
    },
    /// Show the application-owned catalog and its reviewed revision hash.
    Catalog,
    /// Serve the restricted project-files adapter on MCP stdio.
    ProjectFiles {
        #[arg(long)]
        grant: PathBuf,
    },
    /// Start the mTLS coordinator. Database connection is read from its configured env reference.
    Coordinator {
        #[arg(long)]
        config: PathBuf,
        #[arg(long, conflicts_with = "recover")]
        initialize: bool,
        #[arg(long)]
        recover: bool,
    },
    /// Start one locally paired project runner. Ctrl-C stops its active work.
    Runner {
        #[arg(long)]
        config: PathBuf,
    },
    /// Sign, encrypt and submit an explicit task for one enrolled runner.
    Submit {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        enrollment: PathBuf,
        #[arg(long)]
        task: PathBuf,
        #[arg(long)]
        id: Option<String>,
        #[arg(long, default_value_t = 3600)]
        ttl_secs: u64,
    },
    /// Inspect jobs; decrypt also verifies their runner signatures and owner enrollment.
    Jobs {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        decrypt: bool,
    },
    Runners {
        #[arg(long)]
        config: PathBuf,
    },
    Cancel {
        #[arg(long)]
        config: PathBuf,
        job: String,
    },
    Revoke {
        #[arg(long)]
        config: PathBuf,
        runner: String,
    },
    /// Terminal activity dashboard: j/k select, c cancel, r revoke runner, q exit.
    Tui {
        #[arg(long)]
        config: PathBuf,
    },
    /// Persistently block local execution, including after restarting the runner.
    Disconnect {
        #[arg(long)]
        state: PathBuf,
    },
    /// Remove the local block; does not undo a remote revocation or restart a service.
    Connect {
        #[arg(long)]
        state: PathBuf,
    },
    /// Inspect encrypted durable local receipts, including uncertain effects.
    LocalReceipts {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        job: Option<String>,
    },
}
fn load<T: DeserializeOwned>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&crypto::private_read(
        path,
        MAX_ENVELOPE as u64 + 65536,
    )?)?)
}
fn save<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    crypto::private_write(path, &serde_json::to_vec_pretty(value)?)
}
fn print<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
fn operations(values: Vec<String>) -> Result<BTreeSet<Operation>> {
    let result = values
        .into_iter()
        .map(|s| {
            Ok(match s.as_str() {
                "read" => Operation::Read,
                "write" => Operation::Write,
                "send" => Operation::Send,
                "delete" => Operation::Delete,
                "execute" => Operation::Execute,
                _ => anyhow::bail!("unsupported operation"),
            })
        })
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(
        !result.is_empty(),
        "at least one explicit operation required"
    );
    Ok(result)
}
fn client(config: &ClientConfig) -> Result<RemoteClient> {
    RemoteClient::new(
        &config.endpoint,
        &config.ca,
        &config.certificate,
        &config.tls_key,
    )
}
async fn control(config: &ClientConfig, action: &str, target: &str) -> Result<Value> {
    let now = layeragent_protocol::now_ms();
    let signed = Keys::load(&config.keys)?.sign(
        "control",
        Control {
            id: uuid::Uuid::new_v4().to_string(),
            action: action.into(),
            account: config.account.clone(),
            target: target.into(),
            epoch: config.epoch.clone(),
            issued_at_ms: now,
            expires_at_ms: now + 60000,
            evidence: None,
        },
    )?;
    client(config)?.post("control", &signed).await
}
pub async fn dispatch(command: RemoteCommand) -> Result<()> {
    ensure!(
        cfg!(target_os = "linux"),
        "remote administration currently requires Linux"
    );
    match command {
        RemoteCommand::InspectCompletion {
            config,
            enrollment,
            completion,
            job,
        } => {
            let config: ClientConfig = load(&config)?;
            let keys = Keys::load(&config.keys)?;
            let enrollment: Signed<Enrollment> = load(&enrollment)?;
            enrollment.verify("enrollment", &keys.public(String::new())?.signing_key)?;
            ensure!(
                enrollment.payload.request.account == config.account,
                "receipt account mismatch"
            );
            let completion: Signed<Completion> = load(&completion)?;
            completion.verify(
                "completion",
                &enrollment.payload.request.identity.signing_key,
            )?;
            ensure!(
                completion.payload.lease.job == job
                    && completion.payload.lease.runner == enrollment.payload.request.runner
                    && completion.payload.lease.epoch == enrollment.payload.epoch,
                "archived completion context mismatch"
            );
            print(
                &json!({"job":job,"outcome":completion.payload.outcome,"verified":true,"result":keys.decrypt::<Value>(&completion.payload.ciphertext)?}),
            )?;
        }
        RemoteCommand::ProfileRevision { home } => {
            let config = layeragent_config::Config::load(&home, "default")?;
            print(
                &json!({"revision":config.revision(),"models":config.models.keys().collect::<Vec<_>>()}),
            )?;
        }
        RemoteCommand::Reconcile {
            config,
            job,
            outcome,
            evidence,
        } => {
            let config: ClientConfig = load(&config)?;
            let keys = Keys::load(&config.keys)?;
            let proof = crypto::private_read(&evidence, 65536)?;
            let time = layeragent_protocol::now_ms();
            let id = uuid::Uuid::new_v4().to_string();
            let control = keys.sign(
                "control",
                Control {
                    id: id.clone(),
                    action: format!("reconcile_{outcome}"),
                    account: config.account.clone(),
                    target: job,
                    epoch: config.epoch.clone(),
                    issued_at_ms: time,
                    expires_at_ms: time + 60000,
                    evidence: Some(ReconciliationEvidence {
                        sha256: layeragent_policy::digest(&proof),
                        ciphertext: crypto::encrypt(
                            &std::str::from_utf8(&proof)?,
                            &[keys.public(String::new())?.encryption_key],
                        )?,
                    }),
                },
            )?;
            save(
                &config
                    .keys
                    .parent()
                    .context("key parent required")?
                    .join(format!("control-{id}.json")),
                &control,
            )?;
            print(
                &client(&config)?
                    .post::<_, Value>("control", &control)
                    .await?,
            )?;
        }
        RemoteCommand::Resend { config, submission } => {
            let config: ClientConfig = load(&config)?;
            let keys = Keys::load(&config.keys)?;
            let value: Value = load(&submission)?;
            let public = keys.public(String::new())?.signing_key;
            if value["kind"] == "job" {
                let envelope: Signed<JobEnvelope> = serde_json::from_value(value)?;
                envelope.verify("job", &public)?;
                ensure!(
                    envelope.payload.header.account == config.account
                        && envelope.payload.header.epoch == config.epoch,
                    "saved submission authority mismatch"
                );
                print(&client(&config)?.post::<_, Value>("jobs", &envelope).await?)?;
            } else {
                let control: Signed<Control> = serde_json::from_value(value)?;
                control.verify("control", &public)?;
                ensure!(
                    control.payload.account == config.account
                        && control.payload.epoch == config.epoch,
                    "saved control authority mismatch"
                );
                print(
                    &client(&config)?
                        .post::<_, Value>("control", &control)
                        .await?,
                )?;
            }
        }
        RemoteCommand::Keygen {
            keys,
            certificate,
            public,
        } => {
            let secret = Keys::generate()?;
            let identity = secret.public(transport::certificate_hash(&certificate)?)?;
            secret.save(&keys)?;
            save(&public, &identity)?;
            print(&identity)?;
        }
        RemoteCommand::PairRequest {
            keys,
            certificate,
            account,
            project,
            runner,
            device,
            out,
        } => {
            for id in [&account, &project, &runner, &device] {
                layeragent_protocol::validate_name(id)?;
            }
            let keys = Keys::load(&keys)?;
            let identity = keys.public(transport::certificate_hash(&certificate)?)?;
            let nonce = format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            );
            let request = keys.sign(
                "pair-request",
                PairRequest {
                    account,
                    project,
                    runner,
                    device,
                    nonce,
                    expires_at_ms: layeragent_protocol::now_ms() + 600000,
                    identity,
                },
            )?;
            save(&out, &request)?;
            print(
                &json!({"fingerprint":layeragent_policy::digest(request.payload.identity.signing_key.as_bytes()),"request":request.payload}),
            )?;
        }
        RemoteCommand::Authorize {
            config,
            request,
            confirm_fingerprint,
            operations: ops,
            revision,
            out,
        } => {
            let config: ClientConfig = load(&config)?;
            let request: Signed<PairRequest> = load(&request)?;
            request.verify("pair-request", &request.payload.identity.signing_key)?;
            ensure!(
                layeragent_policy::digest(request.payload.identity.signing_key.as_bytes())
                    == confirm_fingerprint,
                "pairing fingerprint does not match local confirmation"
            );
            let now = layeragent_protocol::now_ms();
            ensure!(
                request.payload.account == config.account
                    && request.payload.expires_at_ms > now
                    && request.payload.expires_at_ms <= now + 600000,
                "pairing account or expiry rejected"
            );
            let enrollment = Enrollment {
                request: request.payload,
                protocol: REMOTE_VERSION,
                epoch: config.epoch.clone(),
                policy_revision: revision,
                capabilities: operations(ops)?,
                runtime_version: env!("CARGO_PKG_VERSION").into(),
                limits: RunnerLimits::default(),
            };
            enrollment.validate()?;
            let enrollment = Keys::load(&config.keys)?.sign("enrollment", enrollment)?;
            save(&out, &enrollment)?;
            print(
                &client(&config)?
                    .post::<_, Value>("runners", &enrollment)
                    .await?,
            )?;
        }
        RemoteCommand::PairAccept {
            keys,
            certificate,
            request,
            enrollment,
            owner_key,
            out,
        } => {
            let keys = Keys::load(&keys)?;
            let request: Signed<PairRequest> = load(&request)?;
            let enrollment: Signed<Enrollment> = load(&enrollment)?;
            let public = keys.public(transport::certificate_hash(&certificate)?)?;
            request.verify("pair-request", &public.signing_key)?;
            enrollment.verify("enrollment", &owner_key)?;
            enrollment.payload.validate()?;
            ensure!(
                serde_json::to_value(&request.payload)?
                    == serde_json::to_value(&enrollment.payload.request)?
                    && serde_json::to_value(public)?
                        == serde_json::to_value(&enrollment.payload.request.identity)?,
                "pairing identity or account differs from pending local request"
            );
            ensure!(
                request.payload.expires_at_ms > layeragent_protocol::now_ms(),
                "local pairing expired"
            );
            let marker = out
                .parent()
                .context("enrollment parent required")?
                .join(format!(".pairing-{}.used", request.payload.nonce));
            crypto::private_write(&marker, b"consumed")?;
            save(&out, &enrollment)?;
            print(
                &json!({"paired":true,"account":enrollment.payload.request.account,"project":enrollment.payload.request.project,"runner":enrollment.payload.request.runner}),
            )?;
        }
        RemoteCommand::Grant {
            root,
            project,
            tenant,
            principal,
            operations: ops,
            destinations,
            revision,
            out,
        } => {
            let ops = operations(ops)?;
            let destinations = destinations.into_iter().collect::<BTreeSet<_>>();
            ensure!(
                !destinations.is_empty(),
                "explicit destinations required; use . only to grant the project root"
            );
            let tools = layeragent_policy::file_catalog()
                .into_iter()
                .filter_map(|t| {
                    let name = t["name"].as_str()?.to_string();
                    let operation = layeragent_policy::operation(&name).ok()?;
                    ops.contains(&operation).then(|| {
                        (
                            name,
                            ToolGrant {
                                operations: BTreeSet::from([operation]),
                                destinations: destinations.clone(),
                            },
                        )
                    })
                })
                .collect::<BTreeMap<_, _>>();
            let grant = McpGrant {
                scope: layeragent_protocol::Scope { tenant, principal },
                project_id: project,
                root: root.canonicalize()?,
                revision,
                catalog_sha256: layeragent_policy::catalog_hash(),
                tools,
            };
            grant.validate()?;
            save(&out, &grant)?;
            print(&grant)?;
        }
        RemoteCommand::Catalog => print(
            &json!({"sha256":layeragent_policy::catalog_hash(),"tools":layeragent_policy::file_catalog()}),
        )?,
        RemoteCommand::ProjectFiles { grant } => {
            #[cfg(target_os = "linux")]
            {
                use rmcp::ServiceExt;
                let broker = std::sync::Arc::new(
                    layeragent_policy::files::FileBroker::open_reviewed_file(&grant)?,
                );
                let grant = broker.grant().clone();
                layeragent_policy::mcp::ProjectFiles {
                    broker,
                    grant,
                    cancel: CancellationToken::new(),
                }
                .serve(rmcp::transport::stdio())
                .await?
                .waiting()
                .await?;
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = grant;
                anyhow::bail!("project-files requires Linux");
            }
        }
        RemoteCommand::Coordinator {
            config,
            initialize,
            recover,
        } => {
            let cancel = shutdown_signal();
            transport::serve(load(&config)?, initialize, recover, cancel).await?;
        }
        RemoteCommand::Runner { config } => {
            #[cfg(target_os = "linux")]
            layeragent_remote::runner::run(load(&config)?, &config, shutdown_signal()).await?;
            #[cfg(not(target_os = "linux"))]
            {
                let _ = config;
                anyhow::bail!("runner requires Linux");
            }
        }
        RemoteCommand::Submit {
            config,
            enrollment,
            task,
            id,
            ttl_secs,
        } => {
            let config: ClientConfig = load(&config)?;
            let keys = Keys::load(&config.keys)?;
            let enrollment: Signed<Enrollment> = load(&enrollment)?;
            enrollment.verify("enrollment", &keys.public(String::new())?.signing_key)?;
            enrollment.payload.validate()?;
            let e = enrollment.payload;
            ensure!(
                e.request.account == config.account && e.epoch == config.epoch,
                "recipient account mismatch"
            );
            let task: Task = load(&task)?;
            ensure!(
                (1..=86400).contains(&ttl_secs),
                "job lifetime must be 1..86400 seconds"
            );
            let ops = task.operations()?;
            ensure!(
                ops.is_subset(&e.capabilities),
                "requested task exceeds enrolled capabilities"
            );
            let now = layeragent_protocol::now_ms();
            let id = id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            layeragent_protocol::validate_name(&id)?;
            let envelope = keys.sign(
                "job",
                JobEnvelope {
                    header: JobHeader {
                        version: REMOTE_VERSION,
                        id: id.clone(),
                        account: config.account.clone(),
                        project: e.request.project,
                        runner: e.request.runner,
                        device: e.request.device,
                        epoch: config.epoch.clone(),
                        policy_revision: e.policy_revision,
                        catalog_sha256: layeragent_policy::catalog_hash(),
                        issued_at_ms: now,
                        expires_at_ms: now + ttl_secs as i64 * 1000,
                        operations: ops,
                        retry_safe: task.retry_safe()?,
                    },
                    ciphertext: crypto::encrypt(
                        &task,
                        &[
                            e.request.identity.encryption_key,
                            keys.public(String::new())?.encryption_key,
                        ],
                    )?,
                },
            )?;
            // Durable submission artifact permits exact retransmission after a lost response.
            let artifact = config
                .keys
                .parent()
                .context("key parent required")?
                .join(format!("submission-{id}.json"));
            save(&artifact, &envelope)?;
            print(&client(&config)?.post::<_, Value>("jobs", &envelope).await?)?;
            print(&json!({"job":id,"submission_saved":true}))?;
        }
        RemoteCommand::Jobs { config, decrypt } => {
            let config: ClientConfig = load(&config)?;
            let client = client(&config)?;
            let mut jobs: Value = client.get("jobs").await?;
            if decrypt {
                let keys = Keys::load(&config.keys)?;
                let owner = keys.public(String::new())?.signing_key;
                let runners: Value = client.get("runners").await?;
                for job in jobs.as_array_mut().context("invalid jobs")? {
                    let detail: Value = client
                        .get(&format!(
                            "jobs/{}",
                            job["id"].as_str().context("job ID required")?
                        ))
                        .await?;
                    job["result"] = detail["result"].clone();
                    job["outcome"] = detail["outcome"].clone();
                    if job["result"].is_null() {
                        continue;
                    }
                    let runner = runners
                        .as_array()
                        .context("invalid runner list")?
                        .iter()
                        .find(|r| r["id"] == job["runner"])
                        .context("unknown result signer")?;
                    let enrollment: Signed<Enrollment> =
                        serde_json::from_value(runner["enrollment"].clone())?;
                    enrollment.verify("enrollment", &owner)?;
                    let result: Signed<Completion> = serde_json::from_value(job["result"].clone())?;
                    result.verify(
                        "completion",
                        &enrollment.payload.request.identity.signing_key,
                    )?;
                    ensure!(
                        result.payload.lease.job == job["id"]
                            && result.payload.lease.runner == job["runner"]
                            && result.payload.lease.epoch == enrollment.payload.epoch,
                        "result context mismatch"
                    );
                    ensure!(
                        result.payload.lease.attempt
                            == job["attempt"].as_i64().context("attempt required")?
                            && enrollment.payload.request.project == job["project"],
                        "result attempt or project mismatch"
                    );
                    job["historical_authority"] = json!(result.payload.lease.epoch != config.epoch);
                    job["decrypted"] = keys.decrypt::<Value>(&result.payload.ciphertext)?;
                }
            }
            print(&jobs)?;
        }
        RemoteCommand::Runners { config } => {
            let config: ClientConfig = load(&config)?;
            print(&client(&config)?.get::<Value>("runners").await?)?;
        }
        RemoteCommand::Cancel { config, job } => {
            print(&control(&load(&config)?, "cancel", &job).await?)?
        }
        RemoteCommand::Revoke { config, runner } => {
            print(&control(&load(&config)?, "revoke", &runner).await?)?
        }
        RemoteCommand::Tui { config } => dashboard(load(&config)?).await?,
        RemoteCommand::Disconnect { state } => {
            layeragent_config::private_dir(&state)?;
            if !state.join("STOP").exists() {
                crypto::private_write(&state.join("STOP"), b"local disconnect")?;
            }
            print(&json!({"locally_blocked":true}))?;
        }
        RemoteCommand::Connect { state } => {
            if state.join("STOP").exists() {
                crypto::private_read(&state.join("STOP"), 65536)?;
                std::fs::remove_file(state.join("STOP"))?;
            }
            print(&json!({"local_block_removed":true,"remote_revocation_unchanged":true}))?;
        }
        RemoteCommand::LocalReceipts { state, job } => {
            #[cfg(target_os = "linux")]
            {
                let journal = layeragent_remote::runner::Journal::open(&state)?;
                print(&if let Some(job) = job {
                    journal.completion(&job)?
                } else {
                    journal.inspect()?
                })?;
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (state, job);
                anyhow::bail!("runner receipts require Linux");
            }
        }
    }
    Ok(())
}
fn shutdown_signal() -> CancellationToken {
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        stop.cancel();
    });
    cancel
}
async fn dashboard(config: ClientConfig) -> Result<()> {
    use crossterm::event::{self, Event, KeyCode};
    use ratatui::{
        layout::{Constraint, Direction, Layout},
        widgets::{Block, Borders, Paragraph},
    };
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
            let _ =
                crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen);
        }
    }
    let client = client(&config)?;
    crossterm::terminal::enable_raw_mode()?;
    let _restore = Restore;
    crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen)?;
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout()))?;
    let mut jobs = Vec::<Value>::new();
    let mut selected = 0usize;
    let mut status = String::new();
    let mut pending: Option<(String, String)> = None;
    let mut refresh = std::time::Instant::now() - std::time::Duration::from_secs(10);
    loop {
        if refresh.elapsed() >= std::time::Duration::from_secs(2) {
            match client.get::<Vec<Value>>("jobs").await {
                Ok(v) => {
                    jobs = v;
                    selected = selected.min(jobs.len().saturating_sub(1));
                }
                Err(_) => {
                    status = "Coordinator unavailable; local permits expire independently".into()
                }
            }
            refresh = std::time::Instant::now();
        }
        terminal.draw(|frame| {
            let layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(4)])
                .split(frame.area());
            let lines = jobs
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    format!(
                        "{} {}  {}  {}",
                        if i == selected { ">" } else { " " },
                        v["id"].as_str().unwrap_or("?"),
                        v["status"].as_str().unwrap_or("?"),
                        v["runner"].as_str().unwrap_or("?")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            frame.render_widget(
                Paragraph::new(lines).block(
                    Block::default()
                        .title("LayerAgent · Remote activity")
                        .borders(Borders::ALL),
                ),
                layout[0],
            );
            frame.render_widget(
                Paragraph::new(format!(
                    "j/k select · c cancel · r revoke runner · q exit\n{status}"
                ))
                .block(Block::default().borders(Borders::ALL)),
                layout[1],
            );
        })?;
        if event::poll(std::time::Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            match key.code {
                KeyCode::Char('q') => break,
                KeyCode::Char('j') => selected = (selected + 1).min(jobs.len().saturating_sub(1)),
                KeyCode::Char('k') => selected = selected.saturating_sub(1),
                KeyCode::Char('c' | 'r') => {
                    if let Some(job) = jobs.get(selected) {
                        let action = if key.code == KeyCode::Char('c') {
                            "cancel"
                        } else {
                            "revoke"
                        };
                        let target = job[if action == "cancel" { "id" } else { "runner" }]
                            .as_str()
                            .context("invalid target")?
                            .to_string();
                        status = format!("Confirm {action} {target}: y / any other key to dismiss");
                        pending = Some((action.into(), target));
                    }
                }
                KeyCode::Char('y') => {
                    if let Some((action, target)) = pending.take() {
                        status = if control(&config, &action, &target).await.is_ok() {
                            "Control accepted; already produced effects may require review".into()
                        } else {
                            "Control not confirmed; inspect coordinator state before retrying"
                                .into()
                        };
                        refresh -= std::time::Duration::from_secs(3);
                    }
                }
                _ => {
                    pending = None;
                }
            }
        }
    }
    Ok(())
}

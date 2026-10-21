use crate::{
    crypto::{Keys, Signed},
    transport::RemoteClient,
    *,
};
use anyhow::{Context, Result, ensure};
use layeragent_policy::{McpGrant, files::FileBroker};
use layeragent_sandbox::{ExecRequest, NativeSandbox, Sandbox};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

/// CLOCK_BOOTTIME includes suspend. Never restore a cached lease after restart.
mod agent;
pub fn boot_ms() -> Result<i64> {
    let mut t = std::mem::MaybeUninit::<libc::timespec>::uninit();
    ensure!(
        unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, t.as_mut_ptr()) } == 0,
        "lease clock unavailable"
    );
    let t = unsafe { t.assume_init() };
    Ok(t.tv_sec.saturating_mul(1000) + t.tv_nsec / 1_000_000)
}
pub struct Permit {
    pub(crate) job: String,
    deadline: AtomicI64,
    cancel: CancellationToken,
    state: PathBuf,
    grant: PathBuf,
    grant_digest: String,
    authority_files: Vec<(PathBuf, String)>,
}
fn check_authority(grant: &Path, grant_digest: &str, files: &[(PathBuf, String)]) -> Result<()> {
    ensure!(
        layeragent_policy::digest(&crypto::private_read(grant, 65536)?) == grant_digest,
        "local grant changed"
    );
    for (path, digest) in files {
        ensure!(
            layeragent_policy::digest(&crypto::private_read(path, 65536)?) == *digest,
            "local authority configuration changed"
        );
    }
    Ok(())
}
impl Permit {
    pub(crate) fn check(&self) -> Result<()> {
        ensure!(
            !self.cancel.is_cancelled() && boot_ms()? < self.deadline.load(Ordering::SeqCst),
            "execution permit expired or cancelled"
        );
        ensure!(
            !self.state.join("STOP").exists(),
            "runner locally disconnected"
        );
        check_authority(&self.grant, &self.grant_digest, &self.authority_files)
    }
}
pub struct Journal {
    db: Connection,
}
impl Journal {
    pub fn open(state: &Path) -> Result<Self> {
        layeragent_config::private_dir(state)?;
        let path = state.join("receipts.sqlite");
        if !path.exists() {
            crypto::private_write(&path, &[])?;
        }
        crypto::private_file(&path, 256 * 1024 * 1024)?;
        let db = Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        let version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
        ensure!(
            version <= 1,
            "newer runner journal cannot be opened by this runtime"
        );
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS receipts(job TEXT PRIMARY KEY,digest TEXT NOT NULL,state TEXT NOT NULL,ciphertext TEXT,completion TEXT);
            CREATE TABLE IF NOT EXISTS authority(singleton INTEGER PRIMARY KEY CHECK(singleton=1),binding TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS activity(seq INTEGER PRIMARY KEY,job TEXT NOT NULL,state TEXT NOT NULL,at_ms INTEGER NOT NULL);")?;
        db.pragma_update(None, "user_version", 1)?;
        Ok(Self { db })
    }
    pub fn bind(&self, binding: &str) -> Result<()> {
        let previous: Option<String> = self
            .db
            .query_row("SELECT binding FROM authority WHERE singleton=1", [], |r| {
                r.get(0)
            })
            .optional()?;
        if let Some(previous) = previous {
            ensure!(
                previous == binding,
                "runner account or enrollment changed; fresh state and explicit pairing required"
            );
        } else {
            self.db.execute(
                "INSERT INTO authority(singleton,binding) VALUES(1,?1)",
                [binding],
            )?;
        }
        Ok(())
    }
    /// Return a durable prior outcome. An unacknowledged mutation is never replayed.
    fn begin(&self, job: &str, digest: &str, retry_safe: bool) -> Result<Option<(String, String)>> {
        let row: Option<(String, String, Option<String>)> = self
            .db
            .query_row(
                "SELECT digest,state,ciphertext FROM receipts WHERE job=?1",
                [job],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((old, state, cipher)) = row {
            ensure!(
                old == digest,
                "job identifier reused with different ciphertext"
            );
            if let Some(cipher) = cipher {
                return Ok(Some((state, cipher)));
            }
            ensure!(
                retry_safe,
                "previous effect outcome unknown; local reconciliation required"
            );
            return Ok(None);
        }
        self.db.execute(
            "INSERT INTO receipts(job,digest,state) VALUES(?1,?2,'intent')",
            params![job, digest],
        )?;
        self.activity(job, "intent")?;
        Ok(None)
    }
    fn finish(&self, job: &str, state: &str, ciphertext: &str) -> Result<()> {
        self.db.execute(
            "UPDATE receipts SET state=?2,ciphertext=?3 WHERE job=?1",
            params![job, state, ciphertext],
        )?;
        self.activity(job, state)
    }
    fn activity(&self, job: &str, state: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO activity(job,state,at_ms) VALUES(?1,?2,?3)",
            params![job, state, layeragent_protocol::now_ms()],
        )?;
        Ok(())
    }
    pub fn inspect(&self) -> Result<Value> {
        let mut query = self.db.prepare(
            "SELECT job,state,ciphertext,completion FROM receipts ORDER BY rowid DESC LIMIT 1000",
        )?;
        let rows=query.query_map([],|r|Ok(json!({"job":r.get::<_,String>(0)?,"state":r.get::<_,String>(1)?,"ciphertext":r.get::<_,Option<String>>(2)?,"completion":r.get::<_,Option<String>>(3)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
        Ok(json!(rows))
    }
    pub fn completion(&self, job: &str) -> Result<Value> {
        let result: Option<String> = self
            .db
            .query_row(
                "SELECT completion FROM receipts WHERE job=?1",
                [job],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        Ok(serde_json::from_str(&result.context(
            "signed local completion not available; effect may still be uncertain",
        )?)?)
    }
    fn record_completion(&self, completion: &Signed<Completion>) -> Result<()> {
        self.db.execute(
            "UPDATE receipts SET completion=?2 WHERE job=?1",
            params![
                completion.payload.lease.job,
                serde_json::to_string(completion)?
            ],
        )?;
        Ok(())
    }
}
fn validate_assignment(
    assignment: &Assignment,
    enrollment: &Enrollment,
    owner: &str,
    task: &Task,
    grant: &McpGrant,
) -> Result<()> {
    assignment.envelope.verify("job", owner)?;
    let h = &assignment.envelope.payload.header;
    let l = &assignment.lease;
    let r = &enrollment.request;
    ensure!(
        h.version == REMOTE_VERSION
            && h.id == l.job
            && h.runner == r.runner
            && l.runner == r.runner
            && h.device == r.device
            && h.account == r.account
            && h.project == r.project,
        "job recipient mismatch"
    );
    ensure!(
        h.epoch == enrollment.epoch
            && l.epoch == h.epoch
            && h.policy_revision == enrollment.policy_revision
            && l.policy_revision == grant.revision
            && h.policy_revision == grant.revision,
        "authority revision mismatch"
    );
    ensure!(
        h.catalog_sha256 == grant.catalog_sha256 && grant.project_id == r.project,
        "catalog or project changed"
    );
    ensure!(
        h.operations == task.operations()?
            && h.retry_safe == task.retry_safe()?
            && h.operations.is_subset(&enrollment.capabilities),
        "task exceeds signed permissions"
    );
    let time = layeragent_protocol::now_ms();
    ensure!(
        h.issued_at_ms <= time + 30_000
            && h.expires_at_ms > time
            && h.expires_at_ms
                .checked_sub(h.issued_at_ms)
                .is_some_and(|lifetime| (1..=86_400_000).contains(&lifetime))
            && l.expires_at_ms > time
            && l.expires_at_ms <= time + LEASE_MS + 30_000,
        "expired task or clock outside tolerance"
    );
    ensure!(
        l.attempt > 0
            && l.generation.len() >= 32
            && l.renewal == 0
            && (1..=LEASE_MS).contains(&l.valid_for_ms),
        "invalid initial lease"
    );
    if let Task::File { tool, arguments } = task {
        grant.authorize(&grant.scope, tool, arguments)?;
    }
    Ok(())
}
pub async fn run(
    config: RunnerConfig,
    configuration: &Path,
    cancel: CancellationToken,
) -> Result<()> {
    ensure!(
        unsafe { libc::geteuid() } != 0,
        "remote runners must use an unprivileged project account, not root"
    );
    let configuration_bytes = crypto::private_read(configuration, 65536)?;
    ensure!(
        serde_json::to_value(serde_json::from_slice::<RunnerConfig>(
            &configuration_bytes
        )?)? == serde_json::to_value(&config)?,
        "configuration changed during startup"
    );
    let key_bytes = crypto::private_read(&config.keys, 8192)?;
    let keys: Keys = serde_json::from_slice(&key_bytes)?;
    let enrollment_bytes = crypto::private_read(&config.enrollment, 65536)?;
    let signed: Signed<Enrollment> = serde_json::from_slice(&enrollment_bytes)?;
    let authority_files = vec![
        (
            configuration.to_path_buf(),
            layeragent_policy::digest(&configuration_bytes),
        ),
        (
            config.enrollment.clone(),
            layeragent_policy::digest(&enrollment_bytes),
        ),
        (config.keys.clone(), layeragent_policy::digest(&key_bytes)),
        (
            config.tls_key.clone(),
            layeragent_policy::digest(&crypto::private_read(&config.tls_key, 65536)?),
        ),
    ];
    signed.verify("enrollment", &config.owner_signing_key)?;
    signed.payload.validate()?;
    let enrollment = signed.payload;
    config.limits.validate()?;
    ensure!(
        config.limits == enrollment.limits,
        "local resource limits differ from enrollment"
    );
    let public = keys.public(transport::certificate_hash(&config.certificate)?)?;
    ensure!(
        serde_json::to_value(&public)? == serde_json::to_value(&enrollment.request.identity)?,
        "local keys or certificate differ from confirmed pairing"
    );
    let grant_bytes = crypto::private_read(&config.grant, 65536)?;
    let grant_digest = layeragent_policy::digest(&grant_bytes);
    let grant: McpGrant = serde_json::from_slice(&grant_bytes)?;
    grant.validate()?;
    ensure!(
        grant.project_id == enrollment.request.project
            && grant.revision == enrollment.policy_revision,
        "local project grant differs from enrollment"
    );
    layeragent_config::private_dir(&config.state)?;
    let state = config.state.canonicalize()?;
    let root = grant.root.canonicalize()?;
    for path in [
        &config.keys,
        &config.tls_key,
        &config.enrollment,
        &config.grant,
        &config.state,
        configuration,
    ] {
        ensure!(
            !path.canonicalize()?.starts_with(&root),
            "runner authority files cannot reside inside an authorized project"
        );
    }
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(state.join("runner.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock).context("runner already active")?;
    let journal = Journal::open(&state)?;
    journal.bind(&layeragent_policy::digest(&serde_json::to_vec(&(
        &enrollment,
        &config.owner_signing_key,
        &grant,
        &config,
    ))?))?;
    let broker = Arc::new(FileBroker::open(grant.clone())?);
    if enrollment.capabilities.contains(&Operation::Execute) || !config.allowed_models.is_empty() {
        let probe = tempfile::tempdir_in(&state)?;
        let output = NativeSandbox::new(std::env::current_exe()?)
            .execute(
                ExecRequest {
                    argv: vec!["/usr/bin/true".into()],
                    workspace: probe.path().into(),
                    stdin: None,
                    policy: layeragent_config::Policy {
                        network: false,
                        write: false,
                        limits: layeragent_config::Limits {
                            cgroup: true,
                            memory_bytes: Some(config.limits.memory_bytes),
                            max_pids: Some(config.limits.max_pids),
                            cpu_percent: Some(u32::from(config.limits.cpu_percent)),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                },
                cancel.clone(),
                None,
            )
            .await?;
        ensure!(
            output.code == Some(0) && !output.interrupted,
            "native runner qualification failed"
        );
    }
    let client = RemoteClient::new(
        &config.endpoint,
        &config.ca,
        &config.certificate,
        &config.tls_key,
    )?;
    loop {
        if cancel.is_cancelled() || state.join("STOP").exists() {
            break;
        }
        check_authority(&config.grant, &grant_digest, &authority_files)?;
        let started = boot_ms()?;
        let assignment: Option<Assignment> = match client.post("claim", &json!({})).await {
            Ok(assignment) => assignment,
            Err(_) => {
                tokio::select! {_=cancel.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(3))=>{}};
                continue;
            }
        };
        let Some(assignment) = assignment else {
            tokio::select! {_=cancel.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(1))=>{}};
            continue;
        };
        check_authority(&config.grant, &grant_digest, &authority_files)?;
        assignment
            .envelope
            .verify("job", &config.owner_signing_key)?;
        let task = keys
            .decrypt::<Task>(&assignment.envelope.payload.ciphertext)
            .and_then(|task| {
                validate_assignment(
                    &assignment,
                    &enrollment,
                    &config.owner_signing_key,
                    &task,
                    &grant,
                )?;
                Ok(task)
            });
        let task = match task {
            Ok(task) => task,
            Err(_) => {
                let completion=keys.sign("completion",Completion{lease:assignment.lease,outcome:"failed".into(),ciphertext:crypto::encrypt(&json!({"state":"rejected","reason":"local project, destination, schema or capability policy rejected this job"}),std::slice::from_ref(&config.owner_encryption_key))?})?;
                let _ = client.post::<_, Value>("complete", &completion).await;
                continue;
            }
        };
        let digest = layeragent_policy::digest(&serde_json::to_vec(&assignment.envelope)?);
        let prior = journal.begin(
            &assignment.lease.job,
            &digest,
            assignment.envelope.payload.header.retry_safe,
        );
        let mut lease = assignment.lease;
        let (outcome, ciphertext) = match prior {
            Ok(Some(prior)) => prior,
            Err(_) => (
                "unknown".into(),
                crypto::encrypt(
                    &json!({"state":"unknown","reason":"prior effect requires local reconciliation"}),
                    std::slice::from_ref(&config.owner_encryption_key),
                )?,
            ),
            Ok(None) => {
                let permit = Arc::new(Permit {
                    job: lease.job.clone(),
                    deadline: AtomicI64::new(started + lease.valid_for_ms),
                    cancel: cancel.child_token(),
                    state: state.clone(),
                    grant: config.grant.clone(),
                    grant_digest: grant_digest.clone(),
                    authority_files: authority_files.clone(),
                });
                permit.check()?;
                let work = timed_execute(&task, &config, broker.clone(), permit.clone());
                tokio::pin!(work);
                let mut heartbeat = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECS));
                heartbeat.tick().await;
                let mut watch = tokio::time::interval(Duration::from_millis(200));
                let result = loop {
                    tokio::select! {
                        result=&mut work=>break result,
                        _=watch.tick()=>{if permit.check().is_err(){permit.cancel.cancel();break work.await;}},
                        _=heartbeat.tick()=>{
                            let sent=boot_ms()?;
                            match guarded_renew(client.post::<_,Lease>("renew",&lease),&permit).await {
                                Ok(next) if next.job==lease.job && next.generation==lease.generation && next.attempt==lease.attempt && next.runner==lease.runner && next.epoch==lease.epoch && next.policy_revision==lease.policy_revision && next.renewal==lease.renewal+1 && (1..=LEASE_MS).contains(&next.valid_for_ms) && sent+next.valid_for_ms>boot_ms()? && next.expires_at_ms<=layeragent_protocol::now_ms()+LEASE_MS+30_000=>{
                                    permit.deadline.store(sent+next.valid_for_ms,Ordering::SeqCst);lease=next;
                                },
                                _=>{permit.cancel.cancel();break work.await;}
                            }
                        }
                    }
                };
                let (outcome, value) = match result {
                    Ok(v) => ("completed", v),
                    Err(_) => {
                        if assignment.envelope.payload.header.retry_safe {
                            (
                                "failed",
                                json!({"state":"failed","reason":"read operation failed or was interrupted"}),
                            )
                        } else {
                            (
                                "unknown",
                                json!({"state":"unknown","reason":"operation rejected, failed or interrupted; inspect local effects before resubmitting"}),
                            )
                        }
                    }
                };
                let (outcome, ciphertext) = match crypto::encrypt(
                    &value,
                    std::slice::from_ref(&config.owner_encryption_key),
                ) {
                    Ok(ciphertext) => (outcome, ciphertext),
                    Err(_) => {
                        let state = if assignment.envelope.payload.header.retry_safe {
                            "failed"
                        } else {
                            "unknown"
                        };
                        (
                            state,
                            crypto::encrypt(
                                &json!({"state":state,"reason":"result exceeded encryption limit; inspect local effects before resubmitting"}),
                                std::slice::from_ref(&config.owner_encryption_key),
                            )?,
                        )
                    }
                };
                journal.finish(&lease.job, outcome, &ciphertext)?;
                (outcome.into(), ciphertext)
            }
        };
        let completion = keys.sign(
            "completion",
            Completion {
                lease,
                outcome,
                ciphertext,
            },
        )?;
        journal.record_completion(&completion)?;
        // Retrying an identical completion is safe; retrying the effect is not.
        for _ in 0..2 {
            if client
                .post::<_, Value>("complete", &completion)
                .await
                .is_ok()
            {
                break;
            }
        }
    }
    Ok(())
}
async fn execute(
    task: &Task,
    config: &RunnerConfig,
    broker: Arc<FileBroker>,
    permit: Arc<Permit>,
) -> Result<Value> {
    match task {
        Task::Agent { .. } => agent::execute_agent(task, config, broker, permit).await,
        Task::File { tool, arguments } => {
            let scope = load_grant(&config.grant)?.scope;
            let tool = tool.clone();
            let args = arguments.clone();
            tokio::task::spawn_blocking(move || {
                broker.invoke_checked(&scope, &tool, &args, || permit.check())
            })
            .await?
        }
        Task::Execute { template } => {
            let template = config
                .templates
                .get(template)
                .context("execution template not locally approved")?;
            ensure!(
                !template.argv.is_empty()
                    && (1..=config.limits.execution_secs).contains(&template.timeout_secs),
                "invalid execution template"
            );
            permit.check()?;
            let snapshot = tempfile::tempdir_in(&config.state)?;
            let grant = load_grant(&config.grant)?;
            snapshot_project(&broker, &grant, snapshot.path(), &permit)?;
            let policy = layeragent_config::Policy {
                network: false,
                write: true,
                timeout_secs: template.timeout_secs,
                max_output_bytes: 131072,
                limits: layeragent_config::Limits {
                    cgroup: true,
                    memory_bytes: Some(config.limits.memory_bytes),
                    max_pids: Some(config.limits.max_pids),
                    cpu_percent: Some(u32::from(config.limits.cpu_percent)),
                    ..Default::default()
                },
                ..Default::default()
            };
            let sandbox = NativeSandbox::new(std::env::current_exe()?);
            permit.check()?;
            let output = sandbox
                .execute(
                    ExecRequest {
                        argv: template.argv.clone(),
                        workspace: snapshot.path().into(),
                        policy,
                        stdin: None,
                    },
                    permit.cancel.clone(),
                    None,
                )
                .await?;
            permit.check()?;
            ensure!(!output.interrupted, "execution interrupted");
            Ok(
                json!({"execution":output,"workspace_changes":"isolated scratch only; project was not modified"}),
            )
        }
    }
}
/// Keep the local watchdog running while HTTP is stalled. A late response must
/// never revive a permit that has already expired or been locally revoked.
async fn guarded_renew(
    future: impl std::future::Future<Output = Result<Lease>>,
    permit: &Permit,
) -> Result<Lease> {
    tokio::pin!(future);
    let mut watch = tokio::time::interval(Duration::from_millis(50));
    permit.check()?;
    loop {
        tokio::select! {
            result=&mut future=>{permit.check()?;return result;},
            _=watch.tick()=>permit.check()?,
        }
    }
}
async fn timed_execute(
    task: &Task,
    config: &RunnerConfig,
    broker: Arc<FileBroker>,
    permit: Arc<Permit>,
) -> Result<Value> {
    let work = execute(task, config, broker, permit.clone());
    tokio::pin!(work);
    tokio::select! {
        result=&mut work=>result,
        _=tokio::time::sleep(Duration::from_secs(config.limits.execution_secs))=>{
            permit.cancel.cancel();
            let _=tokio::time::timeout(Duration::from_secs(5),&mut work).await;
            anyhow::bail!("whole-job execution deadline exceeded; reconcile effects")
        }
    }
}
pub(crate) fn snapshot_project(
    broker: &FileBroker,
    grant: &McpGrant,
    destination: &Path,
    permit: &Permit,
) -> Result<std::collections::BTreeMap<String, String>> {
    let mut directories = vec![".".to_string()];
    let mut count = 0;
    let mut bytes = 0;
    let mut files = std::collections::BTreeMap::new();
    while let Some(directory) = directories.pop() {
        permit.check()?;
        let listing = broker.invoke_checked(
            &grant.scope,
            "list_files",
            &json!({"path":directory}),
            || permit.check(),
        )?;
        for name in listing["entries"].as_array().context("invalid listing")? {
            count += 1;
            ensure!(count <= 1000, "snapshot entry limit exceeded");
            let name = name.as_str().context("invalid name")?;
            if ["target", "node_modules", ".venv", "__pycache__"].contains(&name) {
                continue;
            }
            let path = if directory == "." {
                name.into()
            } else {
                format!("{directory}/{name}")
            };
            if listing["directories"]
                .as_array()
                .context("directory types required")?
                .iter()
                .any(|d| d == name)
            {
                directories.push(path);
                continue;
            }
            let data = broker.read_bytes_checked(&grant.scope, &path, || permit.check())?;
            bytes += data.len();
            ensure!(bytes <= 16 * 1024 * 1024, "snapshot byte limit exceeded");
            let target = destination.join(&path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(target, &data)?;
            files.insert(path, layeragent_policy::digest(&data));
        }
    }
    Ok(files)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uncertain_mutations_are_never_replayed() {
        let state = tempfile::tempdir().unwrap();
        let journal = Journal::open(state.path()).unwrap();
        journal.bind("account-project-runner").unwrap();
        assert!(journal.bind("other-account").is_err());
        assert!(journal.begin("job", "digest", false).unwrap().is_none());
        assert!(journal.begin("job", "digest", false).is_err());
        assert!(journal.begin("job", "other", true).is_err());
        journal.finish("job", "completed", "cipher").unwrap();
        assert_eq!(
            journal.begin("job", "digest", false).unwrap(),
            Some(("completed".into(), "cipher".into()))
        );
    }
    #[test]
    fn expired_or_locally_revoked_permits_fail() {
        let state = tempfile::tempdir().unwrap();
        let grant = state.path().join("grant");
        crypto::private_write(&grant, b"grant").unwrap();
        let p = Permit {
            job: "test".into(),
            deadline: AtomicI64::new(boot_ms().unwrap() + 10000),
            cancel: CancellationToken::new(),
            state: state.path().into(),
            grant: grant.clone(),
            grant_digest: layeragent_policy::digest(b"grant"),
            authority_files: vec![],
        };
        p.check().unwrap();
        crypto::private_write(&state.path().join("STOP"), b"stop").unwrap();
        assert!(p.check().is_err());
        std::fs::remove_file(state.path().join("STOP")).unwrap();
        p.deadline.store(0, Ordering::SeqCst);
        assert!(p.check().is_err());
    }
    #[tokio::test]
    async fn a_stalled_renewal_cannot_block_the_local_watchdog() {
        let state = tempfile::tempdir().unwrap();
        let grant = state.path().join("grant");
        crypto::private_write(&grant, b"grant").unwrap();
        let permit = Permit {
            job: "watchdog".into(),
            deadline: AtomicI64::new(boot_ms().unwrap() + 30),
            cancel: CancellationToken::new(),
            state: state.path().into(),
            grant,
            grant_digest: layeragent_policy::digest(b"grant"),
            authority_files: vec![],
        };
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            guarded_renew(std::future::pending(), &permit),
        )
        .await;
        assert!(result.unwrap().is_err());
    }
}

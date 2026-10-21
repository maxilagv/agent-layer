//! PostgreSQL is the assignment authority, including across coordinator restarts.
//! A transaction advisory lock serializes control-plane transitions. This first
//! personal deployment favors auditable consistency over scheduler throughput.
use crate::{crypto::Signed, *};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_postgres::{Client, NoTls, Transaction};

#[derive(Clone)]
pub struct Queue {
    db: Arc<Mutex<Client>>,
    pub account: String,
    pub epoch: String,
    pub owner: String,
    guard: PathBuf,
}
async fn lock(tx: &Transaction<'_>) -> Result<()> {
    tx.batch_execute("SELECT pg_advisory_xact_lock(844173294)")
        .await?;
    Ok(())
}
async fn now(tx: &Transaction<'_>) -> Result<i64> {
    Ok(tx
        .query_one(
            "SELECT (extract(epoch FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .await?
        .get(0))
}
async fn event(tx: &Transaction<'_>, kind: &str, target: &str, state: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO la_remote_events(kind,target,state) VALUES($1,$2,$3)",
        &[&kind, &target, &state],
    )
    .await?;
    Ok(())
}
impl Queue {
    pub async fn open(
        dsn: &str,
        account: &str,
        epoch: &str,
        owner: &str,
        guard: &std::path::Path,
        initialize: bool,
        recover: bool,
    ) -> Result<Self> {
        layeragent_protocol::validate_name(account)?;
        crypto::public_key(owner)?;
        ensure!(epoch.len() >= 32, "authority epoch required");
        let config: tokio_postgres::Config = dsn.parse()?;
        ensure!(
            !config.get_hosts().is_empty()
                && config.get_hosts().iter().all(|host| match host {
                    tokio_postgres::config::Host::Tcp(host) => host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback()),
                    #[cfg(unix)]
                    tokio_postgres::config::Host::Unix(_) => true,
                }),
            "database must use a local Unix socket or explicit loopback address"
        );
        let (mut client, connection) = config.connect(NoTls).await?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        ensure!(
            client
                .query_one("SELECT pg_try_advisory_lock(844173295)", &[])
                .await?
                .get::<_, bool>(0),
            "another coordinator owns this database; stop it before starting or recovering"
        );
        let tx = client.transaction().await?;
        lock(&tx).await?;
        tx.batch_execute(include_str!("schema.sql")).await?;
        let row=tx.query_opt("SELECT account,epoch,owner_key,control_sequence,schema_version FROM la_remote_authority WHERE singleton",&[]).await?;
        if let Some(row) = row {
            ensure!(
                row.get::<_, i32>(4) == 1,
                "unsupported remote database schema version"
            );
            ensure!(
                row.get::<_, String>(0) == account
                    && (row.get::<_, String>(2) == owner
                        || (recover && row.get::<_, String>(1) != epoch)),
                "authority identity mismatch"
            );
            if row.get::<_, String>(1) != epoch {
                ensure!(
                    recover,
                    "epoch mismatch: explicit local recovery and re-enrollment required"
                );
                tx.execute("UPDATE la_remote_runners SET revoked=TRUE", &[])
                    .await?;
                tx.execute("UPDATE la_remote_jobs SET status=CASE WHEN status='pending' THEN 'cancelled' ELSE 'unknown' END WHERE status IN ('pending','leased')",&[]).await?;
                tx.execute(
                    "UPDATE la_remote_authority SET epoch=$1,owner_key=$2",
                    &[&epoch, &owner],
                )
                .await?;
                crypto::private_replace(
                    guard,
                    &serde_json::to_vec(
                        &json!({"account":account,"epoch":epoch,"owner":owner,"sequence":row.get::<_,i64>(3)}),
                    )?,
                )?;
                event(&tx, "recovery", account, "re-enrollment-required").await?;
            } else {
                ensure!(!recover, "recovery requires a fresh authority epoch");
                let checkpoint: Value =
                    serde_json::from_slice(&crypto::private_read(guard, 65536)?)?;
                ensure!(
                    checkpoint
                        == json!({"account":account,"epoch":epoch,"owner":owner,"sequence":row.get::<_,i64>(3)}),
                    "authority rollback or incomplete control transaction: fresh epoch and explicit recovery required"
                );
            }
        } else {
            ensure!(
                initialize && !recover,
                "uninitialized authority; explicit initialization required"
            );
            tx.execute(
                "INSERT INTO la_remote_authority(account,epoch,owner_key) VALUES($1,$2,$3)",
                &[&account, &epoch, &owner],
            )
            .await?;
            ensure!(
                !guard.exists(),
                "existing authority checkpoint cannot initialize a new database"
            );
            crypto::private_replace(
                guard,
                &serde_json::to_vec(
                    &json!({"account":account,"epoch":epoch,"owner":owner,"sequence":0}),
                )?,
            )?;
        }
        // No lease is restored from process memory or database snapshots.
        tx.execute("UPDATE la_remote_jobs SET status=CASE WHEN retry_safe AND attempt<3 THEN 'pending' ELSE 'unknown' END WHERE status='leased'",&[]).await?;
        tx.commit().await?;
        Ok(Self {
            db: Arc::new(Mutex::new(client)),
            account: account.into(),
            epoch: epoch.into(),
            owner: owner.into(),
            guard: guard.into(),
        })
    }
    async fn authority(&self, tx: &Transaction<'_>) -> Result<()> {
        let row=tx.query_one("SELECT account,epoch,owner_key,control_sequence,schema_version FROM la_remote_authority WHERE singleton",&[]).await?;
        ensure!(
            row.get::<_, String>(0) == self.account
                && row.get::<_, String>(1) == self.epoch
                && row.get::<_, String>(2) == self.owner,
            "authority changed; restart coordinator"
        );
        let checkpoint: Value = serde_json::from_slice(&crypto::private_read(&self.guard, 65536)?)?;
        ensure!(
            checkpoint
                == json!({"account":self.account,"epoch":self.epoch,"owner":self.owner,"sequence":row.get::<_,i64>(3)}),
            "authority rollback or incomplete control transaction; recovery required"
        );
        Ok(())
    }
    pub async fn enroll(&self, enrollment: Signed<Enrollment>) -> Result<()> {
        enrollment.verify("enrollment", &self.owner)?;
        enrollment.payload.validate()?;
        let e = &enrollment.payload;
        ensure!(
            e.request.account == self.account && e.epoch == self.epoch,
            "enrollment authority mismatch"
        );
        let mut db = self.db.lock().await;
        let tx = db.transaction().await?;
        lock(&tx).await?;
        self.authority(&tx).await?;
        let time = now(&tx).await?;
        ensure!(
            e.request.expires_at_ms > time && e.request.expires_at_ms - time <= 600_000,
            "pairing expired or excessive lifetime"
        );
        if let Some(row) = tx
            .query_opt(
                "SELECT enrollment,revoked FROM la_remote_runners WHERE id=$1",
                &[&e.request.runner],
            )
            .await?
        {
            ensure!(
                !row.get::<_, bool>(1)
                    && row.get::<_, Value>(0) == serde_json::to_value(&enrollment)?,
                "runner identity already exists; fresh identity required"
            );
            return Ok(());
        }
        tx.execute(
            "INSERT INTO la_remote_pairings(nonce,runner) VALUES($1,$2)",
            &[&e.request.nonce, &e.request.runner],
        )
        .await?;
        tx.execute("INSERT INTO la_remote_runners(id,project,device,certificate,signing_key,enrollment) VALUES($1,$2,$3,$4,$5,$6)",&[&e.request.runner,&e.request.project,&e.request.device,&e.request.identity.certificate_sha256,&e.request.identity.signing_key,&serde_json::to_value(&enrollment)?]).await?;
        event(&tx, "runner", &e.request.runner, "enrolled").await?;
        tx.commit().await?;
        Ok(())
    }
    async fn runner(tx: &Transaction<'_>, certificate: &str) -> Result<Enrollment> {
        let row = tx
            .query_opt(
                "SELECT enrollment FROM la_remote_runners WHERE certificate=$1 AND NOT revoked",
                &[&certificate],
            )
            .await?
            .context("runner not authorized")?;
        let e: Signed<Enrollment> = serde_json::from_value(row.get(0))?;
        e.payload.validate()?;
        Ok(e.payload)
    }
    async fn expire(tx: &Transaction<'_>) -> Result<()> {
        tx.execute("UPDATE la_remote_jobs SET status=CASE WHEN retry_safe AND attempt<3 AND deadline_ms>(extract(epoch FROM clock_timestamp())*1000)::bigint THEN 'pending' ELSE 'unknown' END WHERE status='leased' AND expires_at<=clock_timestamp()",&[]).await?;
        tx.execute("UPDATE la_remote_jobs SET status='cancelled' WHERE status='pending' AND deadline_ms<=(extract(epoch FROM clock_timestamp())*1000)::bigint",&[]).await?;
        Ok(())
    }
    pub async fn submit(&self, envelope: Signed<JobEnvelope>) -> Result<()> {
        envelope.verify("job", &self.owner)?;
        let h = &envelope.payload.header;
        ensure!(
            h.version == REMOTE_VERSION && h.account == self.account && h.epoch == self.epoch,
            "job authority mismatch"
        );
        layeragent_protocol::validate_name(&h.id)?;
        ensure!(
            serde_json::to_vec(&envelope)?.len() <= MAX_ENVELOPE,
            "job exceeds limit"
        );
        ensure!(
            h.catalog_sha256 == layeragent_policy::catalog_hash() && !h.operations.is_empty(),
            "job policy mismatch"
        );
        ensure!(
            !h.retry_safe || h.operations == BTreeSet::from([Operation::Read]),
            "unsafe retry class"
        );
        let value = serde_json::to_value(&envelope)?;
        let digest = layeragent_policy::digest(&serde_json::to_vec(&value)?);
        let mut db = self.db.lock().await;
        let tx = db.transaction().await?;
        lock(&tx).await?;
        self.authority(&tx).await?;
        let time = now(&tx).await?;
        ensure!(
            h.issued_at_ms <= time + 30_000
                && h.expires_at_ms > time
                && h.expires_at_ms
                    .checked_sub(h.issued_at_ms)
                    .is_some_and(|lifetime| (1..=86_400_000).contains(&lifetime)),
            "job lifetime rejected"
        );
        if let Some(row) = tx
            .query_opt("SELECT digest FROM la_remote_jobs WHERE id=$1", &[&h.id])
            .await?
        {
            ensure!(
                row.get::<_, String>(0) == digest,
                "idempotency key reused with different content"
            );
            return Ok(());
        }
        let row = tx
            .query_opt(
                "SELECT enrollment FROM la_remote_runners WHERE id=$1 AND NOT revoked",
                &[&h.runner],
            )
            .await?
            .context("runner not eligible")?;
        let e: Signed<Enrollment> = serde_json::from_value(row.get(0))?;
        let e = e.payload;
        ensure!(
            e.request.project == h.project
                && e.request.device == h.device
                && e.epoch == h.epoch
                && e.policy_revision == h.policy_revision
                && h.operations.is_subset(&e.capabilities),
            "runner incompatible with job"
        );
        ensure!(tx.query_one("SELECT count(*) FROM la_remote_jobs WHERE status IN ('pending','leased','unknown')",&[]).await?.get::<_,i64>(0)<1000,"queue capacity exceeded; reconcile unknown jobs");
        tx.execute("INSERT INTO la_remote_jobs(id,runner,project,envelope,digest,status,retry_safe,deadline_ms) VALUES($1,$2,$3,$4,$5,'pending',$6,$7)",&[&h.id,&h.runner,&h.project,&value,&digest,&h.retry_safe,&h.expires_at_ms]).await?;
        event(&tx, "job", &h.id, "pending").await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn claim(&self, certificate: &str) -> Result<Option<Assignment>> {
        let mut db = self.db.lock().await;
        let tx = db.transaction().await?;
        lock(&tx).await?;
        self.authority(&tx).await?;
        let runner = Self::runner(&tx, certificate).await?;
        ensure!(runner.epoch == self.epoch, "runner epoch expired");
        Self::expire(&tx).await?;
        tx.execute(
            "UPDATE la_remote_runners SET last_seen=clock_timestamp() WHERE id=$1",
            &[&runner.request.runner],
        )
        .await?;
        if tx
            .query_one(
                "SELECT count(*) FROM la_remote_jobs WHERE runner=$1 AND status='leased'",
                &[&runner.request.runner],
            )
            .await?
            .get::<_, i64>(0)
            > 0
        {
            tx.commit().await?;
            return Ok(None);
        }
        let Some(row)=tx.query_opt("SELECT id,envelope,attempt FROM la_remote_jobs WHERE runner=$1 AND project=$2 AND status='pending' ORDER BY created_at,id LIMIT 1 FOR UPDATE",&[&runner.request.runner,&runner.request.project]).await? else{tx.commit().await?;return Ok(None);};
        let envelope: Signed<JobEnvelope> = serde_json::from_value(row.get(1))?;
        let h = &envelope.payload.header;
        ensure!(
            h.policy_revision == runner.policy_revision
                && h.operations.is_subset(&runner.capabilities),
            "runner capability changed"
        );
        let generation = uuid::Uuid::new_v4().to_string();
        let id: String = row.get(0);
        let attempt: i64 = row.get::<_, i64>(2) + 1;
        let time = now(&tx).await?;
        let expiry = time.saturating_add(LEASE_MS).min(h.expires_at_ms);
        tx.execute("UPDATE la_remote_jobs SET status='leased',attempt=$2,generation=$3,renewal=0,expires_at=to_timestamp($4::bigint::double precision/1000) WHERE id=$1",&[&id,&attempt,&generation,&expiry]).await?;
        event(&tx, "job", &id, "leased").await?;
        tx.commit().await?;
        Ok(Some(Assignment {
            envelope,
            lease: Lease {
                job: id,
                attempt,
                generation,
                runner: runner.request.runner,
                epoch: self.epoch.clone(),
                policy_revision: runner.policy_revision,
                renewal: 0,
                valid_for_ms: expiry - time,
                expires_at_ms: expiry,
            },
        }))
    }
    async fn check_lease(
        &self,
        tx: &Transaction<'_>,
        certificate: &str,
        lease: &Lease,
    ) -> Result<Enrollment> {
        self.authority(tx).await?;
        let e = Self::runner(tx, certificate).await?;
        ensure!(
            lease.runner == e.request.runner
                && lease.epoch == self.epoch
                && lease.policy_revision == e.policy_revision,
            "lease identity mismatch"
        );
        ensure!(tx.query_opt("SELECT id FROM la_remote_jobs WHERE id=$1 AND runner=$2 AND attempt=$3 AND generation=$4 AND renewal=$5 AND status='leased' AND expires_at>clock_timestamp()",&[&lease.job,&lease.runner,&lease.attempt,&lease.generation,&lease.renewal]).await?.is_some(),"stale, cancelled or expired assignment");
        Ok(e)
    }
    pub async fn renew(&self, certificate: &str, lease: Lease) -> Result<Lease> {
        let mut db = self.db.lock().await;
        let tx = db.transaction().await?;
        lock(&tx).await?;
        self.check_lease(&tx, certificate, &lease).await?;
        let deadline: i64 = tx
            .query_one(
                "SELECT deadline_ms FROM la_remote_jobs WHERE id=$1",
                &[&lease.job],
            )
            .await?
            .get(0);
        let time = now(&tx).await?;
        let expiry = time.saturating_add(LEASE_MS).min(deadline);
        ensure!(expiry > time, "job deadline expired");
        let renewal = lease.renewal + 1;
        tx.execute("UPDATE la_remote_jobs SET renewal=$2,expires_at=to_timestamp($3::bigint::double precision/1000) WHERE id=$1",&[&lease.job,&renewal,&expiry]).await?;
        tx.commit().await?;
        Ok(Lease {
            expires_at_ms: expiry,
            valid_for_ms: expiry - time,
            renewal,
            ..lease
        })
    }
    pub async fn complete(&self, certificate: &str, completion: Signed<Completion>) -> Result<()> {
        let mut db = self.db.lock().await;
        let tx = db.transaction().await?;
        lock(&tx).await?;
        self.authority(&tx).await?;
        let e = Self::runner(&tx, certificate).await?;
        completion.verify("completion", &e.request.identity.signing_key)?;
        let c = &completion.payload;
        ensure!(
            c.lease.runner == e.request.runner
                && c.lease.epoch == self.epoch
                && ["completed", "failed", "unknown"].contains(&c.outcome.as_str())
                && c.ciphertext.len() <= MAX_ENVELOPE,
            "invalid completion"
        );
        let value = serde_json::to_value(&completion)?;
        if let Some(row)=tx.query_opt("SELECT result FROM la_remote_jobs WHERE id=$1 AND runner=$2 AND status IN ('completed','failed','unknown')",&[&c.lease.job,&c.lease.runner]).await? {let prior:Option<Value>=row.get(0);ensure!(prior.as_ref()==Some(&value),"obsolete completion");return Ok(());}
        self.check_lease(&tx, certificate, &c.lease).await?;
        tx.execute(
            "UPDATE la_remote_jobs SET status=$2,outcome=$2,result=$3 WHERE id=$1",
            &[&c.lease.job, &c.outcome, &value],
        )
        .await?;
        event(&tx, "job", &c.lease.job, &c.outcome).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn control(&self, control: Signed<Control>) -> Result<()> {
        control.verify("control", &self.owner)?;
        let c = &control.payload;
        layeragent_protocol::validate_name(&c.id)?;
        layeragent_protocol::validate_name(&c.target)?;
        ensure!(
            c.account == self.account
                && c.epoch == self.epoch
                && [
                    "cancel",
                    "revoke",
                    "reconcile_completed",
                    "reconcile_failed"
                ]
                .contains(&c.action.as_str()),
            "invalid control authority"
        );
        let digest = layeragent_policy::digest(&serde_json::to_vec(&control)?);
        let mut db = self.db.lock().await;
        let tx = db.transaction().await?;
        lock(&tx).await?;
        self.authority(&tx).await?;
        if let Some(row) = tx
            .query_opt(
                "SELECT digest FROM la_remote_controls WHERE id=$1",
                &[&c.id],
            )
            .await?
        {
            ensure!(row.get::<_, String>(0) == digest, "control replay conflict");
            return Ok(());
        }
        let time = now(&tx).await?;
        ensure!(
            c.issued_at_ms <= time + 30_000
                && c.expires_at_ms > time
                && c.expires_at_ms
                    .checked_sub(c.issued_at_ms)
                    .is_some_and(|lifetime| (1..=60_000).contains(&lifetime)),
            "expired control"
        );
        if c.action == "revoke" {
            tx.execute(
                "UPDATE la_remote_runners SET revoked=TRUE WHERE id=$1",
                &[&c.target],
            )
            .await?;
            tx.execute("UPDATE la_remote_jobs SET status=CASE WHEN status='pending' THEN 'cancelled' ELSE 'unknown' END WHERE runner=$1 AND status IN ('pending','leased')",&[&c.target]).await?;
        } else if c.action == "cancel" {
            tx.execute("UPDATE la_remote_jobs SET status=CASE WHEN status='pending' THEN 'cancelled' ELSE 'unknown' END WHERE id=$1 AND status IN ('pending','leased')",&[&c.target]).await?;
        } else {
            let evidence = c
                .evidence
                .as_ref()
                .context("reconciliation evidence required")?;
            ensure!(
                evidence.sha256.len() == 64
                    && hex::decode(&evidence.sha256)?.len() == 32
                    && evidence.ciphertext.len() <= MAX_ENVELOPE,
                "invalid reconciliation evidence"
            );
            let outcome = if c.action == "reconcile_completed" {
                "completed"
            } else {
                "failed"
            };
            ensure!(tx.execute("UPDATE la_remote_jobs SET status=$2,outcome=$3 WHERE id=$1 AND status='unknown'",&[&c.target,&outcome,&c.action]).await?==1,"only unknown jobs may be reconciled");
        }
        tx.execute(
            "INSERT INTO la_remote_controls(id,digest,envelope) VALUES($1,$2,$3)",
            &[&c.id, &digest, &serde_json::to_value(&control)?],
        )
        .await?;
        event(&tx, &c.action, &c.target, "blocked").await?;
        let sequence:i64=tx.query_one("UPDATE la_remote_authority SET control_sequence=control_sequence+1 RETURNING control_sequence",&[]).await?.get(0);
        // Write-ahead checkpoint: a crash before COMMIT blocks the authority,
        // rather than silently permitting a lost revocation after restoration.
        crypto::private_replace(
            &self.guard,
            &serde_json::to_vec(
                &json!({"account":self.account,"epoch":self.epoch,"owner":self.owner,"sequence":sequence}),
            )?,
        )?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn jobs(&self) -> Result<Value> {
        let mut db = self.db.lock().await;
        let tx = db.transaction().await?;
        lock(&tx).await?;
        self.authority(&tx).await?;
        Self::expire(&tx).await?;
        let rows=tx.query("SELECT id,runner,project,status,attempt FROM la_remote_jobs ORDER BY created_at DESC LIMIT 1000",&[]).await?;
        let result=json!(rows.into_iter().map(|r|json!({"id":r.get::<_,String>(0),"runner":r.get::<_,String>(1),"project":r.get::<_,String>(2),"status":r.get::<_,String>(3),"attempt":r.get::<_,i64>(4)})).collect::<Vec<_>>());
        tx.commit().await?;
        Ok(result)
    }
    pub async fn job(&self, id: &str) -> Result<Value> {
        layeragent_protocol::validate_name(id)?;
        let mut db = self.db.lock().await;
        let tx = db.transaction().await?;
        lock(&tx).await?;
        self.authority(&tx).await?;
        let row = tx
            .query_opt(
                "SELECT result,outcome FROM la_remote_jobs WHERE id=$1",
                &[&id],
            )
            .await?
            .context("job not found")?;
        let result = json!({"result":row.get::<_,Option<Value>>(0),"outcome":row.get::<_,Option<String>>(1)});
        tx.commit().await?;
        Ok(result)
    }
    pub async fn runners(&self) -> Result<Value> {
        let mut db = self.db.lock().await;
        let tx = db.transaction().await?;
        lock(&tx).await?;
        self.authority(&tx).await?;
        let rows=tx.query("SELECT id,project,device,revoked,last_seen::text,enrollment FROM la_remote_runners ORDER BY id LIMIT 1000",&[]).await?;
        let result=json!(rows.into_iter().map(|r|json!({"id":r.get::<_,String>(0),"project":r.get::<_,String>(1),"device":r.get::<_,String>(2),"revoked":r.get::<_,bool>(3),"last_seen":r.get::<_,Option<String>>(4),"enrollment":r.get::<_,Value>(5)})).collect::<Vec<_>>());
        tx.commit().await?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires a disposable empty PostgreSQL database in LAYERAGENT_REMOTE_TEST_DSN"]
    async fn durable_queue_fences_revocation_replays_and_unknown_effects() -> Result<()> {
        let dsn = std::env::var("LAYERAGENT_REMOTE_TEST_DSN")?;
        let state = tempfile::tempdir()?;
        layeragent_config::private_dir(state.path())?;
        let owner = crypto::Keys::generate()?;
        let runner = crypto::Keys::generate()?;
        let owner_public = owner.public(String::new())?;
        let certificate = "a".repeat(64);
        let epoch = uuid::Uuid::new_v4().to_string();
        let queue = Queue::open(
            &dsn,
            "account",
            &epoch,
            &owner_public.signing_key,
            &state.path().join("guard"),
            true,
            false,
        )
        .await?;
        let enrollment = Enrollment {
            request: PairRequest {
                account: "account".into(),
                project: "project".into(),
                runner: "runner".into(),
                device: "device".into(),
                nonce: "b".repeat(64),
                expires_at_ms: layeragent_protocol::now_ms() + 600000,
                identity: runner.public(certificate.clone())?,
            },
            protocol: REMOTE_VERSION,
            epoch: epoch.clone(),
            policy_revision: 1,
            capabilities: BTreeSet::from([Operation::Read, Operation::Write]),
            runtime_version: env!("CARGO_PKG_VERSION").into(),
            limits: RunnerLimits::default(),
        };
        let mut incompatible = enrollment.clone();
        incompatible.runtime_version = "0.0.0".into();
        assert!(
            queue
                .enroll(owner.sign("enrollment", incompatible)?)
                .await
                .is_err()
        );
        queue.enroll(owner.sign("enrollment", enrollment)?).await?;
        let submit = |id: &str, read: bool| {
            owner.sign(
                "job",
                JobEnvelope {
                    header: JobHeader {
                        version: REMOTE_VERSION,
                        id: id.into(),
                        account: "account".into(),
                        project: "project".into(),
                        runner: "runner".into(),
                        device: "device".into(),
                        epoch: epoch.clone(),
                        policy_revision: 1,
                        catalog_sha256: layeragent_policy::catalog_hash(),
                        issued_at_ms: layeragent_protocol::now_ms(),
                        expires_at_ms: layeragent_protocol::now_ms() + 3600000,
                        operations: BTreeSet::from([if read {
                            Operation::Read
                        } else {
                            Operation::Write
                        }]),
                        retry_safe: read,
                    },
                    ciphertext: "opaque-test-payload".into(),
                },
            )
        };
        let first = submit("read", true)?;
        queue.submit(first.clone()).await?;
        queue.submit(first).await?;
        assert!(queue.claim(&"z".repeat(64)).await.is_err());
        let (a, b) = tokio::join!(queue.claim(&certificate), queue.claim(&certificate));
        let assignment = a?.or(b?).context("assignment required")?;
        let old = assignment.lease;
        let renewed = queue.renew(&certificate, old.clone()).await?;
        assert!(queue.renew(&certificate, old.clone()).await.is_err());
        {
            let db = queue.db.lock().await;
            db.execute("UPDATE la_remote_jobs SET expires_at=clock_timestamp()-interval '1 second' WHERE id='read'",&[]).await?;
        }
        let reassigned = queue
            .claim(&certificate)
            .await?
            .context("read retry required")?;
        assert!(reassigned.lease.attempt > renewed.attempt);
        let completion = |lease: Lease| {
            runner.sign(
                "completion",
                Completion {
                    lease,
                    outcome: "completed".into(),
                    ciphertext: "opaque-result".into(),
                },
            )
        };
        assert!(
            queue
                .complete(&certificate, completion(renewed)?)
                .await
                .is_err()
        );
        let current = completion(reassigned.lease)?;
        queue.complete(&certificate, current.clone()).await?;
        queue.complete(&certificate, current).await?;
        queue.submit(submit("write", false)?).await?;
        let mutation = queue
            .claim(&certificate)
            .await?
            .context("mutation required")?;
        {
            let db = queue.db.lock().await;
            db.execute("UPDATE la_remote_jobs SET expires_at=clock_timestamp()-interval '1 second' WHERE id='write'",&[]).await?;
        }
        assert!(queue.claim(&certificate).await?.is_none());
        assert!(
            queue
                .complete(&certificate, completion(mutation.lease)?)
                .await
                .is_err()
        );
        assert!(
            queue
                .jobs()
                .await?
                .as_array()
                .context("jobs required")?
                .iter()
                .any(|job| job["id"] == "write" && job["status"] == "unknown")
        );
        let time = layeragent_protocol::now_ms();
        let reconcile = owner.sign(
            "control",
            Control {
                id: "reconcile-write".into(),
                action: "reconcile_failed".into(),
                account: "account".into(),
                target: "write".into(),
                epoch: epoch.clone(),
                issued_at_ms: time,
                expires_at_ms: time + 60000,
                evidence: Some(ReconciliationEvidence {
                    sha256: "a".repeat(64),
                    ciphertext: "owner-encrypted-inspection".into(),
                }),
            },
        )?;
        queue.control(reconcile.clone()).await?;
        queue.control(reconcile).await?;
        assert!(
            queue
                .jobs()
                .await?
                .as_array()
                .context("jobs required")?
                .iter()
                .any(|job| job["id"] == "write" && job["status"] == "failed")
        );
        queue.submit(submit("queued", true)?).await?;
        let time = layeragent_protocol::now_ms();
        let revoke = owner.sign(
            "control",
            Control {
                id: "revoke".into(),
                action: "revoke".into(),
                account: "account".into(),
                target: "runner".into(),
                epoch: epoch.clone(),
                issued_at_ms: time,
                expires_at_ms: time + 60000,
                evidence: None,
            },
        )?;
        queue.control(revoke.clone()).await?;
        queue.control(revoke).await?;
        assert!(queue.claim(&certificate).await.is_err());
        assert!(queue.submit(submit("after-revoke", true)?).await.is_err());
        {
            let db = queue.db.lock().await;
            db.execute("UPDATE la_remote_authority SET control_sequence=0", &[])
                .await?;
        }
        assert!(queue.jobs().await.is_err());
        Ok(())
    }
}

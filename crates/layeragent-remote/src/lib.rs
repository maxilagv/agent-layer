//! Portable remote execution. The coordinator cannot decrypt task contents.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
pub mod crypto;
pub mod queue;
#[cfg(target_os = "linux")]
pub mod runner;
pub mod transport;
use anyhow::{Result, ensure};
use layeragent_policy::{McpGrant, Operation};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::PathBuf};

pub const REMOTE_VERSION: u32 = 1;
pub const LEASE_MS: i64 = 60_000;
pub const HEARTBEAT_SECS: u64 = 10;
pub const MAX_ENVELOPE: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicIdentity {
    pub signing_key: String,
    pub encryption_key: String,
    pub certificate_sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairRequest {
    pub account: String,
    pub project: String,
    pub runner: String,
    pub device: String,
    pub nonce: String,
    pub expires_at_ms: i64,
    pub identity: PublicIdentity,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Enrollment {
    pub request: PairRequest,
    pub protocol: u32,
    pub epoch: String,
    pub policy_revision: u64,
    pub capabilities: BTreeSet<Operation>,
    pub runtime_version: String,
    pub limits: RunnerLimits,
}
impl Enrollment {
    pub fn validate(&self) -> Result<()> {
        for id in [
            &self.request.account,
            &self.request.project,
            &self.request.runner,
            &self.request.device,
        ] {
            layeragent_protocol::validate_name(id)?;
        }
        ensure!(
            self.protocol == REMOTE_VERSION && self.policy_revision > 0,
            "incompatible enrollment"
        );
        ensure!(
            self.runtime_version == env!("CARGO_PKG_VERSION"),
            "runner runtime version incompatible"
        );
        self.limits.validate()?;
        ensure!(
            self.request.nonce.len() == 64 && self.epoch.len() >= 32,
            "invalid enrollment binding"
        );
        crypto::public_key(&self.request.identity.signing_key)?;
        crypto::recipient(&self.request.identity.encryption_key)?;
        ensure!(
            self.request.identity.certificate_sha256.len() == 64,
            "invalid transport identity"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobHeader {
    pub version: u32,
    pub id: String,
    pub account: String,
    pub project: String,
    pub runner: String,
    pub device: String,
    pub epoch: String,
    pub policy_revision: u64,
    pub catalog_sha256: String,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub operations: BTreeSet<Operation>,
    pub retry_safe: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobEnvelope {
    pub header: JobHeader,
    pub ciphertext: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Task {
    File {
        tool: String,
        arguments: serde_json::Value,
    },
    Execute {
        template: String,
    },
    Agent {
        prompt: String,
        model: String,
        #[serde(default)]
        apply_edits: bool,
        #[serde(default)]
        apply_deletes: bool,
        #[serde(default)]
        allow_shell: bool,
    },
}
impl Task {
    pub fn operations(&self) -> Result<BTreeSet<Operation>> {
        Ok(match self {
            Self::File { tool, .. } => BTreeSet::from([layeragent_policy::operation(tool)?]),
            Self::Execute { .. } => BTreeSet::from([Operation::Read, Operation::Execute]),
            Self::Agent {
                apply_edits,
                apply_deletes,
                allow_shell,
                ..
            } => {
                let mut operations = BTreeSet::from([Operation::Read, Operation::Send]);
                if *apply_edits {
                    operations.insert(Operation::Write);
                }
                if *apply_deletes {
                    operations.insert(Operation::Delete);
                }
                if *allow_shell {
                    operations.insert(Operation::Execute);
                }
                operations
            }
        })
    }
    pub fn retry_safe(&self) -> Result<bool> {
        Ok(self.operations()? == BTreeSet::from([Operation::Read]))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub job: String,
    pub attempt: i64,
    pub generation: String,
    pub runner: String,
    pub epoch: String,
    pub policy_revision: u64,
    pub renewal: i64,
    pub valid_for_ms: i64,
    pub expires_at_ms: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assignment {
    pub envelope: crypto::Signed<JobEnvelope>,
    pub lease: Lease,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Completion {
    pub lease: Lease,
    pub outcome: String,
    pub ciphertext: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Control {
    pub id: String,
    pub action: String,
    pub account: String,
    pub target: String,
    pub epoch: String,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    #[serde(default)]
    pub evidence: Option<ReconciliationEvidence>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationEvidence {
    pub sha256: String,
    pub ciphertext: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteTemplate {
    pub argv: Vec<String>,
    pub timeout_secs: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerLimits {
    pub memory_bytes: u64,
    pub max_pids: u64,
    pub cpu_percent: u16,
    pub execution_secs: u64,
    pub concurrent_jobs: u16,
}
impl Default for RunnerLimits {
    fn default() -> Self {
        Self {
            memory_bytes: 1024 * 1024 * 1024,
            max_pids: 64,
            cpu_percent: 100,
            execution_secs: 300,
            concurrent_jobs: 1,
        }
    }
}
impl RunnerLimits {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (64 * 1024 * 1024..=16 * 1024 * 1024 * 1024).contains(&self.memory_bytes)
                && (1..=512).contains(&self.max_pids)
                && (1..=100).contains(&self.cpu_percent)
                && (1..=300).contains(&self.execution_secs)
                && self.concurrent_jobs == 1,
            "runner resource limits rejected"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerConfig {
    pub endpoint: String,
    pub ca: PathBuf,
    pub certificate: PathBuf,
    pub tls_key: PathBuf,
    pub keys: PathBuf,
    pub owner_signing_key: String,
    pub owner_encryption_key: String,
    pub enrollment: PathBuf,
    pub state: PathBuf,
    pub grant: PathBuf,
    #[serde(default)]
    pub templates: std::collections::BTreeMap<String, ExecuteTemplate>,
    #[serde(default)]
    pub limits: RunnerLimits,
    #[serde(default)]
    pub agent_home: Option<PathBuf>,
    #[serde(default)]
    pub allowed_models: BTreeSet<String>,
    #[serde(default)]
    pub agent_profile_revision: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoordinatorConfig {
    pub bind: std::net::SocketAddr,
    pub ca: PathBuf,
    pub certificate: PathBuf,
    pub tls_key: PathBuf,
    pub database_env: String,
    pub account: String,
    pub epoch: String,
    pub owner_signing_key: String,
    pub owner_certificate_sha256: String,
    /// Store outside PostgreSQL backups; detects rollback of revocations/cancellations.
    pub authority_guard: PathBuf,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    pub endpoint: String,
    pub ca: PathBuf,
    pub certificate: PathBuf,
    pub tls_key: PathBuf,
    pub keys: PathBuf,
    pub account: String,
    pub epoch: String,
}
pub fn load_grant(path: &std::path::Path) -> Result<McpGrant> {
    let grant: McpGrant = serde_json::from_slice(&crypto::private_read(path, 65536)?)?;
    grant.validate()?;
    Ok(grant)
}

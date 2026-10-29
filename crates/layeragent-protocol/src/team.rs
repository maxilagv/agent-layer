//! Cooperative execution contracts shared by the runtime, storage and clients.
use anyhow::{Result, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Individual,
    #[default]
    Delegated,
    Cooperative,
}
impl ExecutionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Individual => "individual",
            Self::Delegated => "delegated",
            Self::Cooperative => "cooperative",
        }
    }
}
impl std::str::FromStr for ExecutionMode {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        Ok(match value {
            "individual" => Self::Individual,
            "delegated" => Self::Delegated,
            "cooperative" => Self::Cooperative,
            other => anyhow::bail!("unknown execution mode {other}"),
        })
    }
}

#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum TeamTemplate {
    #[default]
    Audit,
    Debug,
    Review,
}
impl TeamTemplate {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Audit => "audit",
            Self::Debug => "debug",
            Self::Review => "review",
        }
    }
}
impl std::str::FromStr for TeamTemplate {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        Ok(match value {
            "audit" => Self::Audit,
            "debug" | "debugging" => Self::Debug,
            "review" | "change_review" => Self::Review,
            other => anyhow::bail!("unknown cooperative template {other}"),
        })
    }
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum TeamRole {
    Coordinator,
    Analyst,
    Reviewer,
    Verifier,
    Specialist,
    Writer,
}
impl TeamRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Coordinator => "coordinator",
            Self::Analyst => "analyst",
            Self::Reviewer => "reviewer",
            Self::Verifier => "verifier",
            Self::Specialist => "specialist",
            Self::Writer => "writer",
        }
    }
    pub fn read_only(self) -> bool {
        !matches!(self, Self::Writer)
    }
}
impl std::str::FromStr for TeamRole {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        Ok(match value {
            "coordinator" => Self::Coordinator,
            "analyst" => Self::Analyst,
            "reviewer" => Self::Reviewer,
            "verifier" => Self::Verifier,
            "specialist" => Self::Specialist,
            "writer" => Self::Writer,
            other => anyhow::bail!("unknown team role {other}"),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct CooperativeScope {
    pub resources: Vec<String>,
    pub deliverables: Vec<String>,
    pub allow_write: bool,
}
impl Default for CooperativeScope {
    fn default() -> Self {
        Self {
            resources: vec![],
            deliverables: vec!["findings".into(), "evidence".into(), "report".into()],
            allow_write: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct TeamLimits {
    pub max_members: usize,
    pub max_concurrency: usize,
    pub max_tokens: u64,
    pub max_active_seconds: u64,
    pub max_activations: usize,
    pub max_activations_per_member: usize,
    pub max_debate_exchanges: usize,
    pub max_message_bytes: usize,
    pub max_pending_per_member: usize,
    pub max_pending_total: usize,
    pub max_cost_usd: Option<f64>,
}
impl Default for TeamLimits {
    fn default() -> Self {
        Self {
            max_members: 8,
            max_concurrency: 4,
            max_tokens: 1_500_000,
            max_active_seconds: 1_800,
            max_activations: 64,
            max_activations_per_member: 16,
            max_debate_exchanges: 3,
            max_message_bytes: 8 * 1024,
            max_pending_per_member: 128,
            max_pending_total: 1_024,
            max_cost_usd: None,
        }
    }
}
impl TeamLimits {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=8).contains(&self.max_members),
            "max_members must be 1..8"
        );
        ensure!(
            (1..=8).contains(&self.max_concurrency),
            "max_concurrency must be 1..8"
        );
        ensure!(
            self.max_concurrency <= self.max_members,
            "concurrency exceeds team size"
        );
        ensure!(self.max_tokens > 0, "max_tokens must be positive");
        ensure!(
            (1..=86_400).contains(&self.max_active_seconds),
            "max_active_seconds must be 1..86400"
        );
        ensure!(
            (1..=64).contains(&self.max_activations),
            "max_activations must be 1..64"
        );
        ensure!(
            (1..=64).contains(&self.max_activations_per_member),
            "member activation limit must be 1..64"
        );
        ensure!(
            self.max_debate_exchanges <= 3,
            "debate exchanges cannot exceed 3"
        );
        ensure!(
            (1..=8 * 1024).contains(&self.max_message_bytes),
            "message limit must be 1..8192 bytes"
        );
        ensure!(
            (1..=128).contains(&self.max_pending_per_member),
            "member mailbox limit must be 1..128"
        );
        ensure!(
            (1..=1_024).contains(&self.max_pending_total),
            "team mailbox limit must be 1..1024"
        );
        if let Some(cost) = self.max_cost_usd {
            ensure!(
                cost.is_finite() && cost > 0.0,
                "max_cost_usd must be positive"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct CooperativeOptions {
    pub template: TeamTemplate,
    pub scope: CooperativeScope,
    pub limits: TeamLimits,
    pub role_models: BTreeMap<TeamRole, String>,
}
impl CooperativeOptions {
    pub fn normalized(&self, token_ceiling: Option<u64>) -> Result<Self> {
        let mut result = self.clone();
        if let Some(ceiling) = token_ceiling {
            result.limits.max_tokens = result.limits.max_tokens.min(ceiling);
        }
        result.scope.resources.sort();
        result.scope.resources.dedup();
        result.scope.deliverables.sort();
        result.scope.deliverables.dedup();
        result.limits.validate()?;
        ensure!(
            !result.scope.allow_write,
            "cooperative writers are not supported; use read-only scope"
        );
        ensure!(
            !result.role_models.contains_key(&TeamRole::Writer),
            "cooperative writer roles are not supported"
        );
        ensure!(
            result.scope.resources.len() <= 256,
            "too many authorized resources"
        );
        ensure!(
            result.scope.deliverables.len() <= 64,
            "too many deliverables"
        );
        ensure!(
            result.role_models.len() <= 6,
            "too many role model overrides"
        );
        Ok(result)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TeamMemberState {
    Working,
    WaitingResponse,
    WaitingPermission,
    Blocked,
    Inactive,
    Paused,
    Terminated,
    Failed,
}
impl TeamMemberState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::WaitingResponse => "waiting_response",
            Self::WaitingPermission => "waiting_permission",
            Self::Blocked => "blocked",
            Self::Inactive => "inactive",
            Self::Paused => "paused",
            Self::Terminated => "terminated",
            Self::Failed => "failed",
        }
    }
}
impl std::str::FromStr for TeamMemberState {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        Ok(match value {
            "working" => Self::Working,
            "waiting_response" => Self::WaitingResponse,
            "waiting_permission" => Self::WaitingPermission,
            "blocked" => Self::Blocked,
            "inactive" => Self::Inactive,
            "paused" => Self::Paused,
            "terminated" => Self::Terminated,
            "failed" => Self::Failed,
            other => anyhow::bail!("unknown member state {other}"),
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessagePurpose {
    Question,
    Observation,
    Objection,
    VerificationRequest,
    Response,
    UserInstruction,
}
impl MessagePurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Question => "question",
            Self::Observation => "observation",
            Self::Objection => "objection",
            Self::VerificationRequest => "verification_request",
            Self::Response => "response",
            Self::UserInstruction => "user_instruction",
        }
    }
}
impl std::str::FromStr for MessagePurpose {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        Ok(match value {
            "question" => Self::Question,
            "observation" => Self::Observation,
            "objection" => Self::Objection,
            "verification_request" => Self::VerificationRequest,
            "response" => Self::Response,
            "user_instruction" => Self::UserInstruction,
            other => anyhow::bail!("unknown message purpose {other}"),
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FindingState {
    Hypothesis,
    Investigating,
    Reproduced,
    Reviewed,
    Discarded,
    Inconclusive,
}
impl FindingState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hypothesis => "hypothesis",
            Self::Investigating => "investigating",
            Self::Reproduced => "reproduced",
            Self::Reviewed => "reviewed",
            Self::Discarded => "discarded",
            Self::Inconclusive => "inconclusive",
        }
    }
}
impl std::str::FromStr for FindingState {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        Ok(match value {
            "hypothesis" => Self::Hypothesis,
            "investigating" => Self::Investigating,
            "reproduced" => Self::Reproduced,
            "reviewed" => Self::Reviewed,
            "discarded" => Self::Discarded,
            "inconclusive" => Self::Inconclusive,
            other => anyhow::bail!("unknown finding state {other}"),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TeamMessageInput {
    pub idempotency_key: String,
    pub expected_revision: i64,
    pub recipient: Option<String>,
    pub purpose: MessagePurpose,
    pub body: String,
    #[serde(default)]
    pub work_id: Option<String>,
    #[serde(default)]
    pub finding_id: Option<String>,
    #[serde(default)]
    pub evidence_id: Option<String>,
    #[serde(default)]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub provenance: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TeamMemberInput {
    pub idempotency_key: String,
    pub expected_revision: i64,
    pub name: String,
    pub role: TeamRole,
    pub justification: String,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TeamWorkInput {
    pub idempotency_key: String,
    pub expected_revision: i64,
    pub title: String,
    pub deliverable: String,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub priority: i64,
    pub close_conditions: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct FindingInput {
    pub idempotency_key: String,
    pub expected_revision: i64,
    pub claim: String,
    pub state: FindingState,
    pub reason: String,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    #[serde(default)]
    pub snapshot_sha256: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct FindingTransition {
    pub idempotency_key: String,
    pub expected_revision: i64,
    pub state: FindingState,
    pub reason: String,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    #[serde(default)]
    pub snapshot_sha256: Option<String>,
    #[serde(default)]
    pub environmental_failure: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TeamObjectionInput {
    pub idempotency_key: String,
    pub expected_revision: i64,
    pub finding_id: String,
    pub claim: String,
    pub check_required: String,
    #[serde(default)]
    pub new_evidence: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TeamCommand {
    pub idempotency_key: String,
    pub expected_revision: i64,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub recipient: Option<String>,
    #[serde(default)]
    pub priority: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TeamActivationContext {
    pub activation_id: String,
    pub team_id: String,
    pub task_id: String,
    pub member_id: String,
    pub member_name: String,
    pub role: TeamRole,
    pub generation: i64,
    pub model: Option<String>,
    pub template: TeamTemplate,
    pub template_version: u32,
    pub options: CooperativeOptions,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TeamSnapshot {
    pub id: String,
    pub task_id: String,
    pub state: String,
    pub reason: Option<String>,
    pub revision: i64,
    pub template: TeamTemplate,
    pub template_version: u32,
    pub options: CooperativeOptions,
    pub members: Vec<Value>,
    pub usage: Value,
    pub reservations: Value,
    pub open_questions: u64,
    /// Durable parent-task event cursor captured with this snapshot.
    #[serde(default)]
    pub last_event_seq: i64,
    /// True until active members have reached a safe pause boundary.
    #[serde(default)]
    pub pause_pending: bool,
    #[serde(default)]
    pub pending_approvals: Vec<Value>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TeamExport {
    pub team: TeamSnapshot,
    pub messages: Vec<Value>,
    #[serde(default)]
    pub deliveries: Vec<Value>,
    #[serde(default)]
    pub finding_revisions: Vec<Value>,
    #[serde(default)]
    pub events: Vec<Value>,
    pub work: Vec<Value>,
    pub findings: Vec<Value>,
    pub objections: Vec<Value>,
    pub evidence: Vec<Value>,
    pub markdown: String,
}

pub fn template_instructions(template: TeamTemplate) -> &'static str {
    match template {
        TeamTemplate::Audit => {
            "Audit template v1. Record an independent initial assessment before reading peers' conclusions. Start read-only. Separate claims from execution receipts, identify coverage and exclusions, and require another member for review."
        }
        TeamTemplate::Debug => {
            "Debugging template v1. Form falsifiable hypotheses, preserve negative controls and exact commands, distinguish environmental failures from reproduction, and re-run affected checks after integration."
        }
        TeamTemplate::Review => {
            "Change review template v1. Record an independent initial assessment before reading peers' conclusions. Review the authorized dirty snapshot, preserve historical findings when validity changes, and require fresh evidence on the integrated result."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_are_bounded_and_round_trip() {
        let options = CooperativeOptions::default()
            .normalized(None)
            .expect("defaults");
        assert_eq!(options.limits.max_members, 8);
        assert_eq!(options.limits.max_pending_total, 1024);
        assert_eq!(
            "cooperative".parse::<ExecutionMode>().ok(),
            Some(ExecutionMode::Cooperative)
        );
        assert_eq!(
            "review".parse::<TeamTemplate>().ok(),
            Some(TeamTemplate::Review)
        );
    }
}

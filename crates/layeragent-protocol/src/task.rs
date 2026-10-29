//! Durable task contract shared by every interface.
//!
//! A task identity is independent from the connection that submitted it. State
//! changes are persisted before they become visible and survive a daemon restart.
use crate::{EffortLevel, RunMode};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Lifecycle exposed to clients. `blocked` covers recovery and reconciliation
/// states that must not be resumed automatically.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Prepared,
    Running,
    AwaitingPermission,
    Blocked,
    Completed,
    Failed,
    Cancelled,
}
impl TaskState {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Running => "running",
            Self::AwaitingPermission => "awaiting_permission",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}
impl std::str::FromStr for TaskState {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "prepared" => Self::Prepared,
            "running" => Self::Running,
            "awaiting_permission" => Self::AwaitingPermission,
            "blocked" => Self::Blocked,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            other => anyhow::bail!("unknown task state {other}"),
        })
    }
}

/// Cancellation is tracked apart from the task state because a host restart can
/// leave the external effect of an active tool unknown.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CancellationState {
    #[default]
    None,
    Requested,
    Confirmed,
    Uncertain,
}
impl CancellationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Requested => "requested",
            Self::Confirmed => "confirmed",
            Self::Uncertain => "uncertain",
        }
    }
}
impl std::str::FromStr for CancellationState {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "none" => Self::None,
            "requested" => Self::Requested,
            "confirmed" => Self::Confirmed,
            "uncertain" => Self::Uncertain,
            other => anyhow::bail!("unknown cancellation state {other}"),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Task {
    pub id: String,
    pub tenant: String,
    pub principal: String,
    /// Canonical project workspace. Never a display label.
    pub project: String,
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    pub objective: String,
    pub executor: String,
    pub state: TaskState,
    /// Monotonic metadata revision. Clients must quote it in corrections and
    /// approval decisions; a stale value receives a conflict.
    pub revision: i64,
    #[serde(default)]
    pub usage: Value,
    pub cancellation: CancellationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correction: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub last_event_seq: i64,
    #[serde(default)]
    pub effort: EffortLevel,
    #[serde(default)]
    pub mode: RunMode,
    /// Deprecated compatibility field. `true` is never treated as cooperative execution.
    #[serde(default)]
    pub bridge: bool,
    #[serde(default)]
    pub execution: crate::ExecutionMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewTask {
    /// Canonical project directory.
    pub workspace: String,
    #[serde(default)]
    pub session_id: Option<String>,
    pub objective: String,
    #[serde(default)]
    pub model: Option<String>,
    /// Defaults to `local`.
    #[serde(default)]
    pub executor: Option<String>,
    #[serde(default)]
    pub effort: EffortLevel,
    #[serde(default)]
    pub mode: RunMode,
    /// Deprecated compatibility field. Use `execution` and `cooperative` instead.
    #[serde(default)]
    pub bridge: bool,
    #[serde(default)]
    pub execution: crate::ExecutionMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooperative: Option<crate::CooperativeOptions>,
}
impl NewTask {
    /// Content whose hash makes a repeated request idempotent. The key itself is
    /// never persisted in the task body or exposed by the API.
    pub fn fingerprint(&self) -> anyhow::Result<String> {
        use sha2::{Digest, Sha256};
        let cooperative = self
            .cooperative
            .as_ref()
            .map(|options| options.normalized(None))
            .transpose()?;
        let body = serde_json::to_vec(&(
            &self.workspace,
            &self.session_id,
            &self.objective,
            &self.model,
            &self.executor,
            self.effort,
            self.mode,
            self.bridge,
            self.execution,
            cooperative,
        ))?;
        Ok(hex::encode(Sha256::digest(body)))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TaskCorrection {
    pub id: String,
    pub task_id: String,
    pub instruction: String,
    pub expected_revision: i64,
    pub status: String,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TaskPage {
    pub tasks: Vec<Task>,
    /// Pass as `after` on the next request.
    pub next: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TaskEventPage {
    pub events: Vec<Value>,
    pub next: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalDecision {
    /// Metadata revision observed in `approval_required`.
    pub expected_revision: i64,
    /// SHA-256 of the canonical tool arguments shown to the reviewer.
    pub arguments_hash: String,
    pub allow: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CorrectTask {
    pub instruction: String,
    pub expected_revision: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_is_not_a_task_state() {
        for state in [
            TaskState::Prepared,
            TaskState::Running,
            TaskState::AwaitingPermission,
            TaskState::Blocked,
            TaskState::Completed,
            TaskState::Failed,
            TaskState::Cancelled,
        ] {
            assert_eq!(state.as_str().parse::<TaskState>().ok(), Some(state));
        }
        for value in ["requested", "confirmed", "uncertain", "none"] {
            assert_eq!(
                value.parse::<CancellationState>().ok().map(|s| s.as_str()),
                Some(value)
            );
        }
    }
    #[test]
    fn fingerprint_binds_all_semantic_fields() {
        let base = NewTask {
            workspace: "/tmp/p".into(),
            session_id: None,
            objective: "ship".into(),
            model: None,
            executor: None,
            effort: EffortLevel::Auto,
            mode: RunMode::Normal,
            bridge: false,
            execution: crate::ExecutionMode::Delegated,
            cooperative: None,
        };
        let mut other = base.clone();
        other.objective = "ship it".into();
        assert_ne!(
            base.fingerprint().expect("fingerprint"),
            other.fingerprint().expect("fingerprint")
        );
    }
}

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

pub mod redact;
pub mod task;
pub use task::*;

pub const PROTOCOL_VERSION: u32 = 1;
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub fn validate_name(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.is_empty()
            && value.len() <= 96
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "invalid identifier"
    );
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct Scope {
    pub tenant: String,
    pub principal: String,
}
impl Scope {
    pub fn local() -> Self {
        Self {
            tenant: "default".into(),
            principal: "owner".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub provider_data: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Message {
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub provider_data: Value,
}
impl Message {
    pub fn text(role: &str, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            tool_calls: vec![],
            tool_call_id: None,
            provider_data: Value::Null,
        }
    }
    pub fn tool(call: &ToolCall, result: &Value) -> Self {
        Self {
            role: "tool".into(),
            content: result.to_string(),
            tool_calls: vec![],
            tool_call_id: Some(call.id.clone()),
            provider_data: Value::Null,
        }
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Completion {
    pub text: String,
    pub calls: Vec<ToolCall>,
    pub usage: Usage,
    pub model: String,
    pub provider_data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnState {
    Preflight,
    Context,
    Inference,
    Tools,
    AwaitingApproval,
    Finalizing,
    Completed,
    Failed,
    Interrupted,
}
impl TurnState {
    pub fn terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Interrupted)
    }
    pub fn permits(&self, next: &Self) -> bool {
        if self.terminal() {
            return false;
        }
        if matches!(next, Self::Failed | Self::Interrupted) {
            return true;
        }
        matches!(
            (self, next),
            (Self::Preflight, Self::Context)
                | (Self::Context, Self::Inference)
                | (Self::Inference, Self::Tools)
                | (Self::Inference, Self::Finalizing)
                | (Self::Tools, Self::AwaitingApproval)
                | (Self::AwaitingApproval, Self::Tools)
                | (Self::Tools, Self::Context)
                | (Self::Finalizing, Self::Completed)
        )
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Event {
    pub seq: i64,
    pub at: i64,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub kind: String,
    pub data: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Session {
    pub id: String,
    pub principal: String,
    pub workspace: String,
    pub created_at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunRequest {
    pub session_id: Option<String>,
    pub workspace: Option<String>,
    pub prompt: String,
    #[serde(default)]
    pub model: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunResult {
    pub session_id: String,
    pub turn_id: String,
    pub text: String,
    pub usage: Usage,
    #[serde(default)]
    pub cost_usd: f64,
    /// Milliseconds spent waiting for the provider across all attempts.
    #[serde(default)]
    pub provider_ms: u64,
    /// Milliseconds spent inside tool execution.
    #[serde(default)]
    pub tool_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlatformEvent {
    pub external_id: String,
    pub platform: String,
    pub channel_id: String,
    pub sender_id: String,
    pub is_bot: bool,
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    pub sha256: String,
    pub bytes: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Evidence {
    pub id: String,
    pub at: i64,
    pub category: String,
    pub source: String,
    pub source_sha256: String,
    pub offset: Option<u64>,
    pub description: String,
    pub previous_hash: String,
    pub hash: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_states_cannot_restart() {
        for state in [
            TurnState::Completed,
            TurnState::Failed,
            TurnState::Interrupted,
        ] {
            assert!(!state.permits(&TurnState::Inference));
        }
    }
    #[test]
    fn path_like_identifiers_rejected() {
        for s in ["../x", "", "/tmp", "a/b", "a\\b", "a.b"] {
            assert!(validate_name(s).is_err());
        }
    }
}

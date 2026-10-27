//! Error shape returned to the webview.
//!
//! Tauri serializes the error of a failed command as it is, so the frontend
//! always receives a structured object instead of a bare string.

use serde::Serialize;

use crate::daemon::DaemonError;

/// Structured failure of a desktop command.
#[derive(Debug, Clone, Serialize)]
pub struct CommandError {
    /// Human readable description of the failure.
    pub error: String,
    /// `true` when the daemon does not implement the requested route.
    pub unavailable: bool,
}

impl CommandError {
    /// A failure caused by the request itself.
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            error: message.into(),
            unavailable: false,
        }
    }

    /// A failure caused by a route the daemon does not implement yet.
    pub fn missing(message: impl Into<String>) -> Self {
        Self {
            error: message.into(),
            unavailable: true,
        }
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.error)
    }
}

impl std::error::Error for CommandError {}

impl From<DaemonError> for CommandError {
    fn from(error: DaemonError) -> Self {
        Self {
            error: error.to_string(),
            unavailable: error.is_unavailable(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_routes_are_marked_as_unavailable() {
        let error = CommandError::from(DaemonError::Unavailable("/v1/tasks".to_string()));
        assert!(error.unavailable);
        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(value["unavailable"], serde_json::Value::Bool(true));
        assert!(value["error"].as_str().unwrap().contains("/v1/tasks"));
    }

    #[test]
    fn other_failures_are_not_marked_as_unavailable() {
        let error = CommandError::from(DaemonError::Unreachable("no socket".to_string()));
        assert!(!error.unavailable);
    }
}

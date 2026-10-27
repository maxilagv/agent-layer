//! Bounded HTTP client for the LayerAgent daemon.
//!
//! The daemon serves its API over HTTP on a UNIX socket that lives inside the
//! LayerAgent home directory (`<home>/runtime/<profile>.sock`). Filesystem
//! permissions on that socket are the only credential, therefore this client
//! never falls back to TCP and never carries a token.
//!
//! Every request issued here is bounded:
//!
//! * the request path is either a constant or built from a value that matches
//!   a strict identifier pattern, so unvalidated input never reaches a URL,
//! * each request has a connect timeout and a total timeout,
//! * the response body is read with a hard size limit.

use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::{Client, Method, Response, StatusCode};
use serde_json::{json, Value};
use thiserror::Error;

/// Channel used to push daemon events to the webview.
pub const EVENT_CHANNEL: &str = "layeragent://event";
/// The channel the shared frontend listens on.
pub const TASK_EVENT_CHANNEL: &str = "layeragent://task-event";
/// Channel used to push daemon status changes to the webview.
pub const STATUS_CHANNEL: &str = "layeragent://status";

/// Hard limit for a decoded daemon response.
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
/// Total time budget for a single request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Time budget for establishing the connection to the socket.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest accepted identifier.
const MAX_ID_LEN: usize = 64;
/// Longest accepted free text field.
const MAX_TEXT_LEN: usize = 64 * 1024;
/// Longest accepted filesystem path.
const MAX_PATH_LEN: usize = 4096;
/// Longest accepted request path.
const MAX_REQUEST_PATH_LEN: usize = 512;
/// Default number of tasks requested per page.
const DEFAULT_TASK_LIMIT: u32 = 50;
/// Largest accepted page size.
const MAX_TASK_LIMIT: u32 = 500;

/// Actions accepted by `POST /v1/connections/{id}/{action}`.
pub const CONNECTION_ACTIONS: [&str; 4] = ["probe", "enable", "disable", "revoke"];

/// Failure of a daemon call.
///
/// `Unavailable` marks a route that the running daemon does not implement
/// yet. The bridge reports it to the frontend as a structured error instead of
/// inventing a successful answer.
#[derive(Debug, Error)]
pub enum DaemonError {
    /// The daemon socket could not be reached.
    #[error("daemon is unreachable: {0}")]
    Unreachable(String),
    /// The daemon answered with an error status.
    #[error("daemon rejected the request: {0}")]
    Request(String),
    /// The daemon does not implement the requested route.
    #[error("daemon route is not available: {0}")]
    Unavailable(String),
    /// The request was rejected before it was sent.
    #[error("invalid request: {0}")]
    Invalid(String),
}

impl DaemonError {
    /// Whether the failure is caused by a missing route instead of a real error.
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

/// Location of the daemon socket.
#[derive(Clone, Debug)]
pub struct Endpoint {
    /// LayerAgent home directory.
    pub home: PathBuf,
    /// Selected profile.
    pub profile: String,
    /// UNIX socket the daemon listens on.
    pub socket: PathBuf,
    /// Base URL used on top of the UNIX socket.
    pub base: String,
}

impl Endpoint {
    /// Resolve the endpoint from the environment.
    ///
    /// The home directory is `$LAYERAGENT_HOME` when set, otherwise the platform
    /// configuration directory. The profile defaults to `default`.
    pub fn resolve() -> Result<Self, DaemonError> {
        Self::with_home_and_profile(default_home(), "default")
    }

    /// Resolve the endpoint for an explicit home directory and profile.
    pub fn with_home_and_profile(home: PathBuf, profile: &str) -> Result<Self, DaemonError> {
        validate_profile(profile)?;
        Ok(Self {
            socket: home.join("runtime").join(format!("{profile}.sock")),
            base: "http://localhost".to_string(),
            home,
            profile: profile.to_string(),
        })
    }
}

/// LayerAgent home directory, honouring `LAYERAGENT_HOME` first.
fn default_home() -> PathBuf {
    if let Some(home) = std::env::var_os("LAYERAGENT_HOME") {
        return PathBuf::from(home);
    }
    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(config).join("layeragent");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".config").join("layeragent");
    }
    PathBuf::from(".layeragent")
}

/// A profile name is a short opaque token.
pub fn validate_profile(profile: &str) -> Result<(), DaemonError> {
    if profile.is_empty() || profile.len() > MAX_ID_LEN {
        return Err(DaemonError::Invalid("profile name length".to_string()));
    }
    if !profile
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(DaemonError::Invalid(
            "profile name must match [A-Za-z0-9_-]{1,64}".to_string(),
        ));
    }
    Ok(())
}

/// An object identifier is a short opaque token.
pub fn validate_id(id: &str) -> Result<(), DaemonError> {
    if id.is_empty() || id.len() > MAX_ID_LEN {
        return Err(DaemonError::Invalid("identifier length".to_string()));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(DaemonError::Invalid(
            "identifier must match [A-Za-z0-9_-]{1,64}".to_string(),
        ));
    }
    Ok(())
}

/// An artifact hash is a lowercase SHA-256 digest.
pub fn validate_hash(hash: &str) -> Result<(), DaemonError> {
    if hash.len() != 64 {
        return Err(DaemonError::Invalid(
            "artifact hash must be 64 hexadecimal characters".to_string(),
        ));
    }
    if !hash
        .chars()
        .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    {
        return Err(DaemonError::Invalid(
            "artifact hash must match [a-f0-9]{64}".to_string(),
        ));
    }
    Ok(())
}

/// A free text field is bounded in length.
pub fn validate_text(field: &str, value: &str) -> Result<(), DaemonError> {
    if value.len() > MAX_TEXT_LEN {
        return Err(DaemonError::Invalid(format!(
            "{field} is longer than {MAX_TEXT_LEN} bytes"
        )));
    }
    Ok(())
}

/// A workspace path is bounded in length and has to be absolute.
pub fn validate_workspace(workspace: &str) -> Result<(), DaemonError> {
    if workspace.is_empty() || workspace.len() > MAX_PATH_LEN {
        return Err(DaemonError::Invalid("workspace path length".to_string()));
    }
    if !Path::new(workspace).is_absolute() {
        return Err(DaemonError::Invalid(
            "workspace path must be absolute".to_string(),
        ));
    }
    if workspace.contains('\0') {
        return Err(DaemonError::Invalid(
            "workspace path contains a null byte".to_string(),
        ));
    }
    Ok(())
}

/// A request path is a constant or a validated identifier chain.
fn validate_request_path(path: &str) -> Result<(), DaemonError> {
    if path.is_empty() || path.len() > MAX_REQUEST_PATH_LEN {
        return Err(DaemonError::Invalid("request path length".to_string()));
    }
    if !path.starts_with('/') {
        return Err(DaemonError::Invalid(
            "request path must start with a slash".to_string(),
        ));
    }
    if !path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.'))
    {
        return Err(DaemonError::Invalid(
            "request path contains unsupported characters".to_string(),
        ));
    }
    Ok(())
}

/// Bounded client of the daemon API.
#[derive(Clone, Debug)]
pub struct Daemon {
    endpoint: Endpoint,
    http: Client,
}

impl Daemon {
    /// Build a client for a resolved endpoint.
    ///
    /// The socket is not contacted here: an application that starts before the
    /// daemon still runs and reports the daemon as unreachable.
    pub fn connect(endpoint: Endpoint) -> Result<Self, DaemonError> {
        let http = Client::builder()
            .unix_socket(endpoint.socket.clone())
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|error| DaemonError::Unreachable(error.to_string()))?;
        Ok(Self { endpoint, http })
    }

    /// The endpoint this client talks to.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// `GET /health`.
    pub async fn health(&self) -> Result<Value, DaemonError> {
        self.get("/health").await
    }

    /// `GET <path>`.
    pub async fn get(&self, path: &str) -> Result<Value, DaemonError> {
        self.request(Method::GET, path, &[], None).await
    }

    /// `GET <path>` with bounded query parameters.
    pub async fn get_with_query(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Value, DaemonError> {
        self.request(Method::GET, path, query, None).await
    }

    /// `POST <path>` with a JSON body.
    pub async fn post(&self, path: &str, body: &Value) -> Result<Value, DaemonError> {
        self.request(Method::POST, path, &[], Some(body)).await
    }

    /// `POST <path>` without a body.
    pub async fn post_empty(&self, path: &str) -> Result<Value, DaemonError> {
        self.request(Method::POST, path, &[], Some(&json!({})))
            .await
    }

    /// `POST <path>` with a JSON body and extra headers, answering with the raw
    /// body text. Used for the Model Context Protocol transport, which answers
    /// with a server sent event stream.
    pub async fn post_text(
        &self,
        path: &str,
        body: &Value,
        headers: &[(&str, &str)],
    ) -> Result<String, DaemonError> {
        let (status, bytes) = self
            .send(Method::POST, path, &[], Some(body), headers)
            .await?;
        if status == StatusCode::NOT_FOUND {
            return Err(DaemonError::Unavailable(path.to_string()));
        }
        if !status.is_success() {
            return Err(DaemonError::Request(format!("{status}: request failed")));
        }
        Ok(String::from_utf8_lossy(&bytes).to_string())
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Value, DaemonError> {
        let (status, bytes) = self.send(method, path, query, body, &[]).await?;
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if status == StatusCode::NOT_FOUND {
            return Err(DaemonError::Unavailable(path.to_string()));
        }
        if !status.is_success() {
            let message = value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("request failed")
                .to_string();
            return Err(DaemonError::Request(format!("{status}: {message}")));
        }
        Ok(value)
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
        headers: &[(&str, &str)],
    ) -> Result<(StatusCode, Vec<u8>), DaemonError> {
        validate_request_path(path)?;
        for (key, value) in query {
            if key.is_empty() || key.len() > MAX_ID_LEN || value.len() > MAX_TEXT_LEN {
                return Err(DaemonError::Invalid("query parameter length".to_string()));
            }
        }
        let url = format!("{}{}", self.endpoint.base, path);
        let mut request = self.http.request(method, url);
        if !query.is_empty() {
            request = request.query(query);
        }
        for (key, value) in headers {
            if key.is_empty() || key.len() > MAX_ID_LEN || value.len() > MAX_TEXT_LEN {
                return Err(DaemonError::Invalid("header length".to_string()));
            }
            request = request.header(*key, *value);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|error| DaemonError::Unreachable(error.to_string()))?;
        let status = response.status();
        let bytes = read_bounded(response).await?;
        Ok((status, bytes))
    }
}

/// Read a response body, refusing anything above the size limit.
async fn read_bounded(mut response: Response) -> Result<Vec<u8>, DaemonError> {
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| DaemonError::Request(error.to_string()))?
    {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(DaemonError::Request(
                "daemon response exceeded the size limit".to_string(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// `GET /v1/sessions/{id}/events`.
pub fn session_events_path(id: &str) -> Result<String, DaemonError> {
    validate_id(id)?;
    Ok(format!("/v1/sessions/{id}/events"))
}

/// `GET /v1/tasks/{id}`.
pub fn task_path(id: &str) -> Result<String, DaemonError> {
    validate_id(id)?;
    Ok(format!("/v1/tasks/{id}"))
}

/// `GET /v1/tasks/{id}/events`.
pub fn task_events_path(id: &str) -> Result<String, DaemonError> {
    validate_id(id)?;
    Ok(format!("/v1/tasks/{id}/events"))
}

/// `POST /v1/tasks/{id}/cancel`.
pub fn task_cancel_path(id: &str) -> Result<String, DaemonError> {
    validate_id(id)?;
    Ok(format!("/v1/tasks/{id}/cancel"))
}

/// `POST /v1/tasks/{id}/correct`.
pub fn task_correct_path(id: &str) -> Result<String, DaemonError> {
    validate_id(id)?;
    Ok(format!("/v1/tasks/{id}/correct"))
}

/// `POST /v1/jobs/{id}/cancel`.
pub fn job_cancel_path(id: &str) -> Result<String, DaemonError> {
    validate_id(id)?;
    Ok(format!("/v1/jobs/{id}/cancel"))
}

/// `POST /v1/approvals/{id}`.
pub fn approval_path(id: &str) -> Result<String, DaemonError> {
    validate_id(id)?;
    Ok(format!("/v1/approvals/{id}"))
}

/// `GET /v1/artifacts/{hash}`.
pub fn artifact_path(hash: &str) -> Result<String, DaemonError> {
    validate_hash(hash)?;
    Ok(format!("/v1/artifacts/{hash}"))
}

/// `POST /v1/connections/{id}/{action}`.
pub fn connection_action_path(id: &str, action: &str) -> Result<String, DaemonError> {
    validate_id(id)?;
    if !CONNECTION_ACTIONS.contains(&action) {
        return Err(DaemonError::Invalid(
            "connection action must be one of probe, enable, disable, revoke".to_string(),
        ));
    }
    Ok(format!("/v1/connections/{id}/{action}"))
}

/// A stream the desktop shell can subscribe to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamTarget {
    /// Events of one session.
    Session(String),
    /// Events of one task.
    Task(String),
}

impl StreamTarget {
    /// Identifier of the streamed object.
    pub fn id(&self) -> &str {
        match self {
            Self::Session(id) | Self::Task(id) => id,
        }
    }

    /// Request path that serves the events of this target.
    pub fn path(&self) -> Result<String, DaemonError> {
        match self {
            Self::Session(id) => session_events_path(id),
            Self::Task(id) => task_events_path(id),
        }
    }

    /// Kind of the streamed object.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Session(_) => "session",
            Self::Task(_) => "task",
        }
    }
}

/// Parse the path accepted by the `subscribe` command.
///
/// Only the two event streams of the daemon are accepted; the path never
/// reaches the daemon unchecked.
pub fn parse_stream_path(path: &str) -> Result<StreamTarget, DaemonError> {
    let path = path
        .split('?')
        .next()
        .unwrap_or_default()
        .trim_end_matches('/');
    if let Some(rest) = path.strip_prefix("/v1/sessions/") {
        if let Some(id) = rest.strip_suffix("/events") {
            validate_id(id)?;
            return Ok(StreamTarget::Session(id.to_string()));
        }
    }
    if let Some(rest) = path.strip_prefix("/v1/tasks/") {
        if let Some(id) = rest.strip_suffix("/events") {
            validate_id(id)?;
            return Ok(StreamTarget::Task(id.to_string()));
        }
    }
    Err(DaemonError::Invalid(
        "stream path must be /v1/sessions/{id}/events or /v1/tasks/{id}/events".to_string(),
    ))
}

/// Extract the task list from either a bare array or a paginated page.
pub fn task_list(value: &Value) -> Vec<Value> {
    if let Some(array) = value.as_array() {
        return array.clone();
    }
    if let Some(array) = value.get("tasks").and_then(Value::as_array) {
        return array.clone();
    }
    Vec::new()
}

/// Read a string field of a task object.
pub fn task_field(task: &Value, key: &str) -> Option<String> {
    task.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty())
}

/// Whether a task state is terminal.
pub fn is_terminal_state(state: &str) -> bool {
    matches!(state, "completed" | "failed" | "cancelled")
}

/// Number of tasks that are not in a terminal state.
pub fn count_active_tasks(value: &Value) -> usize {
    task_list(value)
        .iter()
        .filter(|task| {
            task_field(task, "state")
                .map(|state| !is_terminal_state(&state))
                .unwrap_or(true)
        })
        .count()
}

/// Tasks whose cancellation effect is unknown.
pub fn uncertain_tasks(value: &Value) -> Vec<Value> {
    task_list(value)
        .into_iter()
        .filter(|task| task_field(task, "cancellation").as_deref() == Some("uncertain"))
        .collect()
}

/// Tasks whose cancellation was requested but not confirmed yet.
pub fn requested_cancellations(value: &Value) -> Vec<Value> {
    task_list(value)
        .into_iter()
        .filter(|task| task_field(task, "cancellation").as_deref() == Some("requested"))
        .collect()
}

/// Reduce a task to the fields the shell has to show.
pub fn task_summary(task: &Value) -> Value {
    json!({
      "id": task_field(task, "id").unwrap_or_default(),
      "objective": task_field(task, "objective").unwrap_or_default(),
      "state": task_field(task, "state").unwrap_or_default(),
      "cancellation": task_field(task, "cancellation").unwrap_or_default(),
      "project": task_field(task, "project").unwrap_or_default(),
      "executor": task_field(task, "executor").unwrap_or_default(),
      "revision": task.get("revision").cloned().unwrap_or(Value::Null),
      "error": task.get("error").cloned().unwrap_or(Value::Null),
      "usage": task.get("usage").cloned().unwrap_or(Value::Null),
    })
}

/// Normalise a page size to a bounded value.
pub fn bounded_limit(limit: Option<u32>) -> u32 {
    limit.unwrap_or(DEFAULT_TASK_LIMIT).clamp(1, MAX_TASK_LIMIT)
}

/// Extract the events of a stream answer that is either a bare array or a page.
pub fn event_list(value: &Value) -> Vec<Value> {
    if let Some(array) = value.as_array() {
        return array.clone();
    }
    if let Some(array) = value.get("events").and_then(Value::as_array) {
        return array.clone();
    }
    Vec::new()
}

/// Sequence number carried by an event, when present.
pub fn event_seq(event: &Value) -> Option<i64> {
    event
        .get("seq")
        .or_else(|| event.get("sequence"))
        .or_else(|| event.get("id"))
        .and_then(Value::as_i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_restricted() {
        assert!(validate_id("abc-DEF_123").is_ok());
        assert!(validate_id("").is_err());
        assert!(validate_id("../etc/passwd").is_err());
        assert!(validate_id("a b").is_err());
        assert!(validate_id(&"a".repeat(65)).is_err());
    }

    #[test]
    fn hashes_are_restricted() {
        assert!(validate_hash(&"a".repeat(64)).is_ok());
        assert!(validate_hash(&"A".repeat(64)).is_err());
        assert!(validate_hash(&"a".repeat(63)).is_err());
        assert!(validate_hash(&"g".repeat(64)).is_err());
    }

    #[test]
    fn only_event_streams_can_be_subscribed() {
        assert_eq!(
            parse_stream_path("/v1/sessions/abc/events").unwrap(),
            StreamTarget::Session("abc".to_string())
        );
        assert_eq!(
            parse_stream_path("/v1/tasks/abc/events?after=3").unwrap(),
            StreamTarget::Task("abc".to_string())
        );
        assert!(parse_stream_path("/v1/config").is_err());
        assert!(parse_stream_path("/v1/sessions/../events").is_err());
    }

    #[test]
    fn active_tasks_exclude_terminal_states() {
        let tasks = json!([
          {"id": "a", "state": "running"},
          {"id": "b", "state": "completed"},
          {"id": "c", "state": "cancelled"},
          {"id": "d", "state": "blocked"}
        ]);
        assert_eq!(count_active_tasks(&tasks), 2);
        assert_eq!(count_active_tasks(&json!({"tasks": tasks})), 2);
    }

    #[test]
    fn uncertain_tasks_are_reported() {
        let tasks = json!([
          {"id": "a", "cancellation": "uncertain"},
          {"id": "b", "cancellation": "none"},
          {"id": "c", "cancellation": "requested"}
        ]);
        assert_eq!(uncertain_tasks(&tasks).len(), 1);
        assert_eq!(requested_cancellations(&tasks).len(), 1);
    }

    #[test]
    fn connection_actions_are_allowlisted() {
        assert!(connection_action_path("abc", "probe").is_ok());
        assert!(connection_action_path("abc", "destroy").is_err());
        assert!(connection_action_path("../x", "probe").is_err());
    }

    #[test]
    fn workspaces_must_be_absolute() {
        assert!(validate_workspace("/srv/project").is_ok());
        assert!(validate_workspace("relative/path").is_err());
        assert!(validate_workspace("").is_err());
    }
}

//! Bounded commands exposed to the webview.
//!
//! Every command is a thin, honest bridge to the daemon: the shell never
//! invents data. A route that the running daemon does not implement answers
//! with a structured error that carries `unavailable: true`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::sync::watch;

use crate::daemon::{self, Daemon, DaemonError};
use crate::error::CommandError;
use crate::state::{AppState, Subscription};

/// Poll interval of an event subscription.
const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Consecutive failures after which a subscription gives up.
const MAX_FAILURES: u32 = 5;

static SUBSCRIPTION_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Identifier of a new subscription.
fn next_subscription_id() -> String {
    let counter = SUBSCRIPTION_COUNTER.fetch_add(1, Ordering::Relaxed);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    format!("sub-{millis}-{counter}")
}

/// Client of the daemon kept in the managed state.
fn daemon_of<R: Runtime>(app: &AppHandle<R>) -> Daemon {
    app.state::<AppState>().daemon.clone()
}

/// Read one part of a composite answer, recording the parts that are not
/// available instead of failing the whole call.
async fn read_part(
    daemon: &Daemon,
    path: &str,
    part: &str,
    output: &mut Value,
    unavailable: &mut Vec<String>,
    errors: &mut Value,
) {
    match daemon.get(path).await {
        Ok(value) => output[part] = value,
        Err(error) => {
            unavailable.push(part.to_string());
            output[part] = Value::Null;
            if !error.is_unavailable() {
                errors[part] = json!(error.to_string());
            }
        }
    }
}

/// Everything the shell knows about the local installation.
pub async fn bootstrap<R: Runtime>(app: &AppHandle<R>) -> Result<Value, CommandError> {
    let daemon = daemon_of(app);
    let endpoint = daemon.endpoint().clone();
    let mut output = json!({
      "app": {
        "name": "LayerAgent",
        "version": env!("CARGO_PKG_VERSION"),
      },
      "home": endpoint.home.to_string_lossy(),
      "profile": endpoint.profile,
      "socket": endpoint.socket.to_string_lossy(),
      "daemon": { "reachable": false, "health": Value::Null },
      "config": Value::Null,
      "models": Value::Null,
      "sessions": Value::Null,
      "tasks": Value::Null,
      "unavailable": [],
      "errors": {},
    });
    let mut unavailable: Vec<String> = Vec::new();
    let mut errors = json!({});

    let (reachable, health) = match daemon.health().await {
        Ok(value) => (true, value),
        Err(error) => {
            errors["health"] = json!(error.to_string());
            (false, Value::Null)
        }
    };
    output["daemon"] = json!({ "reachable": reachable, "health": health });

    read_part(
        &daemon,
        "/v1/config",
        "config",
        &mut output,
        &mut unavailable,
        &mut errors,
    )
    .await;
    read_part(
        &daemon,
        "/v1/models",
        "models",
        &mut output,
        &mut unavailable,
        &mut errors,
    )
    .await;
    read_part(
        &daemon,
        "/v1/sessions",
        "sessions",
        &mut output,
        &mut unavailable,
        &mut errors,
    )
    .await;
    read_part(
        &daemon,
        "/v1/tasks",
        "tasks",
        &mut output,
        &mut unavailable,
        &mut errors,
    )
    .await;

    output["unavailable"] = json!(unavailable);
    output["errors"] = errors;
    Ok(output)
}

/// Projects known to the daemon, derived from sessions and tasks.
pub async fn list_projects<R: Runtime>(app: &AppHandle<R>) -> Result<Value, CommandError> {
    let daemon = daemon_of(app);
    let mut projects: BTreeMap<String, Value> = BTreeMap::new();

    if let Ok(sessions) = daemon.get("/v1/sessions").await {
        for session in sessions.as_array().into_iter().flatten() {
            if let Some(workspace) = session.get("workspace").and_then(Value::as_str) {
                projects.entry(workspace.to_string()).or_insert_with(|| {
                    json!({
                      "path": workspace,
                      "name": project_name(workspace),
                    })
                });
            }
        }
    }

    if let Ok(tasks) = daemon.get("/v1/tasks").await {
        for task in daemon::task_list(&tasks) {
            if let Some(project) = daemon::task_field(&task, "project") {
                projects.entry(project.clone()).or_insert_with(|| {
                    json!({
                      "path": project,
                      "name": project_name(&project),
                    })
                });
            }
        }
    }

    Ok(json!({
      "projects": projects.into_values().collect::<Vec<Value>>(),
    }))
}

/// Last path segment of a project path.
fn project_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

/// Sessions known to the daemon.
pub async fn list_sessions<R: Runtime>(app: &AppHandle<R>) -> Result<Value, CommandError> {
    let daemon = daemon_of(app);
    let sessions = daemon.get("/v1/sessions").await?;
    Ok(json!({ "sessions": sessions }))
}

/// Events of one session.
pub async fn session_events<R: Runtime>(
    app: &AppHandle<R>,
    id: &str,
    after: Option<i64>,
) -> Result<Value, CommandError> {
    let daemon = daemon_of(app);
    let path = daemon::session_events_path(id)?;
    let query = after_query(after);
    let events = daemon.get_with_query(&path, &query).await?;
    Ok(json!({ "id": id, "events": daemon::event_list(&events) }))
}

/// Query for the `after` cursor.
fn after_query(after: Option<i64>) -> Vec<(&'static str, String)> {
    let cursor = after.unwrap_or_default().max(0);
    vec![("after", cursor.to_string())]
}

/// Tasks known to the daemon.
pub async fn list_tasks<R: Runtime>(
    app: &AppHandle<R>,
    after: Option<i64>,
    limit: Option<u32>,
) -> Result<Value, CommandError> {
    let daemon = daemon_of(app);
    let query = vec![
        ("after", after.unwrap_or_default().max(0).to_string()),
        ("limit", daemon::bounded_limit(limit).to_string()),
    ];
    let tasks = daemon.get_with_query("/v1/tasks", &query).await?;
    Ok(json!({
      "tasks": daemon::task_list(&tasks),
      "next": tasks.get("next").cloned().unwrap_or(Value::Null),
      "active_tasks": daemon::count_active_tasks(&tasks),
    }))
}

/// Submit a task to the daemon.
pub async fn create_task<R: Runtime>(
    app: &AppHandle<R>,
    workspace: &str,
    objective: &str,
    session: Option<String>,
    model: Option<String>,
    executor: Option<String>,
) -> Result<Value, CommandError> {
    let daemon = daemon_of(app);
    daemon::validate_workspace(workspace)?;
    daemon::validate_text("objective", objective)?;
    if objective.trim().is_empty() {
        return Err(CommandError::invalid("objective must not be empty"));
    }
    if let Some(session) = &session {
        daemon::validate_id(session)?;
    }
    if let Some(model) = &model {
        daemon::validate_text("model", model)?;
    }
    if let Some(executor) = &executor {
        daemon::validate_text("executor", executor)?;
    }
    let mut body = json!({ "workspace": workspace, "objective": objective });
    if let Some(session) = session {
        body["session_id"] = json!(session);
    }
    if let Some(model) = model {
        body["model"] = json!(model);
    }
    if let Some(executor) = executor {
        body["executor"] = json!(executor);
    }
    let task = daemon.post("/v1/tasks", &body).await?;
    Ok(task)
}

/// One task.
pub async fn task<R: Runtime>(app: &AppHandle<R>, id: &str) -> Result<Value, CommandError> {
    let daemon = daemon_of(app);
    let path = daemon::task_path(id)?;
    let task = daemon.get(&path).await?;
    Ok(task)
}

/// Events of one task.
pub async fn task_events<R: Runtime>(
    app: &AppHandle<R>,
    id: &str,
    after: Option<i64>,
) -> Result<Value, CommandError> {
    let daemon = daemon_of(app);
    let path = daemon::task_events_path(id)?;
    let query = after_query(after);
    let events = daemon.get_with_query(&path, &query).await?;
    Ok(json!({ "id": id, "events": daemon::event_list(&events) }))
}

/// Ask the daemon to cancel one task.
pub async fn cancel_task<R: Runtime>(app: &AppHandle<R>, id: &str) -> Result<Value, CommandError> {
    let daemon = daemon_of(app);
    let path = daemon::task_cancel_path(id)?;
    match daemon.post_empty(&path).await {
        Ok(value) => Ok(value),
        Err(DaemonError::Unavailable(_)) => {
            let job = daemon::job_cancel_path(id)?;
            Ok(daemon.post_empty(&job).await?)
        }
        Err(error) => Err(error.into()),
    }
}

/// Send a correction to a running task.
pub async fn correct_task<R: Runtime>(
    app: &AppHandle<R>,
    id: &str,
    instruction: &str,
    revision: Option<i64>,
) -> Result<Value, CommandError> {
    let daemon = daemon_of(app);
    daemon::validate_text("instruction", instruction)?;
    if instruction.trim().is_empty() {
        return Err(CommandError::invalid("instruction must not be empty"));
    }
    let path = daemon::task_correct_path(id)?;
    let mut body = json!({ "instruction": instruction });
    if let Some(revision) = revision {
        body["expected_revision"] = json!(revision);
    }
    Ok(daemon.post(&path, &body).await?)
}

/// Record an approval decision.
pub async fn approve<R: Runtime>(
    app: &AppHandle<R>,
    id: &str,
    allow: bool,
    revision: Option<i64>,
    hash: Option<String>,
) -> Result<Value, CommandError> {
    let daemon = daemon_of(app);
    let path = daemon::approval_path(id)?;
    let mut body = json!({ "allow": allow });
    if let Some(revision) = revision {
        body["expected_revision"] = json!(revision);
    }
    if let Some(hash) = hash {
        daemon::validate_hash(&hash)?;
        body["arguments_hash"] = json!(hash);
    }
    Ok(daemon.post(&path, &body).await?)
}

/// One artifact, addressed by its content hash.
pub async fn artifact<R: Runtime>(
    app: &AppHandle<R>,
    hash: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    session: Option<&str>,
    task: Option<&str>,
) -> Result<Value, CommandError> {
    let daemon = daemon_of(app);
    let base = daemon::artifact_path(hash)?;
    let mut query: Vec<(&str, String)> = Vec::new();
    if let Some(offset) = offset {
        query.push(("offset", offset.to_string()));
    }
    if let Some(limit) = limit {
        query.push(("limit", limit.min(65_536).to_string()));
    }
    if let Some(session) = session {
        daemon::validate_id(session)?;
        query.push(("session", session.to_string()));
    }
    if let Some(task) = task {
        daemon::validate_id(task)?;
        query.push(("task", task.to_string()));
    }
    Ok(daemon.get_with_query(&base, &query).await?)
}

/// Connections known to the daemon.
///
/// The shared frontend expects a list.
pub async fn list_connections<R: Runtime>(app: &AppHandle<R>) -> Result<Value, CommandError> {
    let daemon = daemon_of(app);
    let connections = daemon.get("/v1/connections").await?;
    Ok(match connections {
        Value::Array(items) => Value::Array(items),
        Value::Object(ref map) => map
            .get("connections")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
        _ => Value::Array(Vec::new()),
    })
}

/// Probe, enable, disable or revoke one connection.
pub async fn connection_action<R: Runtime>(
    app: &AppHandle<R>,
    id: &str,
    action: &str,
) -> Result<Value, CommandError> {
    let daemon = daemon_of(app);
    let path = daemon::connection_action_path(id, action)?;
    Ok(daemon.post_empty(&path).await?)
}

/// Usage reported by the tasks of the daemon.
///
/// The shared frontend expects the totals and the task list.
pub async fn usage<R: Runtime>(app: &AppHandle<R>) -> Result<Value, CommandError> {
    let daemon = daemon_of(app);
    let query = vec![("limit", daemon::bounded_limit(None).to_string())];
    let tasks = daemon.get_with_query("/v1/tasks", &query).await?;
    let list = daemon::task_list(&tasks);
    let mut totals: BTreeMap<String, f64> = BTreeMap::new();
    for task in &list {
        if let Some(usage) = task.get("usage").and_then(Value::as_object) {
            for (key, value) in usage {
                if let Some(number) = value.as_f64() {
                    *totals.entry(key.clone()).or_insert(0.0) += number;
                }
            }
        }
    }
    Ok(json!({
      "totals": totals,
      "tasks": list,
      "task_count": list.len(),
      "active_tasks": daemon::count_active_tasks(&tasks),
    }))
}

/// Configuration of the daemon.
pub async fn config<R: Runtime>(app: &AppHandle<R>) -> Result<Value, CommandError> {
    let daemon = daemon_of(app);
    Ok(daemon.get("/v1/config").await?)
}

/// Models offered by the daemon.
///
/// The shared frontend expects a list.
pub async fn models<R: Runtime>(app: &AppHandle<R>) -> Result<Value, CommandError> {
    let daemon = daemon_of(app);
    let models = daemon.get("/v1/models").await?;
    Ok(match models {
        Value::Object(map) => Value::Array(
            map.into_iter()
                .map(|(name, mut value)| {
                    if let Some(object) = value.as_object_mut() {
                        object.entry("alias").or_insert(json!(name));
                    }
                    value
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items),
        _ => Value::Array(Vec::new()),
    })
}

/// Alias of [`models`] used by the shared frontend.
pub async fn list_models<R: Runtime>(app: &AppHandle<R>) -> Result<Value, CommandError> {
    models(app).await
}

/// One task, addressed by its identifier.
pub async fn get_task<R: Runtime>(app: &AppHandle<R>, id: &str) -> Result<Value, CommandError> {
    task(app, id).await
}

/// Capabilities of the Model Context Protocol transport of the daemon.
pub async fn mcp_capabilities<R: Runtime>(app: &AppHandle<R>) -> Result<Value, CommandError> {
    let daemon = daemon_of(app);
    let body = json!({
      "jsonrpc": "2.0",
      "id": 1,
      "method": "initialize",
      "params": {
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "clientInfo": {
          "name": "layeragent-desktop",
          "version": env!("CARGO_PKG_VERSION"),
        }
      }
    });
    let headers = [("Accept", "application/json, text/event-stream")];
    let text = match daemon.post_text("/mcp", &body, &headers).await {
        Ok(text) => text,
        Err(error) if error.is_unavailable() => {
            return Err(CommandError::from(error));
        }
        Err(error) => {
            return Ok(json!({
              "available": false,
              "endpoint": "/mcp",
              "error": error.to_string(),
            }));
        }
    };
    let response = parse_json_rpc(&text);
    let available = response.is_some();
    // The shared frontend reads the negotiated capabilities of the daemon.
    let capabilities = response
        .as_ref()
        .and_then(|value| value.get("result"))
        .and_then(|result| result.get("capabilities"));
    let resources = capabilities
        .and_then(|value| value.get("resources"))
        .map(|value| !value.is_null())
        .unwrap_or(false);
    let tools = capabilities
        .and_then(|value| value.get("tools"))
        .map(|value| !value.is_null())
        .unwrap_or(false);
    let prompts = capabilities
        .and_then(|value| value.get("prompts"))
        .map(|value| !value.is_null())
        .unwrap_or(false);
    Ok(json!({
      "available": available,
      "resources": resources,
      "tools": tools,
      "prompts": prompts,
      "endpoint": "/mcp",
      "transport": "streamable-http",
      "response": response.unwrap_or(Value::Null),
      "error": if available {
        Value::Null
      } else {
        json!("the daemon did not answer with a JSON-RPC response")
      },
    }))
}

/// Read the JSON-RPC answer of a Model Context Protocol handshake.
fn parse_json_rpc(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return Some(value);
    }
    for line in trimmed.lines() {
        let line = line.trim();
        if let Some(payload) = line.strip_prefix("data:") {
            if let Ok(value) = serde_json::from_str::<Value>(payload.trim()) {
                return Some(value);
            }
        }
    }
    None
}

/// Reachability and workload of the daemon.
pub async fn daemon_status<R: Runtime>(app: &AppHandle<R>) -> Result<Value, CommandError> {
    let daemon = daemon_of(app);
    let endpoint = daemon.endpoint().clone();
    let (reachable, health, health_error) = match daemon.health().await {
        Ok(value) => (true, value, Value::Null),
        Err(error) => (false, Value::Null, json!(error.to_string())),
    };
    let mut active_tasks = 0usize;
    let mut tasks_available = false;
    let mut tasks_error = Value::Null;
    match daemon
        .get_with_query("/v1/tasks", &[("limit", "500".to_string())])
        .await
    {
        Ok(tasks) => {
            tasks_available = true;
            active_tasks = daemon::count_active_tasks(&tasks);
        }
        Err(error) => tasks_error = json!(error.to_string()),
    }
    Ok(json!({
      "reachable": reachable,
      "active_tasks": active_tasks,
      "daemon": {
        "reachable": reachable,
        "health": health,
        "profile": endpoint.profile,
        "socket": endpoint.socket.to_string_lossy(),
        "active_tasks": active_tasks,
        "tasks_available": tasks_available,
        "error": health_error,
        "tasks_error": tasks_error,
      }
    }))
}

/// Stop the daemon and report every cancellation whose effect is unknown.
///
/// The daemon is never killed: the shell asks it to stop and reports the tasks
/// whose external effect could not be confirmed.
pub async fn stop_daemon<R: Runtime>(
    app: &AppHandle<R>,
    id: Option<String>,
) -> Result<Value, CommandError> {
    let daemon = daemon_of(app);
    let mut errors: Vec<String> = Vec::new();
    let mut cancelled = Value::Null;

    if let Some(id) = &id {
        daemon::validate_id(id)?;
        let path = daemon::task_cancel_path(id)?;
        match daemon.post_empty(&path).await {
            Ok(value) => cancelled = value,
            Err(DaemonError::Unavailable(_)) => {
                let job = daemon::job_cancel_path(id)?;
                match daemon.post_empty(&job).await {
                    Ok(value) => cancelled = value,
                    Err(error) => errors.push(error.to_string()),
                }
            }
            Err(error) => errors.push(error.to_string()),
        }
    }

    let tasks = daemon
        .get_with_query("/v1/tasks", &[("limit", "500".to_string())])
        .await
        .unwrap_or(Value::Null);
    let uncertain = daemon::uncertain_tasks(&tasks)
        .iter()
        .map(daemon::task_summary)
        .collect::<Vec<Value>>();
    let requested = daemon::requested_cancellations(&tasks)
        .iter()
        .map(daemon::task_summary)
        .collect::<Vec<Value>>();

    let stopped = match daemon.post_empty("/v1/shutdown").await {
        Ok(_) => true,
        Err(error) => {
            errors.push(error.to_string());
            false
        }
    };

    Ok(json!({
      "stopped": stopped,
      "uncertain_tasks": uncertain,
      "requested_cancellations": requested,
      "cancelled": cancelled,
      "errors": errors,
    }))
}

/// Stream the events of a session or a task to the webview.
///
/// An empty path cancels every running subscription.
pub async fn subscribe<R: Runtime>(app: &AppHandle<R>, path: &str) -> Result<Value, CommandError> {
    let state = app.state::<AppState>();
    if path.trim().is_empty() {
        let cancelled = state.cancel_all();
        return Ok(json!({ "cancelled": cancelled, "active": 0 }));
    }
    let target = daemon::parse_stream_path(path)?;
    let id = next_subscription_id();
    let (cancel, receiver) = watch::channel(false);
    state.insert_subscription(Subscription {
        id: id.clone(),
        target: target.clone(),
        cancel,
    });
    let daemon = state.daemon.clone();
    drop(state);
    let handle = (*app).clone();
    let stream_target = target.clone();
    let stream_id = id.clone();
    tauri::async_runtime::spawn(async move {
        stream_events(
            handle.clone(),
            daemon,
            stream_target,
            stream_id.clone(),
            receiver,
        )
        .await;
        handle.state::<AppState>().remove_subscription(&stream_id);
    });
    Ok(json!({
      "subscription": id,
      "path": path,
      "target": target.kind(),
      "id": target.id(),
      "started": true,
    }))
}

/// Cancels one subscription.
///
/// The shared frontend disposes a subscription with this command when the
/// view that opened it is left.
pub async fn unsubscribe<R: Runtime>(
    app: &AppHandle<R>,
    id: Option<String>,
) -> Result<Value, CommandError> {
    let state = app.state::<AppState>();
    let Some(id) = id else {
        let cancelled = state.cancel_all();
        return Ok(json!({ "cancelled": cancelled, "active": 0 }));
    };
    let cancelled = state.cancel_subscription(&id);
    Ok(json!({ "cancelled": cancelled, "id": id }))
}

/// Poll one event stream and forward every event to the webview.
async fn stream_events<R: Runtime>(
    app: AppHandle<R>,
    daemon: Daemon,
    target: daemon::StreamTarget,
    id: String,
    mut cancel: watch::Receiver<bool>,
) {
    let path = match target.path() {
        Ok(path) => path,
        Err(_) => return,
    };
    let mut after: i64 = 0;
    let mut failures: u32 = 0;
    loop {
        tokio::select! {
          changed = cancel.changed() => {
            if changed.is_err() || *cancel.borrow() {
              break;
            }
          }
          _ = tokio::time::sleep(POLL_INTERVAL) => {}
        }
        let query = vec![("after", after.to_string())];
        match daemon.get_with_query(&path, &query).await {
            Ok(value) => {
                failures = 0;
                for event in daemon::event_list(&value) {
                    if let Some(seq) = daemon::event_seq(&event) {
                        if seq > after {
                            after = seq;
                        }
                    }
                    let payload = json!({
                      "subscription": id,
                      "target": target.kind(),
                      "id": target.id(),
                      "seq": daemon::event_seq(&event),
                      "event": event.clone(),
                    });
                    let _ = app.emit(daemon::EVENT_CHANNEL, payload);
                    // The shared frontend expects the event itself.
                    let _ = app.emit(daemon::TASK_EVENT_CHANNEL, event);
                }
            }
            Err(error) => {
                failures += 1;
                let payload = json!({
                  "subscription": id,
                  "target": target.kind(),
                  "id": target.id(),
                  "error": error.to_string(),
                  "unavailable": error.is_unavailable(),
                });
                let _ = app.emit(daemon::EVENT_CHANNEL, payload.clone());
                let _ = app.emit(daemon::TASK_EVENT_CHANNEL, payload);
                if failures >= MAX_FAILURES {
                    break;
                }
            }
        }
    }
}

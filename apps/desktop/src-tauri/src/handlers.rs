//! Tauri command wrappers.
//!
//! The wrappers stay free of logic: they only translate the arguments coming
//! from the webview into the bounded commands of [`crate::commands`].
//!
//! The argument keys are snake_case. Every identifier argument is offered
//! under the short name `id` and under the long name of the object it
//! addresses (`task_id`, `session_id`, `approval_id`, `connection_id`), so a
//! caller may use either spelling. The short names are single words and are
//! therefore identical in `snake_case` and in `camelCase`.

use serde_json::Value;
use tauri::AppHandle;

use crate::commands;
use crate::error::CommandError;

/// Resolve an identifier that may arrive under two names.
fn resolve_id(
    id: Option<String>,
    alias: Option<String>,
    object: &str,
) -> Result<String, CommandError> {
    id.or(alias)
        .ok_or_else(|| CommandError::invalid(format!("the identifier of the {object} is required")))
}

/// Everything the shell knows about the local installation.
#[tauri::command(rename_all = "snake_case")]
pub async fn bootstrap(app: AppHandle) -> Result<Value, CommandError> {
    commands::bootstrap(&app).await
}

/// Projects known to the daemon.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_projects(app: AppHandle) -> Result<Value, CommandError> {
    commands::list_projects(&app).await
}

/// Sessions known to the daemon.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_sessions(app: AppHandle) -> Result<Value, CommandError> {
    commands::list_sessions(&app).await
}

/// Events of one session.
#[tauri::command(rename_all = "snake_case")]
pub async fn session_events(
    app: AppHandle,
    id: Option<String>,
    session_id: Option<String>,
    after: Option<i64>,
) -> Result<Value, CommandError> {
    let id = resolve_id(id, session_id, "session")?;
    commands::session_events(&app, &id, after).await
}

/// Tasks known to the daemon.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_tasks(
    app: AppHandle,
    after: Option<i64>,
    limit: Option<u32>,
) -> Result<Value, CommandError> {
    commands::list_tasks(&app, after, limit).await
}

/// Submit a task to the daemon.
#[tauri::command(rename_all = "snake_case")]
pub async fn create_task(
    app: AppHandle,
    workspace: String,
    objective: String,
    session: Option<String>,
    session_id: Option<String>,
    model: Option<String>,
    executor: Option<String>,
) -> Result<Value, CommandError> {
    commands::create_task(
        &app,
        &workspace,
        &objective,
        session.or(session_id),
        model,
        executor,
    )
    .await
}

/// One task.
#[tauri::command(rename_all = "snake_case")]
pub async fn task(
    app: AppHandle,
    id: Option<String>,
    task_id: Option<String>,
) -> Result<Value, CommandError> {
    let id = resolve_id(id, task_id, "task")?;
    commands::task(&app, &id).await
}

/// One task. The shared frontend calls this name.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_task(
    app: AppHandle,
    id: Option<String>,
    task_id: Option<String>,
) -> Result<Value, CommandError> {
    let id = resolve_id(id, task_id, "task")?;
    commands::task(&app, &id).await
}

/// Events of one task.
#[tauri::command(rename_all = "snake_case")]
pub async fn task_events(
    app: AppHandle,
    id: Option<String>,
    task_id: Option<String>,
    after: Option<i64>,
) -> Result<Value, CommandError> {
    let id = resolve_id(id, task_id, "task")?;
    commands::task_events(&app, &id, after).await
}

/// Ask the daemon to cancel one task.
#[tauri::command(rename_all = "snake_case")]
pub async fn cancel_task(
    app: AppHandle,
    id: Option<String>,
    task_id: Option<String>,
) -> Result<Value, CommandError> {
    let id = resolve_id(id, task_id, "task")?;
    commands::cancel_task(&app, &id).await
}

/// Send a correction to a running task.
#[tauri::command(rename_all = "snake_case")]
pub async fn correct_task(
    app: AppHandle,
    id: Option<String>,
    task_id: Option<String>,
    instruction: String,
    revision: Option<i64>,
    expected_revision: Option<i64>,
) -> Result<Value, CommandError> {
    let id = resolve_id(id, task_id, "task")?;
    commands::correct_task(&app, &id, &instruction, revision.or(expected_revision)).await
}

/// Record an approval decision.
#[tauri::command(rename_all = "snake_case")]
pub async fn approve(
    app: AppHandle,
    id: Option<String>,
    approval_id: Option<String>,
    allow: Option<bool>,
    revision: Option<i64>,
    expected_revision: Option<i64>,
    hash: Option<String>,
    arguments_hash: Option<String>,
) -> Result<Value, CommandError> {
    let id = resolve_id(id, approval_id, "approval")?;
    // A missing decision denies the action: the daemon only runs an approval
    // that was explicitly allowed.
    commands::approve(
        &app,
        &id,
        allow.unwrap_or(false),
        revision.or(expected_revision),
        hash.or(arguments_hash),
    )
    .await
}

/// One artifact, addressed by its content hash.
#[tauri::command(rename_all = "snake_case")]
pub async fn artifact(
    app: AppHandle,
    hash: Option<String>,
    artifact_hash: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
    session: Option<String>,
    task: Option<String>,
) -> Result<Value, CommandError> {
    let hash = resolve_id(hash, artifact_hash, "artifact")?;
    commands::artifact(&app, &hash, offset, limit, session.as_deref(), task.as_deref()).await
}

/// Connections known to the daemon.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_connections(app: AppHandle) -> Result<Value, CommandError> {
    commands::list_connections(&app).await
}

/// Probe, enable, disable or revoke one connection.
#[tauri::command(rename_all = "snake_case")]
pub async fn connection_action(
    app: AppHandle,
    id: Option<String>,
    connection_id: Option<String>,
    action: String,
) -> Result<Value, CommandError> {
    let id = resolve_id(id, connection_id, "connection")?;
    commands::connection_action(&app, &id, &action).await
}

/// Usage reported by the tasks of the daemon.
#[tauri::command(rename_all = "snake_case")]
pub async fn usage(app: AppHandle) -> Result<Value, CommandError> {
    commands::usage(&app).await
}

/// Configuration of the daemon.
#[tauri::command(rename_all = "snake_case")]
pub async fn config(app: AppHandle) -> Result<Value, CommandError> {
    commands::config(&app).await
}

/// Models offered by the daemon.
#[tauri::command(rename_all = "snake_case")]
pub async fn models(app: AppHandle) -> Result<Value, CommandError> {
    commands::models(&app).await
}

/// Models offered by the daemon. The shared frontend calls this name.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_models(app: AppHandle) -> Result<Value, CommandError> {
    commands::models(&app).await
}

/// Capabilities of the Model Context Protocol transport of the daemon.
#[tauri::command(rename_all = "snake_case")]
pub async fn mcp_capabilities(app: AppHandle) -> Result<Value, CommandError> {
    commands::mcp_capabilities(&app).await
}

/// Reachability and workload of the daemon.
#[tauri::command(rename_all = "snake_case")]
pub async fn daemon_status(app: AppHandle) -> Result<Value, CommandError> {
    commands::daemon_status(&app).await
}

/// Stop the daemon and report every cancellation whose effect is unknown.
#[tauri::command(rename_all = "snake_case")]
pub async fn stop_daemon(
    app: AppHandle,
    id: Option<String>,
    task_id: Option<String>,
) -> Result<Value, CommandError> {
    commands::stop_daemon(&app, id.or(task_id)).await
}

/// Stream the events of a session or a task to the webview.
///
/// An empty path cancels every running subscription.
#[tauri::command(rename_all = "snake_case")]
pub async fn subscribe(app: AppHandle, path: String) -> Result<Value, CommandError> {
    commands::subscribe(&app, &path).await
}

/// Cancel one subscription.
#[tauri::command(rename_all = "snake_case")]
pub async fn unsubscribe(app: AppHandle, id: Option<String>) -> Result<Value, CommandError> {
    commands::unsubscribe(&app, id).await
}

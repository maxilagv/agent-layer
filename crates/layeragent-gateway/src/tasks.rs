//! Durable task surface.
//!
//! Every interface (CLI, TUI, desktop, web) talks to the same persisted task
//! identity. Creation is idempotent, events are read from the durable log, and
//! cancellation/corrections/approvals quote the revision the caller observed.
use super::{ApiError, ApiResult, ApiState, Identity, JobRegistration};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response, sse::Sse},
};
use layeragent_protocol::{ApprovalDecision, CorrectTask, NewTask, RunRequest, Task};
use layeragent_storage::{TaskConflict, TaskCreate};
use serde::Deserialize;
use serde_json::{Value, json};
use std::convert::Infallible;

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 96
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn not_found() -> ApiError {
    ApiError::not_found(anyhow::anyhow!("task not found or inaccessible"))
}

#[derive(Deserialize)]
pub(super) struct TaskQuery {
    #[serde(default)]
    after: i64,
    limit: Option<usize>,
}

pub(super) async fn list(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
    Query(q): Query<TaskQuery>,
) -> ApiResult<Json<Value>> {
    let store = s.engine.store(&i.scope).await?;
    let limit = q.limit.unwrap_or(50);
    let (tasks, next) = store.tasks(&i.scope, q.after, limit).await?;
    Ok(Json(json!({"tasks": tasks, "next": next})))
}

#[derive(Deserialize)]
pub(super) struct TaskInput {
    #[serde(default)]
    workspace: Option<String>,
    objective: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    executor: Option<String>,
    #[serde(default)]
    idempotency_key: Option<String>,
    #[serde(default)]
    effort: layeragent_protocol::EffortLevel,
    #[serde(default)]
    mode: layeragent_protocol::RunMode,
    #[serde(default)]
    bridge: bool,
    #[serde(default)]
    execution: layeragent_protocol::ExecutionMode,
    #[serde(default)]
    cooperative: Option<layeragent_protocol::CooperativeOptions>,
}

/// Creates the durable task and returns its identity before execution starts.
/// Identical replay returns the same task and never starts a second run.
pub(super) async fn create(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
    headers: HeaderMap,
    Json(r): Json<TaskInput>,
) -> ApiResult<Response> {
    let key = r
        .idempotency_key
        .clone()
        .or_else(|| {
            headers
                .get("idempotency-key")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        })
        .filter(|key| !key.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Idempotency-Key header is required"))?;
    if key.len() > 200 {
        return Err(anyhow::anyhow!("idempotency key is too long").into());
    }
    if r.objective.trim().is_empty() || r.objective.len() > 262_144 {
        return Err(anyhow::anyhow!("objective must contain 1..262144 bytes").into());
    }
    if r.bridge {
        return Err(anyhow::anyhow!(
            "bridge is deprecated; use execution=cooperative with typed cooperative options"
        )
        .into());
    }
    if r.mode != layeragent_protocol::RunMode::Normal {
        return Err(anyhow::anyhow!("illustration is not implemented yet").into());
    }
    if r.execution != layeragent_protocol::ExecutionMode::Cooperative && r.cooperative.is_some() {
        return Err(anyhow::anyhow!("cooperative options require execution=cooperative").into());
    }
    let executor = r.executor.clone().unwrap_or_else(|| "local".into());
    if executor != "local" {
        return Err(ApiError::with_status(
            StatusCode::SERVICE_UNAVAILABLE,
            anyhow::anyhow!("executor is not available; configure it in connections"),
        ));
    }
    let store = s.engine.store(&i.scope).await?;
    // A revoked or disabled connection must not start new work. Existing work
    // was stopped when the generation changed.
    let config = s.engine.config().await;
    let cooperative = if r.execution == layeragent_protocol::ExecutionMode::Cooperative {
        let options = r
            .cooperative
            .clone()
            .unwrap_or_default()
            .normalized(config.agents.tree_max_tokens)?;
        for model in options.role_models.values() {
            if !config.models.contains_key(model) {
                return Err(anyhow::anyhow!(
                    "unknown role model {model}; no provider fallback is applied"
                )
                .into());
            }
        }
        if options.limits.max_cost_usd.is_some() {
            let selected = r.model.clone().unwrap_or_else(|| config.primary.clone());
            for alias in std::iter::once(&selected).chain(options.role_models.values()) {
                let model = config
                    .models
                    .get(alias)
                    .ok_or_else(|| anyhow::anyhow!("model {alias} is not configured"))?;
                if model.input_per_million.is_none() || model.output_per_million.is_none() {
                    return Err(anyhow::anyhow!(
                        "cost limits require configured input and output prices for model {alias}"
                    )
                    .into());
                }
            }
        }
        if options.scope.allow_write && !config.policy.write {
            return Err(anyhow::anyhow!("cooperative write scope exceeds task policy").into());
        }
        Some(options)
    } else {
        None
    };
    let alias = r.model.clone().unwrap_or_else(|| config.primary.clone());
    let provider = config
        .models
        .get(&alias)
        .map(|model| model.provider.clone())
        .unwrap_or(alias);
    let connection = super::connections::id("provider", &provider);
    if let Some((enabled, generation)) = store.connection_state(&connection).await?
        && (!enabled
            || !store
                .connection_allows(&connection, Some(generation))
                .await?)
    {
        return Err(ApiError::with_status(
            StatusCode::CONFLICT,
            anyhow::anyhow!(
                "connection {connection} is revoked or disabled; re-enable it before starting work"
            ),
        ));
    }
    let (workspace, session_id) = match &r.session_id {
        Some(id) => {
            let session = store.session(id, &i.scope.principal).await?;
            i.require_workspace(&session.workspace)?;
            if let Some(requested) = &r.workspace {
                let requested = std::path::Path::new(requested).canonicalize()?;
                if requested != std::path::Path::new(&session.workspace) {
                    return Err(anyhow::anyhow!("workspace does not match the session").into());
                }
            }
            (session.workspace, Some(id.clone()))
        }
        None => {
            let path = std::path::Path::new(
                r.workspace
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("workspace is required"))?,
            )
            .canonicalize()?;
            if !(path.is_dir() && path.parent().is_some() && !s.engine.home.starts_with(&path)) {
                return Err(anyhow::anyhow!("invalid workspace").into());
            }
            i.require_workspace(path.to_string_lossy().as_ref())?;
            (path.to_string_lossy().into_owned(), None)
        }
    };
    let request = NewTask {
        workspace: workspace.clone(),
        session_id: session_id.clone(),
        objective: r.objective.clone(),
        model: r.model.clone(),
        executor: Some(executor.clone()),
        effort: r.effort,
        mode: r.mode,
        bridge: r.bridge,
        execution: r.execution,
        cooperative: cooperative.clone(),
    };
    let fingerprint = request.fingerprint()?;
    let created = store
        .create_task_with_execution(
            &i.scope,
            &workspace,
            session_id.as_deref(),
            &r.objective,
            &executor,
            &key,
            &fingerprint,
            request.effort,
            request.mode,
            request.bridge,
            request.execution,
            request.cooperative.clone(),
        )
        .await
        .map_err(|error| {
            if error.downcast_ref::<TaskConflict>().is_some() {
                ApiError::conflict(error)
            } else {
                error.into()
            }
        })?;
    let (task, status) = match created {
        TaskCreate::Created(task) => (task, StatusCode::CREATED),
        TaskCreate::Replayed(task) => (task, StatusCode::OK),
    };
    if should_start(&s, &task) {
        start(&s, &i, &task, r.model.clone());
    }
    Ok((status, Json(task)).into_response())
}

/// A replayed task is started only if it never reached the engine. A live task
/// is left alone, which is what prevents duplicate execution after a lost
/// response.
fn should_start(s: &ApiState, task: &Task) -> bool {
    if task.state != layeragent_protocol::TaskState::Prepared {
        return false;
    }
    s.jobs
        .lock()
        .map(|jobs| !jobs.contains_key(&task.id))
        .unwrap_or(false)
}

pub(super) fn start(s: &ApiState, i: &Identity, task: &Task, model: Option<String>) {
    let cancel = i.authority.child_token();
    {
        let Ok(mut jobs) = s.jobs.lock() else {
            return;
        };
        if jobs.contains_key(&task.id) {
            return;
        }
        jobs.insert(task.id.clone(), (i.scope.clone(), cancel.clone()));
    }
    let engine = s.engine.clone();
    let scope = i.scope.clone();
    let jobs = s.jobs.clone();
    let id = task.id.clone();
    let session = task.session_id.clone();
    let workspace = session.is_none().then_some(task.project.clone());
    let objective = task.objective.clone();
    let (effort, bridge, mode, execution) = (task.effort, task.bridge, task.mode, task.execution);
    tokio::spawn(async move {
        let _registration = JobRegistration {
            jobs: jobs.clone(),
            id: id.clone(),
        };
        let result = if execution == layeragent_protocol::ExecutionMode::Cooperative {
            engine
                .run_cooperative(scope.clone(), id.clone(), model, cancel)
                .await
        } else {
            engine
                .run_bound(
                    scope.clone(),
                    RunRequest {
                        session_id: session,
                        workspace,
                        prompt: objective,
                        model,
                        effort,
                        bridge,
                        mode,
                        execution,
                        cooperative: None,
                    },
                    cancel,
                    id.clone(),
                )
                .await
                .map(|_| ())
        };
        if let Err(error) = result {
            // Execution may have failed before the engine opened the turn, so
            // make sure the durable task cannot remain `prepared` forever.
            if let Ok(store) = engine.store(&scope).await
                && let Ok(current) = store.task(&scope, &id).await
                && !current.state.terminal()
            {
                let _ = store.set_task_blocked(&id, &error.to_string()).await;
            }
        }
    });
}

pub(super) async fn get(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
    Path(id): Path<String>,
) -> ApiResult<Json<Task>> {
    if !valid_id(&id) {
        return Err(not_found());
    }
    Ok(Json(
        s.engine
            .store(&i.scope)
            .await?
            .task(&i.scope, &id)
            .await
            .map_err(|_| not_found())?,
    ))
}

#[derive(Deserialize)]
pub(super) struct EventQuery {
    #[serde(default)]
    after: i64,
    limit: Option<usize>,
}

pub(super) async fn events(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
    Path(id): Path<String>,
    Query(q): Query<EventQuery>,
) -> ApiResult<Json<Value>> {
    if !valid_id(&id) {
        return Err(not_found());
    }
    let store = s.engine.store(&i.scope).await?;
    store.task(&i.scope, &id).await.map_err(|_| not_found())?;
    let events = store
        .task_events(&id, q.after, q.limit.unwrap_or(200))
        .await?;
    let next = events
        .last()
        .and_then(|event| event["seq"].as_i64())
        .filter(|next| *next > q.after);
    Ok(Json(json!({"events": events, "next": next})))
}

/// Connection-independent SSE: events are polled from the durable log, so a
/// reconnect with the last sequence neither repeats nor skips work.
pub(super) async fn stream(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
    Path(id): Path<String>,
    Query(q): Query<EventQuery>,
) -> ApiResult<Response> {
    if !valid_id(&id) {
        return Err(not_found());
    }
    let stream_lease = match s.admission.stream(&i.scope) {
        Ok(lease) => lease,
        Err(status) => return Ok(super::admission::limited_response(status)),
    };
    let store = s.engine.store(&i.scope).await?;
    store.task(&i.scope, &id).await.map_err(|_| not_found())?;
    let authority = i.authority.clone();
    let mut cursor = q.after;
    let stream = async_stream::stream! {
        let mut terminal = false;
        loop {
            tokio::select! {
                _ = authority.cancelled() => break,
                _ = tokio::time::sleep(std::time::Duration::from_millis(150)) => {}
            }
            match store.task_events(&id, cursor, 200).await {
                Ok(events) if events.is_empty() => {
                    if terminal {
                        break;
                    }
                    // Confirm the task still exists and whether it finished
                    // while there were no new events to send.
                    match store.task(&i.scope, &id).await {
                        Ok(task) => terminal = task.state.terminal(),
                        Err(_) => break,
                    }
                }
                Ok(events) => {
                    for event in events {
                        if let Some(seq) = event["seq"].as_i64() {
                            cursor = seq;
                        }
                        let kind = event["kind"].as_str().unwrap_or("event").to_string();
                        yield Ok::<_, Infallible>(
                            axum::response::sse::Event::default()
                                .id(cursor.to_string())
                                .event(kind)
                                .data(event.to_string()),
                        );
                    }
                }
                Err(_) => break,
            }
        }
    };
    Ok(super::admission::hold_response(
        Sse::new(stream)
            .keep_alive(axum::response::sse::KeepAlive::default())
            .into_response(),
        stream_lease,
    ))
}

pub(super) async fn cancel(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    if !valid_id(&id) {
        return Err(not_found());
    }
    let store = s.engine.store(&i.scope).await?;
    let task = store.task(&i.scope, &id).await.map_err(|_| not_found())?;
    if task.state.terminal() {
        return Ok(Json(json!({"cancelled": false, "task": task})));
    }
    let requested = store.request_cancel(&i.scope, &id).await?;
    let live = s.engine.cancel(&i.scope, &id)?;
    if let Ok(jobs) = s.jobs.lock()
        && let Some((scope, token)) = jobs.get(&id)
        && scope == &i.scope
    {
        token.cancel();
    }
    if !live && requested {
        // Nothing reached the engine yet: persist a confirmed cancellation
        // instead of leaving work that looks resumable.
        store
            .cancel_unstarted(&id, "cancelled before execution started")
            .await?;
    }
    let task = store.task(&i.scope, &id).await?;
    Ok(Json(json!({
        "cancelled": requested,
        "live": live,
        "cancellation": task.cancellation,
        "task": task,
    })))
}

#[derive(Deserialize)]
pub(super) struct Correction {
    instruction: String,
    expected_revision: i64,
}

pub(super) async fn correct(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
    Path(id): Path<String>,
    Json(r): Json<Correction>,
) -> ApiResult<Json<Value>> {
    if !valid_id(&id) {
        return Err(not_found());
    }
    let store = s.engine.store(&i.scope).await?;
    store.task(&i.scope, &id).await.map_err(|_| not_found())?;
    let correction = store
        .correct_task(
            &i.scope,
            &id,
            CorrectTask {
                instruction: r.instruction,
                expected_revision: r.expected_revision,
            },
        )
        .await
        .map_err(ApiError::conflict)?;
    Ok(Json(json!(correction)))
}

pub(super) async fn approve(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
    Path((id, approval)): Path<(String, String)>,
    Json(r): Json<ApprovalDecision>,
) -> ApiResult<Json<Value>> {
    if !valid_id(&id) || !valid_id(&approval) {
        return Err(not_found());
    }
    let store = s.engine.store(&i.scope).await?;
    let task = store.task(&i.scope, &id).await.map_err(|_| not_found())?;
    s.engine
        .approve(&i.scope, Some(&task.id), &approval, &r)
        .await
        .map_err(|error| {
            let message = error.to_string();
            if message.contains("inaccessible") {
                ApiError::forbidden(error)
            } else {
                ApiError::conflict(error)
            }
        })?;
    Ok(Json(json!({"recorded": true})))
}

#[derive(Deserialize)]
pub(super) struct ArtifactQuery {
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    task: Option<String>,
}

pub(super) async fn artifact(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
    Path(hash): Path<String>,
    Query(q): Query<ArtifactQuery>,
) -> ApiResult<Json<Value>> {
    // Principal-wide artifact access is owner-only; a browser must quote the
    // session or task that produced the artifact.
    if q.session.is_none() && q.task.is_none() && !i.owner {
        return Err(ApiError::forbidden(anyhow::anyhow!(
            "artifact scope required"
        )));
    }
    let store = s.engine.store(&i.scope).await?;
    if let Some(task) = &q.task {
        let bound = store.task(&i.scope, task).await.map_err(|_| not_found())?;
        if !bound.session_id.as_deref().is_none_or(|session| {
            q.session
                .as_deref()
                .is_none_or(|requested| requested == session)
        }) {
            return Err(not_found());
        }
    }
    let value = store
        .artifact_scoped(
            &i.scope,
            q.session.as_deref(),
            q.task.as_deref(),
            &hash,
            q.offset,
            q.limit.unwrap_or(65_536).clamp(1, 65_536),
        )
        .await
        .map_err(|_| not_found())?;
    Ok(Json(value))
}

pub(super) async fn session_artifact(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
    Path((session, hash)): Path<(String, String)>,
    Query(q): Query<ArtifactQuery>,
) -> ApiResult<Json<Value>> {
    if !valid_id(&session) {
        return Err(not_found());
    }
    let store = s.engine.store(&i.scope).await?;
    let bound = store
        .session(&session, &i.scope.principal)
        .await
        .map_err(|_| not_found())?;
    i.require_workspace(&bound.workspace)?;
    let value = store
        .artifact_scoped(
            &i.scope,
            Some(&session),
            None,
            &hash,
            q.offset,
            q.limit.unwrap_or(65_536).clamp(1, 65_536),
        )
        .await
        .map_err(|_| not_found())?;
    Ok(Json(value))
}

pub(super) async fn task_artifact(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
    Path((id, hash)): Path<(String, String)>,
    Query(q): Query<ArtifactQuery>,
) -> ApiResult<Json<Value>> {
    if !valid_id(&id) {
        return Err(not_found());
    }
    let store = s.engine.store(&i.scope).await?;
    let task = store.task(&i.scope, &id).await.map_err(|_| not_found())?;
    let value = store
        .artifact_scoped(
            &i.scope,
            task.session_id.as_deref(),
            Some(&task.id),
            &hash,
            q.offset,
            q.limit.unwrap_or(65_536).clamp(1, 65_536),
        )
        .await
        .map_err(|_| not_found())?;
    Ok(Json(value))
}

pub(super) async fn metrics(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    if !valid_id(&id) {
        return Err(not_found());
    }
    let store = s.engine.store(&i.scope).await?;
    store.task(&i.scope, &id).await.map_err(|_| not_found())?;
    let metrics = store.task_metrics(&id).await?;
    let timing = store.task_timing(&id).await?;
    Ok(Json(json!({"metrics": metrics, "timing": timing})))
}

pub(super) async fn usage(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
) -> ApiResult<Json<Value>> {
    let store = s.engine.store(&i.scope).await?;
    Ok(Json(json!({"totals": store.usage_totals(&i.scope).await?})))
}

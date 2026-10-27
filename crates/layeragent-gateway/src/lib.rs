use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::ws::Message as WsMessage,
    extract::{DefaultBodyLimit, Path, Query, Request, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{
        IntoResponse, Response,
        sse::{Event as SseEvent, KeepAlive, Sse},
    },
    routing::{get, post},
};
use hmac::{Hmac, Mac};
use layeragent_core::Engine;
use layeragent_protocol::redact::redact_text;
use layeragent_protocol::{ApprovalDecision, PlatformEvent, RunRequest, Scope, new_id};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Sha256;
use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{Arc, Mutex},
};
use subtle::ConstantTimeEq;
use tokio_util::sync::CancellationToken;
mod admission;
mod channels;
mod connections;
pub mod mcp;
mod remote_view;
mod tasks;

#[derive(Clone)]
pub struct ApiState {
    pub engine: Arc<Engine>,
    pub ipc: bool,
    /// Shared secret required on loopback-TCP IPC, where any local process can connect
    /// (UNIX sockets rely on filesystem permissions instead).
    ipc_token: Option<Arc<str>>,
    admission: admission::Admission,
    ingress: Arc<tokio::sync::Semaphore>,
    jobs: Arc<Mutex<HashMap<String, (Scope, CancellationToken)>>>,
}
impl ApiState {
    pub fn new(engine: Arc<Engine>, ipc: bool) -> Self {
        Self {
            engine,
            ipc,
            ipc_token: None,
            admission: admission::Admission::default(),
            ingress: Arc::new(tokio::sync::Semaphore::new(128)),
            jobs: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    pub fn with_ipc_token(mut self, token: String) -> Self {
        self.ipc_token = Some(token.into());
        self
    }
}
/// Removing a disconnected SSE observer must not forget a still-running task.
/// This guard removes registration on completion, cancellation, and unwind.
struct JobRegistration {
    jobs: Arc<Mutex<HashMap<String, (Scope, CancellationToken)>>>,
    id: String,
}
impl Drop for JobRegistration {
    fn drop(&mut self) {
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.remove(&self.id);
        }
    }
}
#[derive(Clone)]
struct Identity {
    scope: Scope,
    read_only: bool,
    owner: bool,
    workspaces: Option<Vec<std::path::PathBuf>>,
    authority: CancellationToken,
}
impl Identity {
    fn permits_workspace(&self, workspace: &str) -> bool {
        self.workspaces.as_ref().is_none_or(|roots| {
            std::path::Path::new(workspace)
                .canonicalize()
                .ok()
                .is_some_and(|path| {
                    roots
                        .iter()
                        .filter_map(|root| root.canonicalize().ok())
                        .any(|root| path.starts_with(root))
                })
        })
    }
    fn require_workspace(&self, workspace: &str) -> Result<()> {
        anyhow::ensure!(
            !self.authority.is_cancelled(),
            "authority changed; reconnect"
        );
        anyhow::ensure!(
            self.permits_workspace(workspace),
            "workspace is not authorized for this principal"
        );
        Ok(())
    }
}
pub struct ApiError {
    status: StatusCode,
    error: anyhow::Error,
}
impl ApiError {
    pub fn conflict(error: anyhow::Error) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            error,
        }
    }
    pub fn not_found(error: anyhow::Error) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            error,
        }
    }
    pub fn unauthorized(error: anyhow::Error) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            error,
        }
    }
    pub fn forbidden(error: anyhow::Error) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            error,
        }
    }
    pub fn with_status(status: StatusCode, error: anyhow::Error) -> Self {
        Self { status, error }
    }
}
impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            error: e.into(),
        }
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"error":redact_text(&self.error.to_string())})),
        )
            .into_response()
    }
}
type ApiResult<T> = std::result::Result<T, ApiError>;
pub fn router(state: ApiState) -> Router {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpService, session::local::LocalSessionManager,
    };
    let engine = state.engine.clone();
    let admission = state.admission.clone();
    let mut mcp_config =
        rmcp::transport::streamable_http_server::StreamableHttpServerConfig::default();
    mcp_config.legacy_session_mode = false;
    mcp_config.json_response = true;
    mcp_config.cancellation_token = state.engine.shutdown.child_token();
    let mcp = StreamableHttpService::new(
        move || {
            Ok(mcp::McpService {
                backend: Arc::new(mcp::LimitedBackend {
                    engine: engine.clone(),
                    admission: admission.clone(),
                }),
            })
        },
        Arc::new(LocalSessionManager::default()),
        mcp_config,
    );
    let mcp_router = Router::new()
        .nest_service("/mcp", mcp)
        .route_layer(middleware::from_fn(owner_only));
    Router::new()
        .merge(mcp_router)
        .route(
            "/health",
            get(|| async { Json(json!({"service":"layeragent","protocol_version":1})) }),
        )
        .route("/v1/sessions", get(sessions).post(create_session))
        .route("/v1/sessions/{id}/turns", post(run))
        .route("/v1/sessions/{id}/events", get(events))
        .route(
            "/v1/sessions/{id}/artifacts/{hash}",
            get(tasks::session_artifact),
        )
        .route("/v1/sessions/{id}/ws", get(websocket))
        .route("/v1/tasks", get(tasks::list).post(tasks::create))
        .route("/v1/tasks/{id}", get(tasks::get))
        .route("/v1/tasks/{id}/events", get(tasks::events))
        .route("/v1/tasks/{id}/events/stream", get(tasks::stream))
        .route("/v1/tasks/{id}/cancel", post(tasks::cancel))
        .route("/v1/tasks/{id}/correct", post(tasks::correct))
        .route("/v1/tasks/{id}/corrections", post(tasks::correct))
        .route("/v1/tasks/{id}/approvals/{approval}", post(tasks::approve))
        .route("/v1/tasks/{id}/artifacts/{hash}", get(tasks::task_artifact))
        .route("/v1/tasks/{id}/metrics", get(tasks::metrics))
        .route("/v1/artifacts/{hash}", get(tasks::artifact))
        .route("/v1/connections", get(connections::connections))
        .route("/v1/remote/jobs", get(remote_view::remote_jobs))
        .route("/v1/connections/{id}/{action}", post(connections::action))
        .route("/v1/usage", get(tasks::usage))
        .route("/v1/mcp/capabilities", get(capabilities))
        .route("/v1/jobs/{id}/cancel", post(cancel))
        .route("/v1/approvals/{id}", post(approve))
        .route(
            "/v1/config",
            get(config).route_layer(middleware::from_fn(owner_only)),
        )
        .route(
            "/v1/config/reload",
            post(reload).route_layer(middleware::from_fn(owner_only)),
        )
        .route("/v1/shutdown", post(shutdown))
        .route("/v1/models", get(models))
        .route("/v1/inbox", get(inbox_status))
        .route("/v1/memory/search", post(search))
        .route("/v1/schema", get(schema))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .route(
            "/webhooks/{channel}",
            get(verify_subscription).post(webhook),
        )
        .layer(DefaultBodyLimit::max(1_048_576))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(1_048_576))
        .layer(middleware::from_fn_with_state(state.clone(), ingress))
        .with_state(state)
}
async fn ingress(State(state): State<ApiState>, request: Request, next: Next) -> Response {
    let permit = match state.ingress.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return admission::limited_response(StatusCode::SERVICE_UNAVAILABLE),
    };
    match tokio::time::timeout(std::time::Duration::from_secs(30), next.run(request)).await {
        Ok(response) => admission::hold_response(response, permit),
        Err(_) => StatusCode::REQUEST_TIMEOUT.into_response(),
    }
}
async fn owner_only(request: Request, next: Next) -> Response {
    if request
        .extensions()
        .get::<Identity>()
        .is_some_and(|i| i.owner && !i.read_only)
    {
        next.run(request).await
    } else {
        StatusCode::FORBIDDEN.into_response()
    }
}
async fn authenticate(State(state): State<ApiState>, mut request: Request, next: Next) -> Response {
    let Ok((config, authority)) = state.engine.authority_snapshot().await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let presented = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    let ipc_allowed = state
        .ipc_token
        .as_deref()
        .is_none_or(|expected| constant_eq(presented, expected));
    let identity = if state.ipc {
        ipc_allowed.then(|| Identity {
            scope: Scope::local(),
            read_only: false,
            owner: true,
            workspaces: None,
            authority: authority.clone(),
        })
    } else {
        let token = request
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .unwrap_or("");
        let mut identity = None;
        if let Ok(expected) = std::env::var(&config.gateway.token_env)
            && constant_eq(token, &expected)
        {
            identity = Some(Identity {
                scope: Scope::local(),
                read_only: false,
                owner: true,
                workspaces: None,
                authority: authority.clone(),
            });
        }
        for access in &config.access {
            if let Ok(expected) = std::env::var(&access.token_env)
                && constant_eq(token, &expected)
            {
                if let Some(existing) = &identity {
                    // One principal may have several platform bindings with identical
                    // privileges. Distinct identities sharing a bearer are ambiguous.
                    if existing.owner
                        || existing.scope.tenant != access.tenant
                        || existing.scope.principal != access.principal
                        || existing.read_only != access.read_only
                    {
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                    continue;
                }
                identity = Some(Identity {
                    scope: Scope {
                        tenant: access.tenant.clone(),
                        principal: access.principal.clone(),
                    },
                    read_only: access.read_only,
                    owner: false,
                    workspaces: Some(access.workspaces.clone()),
                    authority: authority.clone(),
                });
            }
        }
        identity
    };
    let Some(identity) = identity else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if identity.read_only && request.method() != axum::http::Method::GET {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !same_origin(request.headers()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let lease = match state.admission.request(&identity.scope) {
        Ok(lease) => lease,
        Err(status) => return admission::limited_response(status),
    };
    let stream_lease = if request.uri().path().starts_with("/mcp") {
        match state.admission.stream(&identity.scope) {
            Ok(lease) => Some(lease),
            Err(status) => return admission::limited_response(status),
        }
    } else {
        None
    };
    request.extensions_mut().insert(identity);
    match tokio::time::timeout(std::time::Duration::from_secs(30), next.run(request)).await {
        Ok(response) => {
            let response = admission::hold_response(response, lease);
            if let Some(stream_lease) = stream_lease {
                admission::hold_response(response, stream_lease)
            } else {
                response
            }
        }
        Err(_) => StatusCode::REQUEST_TIMEOUT.into_response(),
    }
}
// Browser callers must originate from this server. Proxy deployments must preserve Host.
fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get("origin") else {
        return true;
    };
    let Some(host) = headers.get("host").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Ok(origin) = origin
        .to_str()
        .ok()
        .and_then(|v| reqwest::Url::parse(v).ok())
        .ok_or(())
    else {
        return false;
    };
    let Ok(expected) = reqwest::Url::parse(&format!("{}://{host}", origin.scheme())) else {
        return false;
    };
    matches!(origin.scheme(), "http" | "https")
        && origin.username().is_empty()
        && origin.password().is_none()
        && origin.path() == "/"
        && origin.query().is_none()
        && origin.fragment().is_none()
        && origin.host_str() == expected.host_str()
        && origin.port_or_known_default() == expected.port_or_known_default()
}
fn constant_eq(a: &str, b: &str) -> bool {
    !a.is_empty() && !b.is_empty() && a.as_bytes().ct_eq(b.as_bytes()).into()
}
async fn sessions(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
) -> ApiResult<Json<Value>> {
    Ok(Json(json!(
        s.engine
            .store(&i.scope)
            .await?
            .sessions(&i.scope.principal)
            .await?
            .into_iter()
            .filter(|session| i.permits_workspace(&session.workspace))
            .collect::<Vec<_>>()
    )))
}
#[derive(Deserialize)]
struct NewSession {
    workspace: String,
}
async fn create_session(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
    Json(r): Json<NewSession>,
) -> ApiResult<Json<Value>> {
    let path = std::path::Path::new(&r.workspace).canonicalize()?;
    if !(path.is_dir() && path.parent().is_some() && !s.engine.home.starts_with(&path)) {
        return Err(anyhow::anyhow!("invalid workspace").into());
    }
    if let Some(roots) = &i.workspaces
        && !roots
            .iter()
            .filter_map(|p| p.canonicalize().ok())
            .any(|root| path.starts_with(root))
    {
        return Err(anyhow::anyhow!("workspace is not authorized for this principal").into());
    }
    Ok(Json(json!(
        s.engine
            .store(&i.scope)
            .await?
            .create_session(&i.scope.principal, &path)
            .await?
    )))
}
#[derive(Deserialize)]
struct TurnInput {
    prompt: String,
    model: Option<String>,
}
async fn run(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
    Path(id): Path<String>,
    Json(r): Json<TurnInput>,
) -> ApiResult<Response> {
    let stream_lease = match s.admission.stream(&i.scope) {
        Ok(lease) => lease,
        Err(status) => return Ok(admission::limited_response(status)),
    };
    let job_lease = match s.admission.job(&i.scope) {
        Ok(lease) => lease,
        Err(status) => return Ok(admission::limited_response(status)),
    };
    let store = s.engine.store(&i.scope).await?;
    i.require_workspace(&store.session(&id, &i.scope.principal).await?.workspace)?;
    let job = new_id();
    let authority = i.authority.clone();
    let cancel = authority.child_token();
    s.jobs
        .lock()
        .map_err(|_| anyhow::anyhow!("job lock poisoned"))?
        .insert(job.clone(), (i.scope.clone(), cancel.clone()));
    let mut rx = s.engine.events.subscribe();
    let engine = s.engine.clone();
    let scope = i.scope.clone();
    let session = id.clone();
    let jobs = s.jobs.clone();
    let job_key = job.clone();
    let task = tokio::spawn(async move {
        let _job_lease = job_lease;
        let _registration = JobRegistration { jobs, id: job_key };
        engine
            .run(
                scope,
                RunRequest {
                    session_id: Some(session),
                    workspace: None,
                    prompt: r.prompt,
                    model: r.model,
                },
                cancel,
            )
            .await
    });
    let stream = async_stream::stream! {
        yield Ok::<_,Infallible>(SseEvent::default().event("job_started").data(json!({"job_id":job,"session_id":id}).to_string()));
        tokio::pin!(task);
        loop{tokio::select!{
            _=authority.cancelled()=>{yield Ok(SseEvent::default().event("error").data("{\"error\":\"authority changed; reconnect to the session\"}"));break;},
            result=&mut task=>{match result{Ok(Ok(value))=>yield Ok(SseEvent::default().event("result").data(serde_json::to_string(&value).unwrap_or_default())),Ok(Err(error))=>yield Ok(SseEvent::default().event("error").data(json!({"error":redact_text(&error.to_string())}).to_string())),Err(_)=>yield Ok(SseEvent::default().event("error").data("{\"error\":\"runtime task failed\"}"))}break;},
            event=rx.recv()=>match event{Ok((scope,event)) if scope==i.scope && event.session_id==id=>{yield Ok(SseEvent::default().id(event.seq.to_string()).event(&event.kind).data(serde_json::to_string(&event).unwrap_or_default()));},Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>yield Ok(SseEvent::default().event("resync_required").data("{}")),Err(tokio::sync::broadcast::error::RecvError::Closed)=>break,_=>{}}
        }}
    };
    Ok(admission::hold_response(
        Sse::new(stream)
            .keep_alive(KeepAlive::default())
            .into_response(),
        stream_lease,
    ))
}
#[derive(Deserialize)]
struct EventQuery {
    #[serde(default)]
    after: i64,
}
async fn events(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
    Path(id): Path<String>,
    Query(q): Query<EventQuery>,
) -> ApiResult<Json<Value>> {
    let store = s.engine.store(&i.scope).await?;
    i.require_workspace(&store.session(&id, &i.scope.principal).await?.workspace)?;
    Ok(Json(json!(store.events(&id, q.after).await?)))
}
async fn websocket(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let session = s
        .engine
        .store(&i.scope)
        .await?
        .session(&id, &i.scope.principal)
        .await?;
    i.require_workspace(&session.workspace)?;
    let stream_lease = match s.admission.stream(&i.scope) {
        Ok(lease) => lease,
        Err(status) => return Ok(admission::limited_response(status)),
    };
    let authority = i.authority.clone();
    Ok(ws.max_message_size(65_536).max_frame_size(65_536).on_upgrade(move |mut socket| async move {
        let _stream_lease = stream_lease;
        let mut events = s.engine.events.subscribe();
        let lifetime = tokio::time::sleep(std::time::Duration::from_secs(3600));
        tokio::pin!(lifetime);
        loop {
            let outgoing = tokio::select! {
                _ = &mut lifetime => break,
                _ = authority.cancelled() => break,
                message = socket.recv() => match message {
                    Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_))) => continue,
                    _ => break,
                },
                event = events.recv() => match event {
                    Ok((scope, event)) if scope == i.scope && event.session_id == id =>
                        WsMessage::Text(serde_json::to_string(&event).unwrap_or_default().into()),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) =>
                        WsMessage::Text("{\"kind\":\"resync_required\"}".into()),
                    Err(_) => break,
                    _ => continue,
                }
            };
            // A peer which stops reading must not pin a slot or delay authority revocation.
            tokio::select! {
                _ = authority.cancelled() => break,
                _ = &mut lifetime => break,
                sent = tokio::time::timeout(std::time::Duration::from_secs(10), socket.send(outgoing)) =>
                    if !matches!(sent, Ok(Ok(()))) { break; },
            }
        }
    }))
}
async fn cancel(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let jobs = s
        .jobs
        .lock()
        .map_err(|_| anyhow::anyhow!("job lock poisoned"))?;
    if let Some((scope, token)) = jobs.get(&id) {
        if scope != &i.scope {
            return Err(anyhow::anyhow!("job inaccessible").into());
        }
        token.cancel();
    } else {
        s.engine.cancel(&i.scope, &id)?;
    }
    Ok(Json(json!({"cancelled":true})))
}
#[derive(Deserialize)]
struct Approval {
    allow: bool,
    #[serde(default)]
    expected_revision: Option<i64>,
    #[serde(default)]
    arguments_hash: Option<String>,
}
async fn approve(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
    Path(id): Path<String>,
    Json(r): Json<Approval>,
) -> ApiResult<Json<Value>> {
    let (Some(expected_revision), Some(arguments_hash)) = (r.expected_revision, r.arguments_hash)
    else {
        return Err(ApiError::conflict(anyhow::anyhow!(
            "approval decision requires expected_revision and arguments_hash; read the task again"
        )));
    };
    let decision = ApprovalDecision {
        expected_revision,
        arguments_hash,
        allow: r.allow,
    };
    s.engine
        .approve(&i.scope, None, &id, &decision)
        .await
        .map_err(|error| {
            if error.to_string().contains("inaccessible") {
                ApiError::forbidden(error)
            } else {
                ApiError::conflict(error)
            }
        })?;
    Ok(Json(json!({"recorded":true})))
}
async fn capabilities(State(s): State<ApiState>) -> Json<Value> {
    // The project-file MCP adapter exposes tools and paginated resources. It
    // never exposes prompts, templates or notifications.
    let _ = &s;
    Json(json!({
        "resources": true,
        "tools": true,
        "prompts": false,
        "templates": false,
        "notifications": false,
    }))
}
async fn config(State(s): State<ApiState>) -> Json<Value> {
    Json(
        json!({"revision":s.engine.config().await.revision(),"config":s.engine.config().await.as_ref()}),
    )
}
async fn reload(State(s): State<ApiState>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({"changed":s.engine.reload().await?})))
}
async fn shutdown(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
) -> ApiResult<Json<Value>> {
    if !i.owner {
        return Err(anyhow::anyhow!("owner access required").into());
    }
    s.engine.stop();
    Ok(Json(json!({"stopping":true})))
}
async fn models(State(s): State<ApiState>) -> Json<Value> {
    Json(json!(s.engine.config().await.models))
}
async fn inbox_status(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
) -> ApiResult<Json<Value>> {
    Ok(Json(json!(
        s.engine
            .store(&i.scope)
            .await?
            .inbox_status(&i.scope.principal)
            .await?
    )))
}
#[derive(Deserialize)]
struct Search {
    workspace: String,
    query: String,
}
async fn search(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
    Json(r): Json<Search>,
) -> ApiResult<Json<Value>> {
    i.require_workspace(&r.workspace)?;
    Ok(Json(json!(
        s.engine
            .store(&i.scope)
            .await?
            .search(&i.scope.principal, &r.workspace, &r.query, 10)
            .await?
    )))
}
async fn schema() -> Json<Value> {
    Json(
        json!({"config":layeragent_config::Config::schema(),"request":schemars_schema(),"run_request":schemars::schema_for!(RunRequest),"event":schemars::schema_for!(layeragent_protocol::Event)}),
    )
}
fn schemars_schema() -> Value {
    json!({"protocol_version":1,"events":["job_started","turn_started","attempt_started","text_delta","attempt_discarded","tool_started","tool_output","tool_completed","approval_required","workspace_diff","turn_completed","failed","interrupted"]})
}

async fn verify_subscription(
    State(s): State<ApiState>,
    Path(name): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let config = s.engine.config().await;
    let Some(channel) = config.channels.get(&name).filter(|c| c.kind == "whatsapp") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let expected = channel
        .verify_token_env
        .as_deref()
        .and_then(|key| std::env::var(key).ok());
    match (
        expected,
        query.get("hub.verify_token"),
        query.get("hub.challenge"),
    ) {
        (Some(expected), Some(received), Some(challenge))
            if !expected.is_empty()
                && expected.as_bytes().ct_eq(received.as_bytes()).into()
                && query
                    .get("hub.mode")
                    .is_some_and(|mode| mode == "subscribe")
                && challenge.len() <= 1024 =>
        {
            challenge.clone().into_response()
        }
        _ => StatusCode::FORBIDDEN.into_response(),
    }
}

async fn webhook(
    State(s): State<ApiState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> ApiResult<Json<Value>> {
    let config = s.engine.config().await;
    let channel = config
        .channels
        .get(&name)
        .context("unknown channel")?
        .clone();
    let secret = channel
        .secret_env
        .as_ref()
        .and_then(|e| std::env::var(e).ok())
        .context("webhook verification secret is required")?;
    verify_webhook(&channel.kind, &headers, &body, &secret)?;
    let raw: Value = serde_json::from_slice(&body)?;
    if channel.kind == "slack" && raw["type"] == "url_verification" {
        return Ok(Json(json!({"challenge":raw["challenge"]})));
    }
    let events = normalize(&channel.kind, &raw)?;
    for event in events {
        if event.is_bot {
            continue;
        }
        let access = config
            .access
            .iter()
            .find(|a| {
                a.platform.as_deref() == Some(&name)
                    && a.sender_id.as_deref() == Some(&event.sender_id)
                    && a.channel_id
                        .as_ref()
                        .is_none_or(|id| id == &event.channel_id)
            })
            .context("sender is not authorized")?;
        let scope = Scope {
            tenant: access.tenant.clone(),
            principal: access.principal.clone(),
        };
        if access.read_only {
            continue;
        }
        let store = s.engine.store(&scope).await?;
        store
            .enqueue_inbox(
                &name,
                &event.external_id,
                &scope.principal,
                &event.channel_id,
                &redact_text(&event.text),
            )
            .await?;
    }
    Ok(Json(json!({"accepted":true})))
}
fn name_key(platform: &str, channel: &str) -> String {
    format!("{platform}:{channel}")
}
pub fn start_channel_workers(engine: Arc<Engine>) {
    channels::start(engine.clone());
    tokio::spawn(async move {
        let slots = Arc::new(tokio::sync::Semaphore::new(16));
        loop {
            if engine.shutdown.is_cancelled() {
                break;
            }
            let config = engine.config().await;
            let tenants = config
                .access
                .iter()
                .filter(|a| a.platform.is_some())
                .map(|a| a.tenant.clone())
                .collect::<std::collections::BTreeSet<_>>();
            for tenant in tenants {
                let scope = Scope {
                    tenant: tenant.clone(),
                    principal: "owner".into(),
                };
                let Ok(store) = engine.store(&scope).await else {
                    continue;
                };
                for _ in 0..16 {
                    let Ok(permit) = slots.clone().try_acquire_owned() else {
                        break;
                    };
                    let item = match store.claim_inbox().await {
                        Ok(Some(v)) => v,
                        _ => break,
                    };
                    let e = engine.clone();
                    let db = store.clone();
                    let tenant = tenant.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        let id = item["id"].as_i64().unwrap_or(0);
                        let principal = item["principal"].as_str().unwrap_or("");
                        let name = item["channel"].as_str().unwrap_or("");
                        let destination = item["destination"].as_str().unwrap_or("");
                        let cfg = e.config().await;
                        let response = async {
                            let access = cfg
                                .access
                                .iter()
                                .find(|a| {
                                    a.tenant == tenant
                                        && a.principal == principal
                                        && a.platform.as_deref() == Some(name)
                                        && !a.read_only
                                })
                                .context("channel authority was revoked")?;
                            let channel = cfg.channels.get(name).context("channel removed")?;
                            let workspace = channel.workspace.canonicalize()?;
                            anyhow::ensure!(
                                access
                                    .workspaces
                                    .iter()
                                    .filter_map(|p| p.canonicalize().ok())
                                    .any(|p| workspace.starts_with(p)),
                                "channel workspace is not authorized"
                            );
                            let key = name_key(name, destination);
                            let session = if let Some(id) = db.binding(&key, principal).await? {
                                db.session(&id, principal).await?
                            } else {
                                let session = db
                                    .create_session(principal, &channel.workspace.canonicalize()?)
                                    .await?;
                                db.bind(&key, principal, &session.id).await?;
                                session
                            };
                            anyhow::ensure!(
                                std::path::Path::new(&session.workspace).canonicalize()?
                                    == workspace,
                                "channel workspace changed; reset the stored conversation binding"
                            );
                            e.run(
                                Scope {
                                    tenant,
                                    principal: principal.into(),
                                },
                                RunRequest {
                                    session_id: Some(session.id),
                                    workspace: None,
                                    prompt: item["body"].as_str().unwrap_or("").into(),
                                    model: None,
                                },
                                e.shutdown.child_token(),
                            )
                            .await
                        }
                        .await;
                        let (error, text) = match response {
                            Ok(r) => (None, r.text),
                            Err(error) => {
                                let message = redact_text(&error.to_string());
                                (
                                    Some(message.clone()),
                                    format!("LayerAgent could not complete this job: {message}"),
                                )
                            }
                        };
                        let limit = if cfg.channels.get(name).is_some_and(|c| c.kind == "discord") {
                            2000
                        } else {
                            4000
                        };
                        let _ = db
                            .finish_inbox(
                                id,
                                principal,
                                name,
                                destination,
                                chunks(&text, limit),
                                error,
                            )
                            .await;
                    });
                }
                for _ in 0..16 {
                    let item = match store.claim_outbox().await {
                        Ok(Some(v)) => v,
                        _ => break,
                    };
                    let id = item["id"].as_str().unwrap_or("");
                    let attempts = item["attempts"].as_i64().unwrap_or(0);
                    let current = engine.config().await;
                    let authorized = current.access.iter().any(|a| {
                        a.tenant == tenant
                            && a.principal == item["principal"].as_str().unwrap_or("")
                            && a.platform.as_deref() == item["channel"].as_str()
                            && !a.read_only
                    });
                    let outcome = if !authorized {
                        Delivery::Rejected("channel authority was revoked".into())
                    } else if let Some(channel) =
                        current.channels.get(item["channel"].as_str().unwrap_or(""))
                    {
                        deliver_one(
                            channel,
                            item["destination"].as_str().unwrap_or(""),
                            item["body"].as_str().unwrap_or(""),
                        )
                        .await
                    } else {
                        Delivery::Rejected("channel removed".into())
                    };
                    let (status, delay, error) = match outcome {
                        Delivery::Sent => ("sent", 0, None),
                        Delivery::Retry(delay) if attempts < 5 => (
                            "pending",
                            delay,
                            Some("platform rate limited the delivery".into()),
                        ),
                        Delivery::Unknown => (
                            "unknown",
                            0,
                            Some("delivery outcome is uncertain; automatic retry disabled".into()),
                        ),
                        Delivery::Rejected(error) => ("failed", 0, Some(error)),
                        _ => ("failed", 0, Some("delivery retry budget exhausted".into())),
                    };
                    let _ = store.finish_outbox(id, status, delay, error).await;
                }
            }
            tokio::select! {_=engine.shutdown.cancelled()=>break,_=tokio::time::sleep(std::time::Duration::from_millis(500))=>{}}
        }
    });
}
enum Delivery {
    Sent,
    Retry(i64),
    Unknown,
    Rejected(String),
}
async fn deliver_one(
    channel: &layeragent_config::Channel,
    destination: &str,
    text: &str,
) -> Delivery {
    if channel.kind == "signal" && channel.mode == "native" {
        return match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            channels::signal_send(channel, destination, text),
        )
        .await
        {
            Ok(Ok(())) => Delivery::Sent,
            _ => Delivery::Unknown,
        };
    }
    let token = match std::env::var(&channel.token_env) {
        Ok(t) => t,
        Err(_) => return Delivery::Rejected("channel credential unavailable".into()),
    };
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(c) => c,
        Err(_) => return Delivery::Rejected("unable to initialize delivery client".into()),
    };
    let request = match channel.kind.as_str() {
        "telegram" => client
            .post(format!("https://api.telegram.org/bot{token}/sendMessage"))
            .json(&json!({"chat_id":destination,"text":text})),
        "slack" => client
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(&token)
            .json(&json!({"channel":destination,"text":text})),
        "discord" => client
            .post(format!(
                "https://discord.com/api/v10/channels/{destination}/messages"
            ))
            .header("authorization", format!("Bot {token}"))
            .json(&json!({"content":text})),
        "matrix" => {
            let Some(base) = &channel.endpoint else {
                return Delivery::Rejected("Matrix homeserver missing".into());
            };
            let Ok(mut url) = reqwest::Url::parse(base) else {
                return Delivery::Rejected("invalid Matrix homeserver".into());
            };
            if let Ok(mut path) = url.path_segments_mut() {
                path.pop_if_empty().extend([
                    "_matrix",
                    "client",
                    "v3",
                    "rooms",
                    destination,
                    "send",
                    "m.room.message",
                    &new_id(),
                ]);
            }
            client
                .put(url)
                .bearer_auth(&token)
                .json(&json!({"msgtype":"m.text","body":text}))
        }
        "whatsapp" => {
            let Some(url) = &channel.endpoint else {
                return Delivery::Rejected("WhatsApp endpoint missing".into());
            };
            client.post(url).bearer_auth(&token).json(&json!({"messaging_product":"whatsapp","to":destination,"type":"text","text":{"body":text}}))
        }
        _ => {
            let Some(url) = &channel.endpoint else {
                return Delivery::Rejected("bridge endpoint missing".into());
            };
            client
                .post(url)
                .bearer_auth(&token)
                .json(&json!({"destination":destination,"text":text}))
        }
    };
    match request.send().await {
        Err(_) => Delivery::Unknown,
        Ok(response) => {
            if response.status().as_u16() == 429 {
                let retry = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(5)
                    .clamp(1, 3600);
                return Delivery::Retry(retry * 1000);
            }
            if response.status().is_server_error() {
                return Delivery::Unknown;
            }
            if !response.status().is_success() {
                return Delivery::Rejected(format!("delivery HTTP {}", response.status().as_u16()));
            }
            if channel.kind == "slack" {
                match response.json::<Value>().await {
                    Ok(v) if v["ok"] == true => Delivery::Sent,
                    _ => Delivery::Rejected("Slack did not acknowledge delivery".into()),
                }
            } else {
                Delivery::Sent
            }
        }
    }
}
pub fn verify_webhook(kind: &str, headers: &HeaderMap, body: &[u8], secret: &str) -> Result<()> {
    let header = |key: &str| headers.get(key).and_then(|v| v.to_str().ok()).unwrap_or("");
    match kind {
        "telegram" => anyhow::ensure!(
            constant_eq(header("x-telegram-bot-api-secret-token"), secret),
            "invalid Telegram secret"
        ),
        "slack" => {
            let timestamp = header("x-slack-request-timestamp");
            let time = timestamp.parse::<i64>()?;
            anyhow::ensure!(
                (layeragent_protocol::now_ms() / 1000 - time).abs() <= 300,
                "expired Slack signature"
            );
            let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())?;
            mac.update(format!("v0:{timestamp}:").as_bytes());
            mac.update(body);
            let signature = hex::decode(
                header("x-slack-signature")
                    .strip_prefix("v0=")
                    .context("missing Slack signature")?,
            )?;
            mac.verify_slice(&signature)
                .map_err(|_| anyhow::anyhow!("invalid Slack signature"))?;
        }
        "whatsapp" | "webhook" => {
            let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())?;
            mac.update(body);
            let signature = hex::decode(
                header("x-hub-signature-256")
                    .strip_prefix("sha256=")
                    .context("missing HMAC signature")?,
            )?;
            mac.verify_slice(&signature)
                .map_err(|_| anyhow::anyhow!("invalid HMAC signature"))?;
        }
        "signal" | "matrix" => anyhow::ensure!(
            constant_eq(
                header("authorization")
                    .strip_prefix("Bearer ")
                    .unwrap_or(""),
                secret
            ),
            "invalid bridge credential"
        ),
        _ => anyhow::bail!("this channel requires its native signature adapter"),
    }
    Ok(())
}
pub fn normalize(kind: &str, v: &Value) -> Result<Vec<PlatformEvent>> {
    let event = match kind {
        "telegram" => {
            let m = &v["message"];
            PlatformEvent {
                external_id: v["update_id"].to_string(),
                platform: kind.into(),
                channel_id: m["chat"]["id"].to_string(),
                sender_id: m["from"]["id"].to_string(),
                is_bot: m["from"]["is_bot"].as_bool().unwrap_or(false),
                text: m["text"].as_str().unwrap_or("").into(),
                attachments: vec![],
            }
        }
        "discord" => PlatformEvent {
            external_id: v["id"]
                .as_str()
                .context("Discord message ID required")?
                .into(),
            platform: kind.into(),
            channel_id: v["channel_id"]
                .as_str()
                .context("Discord channel ID required")?
                .into(),
            sender_id: v["author"]["id"]
                .as_str()
                .context("Discord author required")?
                .into(),
            is_bot: v["author"]["bot"].as_bool().unwrap_or(false),
            text: v["content"].as_str().unwrap_or("").into(),
            attachments: vec![],
        },
        "slack" => {
            let m = &v["event"];
            PlatformEvent {
                external_id: v["event_id"].as_str().context("event id required")?.into(),
                platform: kind.into(),
                channel_id: m["channel"].as_str().unwrap_or("").into(),
                sender_id: m["user"].as_str().unwrap_or("").into(),
                is_bot: m.get("bot_id").is_some(),
                text: m["text"].as_str().unwrap_or("").into(),
                attachments: vec![],
            }
        }
        "whatsapp" => {
            let mut events = vec![];
            for entry in v["entry"].as_array().into_iter().flatten() {
                for change in entry["changes"].as_array().into_iter().flatten() {
                    for m in change["value"]["messages"].as_array().into_iter().flatten() {
                        events.push(PlatformEvent {
                            external_id: m["id"].as_str().context("message id required")?.into(),
                            platform: kind.into(),
                            channel_id: m["from"].as_str().context("sender required")?.into(),
                            sender_id: m["from"].as_str().context("sender required")?.into(),
                            is_bot: false,
                            text: m["text"]["body"].as_str().unwrap_or("").into(),
                            attachments: vec![],
                        });
                    }
                }
            }
            return Ok(events);
        }
        _ => serde_json::from_value(v.clone())?,
    };
    if event.text.is_empty() {
        Ok(vec![])
    } else {
        Ok(vec![event])
    }
}
pub fn chunks(text: &str, limit: usize) -> Vec<String> {
    let limit = limit.max(64);
    let mut result = vec![];
    let mut current = String::new();
    let mut units = 0;
    let mut fence = String::new();
    for line in text.split_inclusive('\n') {
        let marker = line.trim_start().starts_with("```");
        let closing = marker && !fence.is_empty();
        for character in line.chars() {
            let width = character.len_utf16();
            let reserve = if fence.is_empty() { 0 } else { 4 };
            if units + width + reserve > limit {
                if !fence.is_empty() {
                    current.push_str("\n```");
                }
                result.push(std::mem::take(&mut current));
                if !fence.is_empty() {
                    current.push_str(&fence);
                    current.push('\n');
                }
                units = current.encode_utf16().count();
            }
            current.push(character);
            units += width;
        }
        if marker {
            fence = if closing {
                String::new()
            } else {
                line.trim().chars().take(40).collect()
            };
        }
    }
    if !current.is_empty() {
        if !fence.is_empty() && !current.trim_end().ends_with("```") {
            if units + 4 > limit {
                result.push(std::mem::take(&mut current));
                current = fence.clone();
                current.push('\n');
            }
            current.push_str("\n```");
        }
        result.push(current);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_origin_must_match_exact_authority() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "host",
            "localhost:9473".parse().expect("valid fixture host"),
        );
        assert!(same_origin(&headers));
        for origin in [
            "http://evil.example",
            "null",
            "http://localhost:1234",
            "http://user@localhost:9473",
            "http://localhost:9473/path",
        ] {
            headers.insert(
                "origin",
                origin.parse().expect("valid fixture Origin header"),
            );
            assert!(!same_origin(&headers), "{origin}");
        }
        headers.insert(
            "origin",
            "http://localhost:9473"
                .parse()
                .expect("valid fixture Origin"),
        );
        assert!(same_origin(&headers));
    }
    #[test]
    fn rejects_forged_webhooks() {
        assert!(verify_webhook("telegram", &HeaderMap::new(), b"{}", "secret").is_err());
        assert!(verify_webhook("slack", &HeaderMap::new(), b"{}", "secret").is_err());
    }
    #[test]
    fn unicode_chunks_respect_limits() {
        let text = "🦀".repeat(9000);
        assert!(
            chunks(&text, 2000)
                .iter()
                .all(|s| s.chars().count() <= 2000)
        );
    }
}

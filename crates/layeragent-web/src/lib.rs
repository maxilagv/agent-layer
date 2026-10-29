//! Loopback-only web surface for the shared LayerAgent frontend.
//!
//! The server binds to a loopback address, authenticates the browser with a
//! single-use local code, keeps the session in an HttpOnly cookie and
//! requires a CSRF token for every mutation. Requests are authorized again on
//! the server before they reach a task or an artifact.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Component, Path as FsPath, PathBuf},
    sync::Arc,
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

pub mod daemon;
pub mod session;

use daemon::{DaemonClient, safe_hash, safe_identifier};
use session::{Session, SessionStore};

/// Content security policy. The frontend loads only same-origin resources.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data: blob:; font-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

const SESSION_COOKIE: &str = "layeragent_session";
const CSRF_HEADER: &str = "x-layeragent-csrf";

/// Configuration of the local web surface.
#[derive(Clone, Debug)]
pub struct WebConfig {
    /// Loopback address to bind. Non-loopback addresses are refused.
    pub bind: SocketAddr,
    /// Runtime home of the daemon.
    pub home: PathBuf,
    /// Daemon profile.
    pub profile: String,
    /// Directory with the built frontend (`frontend/dist`).
    pub assets: PathBuf,
    /// Idle lifetime of an interface session.
    pub session_ttl: Duration,
    /// Lifetime of a single-use access code.
    pub code_ttl: Duration,
}

impl WebConfig {
    pub fn new(bind: SocketAddr, home: PathBuf, profile: String, assets: PathBuf) -> Self {
        Self {
            bind,
            home,
            profile,
            assets,
            session_ttl: Duration::from_secs(12 * 60 * 60),
            code_ttl: Duration::from_secs(15 * 60),
        }
    }
}

#[derive(Clone)]
struct WebState {
    daemon: DaemonClient,
    sessions: Arc<SessionStore>,
    assets: PathBuf,
    /// Hosts the browser may address this server with.
    allowed_hosts: Arc<Vec<String>>,
    /// Origin the browser may present.
    allowed_origin: String,
}

/// Builds the router and issues the single-use access code.
pub fn build(config: &WebConfig) -> Result<(Router, String)> {
    ensure!(
        config.bind.ip().is_loopback(),
        "the web surface only binds to loopback addresses"
    );
    let daemon = DaemonClient::new(&config.home, &config.profile)?;
    let port = config.bind.port();
    let allowed_hosts = Arc::new(vec![
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ]);
    let state = WebState {
        daemon,
        sessions: Arc::new(SessionStore::new(
            config.code_ttl.as_millis() as i64,
            config.session_ttl.as_millis() as i64,
        )),
        assets: config.assets.clone(),
        allowed_hosts,
        allowed_origin: format!("http://127.0.0.1:{port}"),
    };
    let code = state.sessions.issue_code().code;
    Ok((router(state), code))
}

/// Starts the web surface.
///
/// The single-use access code is returned by [`build`] so the operator sees
/// it before the first request can arrive.
pub async fn serve(config: WebConfig, shutdown: CancellationToken) -> Result<()> {
    let (app, _code) = build(&config)?;
    serve_app(app, config.bind, shutdown).await
}

/// Serves an already built application on a loopback address.
pub async fn serve_app(app: Router, bind: SocketAddr, shutdown: CancellationToken) -> Result<()> {
    ensure!(
        bind.ip().is_loopback(),
        "the web surface only binds to loopback"
    );
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("cannot bind {bind}"))?;
    let (stop, _) = broadcast::channel::<()>(4);
    let signal = stop.clone();
    let token = shutdown.clone();
    tokio::spawn(async move {
        token.cancelled().await;
        let _ = signal.send(());
    });
    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(async move {
            let mut receiver = stop.subscribe();
            loop {
                match receiver.recv().await {
                    Ok(()) | Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        })
        .await
        .context("web surface stopped")?;
    Ok(())
}

fn router(state: WebState) -> Router {
    Router::new()
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/session", get(session_state))
        .route("/api/bootstrap", get(bootstrap))
        .route("/api/projects", get(projects))
        .route("/api/sessions", get(sessions))
        .route("/api/sessions/{id}/events", get(session_events))
        .route("/api/tasks", get(tasks).post(create_task))
        .route("/api/tasks/{id}", get(task))
        .route("/api/tasks/{id}/events", get(task_events))
        .route("/api/tasks/{id}/cancel", post(cancel_task))
        .route("/api/tasks/{id}/correct", post(correct_task))
        .route("/api/approvals/{id}", post(approve))
        .route("/api/artifacts/{hash}", get(artifact))
        .route("/api/connections", get(connections))
        .route("/api/connections/{id}/{action}", post(connection_action))
        .route("/api/connections/{id}/action", post(connection_action_body))
        .route("/api/usage", get(usage))
        .route("/api/config", get(config_view))
        .route("/api/models", get(models))
        .route("/api/mcp/capabilities", get(mcp_capabilities))
        .route("/api/events", get(event_stream))
        .fallback(static_asset)
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .layer(tower_http::limit::RequestBodyLimitLayer::new(1_048_576))
}

/// Validates the request origin, the session cookie and the CSRF token, and
/// attaches the security headers to every response.
async fn guard(
    State(state): State<WebState>,
    mut request: axum::extract::Request,
    next: Next,
) -> Response {
    let mut response = match authorize(&state, &request) {
        Ok(session) => {
            if let Some(session) = session {
                request.extensions_mut().insert(session);
            }
            next.run(request).await
        }
        Err(status) => {
            let mut response = (
                status,
                Json(json!({"error": status.canonical_reason().unwrap_or("denied")})),
            )
                .into_response();
            if status == StatusCode::FORBIDDEN {
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
            }
            response
        }
    };
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Checks the request origin, the session cookie and the CSRF token.
///
/// This runs without awaiting so the request body is never borrowed across
/// an await point.
fn authorize(
    state: &WebState,
    request: &axum::extract::Request,
) -> Result<Option<Session>, StatusCode> {
    let headers = request.headers();
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !state.allowed_hosts.iter().any(|allowed| allowed == host) {
        return Err(StatusCode::FORBIDDEN);
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let origin = origin.to_str().unwrap_or_default();
        if origin != state.allowed_origin && origin != format!("http://{host}") {
            return Err(StatusCode::FORBIDDEN);
        }
    }
    let path = request.uri().path();
    // The application shell and its assets are public: they carry no data.
    // Every path below /api needs a session.
    let public = matches!(path, "/api/login" | "/api/session");
    if public || !path.starts_with("/api") {
        return Ok(None);
    }
    let session = cookies(request.headers())
        .and_then(|cookies| cookies.get(SESSION_COOKIE).cloned())
        .and_then(|id| state.sessions.session(&id))
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if request.method() != Method::GET && request.method() != Method::HEAD {
        let presented = headers
            .get(CSRF_HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !constant_eq(presented, &session.csrf) {
            return Err(StatusCode::FORBIDDEN);
        }
    }
    Ok(Some(session))
}

fn constant_eq(a: &str, b: &str) -> bool {
    !a.is_empty() && !b.is_empty() && a.as_bytes().ct_eq(b.as_bytes()).into()
}

fn cookies(headers: &HeaderMap) -> Option<BTreeMap<String, String>> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    let mut map = BTreeMap::new();
    for part in raw.split(';') {
        let part = part.trim();
        if let Some((name, value)) = part.split_once('=') {
            map.insert(name.trim().to_string(), value.trim().to_string());
        }
    }
    Some(map)
}

#[derive(Deserialize)]
struct Login {
    code: String,
}

async fn login(
    State(state): State<WebState>,
    Json(request): Json<Login>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let session = state.sessions.redeem(&request.code).map_err(|error| {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": error.to_string()})),
        )
    })?;
    // A fresh code is shown for the next sign-in, so a second tab can be
    // opened without restarting the server.
    let next = state.sessions.issue_code();
    println!(
        "LayerAgent web: new access code (single use): {}",
        next.code
    );
    let cookie = format!(
        "{SESSION_COOKIE}={}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}",
        session.id,
        state.session_ttl_secs()
    );
    let mut response = Json(json!({
        "authenticated": true,
        "csrf_token": session.csrf,
        "principal": session.principal,
        "tenant": session.tenant,
    }))
    .into_response();
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    Ok(response)
}

async fn logout(State(state): State<WebState>, headers: HeaderMap) -> Response {
    if let Some(id) = cookies(&headers).and_then(|cookies| cookies.get(SESSION_COOKIE).cloned()) {
        // Only the interface session ends. Running work is untouched.
        state.sessions.invalidate(&id);
    }
    let cookie = format!("{SESSION_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
    let mut response = Json(json!({"authenticated": false})).into_response();
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

async fn session_state(State(state): State<WebState>, headers: HeaderMap) -> Json<Value> {
    let session = cookies(&headers)
        .and_then(|cookies| cookies.get(SESSION_COOKIE).cloned())
        .and_then(|id| state.sessions.session(&id));
    match session {
        Some(session) => Json(json!({
            "authenticated": true,
            "csrf_token": session.csrf,
            "principal": session.principal,
            "tenant": session.tenant,
        })),
        None => Json(json!({"authenticated": false})),
    }
}

impl WebState {
    fn session_ttl_secs(&self) -> u64 {
        12 * 60 * 60
    }
}

async fn bootstrap(
    State(state): State<WebState>,
    axum::Extension(session): axum::Extension<Session>,
) -> Response {
    let projects = project_list(&state).await;
    Json(json!({
        "principal": session.principal,
        "tenant": session.tenant,
        "projects": projects,
        "capabilities": {
            "tasks": true,
            "approvals": true,
            "artifacts": true,
            "connections": true,
            "mcp_resources": mcp_resources_available(&state).await,
        },
        // The CSRF token is handed to the browser once. It is kept in memory
        // by the client and never stored.
        "session": {"csrf_token": session.csrf},
    }))
    .into_response()
}

/// Stable identifier of a project directory.
fn project_id(path: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(path.as_bytes()))[..16].to_string()
}

/// Project directories known to the daemon, derived from its sessions.
async fn project_list(state: &WebState) -> Vec<Value> {
    let Ok(reply) = state.daemon.get("/v1/sessions").await else {
        return Vec::new();
    };
    let mut paths: Vec<String> = Vec::new();
    if let Some(items) = reply.body.as_array() {
        for item in items {
            if let Some(workspace) = item["workspace"].as_str()
                && !paths.iter().any(|existing| existing == workspace)
            {
                paths.push(workspace.to_string());
            }
        }
    }
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let name = FsPath::new(&path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone());
            json!({"id": project_id(&path), "path": path, "name": name})
        })
        .collect()
}

async fn projects(State(state): State<WebState>) -> Json<Value> {
    Json(json!(project_list(&state).await))
}

#[derive(Deserialize)]
struct ProjectQuery {
    project: Option<String>,
}

async fn sessions(State(state): State<WebState>, Query(query): Query<ProjectQuery>) -> Response {
    let Ok(reply) = state.daemon.get("/v1/sessions").await else {
        return daemon_unavailable();
    };
    let items = reply.body.as_array().cloned().unwrap_or_default();
    let filtered = match query.project.as_deref() {
        Some(project) if !project.is_empty() => items
            .into_iter()
            .filter(|item| item["workspace"].as_str() == Some(project))
            .collect::<Vec<Value>>(),
        _ => items,
    };
    Json(json!(filtered)).into_response()
}

#[derive(Deserialize)]
struct CursorQuery {
    after: Option<i64>,
    limit: Option<usize>,
}

async fn session_events(
    State(state): State<WebState>,
    Path(id): Path<String>,
    Query(query): Query<CursorQuery>,
) -> Response {
    if !safe_identifier(&id) {
        return bad_request("invalid session identifier");
    }
    let after = query.after.unwrap_or(0).max(0);
    let path = format!("/v1/sessions/{id}/events?after={after}");
    match state.daemon.get(&path).await {
        Ok(reply) => forward(reply),
        Err(_) => daemon_unavailable(),
    }
}

async fn tasks(State(state): State<WebState>, Query(query): Query<CursorQuery>) -> Response {
    let after = query.after.unwrap_or(0).max(0);
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let path = format!("/v1/tasks?after={after}&limit={limit}");
    let Ok(reply) = state.daemon.get(&path).await else {
        return daemon_unavailable();
    };
    // Accept both a bare list and the paged shape.
    let body = match reply.body {
        Value::Array(items) => json!({"tasks": items, "next": Value::Null}),
        other => other,
    };
    (reply.status, Json(body)).into_response()
}

#[derive(Deserialize)]
struct NewTask {
    workspace: String,
    objective: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    executor: Option<String>,
    #[serde(default)]
    idempotency_key: Option<String>,
    #[serde(default = "default_effort")]
    effort: String,
    #[serde(default)]
    bridge: bool,
    #[serde(default = "default_mode")]
    mode: String,
}
fn default_effort() -> String {
    "auto".into()
}
fn default_mode() -> String {
    "normal".into()
}

async fn create_task(State(state): State<WebState>, Json(request): Json<NewTask>) -> Response {
    if request.objective.trim().is_empty() || request.objective.len() > 262_144 {
        return bad_request("objective must contain 1..262144 bytes");
    }
    if request.workspace.trim().is_empty() || request.workspace.len() > 4096 {
        return bad_request("workspace is required");
    }
    let key = request
        .idempotency_key
        .clone()
        .unwrap_or_else(|| session::random_token(16));
    if key.len() > 200 {
        return bad_request("idempotency key is too long");
    }
    let body = json!({
        "workspace": request.workspace,
        "objective": request.objective,
        "session_id": request.session_id,
        "model": request.model,
        "executor": request.executor,
        "effort": request.effort,
        "bridge": request.bridge,
        "mode": request.mode,
        "idempotency_key": key,
    });
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(&key) {
        headers.insert("idempotency-key", value);
    }
    match state
        .daemon
        .post_with_headers("/v1/tasks", body, headers)
        .await
    {
        Ok(reply) => forward(reply),
        Err(_) => daemon_unavailable(),
    }
}

async fn task(State(state): State<WebState>, Path(id): Path<String>) -> Response {
    if !safe_identifier(&id) {
        return bad_request("invalid task identifier");
    }
    match state.daemon.get(&format!("/v1/tasks/{id}")).await {
        Ok(reply) => forward(reply),
        Err(_) => daemon_unavailable(),
    }
}

async fn task_events(
    State(state): State<WebState>,
    Path(id): Path<String>,
    Query(query): Query<CursorQuery>,
) -> Response {
    if !safe_identifier(&id) {
        return bad_request("invalid task identifier");
    }
    let after = query.after.unwrap_or(0).max(0);
    match state
        .daemon
        .get(&format!("/v1/tasks/{id}/events?after={after}"))
        .await
    {
        Ok(reply) => forward(reply),
        Err(_) => daemon_unavailable(),
    }
}

async fn cancel_task(State(state): State<WebState>, Path(id): Path<String>) -> Response {
    if !safe_identifier(&id) {
        return bad_request("invalid task identifier");
    }
    match state
        .daemon
        .post(&format!("/v1/tasks/{id}/cancel"), json!({}))
        .await
    {
        Ok(reply) => forward(reply),
        Err(_) => daemon_unavailable(),
    }
}

#[derive(Deserialize)]
struct Correction {
    instruction: String,
    expected_revision: i64,
}

async fn correct_task(
    State(state): State<WebState>,
    Path(id): Path<String>,
    Json(request): Json<Correction>,
) -> Response {
    if !safe_identifier(&id) {
        return bad_request("invalid task identifier");
    }
    if request.instruction.trim().is_empty() || request.instruction.len() > 65_536 {
        return bad_request("instruction must contain 1..65536 bytes");
    }
    match state
        .daemon
        .post(
            &format!("/v1/tasks/{id}/correct"),
            json!({
                "instruction": request.instruction,
                "expected_revision": request.expected_revision,
            }),
        )
        .await
    {
        // A stale revision reaches the browser as 409 so the client reloads.
        Ok(reply) => forward(reply),
        Err(_) => daemon_unavailable(),
    }
}

#[derive(Deserialize)]
struct Decision {
    expected_revision: i64,
    arguments_hash: String,
    allow: bool,
}

async fn approve(
    State(state): State<WebState>,
    Path(id): Path<String>,
    Json(request): Json<Decision>,
) -> Response {
    if !safe_identifier(&id) {
        return bad_request("invalid approval identifier");
    }
    if !safe_hash(&request.arguments_hash) {
        return bad_request("invalid arguments hash");
    }
    match state
        .daemon
        .post(
            &format!("/v1/approvals/{id}"),
            json!({
                "expected_revision": request.expected_revision,
                "arguments_hash": request.arguments_hash,
                "allow": request.allow,
            }),
        )
        .await
    {
        Ok(reply) => forward(reply),
        Err(_) => daemon_unavailable(),
    }
}

#[derive(Deserialize)]
struct ArtifactQuery {
    offset: Option<usize>,
    limit: Option<usize>,
    session: Option<String>,
    task: Option<String>,
}

async fn artifact(
    State(state): State<WebState>,
    Path(hash): Path<String>,
    Query(query): Query<ArtifactQuery>,
) -> Response {
    if !safe_hash(&hash) {
        return bad_request("invalid artifact hash");
    }
    let offset = query.offset.unwrap_or(0);
    let limit = query.limit.unwrap_or(65_536).clamp(1, 65_536);
    let mut path = format!("/v1/artifacts/{hash}?offset={offset}&limit={limit}");
    if let Some(session) = query.session.as_deref() {
        if !safe_identifier(session) {
            return bad_request("invalid session identifier");
        }
        path.push_str(&format!("&session={session}"));
    }
    if let Some(task) = query.task.as_deref() {
        if !safe_identifier(task) {
            return bad_request("invalid task identifier");
        }
        path.push_str(&format!("&task={task}"));
    }
    match state.daemon.get(&path).await {
        Ok(reply) => forward(reply),
        Err(_) => daemon_unavailable(),
    }
}

async fn connections(State(state): State<WebState>) -> Response {
    let Ok(reply) = state.daemon.get("/v1/connections").await else {
        return daemon_unavailable();
    };
    let mut items = match reply.body {
        Value::Array(items) => items,
        Value::Object(ref map) => map
            .get("connections")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    // The remote bridge appears as a runner connection. Only its public
    // metadata is presented; no pairing key leaves the daemon.
    if let Ok(remote) = state.daemon.get("/v1/remote/jobs").await
        && remote.body["configured"].as_bool() == Some(true)
    {
        let runner = &remote.body["runner"];
        if !runner.is_null() {
            items.push(json!({
                "id": "runner-bridge",
                "kind": "runner",
                "name": runner["runner"].as_str().unwrap_or("runner"),
                "account": runner["account"],
                "projects": [runner["project"]],
                "operations": runner["operations"],
                "status": "configured",
                "revision": runner["policy_revision"],
                "enabled": true,
                "generation": 1,
                "detail": format!(
                    "{} remote job(s) known to this runtime",
                    remote.body["job_count"].as_u64().unwrap_or(0)
                ),
            }));
        }
    }
    Json(json!(items)).into_response()
}

#[derive(Deserialize)]
struct ConnectionAction {
    action: String,
}

async fn connection_action_body(
    State(state): State<WebState>,
    Path(id): Path<String>,
    Json(request): Json<ConnectionAction>,
) -> Response {
    connection_action_inner(state, id, request.action).await
}

async fn connection_action(
    State(state): State<WebState>,
    Path((id, action)): Path<(String, String)>,
) -> Response {
    connection_action_inner(state, id, action).await
}

async fn connection_action_inner(state: WebState, id: String, action: String) -> Response {
    if !safe_identifier(&id) {
        return bad_request("invalid connection identifier");
    }
    if !matches!(action.as_str(), "probe" | "enable" | "disable" | "revoke") {
        return bad_request("invalid connection action");
    }
    match state
        .daemon
        .post(&format!("/v1/connections/{id}/{action}"), json!({}))
        .await
    {
        Ok(reply) => forward(reply),
        Err(_) => daemon_unavailable(),
    }
}

async fn usage(State(state): State<WebState>) -> Response {
    let mut totals = json!({"tasks": 0, "input_tokens": 0, "output_tokens": 0, "cost_usd": 0.0});
    let mut tasks = Vec::new();
    if let Ok(reply) = state.daemon.get("/v1/usage").await
        && let Some(value) = reply.body["totals"].as_object()
    {
        totals = json!(value);
    }
    if let Ok(reply) = state.daemon.get("/v1/tasks?after=0&limit=200").await {
        if let Some(items) = reply.body["tasks"].as_array() {
            tasks = items.clone();
        } else if let Some(items) = reply.body.as_array() {
            tasks = items.clone();
        }
    }
    if totals["tasks"].as_u64().unwrap_or(0) == 0 {
        totals["tasks"] = json!(tasks.len());
    }
    Json(json!({"totals": totals, "tasks": tasks})).into_response()
}

async fn config_view(State(state): State<WebState>) -> Response {
    match state.daemon.get("/v1/config").await {
        Ok(reply) => forward(reply),
        Err(_) => daemon_unavailable(),
    }
}

async fn models(State(state): State<WebState>) -> Response {
    let Ok(reply) = state.daemon.get("/v1/models").await else {
        return daemon_unavailable();
    };
    // The models are a map in the daemon; the browser receives a list.
    let list = match reply.body {
        Value::Object(map) => map
            .into_iter()
            .map(|(name, value)| {
                let mut entry = value.clone();
                if let Some(object) = entry.as_object_mut() {
                    object.entry("alias").or_insert(json!(name));
                }
                entry
            })
            .collect::<Vec<Value>>(),
        Value::Array(items) => items,
        _ => Vec::new(),
    };
    Json(json!(list)).into_response()
}

async fn mcp_resources_available(state: &WebState) -> bool {
    match state.daemon.get("/v1/mcp/capabilities").await {
        Ok(reply) => reply
            .body
            .get("resources")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        Err(_) => false,
    }
}

async fn mcp_capabilities(State(state): State<WebState>) -> Response {
    match state.daemon.get("/v1/mcp/capabilities").await {
        Ok(reply) => forward(reply),
        Err(_) => Json(json!({
            "resources": false,
            "tools": true,
            "prompts": false,
            "notice": "recursos no disponibles",
        }))
        .into_response(),
    }
}

#[derive(Deserialize)]
struct StreamQuery {
    /// Subscription path as used by the shared frontend, for example
    /// `tasks?project=<id>`, `tasks/<id>` or `sessions/<id>`.
    path: Option<String>,
    session: Option<String>,
    task: Option<String>,
    project: Option<String>,
    after: Option<i64>,
}

/// Streams ordered events from a cursor. The client reconnects with the last
/// sequence it observed, so a reload never repeats or skips work.
async fn event_stream(State(state): State<WebState>, Query(query): Query<StreamQuery>) -> Response {
    let request = match stream_target(&query) {
        Ok(target) => target,
        Err(response) => return *response,
    };
    let mut cursor = query.after.unwrap_or(0).max(0);
    let stream = async_stream::stream! {
        let mut counter = cursor;
        let mut seen: std::collections::HashMap<String, (i64, i64)> =
            std::collections::HashMap::new();
        loop {
            match request.fetch(&state, cursor).await {
                Ok(events) => {
                    for event in events {
                        let seq = event["seq"].as_i64().unwrap_or(counter);
                        cursor = cursor.max(seq);
                        counter = counter.max(seq);
                        let payload = json!({
                            "seq": seq,
                            "at": event["at"].clone(),
                            "kind": event["kind"].clone(),
                            "task_id": event["task_id"].clone(),
                            "session_id": event["session_id"].clone(),
                            "data": event["data"].clone(),
                        });
                        yield Ok::<_, std::convert::Infallible>(
                            axum::response::sse::Event::default()
                                .id(seq.to_string())
                                .event("message")
                                .data(payload.to_string()),
                        );
                    }
                }
                Err(()) => {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    continue;
                }
            }
            if request.kind == StreamKind::Tasks {
                // The task list has no per-task event cursor, so changed tasks
                // are reported with a stream-local sequence number.
                if let Ok(reply) = state.daemon.get("/v1/tasks?after=0&limit=200").await {
                    let mut changed = Vec::new();
                    if let Some(items) = reply.body["tasks"].as_array() {
                        for task in items {
                            let id = task["id"].as_str().unwrap_or_default().to_string();
                            let revision = task["revision"].as_i64().unwrap_or(0);
                            let updated = task["updated_at"].as_i64().unwrap_or(0);
                            let previous = seen.get(&id).copied();
                            if previous != Some((revision, updated)) {
                                seen.insert(id.clone(), (revision, updated));
                                changed.push((id, revision, updated, task.clone()));
                            }
                        }
                    }
                    for (id, _revision, updated, task) in changed {
                        if let Some(project) = request.project.as_deref()
                            && task["project"].as_str() != Some(project)
                            && project_id(task["project"].as_str().unwrap_or_default())
                                != project
                        {
                            continue;
                        }
                        counter += 1;
                        cursor = counter;
                        let payload = json!({
                            "seq": counter,
                            "at": updated,
                            "kind": "task_state",
                            "task_id": id,
                            "data": task,
                        });
                        yield Ok::<_, std::convert::Infallible>(
                            axum::response::sse::Event::default()
                                .id(counter.to_string())
                                .event("message")
                                .data(payload.to_string()),
                        );
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
    };
    axum::response::sse::Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StreamKind {
    Tasks,
    Task,
    Session,
}

struct StreamTarget {
    kind: StreamKind,
    id: Option<String>,
    project: Option<String>,
}

impl StreamTarget {
    async fn fetch(&self, state: &WebState, cursor: i64) -> Result<Vec<Value>, ()> {
        let path = match (self.kind, &self.id) {
            (StreamKind::Task, Some(id)) => format!("/v1/tasks/{id}/events?after={cursor}"),
            (StreamKind::Session, Some(id)) => {
                format!("/v1/sessions/{id}/events?after={cursor}")
            }
            _ => return Ok(Vec::new()),
        };
        let reply = state.daemon.get(&path).await.map_err(|_| ())?;
        Ok(match reply.body {
            Value::Array(items) => items,
            Value::Object(ref map) => map
                .get("events")
                .and_then(|value| value.as_array())
                .cloned()
                .unwrap_or_default(),
            _ => Vec::new(),
        })
    }
}

/// Resolves the frontend subscription path into a concrete stream.
fn stream_target(query: &StreamQuery) -> Result<StreamTarget, Box<Response>> {
    if let Some(task) = query.task.as_deref() {
        if !safe_identifier(task) {
            return Err(Box::new(bad_request("invalid task identifier")));
        }
        return Ok(StreamTarget {
            kind: StreamKind::Task,
            id: Some(task.to_string()),
            project: None,
        });
    }
    if let Some(session) = query.session.as_deref() {
        if !safe_identifier(session) {
            return Err(Box::new(bad_request("invalid session identifier")));
        }
        return Ok(StreamTarget {
            kind: StreamKind::Session,
            id: Some(session.to_string()),
            project: None,
        });
    }
    let Some(path) = query.path.as_deref() else {
        return Err(Box::new(bad_request("a subscription path is required")));
    };
    let (path, parameters) = match path.split_once('?') {
        Some((path, parameters)) => (path, Some(parameters)),
        None => (path, None),
    };
    let mut project = query.project.clone();
    if let Some(parameters) = parameters {
        for pair in parameters.split('&') {
            if let Some((key, value)) = pair.split_once('=')
                && key == "project"
            {
                project = Some(value.to_string());
            }
        }
    }
    let parts = path.trim_matches('/').split('/').collect::<Vec<_>>();
    match parts.as_slice() {
        ["tasks"] => Ok(StreamTarget {
            kind: StreamKind::Tasks,
            id: None,
            project,
        }),
        ["tasks", id] => {
            if !safe_identifier(id) {
                return Err(Box::new(bad_request("invalid task identifier")));
            }
            Ok(StreamTarget {
                kind: StreamKind::Task,
                id: Some((*id).to_string()),
                project,
            })
        }
        ["sessions", id] => {
            if !safe_identifier(id) {
                return Err(Box::new(bad_request("invalid session identifier")));
            }
            Ok(StreamTarget {
                kind: StreamKind::Session,
                id: Some((*id).to_string()),
                project,
            })
        }
        _ => Err(Box::new(bad_request("unknown subscription path"))),
    }
}

/// Forwards a daemon answer, preserving its status so conflicts stay visible.
fn forward(reply: daemon::DaemonReply) -> Response {
    let status = reply.status;
    let body = if reply.body.is_null() {
        json!({})
    } else {
        reply.body
    };
    (status, Json(body)).into_response()
}

fn bad_request(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": message}))).into_response()
}

fn daemon_unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": "the local runtime is not reachable"})),
    )
        .into_response()
}

/// Serves the built frontend. Unknown paths fall back to the application
/// shell so client-side routes survive a reload.
async fn static_asset(State(state): State<WebState>, uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let mut resolved = state.assets.clone();
    if !path.is_empty() {
        let mut safe = true;
        for component in FsPath::new(path).components() {
            match component {
                Component::Normal(part) => resolved.push(part),
                _ => {
                    safe = false;
                    break;
                }
            }
        }
        if !safe {
            return (StatusCode::BAD_REQUEST, "invalid path").into_response();
        }
    }
    if resolved.is_file()
        && let Ok(bytes) = tokio::fs::read(&resolved).await
    {
        let mime = content_type(&resolved);
        return (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, mime),
                (header::CACHE_CONTROL, "no-store"),
            ],
            bytes,
        )
            .into_response();
    }
    let index = state.assets.join("index.html");
    match tokio::fs::read(&index).await {
        Ok(bytes) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            bytes,
        )
            .into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "the shared frontend is not built",
                "hint": "run npm install && npm run build in frontend/",
            })),
        )
            .into_response(),
    }
}

fn content_type(path: &FsPath) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_binds_are_accepted() {
        let config = WebConfig::new(
            "0.0.0.0:8080".parse().unwrap(),
            PathBuf::from("/tmp"),
            "default".into(),
            PathBuf::from("/tmp"),
        );
        assert!(!config.bind.ip().is_loopback());
        let config = WebConfig::new(
            "127.0.0.1:8080".parse().unwrap(),
            PathBuf::from("/tmp"),
            "default".into(),
            PathBuf::from("/tmp"),
        );
        assert!(config.bind.ip().is_loopback());
    }

    #[test]
    fn identifiers_and_hashes_are_validated() {
        assert!(safe_identifier("abc-123_x"));
        assert!(!safe_identifier("../etc/passwd"));
        assert!(!safe_identifier(""));
        assert!(safe_hash(&"a".repeat(64)));
        assert!(!safe_hash("short"));
    }
}

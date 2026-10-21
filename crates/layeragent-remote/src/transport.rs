//! Direct mTLS termination; forwarded headers never establish runner identity.
use crate::{crypto::Signed, queue::Queue, *};
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Extension, Path as RoutePath, State},
    http::StatusCode,
    routing::{get, post},
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use serde::{Serialize, de::DeserializeOwned};
use std::{path::Path, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

pub fn certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let bytes = std::fs::read(path)?;
    ensure!(bytes.len() <= 65536, "certificate bundle exceeds limit");
    let certs =
        CertificateDer::pem_slice_iter(&bytes).collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(!certs.is_empty(), "certificate required");
    Ok(certs)
}
pub fn certificate_hash(path: &Path) -> Result<String> {
    Ok(layeragent_policy::digest(
        certificates(path)?
            .first()
            .context("certificate required")?
            .as_ref(),
    ))
}
pub async fn serve(
    config: CoordinatorConfig,
    initialize: bool,
    recover: bool,
    cancel: CancellationToken,
) -> Result<()> {
    ensure!(cfg!(target_os = "linux"), "remote bridge requires Linux");
    #[cfg(unix)]
    ensure!(
        unsafe { libc::geteuid() } != 0,
        "remote coordinator must use its unprivileged service account, not root"
    );
    let _ = rustls::crypto::ring::default_provider().install_default();
    crypto::public_key(&config.owner_signing_key)?;
    ensure!(
        config.owner_certificate_sha256.len() == 64,
        "owner transport identity required"
    );
    let dsn = std::env::var(&config.database_env)
        .context("database environment reference unavailable")?;
    let queue = Queue::open(
        &dsn,
        &config.account,
        &config.epoch,
        &config.owner_signing_key,
        &config.authority_guard,
        initialize,
        recover,
    )
    .await?;
    let mut roots = rustls::RootCertStore::empty();
    for cert in certificates(&config.ca)? {
        roots.add(cert)?;
    }
    let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots)).build()?;
    let key = PrivateKeyDer::from_pem_slice(&crypto::private_read(&config.tls_key, 65536)?)?;
    let mut tls = rustls::ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(certificates(&config.certificate)?, key)?;
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
    let state = Api {
        queue,
        owner: config.owner_certificate_sha256,
        budgets: Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new())),
    };
    let app = Router::new()
        .route("/remote/v1/jobs", get(jobs).post(submit))
        .route("/remote/v1/jobs/{id}", get(job))
        .route("/remote/v1/runners", get(runners).post(enroll))
        .route("/remote/v1/control", post(control))
        .route("/remote/v1/claim", post(claim))
        .route("/remote/v1/renew", post(renew))
        .route("/remote/v1/complete", post(complete))
        .layer(DefaultBodyLimit::max(MAX_ENVELOPE + 16384))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            admission,
        ))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    let slots = Arc::new(tokio::sync::Semaphore::new(64));
    loop {
        let (socket, _) = tokio::select! {_=cancel.cancelled()=>break,r=listener.accept()=>r?};
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            continue;
        };
        let acceptor = acceptor.clone();
        let app = app.clone();
        let stop = cancel.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let work = async {
                let tls = tokio::time::timeout(Duration::from_secs(5), acceptor.accept(socket))
                    .await
                    .ok()?
                    .ok()?;
                let certificate = tls.get_ref().1.peer_certificates()?.first()?;
                let peer = Peer(layeragent_policy::digest(certificate.as_ref()));
                let service =
                    hyper_util::service::TowerToHyperService::new(app.layer(Extension(peer)));
                let mut builder = hyper_util::server::conn::auto::Builder::new(
                    hyper_util::rt::TokioExecutor::new(),
                );
                builder
                    .http1()
                    .timer(hyper_util::rt::TokioTimer::new())
                    .header_read_timeout(Duration::from_secs(10));
                let _ = builder
                    .serve_connection(hyper_util::rt::TokioIo::new(tls), service)
                    .await;
                Some(())
            };
            tokio::select! {_=stop.cancelled()=>{},_=tokio::time::timeout(Duration::from_secs(30),work)=>{}}
        });
    }
    Ok(())
}
#[derive(Clone)]
struct Api {
    queue: Queue,
    owner: String,
    budgets: Arc<std::sync::Mutex<std::collections::BTreeMap<String, (std::time::Instant, u32)>>>,
}
#[derive(Clone)]
struct Peer(String);
type ApiResult = std::result::Result<Json<serde_json::Value>, StatusCode>;
async fn admission(
    State(api): State<Api>,
    Extension(peer): Extension<Peer>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let is_owner = peer.0 == api.owner;
    if !is_owner
        && (request.method() != axum::http::Method::POST
            || ![
                "/remote/v1/claim",
                "/remote/v1/renew",
                "/remote/v1/complete",
            ]
            .contains(&request.uri().path()))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let accepted = (|| {
        let mut budgets = api.budgets.lock().ok()?;
        let now = std::time::Instant::now();
        budgets.retain(|_, (start, _)| now.duration_since(*start).as_secs() < 60);
        if !budgets.contains_key(&peer.0) && budgets.len() >= 2048 {
            return None;
        }
        let entry = budgets.entry(peer.0).or_insert((now, 0));
        if now.duration_since(entry.0).as_secs() >= 10 {
            *entry = (now, 0);
        }
        let limit = if is_owner { 1200 } else { 30 };
        if entry.1 >= limit {
            return None;
        }
        entry.1 += 1;
        Some(())
    })();
    if accepted.is_none() {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    next.run(request).await
}
fn owner(api: &Api, peer: &Peer) -> std::result::Result<(), StatusCode> {
    if api.owner == peer.0 {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}
fn response(result: Result<serde_json::Value>) -> ApiResult {
    result.map(Json).map_err(|_| StatusCode::CONFLICT)
}
async fn jobs(State(s): State<Api>, Extension(p): Extension<Peer>) -> ApiResult {
    owner(&s, &p)?;
    response(s.queue.jobs().await)
}
async fn job(
    State(s): State<Api>,
    Extension(p): Extension<Peer>,
    RoutePath(id): RoutePath<String>,
) -> ApiResult {
    owner(&s, &p)?;
    response(s.queue.job(&id).await)
}
async fn runners(State(s): State<Api>, Extension(p): Extension<Peer>) -> ApiResult {
    owner(&s, &p)?;
    response(s.queue.runners().await)
}
async fn submit(
    State(s): State<Api>,
    Extension(p): Extension<Peer>,
    Json(v): Json<Signed<JobEnvelope>>,
) -> ApiResult {
    owner(&s, &p)?;
    response(
        s.queue
            .submit(v)
            .await
            .map(|_| serde_json::json!({"accepted":true})),
    )
}
async fn enroll(
    State(s): State<Api>,
    Extension(p): Extension<Peer>,
    Json(v): Json<Signed<Enrollment>>,
) -> ApiResult {
    owner(&s, &p)?;
    response(
        s.queue
            .enroll(v)
            .await
            .map(|_| serde_json::json!({"enrolled":true})),
    )
}
async fn control(
    State(s): State<Api>,
    Extension(p): Extension<Peer>,
    Json(v): Json<Signed<Control>>,
) -> ApiResult {
    owner(&s, &p)?;
    response(
        s.queue
            .control(v)
            .await
            .map(|_| serde_json::json!({"blocked":true})),
    )
}
async fn claim(State(s): State<Api>, Extension(p): Extension<Peer>) -> ApiResult {
    response(
        s.queue
            .claim(&p.0)
            .await
            .and_then(|v| Ok(serde_json::to_value(v)?)),
    )
}
async fn renew(
    State(s): State<Api>,
    Extension(p): Extension<Peer>,
    Json(v): Json<Lease>,
) -> ApiResult {
    response(
        s.queue
            .renew(&p.0, v)
            .await
            .and_then(|v| Ok(serde_json::to_value(v)?)),
    )
}
async fn complete(
    State(s): State<Api>,
    Extension(p): Extension<Peer>,
    Json(v): Json<Signed<Completion>>,
) -> ApiResult {
    response(
        s.queue
            .complete(&p.0, v)
            .await
            .map(|_| serde_json::json!({"recorded":true})),
    )
}

#[derive(Clone)]
pub struct RemoteClient {
    http: reqwest::Client,
    endpoint: String,
}
impl RemoteClient {
    pub fn new(endpoint: &str, ca: &Path, certificate: &Path, key: &Path) -> Result<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let url = reqwest::Url::parse(endpoint)?;
        ensure!(
            url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path() == "/",
            "coordinator requires an HTTPS origin"
        );
        let mut pem = crypto::private_read(key, 65536)?;
        pem.extend_from_slice(&std::fs::read(certificate)?);
        let identity = reqwest::Identity::from_pem(&pem)?;
        let mut builder = reqwest::Client::builder()
            .tls_certs_only(Vec::<reqwest::Certificate>::new())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .identity(identity);
        for certificate in certificates(ca)? {
            builder =
                builder.add_root_certificate(reqwest::Certificate::from_der(certificate.as_ref())?);
        }
        Ok(Self {
            http: builder.build()?,
            endpoint: endpoint.trim_end_matches('/').into(),
        })
    }
    async fn decode<T: DeserializeOwned>(mut response: reqwest::Response) -> Result<T> {
        ensure!(
            response.status().is_success(),
            "remote request rejected ({})",
            response.status().as_u16()
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len() + chunk.len() <= MAX_ENVELOPE + 65536,
                "remote response exceeds limit"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok(serde_json::from_slice(&bytes)?)
    }
    pub async fn get<T: DeserializeOwned>(&self, route: &str) -> Result<T> {
        Self::decode(
            self.http
                .get(format!("{}/remote/v1/{}", self.endpoint, route))
                .send()
                .await?,
        )
        .await
    }
    pub async fn post<T: Serialize, R: DeserializeOwned>(
        &self,
        route: &str,
        value: &T,
    ) -> Result<R> {
        ensure!(
            serde_json::to_vec(value)?.len() <= MAX_ENVELOPE + 16384,
            "request exceeds limit"
        );
        Self::decode(
            self.http
                .post(format!("{}/remote/v1/{}", self.endpoint, route))
                .json(value)
                .send()
                .await?,
        )
        .await
    }
}

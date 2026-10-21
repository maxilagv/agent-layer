//! Conservative, process-local HTTP admission limits. Never trust forwarded client identifiers.
use axum::{
    body::Body,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use layeragent_protocol::Scope;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const BURST: f64 = 240.0;
const REQUESTS_PER_SECOND: f64 = 2.0;
const MAX_IDENTITIES: usize = 1024;
struct Principal {
    tokens: f64,
    updated: Instant,
    requests: Arc<Semaphore>,
    streams: Arc<Semaphore>,
    jobs: Arc<Semaphore>,
}
#[derive(Clone)]
pub(crate) struct Admission {
    requests: Arc<Semaphore>,
    streams: Arc<Semaphore>,
    jobs: Arc<Semaphore>,
    principals: Arc<Mutex<HashMap<(String, String), Principal>>>,
}
pub(crate) struct Lease {
    _global: OwnedSemaphorePermit,
    _principal: OwnedSemaphorePermit,
}
impl Default for Admission {
    fn default() -> Self {
        Self {
            requests: Arc::new(Semaphore::new(128)),
            streams: Arc::new(Semaphore::new(64)),
            jobs: Arc::new(Semaphore::new(32)),
            principals: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}
impl Admission {
    pub(crate) fn request(&self, scope: &Scope) -> Result<Lease, StatusCode> {
        self.acquire(scope, 0)
    }
    pub(crate) fn stream(&self, scope: &Scope) -> Result<Lease, StatusCode> {
        self.acquire(scope, 1)
    }
    pub(crate) fn job(&self, scope: &Scope) -> Result<Lease, StatusCode> {
        self.acquire(scope, 2)
    }
    fn acquire(&self, scope: &Scope, kind: u8) -> Result<Lease, StatusCode> {
        let mut principals = self
            .principals
            .lock()
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let key = (scope.tenant.clone(), scope.principal.clone());
        if !principals.contains_key(&key) {
            // Retain active principals even after their last request, without unbounded growth.
            principals.retain(|_, p| {
                p.updated.elapsed() < Duration::from_secs(600)
                    || p.requests.available_permits() != 32
                    || p.streams.available_permits() != 8
                    || p.jobs.available_permits() != 4
            });
            if principals.len() >= MAX_IDENTITIES {
                return Err(StatusCode::SERVICE_UNAVAILABLE);
            }
        }
        let p = principals.entry(key).or_insert_with(|| Principal {
            tokens: BURST,
            updated: Instant::now(),
            requests: Arc::new(Semaphore::new(32)),
            streams: Arc::new(Semaphore::new(8)),
            jobs: Arc::new(Semaphore::new(4)),
        });
        if kind == 0 {
            p.tokens =
                (p.tokens + p.updated.elapsed().as_secs_f64() * REQUESTS_PER_SECOND).min(BURST);
            p.updated = Instant::now();
            if p.tokens < 1.0 {
                return Err(StatusCode::TOO_MANY_REQUESTS);
            }
            p.tokens -= 1.0;
        }
        let (global, principal) = match kind {
            1 => (&self.streams, &p.streams),
            2 => (&self.jobs, &p.jobs),
            _ => (&self.requests, &p.requests),
        };
        let global = global
            .clone()
            .try_acquire_owned()
            .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
        let principal = principal
            .clone()
            .try_acquire_owned()
            .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
        Ok(Lease {
            _global: global,
            _principal: principal,
        })
    }
}
pub(crate) fn limited_response(status: StatusCode) -> Response {
    (
        status,
        [("retry-after", "1")],
        "gateway admission limit reached",
    )
        .into_response()
}
/// Hold capacity until the client consumes or drops the body, including MCP/SSE streams.
pub(crate) fn hold_response<T: Send + 'static>(response: Response, lease: T) -> Response {
    let (parts, body) = response.into_parts();
    let stream = async_stream::stream! {
        let _lease = lease;
        let mut body = body.into_data_stream();
        let deadline = tokio::time::sleep(Duration::from_secs(3600));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                chunk = body.next() => match chunk { Some(chunk) => yield chunk, None => break },
            }
        }
    };
    Response::from_parts(parts, Body::from_stream(stream))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scope(principal: &str) -> Scope {
        Scope {
            tenant: "tenant".into(),
            principal: principal.into(),
        }
    }
    #[test]
    fn limits_are_per_principal_and_release_on_drop() {
        let admission = Admission::default();
        let alice = scope("alice");
        let mut leases: Vec<_> = (0..4)
            .map(|_| admission.job(&alice).expect("fixture job admitted"))
            .collect();
        assert!(matches!(
            admission.job(&alice),
            Err(StatusCode::TOO_MANY_REQUESTS)
        ));
        assert!(admission.job(&scope("bob")).is_ok());
        leases.pop();
        assert!(admission.job(&alice).is_ok());
        let streams: Vec<_> = (0..8)
            .map(|_| admission.stream(&alice).expect("fixture stream admitted"))
            .collect();
        assert!(admission.stream(&alice).is_err());
        drop(streams);
        assert!(admission.stream(&alice).is_ok());
    }
    #[test]
    fn rate_limit_cannot_be_bypassed_by_releasing_connections() {
        let admission = Admission::default();
        for _ in 0..BURST as usize {
            drop(
                admission
                    .request(&scope("alice"))
                    .expect("fixture request admitted"),
            );
        }
        assert!(matches!(
            admission.request(&scope("alice")),
            Err(StatusCode::TOO_MANY_REQUESTS)
        ));
        assert!(admission.request(&scope("bob")).is_ok());
    }
}

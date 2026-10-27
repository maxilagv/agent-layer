//! Local one-time codes and browser sessions.
//!
//! The web surface never carries a bearer token in a URL or in browser
//! storage. A one-time code is shown to the operator on the terminal that
//! started the server; redeeming it creates an HttpOnly session whose CSRF
//! token must accompany every mutation.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
use anyhow::{Result, ensure};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// A single-use access code shown to the operator.
#[derive(Clone, Debug)]
pub struct AccessCode {
    pub code: String,
    pub expires_at: i64,
}

/// A browser session bound to one local principal.
#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    pub csrf: String,
    pub principal: String,
    pub tenant: String,
    pub created_at: i64,
    pub expires_at: i64,
}

#[derive(Default)]
struct Inner {
    codes: HashMap<String, (i64, u32)>,
    sessions: HashMap<String, Session>,
}

/// Bounded session and code store.
pub struct SessionStore {
    inner: Mutex<Inner>,
    code_ttl_ms: i64,
    session_ttl_ms: i64,
}

impl SessionStore {
    pub fn new(code_ttl_ms: i64, session_ttl_ms: i64) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            code_ttl_ms,
            session_ttl_ms,
        }
    }

    /// Issues a fresh single-use code and drops every previous one.
    pub fn issue_code(&self) -> AccessCode {
        // Uppercase hexadecimal keeps the code easy to read and to type.
        let code = random_token(8).to_ascii_uppercase();
        let expires_at = now_ms() + self.code_ttl_ms;
        let mut inner = self.inner.lock().expect("session store poisoned");
        inner.codes.clear();
        inner.codes.insert(code.clone(), (expires_at, 0));
        AccessCode { code, expires_at }
    }

    /// Redeems a code exactly once. Repeated or expired attempts fail.
    pub fn redeem(&self, code: &str) -> Result<Session> {
        let code = code.trim().to_ascii_uppercase();
        let mut inner = self.inner.lock().expect("session store poisoned");
        let (expires_at, attempts) = inner
            .codes
            .get_mut(&code)
            .ok_or_else(|| anyhow::anyhow!("invalid access code"))?;
        if *attempts >= 5 {
            anyhow::bail!("too many attempts; request a new code");
        }
        if now_ms() > *expires_at {
            inner.codes.remove(&code);
            anyhow::bail!("access code expired");
        }
        *attempts += 1;
        let (expires_at, _) = inner.codes.remove(&code).expect("code present");
        ensure!(now_ms() <= expires_at, "access code expired");
        let session = Session {
            id: random_token(32),
            csrf: random_token(32),
            principal: "owner".into(),
            tenant: "default".into(),
            created_at: now_ms(),
            expires_at: now_ms() + self.session_ttl_ms,
        };
        inner.sessions.insert(session.id.clone(), session.clone());
        Ok(session)
    }

    /// Returns a live session and refreshes its expiry.
    pub fn session(&self, id: &str) -> Option<Session> {
        let mut inner = self.inner.lock().expect("session store poisoned");
        let session = inner.sessions.get(id)?.clone();
        if now_ms() > session.expires_at {
            inner.sessions.remove(id);
            return None;
        }
        let mut refreshed = session;
        refreshed.expires_at = now_ms() + self.session_ttl_ms;
        inner.sessions.insert(id.to_string(), refreshed.clone());
        Some(refreshed)
    }

    /// Invalidates one interface session. Running work is not affected.
    pub fn invalidate(&self, id: &str) {
        let mut inner = self.inner.lock().expect("session store poisoned");
        inner.sessions.remove(id);
    }

    pub fn live_sessions(&self) -> usize {
        let mut inner = self.inner.lock().expect("session store poisoned");
        let now = now_ms();
        inner.sessions.retain(|_, session| session.expires_at > now);
        inner.sessions.len()
    }
}

/// URL-safe random token.
pub fn random_token(bytes: usize) -> String {
    use sha2::{Digest, Sha256};
    let mut seed = Vec::new();
    seed.extend_from_slice(&now_ms().to_le_bytes());
    seed.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    seed.extend_from_slice(&(std::process::id() as u64).to_le_bytes());
    let mut digest = Sha256::digest(&seed);
    let mut out = String::new();
    while out.len() < bytes {
        out.push_str(&hex::encode(digest));
        digest = Sha256::digest(digest);
    }
    out.truncate(bytes);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_is_single_use() {
        let store = SessionStore::new(60_000, 60_000);
        let code = store.issue_code();
        assert!(store.redeem(&code.code).is_ok());
        assert!(store.redeem(&code.code).is_err());
    }

    #[test]
    fn an_expired_code_is_refused() {
        let store = SessionStore::new(-1, 60_000);
        let code = store.issue_code();
        assert!(store.redeem(&code.code).is_err());
    }

    #[test]
    fn logout_invalidates_only_the_interface_session() {
        let store = SessionStore::new(60_000, 60_000);
        let code = store.issue_code();
        let session = store.redeem(&code.code).unwrap();
        assert!(store.session(&session.id).is_some());
        store.invalidate(&session.id);
        assert!(store.session(&session.id).is_none());
    }
}

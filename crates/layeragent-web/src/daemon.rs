//! Bounded client for the local daemon.
//!
//! The daemon listens on a UNIX socket inside the operator's private home
//! directory, so the web surface never carries a bearer token on the wire.
//! Every request path is assembled here from validated parts.
use anyhow::{Context, Result, ensure};
use reqwest::{StatusCode, header::HeaderMap};
use serde_json::Value;
use std::path::Path;

/// Identifiers accepted from a browser before they are placed in a path.
pub fn safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Content hashes accepted from a browser.
pub fn safe_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// A daemon response the web surface forwards verbatim.
pub struct DaemonReply {
    pub status: StatusCode,
    pub body: Value,
}

/// A daemon response whose body is not JSON.
pub struct DaemonText {
    pub status: StatusCode,
    pub body: String,
}

#[derive(Clone)]
pub struct DaemonClient {
    http: reqwest::Client,
    base: String,
}

impl DaemonClient {
    /// Builds a client for the daemon socket of one profile.
    pub fn new(home: &Path, profile: &str) -> Result<Self> {
        let socket = home.join("runtime").join(format!("{profile}.sock"));
        let http = reqwest::Client::builder()
            .unix_socket(socket)
            .timeout(std::time::Duration::from_secs(120))
            .build()?;
        Ok(Self {
            http,
            base: "http://localhost".into(),
        })
    }

    pub async fn get(&self, path: &str) -> Result<DaemonReply> {
        let response = self
            .http
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .context("daemon is not reachable")?;
        let status = response.status();
        let body = response.json::<Value>().await.unwrap_or(Value::Null);
        Ok(DaemonReply { status, body })
    }

    pub async fn get_text(&self, path: &str) -> Result<DaemonText> {
        let response = self
            .http
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .context("daemon is not reachable")?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Ok(DaemonText { status, body })
    }

    pub async fn post(&self, path: &str, body: Value) -> Result<DaemonReply> {
        self.post_with_headers(path, body, HeaderMap::new()).await
    }

    pub async fn post_with_headers(
        &self,
        path: &str,
        body: Value,
        headers: HeaderMap,
    ) -> Result<DaemonReply> {
        let mut request = self.http.post(format!("{}{path}", self.base)).json(&body);
        for (name, value) in headers.iter() {
            request = request.header(name, value);
        }
        let response = request.send().await.context("daemon is not reachable")?;
        let status = response.status();
        let body = response.json::<Value>().await.unwrap_or(Value::Null);
        Ok(DaemonReply { status, body })
    }

    /// Confirms the daemon answers before the web surface starts serving.
    pub async fn ready(&self) -> Result<bool> {
        let response = self
            .http
            .get(format!("{}/health", self.base))
            .send()
            .await
            .context("daemon is not reachable")?;
        Ok(response.status().is_success())
    }

    /// Rejects a daemon path that was not produced by this module.
    pub fn checked(path: &str) -> Result<&str> {
        ensure!(
            path.starts_with('/')
                && !path.contains("..")
                && !path.contains("//")
                && path.len() <= 2048,
            "invalid daemon path"
        );
        Ok(path)
    }
}

//! HTTP contract for durable tasks: status codes and idempotent replay.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use anyhow::Result;
use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use layeragent_config::Config;
use layeragent_core::Engine;
use layeragent_gateway::{ApiState, router};
use layeragent_protocol::{Completion, Message, ToolDefinition};
use layeragent_providers::{Delta, LanguageModel};
use layeragent_sandbox::{ExecOutput, ExecRequest, Sandbox};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::mpsc;
use tower::ServiceExt;

struct NoopModel;
#[async_trait]
impl LanguageModel for NoopModel {
    async fn infer(
        &self,
        _: &Config,
        _: &str,
        _: &[Message],
        _: &[ToolDefinition],
        _: tokio_util::sync::CancellationToken,
        _: mpsc::Sender<Delta>,
    ) -> Result<Completion> {
        Ok(Completion {
            text: "ok".into(),
            ..Default::default()
        })
    }
}

struct NoopSandbox;
#[async_trait]
impl Sandbox for NoopSandbox {
    async fn execute(
        &self,
        _: ExecRequest,
        _: tokio_util::sync::CancellationToken,
        _: Option<mpsc::Sender<String>>,
    ) -> Result<ExecOutput> {
        anyhow::bail!("no tool expected")
    }
}

fn app() -> Result<(axum::Router, tempfile::TempDir)> {
    let home = tempfile::tempdir()?;
    // Deliberately no model: the task must still be created and identified
    // before execution reports that the capability is unavailable.
    let engine = Engine::with_components(
        home.path().into(),
        "default".into(),
        Config::default(),
        Arc::new(NoopModel),
        Arc::new(NoopSandbox),
    )?;
    Ok((router(ApiState::new(engine, true)), home))
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    key: Option<&str>,
    body: Value,
) -> Result<(StatusCode, Value)> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string()))?)
        .await?;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await?;
    Ok((status, serde_json::from_slice(&bytes)?))
}

#[tokio::test]
async fn create_replay_and_conflict_have_distinct_status_codes() -> Result<()> {
    let (app, home) = app()?;
    let workspace = tempfile::tempdir()?;
    let workspace = workspace.path().to_string_lossy().into_owned();
    let body = json!({"workspace": workspace, "objective": "build it", "executor": "local"});
    let (status, first) = call(&app, "POST", "/v1/tasks", Some("key-1"), body.clone()).await?;
    assert_eq!(status, StatusCode::CREATED);
    let id = first["id"].as_str().expect("id").to_string();
    assert_eq!(first["project"], workspace);
    assert_eq!(first["executor"], "local");
    let (status, replay) = call(&app, "POST", "/v1/tasks", Some("key-1"), body.clone()).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["id"], id.as_str());
    let changed =
        json!({"workspace": workspace, "objective": "something else", "executor": "local"});
    let (status, conflict) = call(&app, "POST", "/v1/tasks", Some("key-1"), changed).await?;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(conflict["error"].as_str().unwrap().contains("idempotency"));
    // The key is required: an otherwise valid request cannot create duplicates.
    let (status, _) = call(&app, "POST", "/v1/tasks", None, body).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let _ = home;
    Ok(())
}

#[tokio::test]
async fn task_read_correction_and_artifact_scopes_return_typed_errors() -> Result<()> {
    let (app, _home) = app()?;
    let workspace = tempfile::tempdir()?;
    let (_, task) = call(
        &app,
        "POST",
        "/v1/tasks",
        Some("key-2"),
        json!({"workspace": workspace.path().to_string_lossy(), "objective": "work"}),
    )
    .await?;
    let id = task["id"].as_str().expect("id").to_string();
    let (status, fetched) = call(&app, "GET", &format!("/v1/tasks/{id}"), None, json!({})).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["id"], id.as_str());
    let (status, _) = call(&app, "GET", "/v1/tasks/missing-task", None, json!({})).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let revision = fetched["revision"].as_i64().expect("revision");
    let stale = json!({"instruction": "do it differently", "expected_revision": revision + 50});
    let (status, _) = call(
        &app,
        "POST",
        &format!("/v1/tasks/{id}/correct"),
        None,
        stale,
    )
    .await?;
    assert_eq!(status, StatusCode::CONFLICT);
    let unknown_artifact = "b".repeat(64);
    let (status, _) = call(
        &app,
        "GET",
        &format!("/v1/artifacts/{unknown_artifact}"),
        None,
        json!({}),
    )
    .await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, connections) = call(&app, "GET", "/v1/connections", None, json!({})).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(connections.is_array());
    let (status, capabilities) = call(&app, "GET", "/v1/mcp/capabilities", None, json!({})).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(capabilities["resources"], true);
    assert_eq!(capabilities["prompts"], false);
    assert_eq!(capabilities["templates"], false);
    let (status, usage) = call(&app, "GET", "/v1/usage", None, json!({})).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(usage["totals"]["tasks"].as_i64().unwrap_or(0) >= 1);
    Ok(())
}

#[tokio::test]
async fn approval_without_the_observed_revision_is_a_conflict() -> Result<()> {
    let (app, _home) = app()?;
    let (status, _) = call(
        &app,
        "POST",
        "/v1/approvals/approval-1",
        None,
        json!({"allow": true}),
    )
    .await?;
    assert_eq!(status, StatusCode::CONFLICT);
    Ok(())
}

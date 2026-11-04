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
    let directory = tempfile::tempdir()?;
    // The task records the canonical project path (on macOS the temporary
    // directory is reached through the /var -> /private/var symlink).
    let workspace = directory
        .path()
        .canonicalize()?
        .to_string_lossy()
        .into_owned();
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

/// A daemon whose models carry the given team access; the first is primary.
fn team_models_app(
    models: &[(&str, &str, layeragent_config::ModelTeamAccess)],
) -> Result<(axum::Router, tempfile::TempDir)> {
    let home = tempfile::tempdir()?;
    let mut config = Config::default();
    config.providers.insert(
        "local".into(),
        layeragent_config::Provider {
            wire: layeragent_config::Wire::Ollama,
            base_url: "http://127.0.0.1:9".into(),
            key_env: None,
            oauth: None,
            headers: Default::default(),
        },
    );
    for (alias, id, access) in models {
        config.models.insert(
            (*alias).into(),
            layeragent_config::Model {
                provider: "local".into(),
                id: (*id).into(),
                context_window: 32768,
                tools: true,
                emulate_tools: false,
                input_per_million: None,
                output_per_million: None,
                parameters: Default::default(),
                notes: None,
                team_access: *access,
                cache_read_per_million: None,
                cache_write_per_million: None,
                prompt_cache: false,
            },
        );
    }
    config.primary = models[0].0.into();
    config.policy.write = true;
    let engine = Engine::with_components(
        home.path().into(),
        "default".into(),
        config,
        Arc::new(NoopModel),
        Arc::new(NoopSandbox),
    )?;
    Ok((router(ApiState::new(engine, true)), home))
}

async fn create_team(
    app: &axum::Router,
    key: &str,
    model: Option<&str>,
    cooperative: Value,
) -> Result<(StatusCode, Value)> {
    let directory = tempfile::tempdir()?;
    let workspace = directory
        .path()
        .canonicalize()?
        .to_string_lossy()
        .into_owned();
    let mut body = json!({"workspace": workspace, "objective": "review it", "executor": "local", "execution": "cooperative", "cooperative": cooperative});
    if let Some(model) = model {
        body["model"] = json!(model);
    }
    call(app, "POST", "/v1/tasks", Some(key), body).await
}

#[tokio::test]
async fn a_team_runs_only_the_models_the_operator_allows() -> Result<()> {
    use layeragent_config::ModelTeamAccess::{None as Out, Read, Write};
    let (app, _home) = team_models_app(&[
        ("writer", "model-a", Write),
        ("reader", "model-b", Read),
        ("outside", "model-c", Out),
    ])?;
    let (status, refused) = create_team(&app, "k1", Some("outside"), json!({})).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("may not run team members"),
        "{refused}"
    );
    let (status, refused) = create_team(
        &app,
        "k2",
        None,
        json!({"role_models": {"reviewer": "outside"}}),
    )
    .await?;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
    // A read-only model runs members and reviews; writing needs the writer.
    let cross = json!({"scope": {"allow_write": true}, "cross_model_review": true, "role_models": {"reviewer": "reader"}});
    let (status, created) = create_team(&app, "k3", Some("reader"), cross.clone()).await?;
    assert_eq!(status, StatusCode::CREATED, "{created}");

    // An excluded model does not count as the second model.
    let (app, _home) =
        team_models_app(&[("writer", "model-a", Write), ("outside", "model-c", Out)])?;
    let (status, refused) = create_team(
        &app,
        "k4",
        None,
        json!({"scope": {"allow_write": true}, "cross_model_review": true}),
    )
    .await?;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("two models allowed in teams"),
        "{refused}"
    );

    // Write scope needs some model that may write.
    let (app, _home) = team_models_app(&[("reader", "model-b", Read)])?;
    let (status, refused) =
        create_team(&app, "k5", None, json!({"scope": {"allow_write": true}})).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
    assert!(
        refused["error"].as_str().unwrap().contains("team_access"),
        "{refused}"
    );
    let (status, created) = create_team(&app, "k6", None, json!({})).await?;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "a read-only team on it is fine: {created}"
    );
    Ok(())
}

#[tokio::test]
async fn a_task_budget_needs_prices_and_bounds_the_whole_task() -> Result<()> {
    use layeragent_config::ModelTeamAccess::Write;
    let (app, _home) = team_models_app(&[("unpriced", "model-a", Write)])?;
    let directory = tempfile::tempdir()?;
    let workspace = directory
        .path()
        .canonicalize()?
        .to_string_lossy()
        .into_owned();
    let body = json!({"workspace": workspace, "objective": "build it", "executor": "local", "budget_usd": 2.0});
    let (status, refused) = call(&app, "POST", "/v1/tasks", Some("b1"), body.clone()).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
    assert!(
        refused["error"].as_str().unwrap().contains("prices"),
        "{refused}"
    );

    let home = tempfile::tempdir()?;
    let mut config = Config::default();
    config.providers.insert(
        "local".into(),
        layeragent_config::Provider {
            wire: layeragent_config::Wire::Ollama,
            base_url: "http://127.0.0.1:9".into(),
            key_env: None,
            oauth: None,
            headers: Default::default(),
        },
    );
    config.models.insert(
        "priced".into(),
        layeragent_config::Model {
            provider: "local".into(),
            id: "priced-model".into(),
            context_window: 32768,
            tools: true,
            emulate_tools: false,
            input_per_million: Some(1.0),
            output_per_million: Some(2.0),
            parameters: Default::default(),
            notes: None,
            team_access: Write,
            cache_read_per_million: None,
            cache_write_per_million: None,
            prompt_cache: false,
        },
    );
    config.primary = "priced".into();
    let engine = Engine::with_components(
        home.path().into(),
        "default".into(),
        config,
        Arc::new(NoopModel),
        Arc::new(NoopSandbox),
    )?;
    let app = router(ApiState::new(engine, true));
    let (status, bad) = call(&app, "POST", "/v1/tasks", Some("b2"), json!({"workspace": workspace, "objective": "build it", "executor": "local", "budget_usd": -1.0})).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    let (status, created) = call(&app, "POST", "/v1/tasks", Some("b3"), body).await?;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["budget_usd"], 2.0);
    let id = created["id"].as_str().unwrap();
    let (status, report) = call(
        &app,
        "GET",
        &format!("/v1/tasks/{id}/usage"),
        None,
        json!({}),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["budget_usd"], 2.0);
    assert!(
        report["by_model"].is_array() && report["hints"].is_array(),
        "{report}"
    );

    // For a team, the budget is its cost ceiling unless that is lower.
    let team = json!({"workspace": workspace, "objective": "review it", "executor": "local", "execution": "cooperative", "cooperative": {"limits": {"max_cost_usd": 5.0}}, "budget_usd": 1.5});
    let (status, created) = call(&app, "POST", "/v1/tasks", Some("b4"), team).await?;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap();
    let (status, snapshot) = call(
        &app,
        "GET",
        &format!("/v1/tasks/{id}/team"),
        None,
        json!({}),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{snapshot}");
    assert_eq!(snapshot["team"]["options"]["limits"]["max_cost_usd"], 1.5);
    let _ = home;
    Ok(())
}

use super::{ApiError, ApiResult, ApiState, Identity};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use layeragent_protocol::{ExecutionMode, Task, TaskState, TeamCommand, TeamMessageInput};
use layeragent_storage::TeamConflict;
use serde::Deserialize;
use serde_json::{Value, json};

fn conflict(error: anyhow::Error) -> ApiError {
    if error.downcast_ref::<TeamConflict>().is_some() {
        ApiError::conflict(error)
    } else {
        error.into()
    }
}

async fn task(s: &ApiState, identity: &Identity, id: &str) -> ApiResult<Task> {
    let store = s.engine.store(&identity.scope).await?;
    let task = store
        .task(&identity.scope, id)
        .await
        .map_err(|_| ApiError::not_found(anyhow::anyhow!("task not found or inaccessible")))?;
    identity.require_workspace(&task.project)?;
    if task.execution != ExecutionMode::Cooperative || task.team_id.is_none() {
        return Err(ApiError::not_found(anyhow::anyhow!(
            "cooperative team not found"
        )));
    }
    Ok(task)
}

fn mutable(identity: &Identity) -> ApiResult<()> {
    if identity.read_only {
        Err(ApiError::forbidden(anyhow::anyhow!(
            "read-only identity cannot modify a team"
        )))
    } else {
        Ok(())
    }
}

pub(super) async fn snapshot(
    State(s): State<ApiState>,
    axum::Extension(identity): axum::Extension<Identity>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    task(&s, &identity, &id).await?;
    let (snapshot, task_revision) = s
        .engine
        .store(&identity.scope)
        .await?
        .team_snapshot_with_task_revision(&identity.scope, &id)
        .await?;
    Ok(Json(json!({"team":snapshot,"task_revision":task_revision})))
}

#[derive(Deserialize)]
pub(super) struct ResourceQuery {
    #[serde(default)]
    after: i64,
    limit: Option<usize>,
}

pub(super) async fn resource(
    State(s): State<ApiState>,
    axum::Extension(identity): axum::Extension<Identity>,
    Path((id, resource)): Path<(String, String)>,
    Query(query): Query<ResourceQuery>,
) -> ApiResult<Json<Value>> {
    task(&s, &identity, &id).await?;
    let (items, next) = s
        .engine
        .store(&identity.scope)
        .await?
        .team_resource(
            &identity.scope,
            &id,
            &resource,
            query.after,
            query.limit.unwrap_or(100),
        )
        .await?;
    Ok(Json(json!({"items":items,"next":next})))
}

pub(super) async fn messages(
    state: State<ApiState>,
    identity: axum::Extension<Identity>,
    Path(id): Path<String>,
    query: Query<ResourceQuery>,
) -> ApiResult<Json<Value>> {
    resource(state, identity, Path((id, "messages".to_string())), query).await
}

pub(super) async fn message(
    State(s): State<ApiState>,
    axum::Extension(identity): axum::Extension<Identity>,
    Path(id): Path<String>,
    Json(input): Json<TeamMessageInput>,
) -> ApiResult<Json<Value>> {
    mutable(&identity)?;
    task(&s, &identity, &id).await?;
    let result = s
        .engine
        .store(&identity.scope)
        .await?
        .team_user_message(&identity.scope, &id, input)
        .await
        .map_err(conflict)?;
    Ok(Json(result))
}

pub(super) async fn command(
    State(s): State<ApiState>,
    axum::Extension(identity): axum::Extension<Identity>,
    Path((id, action)): Path<(String, String)>,
    Json(command): Json<TeamCommand>,
) -> ApiResult<Json<Value>> {
    mutable(&identity)?;
    let task = task(&s, &identity, &id).await?;
    let result = s
        .engine
        .store(&identity.scope)
        .await?
        .team_command(&identity.scope, &id, &action, command)
        .await
        .map_err(conflict)?;
    if action == "cancel" {
        s.engine.cancel(&identity.scope, &id)?;
        if let Ok(jobs) = s.jobs.lock()
            && let Some((scope, token)) = jobs.get(&id)
            && scope == &identity.scope
        {
            token.cancel();
        }
    } else if action == "resume" && task.state == TaskState::Blocked {
        let running = s
            .jobs
            .lock()
            .map(|jobs| jobs.contains_key(&id))
            .unwrap_or(true);
        if !running {
            super::tasks::start(&s, &identity, &task, None);
        }
    }
    Ok(Json(result))
}

pub(super) async fn export(
    State(s): State<ApiState>,
    axum::Extension(identity): axum::Extension<Identity>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    task(&s, &identity, &id).await?;
    let export = s
        .engine
        .store(&identity.scope)
        .await?
        .team_export(&identity.scope, &id)
        .await?;
    Ok(Json(serde_json::to_value(export)?))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
        routing::{get, post},
    };
    use layeragent_config::Config;
    use layeragent_core::Engine;
    use layeragent_protocol::{
        Completion, CooperativeOptions, EffortLevel, Message, RunMode, Scope, ToolDefinition,
    };
    use layeragent_providers::{Delta, LanguageModel};
    use layeragent_sandbox::{ExecOutput, ExecRequest, Sandbox};
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    struct NeverModel;
    #[async_trait]
    impl LanguageModel for NeverModel {
        async fn infer(
            &self,
            _: &Config,
            _: &str,
            _: &[Message],
            _: &[ToolDefinition],
            _: CancellationToken,
            _: mpsc::Sender<Delta>,
        ) -> Result<Completion> {
            anyhow::bail!("route test must not invoke provider")
        }
    }
    struct NeverSandbox;
    #[async_trait]
    impl Sandbox for NeverSandbox {
        async fn execute(
            &self,
            _: ExecRequest,
            _: CancellationToken,
            _: Option<mpsc::Sender<String>>,
        ) -> Result<ExecOutput> {
            anyhow::bail!("route test must not execute tools")
        }
    }
    struct Fixture {
        _home: tempfile::TempDir,
        workspace: tempfile::TempDir,
        state: ApiState,
        task: String,
    }
    impl Fixture {
        async fn new() -> Result<Self> {
            let home = tempfile::tempdir()?;
            let workspace = tempfile::tempdir()?;
            let engine = Engine::with_components(
                home.path().into(),
                "default".into(),
                Config::default(),
                Arc::new(NeverModel),
                Arc::new(NeverSandbox),
            )?;
            let store = engine.store(&Scope::local()).await?;
            let task = store
                .create_task_with_execution(
                    &Scope::local(),
                    &workspace.path().to_string_lossy(),
                    None,
                    "route check",
                    "local",
                    "team-http",
                    "fixture",
                    EffortLevel::Auto,
                    RunMode::Normal,
                    false,
                    ExecutionMode::Cooperative,
                    Some(CooperativeOptions::default()),
                )
                .await?;
            let task = match task {
                layeragent_storage::TaskCreate::Created(t)
                | layeragent_storage::TaskCreate::Replayed(t) => t.id,
            };
            Ok(Self {
                _home: home,
                workspace,
                state: ApiState::new(engine, true),
                task,
            })
        }
        fn app(&self, read_only: bool, principal: &str, workspace_allowed: bool) -> Router {
            let identity = Identity {
                scope: Scope {
                    tenant: "default".into(),
                    principal: principal.into(),
                },
                read_only,
                owner: false,
                workspaces: Some(if workspace_allowed {
                    vec![self.workspace.path().into()]
                } else {
                    vec![]
                }),
                authority: CancellationToken::new(),
            };
            // Exercise the same handlers and authenticated identity contract as the
            // public router, without process-global environment credentials.
            Router::new()
                .route("/v1/tasks/{id}/team", get(snapshot))
                .route("/v1/tasks/{id}/team/export", get(export))
                .route("/v1/tasks/{id}/team/messages", get(messages).post(message))
                .route("/v1/tasks/{id}/team/commands/{action}", post(command))
                .route("/v1/tasks/{id}/team/{resource}", get(resource))
                .layer(axum::Extension(identity))
                .with_state(self.state.clone())
        }
        async fn call(
            &self,
            app: Router,
            method: &str,
            suffix: &str,
            body: Value,
        ) -> Result<(StatusCode, Value)> {
            let response = app
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(format!("/v1/tasks/{}/team{suffix}", self.task))
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))?,
                )
                .await?;
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await?;
            Ok((status, serde_json::from_slice(&bytes)?))
        }
    }

    #[tokio::test]
    async fn team_routes_enforce_identity_workspace_and_read_only_authority() -> Result<()> {
        let f = Fixture::new().await?;
        let principal = Scope::local().principal;
        for suffix in [
            "",
            "/export",
            "/messages",
            "/deliveries",
            "/finding_revisions",
            "/events",
        ] {
            assert_eq!(
                f.call(f.app(true, &principal, true), "GET", suffix, json!({}))
                    .await?
                    .0,
                StatusCode::OK
            );
            assert_eq!(
                f.call(f.app(true, "foreign", true), "GET", suffix, json!({}))
                    .await?
                    .0,
                StatusCode::NOT_FOUND
            );
            assert!(
                !f.call(f.app(true, &principal, false), "GET", suffix, json!({}))
                    .await?
                    .0
                    .is_success()
            );
        }
        let input = json!({"idempotency_key":"message","expected_revision":1,"recipient":null,"purpose":"user_instruction","body":"check this"});
        assert_eq!(
            f.call(f.app(true, &principal, true), "POST", "/messages", input)
                .await?
                .0,
            StatusCode::FORBIDDEN
        );
        let command = json!({"idempotency_key":"pause","expected_revision":1});
        assert_eq!(
            f.call(
                f.app(true, &principal, true),
                "POST",
                "/commands/pause",
                command
            )
            .await?
            .0,
            StatusCode::FORBIDDEN
        );
        Ok(())
    }

    #[tokio::test]
    async fn team_routes_preserve_revision_conflicts_and_message_dispositions() -> Result<()> {
        let f = Fixture::new().await?;
        let principal = Scope::local().principal;
        let (_, snapshot) = f
            .call(f.app(false, &principal, true), "GET", "", json!({}))
            .await?;
        assert!(snapshot["team"]["last_event_seq"].is_number());
        let revision = snapshot["team"]["revision"]
            .as_i64()
            .expect("team revision");
        let input = json!({"idempotency_key":"message","expected_revision":revision,"recipient":null,"purpose":"user_instruction","body":"inspect first"});
        let (status, receipt) = f
            .call(
                f.app(false, &principal, true),
                "POST",
                "/messages",
                input.clone(),
            )
            .await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(receipt["state"], "accepted");
        let (_, replay) = f
            .call(
                f.app(false, &principal, true),
                "POST",
                "/messages",
                input.clone(),
            )
            .await?;
        assert_eq!(receipt, replay);
        let mut conflicting = input;
        conflicting["body"] = json!("changed content");
        assert_eq!(
            f.call(
                f.app(false, &principal, true),
                "POST",
                "/messages",
                conflicting
            )
            .await?
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            f.call(
                f.app(false, &principal, true),
                "POST",
                "/commands/pause",
                json!({"idempotency_key":"pause","expected_revision":revision})
            )
            .await?
            .0,
            StatusCode::CONFLICT
        );
        let (_, snapshot) = f
            .call(f.app(false, &principal, true), "GET", "", json!({}))
            .await?;
        assert_eq!(f.call(f.app(false,&principal,true),"POST","/commands/pause",json!({"idempotency_key":"target","expected_revision":snapshot["team"]["revision"],"recipient":"analyst"})).await?.0,StatusCode::BAD_REQUEST);
        let (_, receipts) = f
            .call(
                f.app(false, &principal, true),
                "GET",
                "/deliveries?limit=1",
                json!({}),
            )
            .await?;
        assert_eq!(receipts["items"].as_array().expect("items").len(), 1);
        assert!(receipts["next"].is_number());
        let (_, export) = f
            .call(f.app(true, &principal, true), "GET", "/export", json!({}))
            .await?;
        assert_eq!(
            export["deliveries"].as_array().expect("deliveries").len(),
            4
        );
        assert!(export["events"].is_array() && export["finding_revisions"].is_array());
        Ok(())
    }
}

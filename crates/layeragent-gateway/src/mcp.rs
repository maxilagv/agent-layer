use anyhow::{Context, Result};
use async_trait::async_trait;
use rmcp::{ErrorData, RoleServer, ServerHandler, model::*, service::RequestContext};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[async_trait]
pub trait McpBackend: Send + Sync {
    async fn invoke(
        &self,
        name: &str,
        arguments: Value,
        cancel: CancellationToken,
    ) -> Result<Value>;
}
#[derive(Clone)]
pub struct McpService {
    pub backend: Arc<dyn McpBackend>,
}
fn tools() -> Vec<Tool> {
    vec![
    Tool::new("layeragent_run","Run or resume a LayerAgent engineering session",json!({"type":"object","properties":{"prompt":{"type":"string"},"workspace":{"type":"string"},"session_id":{"type":"string"},"model":{"type":"string"}},"required":["prompt"],"additionalProperties":false}).as_object().cloned().unwrap_or_default()),
    Tool::new("layeragent_sessions","List the operator's sessions",json!({"type":"object","properties":{},"additionalProperties":false}).as_object().cloned().unwrap_or_default()),
]
}
impl ServerHandler for McpService {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = Implementation::new("layeragent", env!("CARGO_PKG_VERSION"));
        info
    }
    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools().into_iter().find(|t| t.name == name)
    }
    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> std::result::Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: tools(),
            ..Default::default()
        })
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResponse, ErrorData> {
        let args = Value::Object(request.arguments.unwrap_or_default());
        match self.backend.invoke(&request.name, args, context.ct).await {
            Ok(value) => {
                Ok(CallToolResult::success(vec![ContentBlock::text(value.to_string())]).into())
            }
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(
                layeragent_protocol::redact::redact_text(&error.to_string()),
            )])
            .into()),
        }
    }
}
pub struct EngineBackend(pub Arc<layeragent_core::Engine>);
#[async_trait]
impl McpBackend for EngineBackend {
    async fn invoke(
        &self,
        name: &str,
        arguments: Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let scope = layeragent_protocol::Scope::local();
        match name {
            "layeragent_run" => {
                let request: layeragent_protocol::RunRequest =
                    serde_json::from_value(arguments).context("invalid run parameters")?;
                Ok(serde_json::to_value(
                    self.0.run(scope, request, cancel).await?,
                )?)
            }
            "layeragent_sessions" => Ok(json!(
                self.0
                    .store(&scope)
                    .await?
                    .sessions(&scope.principal)
                    .await?
            )),
            _ => anyhow::bail!("unknown LayerAgent MCP tool"),
        }
    }
}

/// HTTP entry point shares the gateway's run quota with REST jobs.
pub(crate) struct LimitedBackend {
    pub(crate) engine: Arc<layeragent_core::Engine>,
    pub(crate) admission: crate::admission::Admission,
}
#[async_trait]
impl McpBackend for LimitedBackend {
    async fn invoke(
        &self,
        name: &str,
        arguments: Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let _lease = if name == "layeragent_run" {
            Some(
                self.admission
                    .job(&layeragent_protocol::Scope::local())
                    .map_err(|_| anyhow::anyhow!("gateway running job limit reached"))?,
            )
        } else {
            None
        };
        EngineBackend(self.engine.clone())
            .invoke(name, arguments, cancel)
            .await
    }
}

//! Application-owned MCP adapter. Enforcement lives in the file service itself.
use crate::{McpGrant, file_catalog};
use rmcp::model::*;
#[cfg(target_os = "linux")]
use rmcp::{ErrorData, RoleServer, ServerHandler, service::RequestContext};
#[cfg(target_os = "linux")]
use std::sync::Arc;

#[cfg(target_os = "linux")]
#[derive(Clone)]
pub struct ProjectFiles {
    pub broker: Arc<crate::files::FileBroker>,
    pub grant: McpGrant,
    pub cancel: tokio_util::sync::CancellationToken,
}
#[cfg(target_os = "linux")]
impl ServerHandler for ProjectFiles {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info =
            Implementation::new("layeragent-project-files", env!("CARGO_PKG_VERSION"));
        info
    }
    fn get_tool(&self, name: &str) -> Option<Tool> {
        if self.cancel.is_cancelled() || self.broker.check_source().is_err() {
            return None;
        }
        tools(&self.grant).into_iter().find(|t| t.name == name)
    }
    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        if self.cancel.is_cancelled() || self.broker.check_source().is_err() {
            return Err(ErrorData::invalid_request(
                "MCP authority changed; owner review required",
                None,
            ));
        }
        Ok(ListToolsResult {
            tools: tools(&self.grant),
            ..Default::default()
        })
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if context.ct.is_cancelled() {
            return Err(ErrorData::invalid_request("cancelled", None));
        }
        let args = serde_json::Value::Object(request.arguments.unwrap_or_default());
        let broker = self.broker.clone();
        let scope = self.grant.scope.clone();
        let cancel = self.cancel.clone();
        let request_cancel = context.ct.clone();
        let name = request.name.to_string();
        let result = tokio::task::spawn_blocking(move || {
            broker.invoke_checked(&scope, &name, &args, || {
                anyhow::ensure!(
                    !cancel.is_cancelled() && !request_cancel.is_cancelled(),
                    "MCP authorization cancelled"
                );
                Ok(())
            })
        })
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("MCP broker worker failed")));
        Ok(match result {
            Ok(v)=>CallToolResult::success(vec![ContentBlock::text(v.to_string())]),
            Err(_)=>CallToolResult::error(vec![ContentBlock::text("Project file operation rejected or failed; inspect local audit before retrying a mutation")]),
        }.into())
    }
}
pub fn tools(grant: &McpGrant) -> Vec<Tool> {
    file_catalog()
        .into_iter()
        .filter(|v| {
            grant
                .tools
                .contains_key(v["name"].as_str().unwrap_or_default())
        })
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::{Operation, ToolGrant, catalog_hash};
    use layeragent_protocol::Scope;
    use rmcp::ServiceExt;
    use std::collections::{BTreeMap, BTreeSet};
    #[tokio::test]
    async fn real_mcp_transport_cannot_turn_read_permission_into_a_write() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a"), "original").unwrap();
        let grant = McpGrant {
            scope: Scope::local(),
            project_id: "test".into(),
            root: root.path().into(),
            revision: 1,
            catalog_sha256: catalog_hash(),
            tools: BTreeMap::from([(
                "read_file".into(),
                ToolGrant {
                    operations: BTreeSet::from([Operation::Read]),
                    destinations: BTreeSet::from([".".into()]),
                },
            )]),
        };
        let adapter = ProjectFiles {
            broker: Arc::new(crate::files::FileBroker::open(grant.clone()).unwrap()),
            grant,
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        let (client, server) = tokio::io::duplex(8192);
        let (server, client) = tokio::join!(adapter.serve(server), ().serve(client));
        let (server, client) = (server.unwrap(), client.unwrap());
        let tools = client.list_tools(None).await.unwrap().tools;
        assert_eq!(tools.len(), 1);
        let request = CallToolRequestParams::new("write_file").with_arguments(
            serde_json::json!({"path":"a","content":"unauthorized"})
                .as_object()
                .unwrap()
                .clone(),
        );
        let response = client.call_tool(request).await;
        assert!(
            response.is_err()
                || serde_json::to_value(response.unwrap()).unwrap()["isError"] == true
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("a")).unwrap(),
            "original"
        );
        let _ = client.cancel().await;
        let _ = server.cancel().await;
    }
}

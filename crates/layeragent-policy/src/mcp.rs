//! Application-owned MCP adapter. Enforcement lives in the file service itself.
use crate::{McpGrant, file_catalog};
use rmcp::model::*;
#[cfg(target_os = "linux")]
use rmcp::{ErrorData, RoleServer, ServerHandler, service::RequestContext};
#[cfg(target_os = "linux")]
use std::sync::Arc;

/// Resources are listed and read through the same file broker and the same
/// read grant as the file tools. A resource never widens the grant.
#[cfg(target_os = "linux")]
const RESOURCE_PAGE: usize = 100;

#[cfg(target_os = "linux")]
#[derive(Clone)]
pub struct ProjectFiles {
    pub broker: Arc<crate::files::FileBroker>,
    pub grant: McpGrant,
    pub cancel: tokio_util::sync::CancellationToken,
}
#[cfg(target_os = "linux")]
impl ProjectFiles {
    fn resource_uri(&self, path: &str) -> String {
        format!("file://{}/{}", self.grant.project_id, path)
    }
    fn denied() -> ErrorData {
        ErrorData::invalid_request(
            "project resources are not readable under the current grant",
            None,
        )
    }
}
#[cfg(target_os = "linux")]
impl ServerHandler for ProjectFiles {
    fn get_info(&self) -> ServerConfig {
        let mut capabilities = ServerCapabilities::builder().enable_tools().build();
        // Resources are advertised only when a read grant can serve them.
        if self.grant.read_destinations().is_ok() {
            capabilities.resources = Some(ResourcesCapability::default());
        }
        let mut info = ServerConfig::default();
        info.capabilities = capabilities;
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
    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        if self.cancel.is_cancelled() || self.broker.check_source().is_err() {
            return Err(ErrorData::invalid_request(
                "MCP authority changed; owner review required",
                None,
            ));
        }
        if self.grant.read_destinations().is_err() {
            return Err(ErrorData::invalid_request(
                "project resources are not available under the current grant",
                None,
            ));
        }
        let offset = match request.and_then(|request| request.cursor) {
            None => 0,
            // An unreadable cursor must not silently restart at the first
            // page: a client would re-read entries and could loop forever.
            Some(cursor) => cursor
                .parse::<usize>()
                .map_err(|_| ErrorData::invalid_params("invalid resource cursor", None))?,
        };
        let broker = self.broker.clone();
        let scope = self.grant.scope.clone();
        let cancel = self.cancel.clone();
        let request_cancel = context.ct.clone();
        let entries = tokio::task::spawn_blocking(move || {
            broker.resource_catalog(&scope, || {
                anyhow::ensure!(
                    !cancel.is_cancelled() && !request_cancel.is_cancelled(),
                    "MCP authorization cancelled"
                );
                Ok(())
            })
        })
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("MCP broker worker failed")))
        .map_err(|_| Self::denied())?;
        let page = entries
            .iter()
            .skip(offset)
            .take(RESOURCE_PAGE)
            .map(|entry| {
                let path = entry["path"].as_str().unwrap_or_default();
                let mut resource = Resource::new(self.resource_uri(path), path);
                resource.mime_type = Some("text/plain".into());
                resource.size = entry["bytes"].as_u64();
                resource.description =
                    Some("Project file exposed through the reviewed project grant".into());
                resource
            })
            .collect::<Vec<Resource>>();
        let consumed = offset.saturating_add(page.len());
        Ok(ListResourcesResult {
            resources: page,
            next_cursor: (consumed < entries.len()).then(|| consumed.to_string()),
            ..Default::default()
        })
    }
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        if self.cancel.is_cancelled() || self.broker.check_source().is_err() {
            return Err(ErrorData::invalid_request(
                "MCP authority changed; owner review required",
                None,
            ));
        }
        let path = self
            .grant
            .authorize_resource(&self.grant.scope, &request.uri)
            .map_err(|_| Self::denied())?;
        let broker = self.broker.clone();
        let scope = self.grant.scope.clone();
        let cancel = self.cancel.clone();
        let request_cancel = context.ct.clone();
        let value = tokio::task::spawn_blocking(move || {
            broker.read_resource(&scope, &path, || {
                anyhow::ensure!(
                    !cancel.is_cancelled() && !request_cancel.is_cancelled(),
                    "MCP authorization cancelled"
                );
                Ok(())
            })
        })
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("MCP broker worker failed")))
        .map_err(|_| Self::denied())?;
        let text = value["text"].as_str().unwrap_or_default().to_string();
        let text = if value["truncated"].as_bool().unwrap_or(false) {
            // Keep the whole response inside the same 64 KiB budget while
            // telling the client where the rest of the file is available.
            const NOTE: &str = "\n\n[resource truncated at 64 KiB; read the remaining content with the file tool's offset and limit]";
            let budget = crate::files::RESOURCE_READ_LIMIT.saturating_sub(NOTE.len());
            let mut cut = budget.min(text.len());
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            let mut bounded = text[..cut].to_string();
            bounded.push_str(NOTE);
            bounded
        } else {
            text
        };
        Ok(ReadResourceResult::new(vec![ResourceContents::text(text, request.uri)]).into())
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

    async fn adapter(
        grant: McpGrant,
    ) -> (
        rmcp::service::RunningService<rmcp::RoleClient, ()>,
        rmcp::service::RunningService<rmcp::RoleServer, ProjectFiles>,
    ) {
        let adapter = ProjectFiles {
            broker: Arc::new(crate::files::FileBroker::open(grant.clone()).unwrap()),
            grant,
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        let (client_io, server_io) = tokio::io::duplex(8192);
        let (server, client) = tokio::join!(adapter.serve(server_io), ().serve(client_io));
        (client.unwrap(), server.unwrap())
    }

    fn read_grant(root: &std::path::Path, destinations: BTreeSet<String>) -> McpGrant {
        McpGrant {
            scope: Scope::local(),
            project_id: "test".into(),
            root: root.into(),
            revision: 1,
            catalog_sha256: catalog_hash(),
            tools: BTreeMap::from([(
                "read_file".into(),
                ToolGrant {
                    operations: BTreeSet::from([Operation::Read]),
                    destinations,
                },
            )]),
        }
    }

    #[tokio::test]
    async fn resources_are_listed_and_read_through_the_same_grant() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.txt"), "hello resource").unwrap();
        std::fs::create_dir(root.path().join("docs")).unwrap();
        std::fs::write(root.path().join("docs").join("b.txt"), "second").unwrap();
        let grant = read_grant(root.path(), BTreeSet::from([".".into()]));
        let (client, server) = adapter(grant).await;
        let listed = client.list_resources(None).await.unwrap();
        let mut names = listed
            .resources
            .iter()
            .map(|r| r.uri.clone())
            .collect::<Vec<String>>();
        names.sort();
        assert_eq!(
            names,
            vec![
                "file://test/a.txt".to_string(),
                "file://test/docs/b.txt".to_string()
            ]
        );
        let contents = client
            .read_resource(ReadResourceRequestParams::new("file://test/a.txt"))
            .await
            .unwrap();
        let text = contents.contents[0].clone();
        match text {
            ResourceContents::TextResourceContents { text, .. } => {
                assert_eq!(text, "hello resource")
            }
            other => panic!("unexpected resource contents {other:?}"),
        }
        let _ = client.cancel().await;
        let _ = server.cancel().await;
    }

    #[tokio::test]
    async fn resources_outside_the_grant_are_refused() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("secret.txt"), "hidden").unwrap();
        std::fs::create_dir(root.path().join("docs")).unwrap();
        std::fs::write(root.path().join("docs").join("visible.txt"), "shown").unwrap();
        let grant = read_grant(root.path(), BTreeSet::from(["docs".into()]));
        let (client, server) = adapter(grant).await;
        let listed = client.list_resources(None).await.unwrap();
        let names = listed
            .resources
            .iter()
            .map(|r| r.uri.clone())
            .collect::<Vec<String>>();
        assert_eq!(names, vec!["file://test/docs/visible.txt".to_string()]);
        let denied = client
            .read_resource(ReadResourceRequestParams::new("file://test/secret.txt"))
            .await;
        assert!(denied.is_err());
        let _ = client.cancel().await;
        let _ = server.cancel().await;
    }

    #[tokio::test]
    async fn resource_reads_are_bounded_to_64_kib() {
        let root = tempfile::tempdir().unwrap();
        let big = "x".repeat(70 * 1024);
        std::fs::write(root.path().join("big.txt"), &big).unwrap();
        let grant = read_grant(root.path(), BTreeSet::from([".".into()]));
        let (client, server) = adapter(grant).await;
        let contents = client
            .read_resource(ReadResourceRequestParams::new("file://test/big.txt"))
            .await
            .unwrap();
        match contents.contents[0].clone() {
            ResourceContents::TextResourceContents { text, .. } => {
                assert!(text.len() <= crate::files::RESOURCE_READ_LIMIT);
                assert_eq!(text.len(), crate::files::RESOURCE_READ_LIMIT);
                assert!(text.contains("offset and limit"));
            }
            other => panic!("unexpected resource contents {other:?}"),
        }
        let _ = client.cancel().await;
        let _ = server.cancel().await;
    }

    #[tokio::test]
    async fn multi_byte_resources_stay_within_the_byte_budget() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("utf8.txt"), "é".repeat(40_000)).unwrap();
        let grant = read_grant(root.path(), BTreeSet::from([".".into()]));
        let (client, server) = adapter(grant).await;
        let contents = client
            .read_resource(ReadResourceRequestParams::new("file://test/utf8.txt"))
            .await
            .unwrap();
        match contents.contents[0].clone() {
            ResourceContents::TextResourceContents { text, .. } => {
                assert!(text.len() <= crate::files::RESOURCE_READ_LIMIT);
                assert!(text.contains("offset and limit"));
            }
            other => panic!("unexpected resource contents {other:?}"),
        }
        let _ = client.cancel().await;
        let _ = server.cancel().await;
    }

    #[tokio::test]
    async fn resources_are_paginated_and_an_invalid_cursor_is_refused() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..(RESOURCE_PAGE + 25) {
            std::fs::write(root.path().join(format!("f{index:03}.txt")), "x").unwrap();
        }
        let grant = read_grant(root.path(), BTreeSet::from([".".into()]));
        let (client, server) = adapter(grant).await;
        let first = client.list_resources(None).await.unwrap();
        assert_eq!(first.resources.len(), RESOURCE_PAGE);
        let cursor = first.next_cursor.expect("a second page");
        let second = client
            .list_resources(Some(
                PaginatedRequestParams::default().with_cursor(Some(cursor)),
            ))
            .await
            .unwrap();
        assert_eq!(second.resources.len(), 25);
        assert!(second.next_cursor.is_none());
        let mut seen = first
            .resources
            .iter()
            .chain(second.resources.iter())
            .map(|resource| resource.uri.clone())
            .collect::<Vec<_>>();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), RESOURCE_PAGE + 25);
        let invalid = client
            .list_resources(Some(
                PaginatedRequestParams::default().with_cursor(Some("not-a-cursor".into())),
            ))
            .await;
        assert!(invalid.is_err());
        let _ = client.cancel().await;
        let _ = server.cancel().await;
    }

    #[tokio::test]
    async fn prohibited_resource_uris_are_refused() {
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), "outside").unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("ok.txt"), "ok").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        let grant = read_grant(root.path(), BTreeSet::from([".".into()]));
        let (client, server) = adapter(grant).await;
        for uri in ["file://test/escape", "file://test/../outside.txt"] {
            let denied = client
                .read_resource(ReadResourceRequestParams::new(uri))
                .await;
            assert!(denied.is_err(), "{uri} must not be readable");
        }
        let _ = client.cancel().await;
        let _ = server.cancel().await;
    }

    #[tokio::test]
    async fn a_project_without_a_read_grant_serves_no_resources() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.txt"), "hello").unwrap();
        let mut grant = read_grant(root.path(), BTreeSet::from([".".into()]));
        grant.tools.clear();
        let (client, server) = adapter(grant).await;
        // The capability is not announced, so a client cannot rely on it.
        let capabilities = &client.peer_info().unwrap().capabilities;
        assert!(capabilities.resources.is_none());
        let listed = client.list_resources(None).await;
        assert!(listed.is_err());
        let denied = client
            .read_resource(ReadResourceRequestParams::new("file://test/a.txt"))
            .await;
        assert!(denied.is_err());
        let _ = client.cancel().await;
        let _ = server.cancel().await;
    }

    #[tokio::test]
    async fn resource_capability_is_renegotiated_on_reconnect() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.txt"), "hello").unwrap();
        let mut denied = read_grant(root.path(), BTreeSet::from([".".into()]));
        denied.tools.clear();
        let (client, server) = adapter(denied).await;
        assert!(
            client.peer_info().unwrap().capabilities.resources.is_none(),
            "a connection without a read grant must not advertise resources"
        );
        assert!(client.list_resources(None).await.is_err());
        let _ = client.cancel().await;
        let _ = server.cancel().await;

        // A reconnect with the reviewed read grant negotiates the capability
        // again; nothing from the previous connection's catalog is assumed.
        let grant = read_grant(root.path(), BTreeSet::from([".".into()]));
        let (client, server) = adapter(grant).await;
        assert!(
            client.peer_info().unwrap().capabilities.resources.is_some(),
            "a reviewed read grant must advertise resources"
        );
        assert_eq!(
            client.list_resources(None).await.unwrap().resources.len(),
            1
        );
        let _ = client.cancel().await;
        let _ = server.cancel().await;
    }
}

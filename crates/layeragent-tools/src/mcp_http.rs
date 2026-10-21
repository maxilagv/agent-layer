//! MCP HTTP boundary: one configured destination, no redirects, bounded response bodies.
use anyhow::{Result, ensure};
use futures_util::{StreamExt, stream::BoxStream};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use rmcp::{model::*, transport::streamable_http_client::*};
use std::{collections::HashMap, io, sync::Arc, time::Duration};

#[derive(Clone)]
pub(crate) struct BoundedClient {
    client: reqwest::Client,
    uri: Arc<str>,
    limit: usize,
}
fn err(message: &'static str) -> StreamableHttpError<io::Error> {
    StreamableHttpError::Client(io::Error::other(message))
}
impl BoundedClient {
    pub(crate) fn new(uri: &str, limit: usize, timeout_secs: u64) -> Result<Self> {
        let url = reqwest::Url::parse(uri)?;
        ensure!(
            url.username().is_empty() && url.password().is_none() && url.fragment().is_none(),
            "MCP URL credentials and fragments are forbidden"
        );
        ensure!(
            url.scheme() == "https"
                || (url.scheme() == "http" && url.host_str() == Some("127.0.0.1")),
            "MCP requires HTTPS or explicit IPv4 loopback"
        );
        Ok(Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(timeout_secs.min(300)))
                .build()?,
            uri: uri.into(),
            limit,
        })
    }
    fn request(
        &self,
        method: reqwest::Method,
        uri: &str,
        session: Option<&str>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<reqwest::RequestBuilder, StreamableHttpError<io::Error>> {
        if uri != self.uri.as_ref() {
            return Err(err("MCP destination changed"));
        }
        // rmcp manages protocol headers; custom Host/session/auth overrides are rejected.
        for key in headers.keys() {
            if matches!(
                key.as_str(),
                "host"
                    | "authorization"
                    | "mcp-session-id"
                    | "content-length"
                    | "transfer-encoding"
            ) {
                return Err(err("reserved MCP header override denied"));
            }
        }
        let mut request = self
            .client
            .request(method, uri)
            .headers(HeaderMap::from_iter(headers))
            .header("accept", "application/json, text/event-stream");
        if let Some(session) = session {
            request = request.header("mcp-session-id", session);
        }
        if let Some(auth) = auth {
            request = request.bearer_auth(auth);
        }
        Ok(request)
    }
    async fn bounded_body(
        &self,
        response: reqwest::Response,
    ) -> Result<Vec<u8>, StreamableHttpError<io::Error>> {
        if response
            .content_length()
            .is_some_and(|n| n > self.limit as u64)
        {
            return Err(err("MCP response exceeds byte limit"));
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| err("MCP response read failed"))?;
            if bytes.len().saturating_add(chunk.len()) > self.limit {
                return Err(err("MCP response exceeds byte limit"));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
    fn sse(
        &self,
        response: reqwest::Response,
    ) -> BoxStream<'static, Result<sse_stream::Sse, SseError>> {
        let limit = self.limit;
        let mut count = 0usize;
        let bytes = response.bytes_stream().map(move |chunk| {
            let chunk = chunk.map_err(|_| io::Error::other("MCP stream read failed"))?;
            count = count.saturating_add(chunk.len());
            if count > limit {
                return Err(io::Error::other("MCP stream exceeds byte limit"));
            }
            Ok(chunk)
        });
        sse_stream::SseStream::from_bytes_stream(bytes).boxed()
    }
}
impl StreamableHttpClient for BoundedClient {
    type Error = io::Error;
    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        let notification = matches!(
            message,
            ClientJsonRpcMessage::Notification(_)
                | ClientJsonRpcMessage::Response(_)
                | ClientJsonRpcMessage::Error(_)
        );
        let response = self
            .request(
                reqwest::Method::POST,
                &uri,
                session_id.as_deref(),
                auth,
                headers,
            )?
            .json(&message)
            .send()
            .await
            .map_err(|_| err("MCP request failed"))?;
        let status = response.status();
        if matches!(status.as_u16(), 202 | 204) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        if status.as_u16() == 404 && session_id.is_some() {
            return Err(StreamableHttpError::SessionExpired);
        }
        if !status.is_success() {
            return Err(err("MCP HTTP request rejected; redirects are disabled"));
        }
        let session = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if content_type.starts_with("text/event-stream") {
            return Ok(StreamableHttpPostResponse::Sse(self.sse(response), session));
        }
        if !content_type.starts_with("application/json")
            && !(notification && response.content_length() == Some(0))
        {
            return Err(err("unexpected MCP content type"));
        }
        let body = self.bounded_body(response).await?;
        if notification && body.is_empty() {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        let message =
            serde_json::from_slice(&body).map_err(|_| err("invalid MCP JSON response"))?;
        Ok(StreamableHttpPostResponse::Json(message, session))
    }
    async fn delete_session(
        &self,
        uri: Arc<str>,
        session: Arc<str>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        let response = self
            .request(reqwest::Method::DELETE, &uri, Some(&session), auth, headers)?
            .send()
            .await
            .map_err(|_| err("MCP session cleanup failed"))?;
        if response.status().is_success() || response.status().as_u16() == 405 {
            Ok(())
        } else {
            Err(err("MCP session cleanup rejected"))
        }
    }
    async fn get_stream(
        &self,
        uri: Arc<str>,
        session: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<
        BoxStream<'static, Result<sse_stream::Sse, SseError>>,
        StreamableHttpError<Self::Error>,
    > {
        let mut request = self.request(
            reqwest::Method::GET,
            &uri,
            session.as_deref(),
            auth,
            headers,
        )?;
        if let Some(id) = last_event_id {
            request = request.header("last-event-id", id);
        }
        let response = request
            .send()
            .await
            .map_err(|_| err("MCP stream request failed"))?;
        if response.status().as_u16() == 405 {
            return Err(StreamableHttpError::ServerDoesNotSupportSse);
        }
        if !response.status().is_success() {
            return Err(err("MCP stream request rejected"));
        }
        if !response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"))
        {
            return Err(err("unexpected MCP stream content type"));
        }
        Ok(self.sse(response))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    async fn server(response: String) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let uri = format!("http://{}/mcp", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            let _ = socket.read(&mut request).await.unwrap();
            let _ = socket.write_all(response.as_bytes()).await;
        });
        (uri, task)
    }
    #[tokio::test]
    async fn redirect_cannot_reach_another_private_endpoint() {
        let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let response = format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{}/secret\r\nContent-Length: 0\r\n\r\n",
            sink.local_addr().unwrap()
        );
        let (uri, task) = server(response).await;
        let client = BoundedClient::new(&uri, 1024, 2).unwrap();
        let message = serde_json::from_value(
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        )
        .unwrap();
        assert!(
            client
                .post_message(uri.into(), message, None, None, HashMap::new())
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), sink.accept())
                .await
                .is_err()
        );
        task.await.unwrap();
    }
    #[tokio::test]
    async fn json_limit_checks_chunked_bytes_without_trusting_length() {
        let body = "x".repeat(2048);
        let (uri, task) = server(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n800\r\n{body}\r\n0\r\n\r\n")).await;
        let client = BoundedClient::new(&uri, 1024, 2).unwrap();
        let message = serde_json::from_value(
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        )
        .unwrap();
        let error = client
            .post_message(uri.into(), message, None, None, HashMap::new())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("byte limit"), "{error}");
        task.await.unwrap();
    }
    #[tokio::test]
    async fn valid_json_and_sse_responses_still_work() {
        for mime in ["application/json", "text/event-stream"] {
            let json = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
            let body = if mime == "application/json" {
                json.to_string()
            } else {
                format!("data: {json}\n\n")
            };
            let (uri, task) = server(format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ))
            .await;
            let client = BoundedClient::new(&uri, 1024, 2).unwrap();
            let message = serde_json::from_value(
                serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            )
            .unwrap();
            match client
                .post_message(uri.into(), message, None, None, HashMap::new())
                .await
                .unwrap()
            {
                StreamableHttpPostResponse::Json(_, _) => assert_eq!(mime, "application/json"),
                StreamableHttpPostResponse::Sse(mut stream, _) => {
                    assert!(stream.next().await.unwrap().is_ok())
                }
                _ => panic!("response was lost"),
            }
            task.await.unwrap();
        }
    }
    #[test]
    fn rejects_credentials_and_ambiguous_or_cleartext_destinations() {
        for uri in [
            "https://user:secret@example.com/mcp",
            "http://localhost:1/mcp",
            "http://127.0.0.1.attacker/mcp",
            "https://example.com/mcp#fragment",
        ] {
            assert!(BoundedClient::new(uri, 1024, 2).is_err(), "{uri}");
        }
        assert!(BoundedClient::new("https://example.com/mcp", 1024, 2).is_ok());
    }
}

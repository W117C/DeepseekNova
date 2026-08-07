//! HTTP/SSE transport for MCP servers.
//!
//! MCP HTTP transport (Phase 2 — direct POST):
//!   Client POSTs JSON-RPC requests, receives JSON-RPC responses.
//!   Full persistent SSE streaming will be implemented in Phase 3.

use crate::types::*;
use anyhow::Context;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::info;

/// An MCP connection over HTTP transport.
///
/// In Phase 2 this uses direct POST requests. Phase 3 will add persistent
/// SSE connections for true streaming and server→client notifications.
pub struct McpHttpConnection {
    /// The endpoint URL for POST requests (discovered from SSE or configured).
    post_url: String,
    /// Next request ID.
    next_id: AtomicU64,
    /// Default timeout for requests.
    pub request_timeout: Duration,
    /// Server info from initialize.
    pub server_info: RwLock<ServerInfo>,
    /// Server capabilities.
    pub server_capabilities: RwLock<ServerCapabilities>,
    /// HTTP client.
    client: reqwest::Client,
}

impl McpHttpConnection {
    /// Connect to an MCP server over HTTP.
    ///
    /// `sse_url` is the SSE endpoint (e.g., `http://localhost:3000/sse`).
    /// In Phase 2, we first try the SSE endpoint to discover the POST URL,
    /// then fall back to using the SSE URL directly as the POST endpoint.
    pub async fn connect(sse_url: &str, request_timeout: Duration) -> anyhow::Result<Arc<Self>> {
        let client = reqwest::Client::builder()
            .timeout(request_timeout)
            .build()
            .context("failed to build HTTP client")?;

        // Try to discover the POST URL from SSE
        let post_url = match discover_post_url(&client, sse_url).await {
            Ok(url) => {
                info!("MCP HTTP: discovered POST endpoint: {url}");
                url
            }
            Err(_) => {
                // Fall back: use the SSE URL as the POST URL
                info!("MCP HTTP: using SSE URL as POST endpoint: {sse_url}");
                sse_url.to_string()
            }
        };

        let conn = Arc::new(Self {
            post_url,
            next_id: AtomicU64::new(1),
            request_timeout,
            server_info: RwLock::new(ServerInfo {
                name: "http-mcp".into(),
                version: String::new(),
            }),
            server_capabilities: RwLock::new(ServerCapabilities {
                tools: None,
                resources: None,
                prompts: None,
                logging: None,
                experimental: None,
            }),
            client,
        });

        Ok(conn)
    }

    /// Send a JSON-RPC request and wait for the response.
    pub async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout_dur: Duration,
    ) -> anyhow::Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id,
            method: method.into(),
            params,
        };

        let resp = self
            .client
            .post(&self.post_url)
            .json(&req)
            .timeout(timeout_dur)
            .send()
            .await
            .context("MCP HTTP POST failed")?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            let short_body: String = if body.len() > 500 {
                format!("{}…", &body[..500])
            } else {
                body
            };
            anyhow::bail!("MCP HTTP error {status}: {short_body}");
        }

        let val: Value =
            serde_json::from_str(&body).context("failed to parse MCP HTTP response")?;

        if let Some(err) = val.get("error") {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            anyhow::bail!("MCP error: {msg}");
        }

        Ok(val.get("result").cloned().unwrap_or(Value::Null))
    }
}

/// Try to discover the POST URL from an SSE endpoint.
async fn discover_post_url(client: &reqwest::Client, sse_url: &str) -> anyhow::Result<String> {
    let response = client
        .get(sse_url)
        .header("Accept", "text/event-stream")
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .context("failed to connect to MCP SSE endpoint")?;

    if !response.status().is_success() {
        let status = response.status();
        anyhow::bail!("MCP SSE connection failed: HTTP {status}");
    }

    let text = response
        .text()
        .await
        .context("failed to read SSE stream body")?;

    parse_sse_endpoint(&text).context("MCP SSE: no 'endpoint' event found in response")
}

/// Parse the `endpoint` event from an SSE stream chunk.
fn parse_sse_endpoint(text: &str) -> Option<String> {
    let mut expecting_endpoint = false;
    for line in text.lines() {
        let line = line.trim();
        if line == "event: endpoint" {
            expecting_endpoint = true;
            continue;
        }
        if expecting_endpoint {
            if let Some(url) = line.strip_prefix("data: ") {
                let url = url.trim();
                if url.starts_with("http://") || url.starts_with("https://") {
                    return Some(url.to_string());
                }
            }
            expecting_endpoint = false;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;

    // ── SSE endpoint discovery (pure parser) ────────────────────────

    #[test]
    fn parse_sse_endpoint_extracts_http_url() {
        let text = "event: endpoint\ndata: http://localhost:8080/mcp\n\n";
        assert_eq!(
            parse_sse_endpoint(text),
            Some("http://localhost:8080/mcp".to_string())
        );
    }

    #[test]
    fn parse_sse_endpoint_ignores_non_http_urls() {
        let text = "event: endpoint\ndata: ws://localhost:8080/mcp\n";
        assert_eq!(parse_sse_endpoint(text), None);
    }

    #[test]
    fn parse_sse_endpoint_requires_event_line() {
        let text = "data: http://localhost:8080/mcp\n";
        assert_eq!(parse_sse_endpoint(text), None);
    }

    #[test]
    fn parse_sse_endpoint_resets_on_non_data_line() {
        // Current behaviour: any line between `event: endpoint` and the
        // `data:` payload (e.g. a blank separator) resets the expectation.
        let text = "event: endpoint\n\ndata: http://localhost:8080/mcp\n";
        assert_eq!(parse_sse_endpoint(text), None);
    }

    // ── Minimal HTTP/1.1 mock server ───────────────────────────────

    /// Spawn a minimal HTTP/1.1 server that routes `GET` to `on_get` and any
    /// other method to `on_post`. `on_post` receives the full request text
    /// (request line + headers + body) so tests can assert what the client
    /// actually sent, and may return extra response headers (e.g. a session
    /// header). Returns the base URL and a handle to abort on teardown.
    fn spawn_http_mock<G, P>(on_get: G, on_post: P) -> (String, JoinHandle<()>)
    where
        G: Fn() -> (u16, String) + Send + Clone + 'static,
        P: Fn(String) -> (u16, String, Vec<(String, String)>) + Send + Clone + 'static,
    {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock listener");
        listener.set_nonblocking(true).expect("set nonblocking");
        let listener = TcpListener::from_std(listener).expect("into tokio listener");
        let addr = listener.local_addr().expect("mock address");
        let url = format!("http://{addr}");

        let handle = tokio::spawn(async move {
            loop {
                let (mut socket, _) = match listener.accept().await {
                    Ok(x) => x,
                    Err(_) => break,
                };
                let on_get = on_get.clone();
                let on_post = on_post.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(&mut socket);
                    let mut request_line = String::new();
                    if reader.read_line(&mut request_line).await.is_err() {
                        return;
                    }
                    let mut headers = Vec::new();
                    let mut content_length = 0usize;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).await.is_err() {
                            return;
                        }
                        if line == "\r\n" || line == "\n" {
                            break;
                        }
                        if let Some(rest) =
                            line.to_ascii_lowercase().strip_prefix("content-length:")
                        {
                            content_length = rest.trim().parse().unwrap_or(0);
                        }
                        headers.push(line.trim().to_string());
                    }
                    let mut body = vec![0u8; content_length];
                    let _ = reader.read_exact(&mut body).await;
                    let body_str = String::from_utf8_lossy(&body).to_string();

                    let (status, resp_body, extra_headers) = if request_line.starts_with("GET ") {
                        let (s, b) = on_get();
                        (s, b, Vec::new())
                    } else {
                        let full_request =
                            format!("{request_line}\n{}\n{body_str}", headers.join("\n"));
                        on_post(full_request)
                    };

                    let reason = match status {
                        200 => "OK",
                        404 => "Not Found",
                        500 => "Internal Server Error",
                        _ => "",
                    };
                    let mut response = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                        resp_body.len()
                    );
                    for (k, v) in extra_headers {
                        response.push_str(&format!("{k}: {v}\r\n"));
                    }
                    response.push_str("\r\n");
                    response.push_str(&resp_body);
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.flush().await;
                });
            }
        });

        (url, handle)
    }

    // ── Connection + request behaviour ─────────────────────────────

    #[tokio::test]
    async fn sse_discovery_routes_posts_to_discovered_endpoint() {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_capture = Arc::clone(&seen);
        // The discovered POST endpoint must point back at the mock server.
        let endpoint: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let endpoint_for_get = Arc::clone(&endpoint);

        let (url, server) = spawn_http_mock(
            move || {
                let ep = endpoint_for_get
                    .lock()
                    .unwrap()
                    .clone()
                    .expect("endpoint set");
                (200, format!("event: endpoint\ndata: {ep}\n\n"))
            },
            move |req_text| {
                seen_capture.lock().unwrap().push(req_text.clone());
                (
                    200,
                    r#"{"jsonrpc":"2.0","id":1,"result":{"pong":true}}"#.to_string(),
                    Vec::new(),
                )
            },
        );

        // Now that the listener is bound, the fake server can advertise itself.
        *endpoint.lock().unwrap() = Some(format!("{url}/mcp"));

        let conn = McpHttpConnection::connect(&format!("{url}/sse"), Duration::from_secs(5))
            .await
            .expect("connect");
        assert_eq!(conn.post_url, format!("{url}/mcp"));

        let result = conn
            .request("ping", None, Duration::from_secs(5))
            .await
            .expect("request");
        assert_eq!(result, json!({"pong": true}));
        server.abort();

        let reqs = seen.lock().unwrap();
        let post = reqs
            .iter()
            .find(|r| r.starts_with("POST "))
            .expect("client must POST to the discovered endpoint");
        assert!(
            post.contains("POST /mcp"),
            "unexpected request line: {post}"
        );
        assert!(
            post.to_ascii_lowercase()
                .contains("content-type: application/json"),
            "missing JSON content type: {post}"
        );
        assert!(
            post.contains("\"method\":\"ping\""),
            "missing method: {post}"
        );
    }

    #[tokio::test]
    async fn connect_falls_back_to_sse_url_when_discovery_fails() {
        let (url, server) = spawn_http_mock(
            || (404, "not found".to_string()),
            |_req_text| {
                (
                    200,
                    r#"{"jsonrpc":"2.0","id":1,"result":{"fallback":true}}"#.to_string(),
                    Vec::new(),
                )
            },
        );

        let conn = McpHttpConnection::connect(&format!("{url}/sse"), Duration::from_secs(5))
            .await
            .expect("connect falls back to SSE URL");
        assert_eq!(conn.post_url, format!("{url}/sse"));

        let result = conn
            .request("ping", None, Duration::from_secs(5))
            .await
            .expect("request via fallback URL");
        assert_eq!(result, json!({"fallback": true}));
        server.abort();
    }

    #[tokio::test]
    async fn http_error_status_surfaces_body() {
        let (url, server) = spawn_http_mock(
            || (404, "no sse".to_string()),
            |_req_text| (500, "server exploded".to_string(), Vec::new()),
        );

        let conn = McpHttpConnection::connect(&url, Duration::from_secs(5))
            .await
            .expect("connect");
        let err = conn
            .request("ping", None, Duration::from_secs(5))
            .await
            .expect_err("HTTP 500 must fail");
        assert!(
            err.to_string().contains("MCP HTTP error 500"),
            "unexpected error: {err}"
        );
        assert!(
            err.to_string().contains("server exploded"),
            "body should be included: {err}"
        );
        server.abort();
    }

    #[tokio::test]
    async fn http_json_rpc_error_object_fails() {
        let (url, server) = spawn_http_mock(
            || (404, "no sse".to_string()),
            |_req_text| {
                (
                    200,
                    r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}"#
                        .to_string(),
                    Vec::new(),
                )
            },
        );

        let conn = McpHttpConnection::connect(&url, Duration::from_secs(5))
            .await
            .expect("connect");
        let err = conn
            .request("nope", None, Duration::from_secs(5))
            .await
            .expect_err("JSON-RPC error object must fail");
        assert!(
            err.to_string().contains("MCP error: Method not found"),
            "unexpected error: {err}"
        );
        server.abort();
    }

    #[tokio::test]
    async fn http_invalid_json_body_fails_cleanly() {
        let (url, server) = spawn_http_mock(
            || (404, "no sse".to_string()),
            |_req_text| (200, "this is not json".to_string(), Vec::new()),
        );

        let conn = McpHttpConnection::connect(&url, Duration::from_secs(5))
            .await
            .expect("connect");
        let err = conn
            .request("ping", None, Duration::from_secs(5))
            .await
            .expect_err("invalid JSON body must fail");
        assert!(
            err.to_string()
                .contains("failed to parse MCP HTTP response"),
            "unexpected error: {err}"
        );
        server.abort();
    }

    #[tokio::test]
    async fn http_session_header_is_tolerated_and_not_persisted() {
        // A server that returns an `Mcp-Session-Id` header. The client must
        // not choke on it (returns the result), and must not send it back on
        // subsequent requests — session persistence is streamable-HTTP scope
        // (P2-3) and currently intentionally unsupported.
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_capture = Arc::clone(&seen);
        let (url, server) = spawn_http_mock(
            || (404, "no sse".to_string()),
            move |req_text| {
                seen_capture.lock().unwrap().push(req_text.clone());
                (
                    200,
                    r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#.to_string(),
                    vec![("Mcp-Session-Id".to_string(), "sess-abc-123".to_string())],
                )
            },
        );

        let conn = McpHttpConnection::connect(&url, Duration::from_secs(5))
            .await
            .expect("connect");
        for _ in 0..2 {
            conn.request("ping", None, Duration::from_secs(5))
                .await
                .expect("request succeeds despite session header");
        }
        server.abort();

        let reqs = seen.lock().unwrap();
        assert_eq!(reqs.len(), 2, "client should have sent two requests");
        for req in reqs.iter() {
            assert!(
                !req.to_ascii_lowercase().contains("mcp-session-id"),
                "client must not echo a session header: {req}"
            );
        }
    }
}

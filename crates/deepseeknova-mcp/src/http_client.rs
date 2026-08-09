//! HTTP transports for MCP servers.
//!
//! Two HTTP transports are supported:
//! - **Legacy SSE** — a `GET /sse` endpoint emits an `endpoint` event pointing
//!   at the message POST endpoint. Kept for backwards compatibility with older
//!   servers.
//! - **Streamable HTTP** (protocol 2025-03-26+) — a single endpoint accepts
//!   JSON-RPC over POST. Responses may be a single JSON document or an SSE
//!   event stream. Session state is carried in the `Mcp-Session-Id` header and
//!   the protocol version is negotiated during `initialize`.
//!
//! The transport is auto-detected at connect time: if the URL serves a legacy
//! SSE `endpoint` event that URL is used for POSTs; otherwise the URL itself is
//! treated as a streamable HTTP endpoint.

use crate::protocol;
use crate::types::*;
use deepseeknova_core::DeepseeknovaError;
use futures::StreamExt;
use reqwest::StatusCode;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};

/// Failure modes for a single HTTP request. Session expiry is reported
/// separately from other transport errors so [`McpHttpConnection::request`]
/// can attempt a reconnect before failing.
#[derive(Debug)]
enum SendError {
    /// The server signalled the session ended: an HTTP 404 carrying a
    /// `Mcp-Session-Id` header, or an empty `Mcp-Session-Id` value.
    SessionExpired,
    /// Any other transport or protocol failure.
    Other(DeepseeknovaError),
}

impl From<SendError> for DeepseeknovaError {
    fn from(e: SendError) -> Self {
        match e {
            SendError::SessionExpired => DeepseeknovaError::Runner("MCP session expired".into()),
            SendError::Other(err) => err,
        }
    }
}

/// An MCP connection over an HTTP transport (legacy SSE or streamable HTTP).
///
/// [`Self::connect`] runs the `initialize` handshake: the protocol version is
/// negotiated and the server's `Mcp-Session-Id` (if any) is captured. Every
/// subsequent request is a JSON-RPC POST that echoes the session id and
/// follows server-side session rotations.
pub struct McpHttpConnection {
    /// The endpoint URL for POST requests (discovered from legacy SSE or the
    /// URL itself for streamable HTTP).
    post_url: String,
    /// Next request ID.
    next_id: AtomicU64,
    /// Default timeout for requests.
    pub request_timeout: Duration,
    /// Server info from initialize.
    pub server_info: RwLock<ServerInfo>,
    /// Server capabilities.
    pub server_capabilities: RwLock<ServerCapabilities>,
    /// Negotiated protocol version (from the initialize response).
    protocol_version: RwLock<Option<String>>,
    /// The server session id (`Mcp-Session-Id`), resent on every request.
    session_id: RwLock<Option<String>>,
    /// Serializes session re-establishment so concurrent requests do not each
    /// run their own `initialize` handshake.
    reconnect_lock: Mutex<()>,
    /// Bumped after every successful reconnect; used to detect that another
    /// concurrent request already re-established the session.
    reconnect_generation: AtomicU64,
    /// HTTP client.
    client: reqwest::Client,
}

impl McpHttpConnection {
    /// Connect to an MCP server over HTTP.
    ///
    /// `url` is either a legacy SSE endpoint (e.g. `http://host/sse`) or a
    /// streamable HTTP endpoint (e.g. `http://host/mcp`). The transport is
    /// auto-detected: if the URL serves a legacy SSE `endpoint` event, the
    /// discovered URL is used for POST requests; otherwise the URL itself is
    /// used as the single streamable HTTP endpoint.
    ///
    /// The `initialize` handshake runs as part of connecting: the protocol
    /// version is negotiated and the server's `Mcp-Session-Id` (if any) is
    /// captured for subsequent requests.
    pub async fn connect(
        url: &str,
        request_timeout: Duration,
    ) -> Result<Arc<Self>, DeepseeknovaError> {
        let client = reqwest::Client::builder()
            .timeout(request_timeout)
            .build()
            .map_err(|e| DeepseeknovaError::Runner(format!("failed to build HTTP client: {e}")))?;

        // Try to discover a legacy SSE POST endpoint first. Servers without
        // one are streamable HTTP endpoints served at the URL itself.
        let post_url = match discover_post_url(&client, url).await {
            Ok(discovered) => {
                info!("MCP HTTP: discovered legacy SSE POST endpoint: {discovered}");
                discovered
            }
            Err(_) => {
                info!("MCP HTTP: no legacy SSE endpoint; treating URL as streamable HTTP: {url}");
                url.to_string()
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
            protocol_version: RwLock::new(None),
            session_id: RwLock::new(None),
            reconnect_lock: Mutex::new(()),
            reconnect_generation: AtomicU64::new(0),
            client,
        });

        conn.handshake(request_timeout).await?;

        Ok(conn)
    }

    /// The negotiated protocol version, if the handshake completed.
    pub async fn protocol_version(&self) -> Option<String> {
        self.protocol_version.read().await.clone()
    }

    /// The server session id currently held (`Mcp-Session-Id`), if any.
    pub async fn session_id(&self) -> Option<String> {
        self.session_id.read().await.clone()
    }

    /// Perform the MCP `initialize` handshake: negotiate the protocol version,
    /// store server info/capabilities, and capture the session id.
    async fn handshake(&self, request_timeout: Duration) -> Result<(), DeepseeknovaError> {
        let init_result = self
            .negotiate_initialize(request_timeout)
            .await
            .map_err(|e| DeepseeknovaError::Runner(format!("MCP initialize failed: {e}")))?;
        let init: InitializeResult = serde_json::from_value(init_result).map_err(|e| {
            DeepseeknovaError::Runner(format!("failed to parse MCP initialize result: {e}"))
        })?;
        *self.server_info.write().await = init.server_info;
        *self.server_capabilities.write().await = init.capabilities;
        *self.protocol_version.write().await = Some(init.protocol_version.clone());

        // Send the `notifications/initialized` notification (best-effort).
        self.send_notification("notifications/initialized", None, request_timeout)
            .await?;

        info!(
            "MCP connected: {} v{} (protocol {})",
            self.server_info.read().await.name,
            self.server_info.read().await.version,
            init.protocol_version
        );
        Ok(())
    }

    /// Send `initialize`, retrying with a lower protocol version if the server
    /// reports a version mismatch. Returns the `initialize` `result` value.
    async fn negotiate_initialize(
        &self,
        request_timeout: Duration,
    ) -> Result<Value, DeepseeknovaError> {
        let mut candidate = protocol::preferred_protocol_version().to_string();

        loop {
            let init_params = serde_json::to_value(InitializeRequest {
                protocol_version: candidate.clone(),
                capabilities: ClientCapabilities {
                    roots: Some(RootsCapability { list_changed: true }),
                    sampling: None,
                    experimental: None,
                },
                client_info: ClientInfo {
                    name: "deepseeknova".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                },
            })?;

            let resp = self
                .send_full("initialize", Some(init_params), request_timeout)
                .await?;

            if let Some(err) = resp.get("error") {
                if protocol::is_version_mismatch(&resp) {
                    let supported = protocol::extract_supported_versions(&resp);
                    if let Some(next) = protocol::highest_mutual(&supported) {
                        if next != candidate {
                            warn!("MCP server rejects protocol {candidate}; retrying with {next}");
                            candidate = next;
                            continue;
                        }
                    }
                    return Err(DeepseeknovaError::Runner(format!(
                        "MCP: no mutually supported protocol version (client {:?}, server {supported:?})",
                        protocol::SUPPORTED_PROTOCOL_VERSIONS
                    )));
                }
                let msg = err
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error");
                return Err(DeepseeknovaError::Runner(format!("MCP error: {msg}")));
            }

            return Ok(resp.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// Send a JSON-RPC request and return the `result` field, failing when the
    /// server answers with a JSON-RPC error object.
    ///
    /// Session expiry (an HTTP 404 carrying `Mcp-Session-Id`, or an empty
    /// `Mcp-Session-Id` value) triggers a single automatic reconnect: the
    /// `initialize` handshake is re-run to obtain a fresh session and the
    /// original request is retried once. Callers observe a successful response
    /// as if no expiry ever happened.
    pub async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout_dur: Duration,
    ) -> Result<Value, DeepseeknovaError> {
        // Capture the reconnect generation before the first attempt: after
        // waiting on the reconnect lock we can tell whether a *different*
        // concurrent request already re-established the session.
        let generation = self.reconnect_generation.load(Ordering::SeqCst);

        let resp = match self.send_full(method, params.clone(), timeout_dur).await {
            Ok(resp) => resp,
            Err(SendError::SessionExpired) => {
                self.reconnect(generation).await?;
                // Retry exactly once with the restored session. A second
                // expiry signal means the server keeps rejecting us.
                self.send_full(method, params, timeout_dur)
                    .await
                    .map_err(|e| match e {
                        SendError::SessionExpired => DeepseeknovaError::Runner(
                            "MCP session expired: reconnect did not restore the session".into(),
                        ),
                        SendError::Other(err) => err,
                    })?
            }
            Err(SendError::Other(e)) => return Err(e),
        };

        if let Some(err) = resp.get("error") {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            return Err(DeepseeknovaError::Runner(format!("MCP error: {msg}")));
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Re-establish an expired MCP session by re-running the `initialize`
    /// handshake.
    ///
    /// Exactly one reconnect is attempted per request. Concurrent requests
    /// that hit the same expiry signal are serialized through
    /// [`Self::reconnect_lock`]: only the first runs the handshake, and the
    /// rest observe the bumped [`Self::reconnect_generation`] and skip a
    /// redundant `initialize`.
    async fn reconnect(&self, generation_at_request: u64) -> Result<(), DeepseeknovaError> {
        let _guard = self.reconnect_lock.lock().await;

        // Another concurrent request already re-established the session while
        // we waited for the lock — nothing left to do.
        if self.reconnect_generation.load(Ordering::SeqCst) != generation_at_request {
            return Ok(());
        }

        info!("MCP session expired; re-running initialize");
        self.handshake(self.request_timeout)
            .await
            .map_err(|e| DeepseeknovaError::Runner(format!("MCP session reconnect failed: {e}")))?;
        self.reconnect_generation.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    /// POST a JSON-RPC request and return the parsed response object (which
    /// may carry an `error` field). Handles single-JSON and SSE-framed
    /// responses, and tracks the server's `Mcp-Session-Id` header.
    ///
    /// Session-expiry signals — an HTTP 404 carrying `Mcp-Session-Id`, or an
    /// empty `Mcp-Session-Id` value — are reported as
    /// [`SendError::SessionExpired`] so the caller can reconnect; every other
    /// failure is [`SendError::Other`].
    async fn send_full(
        &self,
        method: &str,
        params: Option<Value>,
        timeout_dur: Duration,
    ) -> Result<Value, SendError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id,
            method: method.into(),
            params,
        };

        let mut builder = self
            .client
            .post(&self.post_url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .json(&req)
            .timeout(timeout_dur);
        if let Some(session) = self.session_id.read().await.as_deref() {
            builder = builder.header("Mcp-Session-Id", session);
        }

        let resp = builder.send().await.map_err(|e| {
            SendError::Other(DeepseeknovaError::Runner(format!(
                "MCP HTTP POST failed: {e}"
            )))
        })?;

        let status = resp.status();
        let session_header = resp
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let content_type = resp
            .headers()
            .get("Content-Type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();
        let body = resp.text().await.unwrap_or_default();

        // Persist or update the session id, following server-side rotations.
        // An empty value — or a 404 carrying the header — terminates the
        // session and is reported as an expiry signal so the caller can
        // reconnect and retry.
        let mut expired = false;
        if let Some(sess) = session_header.as_ref() {
            if sess.is_empty() {
                *self.session_id.write().await = None;
                warn!("MCP server terminated the session (empty Mcp-Session-Id)");
                expired = true;
            } else {
                *self.session_id.write().await = Some(sess.clone());
            }
        }
        if status == StatusCode::NOT_FOUND {
            *self.session_id.write().await = None;
            if session_header.is_some() {
                expired = true;
            } else {
                return Err(SendError::Other(DeepseeknovaError::Runner(
                    "MCP HTTP error 404: not found".into(),
                )));
            }
        }
        if expired {
            return Err(SendError::SessionExpired);
        }

        if !status.is_success() {
            // Truncate on a char boundary so multi-byte bodies cannot panic.
            let short_body = if body.len() > 500 {
                let end = body.floor_char_boundary(500);
                format!("{}…", &body[..end])
            } else {
                body.clone()
            };
            return Err(SendError::Other(DeepseeknovaError::Runner(format!(
                "MCP HTTP error {status}: {short_body}"
            ))));
        }

        let val: Value = if content_type.contains("text/event-stream") {
            parse_sse_response(&body, id).ok_or_else(|| {
                SendError::Other(DeepseeknovaError::Runner(format!(
                    "MCP SSE response: no event matched request id {id}"
                )))
            })?
        } else {
            serde_json::from_str(&body).map_err(|e| {
                SendError::Other(DeepseeknovaError::Runner(format!(
                    "failed to parse MCP HTTP response: {e}"
                )))
            })?
        };

        Ok(val)
    }

    /// POST a JSON-RPC notification (no id). The response is expected to be an
    /// empty 2xx (202 for streamable HTTP); its body is ignored.
    async fn send_notification(
        &self,
        method: &str,
        params: Option<Value>,
        timeout_dur: Duration,
    ) -> Result<(), DeepseeknovaError> {
        let notif = JsonRpcNotification {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params,
        };
        let mut builder = self
            .client
            .post(&self.post_url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .json(&notif)
            .timeout(timeout_dur);
        if let Some(session) = self.session_id.read().await.as_deref() {
            builder = builder.header("Mcp-Session-Id", session);
        }

        let resp = builder.send().await.map_err(|e| {
            DeepseeknovaError::Runner(format!("MCP HTTP notification POST failed: {e}"))
        })?;
        let status = resp.status();
        if status != StatusCode::OK
            && status != StatusCode::ACCEPTED
            && status != StatusCode::NO_CONTENT
        {
            warn!("MCP notification {method} returned HTTP {status}");
        }
        Ok(())
    }
}

/// Try to discover the legacy SSE POST endpoint by streaming the SSE endpoint
/// until the `endpoint` event arrives. Legacy servers keep the stream open
/// after the event, so waiting for EOF would hang — hence incremental reads.
async fn discover_post_url(
    client: &reqwest::Client,
    sse_url: &str,
) -> Result<String, DeepseeknovaError> {
    let response = client
        .get(sse_url)
        .header("Accept", "text/event-stream")
        .send()
        .await
        .map_err(|e| {
            DeepseeknovaError::Runner(format!("failed to connect to MCP SSE endpoint: {e}"))
        })?;

    if !response.status().is_success() {
        let status = response.status();
        return Err(DeepseeknovaError::Runner(format!(
            "MCP SSE connection failed: HTTP {status}"
        )));
    }

    tokio::time::timeout(Duration::from_secs(10), async {
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                DeepseeknovaError::Runner(format!("failed to read MCP SSE stream: {e}"))
            })?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            if buffer.len() > 1 << 20 {
                return Err(DeepseeknovaError::Runner(
                    "MCP SSE: endpoint event too large".into(),
                ));
            }
            if let Some(url) = parse_sse_endpoint(&buffer) {
                return Ok(url);
            }
        }
        Err(DeepseeknovaError::Runner(
            "MCP SSE: no 'endpoint' event found in response".into(),
        ))
    })
    .await
    .map_err(|_| DeepseeknovaError::Runner("MCP SSE discovery timed out".into()))?
}

/// Parse the legacy SSE `endpoint` event (an HTTP URL) from a stream chunk.
fn parse_sse_endpoint(text: &str) -> Option<String> {
    parse_sse_events(text)
        .into_iter()
        .find_map(|(event, data)| {
            if event != "endpoint" {
                return None;
            }
            let url = data.trim();
            if url.starts_with("http://") || url.starts_with("https://") {
                Some(url.to_string())
            } else {
                None
            }
        })
}

/// Parse SSE `data:` payloads from a response body into `(event, data)` pairs.
///
/// SSE frames are separated by blank lines; a frame may carry an `event:` type
/// (defaulting to `message`) and one or more `data:` lines joined with
/// newlines. `id:`/`retry:` fields and comment lines are ignored, which covers
/// everything the MCP transports emit.
fn parse_sse_events(body: &str) -> Vec<(String, String)> {
    let mut events = Vec::new();
    let mut event_type = "message".to_string();
    let mut data_lines: Vec<String> = Vec::new();

    for line in body.lines() {
        let line = line.trim_end(); // SSE allows CRLF
        if line.is_empty() {
            if !data_lines.is_empty() {
                events.push((event_type.clone(), data_lines.join("\n")));
                data_lines.clear();
            }
            event_type = "message".to_string();
            continue;
        }
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start().to_string());
        } else if let Some(event) = line.strip_prefix("event:") {
            event_type = event.trim().to_string();
        }
        // `id:` / `retry:` / comment (`:`) lines are ignored.
    }
    if !data_lines.is_empty() {
        events.push((event_type, data_lines.join("\n")));
    }
    events
}

/// Find the SSE event whose JSON-RPC `id` matches `id`, returning the parsed
/// message. Interleaved events (e.g. other requests' responses) are skipped.
fn parse_sse_response(body: &str, id: u64) -> Option<Value> {
    parse_sse_events(body)
        .into_iter()
        .filter_map(|(_, data)| serde_json::from_str::<Value>(&data).ok())
        .find(|v| v.get("id").and_then(|i| i.as_u64()) == Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;

    /// 串行化 env 相关测试：`clear_proxy_env` 修改进程级环境变量，
    /// 与 reqwest::Client 构建存在 data race（std::env 文档明确不线程安全）。
    /// guard 需跨 await 持有，故用 tokio::sync::Mutex。
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 清除代理环境变量：HTTP 测试连本地 mock（127.0.0.1:随机端口），
    /// reqwest 默认尊重 HTTP_PROXY/HTTPS_PROXY 会把请求转发到代理，
    /// 代理无法连本地随机端口导致 Connect 失败（os error 61）。
    /// 在 ENV_LOCK 内调用保证串行，移除后不影响其他测试。
    fn clear_proxy_env() {
        for v in &[
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "http_proxy",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            std::env::remove_var(v);
        }
    }

    // ── SSE parsing (pure) ─────────────────────────────────────────

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
        // Any line between `event: endpoint` and the `data:` payload (e.g. a
        // blank separator) resets the expectation.
        let text = "event: endpoint\n\ndata: http://localhost:8080/mcp\n";
        assert_eq!(parse_sse_endpoint(text), None);
    }

    #[test]
    fn parse_sse_events_handles_crlf_and_multiple_frames() {
        let body =
            "event: message\r\ndata: {\"a\":1}\r\n\r\nevent: message\r\ndata: {\"b\":2}\r\n\r\n";
        let events = parse_sse_events(body);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, "message");
        assert_eq!(events[0].1, r#"{"a":1}"#);
        assert_eq!(events[1].1, r#"{"b":2}"#);
    }

    #[test]
    fn parse_sse_response_matches_by_id() {
        let body = "\
event: message
data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"first\":true}}

event: message
data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"second\":true}}

";
        let found = parse_sse_response(body, 2).expect("id 2 must be found");
        assert_eq!(found["result"], json!({"second": true}));
        assert!(parse_sse_response(body, 99).is_none());
    }

    // ── Minimal HTTP/1.1 mock server ───────────────────────────────

    /// Spawn a minimal HTTP/1.1 server that routes `GET` to `on_get` and any
    /// other method to `on_post`. `on_post` receives the full request text
    /// (request line + headers + body) so tests can assert what the client
    /// actually sent, and may return extra response headers (e.g. a session
    /// header or an SSE `Content-Type`). Returns the base URL and a handle to
    /// abort on teardown.
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
                    let has_content_type = extra_headers
                        .iter()
                        .any(|(k, _)| k.eq_ignore_ascii_case("Content-Type"));
                    let mut response = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
                        resp_body.len()
                    );
                    if !has_content_type {
                        response.push_str("Content-Type: application/json\r\n");
                    }
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

    // ── Mock-server request helpers ────────────────────────────────

    /// Extract the JSON-RPC `id` from a request body.
    fn extract_id(req_text: &str) -> u64 {
        req_text
            .split("\"id\":")
            .nth(1)
            .and_then(|s| {
                s.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse()
                    .ok()
            })
            .unwrap_or(0)
    }

    /// Whether the request is an `initialize` call.
    fn is_initialize(req_text: &str) -> bool {
        req_text.contains("\"method\":\"initialize\"")
    }

    /// A valid initialize result JSON body for the given request id.
    fn init_json_body(id: u64, version: &str) -> String {
        format!(
            r#"{{"jsonrpc":"2.0","id":{id},"result":{{"protocolVersion":"{version}","capabilities":{{"tools":{{"listChanged":true}}}},"serverInfo":{{"name":"mock","version":"1.0.0"}}}}}}"#
        )
    }

    // ── Legacy SSE discovery + fallback ────────────────────────────

    #[tokio::test]
    async fn sse_discovery_routes_posts_to_discovered_endpoint() {
        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
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
                let id = extract_id(&req_text);
                if is_initialize(&req_text) {
                    (200, init_json_body(id, "2025-06-18"), Vec::new())
                } else {
                    (
                        200,
                        format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"pong":true}}}}"#),
                        Vec::new(),
                    )
                }
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
            .find(|r| r.starts_with("POST ") && r.contains("\"method\":\"ping\""))
            .expect("client must POST ping to the discovered endpoint");
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
        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
        // No legacy SSE endpoint: the URL itself becomes the streamable HTTP
        // POST endpoint.
        let (url, server) = spawn_http_mock(
            || (404, "not found".to_string()),
            |req_text| {
                let id = extract_id(&req_text);
                if is_initialize(&req_text) {
                    (200, init_json_body(id, "2025-06-18"), Vec::new())
                } else {
                    (
                        200,
                        format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"fallback":true}}}}"#),
                        Vec::new(),
                    )
                }
            },
        );

        let conn = McpHttpConnection::connect(&format!("{url}/sse"), Duration::from_secs(5))
            .await
            .expect("connect falls back to the SSE URL");
        assert_eq!(conn.post_url, format!("{url}/sse"));

        let result = conn
            .request("ping", None, Duration::from_secs(5))
            .await
            .expect("request via fallback URL");
        assert_eq!(result, json!({"fallback": true}));
        server.abort();
    }

    // ── HTTP error surfacing ───────────────────────────────────────

    #[tokio::test]
    async fn http_error_status_surfaces_body() {
        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
        let (url, server) = spawn_http_mock(
            || (404, "no sse".to_string()),
            |req_text| {
                let id = extract_id(&req_text);
                if is_initialize(&req_text) {
                    (200, init_json_body(id, "2025-06-18"), Vec::new())
                } else {
                    (500, "server exploded".to_string(), Vec::new())
                }
            },
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
        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
        let (url, server) = spawn_http_mock(
            || (404, "no sse".to_string()),
            |req_text| {
                let id = extract_id(&req_text);
                if is_initialize(&req_text) {
                    (200, init_json_body(id, "2025-06-18"), Vec::new())
                } else {
                    (
                        200,
                        format!(
                            r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32601,"message":"Method not found"}}}}"#
                        ),
                        Vec::new(),
                    )
                }
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
        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
        let (url, server) = spawn_http_mock(
            || (404, "no sse".to_string()),
            |req_text| {
                let id = extract_id(&req_text);
                if is_initialize(&req_text) {
                    (200, init_json_body(id, "2025-06-18"), Vec::new())
                } else {
                    (200, "this is not json".to_string(), Vec::new())
                }
            },
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

    // ── Streamable HTTP ────────────────────────────────────────────

    #[tokio::test]
    async fn streamable_http_parses_sse_framed_response() {
        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
        // A streamable HTTP server answers requests with an SSE event stream
        // rather than a single JSON body.
        let (url, server) = spawn_http_mock(
            || (404, "no sse".to_string()),
            |req_text| {
                let id = extract_id(&req_text);
                let result = if is_initialize(&req_text) {
                    r#"{"protocolVersion":"2025-06-18","capabilities":{"tools":{"listChanged":true}},"serverInfo":{"name":"mock","version":"1.0.0"}}"#
                } else {
                    r#"{"pong":true}"#
                };
                let body = format!("event: message\ndata: {{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{result}}}\n\n");
                (
                    200,
                    body,
                    vec![("Content-Type".to_string(), "text/event-stream".to_string())],
                )
            },
        );

        let conn = McpHttpConnection::connect(&url, Duration::from_secs(5))
            .await
            .expect("connect to streamable HTTP server");
        assert_eq!(
            conn.protocol_version().await.as_deref(),
            Some("2025-06-18"),
            "negotiated version must come from the initialize result"
        );

        let result = conn
            .request("ping", None, Duration::from_secs(5))
            .await
            .expect("SSE-framed response must parse");
        assert_eq!(result, json!({"pong": true}));
        server.abort();
    }

    #[tokio::test]
    async fn streamable_http_fails_when_sse_has_no_matching_id() {
        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
        // A streamable server that responds with an SSE frame for a different
        // request id must surface a clear error, not a stale result.
        let (url, server) = spawn_http_mock(
            || (404, "no sse".to_string()),
            |req_text| {
                let id = extract_id(&req_text);
                if is_initialize(&req_text) {
                    let result = r#"{"protocolVersion":"2025-06-18","capabilities":{"tools":{"listChanged":true}},"serverInfo":{"name":"mock","version":"1.0.0"}}"#;
                    let body = format!("event: message\ndata: {{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{result}}}\n\n");
                    (
                        200,
                        body,
                        vec![("Content-Type".to_string(), "text/event-stream".to_string())],
                    )
                } else {
                    // Answer with a frame whose id never matches (999).
                    (
                        200,
                        "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":999,\"result\":{\"stale\":true}}\n\n"
                            .to_string(),
                        vec![("Content-Type".to_string(), "text/event-stream".to_string())],
                    )
                }
            },
        );

        let conn = McpHttpConnection::connect(&url, Duration::from_secs(5))
            .await
            .expect("connect");
        let err = conn
            .request("ping", None, Duration::from_secs(5))
            .await
            .expect_err("stale SSE frame must not satisfy the request");
        assert!(
            err.to_string().contains("no event matched request id"),
            "unexpected error: {err}"
        );
        server.abort();
    }

    // ── Session id lifecycle ───────────────────────────────────────

    #[tokio::test]
    async fn http_session_id_is_persisted_and_resent() {
        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_capture = Arc::clone(&seen);
        // initialize issues sess-1; each subsequent request rotates to sess-2.
        let (url, server) = spawn_http_mock(
            || (404, "no sse".to_string()),
            move |req_text| {
                seen_capture.lock().unwrap().push(req_text.clone());
                let id = extract_id(&req_text);
                if is_initialize(&req_text) {
                    (
                        200,
                        init_json_body(id, "2025-06-18"),
                        vec![("Mcp-Session-Id".to_string(), "sess-1".to_string())],
                    )
                } else {
                    (
                        200,
                        format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"ok":true}}}}"#),
                        vec![("Mcp-Session-Id".to_string(), "sess-2".to_string())],
                    )
                }
            },
        );

        let conn = McpHttpConnection::connect(&url, Duration::from_secs(5))
            .await
            .expect("connect");
        assert_eq!(
            conn.session_id().await.as_deref(),
            Some("sess-1"),
            "initialize must capture the session id"
        );

        conn.request("ping", None, Duration::from_secs(5))
            .await
            .expect("first ping");
        assert_eq!(
            conn.session_id().await.as_deref(),
            Some("sess-2"),
            "client must follow a server-side session rotation"
        );

        conn.request("ping", None, Duration::from_secs(5))
            .await
            .expect("second ping");
        server.abort();

        let reqs = seen.lock().unwrap();
        let inits: Vec<&String> = reqs
            .iter()
            .filter(|r| r.contains("\"initialize\""))
            .collect();
        assert_eq!(inits.len(), 1, "exactly one initialize expected");
        assert!(
            !inits[0].to_ascii_lowercase().contains("mcp-session-id"),
            "initialize must not carry a session header: {}",
            inits[0]
        );

        let pings: Vec<&String> = reqs
            .iter()
            .filter(|r| r.starts_with("POST ") && r.contains("\"method\":\"ping\""))
            .collect();
        assert_eq!(pings.len(), 2, "two ping requests expected");
        assert!(
            pings[0]
                .to_ascii_lowercase()
                .contains("mcp-session-id: sess-1"),
            "first ping must echo sess-1: {}",
            pings[0]
        );
        assert!(
            pings[1]
                .to_ascii_lowercase()
                .contains("mcp-session-id: sess-2"),
            "second ping must echo the rotated sess-2: {}",
            pings[1]
        );
    }

    #[tokio::test]
    async fn http_session_expired_404_clears_session() {
        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
        // A 404 carrying an empty Mcp-Session-Id means the session was
        // terminated: the client must drop the id and fail the request.
        let (url, server) = spawn_http_mock(
            || (404, "no sse".to_string()),
            |req_text| {
                let id = extract_id(&req_text);
                if is_initialize(&req_text) {
                    (
                        200,
                        init_json_body(id, "2025-06-18"),
                        vec![("Mcp-Session-Id".to_string(), "sess-1".to_string())],
                    )
                } else {
                    (
                        404,
                        String::new(),
                        vec![("Mcp-Session-Id".to_string(), String::new())],
                    )
                }
            },
        );

        let conn = McpHttpConnection::connect(&url, Duration::from_secs(5))
            .await
            .expect("connect");
        assert_eq!(conn.session_id().await.as_deref(), Some("sess-1"));

        let err = conn
            .request("ping", None, Duration::from_secs(5))
            .await
            .expect_err("expired session must fail the request");
        assert!(
            err.to_string().contains("session expired"),
            "unexpected error: {err}"
        );
        assert_eq!(
            conn.session_id().await,
            None,
            "expired session id must be cleared"
        );
        server.abort();
    }

    #[tokio::test]
    async fn session_expired_404_reconnects_and_retries_request() {
        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
        // Sequence: connect initialize → sess-1; first ping → 404 + session
        // header (expired); reconnect initialize → sess-2; retried ping → 200.
        // The caller must observe a plain success.
        let init_count = Arc::new(AtomicUsize::new(0));
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let init_count_c = Arc::clone(&init_count);
        let seen_c = Arc::clone(&seen);
        let (url, server) = spawn_http_mock(
            || (404, "no sse".to_string()),
            move |req_text| {
                seen_c.lock().unwrap().push(req_text.clone());
                let id = extract_id(&req_text);
                if is_initialize(&req_text) {
                    let n = init_count_c.fetch_add(1, Ordering::SeqCst);
                    let sess = if n == 0 { "sess-1" } else { "sess-2" };
                    (
                        200,
                        init_json_body(id, "2025-06-18"),
                        vec![("Mcp-Session-Id".to_string(), sess.to_string())],
                    )
                } else if req_text.contains("\"method\":\"ping\"") {
                    let lower = req_text.to_ascii_lowercase();
                    if lower.contains("mcp-session-id: sess-2") {
                        (
                            200,
                            format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"pong":true}}}}"#),
                            vec![("Mcp-Session-Id".to_string(), "sess-2".to_string())],
                        )
                    } else {
                        (
                            404,
                            String::new(),
                            vec![("Mcp-Session-Id".to_string(), String::new())],
                        )
                    }
                } else {
                    // notifications/initialized — best effort.
                    (200, String::new(), Vec::new())
                }
            },
        );

        let conn = McpHttpConnection::connect(&url, Duration::from_secs(5))
            .await
            .expect("connect");
        assert_eq!(conn.session_id().await.as_deref(), Some("sess-1"));

        let result = conn
            .request("ping", None, Duration::from_secs(5))
            .await
            .expect("request must succeed transparently after reconnect");
        assert_eq!(result, json!({"pong": true}));
        assert_eq!(conn.session_id().await.as_deref(), Some("sess-2"));
        server.abort();

        let reqs = seen.lock().unwrap();
        let inits: Vec<&String> = reqs
            .iter()
            .filter(|r| r.contains("\"initialize\""))
            .collect();
        assert_eq!(inits.len(), 2, "connect + exactly one reconnect initialize");
        let pings: Vec<&String> = reqs
            .iter()
            .filter(|r| r.starts_with("POST ") && r.contains("\"method\":\"ping\""))
            .collect();
        assert_eq!(pings.len(), 2, "original ping + one retry after reconnect");
    }

    #[tokio::test]
    async fn empty_session_id_on_response_triggers_reconnect() {
        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
        // A successful response carrying an empty Mcp-Session-Id signals that
        // the server terminated the session: the client must reconnect and
        // retry the request transparently.
        let init_count = Arc::new(AtomicUsize::new(0));
        let init_count_c = Arc::clone(&init_count);
        let (url, server) = spawn_http_mock(
            || (404, "no sse".to_string()),
            move |req_text| {
                let id = extract_id(&req_text);
                if is_initialize(&req_text) {
                    let n = init_count_c.fetch_add(1, Ordering::SeqCst);
                    let sess = if n == 0 { "sess-1" } else { "sess-2" };
                    (
                        200,
                        init_json_body(id, "2025-06-18"),
                        vec![("Mcp-Session-Id".to_string(), sess.to_string())],
                    )
                } else if req_text.contains("\"method\":\"ping\"") {
                    let lower = req_text.to_ascii_lowercase();
                    if lower.contains("mcp-session-id: sess-2") {
                        (
                            200,
                            format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"pong":true}}}}"#),
                            vec![("Mcp-Session-Id".to_string(), "sess-2".to_string())],
                        )
                    } else {
                        (
                            200,
                            format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"pong":true}}}}"#),
                            vec![("Mcp-Session-Id".to_string(), String::new())],
                        )
                    }
                } else {
                    (200, String::new(), Vec::new())
                }
            },
        );

        let conn = McpHttpConnection::connect(&url, Duration::from_secs(5))
            .await
            .expect("connect");
        let result = conn
            .request("ping", None, Duration::from_secs(5))
            .await
            .expect("request must succeed after reconnect");
        assert_eq!(result, json!({"pong": true}));
        assert_eq!(conn.session_id().await.as_deref(), Some("sess-2"));
        server.abort();
    }

    #[tokio::test]
    async fn reconnect_does_not_mask_persistent_session_expiry() {
        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
        // initialize always succeeds (sess-1); every ping 404s with the session
        // header. The single reconnect attempt cannot restore the session, so
        // the request must fail with a clear error instead of looping.
        let (url, server) = spawn_http_mock(
            || (404, "no sse".to_string()),
            |req_text| {
                let id = extract_id(&req_text);
                if is_initialize(&req_text) {
                    (
                        200,
                        init_json_body(id, "2025-06-18"),
                        vec![("Mcp-Session-Id".to_string(), "sess-1".to_string())],
                    )
                } else if req_text.contains("\"method\":\"ping\"") {
                    (
                        404,
                        String::new(),
                        vec![("Mcp-Session-Id".to_string(), String::new())],
                    )
                } else {
                    (200, String::new(), Vec::new())
                }
            },
        );

        let conn = McpHttpConnection::connect(&url, Duration::from_secs(5))
            .await
            .expect("connect");
        let err = conn
            .request("ping", None, Duration::from_secs(5))
            .await
            .expect_err("persistently expired session must fail");
        assert!(
            err.to_string().contains("session expired"),
            "unexpected error: {err}"
        );
        assert!(
            err.to_string().contains("reconnect did not restore"),
            "error should be unambiguous about the failed reconnect: {err}"
        );
        assert_eq!(
            conn.session_id().await,
            None,
            "session must be cleared after the failed reconnection"
        );
        server.abort();
    }

    #[tokio::test]
    async fn reconnect_initialize_failure_surfaces_clear_error() {
        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
        // The ping expires the session; the re-run initialize then fails with
        // an HTTP 500. The request must surface that reconnect failure rather
        // than silently retrying.
        let init_count = Arc::new(AtomicUsize::new(0));
        let init_count_c = Arc::clone(&init_count);
        let (url, server) = spawn_http_mock(
            || (404, "no sse".to_string()),
            move |req_text| {
                let id = extract_id(&req_text);
                if is_initialize(&req_text) {
                    if init_count_c.fetch_add(1, Ordering::SeqCst) == 0 {
                        (
                            200,
                            init_json_body(id, "2025-06-18"),
                            vec![("Mcp-Session-Id".to_string(), "sess-1".to_string())],
                        )
                    } else {
                        (500, "reconnect exploded".to_string(), Vec::new())
                    }
                } else if req_text.contains("\"method\":\"ping\"") {
                    (
                        404,
                        String::new(),
                        vec![("Mcp-Session-Id".to_string(), String::new())],
                    )
                } else {
                    (200, String::new(), Vec::new())
                }
            },
        );

        let conn = McpHttpConnection::connect(&url, Duration::from_secs(5))
            .await
            .expect("connect");
        let err = conn
            .request("ping", None, Duration::from_secs(5))
            .await
            .expect_err("failed reconnect must surface");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("MCP session reconnect failed"),
            "reconnect failure must be labelled: {err:#}"
        );
        assert!(
            chain.contains("reconnect exploded"),
            "underlying HTTP failure should be preserved: {err:#}"
        );
        server.abort();
    }

    #[tokio::test]
    async fn concurrent_requests_share_single_reconnect() {
        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
        // All pings carrying the stale session (sess-1) expire; only pings with
        // the re-established session (sess-2) succeed. Concurrent requests must
        // be serialized through the reconnect lock — one reconnect total — and
        // all must complete without panicking or hanging.
        const CONCURRENCY: usize = 8;

        let init_count = Arc::new(AtomicUsize::new(0));
        let init_count_c = Arc::clone(&init_count);
        let (url, server) = spawn_http_mock(
            || (404, "no sse".to_string()),
            move |req_text| {
                let id = extract_id(&req_text);
                if is_initialize(&req_text) {
                    let n = init_count_c.fetch_add(1, Ordering::SeqCst);
                    let sess = if n == 0 { "sess-1" } else { "sess-2" };
                    (
                        200,
                        init_json_body(id, "2025-06-18"),
                        vec![("Mcp-Session-Id".to_string(), sess.to_string())],
                    )
                } else if req_text.contains("\"method\":\"ping\"") {
                    let lower = req_text.to_ascii_lowercase();
                    if lower.contains("mcp-session-id: sess-2") {
                        (
                            200,
                            format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"pong":true}}}}"#),
                            vec![("Mcp-Session-Id".to_string(), "sess-2".to_string())],
                        )
                    } else {
                        (
                            404,
                            String::new(),
                            vec![("Mcp-Session-Id".to_string(), String::new())],
                        )
                    }
                } else {
                    (200, String::new(), Vec::new())
                }
            },
        );

        let conn = McpHttpConnection::connect(&url, Duration::from_secs(5))
            .await
            .expect("connect");

        let mut tasks = Vec::new();
        for i in 0..CONCURRENCY {
            let conn = Arc::clone(&conn);
            tasks.push(tokio::spawn(async move {
                conn.request("ping", Some(json!({"i": i})), Duration::from_secs(5))
                    .await
            }));
        }

        let results = tokio::time::timeout(Duration::from_secs(10), async {
            futures::future::join_all(tasks).await
        })
        .await
        .expect("concurrent requests must not hang during a reconnect");

        for (i, result) in results.iter().enumerate() {
            let result = result.as_ref().expect("request task must not panic");
            assert_eq!(
                result
                    .as_ref()
                    .expect("request must succeed after reconnect"),
                &json!({"pong": true}),
                "request {i} failed after reconnect"
            );
        }

        assert_eq!(
            init_count.load(Ordering::SeqCst),
            2,
            "connect + exactly one reconnect, not one per concurrent request"
        );
        server.abort();
    }

    // ── Protocol version negotiation ───────────────────────────────

    #[tokio::test]
    async fn http_protocol_version_negotiates_down_on_version_mismatch() {
        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_capture = Arc::clone(&seen);
        // Reject 2025-06-18; accept only 2025-03-26.
        let (url, server) = spawn_http_mock(
            || (404, "no sse".to_string()),
            move |req_text| {
                seen_capture.lock().unwrap().push(req_text.clone());
                let id = extract_id(&req_text);
                if is_initialize(&req_text) {
                    if req_text.contains("2025-06-18") {
                        let body = format!(
                            r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32602,"message":"Unsupported protocol version","data":{{"supported":["2025-03-26","2024-11-05"]}}}}}}"#
                        );
                        (200, body, Vec::new())
                    } else {
                        (200, init_json_body(id, "2025-03-26"), Vec::new())
                    }
                } else {
                    (
                        200,
                        format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"ok":true}}}}"#),
                        Vec::new(),
                    )
                }
            },
        );

        let conn = McpHttpConnection::connect(&url, Duration::from_secs(5))
            .await
            .expect("connect should retry and succeed");
        assert_eq!(
            conn.protocol_version().await.as_deref(),
            Some("2025-03-26"),
            "negotiated version must be the highest mutually supported one"
        );
        server.abort();

        let reqs = seen.lock().unwrap();
        let inits: Vec<&String> = reqs
            .iter()
            .filter(|r| r.contains("\"initialize\""))
            .collect();
        assert_eq!(inits.len(), 2, "expected a retry after version mismatch");
        assert!(
            inits[0].contains("2025-06-18"),
            "first attempt must offer the newest version: {}",
            inits[0]
        );
        assert!(
            inits[1].contains("2025-03-26"),
            "second attempt must offer the mutually supported version: {}",
            inits[1]
        );
    }
}

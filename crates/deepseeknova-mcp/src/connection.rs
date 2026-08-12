use crate::protocol;
use crate::types::*;
use deepseeknova_core::DeepseeknovaError;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, error, info, warn};

// ---------------------------------------------------------------------------
// McpConnection — manages an MCP server process lifecycle
// ---------------------------------------------------------------------------

/// Capacity of the bounded write channel between request senders and the
/// background writer task.
///
/// When the child process stops reading its stdin (or its pipe is dead), the
/// writer task blocks on `write_all`; a bounded channel then applies
/// backpressure to [`send_raw`](McpConnection::send_raw) so senders await for
/// buffer space instead of letting the request queue grow without bound. 1024
/// in-flight frames is far beyond any realistic request burst while still
/// bounding memory.
const WRITE_CHANNEL_CAPACITY: usize = 1024;

/// 单条 MCP stdout 行的最大字节数（16MB，含行尾换行符）。
///
/// MCP 的 stdio 传输把每条 JSON-RPC 消息编码为单行 JSON，合法消息远小于该
/// 上限。reader 用此常量封顶单行累积（T-H3）：一旦超限即丢弃该行剩余字节并
/// 告警（见 [`skip_to_newline`]），绝不无限累积——防止恶意或损坏的服务器输出
/// 无限长单行耗尽内存。与 `http_client.rs::discover_post_url` 的 1MB 读取
/// 上限同一口径。
const MAX_MCP_LINE_BYTES: usize = 16 * 1024 * 1024;

/// 丢弃 MCP stdout 上超长行的剩余内容（直到换行或 EOF）。
///
/// 返回 `true` 表示已消费到换行符（该行正常结束）；`false` 表示遇到 EOF 或
/// 读取错误（读取错误已记录 error 日志）。读取按 4096 字节分块进行，只保留
/// 当前块、即时清空，绝不把整行累积进内存。
async fn skip_to_newline<R>(reader: &mut BufReader<R>) -> bool
where
    R: AsyncRead + Unpin,
{
    let mut drain = Vec::with_capacity(4096);
    loop {
        drain.clear();
        let mut limited = (&mut *reader).take(4096);
        match limited.read_until(b'\n', &mut drain).await {
            Ok(0) => return false, // EOF：行未以换行结束
            Ok(_) if drain.last() == Some(&b'\n') => return true,
            Ok(_) => continue, // 本块未到换行，继续读下一块
            Err(e) => {
                error!("MCP read error while skipping oversized line: {e}");
                return false;
            }
        }
    }
}

/// Tracks a pending JSON-RPC request waiting for a response.
struct PendingRequest {
    response_tx: oneshot::Sender<Result<Value, DeepseeknovaError>>,
}

/// A live, initialized connection to an MCP server process.
/// The background reader task demuxes stdout lines into pending-request
/// responses and notifications. The writer task serializes writes to stdin.
pub struct McpConnection {
    /// The child process.
    child: RwLock<Option<Child>>,
    /// Channel for the background writer task. Bounded — see
    /// [`WRITE_CHANNEL_CAPACITY`] for the backpressure rationale.
    write_tx: mpsc::Sender<String>,
    /// Pending request map: id → oneshot sender.
    pending: Arc<RwLock<HashMap<u64, PendingRequest>>>,
    /// Next JSON-RPC request id.
    next_id: AtomicU64,
    /// Default timeout for requests.
    pub request_timeout: Duration,
    /// Server info from initialize.
    pub server_info: RwLock<ServerInfo>,
    /// Server capabilities from initialize.
    pub server_capabilities: RwLock<ServerCapabilities>,
    /// Join handles for background tasks.
    _reader_handle: RwLock<Option<tokio::task::JoinHandle<()>>>,
    _writer_handle: RwLock<Option<tokio::task::JoinHandle<()>>>,
}

#[cfg(test)]
impl McpConnection {
    /// Create a minimal McpConnection for testing (no actual process spawned).
    pub fn new_test() -> Self {
        let (tx, _rx) = mpsc::channel(WRITE_CHANNEL_CAPACITY);
        Self {
            child: RwLock::new(None),
            write_tx: tx,
            pending: Arc::new(RwLock::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            request_timeout: Duration::from_secs(5),
            server_info: RwLock::new(ServerInfo {
                name: "test-server".into(),
                version: "1.0.0".into(),
            }),
            server_capabilities: RwLock::new(ServerCapabilities {
                tools: None,
                resources: None,
                prompts: None,
                logging: None,
                experimental: None,
            }),
            _reader_handle: RwLock::new(None),
            _writer_handle: RwLock::new(None),
        }
    }
}

impl McpConnection {
    /// Spawn an MCP server, perform the initialize handshake, and return
    /// a ready-to-use connection.
    pub async fn connect(
        command: &str,
        args: &[String],
        env: &[(String, String)],
        request_timeout: Duration,
    ) -> Result<Arc<McpConnection>, DeepseeknovaError> {
        // Spawn child process
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);

        for (key, val) in env {
            cmd.env(key, val);
        }

        let mut child = cmd.spawn().map_err(|e| {
            DeepseeknovaError::runner(format!("failed to spawn MCP server: {command}: {e}"))
        })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            DeepseeknovaError::runner("no stdin on MCP child process".to_string())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            DeepseeknovaError::runner("no stdout on MCP child process".to_string())
        })?;

        let conn = Self::from_streams(
            stdin,
            stdout,
            Some(child),
            request_timeout,
            WRITE_CHANNEL_CAPACITY,
        )
        .await?;

        conn.handshake(request_timeout).await?;

        Ok(conn)
    }

    /// Wire the background I/O tasks and channel plumbing around already-open
    /// stdio streams.
    ///
    /// Used by [`Self::connect`] with a spawned child process, and by
    /// `#[cfg(test)]` tests with in-memory `tokio::io::duplex` streams (the
    /// fake-server replay harness). Does not perform the `initialize`
    /// handshake — callers invoke [`Self::handshake`] when a ready connection
    /// is required.
    ///
    /// `write_capacity` bounds the request queue to the writer task, applying
    /// backpressure to a child that stops reading its stdin.
    pub(crate) async fn from_streams<W, R>(
        stdin: W,
        stdout: R,
        child: Option<Child>,
        request_timeout: Duration,
        write_capacity: usize,
    ) -> Result<Arc<Self>, DeepseeknovaError>
    where
        W: AsyncWrite + Unpin + Send + 'static,
        R: AsyncRead + Unpin + Send + 'static,
    {
        // Bounded channel: a child that stops reading its stdin (or a dead
        // pipe) must not let the request queue grow without bound.
        let (write_tx, mut write_rx) = mpsc::channel::<String>(write_capacity);

        // Shared state
        let pending: Arc<RwLock<HashMap<u64, PendingRequest>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let next_id = AtomicU64::new(1);

        // Background writer task
        let mut writer_stdin = stdin;
        let writer_handle = tokio::spawn(async move {
            while let Some(line) = write_rx.recv().await {
                debug!("MCP → srv: {}", line.trim());
                if let Err(e) = writer_stdin.write_all(line.as_bytes()).await {
                    error!("MCP stdin write error: {e}");
                    break;
                }
                if let Err(e) = writer_stdin.flush().await {
                    error!("MCP stdin flush error: {e}");
                    break;
                }
            }
        });

        // Background reader task
        let pending_r = Arc::clone(&pending);
        // The reader answers server-initiated requests (e.g. `roots/list`) by
        // writing the response through the same channel the writer task drains.
        let reader_write_tx = write_tx.clone();
        let reader_handle = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            // 每行复用的行缓冲，避免每条消息重新分配。
            let mut line_buf: Vec<u8> = Vec::with_capacity(256);

            loop {
                line_buf.clear();
                // 限长读取（T-H3）：MCP stdio 每条 JSON-RPC 消息是单行 JSON，
                // 原 BufReader::lines() 对单行长度无上限，恶意/损坏的服务器可
                // 用无限长单行耗尽内存。这里用 take() 把单行累积封顶在
                // MAX_MCP_LINE_BYTES + 1 字节；若未在限内读到换行即判定超长，
                // 丢弃剩余部分并告警，绝不无限累积。换行符由 read_until 消费，
                // 后续消息保持按行对齐。
                let n = {
                    let mut limited = (&mut reader).take((MAX_MCP_LINE_BYTES + 1) as u64);
                    tokio::select! {
                        l = limited.read_until(b'\n', &mut line_buf) => l,
                        else => break,
                    }
                };

                match n {
                    Ok(0) => {
                        info!("MCP stdout closed");
                        break;
                    }
                    Ok(_) if line_buf.len() > MAX_MCP_LINE_BYTES => {
                        // 单行超限：丢弃本行剩余字节直到换行/EOF（分块读取、
                        // 不累积），保证后续消息仍能被正确解析。
                        warn!(
                            "MCP: oversized stdout line ({} bytes > {}); skipping remainder",
                            line_buf.len(),
                            MAX_MCP_LINE_BYTES
                        );
                        skip_to_newline(&mut reader).await;
                        continue;
                    }
                    Ok(_) => {
                        // 原 BufReader::lines() 会把非法 UTF-8 当作读错误终止
                        // reader；这里仅跳过该行，保持"垃圾行可跳过、不致命"
                        // 的既有语义（见 invalid_json_lines_are_skipped_not_fatal）。
                        let line = match std::str::from_utf8(&line_buf) {
                            Ok(s) => s.trim().to_string(),
                            Err(e) => {
                                warn!("MCP: stdout line is not valid UTF-8 ({e}); skipping");
                                continue;
                            }
                        };
                        if line.is_empty() {
                            continue;
                        }
                        debug!("MCP ← srv: {}", &line[..line.len().min(200)]);

                        let val: Value = match serde_json::from_str(&line) {
                            Ok(v) => v,
                            Err(e) => {
                                warn!("MCP parse error: {e}");
                                continue;
                            }
                        };

                        // Distinguish the three JSON-RPC message kinds a server
                        // may emit on stdout:
                        //  1. server request — `method` + `id`, no result/error
                        //     (e.g. `roots/list`, which the client advertises
                        //     support for during `initialize`). Answer it so the
                        //     server does not block waiting for a reply.
                        //  2. notification — `method`, no `id`.
                        //  3. response — answers one of our pending requests
                        //     (`id` with `result`/`error`, no `method`).
                        if protocol::is_server_request(&val) {
                            let method = val["method"].as_str().unwrap_or("?").to_string();
                            let reply = if method == "roots/list" {
                                protocol::build_server_response(
                                    &val,
                                    serde_json::json!({"roots": []}),
                                )
                            } else {
                                protocol::build_server_error(
                                    &val,
                                    -32601,
                                    format!("Method not found: {method}"),
                                )
                            };
                            let reply_str =
                                serde_json::to_string(&reply).unwrap_or_else(|_| String::new());
                            debug!("MCP ← srv request {method}; replying");
                            // 有界通道满时不可阻塞：reader 是子进程 stdout 的唯一
                            // 消费者，阻塞发送会与 writer/子进程形成双向管道死锁
                            // （子进程停止读 stdin → writer 阻塞 → 通道满 →
                            // reader 阻塞 → stdout 满 → 子进程挂死）。满时丢弃
                            // 应答并告警，保证 reader 永远继续排空 stdout。
                            if let Err(e) = reader_write_tx.try_send(format!("{reply_str}\n")) {
                                match e {
                                    tokio::sync::mpsc::error::TrySendError::Full(_) => {
                                        warn!("MCP write channel full; dropping answer to server request {method}");
                                    }
                                    tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                                        warn!("MCP write channel closed while answering {method}");
                                        break;
                                    }
                                }
                            }
                        } else if let Some(id) = val.get("id").and_then(|i| i.as_u64()) {
                            // Response to a pending request — forward the full
                            // response object (including any `error` field) so
                            // callers can inspect it (e.g. protocol-version
                            // negotiation). Result extraction happens in
                            // [`McpConnection::send_raw`].
                            //
                            // 注意：response 分支必须在 notification 分支之前——
                            // 部分服务器（非规范但常见）会在响应对象里回声
                            // `method` 字段（如 {"id":7,"method":"initialize",
                            // "result":{...}}）。若先判 method 会把这类响应当
                            // notification 丢弃，pending 请求挂到超时。
                            let mut map = pending_r.write().await;
                            if let Some(p) = map.remove(&id) {
                                let _ = p.response_tx.send(Ok(val.clone()));
                            } else {
                                warn!("MCP: response for unknown id {id}");
                            }
                        } else if val.get("method").is_some() && val.get("id").is_none() {
                            // Notification — log for now
                            let method = val["method"].as_str().unwrap_or("?");
                            debug!("MCP notification: {method}");
                        } else {
                            warn!("MCP: unrecognized server message without id or method");
                        }
                    }
                    Err(e) => {
                        error!("MCP read error: {e}");
                        break;
                    }
                }
            }

            // Drain pending on disconnect
            let mut map = pending_r.write().await;
            for (_, p) in map.drain() {
                let _ = p.response_tx.send(Err(DeepseeknovaError::runner(
                    "MCP connection closed".to_string(),
                )));
            }
        });

        // Build the connection handle
        let conn = McpConnection {
            child: RwLock::new(child),
            write_tx,
            pending,
            next_id,
            request_timeout,
            server_info: RwLock::new(ServerInfo {
                name: String::new(),
                version: String::new(),
            }),
            server_capabilities: RwLock::new(ServerCapabilities {
                tools: None,
                resources: None,
                prompts: None,
                logging: None,
                experimental: None,
            }),
            _reader_handle: RwLock::new(Some(reader_handle)),
            _writer_handle: RwLock::new(Some(writer_handle)),
        };

        Ok(Arc::new(conn))
    }

    /// Perform the MCP `initialize` handshake and store the server's reported
    /// info and capabilities. Returns an error if the server rejects the
    /// handshake or the response cannot be parsed.
    ///
    /// The protocol version is negotiated: the client offers its newest
    /// supported version and retries with the highest mutually supported one
    /// when the server reports a version mismatch. The version the server
    /// echoes back in `InitializeResult.protocolVersion` is authoritative.
    pub(crate) async fn handshake(
        &self,
        request_timeout: Duration,
    ) -> Result<(), DeepseeknovaError> {
        let init_result = self
            .negotiate_initialize(request_timeout)
            .await
            .map_err(|e| DeepseeknovaError::runner(format!("MCP initialize failed: {e}")))?;

        let init: InitializeResult = serde_json::from_value(init_result).map_err(|e| {
            DeepseeknovaError::runner(format!("failed to parse MCP initialize result: {e}"))
        })?;

        // Store server info
        *self.server_info.write().await = init.server_info;
        *self.server_capabilities.write().await = init.capabilities;

        // Send initialized notification
        let notif = serde_json::to_string(&JsonRpcNotification {
            jsonrpc: "2.0".into(),
            method: "notifications/initialized".into(),
            params: None,
        })?;
        let _ = self.write_tx.send(format!("{notif}\n")).await;

        info!(
            "MCP connected: {} v{} (protocol {})",
            self.server_info.read().await.name,
            self.server_info.read().await.version,
            init.protocol_version
        );

        Ok(())
    }

    /// Send `initialize` to the server, retrying with a lower protocol version
    /// on a version-mismatch error. Returns the `initialize` `result` value.
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
                    return Err(DeepseeknovaError::runner(format!(
                        "MCP: no mutually supported protocol version (client {:?}, server {supported:?})",
                        protocol::SUPPORTED_PROTOCOL_VERSIONS
                    )));
                }
                let msg = err
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error");
                return Err(DeepseeknovaError::runner(format!("MCP error: {msg}")));
            }

            return Ok(resp.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// Send a JSON-RPC request and wait for the full response object
    /// (including any `error` field). Used by [`Self::negotiate_initialize`]
    /// for protocol negotiation and by [`Self::send_raw`] for result
    /// extraction.
    async fn send_full(
        &self,
        method: &str,
        params: Option<Value>,
        timeout_dur: Duration,
    ) -> Result<Value, DeepseeknovaError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();

        self.pending
            .write()
            .await
            .insert(id, PendingRequest { response_tx: tx });

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id,
            method: method.into(),
            params,
        };
        let req_str = serde_json::to_string(&req)?;

        let result = tokio::time::timeout(timeout_dur, async {
            self.write_tx
                .send(format!("{req_str}\n"))
                .await
                .map_err(|_| DeepseeknovaError::runner("MCP write channel closed".to_string()))?;
            rx.await.map_err(|_| {
                DeepseeknovaError::runner(format!("MCP request cancelled for {method}"))
            })?
        })
        .await;

        match result {
            Ok(inner) => {
                // On a successful response the reader already removed the
                // entry; this also covers the send-error/cancellation paths
                // where it may still be present.
                self.pending.write().await.remove(&id);
                inner
            }
            Err(_) => {
                self.pending.write().await.remove(&id);
                Err(DeepseeknovaError::runner(format!(
                    "MCP request '{method}' timed out after {timeout_dur:?}"
                )))
            }
        }
    }

    /// Send a JSON-RPC request and return the `result` field, failing when the
    /// server answers with a JSON-RPC error object.
    async fn send_raw(
        &self,
        method: &str,
        params: Option<Value>,
        timeout_dur: Duration,
    ) -> Result<Value, DeepseeknovaError> {
        let resp = self.send_full(method, params, timeout_dur).await?;
        if let Some(err) = resp.get("error") {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            return Err(DeepseeknovaError::runner(format!("MCP error: {msg}")));
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Public request method used by McpClient.
    pub async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout_dur: Duration,
    ) -> Result<Value, DeepseeknovaError> {
        self.send_raw(method, params, timeout_dur).await
    }

    /// Shut down the connection. Kills the child process and waits for
    /// background tasks to complete.
    pub async fn shutdown(&self) {
        // 1. Kill the process. Killing the child closes its stdout, which the
        //    reader observes as EOF — this is what lets the reader complete
        //    instead of blocking forever on a live pipe.
        if let Some(mut child) = self.child.write().await.take() {
            info!("MCP: shutting down server process");
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        // 2. Await the reader so it drains pending requests (failing each with
        //    "MCP connection closed") rather than leaking the oneshot senders
        //    and leaving in-flight callers suspended indefinitely.
        if let Some(reader) = self._reader_handle.write().await.take() {
            let _ = reader.await;
        }
        // 3. Abort the writer. It is blocked on write_rx.recv() and never
        //    observes channel close because write_tx is still held by the
        //    connection, so it must be cancelled explicitly.
        if let Some(writer) = self._writer_handle.write().await.take() {
            writer.abort();
        }
    }

    /// Check if the server supports the tools capability.
    pub async fn supports_tools(&self) -> bool {
        self.server_capabilities.read().await.tools.is_some()
    }

    /// Check if the server supports the resources capability.
    pub async fn supports_resources(&self) -> bool {
        self.server_capabilities.read().await.resources.is_some()
    }

    /// Check if the server supports the prompts capability.
    pub async fn supports_prompts(&self) -> bool {
        self.server_capabilities.read().await.prompts.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{connect_ready, connect_streams, init_result};
    use serde_json::json;
    use std::sync::Mutex;
    use tokio::io::duplex;

    /// The fake-server initialize reply, with the request's `id` filled in.
    fn init_reply(id: u64) -> Value {
        let mut resp = init_result();
        if let Value::Object(map) = &mut resp {
            map.insert("id".into(), Value::from(id));
        }
        resp
    }

    // ── Normal request/response ──────────────────────────────────────

    #[tokio::test]
    async fn request_round_trip_returns_result() {
        let conn = connect_ready(
            |_req| Some(json!({"jsonrpc": "2.0", "result": {"pong": true}})),
            WRITE_CHANNEL_CAPACITY,
        )
        .await;

        let result = conn
            .request("ping", None, Duration::from_secs(5))
            .await
            .expect("request should succeed");
        assert_eq!(result, json!({"pong": true}));
    }

    #[tokio::test]
    async fn request_echoes_request_parameters() {
        let conn = connect_ready(
            |req| Some(json!({"jsonrpc": "2.0", "result": req.get("params").cloned().unwrap()})),
            WRITE_CHANNEL_CAPACITY,
        )
        .await;

        let result = conn
            .request("echo", Some(json!({"x": 7})), Duration::from_secs(5))
            .await
            .expect("request should succeed");
        assert_eq!(result, json!({"x": 7}));
    }

    // ── Error frames ────────────────────────────────────────────────

    #[tokio::test]
    async fn request_surfaces_json_rpc_error_frame() {
        let conn = connect_ready(
            |_req| {
                Some(json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32000, "message": "kaboom"}
                }))
            },
            WRITE_CHANNEL_CAPACITY,
        )
        .await;

        let err = conn
            .request("boom", None, Duration::from_secs(5))
            .await
            .expect_err("error frame must fail the request");
        assert!(
            err.to_string().contains("MCP error: kaboom"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn invalid_json_lines_are_skipped_not_fatal() {
        // A server that spams garbage — non-JSON and a JSON notification
        // without an id — before answering the actual request.
        let (conn_stdin, mut srv_rx) = duplex(crate::test_util::DUPLEX_BUF);
        let (mut srv_tx, conn_stdout) = duplex(crate::test_util::DUPLEX_BUF);
        let server = tokio::spawn(async move {
            let buf = BufReader::new(&mut srv_rx);
            let mut lines = buf.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let val: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let Some(id) = val.get("id").and_then(|i| i.as_u64()) else {
                    continue; // notification (e.g. notifications/initialized) — no id
                };
                if val["method"] == "initialize" {
                    let _ = srv_tx
                        .write_all(init_reply(id).to_string().as_bytes())
                        .await;
                    let _ = srv_tx.write_all(b"\n").await;
                } else {
                    for garbage in [
                        "this is not json",
                        "{{{{",
                        r#"{"jsonrpc":"2.0","method":"notifications/ignored"}"#,
                    ] {
                        let _ = srv_tx.write_all(garbage.as_bytes()).await;
                        let _ = srv_tx.write_all(b"\n").await;
                    }
                    let resp = json!({"jsonrpc":"2.0","id": id, "result":{"pong":true}});
                    let _ = srv_tx.write_all(resp.to_string().as_bytes()).await;
                    let _ = srv_tx.write_all(b"\n").await;
                }
                let _ = srv_tx.flush().await;
            }
        });

        let conn = McpConnection::from_streams(
            conn_stdin,
            conn_stdout,
            None,
            Duration::from_secs(5),
            WRITE_CHANNEL_CAPACITY,
        )
        .await
        .expect("from_streams");
        conn.handshake(Duration::from_secs(5))
            .await
            .expect("handshake");

        let result = conn
            .request("ping", None, Duration::from_secs(5))
            .await
            .expect("request should survive surrounding garbage");
        assert_eq!(result, json!({"pong": true}));
        server.abort();
    }

    // ── Oversized single-line output (T-H3) ────────────────────────────

    #[tokio::test]
    async fn oversized_stdout_line_is_skipped_and_following_message_still_parsed() {
        // 服务器先输出一条远超 MAX_MCP_LINE_BYTES、且不含换行的单行内容，
        // 随后才输出正常的 JSON-RPC 响应。reader 必须在达到上限后停止累积、
        // 丢弃该行剩余字节并告警，同时仍能把后续响应按行对齐解析——绝不无限
        // 累积内存（旧实现 BufReader::lines() 会一直读到换行才返回）。
        let (conn_stdin, mut srv_rx) = duplex(crate::test_util::DUPLEX_BUF);
        let (mut srv_tx, conn_stdout) = duplex(crate::test_util::DUPLEX_BUF);

        let server = tokio::spawn(async move {
            let buf = BufReader::new(&mut srv_rx);
            let mut lines = buf.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let val: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let Some(id) = val.get("id").and_then(|i| i.as_u64()) else {
                    continue; // notification — no id
                };
                if val["method"] == "initialize" {
                    let _ = srv_tx
                        .write_all(init_reply(id).to_string().as_bytes())
                        .await;
                    let _ = srv_tx.write_all(b"\n").await;
                } else {
                    // 超长单行：不换行地写入两倍于上限的内容，随后才换行。
                    let garbage = vec![b'x'; MAX_MCP_LINE_BYTES * 2];
                    let _ = srv_tx.write_all(&garbage).await;
                    let _ = srv_tx.write_all(b"\n").await;
                    // 紧跟着的正常响应：证明跳过超长行后消息仍按行对齐。
                    let resp = json!({"jsonrpc":"2.0","id": id, "result":{"pong":true}});
                    let _ = srv_tx.write_all(resp.to_string().as_bytes()).await;
                    let _ = srv_tx.write_all(b"\n").await;
                }
                let _ = srv_tx.flush().await;
            }
        });

        let conn = McpConnection::from_streams(
            conn_stdin,
            conn_stdout,
            None,
            Duration::from_secs(10),
            WRITE_CHANNEL_CAPACITY,
        )
        .await
        .expect("from_streams");
        conn.handshake(Duration::from_secs(10))
            .await
            .expect("handshake should survive");

        let result = conn
            .request("ping", None, Duration::from_secs(10))
            .await
            .expect("request after oversized line should succeed");
        assert_eq!(result, json!({"pong": true}));

        // 再发一次请求，确认 reader 跳过超长行后仍持续正常工作。
        let result = conn
            .request("ping", None, Duration::from_secs(10))
            .await
            .expect("second request after oversized line should succeed");
        assert_eq!(result, json!({"pong": true}));
        server.abort();
    }

    // ── Server requests (client-advertised `roots` capability) ──────────

    #[tokio::test]
    async fn server_request_roots_list_is_answered_and_handshake_does_not_hang() {
        // The client advertises `roots` in initialize, so a compliant server
        // sends a `roots/list` request (id + method, no result/error). The
        // client must answer `{"roots": []}` — not drop it as an unknown
        // response. The server deliberately refuses to answer the follow-up
        // `ping` until it has seen the roots/list answer, which makes any
        // missed reply surface as a request timeout (a hang).
        const ROOTS_REQ_ID: u64 = 9001;

        let (conn_stdin, mut srv_rx) = duplex(crate::test_util::DUPLEX_BUF);
        let (mut srv_tx, conn_stdout) = duplex(crate::test_util::DUPLEX_BUF);
        let seen_roots_answer: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let seen_roots_answer_capture = Arc::clone(&seen_roots_answer);

        let server = tokio::spawn(async move {
            let buf = BufReader::new(&mut srv_rx);
            let mut lines = buf.lines();
            let mut roots_answered = false;
            let mut pending_ping: Option<u64> = None;
            while let Ok(Some(line)) = lines.next_line().await {
                let val: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                // The client's answer to our roots/list request.
                if val.get("id").and_then(|i| i.as_u64()) == Some(ROOTS_REQ_ID) {
                    roots_answered = true;
                    *seen_roots_answer_capture.lock().unwrap() = Some(line.clone());
                    if let Some(pid) = pending_ping.take() {
                        let pong = json!({"jsonrpc":"2.0","id": pid, "result":{"pong":true}});
                        let _ = srv_tx.write_all(pong.to_string().as_bytes()).await;
                        let _ = srv_tx.write_all(b"\n").await;
                        let _ = srv_tx.flush().await;
                    }
                    continue;
                }

                let Some(id) = val.get("id").and_then(|i| i.as_u64()) else {
                    continue; // notification (e.g. notifications/initialized) — no id
                };
                match val["method"].as_str() {
                    Some("initialize") => {
                        // Reply to initialize, then immediately send a
                        // `roots/list` server request.
                        let _ = srv_tx
                            .write_all(init_reply(id).to_string().as_bytes())
                            .await;
                        let _ = srv_tx.write_all(b"\n").await;
                        let roots_req = json!({
                            "jsonrpc": "2.0",
                            "id": ROOTS_REQ_ID,
                            "method": "roots/list",
                            "params": {}
                        });
                        let _ = srv_tx.write_all(roots_req.to_string().as_bytes()).await;
                        let _ = srv_tx.write_all(b"\n").await;
                        let _ = srv_tx.flush().await;
                    }
                    Some("ping") => {
                        // Refuse to answer until the roots/list reply arrived.
                        if roots_answered {
                            let pong = json!({"jsonrpc":"2.0","id": id, "result":{"pong":true}});
                            let _ = srv_tx.write_all(pong.to_string().as_bytes()).await;
                            let _ = srv_tx.write_all(b"\n").await;
                            let _ = srv_tx.flush().await;
                        } else {
                            pending_ping = Some(id);
                        }
                    }
                    _ => {}
                }
            }
        });

        let conn = McpConnection::from_streams(
            conn_stdin,
            conn_stdout,
            None,
            Duration::from_secs(5),
            WRITE_CHANNEL_CAPACITY,
        )
        .await
        .expect("from_streams");

        conn.handshake(Duration::from_secs(5))
            .await
            .expect("handshake must complete despite the roots/list request");

        let result = conn
            .request("ping", None, Duration::from_secs(5))
            .await
            .expect("ping must succeed once the roots/list answer was delivered");
        assert_eq!(result, json!({"pong": true}));
        server.abort();

        let answer = seen_roots_answer
            .lock()
            .unwrap()
            .clone()
            .expect("client must write a roots/list answer to its stdout");
        let answer: Value =
            serde_json::from_str(&answer).expect("roots/list answer must be valid JSON");
        assert_eq!(
            answer["id"], ROOTS_REQ_ID,
            "answer must echo the request id"
        );
        assert_eq!(
            answer["result"],
            json!({"roots": []}),
            "answer must carry an empty roots list"
        );
    }

    // ── Half-close / EOF ────────────────────────────────────────────

    #[tokio::test]
    async fn eof_drains_pending_requests_with_connection_closed() {
        // The server answers initialize, then closes its stdout on the next
        // request without replying — the reader sees EOF and must fail the
        // in-flight request cleanly.
        let (conn_stdin, mut srv_rx) = duplex(crate::test_util::DUPLEX_BUF);
        let (mut srv_tx, conn_stdout) = duplex(crate::test_util::DUPLEX_BUF);
        let server = tokio::spawn(async move {
            let buf = BufReader::new(&mut srv_rx);
            let mut lines = buf.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let val: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let Some(id) = val.get("id").and_then(|i| i.as_u64()) else {
                    continue; // notification — ignore
                };
                if val["method"] == "initialize" {
                    let _ = srv_tx
                        .write_all(init_reply(id).to_string().as_bytes())
                        .await;
                    let _ = srv_tx.write_all(b"\n").await;
                    let _ = srv_tx.flush().await;
                } else {
                    break; // drop srv_tx → stdout EOF
                }
            }
        });

        let conn = McpConnection::from_streams(
            conn_stdin,
            conn_stdout,
            None,
            Duration::from_secs(5),
            WRITE_CHANNEL_CAPACITY,
        )
        .await
        .expect("from_streams");
        conn.handshake(Duration::from_secs(5))
            .await
            .expect("handshake");

        let err = conn
            .request("doomed", None, Duration::from_secs(5))
            .await
            .expect_err("EOF must fail the in-flight request");
        assert!(
            err.to_string().contains("MCP connection closed"),
            "unexpected error: {err}"
        );
        server.abort();
    }

    // ── Timeout ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn request_times_out_when_server_is_silent() {
        let conn = connect_ready(|_req| None, WRITE_CHANNEL_CAPACITY).await;

        let err = conn
            .request("hang", None, Duration::from_millis(50))
            .await
            .expect_err("silent server must time out");
        assert!(
            err.to_string().contains("timed out"),
            "unexpected error: {err}"
        );
    }

    // ── Shutdown ────────────────────────────────────────────────────

    #[tokio::test]
    async fn shutdown_joins_background_tasks_without_hanging() {
        // A fake server that answers initialize and then goes silent. With an
        // in-flight request pending, shutdown must still complete promptly —
        // it joins the reader (which drains pending on EOF) and aborts the
        // writer instead of abandoning them.
        let (conn_stdin, mut srv_rx) = duplex(crate::test_util::DUPLEX_BUF);
        let (mut srv_tx, conn_stdout) = duplex(crate::test_util::DUPLEX_BUF);
        let server = tokio::spawn(async move {
            let buf = BufReader::new(&mut srv_rx);
            let mut lines = buf.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let val: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let Some(id) = val.get("id").and_then(|i| i.as_u64()) else {
                    continue; // notification — ignore
                };
                if val["method"] == "initialize" {
                    let _ = srv_tx
                        .write_all(init_reply(id).to_string().as_bytes())
                        .await;
                    let _ = srv_tx.write_all(b"\n").await;
                    let _ = srv_tx.flush().await;
                }
                // Everything else stays unanswered.
            }
        });

        let conn = McpConnection::from_streams(
            conn_stdin,
            conn_stdout,
            None,
            Duration::from_secs(5),
            WRITE_CHANNEL_CAPACITY,
        )
        .await
        .expect("from_streams");
        conn.handshake(Duration::from_secs(5))
            .await
            .expect("handshake");

        // Issue a request the fake server never answers.
        let req = {
            let conn = conn.clone();
            tokio::spawn(async move { conn.request("hang", None, Duration::from_secs(60)).await })
        };
        // Give the request time to register as pending and be written out.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Close the server's stdout (EOF) so the reader drains pending, then
        // shut down: must complete without hanging on either background task.
        server.abort();

        tokio::time::timeout(Duration::from_secs(5), conn.shutdown())
            .await
            .expect("shutdown must not hang on background tasks");

        // The reader drains pending on EOF, so the in-flight request must fail
        // cleanly rather than staying suspended.
        let err = req
            .await
            .expect("request task should finish")
            .expect_err("in-flight request must fail after shutdown");
        assert!(
            err.to_string().contains("MCP connection closed"),
            "unexpected error: {err}"
        );
    }

    // ── Initialize handshake ────────────────────────────────────────

    #[tokio::test]
    async fn handshake_populates_server_info_and_capabilities() {
        let conn = connect_ready(
            |_req| Some(json!({"jsonrpc": "2.0", "result": {"tools": []}})),
            WRITE_CHANNEL_CAPACITY,
        )
        .await;

        assert_eq!(conn.server_info.read().await.name, "fake-server");
        assert_eq!(conn.server_info.read().await.version, "9.9.9");
        assert!(conn.supports_tools().await);
        assert!(conn.supports_resources().await);
        assert!(conn.supports_prompts().await);
    }

    #[tokio::test]
    async fn handshake_propagates_server_error() {
        let conn = connect_streams(
            |req| {
                if req["method"] == "initialize" {
                    Some(json!({
                        "jsonrpc": "2.0",
                        "error": {"code": -32000, "message": "init denied"}
                    }))
                } else {
                    None
                }
            },
            WRITE_CHANNEL_CAPACITY,
        )
        .await;

        let err = conn
            .handshake(Duration::from_secs(5))
            .await
            .expect_err("rejected initialize must fail handshake");
        assert!(
            err.to_string().contains("init denied"),
            "unexpected error: {err:#}"
        );
    }

    #[tokio::test]
    async fn handshake_retries_with_lower_version_on_mismatch() {
        // The server rejects 2025-06-18 with a version-mismatch error and
        // accepts 2025-03-26. The client must retry and settle on the lower
        // mutually supported version.
        let attempts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let attempts_capture = Arc::clone(&attempts);
        let conn = connect_streams(
            move |req| {
                if req["method"] == "initialize" {
                    let requested = req["params"]["protocolVersion"]
                        .as_str()
                        .unwrap_or("?")
                        .to_string();
                    attempts_capture.lock().unwrap().push(requested);
                    if req["params"]["protocolVersion"] == "2025-06-18" {
                        Some(json!({
                            "jsonrpc": "2.0",
                            "error": {
                                "code": -32602,
                                "message": "Unsupported protocol version",
                                "data": {"supported": ["2025-03-26", "2024-11-05"]}
                            }
                        }))
                    } else {
                        Some(json!({
                            "jsonrpc": "2.0",
                            "result": {
                                "protocolVersion": "2025-03-26",
                                "capabilities": {"tools": {"listChanged": true}},
                                "serverInfo": {"name": "fake", "version": "1.0.0"}
                            }
                        }))
                    }
                } else {
                    None
                }
            },
            WRITE_CHANNEL_CAPACITY,
        )
        .await;

        conn.handshake(Duration::from_secs(5))
            .await
            .expect("handshake should retry and succeed");

        let seen = attempts.lock().unwrap();
        assert_eq!(
            seen.as_slice(),
            &["2025-06-18".to_string(), "2025-03-26".to_string()],
            "client should offer newest first, then the mutually supported version"
        );
    }

    #[tokio::test]
    async fn handshake_rejects_malformed_result() {
        let conn = connect_streams(
            |req| {
                if req["method"] == "initialize" {
                    Some(json!({"jsonrpc": "2.0", "result": {"unexpected": true}}))
                } else {
                    None
                }
            },
            WRITE_CHANNEL_CAPACITY,
        )
        .await;

        let err = conn
            .handshake(Duration::from_secs(5))
            .await
            .expect_err("malformed initialize result must fail handshake");
        assert!(
            err.to_string()
                .contains("failed to parse MCP initialize result"),
            "unexpected error: {err}"
        );
    }

    // ── Backpressure on the bounded write channel ───────────────────

    #[tokio::test]
    async fn bounded_channel_backpressure_does_not_lose_messages() {
        const N_REQUESTS: usize = 12;
        const CHANNEL_CAPACITY: usize = 2;

        // A tiny duplex buffer makes the writer task block on write_all almost
        // immediately, so the bounded channel saturates quickly.
        let (conn_stdin, mut srv_rx) = duplex(32);
        let (mut srv_tx, conn_stdout) = duplex(32);
        let (release_tx, mut release_rx) = mpsc::channel::<()>(1);

        let server = tokio::spawn(async move {
            // Hold off reading stdin until the test has saturated the channel.
            let _ = release_rx.recv().await;
            let buf = BufReader::new(&mut srv_rx);
            let mut lines = buf.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let val: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if let Some(id) = val.get("id").and_then(|i| i.as_u64()) {
                    let resp = json!({"jsonrpc":"2.0","id": id, "result":{"ok":true}});
                    let _ = srv_tx.write_all(resp.to_string().as_bytes()).await;
                    let _ = srv_tx.write_all(b"\n").await;
                    let _ = srv_tx.flush().await;
                }
            }
        });

        let conn = McpConnection::from_streams(
            conn_stdin,
            conn_stdout,
            None,
            Duration::from_secs(10),
            CHANNEL_CAPACITY,
        )
        .await
        .expect("from_streams");

        let mut tasks = Vec::new();
        for i in 0..N_REQUESTS {
            let conn = conn.clone();
            tasks.push(tokio::spawn(async move {
                conn.request("tools/call", Some(json!({"i": i})), Duration::from_secs(10))
                    .await
            }));
        }

        // While the server is not reading, the pipe fills, the writer blocks,
        // and the bounded channel saturates: the first request must not
        // complete, proving sends are genuinely stalled by backpressure.
        let stalled = tokio::time::timeout(Duration::from_millis(100), &mut tasks[0]).await;
        assert!(
            stalled.is_err(),
            "request should be stalled while the server is blocked"
        );

        // Release the server: every request must still receive its response —
        // backpressure delays, it never drops.
        release_tx.send(()).await.expect("release server");
        let results = futures::future::join_all(tasks).await;
        for (i, result) in results.iter().enumerate() {
            let result = result.as_ref().expect("request task must not panic");
            assert_eq!(
                result.as_ref().expect("request must succeed"),
                &json!({"ok": true}),
                "request {i} was lost or failed"
            );
        }

        server.abort();
    }

    // ── Real child process lifecycle ────────────────────────────────

    #[tokio::test]
    async fn real_process_exit_cleans_up_pending_requests() {
        if cfg!(windows) {
            // The scripted server relies on a POSIX shell; skip on Windows.
            return;
        }
        // A shell "server" that answers initialize and then goes silent.
        let script = concat!(
            "while read line; do ",
            "case \"$line\" in ",
            "*initialize*) printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"serverInfo\":{\"name\":\"sh\",\"version\":\"1\"}}}';; ",
            "esac; ",
            "done",
        );
        let conn = McpConnection::connect(
            "/bin/sh",
            &["-c".to_string(), script.to_string()],
            &[],
            Duration::from_secs(5),
        )
        .await
        .expect("connect to scripted server");

        // Issue a request the scripted server will never answer.
        let req = {
            let conn = conn.clone();
            tokio::spawn(async move {
                conn.request("tools/list", None, Duration::from_secs(10))
                    .await
            })
        };
        // Give the request time to register as pending and be written out.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Kill the process: its stdout closes → the reader drains pending.
        if let Some(mut child) = conn.child.write().await.take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }

        let err = req
            .await
            .expect("request task should finish")
            .expect_err("a killed server must fail the in-flight request");
        assert!(
            err.to_string().contains("MCP connection closed"),
            "unexpected error: {err}"
        );
    }
}

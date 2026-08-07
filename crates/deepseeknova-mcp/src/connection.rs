use crate::types::*;
use anyhow::Context;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
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

/// Tracks a pending JSON-RPC request waiting for a response.
struct PendingRequest {
    response_tx: oneshot::Sender<anyhow::Result<Value>>,
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
    ) -> anyhow::Result<Arc<McpConnection>> {
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

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn MCP server: {command}"))?;

        let stdin = child
            .stdin
            .take()
            .context("no stdin on MCP child process")?;
        let stdout = child
            .stdout
            .take()
            .context("no stdout on MCP child process")?;

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
    ) -> anyhow::Result<Arc<Self>>
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
        let reader_handle = tokio::spawn(async move {
            let buf = BufReader::new(stdout);
            let mut lines = buf.lines();

            loop {
                let line = tokio::select! {
                    l = lines.next_line() => l,
                    else => break,
                };

                match line {
                    Ok(Some(line)) => {
                        let line = line.trim().to_string();
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

                        if let Some(id) = val.get("id").and_then(|i| i.as_u64()) {
                            // Response to a pending request
                            let mut map = pending_r.write().await;
                            if let Some(p) = map.remove(&id) {
                                if val.get("error").is_some() {
                                    let err_msg =
                                        val["error"]["message"].as_str().unwrap_or("unknown error");
                                    let _ = p
                                        .response_tx
                                        .send(Err(anyhow::anyhow!("MCP error: {err_msg}")));
                                } else {
                                    let result = val.get("result").cloned().unwrap_or(Value::Null);
                                    let _ = p.response_tx.send(Ok(result));
                                }
                            } else {
                                warn!("MCP: response for unknown id {id}");
                            }
                        } else if val.get("method").is_some() {
                            // Notification — log for now
                            let method = val["method"].as_str().unwrap_or("?");
                            debug!("MCP notification: {method}");
                        }
                    }
                    Ok(None) => {
                        info!("MCP stdout closed");
                        break;
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
                let _ = p
                    .response_tx
                    .send(Err(anyhow::anyhow!("MCP connection closed")));
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
    pub(crate) async fn handshake(&self, request_timeout: Duration) -> anyhow::Result<()> {
        // Perform initialize handshake
        let init_params = serde_json::to_value(InitializeRequest {
            protocol_version: "2024-11-05".into(),
            capabilities: ClientCapabilities {
                roots: Some(RootsCapability { list_changed: true }),
                sampling: None,
                experimental: None,
            },
            client_info: ClientInfo {
                name: "deepseeknova".into(),
                version: "0.1.0".into(),
            },
        })?;

        let init_result = self
            .send_raw("initialize", Some(init_params), request_timeout)
            .await
            .context("MCP initialize failed")?;

        let init: InitializeResult =
            serde_json::from_value(init_result).context("failed to parse MCP initialize result")?;

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

    /// Send a JSON-RPC request and wait for the response.
    ///
    /// The bounded write channel applies backpressure to callers when the
    /// child's stdin is blocked: [`mpsc::Sender::send`] awaits for buffer
    /// space rather than letting the queue grow without bound, so no legal
    /// message is dropped. The whole send-and-wait sequence is bounded by
    /// `timeout_dur`, so a blocked or dead child still fails cleanly instead
    /// of hanging the caller.
    async fn send_raw(
        &self,
        method: &str,
        params: Option<Value>,
        timeout_dur: Duration,
    ) -> anyhow::Result<Value> {
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
                .map_err(|_| anyhow::anyhow!("MCP write channel closed"))?;
            rx.await
                .map_err(|_| anyhow::anyhow!("MCP request cancelled for {method}"))?
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
                anyhow::bail!("MCP request '{method}' timed out after {timeout_dur:?}")
            }
        }
    }

    /// Public request method used by McpClient.
    pub async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout_dur: Duration,
    ) -> anyhow::Result<Value> {
        self.send_raw(method, params, timeout_dur).await
    }

    /// Shut down the connection. Kills the child process and waits for
    /// background tasks to complete.
    pub async fn shutdown(&self) {
        // Kill the process
        if let Some(mut child) = self.child.write().await.take() {
            info!("MCP: shutting down server process");
            let _ = child.start_kill();
            let _ = child.wait().await;
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
            err.chain().any(|c| c.to_string().contains("init denied")),
            "unexpected error: {err:#}"
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

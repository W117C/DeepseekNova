use crate::types::*;
use anyhow::Context;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, error, info, warn};

// ---------------------------------------------------------------------------
// McpConnection — manages an MCP server process lifecycle
// ---------------------------------------------------------------------------

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
    /// Channel for the background writer task.
    write_tx: mpsc::UnboundedSender<String>,
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
        let (tx, _rx) = mpsc::unbounded_channel();
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

        // Channels
        let (write_tx, mut write_rx) = mpsc::unbounded_channel::<String>();

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
        let conn = Arc::new(McpConnection {
            child: RwLock::new(Some(child)),
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
        });

        // Perform initialize handshake
        let init_params = serde_json::to_value(InitializeRequest {
            protocol_version: "2024-11-05".into(),
            capabilities: ClientCapabilities {
                roots: Some(RootsCapability { list_changed: true }),
                sampling: None,
                experimental: None,
            },
            client_info: ClientInfo {
                name: "deepnova".into(),
                version: "0.1.0".into(),
            },
        })?;

        let init_result = conn
            .send_raw("initialize", Some(init_params), request_timeout)
            .await
            .context("MCP initialize failed")?;

        let init: InitializeResult =
            serde_json::from_value(init_result).context("failed to parse MCP initialize result")?;

        // Store server info
        *conn.server_info.write().await = init.server_info;
        *conn.server_capabilities.write().await = init.capabilities;

        // Send initialized notification
        let notif = serde_json::to_string(&JsonRpcNotification {
            jsonrpc: "2.0".into(),
            method: "notifications/initialized".into(),
            params: None,
        })?;
        let _ = conn.write_tx.send(format!("{notif}\n"));

        info!(
            "MCP connected: {} v{} (protocol {})",
            conn.server_info.read().await.name,
            conn.server_info.read().await.version,
            init.protocol_version
        );

        Ok(conn)
    }

    /// Send a JSON-RPC request and wait for the response.
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
        self.write_tx
            .send(format!("{req_str}\n"))
            .context("MCP write channel closed")?;

        match tokio::time::timeout(timeout_dur, rx).await {
            Ok(Ok(result)) => Ok(result?),
            Ok(Err(_)) => {
                self.pending.write().await.remove(&id);
                anyhow::bail!("MCP request cancelled for {method}")
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

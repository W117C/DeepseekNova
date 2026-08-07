//! Shared test-double infrastructure for the MCP crate.
//!
//! Only compiled under `#[cfg(test)]`. Provides:
//! - A fake stdio MCP server over in-memory `tokio::io::duplex` streams that
//!   replays scripted JSON-RPC frames — a test closure decides each response,
//!   including "no response" for timeout scenarios.
//! - Helpers to build a live [`McpConnection`] backed by that fake server,
//!   with or without completing the `initialize` handshake.

use crate::connection::McpConnection;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio::task::JoinHandle;

/// Duplex buffer size for fake servers. Large enough to hold several frames
/// without engaging backpressure; backpressure tests deliberately use smaller
/// buffers.
pub const DUPLEX_BUF: usize = 4096;

/// A standard MCP `initialize` result that fake servers reply with.
pub fn init_result() -> Value {
    json!({
        "jsonrpc": "2.0",
        "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {"listChanged": true},
                "resources": {"subscribe": false, "listChanged": false},
                "prompts": {"listChanged": false}
            },
            "serverInfo": {"name": "fake-server", "version": "9.9.9"}
        }
    })
}

/// Spawn a fake MCP stdio server over two duplex streams.
///
/// Returns `(conn_stdin, conn_stdout, server_handle)` — the two streams to
/// hand to [`McpConnection::from_streams`] and the task to keep alive or
/// abort.
///
/// `handler` maps each incoming JSON-RPC request (a value with an `id`) to an
/// optional response body. `None` leaves the request unanswered (timeout
/// scenarios). Requests whose `method` is `initialize` fall back to the
/// default [`init_result`] when the handler returns `None`, so the connection
/// helpers below can transparently complete the handshake. The response's
/// `id` is filled in from the request when the handler omits it.
pub fn spawn_fake_server<F>(mut handler: F) -> (DuplexStream, DuplexStream, JoinHandle<()>)
where
    F: FnMut(Value) -> Option<Value> + Send + 'static,
{
    let (conn_stdin, mut srv_rx) = duplex(DUPLEX_BUF);
    let (mut srv_tx, conn_stdout) = duplex(DUPLEX_BUF);

    let handle = tokio::spawn(async move {
        let buf = BufReader::new(&mut srv_rx);
        let mut lines = buf.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let val: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let Some(id) = val.get("id").and_then(|i| i.as_u64()) else {
                continue;
            };
            let is_initialize = val.get("method").and_then(|m| m.as_str()) == Some("initialize");
            let response = match handler(val) {
                Some(resp) => resp,
                None if is_initialize => init_result(),
                None => continue,
            };
            let mut body = response;
            if body.get("id").is_none() {
                if let Value::Object(map) = &mut body {
                    map.insert("id".into(), Value::from(id));
                }
            }
            let frame = serde_json::to_string(&body).expect("response must serialize");
            if srv_tx.write_all(frame.as_bytes()).await.is_err()
                || srv_tx.write_all(b"\n").await.is_err()
            {
                break;
            }
            let _ = srv_tx.flush().await;
        }
    });

    (conn_stdin, conn_stdout, handle)
}

/// Build a [`McpConnection`] wired to a fake server, without performing the
/// `initialize` handshake.
pub async fn connect_streams<F>(handler: F, write_capacity: usize) -> Arc<McpConnection>
where
    F: FnMut(Value) -> Option<Value> + Send + 'static,
{
    let (conn_stdin, conn_stdout, _server) = spawn_fake_server(handler);
    McpConnection::from_streams(
        conn_stdin,
        conn_stdout,
        None,
        Duration::from_secs(5),
        write_capacity,
    )
    .await
    .expect("from_streams should succeed")
}

/// Build a [`McpConnection`] wired to a fake server and complete the
/// `initialize` handshake.
///
/// The handshake uses the default [`init_result`]; tests that need to control
/// the initialize response should use [`connect_streams`] and call
/// [`McpConnection::handshake`] themselves.
pub async fn connect_ready<F>(mut handler: F, write_capacity: usize) -> Arc<McpConnection>
where
    F: FnMut(Value) -> Option<Value> + Send + 'static,
{
    let conn = connect_streams(
        move |req| {
            if req.get("method").and_then(|m| m.as_str()) == Some("initialize") {
                return None; // default init result
            }
            handler(req)
        },
        write_capacity,
    )
    .await;
    conn.handshake(Duration::from_secs(5))
        .await
        .expect("initialize handshake should succeed");
    conn
}

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

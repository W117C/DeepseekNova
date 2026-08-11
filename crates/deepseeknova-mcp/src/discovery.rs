use crate::connection::McpConnection;
use crate::http_client::McpHttpConnection;
use deepseeknova_config::{Config, McpServerConfig};
use deepseeknova_core::DeepseeknovaError;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

/// Result of discovering and connecting to MCP servers from config.
pub struct DiscoveredMcpServer {
    /// Name of the MCP server (from the config entry).
    pub name: String,
    /// The established connection for this server.
    pub connection: McpServerConnection,
}

/// Either a stdio or HTTP connection to an MCP server.
pub enum McpServerConnection {
    /// A child-process MCP server connected over stdio.
    Stdio(Arc<McpConnection>),
    /// A remote MCP server connected over HTTP/SSE.
    Http(Arc<McpHttpConnection>),
}

impl McpServerConnection {
    /// Send a JSON-RPC request through whatever transport is active.
    pub async fn request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
    ) -> Result<serde_json::Value, DeepseeknovaError> {
        match self {
            McpServerConnection::Stdio(conn) => conn.request(method, params, timeout).await,
            McpServerConnection::Http(conn) => conn.request(method, params, timeout).await,
        }
    }

    /// The default per-request timeout configured for this transport.
    pub fn request_timeout(&self) -> Duration {
        match self {
            McpServerConnection::Stdio(conn) => conn.request_timeout,
            McpServerConnection::Http(conn) => conn.request_timeout,
        }
    }
}

/// Discover MCP servers from configuration and connect to them.
///
/// For stdio servers (config entries with a `command` field), spawns a child
/// process and performs the MCP initialize handshake.
///
/// For HTTP servers (config entries with a `url` field), connects via HTTP/SSE.
///
/// Servers are connected concurrently. [`join_all`](futures::future::join_all)
/// preserves input order, so the returned Vec follows config order regardless
/// of which handshake finishes first — keeping downstream tool registration
/// (and thus prompt-cache keys) deterministic. Each connection attempt is
/// bounded by `request_timeout` (covering the child-process spawn, which is
/// otherwise unbounded), and failures/timeouts are logged and skipped.
pub async fn discover_and_connect(
    config: &Config,
    request_timeout: Duration,
) -> Vec<DiscoveredMcpServer> {
    let attempts = config.mcp_servers.iter().map(|server_cfg| async move {
        if !server_cfg.enabled {
            info!("MCP server '{}' is disabled, skipping", server_cfg.name);
            return None;
        }

        let name = server_cfg.name.clone();
        match tokio::time::timeout(request_timeout, connect_one(server_cfg, request_timeout)).await
        {
            Ok(Ok(connection)) => {
                info!("MCP server '{name}' connected");
                Some(DiscoveredMcpServer { name, connection })
            }
            Ok(Err(e)) => {
                warn!("MCP server '{name}' failed to connect: {e}");
                None
            }
            Err(_) => {
                warn!("MCP server '{name}' connection timed out after {request_timeout:?}");
                None
            }
        }
    });

    futures::future::join_all(attempts)
        .await
        .into_iter()
        .flatten()
        .collect()
}

/// Connect to a single MCP server based on its config.
async fn connect_one(
    cfg: &McpServerConfig,
    timeout: Duration,
) -> Result<McpServerConnection, DeepseeknovaError> {
    // Determine transport type
    if !cfg.command.is_empty() {
        // Stdio transport
        let conn = McpConnection::connect(
            &cfg.command,
            &cfg.args,
            &cfg.env
                .iter()
                .map(|e| (e.name.clone(), e.value.clone()))
                .collect::<Vec<_>>(),
            timeout,
        )
        .await?;
        Ok(McpServerConnection::Stdio(conn))
    } else if let Some(url) = cfg.args.first() {
        // HTTP transport — the URL is passed as the first arg
        // (convention: if no command, treat first arg as URL)
        if url.starts_with("http://") || url.starts_with("https://") {
            let conn = McpHttpConnection::connect(url, timeout).await?;
            Ok(McpServerConnection::Http(conn))
        } else {
            Err(DeepseeknovaError::runner(format!(
                "MCP server '{}': no command and no HTTP URL configured",
                cfg.name
            )))
        }
    } else {
        Err(DeepseeknovaError::runner(format!(
            "MCP server '{}': must have either a command or an HTTP URL",
            cfg.name
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_config::{Config, McpServerConfig};

    fn bad_server(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.into(),
            // A command that does not exist: spawn fails fast, exercising the
            // warn-and-skip path without blocking sibling servers.
            command: "deepseeknova-nonexistent-mcp-binary".into(),
            args: vec![],
            env: vec![],
            enabled: true,
        }
    }

    #[tokio::test]
    async fn empty_config_yields_no_servers() {
        let config = Config::default();
        let servers = discover_and_connect(&config, Duration::from_secs(1)).await;
        assert!(servers.is_empty());
    }

    #[tokio::test]
    async fn failing_server_does_not_block_others() {
        // Two unreachable servers plus one disabled one. None should connect,
        // but the call must return (not hang) and skip all three gracefully.
        let mut config = Config::default();
        let mut disabled = bad_server("disabled");
        disabled.enabled = false;
        config.mcp_servers = vec![bad_server("first"), disabled, bad_server("second")];

        let servers = discover_and_connect(&config, Duration::from_secs(2)).await;
        assert!(servers.is_empty());
    }
}

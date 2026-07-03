use crate::connection::McpConnection;
use crate::http_client::McpHttpConnection;
use reasonix_config::{Config, McpServerConfig};
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

/// Result of discovering and connecting to MCP servers from config.
pub struct DiscoveredMcpServer {
    pub name: String,
    pub connection: McpServerConnection,
}

/// Either a stdio or HTTP connection to an MCP server.
pub enum McpServerConnection {
    Stdio(Arc<McpConnection>),
    Http(Arc<McpHttpConnection>),
}

impl McpServerConnection {
    /// Send a JSON-RPC request through whatever transport is active.
    pub async fn request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
    ) -> anyhow::Result<serde_json::Value> {
        match self {
            McpServerConnection::Stdio(conn) => conn.request(method, params, timeout).await,
            McpServerConnection::Http(conn) => conn.request(method, params, timeout).await,
        }
    }
}

/// Discover MCP servers from configuration and connect to them.
///
/// For stdio servers (config entries with a `command` field), spawns a child
/// process and performs the MCP initialize handshake.
///
/// For HTTP servers (config entries with a `url` field), connects via HTTP/SSE.
pub async fn discover_and_connect(
    config: &Config,
    request_timeout: Duration,
) -> Vec<DiscoveredMcpServer> {
    let mut servers = Vec::new();

    for server_cfg in &config.mcp_servers {
        if !server_cfg.enabled {
            info!("MCP server '{}' is disabled, skipping", server_cfg.name);
            continue;
        }

        let name = server_cfg.name.clone();
        match connect_one(server_cfg, request_timeout).await {
            Ok(connection) => {
                info!("MCP server '{name}' connected");
                servers.push(DiscoveredMcpServer { name, connection });
            }
            Err(e) => {
                warn!("MCP server '{name}' failed to connect: {e}");
            }
        }
    }

    servers
}

/// Connect to a single MCP server based on its config.
async fn connect_one(
    cfg: &McpServerConfig,
    timeout: Duration,
) -> anyhow::Result<McpServerConnection> {
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
            anyhow::bail!(
                "MCP server '{}': no command and no HTTP URL configured",
                cfg.name
            )
        }
    } else {
        anyhow::bail!(
            "MCP server '{}': must have either a command or an HTTP URL",
            cfg.name
        )
    }
}

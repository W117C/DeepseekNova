use crate::connection::McpConnection;
use crate::types::*;
use anyhow::Context;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// Typed client for MCP protocol operations.
/// Wraps an McpConnection with domain-specific methods.
pub struct McpClient {
    conn: Arc<McpConnection>,
}

impl McpClient {
    pub fn new(conn: Arc<McpConnection>) -> Self {
        Self { conn }
    }

    /// Get the default timeout for requests.
    fn timeout(&self) -> Duration {
        self.conn.request_timeout
    }

    // ------------------------------------------------------------------
    // Tools
    // ------------------------------------------------------------------

    /// List available tools from the MCP server.
    pub async fn list_tools(&self) -> anyhow::Result<Vec<ToolDef>> {
        let result = self
            .conn
            .request("tools/list", None, self.timeout())
            .await
            .context("tools/list failed")?;
        let list: ListToolsResult =
            serde_json::from_value(result).context("invalid tools/list response")?;
        Ok(list.tools)
    }

    /// Call a tool on the MCP server.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> anyhow::Result<CallToolResult> {
        let params = serde_json::to_value(CallToolRequest {
            name: name.into(),
            arguments,
        })?;
        let result = self
            .conn
            .request("tools/call", Some(params), self.timeout())
            .await
            .context("tools/call failed")?;
        let call: CallToolResult =
            serde_json::from_value(result).context("invalid tools/call response")?;
        Ok(call)
    }

    // ------------------------------------------------------------------
    // Resources
    // ------------------------------------------------------------------

    /// List available resources.
    pub async fn list_resources(&self) -> anyhow::Result<Vec<ResourceDef>> {
        let result = self
            .conn
            .request("resources/list", None, self.timeout())
            .await
            .context("resources/list failed")?;
        let list: ListResourcesResult =
            serde_json::from_value(result).context("invalid resources/list response")?;
        Ok(list.resources)
    }

    /// Read a resource by URI.
    pub async fn read_resource(&self, uri: &str) -> anyhow::Result<ReadResourceResult> {
        let params = serde_json::to_value(ReadResourceRequest { uri: uri.into() })?;
        let result = self
            .conn
            .request("resources/read", Some(params), self.timeout())
            .await
            .context("resources/read failed")?;
        let read: ReadResourceResult =
            serde_json::from_value(result).context("invalid resources/read response")?;
        Ok(read)
    }

    // ------------------------------------------------------------------
    // Prompts
    // ------------------------------------------------------------------

    /// List available prompts.
    pub async fn list_prompts(&self) -> anyhow::Result<Vec<PromptDef>> {
        let result = self
            .conn
            .request("prompts/list", None, self.timeout())
            .await
            .context("prompts/list failed")?;
        let list: ListPromptsResult =
            serde_json::from_value(result).context("invalid prompts/list response")?;
        Ok(list.prompts)
    }

    /// Get a prompt by name.
    pub async fn get_prompt(&self, name: &str, arguments: Option<Value>) -> anyhow::Result<Value> {
        let mut params_map = serde_json::Map::new();
        params_map.insert("name".into(), name.into());
        if let Some(args) = arguments {
            params_map.insert("arguments".into(), args);
        }
        let params = Value::Object(params_map);
        self.conn
            .request("prompts/get", Some(params), self.timeout())
            .await
            .context("prompts/get failed")
    }
}

use crate::connection::McpConnection;
use crate::discovery::McpServerConnection;
use crate::types::*;
use deepseeknova_core::DeepseeknovaError;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// Typed client for MCP protocol operations.
/// Wraps an [`McpServerConnection`] (stdio or HTTP) with domain-specific
/// methods, so the same client works over either transport.
pub struct McpClient {
    conn: McpServerConnection,
}

impl McpClient {
    /// Build a client from a stdio connection.
    pub fn new(conn: Arc<McpConnection>) -> Self {
        Self {
            conn: McpServerConnection::Stdio(conn),
        }
    }

    /// Build a client from any discovered transport (stdio or HTTP).
    pub fn from_connection(conn: McpServerConnection) -> Self {
        Self { conn }
    }

    /// Get the default timeout for requests.
    fn timeout(&self) -> Duration {
        self.conn.request_timeout()
    }

    // ------------------------------------------------------------------
    // Tools
    // ------------------------------------------------------------------

    /// List available tools from the MCP server.
    pub async fn list_tools(&self) -> Result<Vec<ToolDef>, DeepseeknovaError> {
        let result = self
            .conn
            .request("tools/list", None, self.timeout())
            .await
            .map_err(|e| DeepseeknovaError::runner(format!("tools/list failed: {e}")))?;
        let list: ListToolsResult = serde_json::from_value(result)
            .map_err(|e| DeepseeknovaError::runner(format!("invalid tools/list response: {e}")))?;
        Ok(list.tools)
    }

    /// Call a tool on the MCP server.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<CallToolResult, DeepseeknovaError> {
        let params = serde_json::to_value(CallToolRequest {
            name: name.into(),
            arguments,
        })?;
        let result = self
            .conn
            .request("tools/call", Some(params), self.timeout())
            .await
            .map_err(|e| DeepseeknovaError::runner(format!("tools/call failed: {e}")))?;
        let call: CallToolResult = serde_json::from_value(result)
            .map_err(|e| DeepseeknovaError::runner(format!("invalid tools/call response: {e}")))?;
        Ok(call)
    }

    // ------------------------------------------------------------------
    // Resources
    // ------------------------------------------------------------------

    /// List available resources.
    pub async fn list_resources(&self) -> Result<Vec<ResourceDef>, DeepseeknovaError> {
        let result = self
            .conn
            .request("resources/list", None, self.timeout())
            .await
            .map_err(|e| DeepseeknovaError::runner(format!("resources/list failed: {e}")))?;
        let list: ListResourcesResult = serde_json::from_value(result).map_err(|e| {
            DeepseeknovaError::runner(format!("invalid resources/list response: {e}"))
        })?;
        Ok(list.resources)
    }

    /// Read a resource by URI.
    pub async fn read_resource(&self, uri: &str) -> Result<ReadResourceResult, DeepseeknovaError> {
        let params = serde_json::to_value(ReadResourceRequest { uri: uri.into() })?;
        let result = self
            .conn
            .request("resources/read", Some(params), self.timeout())
            .await
            .map_err(|e| DeepseeknovaError::runner(format!("resources/read failed: {e}")))?;
        let read: ReadResourceResult = serde_json::from_value(result).map_err(|e| {
            DeepseeknovaError::runner(format!("invalid resources/read response: {e}"))
        })?;
        Ok(read)
    }

    // ------------------------------------------------------------------
    // Prompts
    // ------------------------------------------------------------------

    /// List available prompts.
    pub async fn list_prompts(&self) -> Result<Vec<PromptDef>, DeepseeknovaError> {
        let result = self
            .conn
            .request("prompts/list", None, self.timeout())
            .await
            .map_err(|e| DeepseeknovaError::runner(format!("prompts/list failed: {e}")))?;
        let list: ListPromptsResult = serde_json::from_value(result).map_err(|e| {
            DeepseeknovaError::runner(format!("invalid prompts/list response: {e}"))
        })?;
        Ok(list.prompts)
    }

    /// Get a prompt by name.
    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: Option<Value>,
    ) -> Result<Value, DeepseeknovaError> {
        let mut params_map = serde_json::Map::new();
        params_map.insert("name".into(), name.into());
        if let Some(args) = arguments {
            params_map.insert("arguments".into(), args);
        }
        let params = Value::Object(params_map);
        self.conn
            .request("prompts/get", Some(params), self.timeout())
            .await
            .map_err(|e| DeepseeknovaError::runner(format!("prompts/get failed: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::connect_ready;
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    /// Small write-channel capacity is enough for the fake server to keep up.
    const CHANNEL_CAPACITY: usize = 64;

    async fn client_with<F>(handler: F) -> McpClient
    where
        F: FnMut(Value) -> Option<Value> + Send + 'static,
    {
        McpClient::new(connect_ready(handler, CHANNEL_CAPACITY).await)
    }

    // ── Tools ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_tools_parses_result() {
        let client = client_with(|_req| {
            Some(json!({"result": {"tools": [
                {"name": "t1", "inputSchema": {"type": "object"}},
                {"name": "t2", "description": "d", "inputSchema": {"type": "object"}}
            ]}}))
        })
        .await;

        let tools = client.list_tools().await.expect("tools/list");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "t1");
        assert_eq!(tools[1].description.as_deref(), Some("d"));
    }

    #[tokio::test]
    async fn list_tools_rejects_malformed_response() {
        let client = client_with(|_req| Some(json!({"result": {"unexpected": true}}))).await;

        let err = client
            .list_tools()
            .await
            .expect_err("missing tools field must fail");
        assert!(
            err.to_string().contains("invalid tools/list response"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn call_tool_serializes_params_and_parses_result() {
        let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_capture = Arc::clone(&seen);
        let client = client_with(move |req| {
            seen_capture.lock().unwrap().push(req);
            Some(json!({"result": {"content": [{"type": "text", "text": "hi"}], "isError": false}}))
        })
        .await;

        let result = client
            .call_tool("my_tool", json!({"a": 1}))
            .await
            .expect("tools/call");
        assert_eq!(result.content.len(), 1);
        assert_eq!(result.content[0].text.as_deref(), Some("hi"));

        let captured = seen.lock().unwrap();
        let call = captured
            .iter()
            .find(|r| r["method"] == "tools/call")
            .expect("server saw a tools/call request");
        assert_eq!(call["params"]["name"], "my_tool");
        assert_eq!(call["params"]["arguments"], json!({"a": 1}));
    }

    #[tokio::test]
    async fn call_tool_surfaces_server_error() {
        let client = client_with(|_req| {
            Some(json!({"error": {"code": -32000, "message": "tool exploded"}}))
        })
        .await;

        let err = client
            .call_tool("x", json!({}))
            .await
            .expect_err("server error must fail the call");
        assert!(
            err.to_string().contains("MCP error: tool exploded"),
            "unexpected error: {err:#}"
        );
    }

    // ── Resources ───────────────────────────────────────────────────

    #[tokio::test]
    async fn read_resource_parses_result() {
        let client = client_with(|_req| {
            Some(json!({"result": {"contents": [{"uri": "file:///tmp/a", "text": "data"}]}}))
        })
        .await;

        let result = client
            .read_resource("file:///tmp/a")
            .await
            .expect("resources/read");
        assert_eq!(result.contents.len(), 1);
        assert_eq!(result.contents[0].text.as_deref(), Some("data"));
    }

    // ── Prompts ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_prompts_parses_result() {
        let client = client_with(|_req| {
            Some(json!({"result": {"prompts": [
                {"name": "review", "description": "Code review"}
            ]}}))
        })
        .await;

        let prompts = client.list_prompts().await.expect("prompts/list");
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].name, "review");
        assert_eq!(prompts[0].description.as_deref(), Some("Code review"));
    }

    #[tokio::test]
    async fn get_prompt_forwards_arguments() {
        let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_capture = Arc::clone(&seen);
        let client = client_with(move |req| {
            seen_capture.lock().unwrap().push(req);
            Some(json!({"result": {"messages": []}}))
        })
        .await;

        let result = client
            .get_prompt("review", Some(json!({"code": "x"})))
            .await
            .expect("prompts/get");
        assert_eq!(result["messages"], json!([]));

        let captured = seen.lock().unwrap();
        let get = captured
            .iter()
            .find(|r| r["method"] == "prompts/get")
            .expect("server saw a prompts/get request");
        assert_eq!(get["params"]["name"], "review");
        assert_eq!(get["params"]["arguments"], json!({"code": "x"}));
    }
}

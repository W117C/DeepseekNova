use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 base types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

// ---------------------------------------------------------------------------
// Initialize
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct InitializeRequest {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ClientCapabilities,
    #[serde(rename = "clientInfo")]
    pub client_info: ClientInfo,
}

#[derive(Debug, Serialize)]
pub struct ClientCapabilities {
    #[serde(default)]
    pub roots: Option<RootsCapability>,
    #[serde(default)]
    pub sampling: Option<Value>,
    #[serde(default)]
    pub experimental: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct RootsCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

#[derive(Debug, Serialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
    #[serde(default)]
    pub instructions: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ServerCapabilities {
    #[serde(default)]
    pub tools: Option<ToolsCapability>,
    #[serde(default)]
    pub resources: Option<ResourcesCapability>,
    #[serde(default)]
    pub prompts: Option<PromptsCapability>,
    #[serde(default)]
    pub logging: Option<Value>,
    #[serde(default)]
    pub experimental: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct ToolsCapability {
    #[serde(rename = "listChanged", default)]
    pub list_changed: bool,
}

#[derive(Debug, Deserialize)]
pub struct ResourcesCapability {
    #[serde(default)]
    pub subscribe: bool,
    #[serde(rename = "listChanged", default)]
    pub list_changed: bool,
}

#[derive(Debug, Deserialize)]
pub struct PromptsCapability {
    #[serde(rename = "listChanged", default)]
    pub list_changed: bool,
}

#[derive(Debug, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ListToolsResult {
    pub tools: Vec<ToolDef>,
}

#[derive(Debug, Deserialize)]
pub struct ToolDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Serialize)]
pub struct CallToolRequest {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Deserialize)]
pub struct CallToolResult {
    pub content: Vec<ToolContent>,
    #[serde(rename = "isError", default)]
    pub is_error: bool,
}

#[derive(Debug, Deserialize)]
pub struct ToolContent {
    #[serde(rename = "type")]
    pub content_type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
}

// ---------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ListResourcesResult {
    pub resources: Vec<ResourceDef>,
}

#[derive(Debug, Deserialize)]
pub struct ResourceDef {
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ReadResourceRequest {
    pub uri: String,
}

#[derive(Debug, Deserialize)]
pub struct ReadResourceResult {
    pub contents: Vec<ResourceContent>,
}

#[derive(Debug, Deserialize)]
pub struct ResourceContent {
    pub uri: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub blob: Option<String>,
}

// ---------------------------------------------------------------------------
// Prompts
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ListPromptsResult {
    pub prompts: Vec<PromptDef>,
}

#[derive(Debug, Deserialize)]
pub struct PromptDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub arguments: Option<Vec<PromptArgument>>,
}

#[derive(Debug, Deserialize)]
pub struct PromptArgument {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── JSON-RPC base ────────────────────────────────────────────

    #[test]
    fn test_jsonrpc_request_serialization() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: 1,
            method: "tools/list".into(),
            params: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"method\":\"tools/list\""));
        // params should be omitted when None (skip_serializing_if)
        assert!(!json.contains("params"), "params should be omitted: {json}");
    }

    #[test]
    fn test_jsonrpc_response_success_deserialization() {
        let json_str = r#"{"jsonrpc":"2.0","id":1,"result":{"name":"test"}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json_str).unwrap();
        assert_eq!(resp.jsonrpc, "2.0");
        assert_eq!(resp.id, Some(1));
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_jsonrpc_response_error_deserialization() {
        let json_str =
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json_str).unwrap();
        let err = resp.error.unwrap();
        assert_eq!(err.code, -32601);
        assert_eq!(err.message, "Method not found");
    }

    #[test]
    fn test_jsonrpc_notification_serialization() {
        let notif = JsonRpcNotification {
            jsonrpc: "2.0".into(),
            method: "notifications/initialized".into(),
            params: None,
        };
        let json = serde_json::to_string(&notif).unwrap();
        assert!(json.contains("\"notifications/initialized\""));
        assert!(!json.contains("params"));
    }

    // ── Initialize ───────────────────────────────────────────────

    #[test]
    fn test_initialize_request_serialization() {
        let req = InitializeRequest {
            protocol_version: "2024-11-05".into(),
            capabilities: ClientCapabilities {
                roots: None,
                sampling: None,
                experimental: None,
            },
            client_info: ClientInfo {
                name: "deepnova".into(),
                version: "0.3.0".into(),
            },
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"protocolVersion\""));
        assert!(json.contains("\"clientInfo\""));
        assert!(json.contains("\"deepnova\""));
    }

    #[test]
    fn test_initialize_result_deserialization() {
        let json_str = r#"{
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {"listChanged": true}
            },
            "serverInfo": {
                "name": "test-server",
                "version": "1.0.0"
            }
        }"#;
        let result: InitializeResult = serde_json::from_str(json_str).unwrap();
        assert_eq!(result.protocol_version, "2024-11-05");
        assert_eq!(result.server_info.name, "test-server");
        assert!(result.capabilities.tools.is_some());
        assert!(result.capabilities.resources.is_none());
    }

    // ── Tools ────────────────────────────────────────────────────

    #[test]
    fn test_tool_def_deserialization() {
        let json_str = r#"{
            "name": "read_file",
            "description": "Read a file",
            "inputSchema": {
                "type": "object",
                "properties": {"path": {"type": "string"}}
            }
        }"#;
        let tool: ToolDef = serde_json::from_str(json_str).unwrap();
        assert_eq!(tool.name, "read_file");
        assert_eq!(tool.description.unwrap(), "Read a file");
    }

    #[test]
    fn test_call_tool_request_serialization() {
        let req = CallToolRequest {
            name: "read_file".into(),
            arguments: json!({"path": "/tmp/test.txt"}),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"read_file\""));
        assert!(json.contains("\"/tmp/test.txt\""));
    }

    #[test]
    fn test_call_tool_result_deserialization() {
        let json_str = r#"{
            "content": [
                {"type": "text", "text": "hello world"}
            ],
            "isError": false
        }"#;
        let result: CallToolResult = serde_json::from_str(json_str).unwrap();
        assert_eq!(result.content.len(), 1);
        assert_eq!(result.content[0].text.as_deref(), Some("hello world"));
        assert!(!result.is_error);
    }

    #[test]
    fn test_list_tools_result_deserialization() {
        let json_str = r#"{
            "tools": [
                {"name": "tool1", "inputSchema": {"type": "object"}},
                {"name": "tool2", "description": "desc", "inputSchema": {"type": "object"}}
            ]
        }"#;
        let result: ListToolsResult = serde_json::from_str(json_str).unwrap();
        assert_eq!(result.tools.len(), 2);
        assert_eq!(result.tools[0].name, "tool1");
        assert!(result.tools[0].description.is_none());
        assert_eq!(result.tools[1].description.as_deref(), Some("desc"));
    }

    // ── Resources ────────────────────────────────────────────────

    #[test]
    fn test_resource_def_deserialization() {
        let json_str = r#"{
            "uri": "file:///tmp/doc.txt",
            "name": "doc",
            "mime_type": "text/plain"
        }"#;
        let res: ResourceDef = serde_json::from_str(json_str).unwrap();
        assert_eq!(res.uri, "file:///tmp/doc.txt");
        assert_eq!(res.mime_type.as_deref(), Some("text/plain"));
    }

    #[test]
    fn test_read_resource_result_deserialization() {
        let json_str = r#"{
            "contents": [{"uri": "file:///tmp/doc.txt", "text": "content"}]
        }"#;
        let result: ReadResourceResult = serde_json::from_str(json_str).unwrap();
        assert_eq!(result.contents.len(), 1);
        assert_eq!(result.contents[0].text.as_deref(), Some("content"));
    }

    // ── Prompts ──────────────────────────────────────────────────

    #[test]
    fn test_prompt_def_deserialization() {
        let json_str = r#"{
            "name": "review",
            "description": "Code review prompt",
            "arguments": [
                {"name": "code", "description": "code to review", "required": true}
            ]
        }"#;
        let prompt: PromptDef = serde_json::from_str(json_str).unwrap();
        assert_eq!(prompt.name, "review");
        let args = prompt.arguments.unwrap();
        assert_eq!(args.len(), 1);
        assert!(args[0].required);
    }
}

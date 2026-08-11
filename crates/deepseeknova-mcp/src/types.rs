use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 base types
// ---------------------------------------------------------------------------

/// A JSON-RPC 2.0 request sent to the MCP server.
#[derive(Debug, Serialize)]
pub struct JsonRpcRequest {
    /// The JSON-RPC version, always `"2.0"`.
    pub jsonrpc: String,
    /// Request identifier used to correlate the response.
    pub id: u64,
    /// Name of the method being invoked.
    pub method: String,
    /// Optional method-specific parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// A JSON-RPC 2.0 response received from the MCP server.
#[derive(Debug, Deserialize)]
pub struct JsonRpcResponse {
    /// The JSON-RPC version, always `"2.0"`.
    pub jsonrpc: String,
    /// Identifier of the request this response answers.
    #[serde(default)]
    pub id: Option<u64>,
    /// Successful result payload, present when `error` is `None`.
    #[serde(default)]
    pub result: Option<Value>,
    /// Error details, present when the request failed.
    #[serde(default)]
    pub error: Option<JsonRpcError>,
}

/// A JSON-RPC 2.0 error object.
#[derive(Debug, Deserialize)]
pub struct JsonRpcError {
    /// Numeric error code (e.g. `-32700` parse error, `-32601` method not found).
    pub code: i64,
    /// Human-readable error description.
    pub message: String,
    /// Optional structured error payload.
    #[serde(default)]
    pub data: Option<Value>,
}

/// A JSON-RPC 2.0 notification (a request without an `id`; no response is expected).
#[derive(Debug, Serialize)]
pub struct JsonRpcNotification {
    /// The JSON-RPC version, always `"2.0"`.
    pub jsonrpc: String,
    /// Name of the method being invoked.
    pub method: String,
    /// Optional method-specific parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

// ---------------------------------------------------------------------------
// Initialize
// ---------------------------------------------------------------------------

/// The MCP `initialize` request used to negotiate the protocol version and capabilities.
#[derive(Debug, Serialize)]
pub struct InitializeRequest {
    /// The MCP protocol version the client supports (e.g. `"2024-11-05"`).
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    /// Capabilities the client advertises to the server.
    pub capabilities: ClientCapabilities,
    /// Client name and version, sent as `clientInfo`.
    #[serde(rename = "clientInfo")]
    pub client_info: ClientInfo,
}

/// Capabilities the MCP client advertises during initialization.
#[derive(Debug, Serialize)]
pub struct ClientCapabilities {
    /// Root-listing support (typically `None` for this client).
    #[serde(default)]
    pub roots: Option<RootsCapability>,
    /// Sampling support (unused; reserved by the protocol).
    #[serde(default)]
    pub sampling: Option<Value>,
    /// Experimental, vendor-specific capabilities.
    #[serde(default)]
    pub experimental: Option<Value>,
}

/// Client capability for exposing filesystem roots.
#[derive(Debug, Serialize)]
pub struct RootsCapability {
    /// Whether the client can notify the server of root-list changes.
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

/// Name and version of the MCP client.
#[derive(Debug, Serialize)]
pub struct ClientInfo {
    /// Client name.
    pub name: String,
    /// Client version.
    pub version: String,
}

/// The MCP server's response to an `initialize` request.
#[derive(Debug, Deserialize)]
pub struct InitializeResult {
    /// The negotiated MCP protocol version.
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    /// Capabilities the server advertises.
    pub capabilities: ServerCapabilities,
    /// Server name and version, sent as `serverInfo`.
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
    /// Optional instructions for the client.
    #[serde(default)]
    pub instructions: Option<String>,
}

/// Capabilities the MCP server advertises during initialization.
#[derive(Debug, Deserialize)]
pub struct ServerCapabilities {
    /// Whether the server supports the tools API.
    #[serde(default)]
    pub tools: Option<ToolsCapability>,
    /// Whether the server supports the resources API.
    #[serde(default)]
    pub resources: Option<ResourcesCapability>,
    /// Whether the server supports the prompts API.
    #[serde(default)]
    pub prompts: Option<PromptsCapability>,
    /// Whether the server supports logging.
    #[serde(default)]
    pub logging: Option<Value>,
    /// Experimental, vendor-specific capabilities.
    #[serde(default)]
    pub experimental: Option<Value>,
}

/// Server capability for the tools API.
#[derive(Debug, Deserialize)]
pub struct ToolsCapability {
    /// Whether the server notifies the client of tool-list changes.
    #[serde(rename = "listChanged", default)]
    pub list_changed: bool,
}

/// Server capability for the resources API.
#[derive(Debug, Deserialize)]
pub struct ResourcesCapability {
    /// Whether the server supports resource subscriptions.
    #[serde(default)]
    pub subscribe: bool,
    /// Whether the server notifies the client of resource-list changes.
    #[serde(rename = "listChanged", default)]
    pub list_changed: bool,
}

/// Server capability for the prompts API.
#[derive(Debug, Deserialize)]
pub struct PromptsCapability {
    /// Whether the server notifies the client of prompt-list changes.
    #[serde(rename = "listChanged", default)]
    pub list_changed: bool,
}

/// Name and version of the MCP server.
#[derive(Debug, Deserialize)]
pub struct ServerInfo {
    /// Server name.
    pub name: String,
    /// Server version.
    pub version: String,
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

/// Result of a `tools/list` request.
#[derive(Debug, Deserialize)]
pub struct ListToolsResult {
    /// The tools the server exposes.
    pub tools: Vec<ToolDef>,
}

/// Description of a tool exposed by the MCP server.
#[derive(Debug, Deserialize)]
pub struct ToolDef {
    /// Tool name used when calling it.
    pub name: String,
    /// Human-readable description of the tool.
    #[serde(default)]
    pub description: Option<String>,
    /// JSON Schema describing the tool's arguments, sent as `inputSchema`.
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// A request to invoke a tool on the MCP server.
#[derive(Debug, Serialize)]
pub struct CallToolRequest {
    /// Name of the tool to call.
    pub name: String,
    /// Tool arguments as a JSON object.
    #[serde(default)]
    pub arguments: Value,
}

/// Result of calling a tool on the MCP server.
#[derive(Debug, Deserialize)]
pub struct CallToolResult {
    /// Content blocks produced by the tool.
    pub content: Vec<ToolContent>,
    /// Whether the tool returned an error rather than a normal result.
    #[serde(rename = "isError", default)]
    pub is_error: bool,
}

/// A single content block within a tool result.
#[derive(Debug, Deserialize)]
pub struct ToolContent {
    /// Content type: `"text"`, `"image"`, `"audio"`, or `"resource"`.
    #[serde(rename = "type")]
    pub content_type: String,
    /// Text content, present for text blocks.
    #[serde(default)]
    pub text: Option<String>,
    /// Base64-encoded binary data, present for image/audio blocks.
    #[serde(default)]
    pub data: Option<String>,
    /// MIME type of the content.
    #[serde(default)]
    pub mime_type: Option<String>,
}

// ---------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------

/// Result of a `resources/list` request.
#[derive(Debug, Deserialize)]
pub struct ListResourcesResult {
    /// The resources the server exposes.
    pub resources: Vec<ResourceDef>,
}

/// Description of a resource exposed by the MCP server.
#[derive(Debug, Deserialize)]
pub struct ResourceDef {
    /// URI identifying the resource.
    pub uri: String,
    /// Human-readable resource name.
    pub name: String,
    /// Optional human-readable description.
    #[serde(default)]
    pub description: Option<String>,
    /// Optional MIME type of the resource contents.
    #[serde(default)]
    pub mime_type: Option<String>,
}

/// A request to read a resource from the MCP server.
#[derive(Debug, Serialize)]
pub struct ReadResourceRequest {
    /// URI of the resource to read.
    pub uri: String,
}

/// Result of reading a resource from the MCP server.
#[derive(Debug, Deserialize)]
pub struct ReadResourceResult {
    /// Content blocks making up the resource.
    pub contents: Vec<ResourceContent>,
}

/// A single content block within a resource.
#[derive(Debug, Deserialize)]
pub struct ResourceContent {
    /// URI of the resource this content belongs to.
    pub uri: String,
    /// Optional MIME type of the content.
    #[serde(default)]
    pub mime_type: Option<String>,
    /// Text content, present when the resource is textual.
    #[serde(default)]
    pub text: Option<String>,
    /// Base64-encoded binary content, present when the resource is binary.
    #[serde(default)]
    pub blob: Option<String>,
}

// ---------------------------------------------------------------------------
// Prompts
// ---------------------------------------------------------------------------

/// Result of a `prompts/list` request.
#[derive(Debug, Deserialize)]
pub struct ListPromptsResult {
    /// The prompts the server exposes.
    pub prompts: Vec<PromptDef>,
}

/// Description of a prompt exposed by the MCP server.
#[derive(Debug, Deserialize)]
pub struct PromptDef {
    /// Prompt name used when referencing it.
    pub name: String,
    /// Optional human-readable description.
    #[serde(default)]
    pub description: Option<String>,
    /// Optional list of arguments accepted by the prompt.
    #[serde(default)]
    pub arguments: Option<Vec<PromptArgument>>,
}

/// An argument accepted by a prompt.
#[derive(Debug, Deserialize)]
pub struct PromptArgument {
    /// Argument name.
    pub name: String,
    /// Optional human-readable description of the argument.
    #[serde(default)]
    pub description: Option<String>,
    /// Whether the argument must be supplied by the client.
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
                name: "deepseeknova".into(),
                version: "0.3.0".into(),
            },
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"protocolVersion\""));
        assert!(json.contains("\"clientInfo\""));
        assert!(json.contains("\"deepseeknova\""));
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

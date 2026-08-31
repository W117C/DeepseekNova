use crate::client::McpClient;
use crate::types::ToolDef;
use async_trait::async_trait;
use deepseeknova_core::{DeepseeknovaError, Tool, ToolContext, ToolSchema};
use std::sync::Arc;

/// McpToolAdapter wraps an MCP tool as a deepseeknova_core::Tool.
/// The tool name is namespaced: `mcp__<server>__<tool>`.
pub struct McpToolAdapter {
    schema: ToolSchema,
    server_name: String,
    client: Arc<McpClient>,
    read_only: bool,
}

impl McpToolAdapter {
    /// Create a new adapter for an MCP tool.
    /// `server_name` is the logical name of the MCP server (from config).
    /// `tool_def` is the tool definition obtained from tools/list.
    pub fn new(server_name: impl Into<String>, tool_def: &ToolDef, client: Arc<McpClient>) -> Self {
        let server_name = server_name.into();
        let namespaced = format!("mcp__{server_name}__{}", tool_def.name);

        let description = tool_def
            .description
            .clone()
            .unwrap_or_else(|| format!("MCP tool: {}", tool_def.name));

        // D.1：遵循 MCP `ToolAnnotations.readOnlyHint`——服务器显式标记只读
        // 的工具直接放行只读路径，未提供 annotations 时默认可写（false）。
        let read_only = tool_def
            .annotations
            .as_ref()
            .map(|a| a.read_only_hint)
            .unwrap_or(false);

        Self {
            schema: ToolSchema {
                name: namespaced,
                description,
                parameters: tool_def.input_schema.clone(),
            },
            server_name,
            client,
            read_only,
        }
    }

    /// Get the server this tool belongs to.
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Get the original (un-namespaced) tool name.
    pub fn original_name(&self) -> &str {
        // Strip the mcp__<server>__ prefix
        let prefix = format!("mcp__{}__", self.server_name);
        self.schema
            .name
            .strip_prefix(&prefix)
            .unwrap_or(&self.schema.name)
    }
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn schema(&self) -> ToolSchema {
        self.schema.clone()
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        args: &str,
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
        let arguments: serde_json::Value = if args.trim().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(args).unwrap_or(serde_json::Value::String(args.into()))
        };

        let result = self
            .client
            .call_tool(self.original_name(), arguments)
            .await?;

        // Extract text content from the result
        let text: String = result
            .content
            .iter()
            .filter_map(|c| c.text.clone())
            .collect::<Vec<_>>()
            .join("\n");

        if result.is_error {
            return Err(deepseeknova_core::DeepseeknovaError::tool(format!(
                "MCP tool error: {text}"
            )));
        }

        Ok(text)
    }

    fn read_only(&self) -> bool {
        self.read_only
    }

    // B.2：MCP 工具是否写文件系统。readOnlyHint=true → 只读（不写 fs）；
    // false / 未提供 → 保守按写（质量闭环对 MCP 写同样触发 verify/review）。
    fn writes_fs(&self) -> bool {
        !self.read_only
    }
}

/// Build McpToolAdapter instances for all tools exposed by an MCP server.
///
/// C1：工具按名字排序返回——MCP server 的 `tools/list` 顺序可能随版本/
/// 会话变化，若直接透传会破坏 DeepSeek 前缀缓存（工具段顺序是缓存前缀
/// 的一部分，任何顺序变化都整段 miss）。排序后前缀在会话间逐字节稳定。
pub async fn discover_mcp_tools(
    server_name: &str,
    client: Arc<McpClient>,
) -> Result<Vec<Arc<dyn Tool>>, DeepseeknovaError> {
    let mut tools = client.list_tools().await?;
    // 按原始工具名排序（namespace 前缀 mcp__<server>__ 相同，比较原名即可）。
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    let mut adapters: Vec<Arc<dyn Tool>> = tools
        .iter()
        .map(|t| {
            Arc::new(McpToolAdapter::new(server_name, t, Arc::clone(&client))) as Arc<dyn Tool>
        })
        .collect();
    // B.4：resources/prompts 不再作为死能力——服务端声明时各追加一个只读
    // 工具（`read_resource` / `get_prompt`）。fail-open：不支持该端点的
    // 服务器（list 失败）保持既有 tools-only 行为，不因附加能力报错。
    if let Ok(resources) = client.list_resources().await {
        if !resources.is_empty() {
            adapters.push(Arc::new(McpResourceTool::new(
                server_name,
                Arc::clone(&client),
            )));
        }
    }
    if let Ok(prompts) = client.list_prompts().await {
        if !prompts.is_empty() {
            adapters.push(Arc::new(McpPromptTool::new(server_name, client)));
        }
    }
    Ok(adapters)
}

/// B.4 只读工具：读取 MCP 服务器声明的资源（`resources/read`）。
///
/// 名字 `mcp__<server>__read_resource`；参数 `{"uri": "<uri>"}`。
/// 仅放行 `file://` / `http(s)://` scheme，拒绝其他形态（SSRF 防护，
/// 对齐 web_fetch 的 scheme 白名单；实际解析由 MCP 服务器完成）。
pub struct McpResourceTool {
    server_name: String,
    client: Arc<McpClient>,
}

impl McpResourceTool {
    /// 构造资源读取工具。
    pub fn new(server_name: impl Into<String>, client: Arc<McpClient>) -> Self {
        Self {
            server_name: server_name.into(),
            client,
        }
    }
}

#[async_trait]
impl Tool for McpResourceTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: format!("mcp__{}__read_resource", self.server_name),
            description: "Reads a resource exposed by the MCP server. URI schemes: file://, http(s)://; other schemes are rejected (SSRF protection).".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"uri": {"type": "string", "description": "Resource URI."}},
                "required": ["uri"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: &ToolContext, args: &str) -> Result<String, DeepseeknovaError> {
        let parsed: serde_json::Value = if args.trim().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(args).unwrap_or(serde_json::Value::String(args.into()))
        };
        let uri = parsed["uri"].as_str().ok_or_else(|| {
            DeepseeknovaError::tool("read_resource requires a string `uri` parameter".to_string())
        })?;
        // SSRF 防护：仅放行 file/http(s) scheme，拒绝其他形态（对齐
        // web_fetch 的 scheme 白名单；实际解析由 MCP 服务器完成）。
        if !uri.starts_with("file://")
            && !uri.starts_with("http://")
            && !uri.starts_with("https://")
        {
            return Err(DeepseeknovaError::tool(format!(
                "read_resource: unsupported URI scheme: {uri}"
            )));
        }
        let result = self.client.read_resource(uri).await?;
        let text: String = result
            .contents
            .iter()
            .filter_map(|c| c.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        Ok(text)
    }
}

/// B.4 只读工具：获取 MCP 服务器声明的 prompt（`prompts/get`）。
///
/// 名字 `mcp__<server>__get_prompt`；参数
/// `{"name": "<prompt>", "arguments": {...}}`（arguments 可选），
/// 返回 prompt 渲染结果的 JSON 文本。
pub struct McpPromptTool {
    server_name: String,
    client: Arc<McpClient>,
}

impl McpPromptTool {
    /// 构造 prompt 获取工具。
    pub fn new(server_name: impl Into<String>, client: Arc<McpClient>) -> Self {
        Self {
            server_name: server_name.into(),
            client,
        }
    }
}

#[async_trait]
impl Tool for McpPromptTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: format!("mcp__{}__get_prompt", self.server_name),
            description:
                "Gets a prompt rendered by the MCP server; returns the rendered prompt text (JSON)."
                    .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "Prompt name."},
                    "arguments": {"type": "object", "description": "Optional prompt arguments."}
                },
                "required": ["name"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: &ToolContext, args: &str) -> Result<String, DeepseeknovaError> {
        let parsed: serde_json::Value = if args.trim().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(args).unwrap_or(serde_json::Value::String(args.into()))
        };
        let name = parsed["name"].as_str().ok_or_else(|| {
            DeepseeknovaError::tool("get_prompt requires a string `name` parameter".to_string())
        })?;
        let arguments = parsed.get("arguments").cloned();
        let result = self.client.get_prompt(name, arguments).await?;
        Ok(result.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::McpConnection;
    use crate::test_util::connect_ready;
    use crate::types::{ToolAnnotations, ToolDef};

    fn make_tool_def(name: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: None,
            input_schema: serde_json::json!({"type": "object"}),
            annotations: None,
        }
    }

    fn make_adapter(server: &str, tool_def: &ToolDef) -> McpToolAdapter {
        let conn = Arc::new(McpConnection::new_test());
        let client = Arc::new(McpClient::new(conn));
        McpToolAdapter::new(server, tool_def, client)
    }

    /// B.4：服务端声明 resources/prompts 时，`discover_mcp_tools` 必须产出
    /// 对应的只读工具（`mcp__<server>__read_resource` / `get_prompt`），
    /// 而非仅 tools——resources/prompts 不得是死能力。
    #[tokio::test]
    async fn discover_exposes_resources_and_prompts_as_read_only_tools() {
        let client = Arc::new(
            client_with(|req| match req["method"].as_str() {
                Some("tools/list") => Some(serde_json::json!({"result": {"tools": [
                    {"name": "t1", "inputSchema": {"type": "object"}}
                ]}})),
                Some("resources/list") => Some(serde_json::json!({"result": {"resources": [
                    {"uri": "file:///tmp/a", "name": "a"}
                ]}})),
                Some("prompts/list") => Some(serde_json::json!({"result": {"prompts": [
                    {"name": "review", "description": "Code review"}
                ]}})),
                _ => Some(serde_json::json!({"result": {}})),
            })
            .await,
        );

        let tools = discover_mcp_tools("srv", client).await.expect("discover");
        let names: Vec<String> = tools.iter().map(|t| t.schema().name.clone()).collect();
        assert!(
            names.iter().any(|n| n == "mcp__srv__read_resource"),
            "resources 声明时必须产出 read_resource 工具, got {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "mcp__srv__get_prompt"),
            "prompts 声明时必须产出 get_prompt 工具, got {names:?}"
        );
        for t in tools {
            let n = t.schema().name.clone();
            if n.contains("read_resource") || n.contains("get_prompt") {
                assert!(t.read_only(), "{n} 必须只读");
                assert!(!t.writes_fs(), "{n} 不得写文件系统");
            }
        }
    }

    /// B.4：服务端不声明 resources/prompts 时，不产出对应工具
    /// （保持既有 discover 行为，不臆造能力）。
    #[tokio::test]
    async fn discover_without_resources_prompts_stays_tools_only() {
        let client = Arc::new(
            client_with(|req| match req["method"].as_str() {
                Some("tools/list") => Some(serde_json::json!({"result": {"tools": [
                    {"name": "t1", "inputSchema": {"type": "object"}}
                ]}})),
                // resources/list 与 prompts/list 返回空列表（未声明能力）。
                Some("resources/list") => Some(serde_json::json!({"result": {"resources": []}})),
                Some("prompts/list") => Some(serde_json::json!({"result": {"prompts": []}})),
                _ => Some(serde_json::json!({"result": {}})),
            })
            .await,
        );

        let tools = discover_mcp_tools("srv", client).await.expect("discover");
        let names: Vec<String> = tools.iter().map(|t| t.schema().name.clone()).collect();
        assert_eq!(
            names,
            vec!["mcp__srv__t1"],
            "未声明能力时仅 tools, got {names:?}"
        );
    }

    #[tokio::test]
    async fn read_resource_tool_forwards_uri_and_reads_text() {
        let client = Arc::new(
            client_with(|req| match req["method"].as_str() {
                Some("tools/list") => Some(serde_json::json!({"result": {"tools": []}})),
                Some("resources/list") => Some(serde_json::json!({"result": {"resources": [
                    {"uri": "file:///tmp/a", "name": "a"}
                ]}})),
                Some("prompts/list") => Some(serde_json::json!({"result": {"prompts": []}})),
                Some("resources/read") => Some(serde_json::json!({"result": {"contents": [
                    {"uri": "file:///tmp/a", "text": "file data"}
                ]}})),
                _ => Some(serde_json::json!({"result": {}})),
            })
            .await,
        );
        let tools = discover_mcp_tools("srv", client).await.expect("discover");
        let read_tool = tools
            .iter()
            .find(|t| t.schema().name == "mcp__srv__read_resource")
            .expect("read_resource tool present");
        let out = read_tool
            .execute(
                &deepseeknova_core::tool::ToolContext::new("c"),
                r#"{"uri":"file:///tmp/a"}"#,
            )
            .await
            .expect("read_resource executes");
        assert!(
            out.contains("file data"),
            "read_resource 返回资源文本: {out}"
        );
    }

    async fn client_with<F>(handler: F) -> McpClient
    where
        F: FnMut(serde_json::Value) -> Option<serde_json::Value> + Send + 'static,
    {
        const CHANNEL_CAPACITY: usize = 64;
        McpClient::new(connect_ready(handler, CHANNEL_CAPACITY).await)
    }

    #[test]
    fn test_adapter_namespaced_name() {
        let tool = make_tool_def("read_file");
        let adapter = make_adapter("my-server", &tool);
        assert_eq!(adapter.schema().name, "mcp__my-server__read_file");
    }

    #[test]
    fn test_adapter_server_name() {
        let tool = make_tool_def("read_file");
        let adapter = make_adapter("my-server", &tool);
        assert_eq!(adapter.server_name(), "my-server");
    }

    #[test]
    fn test_adapter_original_name() {
        let tool = make_tool_def("read_file");
        let adapter = make_adapter("my-server", &tool);
        assert_eq!(adapter.original_name(), "read_file");
    }

    #[test]
    fn test_adapter_description_fallback() {
        let tool = make_tool_def("my_tool");
        let adapter = make_adapter("srv", &tool);
        assert!(adapter.schema().description.contains("MCP tool: my_tool"));
    }

    #[test]
    fn test_adapter_read_only_default() {
        let tool = make_tool_def("any");
        let adapter = make_adapter("srv", &tool);
        assert!(!adapter.read_only());
    }

    #[test]
    fn test_adapter_read_only_from_hint() {
        // 服务器标记 readOnlyHint=true → 适配器只读。
        let tool = ToolDef {
            name: "read_only_tool".into(),
            description: None,
            input_schema: serde_json::json!({"type": "object"}),
            annotations: Some(ToolAnnotations {
                read_only_hint: true,
                ..Default::default()
            }),
        };
        let adapter = make_adapter("srv", &tool);
        assert!(adapter.read_only(), "readOnlyHint=true 应使适配器只读");

        // readOnlyHint 显式为 false → 适配器可写。
        let tool = ToolDef {
            name: "writable_tool".into(),
            description: None,
            input_schema: serde_json::json!({"type": "object"}),
            annotations: Some(ToolAnnotations {
                read_only_hint: false,
                ..Default::default()
            }),
        };
        let adapter = make_adapter("srv", &tool);
        assert!(!adapter.read_only(), "readOnlyHint=false 应保持可写");
    }
}

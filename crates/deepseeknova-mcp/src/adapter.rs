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
    let adapters: Vec<Arc<dyn Tool>> = tools
        .iter()
        .map(|t| {
            Arc::new(McpToolAdapter::new(server_name, t, Arc::clone(&client))) as Arc<dyn Tool>
        })
        .collect();
    Ok(adapters)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::McpConnection;
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

use crate::types::ToolSchema;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

/// ToolContext carries runtime state into every tool execution.
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub cancellation: CancellationToken,
    pub call_id: String,
    pub plan_mode: bool,
}

impl ToolContext {
    pub fn new(call_id: impl Into<String>) -> Self {
        Self {
            cancellation: CancellationToken::new(),
            call_id: call_id.into(),
            plan_mode: false,
        }
    }
}

/// Tool is the unified interface for all tools — builtin, MCP, plugin, skill.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Full schema for the tool: name, description, JSON Schema parameters.
    fn schema(&self) -> ToolSchema;

    /// Whether this tool is side-effect-free. Used by permission layer and plan mode.
    fn read_only(&self) -> bool {
        false
    }

    /// Execute the tool with the given JSON arguments string.
    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String>;
}

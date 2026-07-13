use crate::types::ToolSchema;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParallelSafety {
    Safe,
    Exclusive,
    RequiresResource(String),
}

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

    /// Create a ToolContext that shares an external cancellation token.
    /// When the external token is cancelled, tools can check
    /// `ctx.cancellation.is_cancelled()` to abort long-running operations.
    pub fn with_cancellation(call_id: impl Into<String>, cancel: CancellationToken) -> Self {
        Self {
            cancellation: cancel,
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

    /// Safety level for parallel execution scheduling.
    fn safety(&self) -> ParallelSafety {
        ParallelSafety::Exclusive
    }

    /// Optional: if this tool performs any filesystem/network writes, return
    /// false. The Coordinator uses this to enforce the planner/executor split.
    /// Default: false.
    fn read_only(&self) -> bool {
        false
    }

    /// Execute the tool with the given JSON arguments string.
    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String>;
}

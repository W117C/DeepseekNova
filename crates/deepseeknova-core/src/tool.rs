use crate::types::ToolSchema;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParallelSafety {
    Safe,
    Exclusive,
    RequiresResource(String),
}

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct ExtensionRegistry {
    map: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl ExtensionRegistry {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn insert<T: Any + Send + Sync>(&mut self, value: T) {
        self.map.insert(TypeId::of::<T>(), Arc::new(value));
    }

    pub fn get<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.map
            .get(&TypeId::of::<T>())
            .and_then(|val| val.downcast_ref::<T>())
    }
}

impl std::fmt::Debug for ExtensionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let keys: Vec<TypeId> = self.map.keys().cloned().collect();
        f.debug_struct("ExtensionRegistry")
            .field("keys_count", &keys.len())
            .finish()
    }
}

/// ToolContext carries runtime state into every tool execution.
#[derive(Clone)]
pub struct ToolContext {
    pub cancellation: CancellationToken,
    pub call_id: String,
    pub plan_mode: bool,
    pub workspace_root: PathBuf,
    pub extensions: ExtensionRegistry,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("cancellation", &self.cancellation)
            .field("call_id", &self.call_id)
            .field("plan_mode", &self.plan_mode)
            .field("workspace_root", &self.workspace_root)
            .field("extensions", &self.extensions)
            .finish()
    }
}

impl ToolContext {
    pub fn new(call_id: impl Into<String>) -> Self {
        Self {
            cancellation: CancellationToken::new(),
            call_id: call_id.into(),
            plan_mode: false,
            workspace_root: std::env::current_dir().unwrap_or_default(),
            extensions: ExtensionRegistry::new(),
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
            workspace_root: std::env::current_dir().unwrap_or_default(),
            extensions: ExtensionRegistry::new(),
        }
    }

    /// Builder method to override the default workspace root.
    pub fn with_workspace(mut self, workspace_root: PathBuf) -> Self {
        self.workspace_root = workspace_root;
        self
    }

    /// Builder method to insert an extension into the registry.
    pub fn with_extension<T: Any + Send + Sync>(mut self, extension: T) -> Self {
        self.extensions.insert(extension);
        self
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

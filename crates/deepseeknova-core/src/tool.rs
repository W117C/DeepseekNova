use crate::types::ToolSchema;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

/// 工具并行执行的安全级别（供调度器决定能否与其他工具并行执行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParallelSafety {
    /// 安全：可与其他任意工具并行执行。
    Safe,
    /// 独占：必须单独执行，不与任何工具并行。
    Exclusive,
    /// 共享资源：持有同名资源的工具互斥执行。
    RequiresResource(String),
}

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// 类型键控的扩展注册表：按 `TypeId` 存放任意 `Send + Sync` 值，
/// 供 `ToolContext` 携带运行时扩展状态。
#[derive(Clone, Default)]
pub struct ExtensionRegistry {
    map: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl ExtensionRegistry {
    /// 创建空注册表。
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// 按 `T` 的 `TypeId` 插入一个扩展值（同类型后插入者覆盖前者）。
    pub fn insert<T: Any + Send + Sync>(&mut self, value: T) {
        self.map.insert(TypeId::of::<T>(), Arc::new(value));
    }

    /// 按 `T` 的 `TypeId` 取回扩展值的引用。
    pub fn get<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.map
            .get(&TypeId::of::<T>())
            .and_then(|val| val.downcast_ref::<T>())
    }
}

impl std::fmt::Debug for ExtensionRegistry {
    /// 仅输出已注册扩展键数量（避免打印任意扩展值内容）。
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
    /// 取消令牌：工具可据此中止长任务。
    pub cancellation: CancellationToken,
    /// 本次工具调用的 id。
    pub call_id: String,
    /// 是否处于计划模式（plan mode，仅规划不执行写操作）。
    pub plan_mode: bool,
    /// 工作区根目录。
    pub workspace_root: PathBuf,
    /// 扩展注册表（携带额外运行时状态）。
    pub extensions: ExtensionRegistry,
}

impl std::fmt::Debug for ToolContext {
    /// 逐字段输出（扩展注册表按其自定义 Debug 输出键数量）。
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
    /// 创建一个带新取消令牌的 `ToolContext`（默认非 plan 模式、
    /// 工作区根取当前目录、空扩展）。
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

    /// 是否可与同批其他调用并发 spawn（B.1 并行 fan-out）。
    ///
    /// 与 [`read_only`](Self::read_only) 的区别：可并发的 spawn 类工具
    /// （如 `delegate` 子代理）会写文件、但由引擎的信号量（`max_concurrent`）
    /// 与既有写锁/文件级协调兜底冲突；调度器把 `read_only() || parallelizable()`
    /// 的工具并入可并发组，其余仍串行。默认 `false`（保守：未知工具串行）。
    fn parallelizable(&self) -> bool {
        false
    }

    /// 是否执行文件系统写（B.2 质量闭环覆盖）。
    ///
    /// 质量闭环（verify/review/adversarial review）只在发生文件写时触发；
    /// 名称白名单（`write_file|edit_file|move_file|bash`）覆盖不到的工具
    /// （如 MCP 写工具、`remember` 等未来写工具）通过本方法把「写」语义
    /// 显式告知调度器，避免质量闭环被绕过。默认 `false`；MCP adapter 据
    /// `readOnlyHint` 回填（无 hint 保守按写）。
    fn writes_fs(&self) -> bool {
        false
    }

    /// Execute the tool with the given JSON arguments string.
    ///
    /// 返回 [`crate::DeepseeknovaError`] 而非 `anyhow::Error`，让调用方可按
    /// 错误类别（如 [`crate::DeepseeknovaError::is_retryable`]）做细粒度处理。
    /// 实现体内部应直接构造具体类别变体（如 [`crate::DeepseeknovaError::Tool`]）；
    /// 未注册的外部错误类型需在调用点显式 `.map_err(...)?` 归类到合适变体。
    async fn execute(
        &self,
        ctx: &ToolContext,
        args: &str,
    ) -> Result<String, crate::DeepseeknovaError>;
}

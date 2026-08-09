use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tracing::warn;

// ---------------------------------------------------------------------------
// Execution Graph — plan → nodes → execution
// ---------------------------------------------------------------------------

/// 执行图中节点的唯一标识符。
pub type NodeId = String;

/// 执行图：由节点和带条件的边组成的 DAG，描述一次执行计划。
#[derive(Debug, Clone)]
pub struct ExecutionGraph {
    /// 图中所有节点，按 NodeId 索引。
    pub nodes: HashMap<NodeId, ExecutionNode>,
    /// 图中的有向边集合，每条边可附带条件。
    pub edges: Vec<Edge>,
    /// 图的入口节点 id。
    pub entry: NodeId,
}

impl ExecutionGraph {
    /// 创建一个以 `entry` 为入口的空执行图。
    pub fn new(entry: NodeId) -> Self {
        Self {
            nodes: HashMap::new(),
            edges: Vec::new(),
            entry,
        }
    }

    /// 向图中添加一个节点；若 id 已存在则覆盖。
    pub fn add_node(&mut self, node: ExecutionNode) {
        self.nodes.insert(node.id.clone(), node);
    }

    /// Add a directed edge `from → to` with an optional condition.
    ///
    /// Both endpoints must already exist in the graph. An edge referencing a
    /// node id that was never added (e.g. a hallucinated id in planner
    /// output) is **dropped with a `warn!` log** instead of being stored:
    /// keeping it would poison downstream wiring (`topological_sort` would
    /// misreport a "cycle" or the executor would fail with "node must
    /// exist"), so the graph stays fail-soft but the incident is explicit in
    /// the logs. The signature intentionally stays `()` — making it return
    /// `Result` would be a breaking public API change across ~30 call sites
    /// in three crates for a validation that is deliberately non-fatal.
    pub fn add_edge(&mut self, from: NodeId, to: NodeId, condition: Option<EdgeCondition>) {
        if !self.nodes.contains_key(&from) || !self.nodes.contains_key(&to) {
            warn!(
                "add_edge: node id does not exist (from={from:?}, to={to:?}); edge dropped \
                 (fail-soft, see ExecutionGraph::add_edge docs)"
            );
            return;
        }
        self.edges.push(Edge {
            from,
            to,
            condition,
        });
    }
}

/// 执行图中的一个节点：承载一个动作及其重试/超时策略。
#[derive(Debug, Clone)]
pub struct ExecutionNode {
    /// 节点的唯一标识。
    pub id: NodeId,
    /// 节点要执行的动作。
    pub action: Action,
    /// Deprecated: dependency wiring now derives from `ExecutionGraph::edges`
    /// (see `EdgeCondition`); this field is ignored by the executor. Kept
    /// for public API compatibility.
    pub depends_on: Vec<NodeId>,
    /// 节点的重试策略。
    pub retry: RetryPolicy,
    /// 节点执行的超时时间；`None` 表示不限制。
    pub timeout: Option<Duration>,
}

impl ExecutionNode {
    /// 创建一个节点：默认无依赖、默认重试策略、无超时。
    pub fn new(id: impl Into<String>, action: Action) -> Self {
        Self {
            id: id.into(),
            action,
            depends_on: Vec::new(),
            retry: RetryPolicy::default(),
            timeout: None,
        }
    }
}

/// 节点可执行的动作种类。
#[derive(Debug, Clone)]
pub enum Action {
    /// Call the LLM with a prompt.
    Think {
        /// 发给模型的提示文本。
        prompt: String,
    },
    /// Execute a tool.
    CallTool {
        /// 要调用的工具名称。
        tool: String,
        /// 传给工具的参数（JSON 值）。
        args: serde_json::Value,
    },
    /// Observe a tool result.
    Observe {
        /// 关联的工具调用 id。
        tool_call_id: String,
    },
    /// Reflect on completed work against criteria.
    Reflect {
        /// 用于评估的判据列表。
        criteria: Vec<String>,
    },
    /// Delegate to a sub-agent.
    Delegate {
        /// 目标子代理名称。
        sub_agent: String,
        /// 委派给子代理的目标。
        goal: String,
    },
    /// Execute nodes in parallel.
    Parallel(Vec<ExecutionNode>),
    /// Conditional branching.
    Conditional {
        /// 分支条件表达式。
        condition: String,
        /// 条件成立时执行的节点。
        then: Box<ExecutionNode>,
        /// 条件不成立时执行的节点；`None` 表示不执行。
        r#else: Option<Box<ExecutionNode>>,
    },
}

/// 执行图中的有向边，可附带条件。
#[derive(Debug, Clone)]
pub struct Edge {
    /// 边的起点节点 id。
    pub from: NodeId,
    /// 边的终点节点 id。
    pub to: NodeId,
    /// 边的触发条件；`None` 表示无条件（始终满足）。
    pub condition: Option<EdgeCondition>,
}

/// 边的触发条件：基于源节点的输出决定是否激活下游节点。
#[derive(Debug, Clone)]
pub enum EdgeCondition {
    /// 源节点成功完成时激活（Text/ToolResult 输出）。
    Success,
    /// 源节点失败时激活（Error 输出）。
    Failure,
    /// Advance downstream when the source node **failed**. `Retry(n)` is
    /// currently equivalent to `Failure` (any error output satisfies it);
    /// the `n` threshold is reserved for future attempt-count semantics.
    /// `NodeOutput` does not yet carry attempt counts, so `Retry(n > 1)`
    /// must not be relied on as a stricter threshold (Bugbot 审查 MEDIUM 修复：
    /// 原文档声称 n>1 语义但实现无法求值——契约对齐，避免调用方误解）。
    Retry(u32),
    /// 当源节点产生包含指定 id 的 ToolResult 时激活。
    ToolCall(String),
}

/// 节点的重试策略。
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// 最大尝试次数（含首次执行）。
    pub max_attempts: u32,
    /// 重试之间的退避基准时长（按尝试次数线性放大）。
    pub backoff: Duration,
    /// 是否在退避时长上叠加随机抖动以避免惊群。
    pub jitter: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            backoff: Duration::from_secs(1),
            jitter: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Execution results
// ---------------------------------------------------------------------------

/// 整张执行图的执行结果。
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    /// 每个节点的输出，按 NodeId 索引。
    pub node_outputs: HashMap<NodeId, NodeOutput>,
    /// 整次执行累计的资源用量。
    pub total_usage: crate::chunk::Usage,
    /// 图是否全部完成（无节点失败）。
    pub completed: bool,
}

/// 单个节点的输出。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeOutput {
    /// 模型生成的文本输出。
    Text(String),
    /// 工具调用的结果文本。
    ToolResult(String),
    /// 节点执行失败时的错误信息。
    Error(String),
    /// The node's incoming edge conditions were never satisfied (e.g. a
    /// Success-conditioned edge whose source failed). Explicitly distinct
    /// from failure: a skipped node does not fail the graph, and its own
    /// outgoing edges are still evaluated (a skipped node satisfies no
    /// condition, so Success/Failure/Retry/ToolCall edges out of it all
    /// stay unsatisfied).
    Skipped,
}

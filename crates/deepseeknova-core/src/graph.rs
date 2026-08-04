use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tracing::warn;

// ---------------------------------------------------------------------------
// Execution Graph — plan → nodes → execution
// ---------------------------------------------------------------------------

pub type NodeId = String;

#[derive(Debug, Clone)]
pub struct ExecutionGraph {
    pub nodes: HashMap<NodeId, ExecutionNode>,
    pub edges: Vec<Edge>,
    pub entry: NodeId,
}

impl ExecutionGraph {
    pub fn new(entry: NodeId) -> Self {
        Self {
            nodes: HashMap::new(),
            edges: Vec::new(),
            entry,
        }
    }

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

#[derive(Debug, Clone)]
pub struct ExecutionNode {
    pub id: NodeId,
    pub action: Action,
    /// Deprecated: dependency wiring now derives from `ExecutionGraph::edges`
    /// (see `EdgeCondition`); this field is ignored by the executor. Kept
    /// for public API compatibility.
    pub depends_on: Vec<NodeId>,
    pub retry: RetryPolicy,
    pub timeout: Option<Duration>,
}

impl ExecutionNode {
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

#[derive(Debug, Clone)]
pub enum Action {
    /// Call the LLM with a prompt.
    Think { prompt: String },
    /// Execute a tool.
    CallTool {
        tool: String,
        args: serde_json::Value,
    },
    /// Observe a tool result.
    Observe { tool_call_id: String },
    /// Reflect on completed work against criteria.
    Reflect { criteria: Vec<String> },
    /// Delegate to a sub-agent.
    Delegate { sub_agent: String, goal: String },
    /// Execute nodes in parallel.
    Parallel(Vec<ExecutionNode>),
    /// Conditional branching.
    Conditional {
        condition: String,
        then: Box<ExecutionNode>,
        r#else: Option<Box<ExecutionNode>>,
    },
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub condition: Option<EdgeCondition>,
}

#[derive(Debug, Clone)]
pub enum EdgeCondition {
    Success,
    Failure,
    /// Advance downstream when the source node **failed**. `Retry(n)` is
    /// currently equivalent to `Failure` (any error output satisfies it);
    /// the `n` threshold is reserved for future attempt-count semantics.
    /// `NodeOutput` does not yet carry attempt counts, so `Retry(n > 1)`
    /// must not be relied on as a stricter threshold (Bugbot 审查 MEDIUM 修复：
    /// 原文档声称 n>1 语义但实现无法求值——契约对齐，避免调用方误解）。
    Retry(u32),
    ToolCall(String),
}

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff: Duration,
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

#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub node_outputs: HashMap<NodeId, NodeOutput>,
    pub total_usage: crate::chunk::Usage,
    pub completed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeOutput {
    Text(String),
    ToolResult(String),
    Error(String),
    /// The node's incoming edge conditions were never satisfied (e.g. a
    /// Success-conditioned edge whose source failed). Explicitly distinct
    /// from failure: a skipped node does not fail the graph, and its own
    /// outgoing edges are still evaluated (a skipped node satisfies no
    /// condition, so Success/Failure/Retry/ToolCall edges out of it all
    /// stay unsatisfied).
    Skipped,
}

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

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

    pub fn add_edge(&mut self, from: NodeId, to: NodeId, condition: Option<EdgeCondition>) {
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
}

use crate::chunk::Usage;
use crate::graph::{Action, ExecutionGraph, ExecutionNode, ExecutionResult, NodeId, NodeOutput};
use rand::Rng;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;
use tokio::time::sleep;
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// Callbacks — injected by Runtime to execute actions
// ---------------------------------------------------------------------------

/// ThinkCallback is called for Think actions. Returns the model's text response.
#[async_trait::async_trait]
pub trait ThinkCallback: Send + Sync {
    async fn think(&self, prompt: &str) -> anyhow::Result<String>;
}

/// ToolCallback is called for CallTool actions. Returns the tool output.
#[async_trait::async_trait]
pub trait ToolCallback: Send + Sync {
    async fn call_tool(&self, tool: &str, args: &serde_json::Value) -> anyhow::Result<String>;
}

/// ReflectCallback evaluates criteria against completed work.
#[async_trait::async_trait]
pub trait ReflectCallback: Send + Sync {
    async fn reflect(&self, criteria: &[String], context: &str) -> anyhow::Result<ReflectResult>;
}

#[derive(Debug, Clone)]
pub struct ReflectResult {
    pub passed: bool,
    pub feedback: String,
}

/// DelegateCallback is called for Delegate actions. It dispatches work
/// to a named sub-agent and returns the collected text output.
#[async_trait::async_trait]
pub trait DelegateCallback: Send + Sync {
    async fn delegate(&self, sub_agent: &str, goal: &str) -> anyhow::Result<String>;
}

// ---------------------------------------------------------------------------
// GraphExecutor
// ---------------------------------------------------------------------------

pub struct GraphExecutor {
    think: Arc<dyn ThinkCallback>,
    tool: Arc<dyn ToolCallback>,
    reflect: Arc<dyn ReflectCallback>,
    delegate: Option<Arc<dyn DelegateCallback>>,
}

impl GraphExecutor {
    pub fn new(
        think: Arc<dyn ThinkCallback>,
        tool: Arc<dyn ToolCallback>,
        reflect: Arc<dyn ReflectCallback>,
    ) -> Self {
        Self {
            think,
            tool,
            reflect,
            delegate: None,
        }
    }

    /// Attach a delegate callback for handling `Action::Delegate` nodes.
    pub fn with_delegate(mut self, delegate: Arc<dyn DelegateCallback>) -> Self {
        self.delegate = Some(delegate);
        self
    }

    /// Execute an entire graph and return the result.
    pub async fn execute(
        self: Arc<Self>,
        graph: &ExecutionGraph,
    ) -> anyhow::Result<ExecutionResult> {
        let sorted = topological_sort(graph)?;
        let mut outputs: HashMap<NodeId, NodeOutput> = HashMap::new();
        let mut completed = true;

        // Group nodes by "wave" — nodes at the same topological depth
        // can execute concurrently.
        let waves = group_into_waves(&sorted, graph);

        for (wave_idx, wave) in waves.iter().enumerate() {
            debug!("wave {wave_idx}: {} node(s)", wave.len());

            if wave.len() == 1 {
                // Single node — execute inline
                let node_id = &wave[0];
                let node = graph.nodes.get(node_id).unwrap();

                match self.clone().execute_node(node, &outputs).await {
                    Ok(output) => {
                        outputs.insert(node.id.clone(), output);
                    }
                    Err(e) => {
                        warn!("node {node_id} failed: {e}");
                        outputs.insert(node_id.clone(), NodeOutput::Error(format!("{e}")));
                        completed = false;
                    }
                }
            } else {
                // Multiple nodes — execute concurrently via JoinSet
                let mut set = JoinSet::new();
                for node_id in wave {
                    let node = graph.nodes.get(node_id).unwrap().clone();
                    let outputs_snapshot = outputs.clone();
                    let this = Arc::clone(&self);

                    set.spawn(async move {
                        (
                            node.id.clone(),
                            this.execute_node(&node, &outputs_snapshot).await,
                        )
                    });
                }

                while let Some(result) = set.join_next().await {
                    match result {
                        Ok((id, Ok(output))) => {
                            outputs.insert(id, output);
                        }
                        Ok((id, Err(e))) => {
                            warn!("node {id} failed: {e}");
                            outputs.insert(id, NodeOutput::Error(format!("{e}")));
                            completed = false;
                        }
                        Err(e) => {
                            warn!("join error: {e}");
                            completed = false;
                        }
                    }
                }
            }
        }

        Ok(ExecutionResult {
            node_outputs: outputs,
            total_usage: Usage::default(),
            completed,
        })
    }

    /// Execute a single node with retry.
    async fn execute_node(
        self: Arc<Self>,
        node: &ExecutionNode,
        outputs: &HashMap<NodeId, NodeOutput>,
    ) -> anyhow::Result<NodeOutput> {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            match self.execute_action(&node.action, outputs).await {
                Ok(output) => return Ok(output),
                Err(e) if attempt < node.retry.max_attempts => {
                    let mut delay = node.retry.backoff * attempt;
                    if node.retry.jitter {
                        let max_jitter_ms = (delay.as_millis() as u64).min(1000);
                        let jitter_ms = rand::thread_rng().gen_range(0..=max_jitter_ms);
                        delay += Duration::from_millis(jitter_ms);
                    }
                    warn!(
                        "node {} attempt {}/{} failed: {e}. retrying in {delay:?}",
                        node.id, attempt, node.retry.max_attempts
                    );
                    sleep(delay).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Execute a single Action.
    async fn execute_action(
        &self,
        action: &Action,
        outputs: &HashMap<NodeId, NodeOutput>,
    ) -> anyhow::Result<NodeOutput> {
        match action {
            Action::Think { prompt } => {
                let text = self.think.think(prompt).await?;
                Ok(NodeOutput::Text(text))
            }
            Action::CallTool { tool, args } => {
                let result = self.tool.call_tool(tool, args).await?;
                Ok(NodeOutput::ToolResult(result))
            }
            Action::Observe { tool_call_id: _ } => {
                // Find the tool result from a preceding node
                let result = outputs
                    .values()
                    .find_map(|o| match o {
                        NodeOutput::ToolResult(r) => Some(r.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                Ok(NodeOutput::ToolResult(result))
            }
            Action::Reflect { criteria } => {
                // Build context from all prior outputs
                let context = build_context(outputs);
                let result = self.reflect.reflect(criteria, &context).await?;
                Ok(NodeOutput::Text(if result.passed {
                    format!("✓ passed: {}", result.feedback)
                } else {
                    format!("✗ failed: {}", result.feedback)
                }))
            }
            Action::Delegate { sub_agent, goal } => {
                if let Some(ref d) = self.delegate {
                    let text = d.delegate(sub_agent, goal).await?;
                    Ok(NodeOutput::Text(text))
                } else {
                    anyhow::bail!(
                        "Delegate action (sub_agent='{sub_agent}') requires a \
                         DelegateCallback, but none was configured on GraphExecutor"
                    )
                }
            }
            Action::Parallel(nodes) => {
                // Execute sub-nodes sequentially within this action.
                let mut combined = String::new();
                for child in nodes {
                    let result = Box::pin(self.execute_action(&child.action, outputs)).await;
                    match result {
                        Ok(output) => {
                            combined.push_str(&format!("{output:?}\n"));
                        }
                        Err(e) => {
                            combined.push_str(&format!("error: {e}\n"));
                        }
                    }
                }
                Ok(NodeOutput::Text(combined))
            }
            Action::Conditional {
                condition: _,
                then,
                r#else: _,
            } => Box::pin(self.execute_action(&then.action, outputs)).await,
        }
    }
}

// ---------------------------------------------------------------------------
// Topological sort (Kahn's algorithm)
// ---------------------------------------------------------------------------

fn topological_sort(graph: &ExecutionGraph) -> anyhow::Result<Vec<NodeId>> {
    let mut in_degree: HashMap<&NodeId, usize> = HashMap::new();
    let mut adjacency: HashMap<&NodeId, Vec<&NodeId>> = HashMap::new();

    for node_id in graph.nodes.keys() {
        in_degree.entry(node_id).or_insert(0);
        adjacency.entry(node_id).or_default();
    }

    for edge in &graph.edges {
        *in_degree.entry(&edge.to).or_insert(0) += 1;
        adjacency.entry(&edge.from).or_default().push(&edge.to);
    }

    let mut queue: VecDeque<&NodeId> = in_degree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(id, _)| *id)
        .collect();

    let mut sorted = Vec::new();
    while let Some(node_id) = queue.pop_front() {
        sorted.push(node_id.clone());
        if let Some(neighbors) = adjacency.get(node_id) {
            for neighbor in neighbors {
                if let Some(deg) = in_degree.get_mut(neighbor) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(neighbor);
                    }
                }
            }
        }
    }

    if sorted.len() != graph.nodes.len() {
        anyhow::bail!("graph contains a cycle");
    }

    Ok(sorted)
}

/// Group topologically sorted nodes into waves of concurrent execution.
fn group_into_waves(sorted: &[NodeId], graph: &ExecutionGraph) -> Vec<Vec<NodeId>> {
    // Build reverse adjacency: which nodes depend on which
    let mut depends_on: HashMap<&NodeId, HashSet<&NodeId>> = HashMap::new();
    for node_id in sorted {
        if let Some(node) = graph.nodes.get(node_id) {
            for dep in &node.depends_on {
                depends_on.entry(node_id).or_default().insert(dep);
            }
        }
    }

    let mut waves: Vec<Vec<NodeId>> = Vec::new();
    let mut placed: HashSet<NodeId> = HashSet::new();

    for node_id in sorted {
        if placed.contains(node_id) {
            continue;
        }

        // Find the wave: all nodes whose dependencies are already placed
        let mut wave = Vec::new();
        let mut i = 0;
        while i < sorted.len() {
            let candidate = &sorted[i];
            if placed.contains(candidate) {
                i += 1;
                continue;
            }
            let deps = depends_on.get(candidate);
            let all_deps_placed = deps.is_none_or(|deps| deps.iter().all(|d| placed.contains(*d)));
            if all_deps_placed {
                wave.push(candidate.clone());
            }
            i += 1;
        }

        if wave.is_empty() {
            // Fallback: place remaining one at a time
            for node_id in sorted {
                if !placed.contains(node_id) {
                    waves.push(vec![node_id.clone()]);
                    placed.insert(node_id.clone());
                }
            }
            break;
        }

        for n in &wave {
            placed.insert(n.clone());
        }
        waves.push(wave);
    }

    waves
}

fn build_context(outputs: &HashMap<NodeId, NodeOutput>) -> String {
    let mut ctx = String::new();
    for (id, output) in outputs {
        match output {
            NodeOutput::Text(t) => ctx.push_str(&format!("[{id}]: {t}\n")),
            NodeOutput::ToolResult(r) => ctx.push_str(&format!("[{id}]: {r}\n")),
            NodeOutput::Error(e) => ctx.push_str(&format!("[{id}] ERROR: {e}\n")),
        }
    }
    ctx
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{ExecutionNode, RetryPolicy};

    fn make_think_node(id: &str, prompt: &str) -> ExecutionNode {
        ExecutionNode::new(
            id,
            Action::Think {
                prompt: prompt.to_string(),
            },
        )
    }

    #[allow(dead_code)]
    fn make_tool_node(id: &str, tool: &str) -> ExecutionNode {
        ExecutionNode::new(
            id,
            Action::CallTool {
                tool: tool.to_string(),
                args: serde_json::json!({}),
            },
        )
    }

    #[test]
    fn topological_sort_linear() {
        let mut g = ExecutionGraph::new("a".into());
        g.add_node(make_think_node("a", "first"));
        g.add_node(make_think_node("b", "second"));
        g.add_node(make_think_node("c", "third"));
        g.add_edge("a".into(), "b".into(), None);
        g.add_edge("b".into(), "c".into(), None);

        let sorted = topological_sort(&g).unwrap();
        assert_eq!(sorted, vec!["a", "b", "c"]);
    }

    #[test]
    fn topological_sort_diamond() {
        let mut g = ExecutionGraph::new("a".into());
        g.add_node(make_think_node("a", "start"));
        g.add_node(make_think_node("b", "left"));
        g.add_node(make_think_node("c", "right"));
        g.add_node(make_think_node("d", "end"));
        g.add_edge("a".into(), "b".into(), None);
        g.add_edge("a".into(), "c".into(), None);
        g.add_edge("b".into(), "d".into(), None);
        g.add_edge("c".into(), "d".into(), None);

        let sorted = topological_sort(&g).unwrap();
        assert_eq!(sorted[0], "a");
        assert_eq!(sorted[3], "d");
        // b and c can be in either order
        assert!(sorted[1..3].contains(&"b".to_string()));
        assert!(sorted[1..3].contains(&"c".to_string()));
    }

    #[test]
    fn topological_sort_detects_cycle() {
        let mut g = ExecutionGraph::new("a".into());
        g.add_node(make_think_node("a", "first"));
        g.add_node(make_think_node("b", "second"));
        g.add_edge("a".into(), "b".into(), None);
        g.add_edge("b".into(), "a".into(), None);

        assert!(topological_sort(&g).is_err());
    }

    #[test]
    fn waves_group_independent_nodes() {
        let mut g = ExecutionGraph::new("a".into());
        g.add_node(make_think_node("a", "start"));
        g.add_node(make_think_node("b", "left"));
        g.add_node(make_think_node("c", "right"));

        let sorted = topological_sort(&g).unwrap();
        let waves = group_into_waves(&sorted, &g);

        // a should be alone in first wave (entry point), b and c together
        let total_nodes: usize = waves.iter().map(|w| w.len()).sum();
        assert_eq!(total_nodes, 3);
    }

    #[test]
    fn retry_policy_defaults() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_attempts, 3);
        assert_eq!(policy.backoff, Duration::from_secs(1));
        assert!(policy.jitter);
    }

    // Mock callbacks for integration tests
    struct MockThink;
    #[async_trait::async_trait]
    impl ThinkCallback for MockThink {
        async fn think(&self, prompt: &str) -> anyhow::Result<String> {
            Ok(format!("thought: {prompt}"))
        }
    }

    struct MockTool;
    #[async_trait::async_trait]
    impl ToolCallback for MockTool {
        async fn call_tool(&self, tool: &str, _args: &serde_json::Value) -> anyhow::Result<String> {
            Ok(format!("tool {tool} done"))
        }
    }

    struct MockReflect;
    #[async_trait::async_trait]
    impl ReflectCallback for MockReflect {
        async fn reflect(
            &self,
            criteria: &[String],
            _context: &str,
        ) -> anyhow::Result<ReflectResult> {
            Ok(ReflectResult {
                passed: true,
                feedback: format!("criteria met: {criteria:?}"),
            })
        }
    }

    struct MockDelegate;
    #[async_trait::async_trait]
    impl DelegateCallback for MockDelegate {
        async fn delegate(&self, sub_agent: &str, goal: &str) -> anyhow::Result<String> {
            Ok(format!("[{sub_agent}] executed: {goal}"))
        }
    }

    #[tokio::test]
    async fn executor_runs_single_think_node() {
        let mut g = ExecutionGraph::new("think1".into());
        g.add_node(make_think_node("think1", "hello"));

        let exec = Arc::new(GraphExecutor::new(
            Arc::new(MockThink),
            Arc::new(MockTool),
            Arc::new(MockReflect),
        ));

        let result = exec.execute(&g).await.unwrap();
        assert!(result.completed);
        assert!(result.node_outputs.contains_key("think1"));

        match &result.node_outputs["think1"] {
            NodeOutput::Text(t) => assert!(t.contains("thought: hello")),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn executor_runs_linear_chain() {
        let mut g = ExecutionGraph::new("a".into());
        g.add_node(make_think_node("a", "start"));
        g.add_node(make_think_node("b", "mid"));
        g.add_node(make_think_node("c", "end"));
        g.add_edge("a".into(), "b".into(), None);
        g.add_edge("b".into(), "c".into(), None);

        let exec = Arc::new(GraphExecutor::new(
            Arc::new(MockThink),
            Arc::new(MockTool),
            Arc::new(MockReflect),
        ));

        let result = exec.execute(&g).await.unwrap();
        assert!(result.completed);
        assert_eq!(result.node_outputs.len(), 3);
    }

    #[tokio::test]
    async fn executor_runs_reflect_node() {
        let mut g = ExecutionGraph::new("a".into());
        g.add_node(make_think_node("a", "work"));
        g.add_node(ExecutionNode::new(
            "reflect",
            Action::Reflect {
                criteria: vec!["correctness".into(), "completeness".into()],
            },
        ));
        g.add_edge("a".into(), "reflect".into(), None);

        let exec = Arc::new(GraphExecutor::new(
            Arc::new(MockThink),
            Arc::new(MockTool),
            Arc::new(MockReflect),
        ));

        let result = exec.execute(&g).await.unwrap();
        assert!(result.completed);

        match &result.node_outputs["reflect"] {
            NodeOutput::Text(t) => {
                assert!(t.contains("passed"));
                assert!(t.contains("correctness"));
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn executor_runs_delegate_with_callback() {
        let mut g = ExecutionGraph::new("d1".into());
        g.add_node(ExecutionNode::new(
            "d1",
            Action::Delegate {
                sub_agent: "researcher".into(),
                goal: "find all Rust files".into(),
            },
        ));

        let exec = Arc::new(
            GraphExecutor::new(
                Arc::new(MockThink),
                Arc::new(MockTool),
                Arc::new(MockReflect),
            )
            .with_delegate(Arc::new(MockDelegate)),
        );

        let result = exec.execute(&g).await.unwrap();
        assert!(result.completed);

        match &result.node_outputs["d1"] {
            NodeOutput::Text(t) => {
                assert!(t.contains("[researcher]"));
                assert!(t.contains("find all Rust files"));
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn executor_delegate_without_callback_errors() {
        let mut g = ExecutionGraph::new("d1".into());
        g.add_node(ExecutionNode::new(
            "d1",
            Action::Delegate {
                sub_agent: "worker".into(),
                goal: "do work".into(),
            },
        ));

        let exec = Arc::new(GraphExecutor::new(
            Arc::new(MockThink),
            Arc::new(MockTool),
            Arc::new(MockReflect),
        ));

        let result = exec.execute(&g).await.unwrap();
        assert!(!result.completed);

        match &result.node_outputs["d1"] {
            NodeOutput::Error(e) => {
                assert!(e.contains("DelegateCallback"));
                assert!(e.contains("worker"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }
}

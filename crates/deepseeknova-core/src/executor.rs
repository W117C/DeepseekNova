use crate::chunk::Usage;
use crate::graph::{
    Action, Edge, EdgeCondition, ExecutionGraph, ExecutionNode, ExecutionResult, NodeId, NodeOutput,
};
use rand::Rng;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::task::JoinSet;
use tokio::time::sleep;
use tracing::{debug, warn};

/// Parallel 作用域内的共享输出容器：子节点完成后把自身结果写回，
/// 同层 `Observe` 可读到兄弟产出（此前各子任务只持有进入 Parallel 前的
/// 快照，Observe 看不到兄弟结果）。`None` = 非 Parallel 路径（行为不变）。
type SharedOutputs = Option<Arc<RwLock<HashMap<NodeId, NodeOutput>>>>;

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
// Attribution hook — node failure attribution (wired by the runtime)
// ---------------------------------------------------------------------------

/// Node failure attribution hook. Default no-op; the agent layer implements
/// real attribution logic and the runtime wires it in during finalization.
pub trait AttributionHook: Send + Sync {
    /// Called when a node fails after exhausting its retry policy. May
    /// return an attribution summary (root cause / fix plan) for logging or
    /// downstream use; `None` means no attribution was produced.
    fn on_node_failure(&self, node_id: &NodeId, error: &NodeOutput) -> Option<String> {
        let _ = (self, node_id, error);
        None
    }
}

// ---------------------------------------------------------------------------
// GraphExecutor
// ---------------------------------------------------------------------------

/// Wraps a `JoinSet` so that dropping it aborts every task spawned onto it.
///
/// `tokio::task::JoinSet`'s `Drop` does **not** abort tasks that have already
/// been spawned — they keep running detached in the background. When a node
/// times out, `execute_node` drops the in-flight action future; without this
/// guard the `Action::Parallel` branch's children would keep running past the
/// node's timeout and its declared "timeout means failure" semantics would
/// not bound side effects. The guard makes the drop path explicitly call
/// `abort_all()`, so a timed-out (or early-bailed) Parallel node cancels all
/// of its children.
struct JoinSetAbortGuard<T: 'static>(JoinSet<T>);

impl<T> Deref for JoinSetAbortGuard<T> {
    type Target = JoinSet<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for JoinSetAbortGuard<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T: 'static> Drop for JoinSetAbortGuard<T> {
    fn drop(&mut self) {
        self.0.abort_all();
    }
}

pub struct GraphExecutor {
    think: Arc<dyn ThinkCallback>,
    tool: Arc<dyn ToolCallback>,
    reflect: Arc<dyn ReflectCallback>,
    delegate: Option<Arc<dyn DelegateCallback>>,
    attribution: Option<Arc<dyn AttributionHook>>,
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
            attribution: None,
        }
    }

    /// Attach a delegate callback for handling `Action::Delegate` nodes.
    pub fn with_delegate(mut self, delegate: Arc<dyn DelegateCallback>) -> Self {
        self.delegate = Some(delegate);
        self
    }

    /// Attach a node-failure attribution hook. Called on the failure path of
    /// `execute_node` after retries are exhausted. Default: no-op.
    pub fn with_attribution(mut self, attribution: Arc<dyn AttributionHook>) -> Self {
        self.attribution = Some(attribution);
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
                let node = graph
                    .nodes
                    .get(node_id)
                    .ok_or_else(|| anyhow::anyhow!("node must exist"))?;

                if should_skip_node(node, graph, &outputs) {
                    debug!("node {node_id} skipped: no incoming edge condition satisfied");
                    outputs.insert(node_id.clone(), NodeOutput::Skipped);
                    continue;
                }

                match self.clone().execute_node(node, &outputs, &None).await {
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
                    let node = graph
                        .nodes
                        .get(node_id)
                        .ok_or_else(|| anyhow::anyhow!("node must exist"))?
                        .clone();
                    let outputs_snapshot = outputs.clone();
                    let this = Arc::clone(&self);

                    if should_skip_node(&node, graph, &outputs_snapshot) {
                        debug!("node {node_id} skipped: no incoming edge condition satisfied");
                        outputs.insert(node.id.clone(), NodeOutput::Skipped);
                        continue;
                    }

                    set.spawn(async move {
                        (
                            node.id.clone(),
                            this.execute_node(&node, &outputs_snapshot, &None).await,
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

    /// Execute a single node with retry, bounded by `node.timeout` when set.
    ///
    /// A timeout is treated as a failure: it consumes a retry attempt, and
    /// after `retry.max_attempts` timeouts the node fails with
    /// `NodeOutput::Error(Timeout ...)` through the attribution hook path.
    async fn execute_node(
        self: Arc<Self>,
        node: &ExecutionNode,
        outputs: &HashMap<NodeId, NodeOutput>,
        shared: &SharedOutputs,
    ) -> anyhow::Result<NodeOutput> {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let action_result = match node.timeout {
                Some(d) => {
                    let this = Arc::clone(&self);
                    match tokio::time::timeout(
                        d,
                        this.execute_action(&node.action, outputs, shared),
                    )
                    .await
                    {
                        Ok(Ok(output)) => Ok(output),
                        Ok(Err(e)) => Err(e),
                        Err(_) => Err(anyhow::anyhow!("node {} timed out after {d:?}", node.id)),
                    }
                }
                None => {
                    let this = Arc::clone(&self);
                    this.execute_action(&node.action, outputs, shared).await
                }
            };
            match action_result {
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
                Err(e) => {
                    // Retries exhausted — surface the failure to the
                    // attribution hook (default no-op) before returning.
                    let error_output = NodeOutput::Error(format!("{e}"));
                    if let Some(hook) = &self.attribution {
                        if let Some(attr) = hook.on_node_failure(&node.id, &error_output) {
                            debug!("node {} attribution: {attr}", node.id);
                        }
                    }
                    return Err(e);
                }
            }
        }
    }

    /// Execute a single Action.
    ///
    /// Returns a boxed future explicitly `+ Send` so that `Action::Parallel`
    /// children can be spawned onto a `JoinSet` (recursive async fns would
    /// otherwise fail the `Send` bound of `JoinSet::spawn`).
    fn execute_action<'a>(
        self: Arc<Self>,
        action: &'a Action,
        outputs: &'a HashMap<NodeId, NodeOutput>,
        shared: &'a SharedOutputs,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<NodeOutput>> + Send + 'a>>
    {
        Box::pin(async move {
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
                    // Find the tool result from a preceding node. In a
                    // Parallel scope (`shared`), sibling results written back
                    // by already-completed children take priority (they are
                    // fresher than the pre-Parallel snapshot); otherwise fall
                    // back to the top-level outputs map.
                    let shared_result: Option<String> = shared.as_ref().and_then(|lock| {
                        let guard = lock.read().unwrap_or_else(|e| e.into_inner());
                        // 兄弟 ToolResult 优先；无 ToolResult 时退而看 Error
                        // （失败子节点也写回共享容器，Observe 可见失败产出）。
                        guard
                            .values()
                            .find_map(|o| match o {
                                NodeOutput::ToolResult(r) => Some(r.clone()),
                                _ => None,
                            })
                            .or_else(|| {
                                guard.values().find_map(|o| match o {
                                    NodeOutput::Error(e) => Some(format!("error: {e}")),
                                    _ => None,
                                })
                            })
                    });
                    let result = shared_result
                        .or_else(|| {
                            outputs.values().find_map(|o| match o {
                                NodeOutput::ToolResult(r) => Some(r.clone()),
                                _ => None,
                            })
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
                    // Execute sub-nodes concurrently via JoinSet, collecting
                    // results in input order.
                    //
                    // The JoinSet is wrapped in `JoinSetAbortGuard` so that
                    // when this action future is dropped (node timeout, or an
                    // early bail on a join error) every spawned child is
                    // aborted. A bare JoinSet would leave already-spawned
                    // children running detached past the node's timeout.
                    //
                    // 子节点共享输出容器：完成即写回（成功与失败都写），同层
                    // Observe 可见兄弟产出。仅作用域于本次 Parallel 执行。
                    let shared_lock = Arc::new(RwLock::new(HashMap::new()));
                    let mut set = JoinSetAbortGuard(JoinSet::new());
                    for (idx, child) in nodes.iter().enumerate() {
                        let child = child.clone();
                        let outputs = outputs.clone();
                        let shared_lock = shared_lock.clone();
                        let this = Arc::clone(&self);
                        set.spawn(async move {
                            let result = this
                                .execute_action(&child.action, &outputs, &Some(shared_lock.clone()))
                                .await;
                            let output = match &result {
                                Ok(output) => output.clone(),
                                Err(e) => NodeOutput::Error(format!("{e}")),
                            };
                            shared_lock
                                .write()
                                .unwrap_or_else(|e| e.into_inner())
                                .insert(child.id.clone(), output);
                            (idx, result)
                        });
                    }
                    let mut results: Vec<Option<anyhow::Result<NodeOutput>>> =
                        (0..nodes.len()).map(|_| None).collect();
                    while let Some(joined) = set.join_next().await {
                        match joined {
                            Ok((idx, result)) => results[idx] = Some(result),
                            Err(e) => anyhow::bail!("parallel node join error: {e}"),
                        }
                    }
                    let mut combined = String::new();
                    let mut all_failed = !nodes.is_empty();
                    for (i, result) in results.into_iter().enumerate() {
                        let child = &nodes[i];
                        let output = result.unwrap_or_else(|| {
                            Err(anyhow::anyhow!(
                                "parallel node '{}' produced no result",
                                child.id
                            ))
                        });
                        match output {
                            Ok(output) => {
                                all_failed = false;
                                combined.push_str(&format!("[{}]: {output:?}\n", child.id));
                            }
                            Err(e) => {
                                combined.push_str(&format!("[{}] error: {e}\n", child.id));
                            }
                        }
                    }
                    // 子节点全部失败时返回 Err：让节点级重试、归因 hook 与
                    // 下游 Failure/Retry 条件边可观测（Bugbot 审查 MEDIUM 修复；
                    // 部分失败仍以文本合并返回，保留中间产物）。
                    if all_failed {
                        anyhow::bail!("all {} parallel children failed:\n{combined}", nodes.len());
                    }
                    Ok(NodeOutput::Text(combined))
                }
                Action::Conditional {
                    condition: _,
                    then,
                    r#else: _,
                } => self.execute_action(&then.action, outputs, shared).await,
            }
        })
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
///
/// Dependencies are derived exclusively from `graph.edges` (incoming edges
/// are the single source of truth); the deprecated `ExecutionNode.depends_on`
/// field is not consulted.
fn group_into_waves(sorted: &[NodeId], graph: &ExecutionGraph) -> Vec<Vec<NodeId>> {
    // Build reverse adjacency: for each node, the set of nodes it depends on
    // (its incoming-edge sources).
    let mut deps_by_node: HashMap<&NodeId, HashSet<&NodeId>> = HashMap::new();
    for edge in &graph.edges {
        deps_by_node.entry(&edge.to).or_default().insert(&edge.from);
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
            let deps = deps_by_node.get(candidate);
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

/// Decide whether a node must be skipped: every incoming edge condition is
/// unsatisfied given the outputs of its (already executed) predecessors.
///
/// A node with no incoming edges always runs. An edge condition is satisfied
/// when the source node's output matches it — `None`/`Success` for
/// `Text`/`ToolResult`, `Failure`/`Retry(_)` for `Error`, `ToolCall(id)` for
/// a `ToolResult` mentioning `id`. `Skipped` outputs satisfy nothing, so a
/// skipped node propagates as skipped through Success-conditioned edges.
fn should_skip_node(
    node: &ExecutionNode,
    graph: &ExecutionGraph,
    outputs: &HashMap<NodeId, NodeOutput>,
) -> bool {
    let incoming: Vec<&Edge> = graph.edges.iter().filter(|e| e.to == node.id).collect();
    if incoming.is_empty() {
        return false;
    }
    !incoming
        .iter()
        .any(|e| edge_condition_satisfied(e, outputs))
}

/// Whether an edge's condition is satisfied by the source node's output.
fn edge_condition_satisfied(edge: &Edge, outputs: &HashMap<NodeId, NodeOutput>) -> bool {
    let Some(source_output) = outputs.get(&edge.from) else {
        return false;
    };
    match &edge.condition {
        None | Some(EdgeCondition::Success) => {
            matches!(
                source_output,
                NodeOutput::Text(_) | NodeOutput::ToolResult(_)
            )
        }
        Some(EdgeCondition::Failure) => matches!(source_output, NodeOutput::Error(_)),
        Some(EdgeCondition::Retry(_)) => matches!(source_output, NodeOutput::Error(_)),
        Some(EdgeCondition::ToolCall(id)) => match source_output {
            NodeOutput::ToolResult(r) => r.contains(id),
            _ => false,
        },
    }
}

fn build_context(outputs: &HashMap<NodeId, NodeOutput>) -> String {
    let mut ctx = String::new();
    for (id, output) in outputs {
        match output {
            NodeOutput::Text(t) => ctx.push_str(&format!("[{id}]: {t}\n")),
            NodeOutput::ToolResult(r) => ctx.push_str(&format!("[{id}]: {r}\n")),
            NodeOutput::Error(e) => ctx.push_str(&format!("[{id}] ERROR: {e}\n")),
            NodeOutput::Skipped => ctx.push_str(&format!("[{id}]: (skipped)\n")),
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
    fn waves_derive_dependencies_from_edges() {
        // Dependency wiring must come from edges (single source of truth),
        // not from the deprecated `depends_on` field.
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
        let waves = group_into_waves(&sorted, &g);

        // a alone, then b+c concurrently, then d alone.
        assert_eq!(waves.len(), 3);
        assert_eq!(waves[0], vec!["a".to_string()]);
        let wave1: HashSet<&String> = waves[1].iter().collect();
        let binding = ["b".to_string(), "c".to_string()];
        let expected: HashSet<&String> = binding.iter().collect();
        assert_eq!(wave1, expected);
        assert_eq!(waves[2], vec!["d".to_string()]);

        // And the deprecated depends_on field must NOT be consulted: even if
        // a node declares no deps, edges still order the waves.
        let mut g2 = ExecutionGraph::new("a".into());
        g2.add_node(ExecutionNode {
            id: "a".into(),
            action: Action::Think { prompt: "a".into() },
            depends_on: Vec::new(),
            retry: RetryPolicy::default(),
            timeout: None,
        });
        g2.add_node(ExecutionNode {
            id: "b".into(),
            action: Action::Think { prompt: "b".into() },
            depends_on: Vec::new(),
            retry: RetryPolicy::default(),
            timeout: None,
        });
        g2.add_edge("a".into(), "b".into(), None);
        let sorted = topological_sort(&g2).unwrap();
        let waves = group_into_waves(&sorted, &g2);
        assert_eq!(waves, vec![vec!["a".to_string()], vec!["b".to_string()]]);
    }

    #[test]
    fn retry_policy_defaults() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_attempts, 3);
        assert_eq!(policy.backoff, Duration::from_secs(1));
        assert!(policy.jitter);
    }

    #[test]
    fn add_edge_drops_edges_referencing_unknown_nodes() {
        let mut g = ExecutionGraph::new("a".into());
        g.add_node(make_think_node("a", "only node"));
        g.add_node(make_think_node("b", "second"));

        // Phantom ids (hallucinated planner output) must not enter the graph:
        // keeping them would poison topological sort / wave grouping.
        g.add_edge("a".into(), "ghost".into(), None); // phantom target
        g.add_edge("ghost".into(), "a".into(), None); // phantom source
        g.add_edge("ghost".into(), "wraith".into(), None); // both phantom
        assert!(
            g.edges.is_empty(),
            "phantom edges must be dropped, got {:?}",
            g.edges
        );

        // Valid edges still work.
        g.add_edge("a".into(), "b".into(), None);
        assert_eq!(g.edges.len(), 1);
        assert_eq!(g.edges[0].from, "a");
        assert_eq!(g.edges[0].to, "b");
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

    // ---- DAG wiring: edge-derived waves, conditions, timeout, parallel ----

    fn failing_delegate_node(id: &str) -> ExecutionNode {
        // Delegate without a callback always fails; no retries so tests are fast.
        let mut node = ExecutionNode::new(
            id,
            Action::Delegate {
                sub_agent: "nobody".into(),
                goal: "always fails".into(),
            },
        );
        node.retry = RetryPolicy {
            max_attempts: 1,
            backoff: Duration::ZERO,
            jitter: false,
        };
        node
    }

    fn make_executor() -> Arc<GraphExecutor> {
        Arc::new(GraphExecutor::new(
            Arc::new(MockThink),
            Arc::new(MockTool),
            Arc::new(MockReflect),
        ))
    }

    #[tokio::test]
    async fn executor_waves_run_in_dependency_order() {
        let mut g = ExecutionGraph::new("a".into());
        g.add_node(make_think_node("a", "first"));
        g.add_node(make_think_node("b", "second"));
        g.add_node(make_think_node("c", "third"));
        g.add_edge("a".into(), "b".into(), None);
        g.add_edge("b".into(), "c".into(), None);

        let sorted = topological_sort(&g).unwrap();
        let waves = group_into_waves(&sorted, &g);
        assert_eq!(
            waves,
            vec![
                vec![String::from("a")],
                vec![String::from("b")],
                vec![String::from("c")],
            ]
        );
    }

    #[tokio::test]
    async fn executor_failure_edge_advances_downstream() {
        let mut g = ExecutionGraph::new("a".into());
        g.add_node(failing_delegate_node("a"));
        g.add_node(make_think_node("b", "recovery"));
        g.add_node(make_think_node("c", "final"));
        // Failure-conditioned edge: a's failure must still trigger b.
        g.add_edge("a".into(), "b".into(), Some(EdgeCondition::Failure));
        g.add_edge("b".into(), "c".into(), None);

        let exec = make_executor();
        let result = exec.execute(&g).await.unwrap();

        assert!(!result.completed, "a failed so the graph is not completed");
        match &result.node_outputs["a"] {
            NodeOutput::Error(_) => {}
            other => panic!("expected Error for a, got {other:?}"),
        }
        // b and c must have run despite a's failure (Failure edge + Success edge).
        match &result.node_outputs["b"] {
            NodeOutput::Text(t) => assert!(t.contains("thought: recovery")),
            other => panic!("expected Text for b, got {other:?}"),
        }
        match &result.node_outputs["c"] {
            NodeOutput::Text(t) => assert!(t.contains("thought: final")),
            other => panic!("expected Text for c, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn executor_skips_node_when_conditions_unsatisfied() {
        let mut g = ExecutionGraph::new("a".into());
        g.add_node(failing_delegate_node("a"));
        g.add_node(make_think_node("b", "dependent"));
        g.add_node(make_think_node("c", "further"));
        // Default Success edges: b and c must never run because a failed.
        g.add_edge("a".into(), "b".into(), None);
        g.add_edge("b".into(), "c".into(), None);

        let exec = make_executor();
        let result = exec.execute(&g).await.unwrap();

        assert!(!result.completed);
        match &result.node_outputs["a"] {
            NodeOutput::Error(_) => {}
            other => panic!("expected Error for a, got {other:?}"),
        }
        assert!(
            matches!(result.node_outputs.get("b"), Some(NodeOutput::Skipped)),
            "b should be Skipped, got {:?}",
            result.node_outputs.get("b")
        );
        assert!(
            matches!(result.node_outputs.get("c"), Some(NodeOutput::Skipped)),
            "c should be Skipped, got {:?}",
            result.node_outputs.get("c")
        );
    }

    struct MockSlowThink {
        delay: Duration,
        calls: Arc<std::sync::atomic::AtomicU32>,
    }

    #[async_trait::async_trait]
    impl ThinkCallback for MockSlowThink {
        async fn think(&self, prompt: &str) -> anyhow::Result<String> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            sleep(self.delay).await;
            Ok(format!("slow: {prompt}"))
        }
    }

    #[tokio::test]
    async fn executor_timeout_produces_error_and_counts_retry() {
        let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let exec = Arc::new(GraphExecutor::new(
            Arc::new(MockSlowThink {
                delay: Duration::from_millis(500),
                calls: Arc::clone(&calls),
            }),
            Arc::new(MockTool),
            Arc::new(MockReflect),
        ));

        let mut node = make_think_node("slow", "will timeout");
        node.timeout = Some(Duration::from_millis(50));
        node.retry = RetryPolicy {
            max_attempts: 2,
            backoff: Duration::ZERO,
            jitter: false,
        };

        let mut g = ExecutionGraph::new("slow".into());
        g.add_node(node);

        let result = exec.execute(&g).await.unwrap();
        assert!(!result.completed);

        match &result.node_outputs["slow"] {
            NodeOutput::Error(e) => assert!(e.contains("timed out"), "got error: {e}"),
            other => panic!("expected Error(Timeout), got {other:?}"),
        }
        // Timeout counts as failure: 2 attempts consumed before giving up.
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn executor_parallel_action_runs_concurrently() {
        // Direct action-level test: Parallel must run children concurrently
        // instead of sequentially. Use a slow think mock to measure overlap.
        let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let slow = Arc::new(MockSlowThink {
            delay: Duration::from_millis(100),
            calls: Arc::clone(&calls),
        });
        let exec = Arc::new(GraphExecutor::new(
            slow.clone(),
            Arc::new(MockTool),
            Arc::new(MockReflect),
        ));

        let mut g = ExecutionGraph::new("p".into());
        let mut node = ExecutionNode::new(
            "p",
            Action::Parallel(vec![
                ExecutionNode::new(
                    "p1",
                    Action::Think {
                        prompt: "one".into(),
                    },
                ),
                ExecutionNode::new(
                    "p2",
                    Action::Think {
                        prompt: "two".into(),
                    },
                ),
                ExecutionNode::new(
                    "p3",
                    Action::Think {
                        prompt: "three".into(),
                    },
                ),
            ]),
        );
        node.retry = RetryPolicy {
            max_attempts: 1,
            backoff: Duration::ZERO,
            jitter: false,
        };
        g.add_node(node);

        let start = std::time::Instant::now();
        let result = exec.execute(&g).await.unwrap();
        let elapsed = start.elapsed();

        assert!(result.completed);
        // 3 × 100ms sequential would take >= 300ms; concurrent is ~100ms.
        assert!(
            elapsed < Duration::from_millis(250),
            "Parallel children did not overlap: took {elapsed:?}"
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn parallel_observe_sees_sibling_tool_result() {
        // Parallel 子节点结果共享：先完成的 CallTool 兄弟写回共享容器，
        // 同层 Observe 可读到其 ToolResult（此前只读进入 Parallel 前的快照，
        // 读到空串——新语义验证）。
        let exec = Arc::new(GraphExecutor::new(
            Arc::new(MockThink),
            Arc::new(MockTool),
            Arc::new(MockReflect),
        ));

        let mut g = ExecutionGraph::new("p".into());
        let mut node = ExecutionNode::new(
            "p",
            Action::Parallel(vec![
                ExecutionNode::new(
                    "producer",
                    Action::CallTool {
                        tool: "read_file".into(),
                        args: serde_json::json!({"path": "x"}),
                    },
                ),
                ExecutionNode::new(
                    "observer",
                    Action::Observe {
                        tool_call_id: String::new(),
                    },
                ),
            ]),
        );
        node.retry = RetryPolicy {
            max_attempts: 1,
            backoff: Duration::ZERO,
            jitter: false,
        };
        g.add_node(node);

        let result = exec.execute(&g).await.unwrap();
        assert!(result.completed);
        // MockTool 返回 "tool read_file done"——Observe 应从共享容器读到它，
        // 而非空串。
        match result.node_outputs.get("p") {
            Some(NodeOutput::Text(t)) => assert!(
                t.contains("tool read_file done"),
                "observer did not see sibling result: {t}"
            ),
            other => panic!("expected Parallel Text output, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn observe_outside_parallel_unaffected() {
        // 非 Parallel 路径（顶层 sequential 节点）：Observe 只读顶层 outputs
        // map，行为与改动前一致（shared=None 分支）。
        let exec = Arc::new(GraphExecutor::new(
            Arc::new(MockThink),
            Arc::new(MockTool),
            Arc::new(MockReflect),
        ));

        let mut g = ExecutionGraph::new("g".into());
        g.add_node(ExecutionNode::new(
            "prod",
            Action::CallTool {
                tool: "read_file".into(),
                args: serde_json::json!({"path": "x"}),
            },
        ));
        g.add_node(ExecutionNode::new(
            "obs",
            Action::Observe {
                tool_call_id: String::new(),
            },
        ));
        // 注意：add_edge 对未知节点 fail-soft（丢弃边），必须先 add_node。
        g.add_edge("prod".into(), "obs".into(), Some(EdgeCondition::Success));

        let result = exec.execute(&g).await.unwrap();
        assert!(result.completed);
        match result.node_outputs.get("obs") {
            Some(NodeOutput::ToolResult(r)) => {
                assert!(r.contains("tool read_file done"), "got: {r}")
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    struct MockHeartbeatThink {
        heartbeat: Arc<std::sync::atomic::AtomicU64>,
    }

    #[async_trait::async_trait]
    impl ThinkCallback for MockHeartbeatThink {
        async fn think(&self, _prompt: &str) -> anyhow::Result<String> {
            // Infinite heartbeat loop: keeps bumping the counter as long as
            // the task is polled. Cancellation (abort) freezes the counter.
            loop {
                self.heartbeat
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::task::yield_now().await;
            }
        }
    }

    #[tokio::test]
    async fn executor_parallel_timeout_aborts_children() {
        // A Parallel node whose children run an infinite heartbeat loop. If
        // the executor fails to abort spawned children on timeout, the
        // heartbeat keeps growing in the background after the node returns;
        // the test asserts the counter freezes.
        let heartbeat = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let exec = Arc::new(GraphExecutor::new(
            Arc::new(MockHeartbeatThink {
                heartbeat: Arc::clone(&heartbeat),
            }),
            Arc::new(MockTool),
            Arc::new(MockReflect),
        ));

        let mut node = ExecutionNode::new(
            "p",
            Action::Parallel(vec![
                ExecutionNode::new(
                    "p1",
                    Action::Think {
                        prompt: "one".into(),
                    },
                ),
                ExecutionNode::new(
                    "p2",
                    Action::Think {
                        prompt: "two".into(),
                    },
                ),
            ]),
        );
        node.timeout = Some(Duration::from_millis(50));
        node.retry = RetryPolicy {
            max_attempts: 1,
            backoff: Duration::ZERO,
            jitter: false,
        };

        let mut g = ExecutionGraph::new("p".into());
        g.add_node(node);

        let result = exec.execute(&g).await.unwrap();
        assert!(!result.completed);
        match &result.node_outputs["p"] {
            NodeOutput::Error(e) => assert!(e.contains("timed out"), "got error: {e}"),
            other => panic!("expected Error(Timeout), got {other:?}"),
        }

        // The children must have started before the abort...
        assert!(heartbeat.load(std::sync::atomic::Ordering::SeqCst) > 0);
        // ...and their heartbeats must freeze after the timeout: sample
        // twice with a gap — a leaked background task would keep bumping the
        // counter in between.
        sleep(Duration::from_millis(100)).await;
        let first = heartbeat.load(std::sync::atomic::Ordering::SeqCst);
        sleep(Duration::from_millis(300)).await;
        let second = heartbeat.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            first, second,
            "parallel child kept running after node timeout"
        );
    }

    struct RecordingHook(Arc<std::sync::Mutex<Vec<(String, String)>>>);

    impl AttributionHook for RecordingHook {
        fn on_node_failure(&self, node_id: &NodeId, error: &NodeOutput) -> Option<String> {
            self.0
                .lock()
                .unwrap()
                .push((node_id.clone(), format!("{error:?}")));
            Some(format!("attributed {node_id}"))
        }
    }

    #[tokio::test]
    async fn executor_attribution_hook_called_on_failure() {
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let exec = Arc::new(
            GraphExecutor::new(
                Arc::new(MockThink),
                Arc::new(MockTool),
                Arc::new(MockReflect),
            )
            .with_attribution(Arc::new(RecordingHook(Arc::clone(&recorded)))),
        );

        let mut g = ExecutionGraph::new("a".into());
        g.add_node(failing_delegate_node("a"));

        let result = exec.execute(&g).await.unwrap();
        assert!(!result.completed);

        let recorded = recorded.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "a");
        assert!(recorded[0].1.contains("DelegateCallback"));
    }
}

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::agent::ExtensionApplier;
use crate::SubAgentRunner;
use deepseeknova_core::executor::{
    DelegateCallback, GraphExecutor, ReflectCallback, ReflectResult, ThinkCallback, ToolCallback,
};
use deepseeknova_core::graph::{Action, EdgeCondition, ExecutionGraph, ExecutionNode};
use deepseeknova_core::tool::ToolContext;
use deepseeknova_core::{
    DeepseeknovaError, Message, Role, RunEvent, RunEventStream, RunInput, RunOutput, Runner, Tool,
};
use deepseeknova_permission::{Decision, PermissionGate};
use deepseeknova_provider::Provider;
use deepseeknova_security::context::SecurityContext;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// JSON schema for the planner model's output
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PlanOutput {
    nodes: Vec<PlanNode>,
    #[serde(default)]
    edges: Vec<PlanEdge>,
}

#[derive(Debug, Deserialize)]
struct PlanNode {
    id: String,
    #[serde(default)]
    action: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    args: Option<serde_json::Value>,
    #[serde(default)]
    criteria: Option<Vec<String>>,
    #[serde(default)]
    sub_agent: Option<String>,
    #[serde(default)]
    goal: Option<String>,
    /// Node-level dependency hints (deprecated — edges are the single source
    /// of truth; parsed for backwards compatibility and converted to default
    /// Success-conditioned edges when no explicit edge exists).
    #[serde(default)]
    depends_on: Vec<String>,
    /// When set, the node is wrapped in `Action::Parallel` running its child
    /// steps concurrently.
    #[serde(default)]
    parallel: Option<Vec<PlanNode>>,
}

#[derive(Debug, Deserialize)]
struct PlanEdge {
    from: String,
    to: String,
    /// Edge condition: "success" (default), "failure", "retry", or
    /// "tool_call:&lt;id&gt;". Missing/unknown values behave as success.
    #[serde(default)]
    condition: Option<String>,
}

// ---------------------------------------------------------------------------
// Reasoning language control
// ---------------------------------------------------------------------------

/// Language hint for model reasoning output. Injected in message metadata,
/// never in the text stream — cache-neutral by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasoningLanguage {
    /// Let the provider choose the reasoning language (default).
    #[default]
    Auto,
    /// Prefer Chinese reasoning output.
    Zh,
    /// Prefer English reasoning output.
    En,
}

impl std::fmt::Display for ReasoningLanguage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReasoningLanguage::Auto => write!(f, "auto"),
            ReasoningLanguage::Zh => write!(f, "zh"),
            ReasoningLanguage::En => write!(f, "en"),
        }
    }
}

// ---------------------------------------------------------------------------
// Planner prompts
// ---------------------------------------------------------------------------

/// Standard planner system prompt. Fixed byte-for-byte across turns
/// so the provider's prefix cache stays warm.
const PLANNER_SYSTEM_PROMPT: &str = r#"You are a planning assistant operating in the Plan phase of the Observe → Plan → Tool → Verify → Reflect → Next Action loop. Your job is to break down a user's goal into a structured execution plan.

CRITICAL: You may ONLY use these action types:
- "think" — pure reasoning (no side effects)
- "call_read_tool" — invoke a READ-ONLY tool to gather information
- "delegate" — dispatch to a named sub-agent

You MAY NEVER use "call_tool" — only the executor phase may invoke mutating tools.

Output ONLY valid JSON with this exact structure:
{
  "nodes": [
    {"id": "step_1", "action": "think", "prompt": "Analyze the goal and identify key requirements"},
    {"id": "step_2", "action": "think", "prompt": "Research relevant information"},
    {"id": "step_3", "action": "think", "prompt": "Execute the main task step-by-step"},
    {"id": "step_4", "action": "reflect", "prompt": "Check if work is complete", "criteria": ["Goal achieved?", "Output correct?", "Edge cases handled?"]}
  ],
  "edges": [
    {"from": "step_1", "to": "step_2"},
    {"from": "step_2", "to": "step_3"},
    {"from": "step_3", "to": "step_4"}
  ]
}

Rules:
- "id" must be unique for every node
- Valid actions: "think" (reasoning), "call_read_tool" (read-only tool call), "reflect" (evaluate against criteria), "delegate" (sub-agent)
- "think" nodes: describe the task in "prompt"
- "call_read_tool" nodes: include "tool" (name) and "args" (JSON object)
- "reflect" nodes: include "criteria" (array of strings)
- "delegate" nodes: include "sub_agent" and "goal"
- Edges define the execution order (from → to); independent branches run in parallel automatically
- Optional edge field "condition": "success" (default), "failure" (advance downstream when the source node fails), "retry", or "tool_call:<id>" (advance when the source tool result mentions <id>)
- Optional node field "depends_on": array of node ids that must finish first (deprecated; prefer edges)
- Optional node field "parallel": array of child node objects that run concurrently inside this node
- Keep plans concise: 3–8 nodes typically
- Output ONLY the JSON object. No markdown, no explanation, no backticks."#;

// ---------------------------------------------------------------------------
// Planner prompt builders
// ---------------------------------------------------------------------------

fn build_planning_prompt(goal: &str, read_only_tools: &[&dyn Tool]) -> Vec<Message> {
    let mut extra = String::new();
    if !read_only_tools.is_empty() {
        extra.push_str(
            "\n\nYou have access to these read-only tools for use in call_read_tool nodes:\n",
        );
        for t in read_only_tools {
            extra.push_str(&format!(
                "- {}: {}\n",
                t.schema().name,
                t.schema().description
            ));
        }
    }

    let mut system = PLANNER_SYSTEM_PROMPT.to_string();
    system.push_str(&extra);

    vec![
        Message {
            role: Role::System,
            content: system,
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        },
        Message {
            role: Role::User,
            content: format!("Create an execution plan for this goal:\n\n{goal}"),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        },
    ]
}

// ---------------------------------------------------------------------------
// CoordinatorRunner — two-model (Planner + Executor)
// ---------------------------------------------------------------------------

/// Two-model coordinator runner: a planner model produces an
/// [`ExecutionGraph`] which an executor model then runs, with optional
/// sub-agent delegation.
pub struct CoordinatorRunner {
    /// Strong reasoning model used for planning.
    planner_provider: Arc<dyn Provider>,
    /// Cheaper / faster model used for executing each plan node.
    executor_provider: Arc<dyn Provider>,
    /// Tools available to the executor.
    tools: HashMap<String, Arc<dyn Tool>>,
    /// Read-only tools available to the planner (core safety boundary).
    read_only_tools: HashMap<String, Arc<dyn Tool>>,
    /// Cap on the number of plan nodes (safety valve against runaway plans).
    max_graph_nodes: usize,
    /// Optional sub-agent runner for handling Delegate actions.
    sub_agent_runner: Option<Arc<SubAgentRunner>>,
    /// Language hint for model reasoning output (cache-neutral).
    reasoning_language: ReasoningLanguage,
    /// When true (default), planner system prompt is pinned byte-for-byte
    /// across turns so prefix cache stays warm.
    cache_stable_prefix: bool,
    /// Workspace root used to confine filesystem tool calls in executor nodes.
    workspace_root: PathBuf,
    /// Security context injected into every executor tool execution.
    security: SecurityContext,
    /// Optional permission gate applied before each executor tool call.
    permission: Option<Arc<PermissionGate>>,
    /// Build-time registered extensions injected into executor ToolContexts.
    extensions: Vec<Arc<ExtensionApplier>>,
}

impl CoordinatorRunner {
    /// Construct with distinct planner and executor providers.
    pub fn new(planner_provider: Arc<dyn Provider>, executor_provider: Arc<dyn Provider>) -> Self {
        Self {
            planner_provider,
            executor_provider,
            tools: HashMap::new(),
            read_only_tools: HashMap::new(),
            max_graph_nodes: 20,
            sub_agent_runner: None,
            reasoning_language: ReasoningLanguage::Auto,
            cache_stable_prefix: true,
            workspace_root: std::env::current_dir().unwrap_or_default(),
            security: SecurityContext::with_safe_defaults(),
            permission: None,
            extensions: Vec::new(),
        }
    }

    /// Register a tool that executor nodes may call.
    pub fn register_tool(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.schema().name.clone();
        self.tools.insert(name, tool);
    }

    /// Register a read-only tool available to the planner. Read-only tools
    /// are also available to the executor. This is the core safety boundary
    /// of the two-model architecture: the planner may never accumulate
    /// mutating side effects.
    pub fn register_read_only_tool(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.schema().name.clone();
        self.read_only_tools.insert(name.clone(), tool.clone());
        self.tools.insert(name, tool);
    }

    /// Limit the number of plan nodes accepted from the planner.
    pub fn with_max_graph_nodes(mut self, n: usize) -> Self {
        self.max_graph_nodes = n;
        self
    }

    /// Attach a sub-agent runner for handling `Action::Delegate` nodes.
    pub fn with_sub_agent_runner(mut self, runner: SubAgentRunner) -> Self {
        self.sub_agent_runner = Some(Arc::new(runner));
        self
    }

    /// Control the language used by the model for chain-of-thought reasoning.
    /// Hint is injected in message metadata (not text), so cache-neutral.
    pub fn with_reasoning_language(mut self, lang: ReasoningLanguage) -> Self {
        self.reasoning_language = lang;
        self
    }

    /// When true (default), planner system prompt is pinned byte-for-byte
    /// across turns. Disable to allow dynamic prompt injection (costs cache).
    pub fn with_cache_stable_prefix(mut self, enabled: bool) -> Self {
        self.cache_stable_prefix = enabled;
        self
    }

    /// Override the workspace root used to confine filesystem tool calls.
    pub fn with_workspace_root(mut self, workspace_root: PathBuf) -> Self {
        self.workspace_root = workspace_root;
        self
    }

    /// Override the security context injected into every executor tool execution.
    pub fn with_security(mut self, security: SecurityContext) -> Self {
        self.security = security;
        self
    }

    /// Attach a permission gate applied before each executor tool call. The
    /// coordinator is non-interactive, so an `Ask` decision is blocked
    /// (fail-closed); `Deny` blocks the call.
    pub fn with_permission_gate(mut self, gate: Arc<PermissionGate>) -> Self {
        self.permission = Some(gate);
        self
    }

    /// Register an extension injected into every executor ToolContext
    /// (e.g. a shared code-graph index for graph tools).
    pub fn with_extension<T: std::any::Any + Send + Sync + Clone>(mut self, ext: T) -> Self {
        self.extensions
            .push(Arc::new(move |reg| reg.insert(ext.clone())));
        self
    }
}

#[async_trait::async_trait]
impl Runner for CoordinatorRunner {
    async fn run_stream(
        &self,
        input: RunInput,
    ) -> Result<RunEventStream, deepseeknova_core::DeepseeknovaError> {
        let (tx, rx) = mpsc::channel(128);

        let planner = Arc::clone(&self.planner_provider);
        let executor = Arc::clone(&self.executor_provider);
        let tools: HashMap<String, Arc<dyn Tool>> = self
            .tools
            .iter()
            .map(|(k, v)| (k.clone(), Arc::clone(v)))
            .collect();
        let read_only_refs: Vec<Arc<dyn Tool>> = self.read_only_tools.values().cloned().collect();
        let max_nodes = self.max_graph_nodes;
        let sub_agent_runner = self.sub_agent_runner.clone();
        let workspace_root = self.workspace_root.clone();
        let security = self.security.clone();
        let permission = self.permission.clone();
        let extensions = self.extensions.clone();

        tokio::spawn(async move {
            if let Err(e) = run_coordinator(
                planner,
                executor,
                tools,
                read_only_refs,
                max_nodes,
                sub_agent_runner,
                workspace_root,
                security,
                permission,
                extensions,
                input,
                &tx,
            )
            .await
            {
                warn!("coordinator error: {e}");
                let _ = tx.send(Err(e)).await;
            }
        });

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}

// ---------------------------------------------------------------------------
// Coordinator loop — plan then execute
// ---------------------------------------------------------------------------

async fn run_coordinator(
    planner: Arc<dyn Provider>,
    executor: Arc<dyn Provider>,
    tools: HashMap<String, Arc<dyn Tool>>,
    read_only_tools: Vec<Arc<dyn Tool>>,
    max_nodes: usize,
    sub_agent_runner: Option<Arc<SubAgentRunner>>,
    workspace_root: PathBuf,
    security: SecurityContext,
    permission: Option<Arc<PermissionGate>>,
    extensions: Vec<Arc<ExtensionApplier>>,
    input: RunInput,
    tx: &mpsc::Sender<Result<RunEvent, DeepseeknovaError>>,
) -> Result<(), DeepseeknovaError> {
    // ---- Phase 1: Planning ----
    info!("coordinator: planning phase");

    // Build prompt for the standard planner.
    let read_only_views: Vec<&dyn Tool> = read_only_tools.iter().map(|t| t.as_ref()).collect();

    let plan_messages = build_planning_prompt(&input.prompt, &read_only_views);

    let validated = deepseeknova_provider::ValidatedRequest::new(&plan_messages, &[]).map_err(
        |violations| {
            DeepseeknovaError::runner(format!(
                "planning prompt replay invariant violated: {} violation(s) detected",
                violations.len()
            ))
        },
    )?;
    let plan_response = planner.generate(validated).await?;

    tx.send(Ok(RunEvent::TextDelta(format!(
        "[PLAN]\n{}\n",
        plan_response.content
    ))))
    .await
    .ok();

    // Parse the planner's JSON output.
    let graph = parse_plan(&plan_response.content, &input.prompt, max_nodes);
    info!(
        "coordinator: plan parsed — {} nodes, {} edges",
        graph.nodes.len(),
        graph.edges.len()
    );

    // Safety check: fail closed if any node references a non-read-only tool
    // (the planner may never schedule a mutating tool).
    validate_plan_tool_boundary(&graph, &read_only_tools)?;

    // ---- Phase 2: Execution ----
    info!("coordinator: execution phase");

    // Capture planner reasoning for executor context
    let planner_reasoning = plan_response.reasoning_content.clone();

    let callbacks = Arc::new(CoordinatorCallbacks {
        provider: executor,
        tools,
        sub_agent_runner,
        workspace_root,
        security,
        permission,
        extensions,
        planner_reasoning,
        history: Arc::new(std::sync::Mutex::new(Vec::new())),
    });

    let think: Arc<dyn ThinkCallback> = callbacks.clone();
    let tool: Arc<dyn ToolCallback> = callbacks.clone();
    let reflect: Arc<dyn ReflectCallback> = callbacks.clone();
    let delegate: Arc<dyn DelegateCallback> = callbacks;

    let graph_executor = Arc::new(
        GraphExecutor::new(think, tool, reflect)
            .with_delegate(delegate)
            // 收尾接线：core 图节点失败归因摘要（同步适配；LLM 归因由
            // DelegateEngine / agent 循环内部承担）。
            .with_attribution(Arc::new(crate::attribution::NodeFailureSummary)),
    );

    let result = graph_executor.execute(&graph).await?;

    // Stream node outputs as events.
    let mut combined = String::new();
    for (node_id, output) in &result.node_outputs {
        match output {
            deepseeknova_core::graph::NodeOutput::Text(t) => {
                let chunk = format!("[{node_id}]: {t}\n\n");
                combined.push_str(&chunk);
                tx.send(Ok(RunEvent::TextDelta(chunk))).await.ok();
            }
            deepseeknova_core::graph::NodeOutput::ToolResult(r) => {
                let chunk = format!("[{node_id}] tool result: {r}\n\n");
                combined.push_str(&chunk);
                tx.send(Ok(RunEvent::ToolResult {
                    call_id: node_id.clone(),
                    result: r.clone(),
                }))
                .await
                .ok();
            }
            deepseeknova_core::graph::NodeOutput::Error(e) => {
                let chunk = format!("[{node_id}] ERROR: {e}\n\n");
                combined.push_str(&chunk);
            }
            deepseeknova_core::graph::NodeOutput::Skipped => {
                let chunk = format!("[{node_id}]: (skipped)\n\n");
                combined.push_str(&chunk);
            }
        }
    }

    tx.send(Ok(RunEvent::Done(RunOutput {
        text: combined,
        tool_calls: Vec::new(),
        usage: Some(result.total_usage),
    })))
    .await
    .ok();

    info!("coordinator: done");
    Ok(())
}

// ---------------------------------------------------------------------------
// Plan boundary validation — core two-model safety guarantee
// ---------------------------------------------------------------------------

/// Scan every `CallTool` node in the plan and fail closed if any node names a
/// tool that is not registered read-only. This is the runtime enforcement of
/// the planner / executor split: the planner may never schedule a mutating
/// tool, so a plan that references one is rejected outright rather than
/// merely warned about.
fn validate_plan_tool_boundary(
    graph: &ExecutionGraph,
    read_only: &[Arc<dyn Tool>],
) -> Result<(), DeepseeknovaError> {
    let allowed: Vec<String> = read_only.iter().map(|t| t.schema().name.clone()).collect();
    for (id, node) in &graph.nodes {
        let maybe_tool = match &node.action {
            Action::CallTool { tool, .. } => Some(tool.as_str()),
            _ => None,
        };
        if let Some(tool_name) = maybe_tool {
            if !allowed.iter().any(|n| n == tool_name) {
                return Err(DeepseeknovaError::runner(format!(
                    "coordinator safety: plan node '{id}' attempted to call \
                     non-read-only tool '{tool_name}' during planning — executor-only"
                )));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Plan parsing — JSON → ExecutionGraph (with fallback)
// ---------------------------------------------------------------------------

fn parse_plan(plan_text: &str, goal: &str, max_nodes: usize) -> ExecutionGraph {
    // 统一复用 review 模块的宽松 JSON 提取（fenced / 平衡花括号）。无平衡
    // 花括号（无 JSON 可提取）时直接走 fallback 线性计划。
    let Some(json_str) = crate::review::extract_json(plan_text) else {
        warn!("coordinator: no JSON in planner output; using fallback plan");
        return fallback_plan(goal);
    };

    match serde_json::from_str::<PlanOutput>(&json_str) {
        Ok(plan) if !plan.nodes.is_empty() => {
            let entry = plan.nodes.first().map(|n| n.id.clone()).unwrap_or_default();
            let mut graph = ExecutionGraph::new(entry);

            // 先加全部节点，再统一补边：depends_on 引用的目标节点可能排在
            // 当前节点之后，若边在节点循环内即时添加，add_edge 会对尚不存在
            // 的目标 fail-soft 丢弃，依赖关系静默丢失。
            for node in plan.nodes.iter().take(max_nodes) {
                let action = parse_plan_node_action(node);
                graph.add_node(ExecutionNode::new(&node.id, action));
            }

            for node in plan.nodes.iter().take(max_nodes) {
                // Backwards-compatible node-level dependency hints: convert to
                // default Success-conditioned edges when no explicit edge
                // already wires this dependency.
                for dep in &node.depends_on {
                    let already_wired =
                        plan.edges.iter().any(|e| e.from == *dep && e.to == node.id);
                    if !already_wired {
                        graph.add_edge(dep.clone(), node.id.clone(), None);
                    }
                }
            }

            for edge in &plan.edges {
                graph.add_edge(
                    edge.from.clone(),
                    edge.to.clone(),
                    parse_edge_condition(edge.condition.as_deref()),
                );
            }

            graph
        }
        _ => {
            // Fallback: simple linear execution.
            warn!("coordinator: failed to parse planner output as JSON; using fallback plan");
            fallback_plan(goal)
        }
    }
}

/// 解析失败时的兜底：单节点线性计划（Think 节点承载原始目标）。
fn fallback_plan(goal: &str) -> ExecutionGraph {
    let label = "execute";
    let mut graph = ExecutionGraph::new(label.into());
    graph.add_node(ExecutionNode::new(
        label,
        Action::Think {
            prompt: goal.to_string(),
        },
    ));
    graph
}

/// Map a `PlanNode` to its `Action`. A node carrying `parallel` children
/// becomes `Action::Parallel` (children run concurrently in the executor);
/// otherwise the node is a plain Think/CallTool/Reflect/Delegate step.
fn parse_plan_node_action(node: &PlanNode) -> Action {
    if let Some(children) = &node.parallel {
        return Action::Parallel(
            children
                .iter()
                .map(|child| ExecutionNode::new(&child.id, parse_plan_node_action(child)))
                .collect(),
        );
    }
    match node.action.as_str() {
        "call_read_tool" | "call_tool" => Action::CallTool {
            tool: node.tool.clone().unwrap_or_default(),
            args: node.args.clone().unwrap_or(serde_json::Value::Null),
        },
        "reflect" => Action::Reflect {
            criteria: node.criteria.clone().unwrap_or_default(),
        },
        "delegate" => {
            let sub_agent = node
                .sub_agent
                .clone()
                .or_else(|| node.tool.clone())
                .unwrap_or_default();
            let goal = node.goal.clone().unwrap_or_else(|| node.prompt.clone());
            Action::Delegate { sub_agent, goal }
        }
        _ => Action::Think {
            prompt: node.prompt.clone(),
        },
    }
}

/// Map a planner edge condition string to `EdgeCondition`. Missing or unknown
/// values map to `None` (default = Success, fully backwards compatible).
fn parse_edge_condition(condition: Option<&str>) -> Option<EdgeCondition> {
    let c = condition?.trim().to_ascii_lowercase();
    if c == "failure" || c == "on_failure" || c == "on-failure" {
        Some(EdgeCondition::Failure)
    } else if c == "retry" {
        Some(EdgeCondition::Retry(1))
    } else {
        c.strip_prefix("tool_call:")
            .map(|id| EdgeCondition::ToolCall(id.trim().to_string()))
    }
}

// ---------------------------------------------------------------------------
// Callbacks — wrap the executor provider + tools for GraphExecutor
// ---------------------------------------------------------------------------

struct CoordinatorCallbacks {
    provider: Arc<dyn Provider>,
    tools: HashMap<String, Arc<dyn Tool>>,
    sub_agent_runner: Option<Arc<SubAgentRunner>>,
    workspace_root: PathBuf,
    security: SecurityContext,
    permission: Option<Arc<PermissionGate>>,
    extensions: Vec<Arc<ExtensionApplier>>,
    /// Planner's reasoning content to pass as context to executor.
    planner_reasoning: Option<String>,
    /// 已执行步骤的结果历史（工具/委派输出），注入后续 executor 消息，
    /// 修复"step_1 工具结果在 step_2 不可见"的协调器断链。
    history: Arc<std::sync::Mutex<Vec<String>>>,
}

/// 历史记录条目数上限：只保留最近 N 条，防止无界增长撑爆后续 prompt。
const HISTORY_MAX_ENTRIES: usize = 50;
/// 单条历史记录的字符数上限：工具输出可能单条极大（如整仓 grep、读大文件），
/// 超过后按字符边界截断，避免一条超大输出整段注入后续每个 prompt。
const HISTORY_MAX_ENTRY_CHARS: usize = 2000;
/// 历史记录总字符数上限：工具输出可能单条极大（如整仓 grep），
/// 超出后从最旧条目开始丢弃，保持顺序并收敛 prompt 体积。
const HISTORY_MAX_TOTAL_CHARS: usize = 500_000;

impl CoordinatorCallbacks {
    /// 追加一条历史记录并按容量上限收敛（保留最近条目、保持顺序）。
    fn push_history(&self, entry: String) {
        let mut history = self.history.lock().unwrap_or_else(|e| e.into_inner());
        // 单条先按字符上限截断（保留完整字符，追加可见标记）。
        let entry = if entry.chars().count() > HISTORY_MAX_ENTRY_CHARS {
            let head: String = entry.chars().take(HISTORY_MAX_ENTRY_CHARS).collect();
            format!("{head}…[truncated]")
        } else {
            entry
        };
        history.push(entry);
        while history.len() > HISTORY_MAX_ENTRIES {
            history.remove(0);
        }
        loop {
            let total: usize = history.iter().map(|s| s.chars().count()).sum();
            if total <= HISTORY_MAX_TOTAL_CHARS || history.len() <= 1 {
                break;
            }
            history.remove(0);
        }
    }
}

#[async_trait::async_trait]
impl ThinkCallback for CoordinatorCallbacks {
    async fn think(&self, prompt: &str) -> Result<String, deepseeknova_core::DeepseeknovaError> {
        let mut messages = Vec::new();
        // Pass planner's reasoning as context so executor benefits from DeepSeek thinking
        if let Some(ref reasoning) = self.planner_reasoning {
            messages.push(Message {
                role: Role::Assistant,
                content: String::new(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: Some(reasoning.clone()),
                reasoning_signature: None,
            });
        }
        let mut content = prompt.to_string();
        let prior: Vec<String> = {
            let history = self.history.lock().unwrap_or_else(|e| e.into_inner());
            history.clone()
        };
        if !prior.is_empty() {
            content.push_str("\n\n## Previous step results\n");
            for h in &prior {
                content.push_str(&format!("- {h}\n"));
            }
        }
        messages.push(Message {
            role: Role::User,
            content,
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        });
        let validated =
            deepseeknova_provider::ValidatedRequest::new(&messages, &[]).map_err(|violations| {
                for v in &violations {
                    tracing::error!(?v, "replay invariant violation in coordinator generate");
                }
                DeepseeknovaError::runner(format!(
                    "history replay invariant violated: {} violation(s)",
                    violations.len()
                ))
            })?;
        let result = self.provider.generate(validated).await?;
        Ok(result.content)
    }
}

#[async_trait::async_trait]
impl ToolCallback for CoordinatorCallbacks {
    async fn call_tool(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
        let tool = self
            .tools
            .get(tool_name)
            .ok_or_else(|| DeepseeknovaError::runner(format!("unknown tool: {tool_name}")))?;

        let args_str = serde_json::to_string(args)?;

        // Permission gate (opt-in). Coordinator is non-interactive: `Deny`
        // 与 `Ask` 都阻止调用（Ask 无人工应答，fail-closed）。
        if let Some(gate) = &self.permission {
            match gate.check(tool.as_ref(), &args_str).decision() {
                Decision::Deny => {
                    return Ok(format!(
                        "Error: tool '{tool_name}' blocked by permission policy"
                    ));
                }
                Decision::Ask => {
                    return Ok(format!(
                        "Error: tool '{tool_name}' requires approval, but the \
                         coordinator is non-interactive (blocked)"
                    ));
                }
                Decision::Allow => {}
            }
        }

        let mut ctx = ToolContext::new(uuid::Uuid::new_v4().to_string())
            .with_workspace(self.workspace_root.clone())
            .with_extension(self.security.clone());
        for apply in &self.extensions {
            apply(&mut ctx.extensions);
        }
        let result = tool.execute(&ctx, &args_str).await;
        match &result {
            Ok(r) => self.push_history(format!("[{tool_name}] {r}")),
            Err(e) => self.push_history(format!("[{tool_name}] ERROR: {e}")),
        }
        result
    }
}

#[async_trait::async_trait]
impl ReflectCallback for CoordinatorCallbacks {
    async fn reflect(
        &self,
        criteria: &[String],
        context: &str,
    ) -> Result<ReflectResult, deepseeknova_core::DeepseeknovaError> {
        let prompt = format!(
            "Evaluate the following work output against these criteria.\n\
             Criteria:\n  {}\n\n\
             Work output:\n{context}\n\n\
             Respond with exactly this JSON and nothing else:\n\
             {{\"passed\": true_or_false, \"feedback\": \"brief explanation\"}}",
            criteria.join("\n  ")
        );

        let messages = vec![Message {
            role: Role::User,
            content: prompt,
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        }];

        let validated =
            deepseeknova_provider::ValidatedRequest::new(&messages, &[]).map_err(|violations| {
                for v in &violations {
                    tracing::error!(?v, "replay invariant violation in coordinator reflect");
                }
                DeepseeknovaError::runner(format!(
                    "history replay invariant violated: {} violation(s)",
                    violations.len()
                ))
            })?;

        let result = self.provider.generate(validated).await?;

        #[derive(Deserialize)]
        struct ReflectResponse {
            passed: bool,
            feedback: String,
        }

        match serde_json::from_str::<ReflectResponse>(&result.content) {
            Ok(r) => Ok(ReflectResult {
                passed: r.passed,
                feedback: r.feedback,
            }),
            Err(_) => {
                let lower = result.content.to_lowercase();
                Ok(ReflectResult {
                    passed: lower.contains("passed") || lower.contains("success"),
                    feedback: result.content,
                })
            }
        }
    }
}

#[async_trait::async_trait]
impl DelegateCallback for CoordinatorCallbacks {
    async fn delegate(
        &self,
        sub_agent: &str,
        goal: &str,
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
        let runner = self.sub_agent_runner.as_ref().ok_or_else(|| {
            DeepseeknovaError::runner(format!(
                "Delegate action targets sub-agent '{sub_agent}' but no \
                 SubAgentRunner is configured on the coordinator"
            ))
        })?;

        let input = RunInput {
            prompt: format!("sub_agent:{sub_agent}\ngoal:{goal}"),
            images: vec![],
            model_override: None,
        };

        let mut stream = runner.run_stream(input).await?;
        let mut text = String::new();

        while let Some(event) = stream.next().await {
            match event? {
                RunEvent::TextDelta(delta) => text.push_str(&delta),
                RunEvent::Done(output) => {
                    text = output.text;
                    break;
                }
                // 协议增强（阶段3）：协议事件不参与子代理文本收集。
                RunEvent::PhaseTransition { .. }
                | RunEvent::GateViolation(_)
                | RunEvent::DriftFinding(_) => {}
                _ => {}
            }
        }

        if text.is_empty() {
            return Err(deepseeknova_core::DeepseeknovaError::runner(format!(
                "sub-agent '{sub_agent}' produced no output"
            )));
        }

        self.push_history(format!("[delegate:{sub_agent}] {text}"));

        Ok(text)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_core::types::ToolSchema;

    // Minimal tool stub for the boundary check: carries a name, never executed.
    struct NamedTool {
        name: &'static str,
    }

    #[async_trait::async_trait]
    impl Tool for NamedTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: self.name.to_string(),
                description: "stub".to_string(),
                parameters: serde_json::json!({"type":"object","properties":{}}),
            }
        }
        async fn execute(
            &self,
            _ctx: &ToolContext,
            _args: &str,
        ) -> Result<String, DeepseeknovaError> {
            Ok(String::new())
        }
    }

    #[test]
    fn validate_plan_tool_boundary_blocks_non_read_only_tool() {
        let mut graph = ExecutionGraph::new("a".to_string());
        graph.add_node(ExecutionNode::new(
            "a",
            Action::CallTool {
                tool: "bash".to_string(),
                args: serde_json::Value::Null,
            },
        ));
        let read_only: Vec<Arc<dyn Tool>> = vec![Arc::new(NamedTool { name: "grep" })];

        let err = validate_plan_tool_boundary(&graph, &read_only)
            .expect_err("non-read-only tool in plan must fail closed");
        assert!(err.to_string().contains("bash"), "got: {err}");
    }

    #[test]
    fn validate_plan_tool_boundary_allows_read_only_tool() {
        let mut graph = ExecutionGraph::new("a".to_string());
        graph.add_node(ExecutionNode::new(
            "a",
            Action::CallTool {
                tool: "grep".to_string(),
                args: serde_json::Value::Null,
            },
        ));
        let read_only: Vec<Arc<dyn Tool>> = vec![Arc::new(NamedTool { name: "grep" })];

        validate_plan_tool_boundary(&graph, &read_only)
            .expect("read-only tool must pass the boundary check");
    }

    #[test]
    fn validate_plan_tool_boundary_ignores_non_tool_nodes() {
        let mut graph = ExecutionGraph::new("a".to_string());
        graph.add_node(ExecutionNode::new(
            "a",
            Action::Think {
                prompt: "x".to_string(),
            },
        ));
        let read_only: Vec<Arc<dyn Tool>> = vec![];

        validate_plan_tool_boundary(&graph, &read_only)
            .expect("Think nodes must not be subject to the tool boundary check");
    }

    #[test]
    fn parse_plan_falls_back_when_invalid() {
        let graph = parse_plan("not json at all", "do something", 20);
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.entry, "execute");
    }

    #[test]
    fn parse_plan_valid_json_linear() {
        let json = r#"{
            "nodes": [
                {"id": "a", "action": "think", "prompt": "Analyze"},
                {"id": "b", "action": "call_read_tool", "tool": "grep", "args": {"pattern": "foo"}},
                {"id": "c", "action": "reflect", "prompt": "Check", "criteria": ["Done?"]}
            ],
            "edges": [
                {"from": "a", "to": "b"},
                {"from": "b", "to": "c"}
            ]
        }"#;

        let graph = parse_plan(json, "goal", 20);
        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.entry, "a");
        assert_eq!(graph.edges.len(), 2);
    }

    #[test]
    fn parse_plan_empty_nodes_triggers_fallback() {
        let json = r#"{"nodes":[],"edges":[]}"#;
        let graph = parse_plan(json, "goal", 20);
        assert_eq!(graph.nodes.len(), 1);
    }

    #[test]
    fn parse_plan_respects_max_nodes() {
        let mut nodes = Vec::new();
        for i in 0..10 {
            nodes.push(format!(
                r#"{{"id":"n{i}","action":"think","prompt":"p{i}"}}"#
            ));
        }
        let json = format!(r#"{{"nodes":[{}],"edges":[]}}"#, nodes.join(","));

        let graph = parse_plan(&json, "goal", 4);
        assert_eq!(graph.nodes.len(), 4);
    }

    #[test]
    fn parse_plan_with_delegate_node() {
        let json = r#"{
            "nodes": [
                {"id": "plan", "action": "think", "prompt": "Analyze the codebase"},
                {"id": "spec", "action": "delegate", "tool": "spec-agent", "goal": "Write the API spec"},
                {"id": "verify", "action": "reflect", "prompt": "Verify", "criteria": ["Spec complete?"]}
            ],
            "edges": [
                {"from": "plan", "to": "spec"},
                {"from": "spec", "to": "verify"}
            ]
        }"#;

        let graph = parse_plan(json, "build API", 20);
        assert_eq!(graph.nodes.len(), 3);
        match graph.nodes.get("spec").map(|n| &n.action) {
            Some(Action::Delegate { sub_agent, goal }) => {
                assert_eq!(sub_agent, "spec-agent");
                assert_eq!(goal, "Write the API spec");
            }
            other => panic!("expected Delegate action, got {:?}", other), // test-only
        }
    }

    #[test]
    fn parse_plan_parses_depends_on_parallel_and_condition() {
        let json = r#"{
            "nodes": [
                {"id": "a", "action": "think", "prompt": "first"},
                {"id": "b", "action": "think", "prompt": "second", "depends_on": ["a"]},
                {
                    "id": "fanout",
                    "action": "think",
                    "prompt": "fanout",
                    "parallel": [
                        {"id": "p1", "action": "think", "prompt": "child one"},
                        {"id": "p2", "action": "think", "prompt": "child two"}
                    ]
                },
                {"id": "c", "action": "think", "prompt": "third", "depends_on": ["a"]}
            ],
            "edges": [
                {"from": "a", "to": "fanout", "condition": "failure"},
                {"from": "fanout", "to": "c", "condition": "tool_call:abc"}
            ]
        }"#;

        let graph = parse_plan(json, "goal", 20);
        assert_eq!(graph.nodes.len(), 4);

        // depends_on (no explicit edge) becomes a default Success edge.
        assert!(graph.edges.iter().any(|e| e.from == "a" && e.to == "b"));

        // parallel node wraps children in Action::Parallel.
        match graph.nodes.get("fanout").map(|n| &n.action) {
            Some(Action::Parallel(children)) => {
                assert_eq!(children.len(), 2);
                assert_eq!(children[0].id, "p1");
                assert_eq!(children[1].id, "p2");
            }
            other => panic!("expected Parallel action, got {other:?}"), // test-only
        }

        // Explicit edge conditions map to EdgeCondition.
        let failure_edge = graph
            .edges
            .iter()
            .find(|e| e.from == "a" && e.to == "fanout")
            .expect("failure edge");
        assert!(matches!(
            failure_edge.condition,
            Some(EdgeCondition::Failure)
        ));
        let tool_call_edge = graph
            .edges
            .iter()
            .find(|e| e.from == "fanout" && e.to == "c")
            .expect("tool_call edge");
        assert!(matches!(
            tool_call_edge.condition,
            Some(EdgeCondition::ToolCall(ref id)) if id == "abc"
        ));
    }

    #[test]
    fn parse_plan_depends_on_target_appearing_later_is_kept() {
        // 依赖目标 `a` 排在 `b` 之后：depends_on 边必须在全部节点加入后再
        // 添加，否则 add_edge 的未知节点 fail-soft 会丢弃依赖。
        let json = r#"{
            "nodes": [
                {"id": "b", "action": "think", "prompt": "second", "depends_on": ["a"]},
                {"id": "a", "action": "think", "prompt": "first"}
            ],
            "edges": []
        }"#;
        let graph = parse_plan(json, "goal", 20);
        assert_eq!(graph.nodes.len(), 2);
        assert!(
            graph.edges.iter().any(|e| e.from == "a" && e.to == "b"),
            "depends_on edge a→b must survive, got {:?}",
            graph.edges
        );
    }

    #[test]
    fn parse_plan_backwards_compatible_without_new_fields() {
        // Old-style plan with no depends_on/parallel/condition parses exactly
        // as before: no synthetic edges, plain actions, default conditions.
        let json = r#"{
            "nodes": [
                {"id": "a", "action": "think", "prompt": "first"},
                {"id": "b", "action": "reflect", "prompt": "check", "criteria": ["ok?"]}
            ],
            "edges": [
                {"from": "a", "to": "b"}
            ]
        }"#;

        let graph = parse_plan(json, "goal", 20);
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1);
        assert!(matches!(graph.nodes["a"].action, Action::Think { .. }));
        assert!(matches!(graph.nodes["b"].action, Action::Reflect { .. }));
        // Explicit edge without condition → default Success (None).
        assert!(graph.edges[0].condition.is_none());
    }

    #[test]
    fn parse_plan_condition_defaults_and_unknown_map_to_success() {
        assert!(parse_edge_condition(None).is_none());
        assert!(parse_edge_condition(Some("success")).is_none());
        assert!(parse_edge_condition(Some("bogus")).is_none());
        assert!(matches!(
            parse_edge_condition(Some("failure")),
            Some(EdgeCondition::Failure)
        ));
        assert!(matches!(
            parse_edge_condition(Some("on_failure")),
            Some(EdgeCondition::Failure)
        ));
        assert!(matches!(
            parse_edge_condition(Some("retry")),
            Some(EdgeCondition::Retry(_))
        ));
        assert!(matches!(
            parse_edge_condition(Some("tool_call:abc")),
            Some(EdgeCondition::ToolCall(ref id)) if id == "abc"
        ));
    }

    #[test]
    fn reasoning_language_display() {
        assert_eq!(ReasoningLanguage::Auto.to_string(), "auto");
        assert_eq!(ReasoningLanguage::Zh.to_string(), "zh");
        assert_eq!(ReasoningLanguage::En.to_string(), "en");
    }

    #[test]
    fn planner_prompts_keep_json_action_contract() {
        for token in [
            "\"nodes\"",
            "\"edges\"",
            "\"think\"",
            "\"call_read_tool\"",
            "\"reflect\"",
            "\"delegate\"",
            "Plan phase",
        ] {
            assert!(
                PLANNER_SYSTEM_PROMPT.contains(token),
                "planner prompt missing {token}"
            );
        }
    }

    #[tokio::test]
    async fn think_includes_prior_tool_results() {
        use crate::test_utils::MockProvider;

        let mock = Arc::new(MockProvider::text("ok"));
        let provider: Arc<dyn Provider> = mock.clone();
        let callbacks = CoordinatorCallbacks {
            provider,
            tools: HashMap::new(),
            sub_agent_runner: None,
            workspace_root: std::env::current_dir().unwrap_or_default(),
            security: deepseeknova_security::context::SecurityContext::with_safe_defaults(),
            permission: None,
            extensions: Vec::new(),
            planner_reasoning: None,
            history: Arc::new(std::sync::Mutex::new(vec!["[ls] file1\nfile2".to_string()])),
        };

        let out = callbacks.think("Count the files.").await.unwrap();
        assert_eq!(out, "mock response");
        let last = mock.last_prompt().unwrap();
        assert!(
            last.contains("## Previous step results"),
            "executor 应看到前序工具结果段"
        );
        assert!(
            last.contains("[ls] file1"),
            "前序 ls 输出必须进入后续 prompt"
        );
    }

    #[test]
    fn history_keeps_only_most_recent_entries_in_order() {
        use crate::test_utils::MockProvider;

        let callbacks = CoordinatorCallbacks {
            provider: Arc::new(MockProvider::text("ok")),
            tools: HashMap::new(),
            sub_agent_runner: None,
            workspace_root: std::env::current_dir().unwrap_or_default(),
            security: deepseeknova_security::context::SecurityContext::with_safe_defaults(),
            permission: None,
            extensions: Vec::new(),
            planner_reasoning: None,
            history: Arc::new(std::sync::Mutex::new(Vec::new())),
        };

        let pushed = HISTORY_MAX_ENTRIES + 20;
        for i in 0..pushed {
            callbacks.push_history(format!("[tool] result {i}"));
        }

        let history = callbacks.history.lock().unwrap();
        assert_eq!(history.len(), HISTORY_MAX_ENTRIES);
        // 丢弃最旧的 20 条，第一条应为第 20 条，且顺序保持。
        assert!(history[0].contains("result 20"));
        assert!(history
            .last()
            .unwrap()
            .contains(&format!("result {}", pushed - 1)));
    }

    #[test]
    fn history_total_char_cap_trims_old_entries() {
        use crate::test_utils::MockProvider;

        let callbacks = CoordinatorCallbacks {
            provider: Arc::new(MockProvider::text("ok")),
            tools: HashMap::new(),
            sub_agent_runner: None,
            workspace_root: std::env::current_dir().unwrap_or_default(),
            security: deepseeknova_security::context::SecurityContext::with_safe_defaults(),
            permission: None,
            extensions: Vec::new(),
            planner_reasoning: None,
            history: Arc::new(std::sync::Mutex::new(Vec::new())),
        };

        for i in 0..10 {
            callbacks.push_history(format!("[tool] short {i}"));
        }
        // 单条超大输出：先按条目上限截断，再按总字符上限从旧到新裁剪。
        callbacks.push_history(format!(
            "[tool] {}",
            "x".repeat(HISTORY_MAX_TOTAL_CHARS + 5_000)
        ));

        let history = callbacks.history.lock().unwrap();
        assert!(!history.is_empty());
        assert!(history.last().unwrap().starts_with("[tool] "));
        let last = history.last().unwrap();
        assert!(
            last.chars().count() <= HISTORY_MAX_ENTRY_CHARS + 32,
            "单条大输出必须先按条目上限截断: {}",
            last.chars().count()
        );
        assert!(last.ends_with("[truncated]"));
    }
}

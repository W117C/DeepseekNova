use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::SubAgentRunner;
use dpronix_core::executor::{
    DelegateCallback, GraphExecutor, ReflectCallback, ReflectResult, ThinkCallback, ToolCallback,
};
use dpronix_core::graph::{Action, ExecutionGraph, ExecutionNode};
use dpronix_core::tool::ToolContext;
use dpronix_core::{Message, Role, RunEvent, RunEventStream, RunInput, RunOutput, Runner, Tool};
use dpronix_provider::Provider;
use dpronix_security::context::SecurityContext;
use serde::{Deserialize, Serialize};
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
}

#[derive(Debug, Deserialize)]
struct PlanEdge {
    from: String,
    to: String,
}

// ---------------------------------------------------------------------------
// Goal Contract — forces planner to reason about what before how
// ---------------------------------------------------------------------------

/// A Goal Contract forces the planner to reason about the what and why
/// before the how. Mirrors the Context / Request / Output / Constraints /
/// Pause structure used by DeepSeek-DPronix's TASK_CONTRACT.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalContract {
    /// Background the planner needs (project shape, prior decisions, etc.)
    #[serde(default)]
    pub context: String,
    /// The concrete deliverable: what should exist / be true when done.
    pub request: String,
    /// The exact shape of the expected output (file path, format, content).
    #[serde(default)]
    pub expected_output: String,
    /// Hard constraints (non-functional requirements, limits, things to avoid).
    #[serde(default)]
    pub constraints: Vec<String>,
    /// When the agent should stop and ask for human input.
    #[serde(default)]
    pub pause_when: Vec<String>,
}

impl GoalContract {
    /// Render to a compact, cache-stable string injected as the planner's
    /// user message. The format is fixed — do not add dynamic per-turn
    /// fields above the `---` divider or prefix-cache hits will regress.
    pub fn to_planner_prompt(&self) -> String {
        let mut s = String::new();
        s.push_str("# GOAL CONTRACT\n\n");
        if !self.context.is_empty() {
            s.push_str("## Context\n");
            s.push_str(&self.context);
            s.push_str("\n\n");
        }
        s.push_str("## Request\n");
        s.push_str(&self.request);
        s.push_str("\n\n");
        if !self.expected_output.is_empty() {
            s.push_str("## Expected Output\n");
            s.push_str(&self.expected_output);
            s.push_str("\n\n");
        }
        if !self.constraints.is_empty() {
            s.push_str("## Constraints\n");
            for c in &self.constraints {
                s.push_str(&format!("- {c}\n"));
            }
            s.push('\n');
        }
        if !self.pause_when.is_empty() {
            s.push_str("## Pause When\n");
            for p in &self.pause_when {
                s.push_str(&format!("- {p}\n"));
            }
            s.push('\n');
        }
        s.push_str("---\n");
        s.push_str("Produce an execution plan as JSON. Each step MUST be one of:\n");
        s.push_str("- `think` for reasoning (no side-effects)\n");
        s.push_str("- `call_read_tool` for read-only tool calls\n");
        s.push_str("- `delegate` for sub-agent dispatch\n");
        s.push_str(
            "NEVER use `call_tool` in the plan — only the executor may call mutating tools.\n",
        );
        s
    }
}

// ---------------------------------------------------------------------------
// Reasoning language control
// ---------------------------------------------------------------------------

/// Language hint for model reasoning output. Injected in message metadata,
/// never in the text stream — cache-neutral by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasoningLanguage {
    #[default]
    Auto,
    Zh,
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

/// Standard (non-Goal-Mode) system prompt. Fixed byte-for-byte across turns
/// so the provider's prefix cache stays warm.
const PLANNER_SYSTEM_PROMPT: &str = r#"You are a planning assistant. Your job is to break down a user's goal into a structured execution plan.

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
- Edges define the execution order (from → to)
- Keep plans concise: 3–8 nodes typically
- Output ONLY the JSON object. No markdown, no explanation, no backticks."#;

/// Goal-Mode system prompt. Fixed byte-for-byte across turns.
const PLANNER_SYSTEM_PROMPT_GOAL: &str = r#"You are a planning assistant operating in Goal Mode. Your job is to analyze a structured Goal Contract (Context / Request / Output / Constraints / Pause) and produce an execution plan that satisfies it.

CRITICAL: You may ONLY use these action types:
- "think" — pure reasoning (no side effects)
- "call_read_tool" — invoke a READ-ONLY tool to gather information
- "delegate" — dispatch to a named sub-agent

You MAY NEVER use "call_tool" — only the executor phase may invoke mutating tools.

Output ONLY valid JSON with this exact structure:
{
  "nodes": [
    {"id": "understand", "action": "think", "prompt": "Confirm understanding of the Goal Contract"},
    {"id": "gather", "action": "call_read_tool", "tool": "<read_tool>", "args": {...}},
    {"id": "synthesize", "action": "think", "prompt": "Synthesize findings into a concrete deliverable"},
    {"id": "verify", "action": "reflect", "prompt": "Verify the deliverable against all Goal Contract criteria", "criteria": ["Matches expected output?", "All constraints satisfied?", "Within scope?"]}
  ],
  "edges": [
    {"from": "understand", "to": "gather"},
    {"from": "gather", "to": "synthesize"},
    {"from": "synthesize", "to": "verify"}
  ]
}

Rules:
- "id" must be unique for every node
- Valid actions: "think", "call_read_tool" (read-only only), "reflect", "delegate"
- The Goal Contract's Constraints and Pause When must appear in your plan's reflect criteria
- Edges define the execution order (from → to)
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
        },
        Message {
            role: Role::User,
            content: format!("Create an execution plan for this goal:\n\n{goal}"),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        },
    ]
}

fn build_goal_planning_prompt(
    contract: &GoalContract,
    read_only_tools: &[&dyn Tool],
) -> Vec<Message> {
    let mut extra = String::new();
    if !read_only_tools.is_empty() {
        extra.push_str("\n\nRead-only tools available for call_read_tool nodes:\n");
        for t in read_only_tools {
            extra.push_str(&format!(
                "- {}: {}\n",
                t.schema().name,
                t.schema().description
            ));
        }
    }

    let mut system = PLANNER_SYSTEM_PROMPT_GOAL.to_string();
    system.push_str(&extra);

    vec![
        Message {
            role: Role::System,
            content: system,
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        },
        Message {
            role: Role::User,
            content: contract.to_planner_prompt(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        },
    ]
}

// ---------------------------------------------------------------------------
// CoordinatorRunner — two-model (Planner + Executor)
// ---------------------------------------------------------------------------

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
    /// When true, receive a `GoalContract` and use the Goal-Mode prompt.
    goal_mode: bool,
    /// Language hint for model reasoning output (cache-neutral).
    reasoning_language: ReasoningLanguage,
    /// When true (default), planner system prompt is pinned byte-for-byte
    /// across turns so prefix cache stays warm.
    cache_stable_prefix: bool,
    /// Workspace root used to confine filesystem tool calls in executor nodes.
    workspace_root: PathBuf,
    /// Security context injected into every executor tool execution.
    security: SecurityContext,
}

impl CoordinatorRunner {
    pub fn new(planner_provider: Arc<dyn Provider>, executor_provider: Arc<dyn Provider>) -> Self {
        Self {
            planner_provider,
            executor_provider,
            tools: HashMap::new(),
            read_only_tools: HashMap::new(),
            max_graph_nodes: 20,
            sub_agent_runner: None,
            goal_mode: false,
            reasoning_language: ReasoningLanguage::Auto,
            cache_stable_prefix: true,
            workspace_root: std::env::current_dir().unwrap_or_default(),
            security: SecurityContext::with_safe_defaults(),
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

    /// Enable Goal Mode: the planner receives a Goal Contract instead of a
    /// free-form prompt. Forces reasoning about success criteria before
    /// generating any nodes.
    pub fn with_goal_mode(mut self, enabled: bool) -> Self {
        self.goal_mode = enabled;
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
}

#[async_trait::async_trait]
impl Runner for CoordinatorRunner {
    async fn run_stream(&self, input: RunInput) -> anyhow::Result<RunEventStream> {
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
        let goal_mode = self.goal_mode;
        let workspace_root = self.workspace_root.clone();
        let security = self.security.clone();

        tokio::spawn(async move {
            if let Err(e) = run_coordinator(
                planner,
                executor,
                tools,
                read_only_refs,
                max_nodes,
                sub_agent_runner,
                goal_mode,
                workspace_root,
                security,
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
    goal_mode: bool,
    workspace_root: PathBuf,
    security: SecurityContext,
    input: RunInput,
    tx: &mpsc::Sender<anyhow::Result<RunEvent>>,
) -> anyhow::Result<()> {
    // ---- Phase 1: Planning ----
    info!("coordinator: planning phase (goal_mode={goal_mode})");

    // Build prompt depending on mode.
    let read_only_views: Vec<&dyn Tool> = read_only_tools.iter().map(|t| t.as_ref()).collect();

    let plan_messages = if goal_mode {
        // In goal mode, the prompt field is JSON-encoded GoalContract.
        let contract: GoalContract = match serde_json::from_str::<GoalContract>(&input.prompt) {
            Ok(c) => c,
            Err(_) => {
                // Fallback: wrap the raw prompt as the Request field.
                GoalContract {
                    context: String::new(),
                    request: input.prompt.clone(),
                    expected_output: String::new(),
                    constraints: Vec::new(),
                    pause_when: Vec::new(),
                }
            }
        };
        build_goal_planning_prompt(&contract, &read_only_views)
    } else {
        build_planning_prompt(&input.prompt, &read_only_views)
    };

    let validated =
        dpronix_provider::ValidatedRequest::new(&plan_messages, &[]).map_err(|violations| {
            anyhow::anyhow!(
                "planning prompt replay invariant violated: {} violation(s) detected",
                violations.len()
            )
        })?;
    let plan_response = planner.generate(validated).await?;

    tx.send(Ok(RunEvent::TextDelta(format!(
        "[PLAN]\n{}\n",
        plan_response.content
    ))))
    .await
    .ok();

    // Parse the planner's JSON output.
    let graph = parse_plan(&plan_response.content, &input.prompt, max_nodes, goal_mode);
    info!(
        "coordinator: plan parsed — {} nodes, {} edges",
        graph.nodes.len(),
        graph.edges.len()
    );

    // Safety check: no node may use a non-read-only tool via call_read_tool.
    validate_plan_tool_boundary(&graph, &read_only_tools);

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
        planner_reasoning,
    });

    let think: Arc<dyn ThinkCallback> = callbacks.clone();
    let tool: Arc<dyn ToolCallback> = callbacks.clone();
    let reflect: Arc<dyn ReflectCallback> = callbacks.clone();
    let delegate: Arc<dyn DelegateCallback> = callbacks;

    let graph_executor = Arc::new(GraphExecutor::new(think, tool, reflect).with_delegate(delegate));

    let result = graph_executor.execute(&graph).await?;

    // Stream node outputs as events.
    let mut combined = String::new();
    for (node_id, output) in &result.node_outputs {
        match output {
            dpronix_core::graph::NodeOutput::Text(t) => {
                let chunk = format!("[{node_id}]: {t}\n\n");
                combined.push_str(&chunk);
                tx.send(Ok(RunEvent::TextDelta(chunk))).await.ok();
            }
            dpronix_core::graph::NodeOutput::ToolResult(r) => {
                let chunk = format!("[{node_id}] tool result: {r}\n\n");
                combined.push_str(&chunk);
                tx.send(Ok(RunEvent::ToolResult {
                    call_id: node_id.clone(),
                    result: r.clone(),
                }))
                .await
                .ok();
            }
            dpronix_core::graph::NodeOutput::Error(e) => {
                let chunk = format!("[{node_id}] ERROR: {e}\n\n");
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

/// Scan every `call_read_tool`-action node in the plan and assert the named
/// tool is registered as read-only. This is the runtime enforcement of the
/// planner / executor split.
fn validate_plan_tool_boundary(graph: &ExecutionGraph, read_only: &[Arc<dyn Tool>]) {
    let allowed: Vec<String> = read_only.iter().map(|t| t.schema().name.clone()).collect();
    for (id, node) in &graph.nodes {
        let maybe_tool = match &node.action {
            Action::CallTool { tool, .. } => Some(tool.as_str()),
            _ => None,
        };
        if let Some(tool_name) = maybe_tool {
            if !allowed.iter().any(|n| n == tool_name) {
                warn!(
                    "coordinator safety: plan node '{}' attempted to call \
                     non-read-only tool '{}' during planning — executor-only",
                    id, tool_name
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Plan parsing — JSON → ExecutionGraph (with fallback)
// ---------------------------------------------------------------------------

fn parse_plan(plan_text: &str, goal: &str, max_nodes: usize, goal_mode: bool) -> ExecutionGraph {
    let json_str = extract_json_block(plan_text);

    match serde_json::from_str::<PlanOutput>(&json_str) {
        Ok(plan) if !plan.nodes.is_empty() => {
            let entry = plan.nodes.first().map(|n| n.id.clone()).unwrap_or_default();
            let mut graph = ExecutionGraph::new(entry);

            for node in plan.nodes.iter().take(max_nodes) {
                let action = match node.action.as_str() {
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
                };
                graph.add_node(ExecutionNode::new(&node.id, action));
            }

            for edge in &plan.edges {
                graph.add_edge(edge.from.clone(), edge.to.clone(), None);
            }

            graph
        }
        _ => {
            // Fallback: simple linear execution.
            warn!("coordinator: failed to parse planner output as JSON; using fallback plan");
            let label = if goal_mode { "satisfy" } else { "execute" };
            let mut graph = ExecutionGraph::new(label.into());
            graph.add_node(ExecutionNode::new(
                label,
                Action::Think {
                    prompt: goal.to_string(),
                },
            ));
            graph
        }
    }
}

/// Extract JSON from a model response that may contain markdown fences or
/// surrounding commentary.
fn extract_json_block(text: &str) -> String {
    // ```json ... ```
    if let Some(start) = text.find("```json") {
        let after = &text[start + 7..];
        if let Some(end) = after.find("```") {
            let inner = after[..end].trim();
            if !inner.is_empty() {
                return inner.to_string();
            }
        }
    }
    // ``` ... ```
    if let Some(start) = text.find("```") {
        let after = &text[start + 3..];
        if let Some(end) = after.find("```") {
            let inner = after[..end].trim();
            if !inner.is_empty() {
                return inner.to_string();
            }
        }
    }
    // Raw { ... }
    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) {
        if start < end {
            return text[start..=end].to_string();
        }
    }
    text.to_string()
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
    /// Planner's reasoning content to pass as context to executor.
    planner_reasoning: Option<String>,
}

#[async_trait::async_trait]
impl ThinkCallback for CoordinatorCallbacks {
    async fn think(&self, prompt: &str) -> anyhow::Result<String> {
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
            });
        }
        messages.push(Message {
            role: Role::User,
            content: prompt.to_string(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });
        let validated =
            dpronix_provider::ValidatedRequest::new(&messages, &[]).map_err(|violations| {
                for v in &violations {
                    tracing::error!(?v, "replay invariant violation in coordinator generate");
                }
                anyhow::anyhow!(
                    "history replay invariant violated: {} violation(s)",
                    violations.len()
                )
            })?;
        let result = self.provider.generate(validated).await?;
        Ok(result.content)
    }
}

#[async_trait::async_trait]
impl ToolCallback for CoordinatorCallbacks {
    async fn call_tool(&self, tool_name: &str, args: &serde_json::Value) -> anyhow::Result<String> {
        let tool = self
            .tools
            .get(tool_name)
            .ok_or_else(|| anyhow::anyhow!("unknown tool: {tool_name}"))?;

        let ctx = ToolContext::new(uuid::Uuid::new_v4().to_string())
            .with_workspace(self.workspace_root.clone())
            .with_extension(self.security.clone());
        let args_str = serde_json::to_string(args)?;
        tool.execute(&ctx, &args_str).await
    }
}

#[async_trait::async_trait]
impl ReflectCallback for CoordinatorCallbacks {
    async fn reflect(&self, criteria: &[String], context: &str) -> anyhow::Result<ReflectResult> {
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
        }];

        let validated =
            dpronix_provider::ValidatedRequest::new(&messages, &[]).map_err(|violations| {
                for v in &violations {
                    tracing::error!(?v, "replay invariant violation in coordinator reflect");
                }
                anyhow::anyhow!(
                    "history replay invariant violated: {} violation(s)",
                    violations.len()
                )
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
    async fn delegate(&self, sub_agent: &str, goal: &str) -> anyhow::Result<String> {
        let runner = self.sub_agent_runner.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Delegate action targets sub-agent '{sub_agent}' but no \
                 SubAgentRunner is configured on the coordinator"
            )
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
                _ => {}
            }
        }

        if text.is_empty() {
            anyhow::bail!("sub-agent '{sub_agent}' produced no output");
        }

        Ok(text)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_from_markdown_fence() {
        let input = "Here's the plan:\n```json\n{\"nodes\":[],\"edges\":[]}\n```\nDone.";
        let json = extract_json_block(input);
        assert_eq!(json, "{\"nodes\":[],\"edges\":[]}");
    }

    #[test]
    fn extract_json_from_plain_fence() {
        let input = "```\n{\"nodes\":[{\"id\":\"x\"}]}\n```";
        let json = extract_json_block(input);
        assert!(json.contains("\"nodes\""));
    }

    #[test]
    fn extract_json_raw() {
        let input = " some text {\"key\": \"value\"} trailing ";
        let json = extract_json_block(input);
        assert_eq!(json, "{\"key\": \"value\"}");
    }

    #[test]
    fn parse_plan_falls_back_when_invalid() {
        let graph = parse_plan("not json at all", "do something", 20, false);
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.entry, "execute");
    }

    #[test]
    fn parse_plan_falls_back_goal_mode() {
        let graph = parse_plan("not json at all", "satisfy goal", 20, true);
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.entry, "satisfy");
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

        let graph = parse_plan(json, "goal", 20, false);
        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.entry, "a");
        assert_eq!(graph.edges.len(), 2);
    }

    #[test]
    fn parse_plan_empty_nodes_triggers_fallback() {
        let json = r#"{"nodes":[],"edges":[]}"#;
        let graph = parse_plan(json, "goal", 20, false);
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

        let graph = parse_plan(&json, "goal", 4, false);
        assert_eq!(graph.nodes.len(), 4);
    }

    #[test]
    fn goal_contract_renders_structured_prompt() {
        let contract = GoalContract {
            context: "project uses Rust".into(),
            request: "add a new endpoint".into(),
            expected_output: "file: src/handler.rs".into(),
            constraints: vec!["no async".into()],
            pause_when: vec!["database schema changes".into()],
        };
        let prompt = contract.to_planner_prompt();
        assert!(prompt.contains("GOAL CONTRACT"));
        assert!(prompt.contains("add a new endpoint"));
        assert!(prompt.contains("no async"));
        assert!(prompt.contains("database schema changes"));
        assert!(prompt.contains("NEVER use `call_tool`"));
    }

    #[test]
    fn goal_contract_serializes_to_json() {
        let contract = GoalContract {
            context: "ctx".into(),
            request: "req".into(),
            expected_output: "out".into(),
            constraints: vec!["c1".into()],
            pause_when: vec!["p1".into()],
        };
        let json = serde_json::to_string(&contract).unwrap();
        assert!(json.contains("\"request\":\"req\""));
        assert!(json.contains("\"context\":\"ctx\""));
        let parsed: GoalContract = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.request, "req");
        assert_eq!(parsed.constraints, vec!["c1"]);
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

        let graph = parse_plan(json, "build API", 20, false);
        assert_eq!(graph.nodes.len(), 3);
        match graph.nodes.get("spec").map(|n| &n.action) {
            Some(Action::Delegate { sub_agent, goal }) => {
                assert_eq!(sub_agent, "spec-agent");
                assert_eq!(goal, "Write the API spec");
            }
            other => panic!("expected Delegate action, got {:?}", other),
        }
    }

    #[test]
    fn reasoning_language_display() {
        assert_eq!(ReasoningLanguage::Auto.to_string(), "auto");
        assert_eq!(ReasoningLanguage::Zh.to_string(), "zh");
        assert_eq!(ReasoningLanguage::En.to_string(), "en");
    }
}

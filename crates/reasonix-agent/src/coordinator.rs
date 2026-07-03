use std::collections::HashMap;
use std::sync::Arc;

use crate::SubAgentRunner;
use reasonix_core::executor::{
    DelegateCallback, GraphExecutor, ReflectCallback, ReflectResult, ThinkCallback, ToolCallback,
};
use reasonix_core::graph::{Action, ExecutionGraph, ExecutionNode};
use reasonix_core::tool::ToolContext;
use reasonix_core::{Message, Role, RunEvent, RunEventStream, RunInput, RunOutput, Runner, Tool};
use reasonix_provider::Provider;
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
    prompt: String,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    args: Option<serde_json::Value>,
    #[serde(default)]
    criteria: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct PlanEdge {
    from: String,
    to: String,
}

/// Prompt injected as the system message for the planner model.
const PLANNER_SYSTEM_PROMPT: &str = r#"You are a planning assistant. Your job is to break down a user's goal into a structured execution plan.

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
- Valid actions: "think" (call the model), "call_tool" (invoke a named tool), "reflect" (evaluate results against criteria)
- "think" nodes: describe the task in "prompt"
- "call_tool" nodes: include "tool" (name) and "args" (JSON object). List args as the actual JSON the tool expects.
- "reflect" nodes: include "criteria" (array of strings — evaluation questions)
- Edges define the execution order (from → to)
- Keep plans concise: 3–8 nodes typically
- Output ONLY the JSON object. No markdown, no explanation, no backticks."#;

/// Prompt sent to the planner model alongside the user's goal.
fn build_planning_prompt(goal: &str) -> Vec<Message> {
    vec![
        Message {
            role: Role::System,
            content: PLANNER_SYSTEM_PROMPT.to_string(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        },
        Message {
            role: Role::User,
            content: format!("Create an execution plan for this goal:\n\n{goal}"),
            name: None,
            tool_calls: None,
            tool_call_id: None,
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
    /// Cap on the number of plan nodes (safety valve against runaway plans).
    max_graph_nodes: usize,
    /// Optional sub-agent runner for handling Delegate actions.
    sub_agent_runner: Option<Arc<SubAgentRunner>>,
}

impl CoordinatorRunner {
    pub fn new(planner_provider: Arc<dyn Provider>, executor_provider: Arc<dyn Provider>) -> Self {
        Self {
            planner_provider,
            executor_provider,
            tools: HashMap::new(),
            max_graph_nodes: 20,
            sub_agent_runner: None,
        }
    }

    /// Register a tool that executor nodes may call.
    pub fn register_tool(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.schema().name.clone();
        self.tools.insert(name, tool);
    }

    /// Limit the number of plan nodes accepted from the planner.
    pub fn with_max_graph_nodes(mut self, n: usize) -> Self {
        self.max_graph_nodes = n;
        self
    }

    /// Attach a sub-agent runner for handling `Action::Delegate` nodes
    /// that the planner may generate.
    pub fn with_sub_agent_runner(mut self, runner: SubAgentRunner) -> Self {
        self.sub_agent_runner = Some(Arc::new(runner));
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
        let max_nodes = self.max_graph_nodes;
        let sub_agent_runner = self.sub_agent_runner.clone();

        tokio::spawn(async move {
            if let Err(e) = run_coordinator(
                planner,
                executor,
                tools,
                max_nodes,
                sub_agent_runner,
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
    max_nodes: usize,
    sub_agent_runner: Option<Arc<SubAgentRunner>>,
    input: RunInput,
    tx: &mpsc::Sender<anyhow::Result<RunEvent>>,
) -> anyhow::Result<()> {
    // ---- Phase 1: Planning ----
    info!("coordinator: planning phase");

    let plan_response = planner
        .generate(&build_planning_prompt(&input.prompt), &[])
        .await?;

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

    // ---- Phase 2: Execution ----
    info!("coordinator: execution phase");

    let callbacks = Arc::new(CoordinatorCallbacks {
        provider: executor,
        tools,
        sub_agent_runner,
    });

    let think: Arc<dyn ThinkCallback> = callbacks.clone();
    let tool: Arc<dyn ToolCallback> = callbacks.clone();
    let reflect: Arc<dyn ReflectCallback> = callbacks.clone();
    let delegate: Arc<dyn DelegateCallback> = callbacks;

    let graph_executor = Arc::new(
        GraphExecutor::new(think, tool, reflect).with_delegate(delegate),
    );

    let result = graph_executor.execute(&graph).await?;

    // Stream node outputs as events.
    let mut combined = String::new();
    for (node_id, output) in &result.node_outputs {
        match output {
            reasonix_core::graph::NodeOutput::Text(t) => {
                let chunk = format!("[{node_id}]: {t}\n\n");
                combined.push_str(&chunk);
                tx.send(Ok(RunEvent::TextDelta(chunk))).await.ok();
            }
            reasonix_core::graph::NodeOutput::ToolResult(r) => {
                let chunk = format!("[{node_id}] tool result: {r}\n\n");
                combined.push_str(&chunk);
                tx.send(Ok(RunEvent::ToolResult {
                    call_id: node_id.clone(),
                    result: r.clone(),
                }))
                .await
                .ok();
            }
            reasonix_core::graph::NodeOutput::Error(e) => {
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
// Plan parsing — JSON → ExecutionGraph (with fallback)
// ---------------------------------------------------------------------------

fn parse_plan(plan_text: &str, goal: &str, max_nodes: usize) -> ExecutionGraph {
    let json_str = extract_json_block(plan_text);

    match serde_json::from_str::<PlanOutput>(&json_str) {
        Ok(plan) if !plan.nodes.is_empty() => {
            let entry = plan.nodes.first().map(|n| n.id.clone()).unwrap_or_default();
            let mut graph = ExecutionGraph::new(entry);

            for node in plan.nodes.iter().take(max_nodes) {
                let action = match node.action.as_str() {
                    "call_tool" => Action::CallTool {
                        tool: node.tool.clone().unwrap_or_default(),
                        args: node.args.clone().unwrap_or(serde_json::Value::Null),
                    },
                    "reflect" => Action::Reflect {
                        criteria: node.criteria.clone().unwrap_or_default(),
                    },
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
            // Fallback: simple linear execution as a single think node.
            warn!("coordinator: failed to parse planner output as JSON; using fallback plan");
            let mut graph = ExecutionGraph::new("execute".into());
            graph.add_node(ExecutionNode::new(
                "execute",
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
}

#[async_trait::async_trait]
impl ThinkCallback for CoordinatorCallbacks {
    async fn think(&self, prompt: &str) -> anyhow::Result<String> {
        let messages = vec![Message {
            role: Role::User,
            content: prompt.to_string(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }];
        let result = self.provider.generate(&messages, &[]).await?;
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

        let ctx = ToolContext::new(uuid::Uuid::new_v4().to_string());
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
        }];

        let result = self.provider.generate(&messages, &[]).await?;

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
                // Heuristic fallback: treat "passed" or "success" as passing.
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
        let graph = parse_plan("not json at all", "do something", 20);
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.entry, "execute");
    }

    #[test]
    fn parse_plan_valid_json_linear() {
        let json = r#"{
            "nodes": [
                {"id": "a", "action": "think", "prompt": "Analyze"},
                {"id": "b", "action": "think", "prompt": "Execute"},
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
}

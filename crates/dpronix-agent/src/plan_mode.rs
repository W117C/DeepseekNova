use dpronix_core::registry::Planner;
use dpronix_core::{
    graph::ExecutionGraph, Message, Role, RunEvent, RunEventStream, RunInput, RunOutput, Runner,
};
use dpronix_provider::Provider;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::info;

// ---------------------------------------------------------------------------
// PlanModeRunner — read-only planning, user approval
// ---------------------------------------------------------------------------

/// PlanModeRunner analyzes a goal via an LLM and produces a structured plan.
/// It **never** executes tools — it only plans. The plan is streamed back as
/// [`dpronix_core::RunEvent::TextDelta`] events and the final plan text is returned in
/// [`dpronix_core::RunOutput`].
///
/// If a [`dpronix_core::registry::Planner`] is configured, an [`dpronix_core::graph::ExecutionGraph`] is also generated and
/// appended to the plan output.
pub struct PlanModeRunner {
    provider: Arc<dyn Provider>,
    planner: Option<Arc<dyn Planner>>,
    system_prompt: Option<String>,
}

impl PlanModeRunner {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self {
            provider,
            planner: None,
            system_prompt: None,
        }
    }

    /// Attach a planner that produces an [`dpronix_core::graph::ExecutionGraph`] for the goal.
    pub fn with_planner(mut self, planner: Arc<dyn Planner>) -> Self {
        self.planner = Some(planner);
        self
    }

    /// Override the default planning system prompt.
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }
}

#[async_trait::async_trait]
impl Runner for PlanModeRunner {
    async fn run_stream(&self, input: RunInput) -> anyhow::Result<RunEventStream> {
        let (tx, rx) = mpsc::channel(64);

        let provider = Arc::clone(&self.provider);
        let planner = self.planner.clone();
        let system_prompt = self.system_prompt.clone();
        let goal = input.prompt;

        tokio::spawn(async move {
            if let Err(e) = run_plan_mode(provider, planner, goal, system_prompt, &tx).await {
                let _ = tx.send(Err(e)).await;
            }
        });

        Ok(Box::pin(ReceiverStream::new(rx)))
    }
}

// ---------------------------------------------------------------------------
// Core planning loop
// ---------------------------------------------------------------------------

const DEFAULT_PLANNING_SYSTEM_PROMPT: &str = "\
You are a planning assistant. Your role is to analyze goals and produce \
structured, actionable plans. You do NOT execute anything — you only plan.

For each goal, produce a plan with these sections:
1. **Goal Understanding**: Restate the goal in your own words to confirm understanding.
2. **Task Breakdown**: List concrete steps in dependency order.
3. **Tools Required**: Identify which tools each step needs.
4. **Dependencies**: Note which steps depend on others.
5. **Risks & Edge Cases**: Identify potential pitfalls.

Be thorough but concise. Focus on actionable steps.";

async fn run_plan_mode(
    provider: Arc<dyn Provider>,
    planner: Option<Arc<dyn Planner>>,
    goal: String,
    system_prompt: Option<String>,
    tx: &mpsc::Sender<anyhow::Result<RunEvent>>,
) -> anyhow::Result<()> {
    let planning_system =
        system_prompt.unwrap_or_else(|| DEFAULT_PLANNING_SYSTEM_PROMPT.to_string());

    let messages = vec![
        Message {
            role: Role::System,
            content: planning_system,
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        },
        Message {
            role: Role::User,
            content: format!("Plan the following goal:\n\n---\n{goal}\n---"),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        },
    ];

    // Stream from provider (read-only: NO tools)
    let mut stream = provider.stream(&messages, &[]).await?;
    use tokio_stream::StreamExt;

    let mut plan_text = String::new();

    while let Some(chunk) = stream.next().await {
        match chunk? {
            dpronix_core::chunk::Chunk::TextDelta(delta) => {
                plan_text.push_str(&delta);
                tx.send(Ok(RunEvent::TextDelta(delta))).await.ok();
            }
            dpronix_core::chunk::Chunk::ReasoningDelta { text, signature } => {
                tx.send(Ok(RunEvent::ReasoningDelta { text, signature }))
                    .await
                    .ok();
            }
            dpronix_core::chunk::Chunk::Usage(u) => {
                tx.send(Ok(RunEvent::Usage(u))).await.ok();
            }
            dpronix_core::chunk::Chunk::Done => {}
            // Ignore tool-call chunks in plan mode — we send no tools
            _ => {}
        }
    }

    // Optionally append an ExecutionGraph from the Planner
    if let Some(ref p) = planner {
        match p.plan(&goal).await {
            Ok(graph) => {
                let graph_display = format_execution_graph(&graph);
                let preamble = "\n\n---\n## Execution Graph\n\n";
                plan_text.push_str(preamble);
                plan_text.push_str(&graph_display);
                tx.send(Ok(RunEvent::TextDelta(preamble.to_string())))
                    .await
                    .ok();
                // Stream graph lines as deltas for the caller to display
                for line in graph_display.lines() {
                    let delta = format!("{line}\n");
                    tx.send(Ok(RunEvent::TextDelta(delta))).await.ok();
                }
            }
            Err(e) => {
                let err_msg =
                    format!("\n\n---\n## Execution Graph\n\n⚠ Could not generate graph: {e}\n");
                plan_text.push_str(&err_msg);
                tx.send(Ok(RunEvent::TextDelta(err_msg))).await.ok();
            }
        }
    }

    info!(plan_chars = plan_text.len(), "plan mode complete");

    let output = RunOutput {
        text: plan_text,
        tool_calls: Vec::new(),
        usage: None,
    };
    tx.send(Ok(RunEvent::Done(output))).await.ok();

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Render an [`dpronix_core::graph::ExecutionGraph`] as a human-readable Markdown outline.
fn format_execution_graph(graph: &ExecutionGraph) -> String {
    let mut out = String::new();
    out.push_str(&format!("- **Entry point**: `{}`\n", graph.entry));
    out.push_str(&format!(
        "- **Nodes**: {}, **Edges**: {}\n\n",
        graph.nodes.len(),
        graph.edges.len()
    ));

    out.push_str("### Nodes\n\n");
    for (id, node) in &graph.nodes {
        let kind = action_label(&node.action);
        out.push_str(&format!("- `{id}`: {kind}\n"));
    }

    if !graph.edges.is_empty() {
        out.push_str("\n### Edges\n\n");
        for edge in &graph.edges {
            out.push_str(&format!(
                "  `{from}` → `{to}`\n",
                from = edge.from,
                to = edge.to
            ));
        }
    }

    out
}

fn action_label(action: &dpronix_core::graph::Action) -> String {
    match action {
        dpronix_core::graph::Action::Think { .. } => "Think".to_string(),
        dpronix_core::graph::Action::CallTool { tool, .. } => format!("CallTool({tool})"),
        dpronix_core::graph::Action::Observe { .. } => "Observe".to_string(),
        dpronix_core::graph::Action::Reflect { .. } => "Reflect".to_string(),
        dpronix_core::graph::Action::Delegate { sub_agent, .. } => {
            format!("Delegate({sub_agent})")
        }
        dpronix_core::graph::Action::Parallel(_) => "Parallel".to_string(),
        dpronix_core::graph::Action::Conditional { .. } => "Conditional".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use dpronix_core::chunk::{Chunk, Usage};
    use dpronix_core::graph::{Action, ExecutionNode};
    use dpronix_core::Tool;
    use dpronix_provider::Provider;
    use tokio_stream::StreamExt;

    // -----------------------------------------------------------------------
    // Mock Provider — returns a canned stream of chunks
    // -----------------------------------------------------------------------

    struct MockProvider {
        chunks: Vec<Chunk>,
    }

    impl MockProvider {
        fn new(chunks: Vec<Chunk>) -> Self {
            Self { chunks }
        }

        /// Convenience: returns a single TextDelta chunk + Done.
        fn text(text: impl Into<String>) -> Self {
            Self {
                chunks: vec![Chunk::TextDelta(text.into()), Chunk::Done],
            }
        }
    }

    #[async_trait::async_trait]
    impl Provider for MockProvider {
        async fn generate(
            &self,
            _messages: &[Message],
            _tools: &[&dyn Tool],
        ) -> anyhow::Result<Message> {
            Ok(Message {
                role: Role::Assistant,
                content: "mock plan".to_string(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            })
        }

        async fn stream(
            &self,
            _messages: &[Message],
            _tools: &[&dyn Tool],
        ) -> anyhow::Result<dpronix_core::chunk::ChunkStream> {
            let chunks: Vec<anyhow::Result<Chunk>> =
                self.chunks.clone().into_iter().map(Ok).collect();
            Ok(Box::pin(tokio_stream::iter(chunks)))
        }
    }

    // -----------------------------------------------------------------------
    // Mock Planner — returns a pre-built ExecutionGraph
    // -----------------------------------------------------------------------

    struct MockPlanner {
        name: String,
        graph: ExecutionGraph,
    }

    impl MockPlanner {
        fn new(name: &str, graph: ExecutionGraph) -> Self {
            Self {
                name: name.to_string(),
                graph,
            }
        }
    }

    #[async_trait::async_trait]
    impl Planner for MockPlanner {
        fn name(&self) -> &str {
            &self.name
        }

        async fn plan(&self, _goal: &str) -> anyhow::Result<ExecutionGraph> {
            Ok(self.graph.clone())
        }
    }

    // -----------------------------------------------------------------------
    // action_label tests
    // -----------------------------------------------------------------------

    #[test]
    fn action_label_think() {
        let a = Action::Think {
            prompt: "do something".into(),
        };
        assert_eq!(action_label(&a), "Think");
    }

    #[test]
    fn action_label_call_tool() {
        let a = Action::CallTool {
            tool: "grep".into(),
            args: serde_json::json!({}),
        };
        assert_eq!(action_label(&a), "CallTool(grep)");
    }

    #[test]
    fn action_label_observe() {
        let a = Action::Observe {
            tool_call_id: "abc".into(),
        };
        assert_eq!(action_label(&a), "Observe");
    }

    #[test]
    fn action_label_reflect() {
        let a = Action::Reflect {
            criteria: vec!["done".into()],
        };
        assert_eq!(action_label(&a), "Reflect");
    }

    #[test]
    fn action_label_delegate() {
        let a = Action::Delegate {
            sub_agent: "coder".into(),
            goal: "fix bug".into(),
        };
        assert_eq!(action_label(&a), "Delegate(coder)");
    }

    #[test]
    fn action_label_parallel() {
        let a = Action::Parallel(vec![]);
        assert_eq!(action_label(&a), "Parallel");
    }

    #[test]
    fn action_label_conditional() {
        let a = Action::Conditional {
            condition: "x > 0".into(),
            then: Box::new(ExecutionNode::new(
                "then",
                Action::Think {
                    prompt: "do".into(),
                },
            )),
            r#else: None,
        };
        assert_eq!(action_label(&a), "Conditional");
    }

    // -----------------------------------------------------------------------
    // format_execution_graph tests
    // -----------------------------------------------------------------------

    #[test]
    fn format_graph_includes_entry_nodes_edges() {
        let mut g = ExecutionGraph::new("start".into());
        g.add_node(ExecutionNode::new(
            "start",
            Action::Think {
                prompt: "hello".into(),
            },
        ));
        g.add_node(ExecutionNode::new(
            "end",
            Action::Reflect {
                criteria: vec!["done".into()],
            },
        ));
        g.add_edge("start".into(), "end".into(), None);

        let formatted = format_execution_graph(&g);
        assert!(formatted.contains("Entry point"));
        assert!(formatted.contains("`start`"));
        assert!(formatted.contains("**Nodes**: 2"));
        assert!(formatted.contains("**Edges**: 1"));
        assert!(formatted.contains("`start` → `end`"));
    }

    #[test]
    fn format_graph_empty_edges_no_edges_section() {
        let mut g = ExecutionGraph::new("a".into());
        g.add_node(ExecutionNode::new(
            "a",
            Action::Think { prompt: "x".into() },
        ));

        let formatted = format_execution_graph(&g);
        assert!(formatted.contains("**Nodes**: 1"));
        assert!(formatted.contains("**Edges**: 0"));
        assert!(!formatted.contains("### Edges"));
    }

    // -----------------------------------------------------------------------
    // PlanModeRunner integration tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn plan_mode_runner_streams_text_from_provider() {
        let provider = Arc::new(MockProvider::text("## Plan\n\n1. Step one\n2. Step two\n"));
        let runner = PlanModeRunner::new(provider);

        let input = RunInput {
            prompt: "Write a plan for a todo app".into(),
            images: vec![],
            model_override: None,
        };

        let mut stream = runner.run_stream(input).await.unwrap();
        let mut collected = String::new();

        while let Some(event) = stream.next().await {
            match event.unwrap() {
                RunEvent::TextDelta(delta) => collected.push_str(&delta),
                RunEvent::Done(output) => {
                    collected.push_str(&output.text);
                }
                _ => {}
            }
        }

        assert!(collected.contains("## Plan"));
        assert!(collected.contains("Step one"));
        assert!(collected.contains("Step two"));
    }

    #[tokio::test]
    async fn plan_mode_runner_with_planner_appends_graph() {
        let provider = Arc::new(MockProvider::text("## Plan\n\n1. Do X\n"));

        let mut graph = ExecutionGraph::new("analyze".into());
        graph.add_node(ExecutionNode::new(
            "analyze",
            Action::Think {
                prompt: "analyze goal".into(),
            },
        ));
        graph.add_node(ExecutionNode::new(
            "execute",
            Action::CallTool {
                tool: "shell".into(),
                args: serde_json::json!({}),
            },
        ));
        graph.add_edge("analyze".into(), "execute".into(), None);

        let planner = Arc::new(MockPlanner::new("mock", graph));
        let runner = PlanModeRunner::new(provider).with_planner(planner);

        let input = RunInput {
            prompt: "Do X".into(),
            images: vec![],
            model_override: None,
        };

        let mut stream = runner.run_stream(input).await.unwrap();
        let mut collected = String::new();

        while let Some(event) = stream.next().await {
            match event.unwrap() {
                RunEvent::TextDelta(delta) => collected.push_str(&delta),
                RunEvent::Done(output) => collected.push_str(&output.text),
                _ => {}
            }
        }

        assert!(collected.contains("## Plan"));
        assert!(collected.contains("Execution Graph"));
        assert!(collected.contains("CallTool(shell)"));
        assert!(collected.contains("`analyze` → `execute`"));
    }

    #[tokio::test]
    async fn plan_mode_runner_uses_custom_system_prompt() {
        // The custom prompt is invisible to the test (it goes into the
        // provider call), but we verify the runner constructed with a
        // custom prompt still produces output correctly.
        let provider = Arc::new(MockProvider::text("custom plan output"));
        let runner =
            PlanModeRunner::new(provider).with_system_prompt("You are a custom planner. Be brief.");

        let input = RunInput {
            prompt: "anything".into(),
            images: vec![],
            model_override: None,
        };

        let mut stream = runner.run_stream(input).await.unwrap();
        let mut has_output = false;

        while let Some(event) = stream.next().await {
            match event.unwrap() {
                RunEvent::TextDelta(_) => has_output = true,
                RunEvent::Done(_) => {}
                _ => {}
            }
        }

        assert!(has_output);
    }

    #[tokio::test]
    async fn plan_mode_runner_streams_reasoning_delta() {
        let provider = Arc::new(MockProvider::new(vec![
            Chunk::ReasoningDelta {
                text: "thinking deeply...".into(),
                signature: None,
            },
            Chunk::TextDelta("plan content".into()),
            Chunk::Done,
        ]));
        let runner = PlanModeRunner::new(provider);

        let input = RunInput {
            prompt: "goal".into(),
            images: vec![],
            model_override: None,
        };

        let mut stream = runner.run_stream(input).await.unwrap();
        let mut saw_reasoning = false;

        while let Some(event) = stream.next().await {
            match event.unwrap() {
                RunEvent::ReasoningDelta { text, .. } => {
                    assert_eq!(text, "thinking deeply...");
                    saw_reasoning = true;
                }
                RunEvent::Done(_) => break,
                _ => {}
            }
        }

        assert!(saw_reasoning);
    }

    #[tokio::test]
    async fn plan_mode_runner_streams_usage() {
        let usage = Usage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            cache_hit_tokens: 0,
            cache_miss_tokens: 0,
            reasoning_tokens: 0,
        };
        let provider = Arc::new(MockProvider::new(vec![
            Chunk::TextDelta("plan".into()),
            Chunk::Usage(usage.clone()),
            Chunk::Done,
        ]));
        let runner = PlanModeRunner::new(provider);

        let input = RunInput {
            prompt: "goal".into(),
            images: vec![],
            model_override: None,
        };

        let mut stream = runner.run_stream(input).await.unwrap();
        let mut saw_usage = false;

        while let Some(event) = stream.next().await {
            match event.unwrap() {
                RunEvent::Usage(u) => {
                    assert_eq!(u.total_tokens, 150);
                    saw_usage = true;
                }
                RunEvent::Done(_) => break,
                _ => {}
            }
        }

        assert!(saw_usage);
    }

    #[tokio::test]
    async fn plan_mode_runner_ignores_tool_chunks() {
        let provider = Arc::new(MockProvider::new(vec![
            Chunk::TextDelta("plan".into()),
            Chunk::ToolCallStart {
                id: "t1".into(),
                name: "grep".into(),
            },
            Chunk::ToolCallDelta {
                id: "t1".into(),
                args_delta: r#"{"pattern":"x"}"#.into(),
            },
            Chunk::ToolCallEnd {
                id: "t1".into(),
                name: "grep".into(),
                arguments: r#"{"pattern":"x"}"#.into(),
            },
            Chunk::Done,
        ]));
        let runner = PlanModeRunner::new(provider);

        let input = RunInput {
            prompt: "goal".into(),
            images: vec![],
            model_override: None,
        };

        // Collect via `run` — the convenience method on Runner.
        let output = runner.run(input).await.unwrap();

        // Tool call chunks should be ignored; only text makes it through.
        assert_eq!(output.text, "plan");
        assert!(output.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn plan_mode_runner_empty_goal_produces_output() {
        let provider = Arc::new(MockProvider::text("plan for empty goal"));
        let runner = PlanModeRunner::new(provider);

        let input = RunInput {
            prompt: String::new(),
            images: vec![],
            model_override: None,
        };

        let output = runner.run(input).await.unwrap();
        assert!(!output.text.is_empty());
    }
}

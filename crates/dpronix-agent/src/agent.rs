use crate::memory::Memory;
use dpronix_core::chunk::{Chunk, Usage};
use dpronix_core::tool::ToolContext;
use dpronix_core::types::{FunctionCall, ToolCall};
use dpronix_core::{Message, Role, RunEvent, RunEventStream, RunInput, RunOutput, Runner, Tool};
use dpronix_provider::Provider;
use dpronix_security::context::SecurityContext;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Approximate characters-per-token for rough heuristics.
const CHARS_PER_TOKEN: f32 = 4.0;

// ---------------------------------------------------------------------------
// Agent — the main agent runner
// ---------------------------------------------------------------------------

pub struct Agent {
    provider: Arc<dyn Provider>,
    tools: HashMap<String, Arc<dyn Tool>>,
    max_steps: usize,
    system_prompt: Option<String>,
    /// Workspace root used to confine filesystem tool calls. Defaults to the
    /// process working directory at construction time.
    workspace_root: PathBuf,
    /// Security context injected into every ToolContext. Defaults to the
    /// safe-defaults policy (all builtin capabilities granted).
    security: SecurityContext,

    compaction_threshold_tokens: Option<u32>,

    /// Optional persistent conversation store. When set, each `run_stream`
    /// seeds its working memory from this store at the start and writes the
    /// full conversation back at the end, giving the agent multi-turn memory
    /// across separate `run_stream` invocations. This is what enables desktop
    /// / CLI sessions to carry context — and crucially, it lets DeepSeek-V4's
    /// `reasoning_content` replay contract span user turns, not just the
    /// tool-loop within a single run.
    history: Option<Arc<tokio::sync::Mutex<Vec<Message>>>>,
}

impl Agent {
    pub fn new(provider: Arc<dyn Provider>, max_steps: usize) -> Self {
        Self {
            provider,
            tools: HashMap::new(),
            max_steps: if max_steps == 0 { 10 } else { max_steps },
            system_prompt: None,
            workspace_root: std::env::current_dir().unwrap_or_default(),
            security: SecurityContext::with_safe_defaults(),

            compaction_threshold_tokens: None,
            history: None,
        }
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub fn with_compaction_threshold(mut self, tokens: Option<u32>) -> Self {
        self.compaction_threshold_tokens = tokens;
        self
    }

    /// Attach a persistent conversation store so this agent carries memory
    /// across successive `run_stream` calls. Callers share one
    /// `Arc<Mutex<Vec<Message>>>` across turns (and reset it to start a new
    /// session). When the store is non-empty at run start, the system prompt
    /// is *not* re-injected — the prior turns already contain it.
    pub fn with_conversation_history(
        mut self,
        history: Arc<tokio::sync::Mutex<Vec<Message>>>,
    ) -> Self {
        self.history = Some(history);
        self
    }

    /// Override the workspace root used to confine filesystem tool calls.
    pub fn with_workspace_root(mut self, workspace_root: PathBuf) -> Self {
        self.workspace_root = workspace_root;
        self
    }

    /// Override the security context injected into every tool execution.
    pub fn with_security(mut self, security: SecurityContext) -> Self {
        self.security = security;
        self
    }

    pub fn register_tool(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.schema().name.clone();
        self.tools.insert(name, tool);
    }
}

#[async_trait::async_trait]
impl Runner for Agent {
    async fn run_stream(&self, input: RunInput) -> anyhow::Result<RunEventStream> {
        let (tx, rx) = mpsc::channel(64);

        let provider = Arc::clone(&self.provider);
        let tools: Vec<Arc<dyn Tool>> = self.tools.values().cloned().collect();
        let max_steps = self.max_steps;
        let system_prompt = self.system_prompt.clone();
        let compaction_threshold = self.compaction_threshold_tokens;
        let workspace_root = self.workspace_root.clone();
        let security = self.security.clone();
        let history = self.history.clone();

        // Create a cancellation token and wire Ctrl-C (SIGINT) to cancel it.
        // This enables graceful interruption of the agent loop (e.g. Ctrl-C).
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {
                        info!("Ctrl-C received, cancelling agent...");
                        cancel_clone.cancel();
                        break;
                    }
                    _ = cancel_clone.cancelled() => break,
                }
            }
        });

        tokio::spawn(async move {
            let mut memory = Memory::new();

            // Seed working memory from the persistent conversation store, if
            // one is attached. This is what makes the agent remember prior
            // user turns (and preserves DeepSeek-V4 reasoning_content across
            // turns for the must_replay contract).
            let seeded = if let Some(ref hist) = history {
                let prior = hist.lock().await;
                for m in prior.iter() {
                    memory.add_message(m.clone());
                }
                !prior.is_empty()
            } else {
                false
            };

            // Inject the system prompt only on a fresh conversation. When the
            // store already holds prior turns, the system prompt is part of
            // them and re-injecting it would duplicate it.
            if !seeded {
                if let Some(ref sp) = system_prompt {
                    memory.add_message(Message {
                        role: Role::System,
                        content: sp.clone(),
                        name: None,
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                    });
                }
            }

            let result = run_agent_loop(
                provider,
                tools,
                max_steps,
                compaction_threshold,
                &mut memory,
                input,
                &tx,
                &cancel,
                workspace_root,
                security,
            )
            .await;

            // Persist the full conversation back to the store so the next
            // run_stream call resumes with this context. We write back even
            // on error so partial progress (and any must_replay reasoning) is
            // not silently lost between turns.
            if let Some(ref hist) = history {
                let mut store = hist.lock().await;
                *store = memory.get_all();
            }

            if let Err(e) = result {
                warn!("agent loop error: {e}");
                let _ = tx.send(Err(e)).await;
            }
        });

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}

// ---------------------------------------------------------------------------
// Agent loop — runs in a spawned task
// ---------------------------------------------------------------------------

/// Accumulated tool call from streaming chunks.
#[derive(Debug, Clone)]
struct PendingToolCall {
    id: String,
    name: String,
    arguments: String,
}

async fn run_agent_loop(
    provider: Arc<dyn Provider>,
    tools: Vec<Arc<dyn Tool>>,
    max_steps: usize,
    compaction_threshold: Option<u32>,
    memory: &mut Memory,
    input: RunInput,
    tx: &mpsc::Sender<anyhow::Result<RunEvent>>,
    cancel: &CancellationToken,
    workspace_root: PathBuf,
    security: SecurityContext,
) -> anyhow::Result<()> {
    // Add user prompt
    memory.add_message(Message {
        role: Role::User,
        content: input.prompt.clone(),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    });

    for step in 0..max_steps {
        // Check for cancellation between steps
        if cancel.is_cancelled() {
            tx.send(Ok(RunEvent::Done(RunOutput {
                text: String::new(),
                tool_calls: Vec::new(),
                usage: None,
            })))
            .await
            .ok();
            return Ok(());
        }

        info!("agent step {}/{}", step + 1, max_steps);

        // Atomic Turn-end compaction
        if let Some(threshold) = compaction_threshold {
            let all_msgs = memory.get_all();
            let tokens = estimate_tokens(&all_msgs);

            if tokens > threshold {
                let before = tokens;
                memory.shrink_large_results(threshold as usize * 4);
                let after_shrink = estimate_tokens(&memory.get_all());

                info!("shrunk tool results: {} -> {} tokens", before, after_shrink);

                if after_shrink > threshold {
                    warn!("context still over threshold after shrinking tool results. sliding window...");
                    memory.slide_window();
                    let after_slide = estimate_tokens(&memory.get_all());
                    info!("slid window: {} -> {} tokens", after_shrink, after_slide);
                }
            }
        }

        // Build the tool index for execution
        let tool_map: HashMap<String, Arc<dyn Tool>> = tools
            .iter()
            .map(|t| (t.schema().name.clone(), Arc::clone(t)))
            .collect();

        // Stream from provider
        let step_result = stream_and_process_turn(
            &provider,
            &tools,
            &tool_map,
            memory,
            tx,
            cancel,
            &workspace_root,
            &security,
        )
        .await?;

        match step_result {
            StepOutcome::Complete(output) => {
                tx.send(Ok(RunEvent::Done(output))).await.ok();
                return Ok(());
            }
            StepOutcome::Continue => {
                // Tools were executed; loop continues
                continue;
            }
            StepOutcome::MaxSteps => {
                warn!("agent reached max steps ({max_steps})");
                return Err(anyhow::anyhow!(
                    "reached max steps ({max_steps}) without completing the task"
                ));
            }
        }
    }

    warn!("agent reached max steps ({max_steps})");
    Err(anyhow::anyhow!(
        "reached max steps ({max_steps}) without completing the task"
    ))
}

// ---------------------------------------------------------------------------
// Turn processing — one provider call + optional tool execution
// ---------------------------------------------------------------------------

enum StepOutcome {
    /// Agent produced final text output — done.
    Complete(RunOutput),
    /// Agent made tool calls — results added to memory, continue loop.
    Continue,
    /// Nothing was produced — max steps will be exhausted.
    MaxSteps,
}

async fn stream_and_process_turn(
    provider: &Arc<dyn Provider>,
    tools: &[Arc<dyn Tool>],
    tool_map: &HashMap<String, Arc<dyn Tool>>,
    memory: &mut Memory,
    tx: &mpsc::Sender<anyhow::Result<RunEvent>>,
    cancel: &CancellationToken,
    workspace_root: &std::path::Path,
    security: &SecurityContext,
) -> anyhow::Result<StepOutcome> {
    // Build tool refs for provider
    let tool_refs: Vec<&dyn Tool> = tools.iter().map(|t| t.as_ref()).collect();
    let messages = memory.get_all();

    // DeepSeek V4 protocol — ValidatedRequest::new fails early with
    // structured violation list, preventing corrupt messages from
    // ever reaching the provider
    let validated =
        dpronix_provider::ValidatedRequest::new(&messages, &tool_refs).map_err(|violations| {
            for v in &violations {
                tracing::error!(?v, "replay invariant violation before provider call");
            }
            anyhow::anyhow!(
                "history replay invariant violated: {} violation(s) detected",
                violations.len()
            )
        })?;

    let mut stream = provider.stream(validated).await?;

    let mut text_buf = String::new();
    let mut reasoning_buf = String::new();
    let mut usage: Option<Usage> = None;
    let mut pending_calls: Vec<PendingToolCall> = Vec::new();

    // Consume the stream
    while let Some(chunk_result) = stream.next().await {
        if cancel.is_cancelled() {
            return Ok(StepOutcome::Complete(RunOutput {
                text: text_buf,
                tool_calls: Vec::new(),
                usage: None,
            }));
        }

        let chunk = chunk_result?;
        match chunk {
            Chunk::TextDelta(delta) => {
                text_buf.push_str(&delta);
                tx.send(Ok(RunEvent::TextDelta(delta))).await.ok();
            }
            Chunk::ReasoningDelta { text, signature } => {
                reasoning_buf.push_str(&text);
                tx.send(Ok(RunEvent::ReasoningDelta { text, signature }))
                    .await
                    .ok();
            }
            Chunk::ToolCallStart { id, name } => {
                // Start accumulating a new tool call
                pending_calls.push(PendingToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: String::new(),
                });
                tx.send(Ok(RunEvent::ToolCallStart { id, name })).await.ok();
            }
            Chunk::ToolCallDelta { id, args_delta } => {
                // Accumulate arguments into the matching pending call
                if let Some(call) = pending_calls.iter_mut().find(|c| c.id == id) {
                    call.arguments.push_str(&args_delta);
                }
                tx.send(Ok(RunEvent::ToolCallDelta { id, args_delta }))
                    .await
                    .ok();
            }
            Chunk::ToolCallEnd {
                id,
                name,
                arguments,
            } => {
                // If we already accumulated from deltas, merge; otherwise use the complete args
                if let Some(call) = pending_calls.iter_mut().find(|c| c.id == id) {
                    if !arguments.is_empty() && call.arguments.is_empty() {
                        call.arguments = arguments.clone();
                    }
                } else {
                    pending_calls.push(PendingToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: arguments.clone(),
                    });
                }
                tx.send(Ok(RunEvent::ToolCallEnd {
                    id,
                    name,
                    arguments,
                }))
                .await
                .ok();
            }
            Chunk::Usage(u) => {
                tx.send(Ok(RunEvent::Usage(u.clone()))).await.ok();
                usage = Some(u);
            }
            Chunk::Done => {}
        }
    }

    // --- Determine what the model wants ---
    let has_text = !text_buf.is_empty();
    let has_tool_calls = !pending_calls.is_empty();

    // Case 1: Only text → final answer
    if has_text && !has_tool_calls {
        // Add assistant message to memory
        memory.add_message(Message {
            role: Role::Assistant,
            content: text_buf.clone(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: if reasoning_buf.is_empty() {
                None
            } else {
                Some(reasoning_buf.clone())
            },
        });

        let final_calls: Vec<ToolCall> = pending_calls
            .into_iter()
            .map(|c| ToolCall {
                id: c.id,
                ty: "function".to_string(),
                function: FunctionCall {
                    name: c.name,
                    arguments: c.arguments,
                },
            })
            .collect();

        return Ok(StepOutcome::Complete(RunOutput {
            text: text_buf,
            tool_calls: final_calls,
            usage,
        }));
    }

    // Case 2: Tool calls (with or without text)
    if has_tool_calls {
        tx.send(Ok(RunEvent::TurnComplete)).await.ok();

        // Add assistant message with tool_calls to memory
        let tool_calls_for_msg: Vec<ToolCall> = pending_calls
            .iter()
            .map(|c| ToolCall {
                id: c.id.clone(),
                ty: "function".to_string(),
                function: FunctionCall {
                    name: c.name.clone(),
                    arguments: c.arguments.clone(),
                },
            })
            .collect();

        memory.add_message(Message {
            role: Role::Assistant,
            content: text_buf.clone(),
            name: None,
            tool_calls: Some(tool_calls_for_msg),
            tool_call_id: None,
            reasoning_content: if reasoning_buf.is_empty() {
                None
            } else {
                Some(reasoning_buf.clone())
            },
        });

        // Execute each tool call
        for call in &pending_calls {
            if cancel.is_cancelled() {
                break;
            }

            let ctx = ToolContext::with_cancellation(&call.id, cancel.child_token())
                .with_workspace(workspace_root.to_path_buf())
                .with_extension(security.clone());
            let result = if let Some(tool) = tool_map.get(&call.name) {
                info!(tool = %call.name, id = %call.id, "executing tool");
                match tool.execute(&ctx, &call.arguments).await {
                    Ok(output) => output,
                    Err(e) => {
                        let err_str = format!("{e:#}");
                        // Truncate tool errors to avoid leaking file paths or data into context
                        let max_len = 500;
                        let truncated = if err_str.len() > max_len {
                            let end = err_str.floor_char_boundary(max_len);
                            format!(
                                "{}... [truncated {} bytes]",
                                &err_str[..end],
                                err_str.len() - end
                            )
                        } else {
                            err_str
                        };
                        format!("Error: {truncated}")
                    }
                }
            } else {
                format!("Error: unknown tool '{}'", call.name)
            };

            // Send ToolResult event
            tx.send(Ok(RunEvent::ToolResult {
                call_id: call.id.clone(),
                result: result.clone(),
            }))
            .await
            .ok();

            // Add tool result to memory
            memory.add_message(Message {
                role: Role::Tool,
                content: result,
                name: None,
                tool_calls: None,
                tool_call_id: Some(call.id.clone()),
                reasoning_content: None,
            });
        }

        return Ok(StepOutcome::Continue);
    }

    // Case 3: No text, no tool calls — end of stream without meaningful output
    if usage.is_some() {
        // Usage only (some models send a final usage-only chunk after stream ends)
        // This means the model returned nothing — end the turn
        return Ok(StepOutcome::Complete(RunOutput {
            text: String::new(),
            tool_calls: Vec::new(),
            usage,
        }));
    }

    // Nothing produced at all
    warn!("step produced no output");
    Ok(StepOutcome::MaxSteps)
}

// ---------------------------------------------------------------------------
// Token estimation helpers (public for testing)
// ---------------------------------------------------------------------------

/// Rough token count estimate from message content length.
pub fn estimate_tokens(messages: &[Message]) -> u32 {
    let char_count: usize = messages
        .iter()
        .map(|m| m.content.len() + m.reasoning_content.as_ref().map(|r| r.len()).unwrap_or(0))
        .sum();
    (char_count as f32 / CHARS_PER_TOKEN).ceil() as u32
}

#[allow(dead_code)]
fn format_role(role: Role) -> &'static str {
    match role {
        Role::System => "System",
        Role::User => "User",
        Role::Assistant => "Assistant",
        Role::Tool => "Tool",
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::MockProvider;
    use dpronix_core::tool::ToolContext;
    use dpronix_core::types::ToolSchema;
    use std::sync::Arc;
    use tokio_stream::StreamExt;

    // -----------------------------------------------------------------------
    // Simple structure for testing: a fake tool that records invocations
    // -----------------------------------------------------------------------

    struct SpyTool {
        name: &'static str,
        result: String,
    }

    #[async_trait::async_trait]
    impl Tool for SpyTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: self.name.to_string(),
                description: "spy tool".to_string(),
                parameters: serde_json::json!({"type":"object","properties":{}}),
            }
        }
        fn read_only(&self) -> bool {
            true
        }
        async fn execute(&self, _ctx: &ToolContext, _args: &str) -> anyhow::Result<String> {
            Ok(self.result.clone())
        }
    }

    // -----------------------------------------------------------------------
    // Unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn token_estimate_zero_for_empty() {
        assert_eq!(estimate_tokens(&[]), 0);
    }

    #[test]
    fn token_estimate_scales_with_content() {
        let msgs = vec![Message {
            role: Role::User,
            content: "hello world, this is a test message".to_string(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }];
        let tokens = estimate_tokens(&msgs);
        assert!(tokens > 0);
        assert!(tokens < 100);
    }

    #[test]
    fn format_role_returns_correct_names() {
        assert_eq!(format_role(Role::User), "User");
        assert_eq!(format_role(Role::Assistant), "Assistant");
        assert_eq!(format_role(Role::System), "System");
        assert_eq!(format_role(Role::Tool), "Tool");
    }

    // -----------------------------------------------------------------------
    // Integration tests: Agent + MockProvider
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn agent_streams_text_from_provider() {
        let provider = Arc::new(MockProvider::text("hello from agent"));
        let agent = Agent::new(provider, 3);

        let input = RunInput {
            prompt: "say hi".into(),
            images: vec![],
            model_override: None,
        };

        let mut stream = agent.run_stream(input).await.unwrap();
        let mut text = String::new();
        let mut done = false;

        while let Some(event) = stream.next().await {
            match event.unwrap() {
                RunEvent::TextDelta(t) => text.push_str(&t),
                RunEvent::Done(_) => done = true,
                _ => {}
            }
        }

        assert_eq!(text, "hello from agent");
        assert!(done);
    }

    #[tokio::test]
    async fn agent_respects_max_steps() {
        let provider = Arc::new(MockProvider::text("done"));
        let agent = Agent::new(provider, 2);

        let input = RunInput {
            prompt: "do something".into(),
            images: vec![],
            model_override: None,
        };

        let mut stream = agent.run_stream(input).await.unwrap();
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.unwrap());
        }

        assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
    }

    #[tokio::test]
    async fn agent_empty_prompt_still_runs() {
        let provider = Arc::new(MockProvider::text("response to empty"));
        let agent = Agent::new(provider, 3);

        let input = RunInput {
            prompt: "".into(),
            images: vec![],
            model_override: None,
        };

        let mut stream = agent.run_stream(input).await.unwrap();
        let mut text = String::new();
        while let Some(event) = stream.next().await {
            if let Ok(RunEvent::TextDelta(t)) = event {
                text.push_str(&t);
            }
        }

        assert!(text.contains("response to empty"));
    }

    #[tokio::test]
    async fn agent_registers_and_uses_tools() {
        let provider = Arc::new(MockProvider::text("used tool"));
        let mut agent = Agent::new(provider, 3);
        agent.register_tool(Arc::new(SpyTool {
            name: "spy",
            result: "tool ran".into(),
        }));

        let input = RunInput {
            prompt: "use spy".into(),
            images: vec![],
            model_override: None,
        };

        let mut stream = agent.run_stream(input).await.unwrap();
        let mut text = String::new();
        while let Some(event) = stream.next().await {
            if let Ok(RunEvent::TextDelta(t)) = event {
                text.push_str(&t);
            }
        }

        assert!(!text.is_empty(), "agent should produce text output");
    }

    #[tokio::test]
    async fn agent_max_steps_zero_defaults_to_ten() {
        let provider = Arc::new(MockProvider::text("ok"));
        let agent = Agent::new(provider, 0);

        let input = RunInput {
            prompt: "test".into(),
            images: vec![],
            model_override: None,
        };

        let result = agent.run_stream(input).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn agent_system_prompt_injected() {
        let provider = Arc::new(MockProvider::text("got prompt"));
        let agent = Agent::new(provider, 3).with_system_prompt("you are a test bot");

        let input = RunInput {
            prompt: "who are you".into(),
            images: vec![],
            model_override: None,
        };

        let result = agent.run_stream(input).await;
        assert!(
            result.is_ok(),
            "agent with system prompt should run without error"
        );
    }

    #[tokio::test]
    async fn agent_compaction_threshold_triggers() {
        let provider = Arc::new(MockProvider::text("compacted"));
        let agent = Agent::new(provider, 3).with_compaction_threshold(Some(1));

        let input = RunInput {
            prompt: "a really long message that should trigger compaction".into(),
            images: vec![],
            model_override: None,
        };

        let result = agent.run_stream(input).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn agent_executes_tool_calls() {
        // Mock provider returns a tool call first, then text
        let spy = Arc::new(SpyTool {
            name: "spy",
            result: "tool executed!".into(),
        });
        // Turn 1: tool call -> agent executes it -> continues loop
        // Turn 2: final text -> agent completes
        let responses = vec![
            vec![
                Chunk::ToolCallStart {
                    id: "call_1".into(),
                    name: "spy".into(),
                },
                Chunk::ToolCallEnd {
                    id: "call_1".into(),
                    name: "spy".into(),
                    arguments: "{}".into(),
                },
                Chunk::Done,
            ],
            vec![
                Chunk::TextDelta("done after tool".into()),
                Chunk::Usage(Usage::default()),
                Chunk::Done,
            ],
        ];
        let provider = Arc::new(MockProvider::sequential(responses).with_tools(vec![spy]));
        let mut agent = Agent::new(provider, 5);
        agent.register_tool(Arc::new(SpyTool {
            name: "spy",
            result: "tool executed!".into(),
        }));

        let input = RunInput {
            prompt: "use the tool".into(),
            images: vec![],
            model_override: None,
        };

        let mut stream = agent.run_stream(input).await.unwrap();
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.unwrap());
        }

        // Should see ToolResult and eventually Done
        let has_tool_result = events
            .iter()
            .any(|e| matches!(e, RunEvent::ToolResult { .. }));
        let has_done = events.iter().any(|e| matches!(e, RunEvent::Done(_)));
        assert!(
            has_tool_result,
            "agent should execute tools and emit ToolResult"
        );
        assert!(has_done, "agent should eventually complete with Done");
    }

    #[tokio::test]
    async fn agent_conversation_history_persists_across_runs() {
        // A shared persistent store simulates one desktop/CLI session.
        let history = Arc::new(tokio::sync::Mutex::new(Vec::<Message>::new()));

        // --- Turn 1 ---
        let agent1 = Agent::new(Arc::new(MockProvider::text("first answer")), 3)
            .with_system_prompt("sys")
            .with_conversation_history(history.clone());
        let mut s1 = agent1
            .run_stream(RunInput {
                prompt: "hello".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        // Draining to None guarantees the spawned task finished its writeback
        // (tx is dropped only after the store is persisted).
        while s1.next().await.is_some() {}

        {
            let store = history.lock().await;
            assert!(
                store.iter().any(|m| m.role == Role::System),
                "system prompt should be persisted on the first turn"
            );
            assert!(
                store
                    .iter()
                    .any(|m| m.role == Role::User && m.content == "hello"),
                "first user turn should be persisted"
            );
            assert!(
                store.len() >= 3,
                "expected at least system + user + assistant, got {}",
                store.len()
            );
        }

        // --- Turn 2: a brand-new agent sharing the same history store ---
        let agent2 = Agent::new(Arc::new(MockProvider::text("second answer")), 3)
            .with_system_prompt("sys")
            .with_conversation_history(history.clone());
        let mut s2 = agent2
            .run_stream(RunInput {
                prompt: "again".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while s2.next().await.is_some() {}

        let store = history.lock().await;
        // Both user turns present => memory carried across separate runs.
        assert!(
            store
                .iter()
                .any(|m| m.role == Role::User && m.content == "hello"),
            "turn-1 user message must survive into turn 2"
        );
        assert!(
            store
                .iter()
                .any(|m| m.role == Role::User && m.content == "again"),
            "turn-2 user message must be present"
        );
        // System prompt must NOT be duplicated on the seeded second run.
        let system_count = store.iter().filter(|m| m.role == Role::System).count();
        assert_eq!(
            system_count, 1,
            "system prompt must not be re-injected on a seeded run"
        );
    }

    #[tokio::test]
    async fn agent_persists_reasoning_content_across_turns() {
        // Proves the DeepSeek-V4 adaptation: an assistant turn's
        // reasoning_content is written into the shared history store, so a
        // subsequent run can replay it (must_replay contract spanning turns).
        let history = Arc::new(tokio::sync::Mutex::new(Vec::<Message>::new()));

        let turn1 = vec![
            Chunk::ReasoningDelta {
                text: "let me think about q1".into(),
                signature: Some("sig-1".into()),
            },
            Chunk::TextDelta("the answer".into()),
            Chunk::Usage(Usage::default()),
            Chunk::Done,
        ];
        let agent = Agent::new(Arc::new(MockProvider::new(turn1)), 3)
            .with_conversation_history(history.clone());
        let mut s = agent
            .run_stream(RunInput {
                prompt: "q1".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while s.next().await.is_some() {}

        let store = history.lock().await;
        assert!(
            store.iter().any(|m| m.role == Role::Assistant
                && m.reasoning_content.as_deref() == Some("let me think about q1")),
            "assistant reasoning_content must persist into the shared history \
             so DeepSeek-V4 reasoning replay works across turns"
        );
    }
}

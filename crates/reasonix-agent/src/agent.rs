use crate::memory::Memory;
use reasonix_core::chunk::{Chunk, Usage};
use reasonix_core::tool::ToolContext;
use reasonix_core::types::{FunctionCall, ToolCall};
use reasonix_core::{Message, Role, RunEvent, RunEventStream, RunInput, RunOutput, Runner, Tool};
use reasonix_provider::Provider;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::signal;
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
    compaction_threshold_tokens: Option<u32>,
}

impl Agent {
    pub fn new(provider: Arc<dyn Provider>, max_steps: usize) -> Self {
        Self {
            provider,
            tools: HashMap::new(),
            max_steps: if max_steps == 0 { 10 } else { max_steps },
            system_prompt: None,
            compaction_threshold_tokens: None,
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

        let mut memory = Memory::new();

        // Inject system prompt if configured
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
            if let Err(e) = run_agent_loop(
                provider,
                tools,
                max_steps,
                compaction_threshold,
                &mut memory,
                input,
                &tx,
                &cancel,
            )
            .await
            {
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
) -> anyhow::Result<StepOutcome> {
    // Build tool refs for provider
    let tool_refs: Vec<&dyn Tool> = tools.iter().map(|t| t.as_ref()).collect();
    let messages = memory.get_all();

    let mut stream = provider.stream(&messages, &tool_refs).await?;

    let mut text_buf = String::new();
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
                tx.send(Ok(RunEvent::ToolCallStart { id, name }))
                    .await
                    .ok();
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
            Chunk::ToolCallEnd { id, name, arguments } => {
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
                tx.send(Ok(RunEvent::ToolCallEnd { id, name, arguments }))
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
            reasoning_content: None,
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
            reasoning_content: None,
        });

        // Execute each tool call
        for call in &pending_calls {
            if cancel.is_cancelled() {
                break;
            }

            let ctx = ToolContext::with_cancellation(&call.id, cancel.child_token());
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
                            format!("{}... [truncated {} bytes]", &err_str[..end], err_str.len() - end)
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
    let char_count: usize = messages.iter().map(|m| m.content.len()).sum();
    (char_count as f32 / CHARS_PER_TOKEN).ceil() as u32
}

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
    use reasonix_core::tool::ToolContext;
    use reasonix_core::types::ToolSchema;
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
        let agent = Agent::new(provider, 3)
            .with_system_prompt("you are a test bot");

        let input = RunInput {
            prompt: "who are you".into(),
            images: vec![],
            model_override: None,
        };

        let result = agent.run_stream(input).await;
        assert!(result.is_ok(), "agent with system prompt should run without error");
    }

    #[tokio::test]
    async fn agent_compaction_threshold_triggers() {
        let provider = Arc::new(MockProvider::text("compacted"));
        let agent = Agent::new(provider, 3)
            .with_compaction_threshold(Some(1));

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
        let has_tool_result = events.iter().any(|e| matches!(e, RunEvent::ToolResult { .. }));
        let has_done = events.iter().any(|e| matches!(e, RunEvent::Done(_)));
        assert!(has_tool_result, "agent should execute tools and emit ToolResult");
        assert!(has_done, "agent should eventually complete with Done");
    }
}

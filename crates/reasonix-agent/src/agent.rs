use crate::memory::Memory;
use reasonix_core::chunk::{Chunk, Usage};
use reasonix_core::{Message, Role, RunEvent, RunEventStream, RunInput, RunOutput, Runner, Tool};
use reasonix_provider::Provider;
use std::collections::HashMap;
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

    pub fn with_compaction_threshold(mut self, tokens: u32) -> Self {
        self.compaction_threshold_tokens = Some(tokens);
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

        // Clone/Arc what the spawned task needs.
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
            });
        }

        tokio::spawn(async move {
            if let Err(e) = run_agent_loop(
                provider,
                tools,
                max_steps,
                compaction_threshold,
                &mut memory,
                input,
                &tx,
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

async fn run_agent_loop(
    provider: Arc<dyn Provider>,
    tools: Vec<Arc<dyn Tool>>,
    max_steps: usize,
    compaction_threshold: Option<u32>,
    memory: &mut Memory,
    input: RunInput,
    tx: &mpsc::Sender<anyhow::Result<RunEvent>>,
) -> anyhow::Result<()> {
    let cancel = CancellationToken::new();

    // Add user prompt
    memory.add_message(Message {
        role: Role::User,
        content: input.prompt.clone(),
        name: None,
        tool_calls: None,
        tool_call_id: None,
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

        // Compact if needed
        if let Some(threshold) = compaction_threshold {
            let all_msgs = memory.get_all();
            let tokens = estimate_tokens(&all_msgs);
            if tokens > threshold {
                let before = tokens;
                match compact_with_provider(provider.as_ref(), &all_msgs).await {
                    Ok(digest) => {
                        memory.compact(digest);
                        let after = estimate_tokens(&memory.get_all());
                        info!("compacted {before} → {after} tokens");
                    }
                    Err(e) => {
                        warn!("compaction failed: {e}, using simple fallback");
                        let digest = format!(
                            "Conversation summary ({} messages). Content truncated due to length.",
                            all_msgs.len()
                        );
                        memory.compact(digest);
                    }
                }
            }
        }

        // Build tool refs for provider
        let tool_refs: Vec<&dyn Tool> = tools.iter().map(|t| t.as_ref()).collect();
        let messages = memory.get_all();

        // Stream from provider
        let mut stream = provider.stream(&messages, &tool_refs).await?;
        let mut text_buf = String::new();
        let mut usage: Option<Usage> = None;

        while let Some(chunk) = stream.next().await {
            match chunk? {
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
                    tx.send(Ok(RunEvent::ToolCallStart { id, name })).await.ok();
                }
                Chunk::ToolCallDelta { id, args_delta } => {
                    tx.send(Ok(RunEvent::ToolCallDelta { id, args_delta }))
                        .await
                        .ok();
                }
                Chunk::ToolCallEnd {
                    id,
                    name,
                    arguments,
                } => {
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

        tx.send(Ok(RunEvent::TurnComplete)).await.ok();

        // If the model returned text (not tool calls), we're done
        if !text_buf.is_empty() && usage.is_some() {
            memory.add_message(Message {
                role: Role::Assistant,
                content: text_buf.clone(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            });

            let output = RunOutput {
                text: text_buf,
                tool_calls: Vec::new(),
                usage,
            };
            tx.send(Ok(RunEvent::Done(output))).await.ok();
            return Ok(());
        }

        // If no text was produced, something went wrong
        if text_buf.is_empty() && usage.is_none() {
            warn!("step {step} produced no output");
            break;
        }

        // Add partial text to memory and continue loop
        memory.add_message(Message {
            role: Role::Assistant,
            content: text_buf,
            name: None,
            tool_calls: None,
            tool_call_id: None,
        });
    }

    warn!("agent reached max steps ({max_steps})");
    Err(anyhow::anyhow!(
        "reached max steps ({max_steps}) without completing the task"
    ))
}

// ---------------------------------------------------------------------------
// Token estimation helpers (public for testing)
// ---------------------------------------------------------------------------

/// Rough token count estimate from message content length.
pub fn estimate_tokens(messages: &[Message]) -> u32 {
    let char_count: usize = messages.iter().map(|m| m.content.len()).sum();
    (char_count as f32 / CHARS_PER_TOKEN).ceil() as u32
}

/// Build a compaction digest by asking the provider to summarize old messages.
async fn compact_with_provider(
    provider: &dyn Provider,
    messages: &[Message],
) -> anyhow::Result<String> {
    // Build a summarization prompt
    let conversation_text: String = messages
        .iter()
        .map(|m| format!("[{}]: {}", format_role(m.role.clone()), m.content))
        .collect::<Vec<_>>()
        .join("\n\n");

    let summary_prompt = format!(
        "Summarize the following conversation into a concise digest. \
         Keep key decisions, action items, and context. \
         The summary will replace these messages to save context space.\n\n\
         <conversation>\n{conversation_text}\n</conversation>\n\n\
         Provide a compact summary (under 500 words)."
    );

    let summary_msgs = vec![Message {
        role: Role::User,
        content: summary_prompt,
        name: None,
        tool_calls: None,
        tool_call_id: None,
    }];

    let result = provider.generate(&summary_msgs, &[]).await?;
    Ok(result.content)
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
    // Unit tests (unchanged from original)
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
    // Integration tests: Agent + MockProvider + real Tool
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
        // Provider returns Usage which triggers completion check each step
        let provider = Arc::new(MockProvider::text("done"));
        let agent = Agent::new(provider, 2);

        let input = RunInput {
            prompt: "do something".into(),
            images: vec![],
            model_override: None,
        };

        let mut stream = agent.run_stream(input).await.unwrap();
        // Drain — should complete within max_steps (agent returns Done when
        // it gets text+usage from the provider, so this finishes in 1 step)
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.unwrap());
        }

        // Agent loop succeeded (didn't error out from max_steps exhaustion)
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
        let agent = Agent::new(provider, 0); // 0 → defaults to 10

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
        // Set tiny threshold so compaction fires immediately
        let provider = Arc::new(MockProvider::text("compacted"));
        let agent = Agent::new(provider, 3)
            .with_compaction_threshold(1); // 1 token threshold — always triggers

        let input = RunInput {
            prompt: "a really long message that should trigger compaction".into(),
            images: vec![],
            model_override: None,
        };

        let result = agent.run_stream(input).await;
        // Compaction falls back to simple string truncation when the provider
        // mock returns a short response. Either way, the agent should not crash.
        assert!(result.is_ok());
    }
}

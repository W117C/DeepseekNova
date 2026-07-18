use crate::memory::Memory;
use dpronix_core::chunk::{Chunk, Usage};
use dpronix_core::{Message, Role, RunEvent, RunEventStream, RunInput, RunOutput, Runner, Tool};
use dpronix_provider::Provider;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Approximate characters-per-token for rough heuristics.
const CHARS_PER_TOKEN: f32 = 4.0;

// ---------------------------------------------------------------------------
// SubAgentConfig — independent context for a single sub-agent type
// ---------------------------------------------------------------------------

/// Configuration for a named sub-agent. Each sub-agent has its own
/// system prompt, tool set, and execution parameters.
#[derive(Clone)]
pub struct SubAgentConfig {
    pub name: String,
    pub system_prompt: String,
    pub tools: Vec<Arc<dyn Tool>>,
    pub max_steps: usize,
}

impl fmt::Debug for SubAgentConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubAgentConfig")
            .field("name", &self.name)
            .field("system_prompt", &self.system_prompt)
            .field("tools_count", &self.tools.len())
            .field("max_steps", &self.max_steps)
            .finish()
    }
}

impl SubAgentConfig {
    pub fn new(name: impl Into<String>, system_prompt: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            system_prompt: system_prompt.into(),
            tools: Vec::new(),
            max_steps: 10,
        }
    }

    pub fn with_tools(mut self, tools: Vec<Arc<dyn Tool>>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_max_steps(mut self, steps: usize) -> Self {
        self.max_steps = if steps == 0 { 10 } else { steps };
        self
    }
}

// ---------------------------------------------------------------------------
// SubAgentRunner — delegate dispatch with independent context
// ---------------------------------------------------------------------------

/// `SubAgentRunner` implements the `Runner` trait and dispatches tasks
/// to named sub-agents. Each sub-agent invocation gets an independent
/// memory context, system prompt, and tool set.
///
/// The runner accepts a `RunInput` whose `prompt` encodes the sub-agent
/// name and goal. The expected format is:
///
/// ```text
/// sub_agent:<name>
/// goal:<goal text>
/// ```
///
/// If no `sub_agent:` prefix is found, the runner dispatches to a
/// default sub-agent named `"default"` if one is registered.
pub struct SubAgentRunner {
    provider: Arc<dyn Provider>,
    sub_agents: HashMap<String, SubAgentConfig>,
    default_sub_agent: Option<String>,
    compaction_threshold_tokens: Option<u32>,
}

impl SubAgentRunner {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self {
            provider,
            sub_agents: HashMap::new(),
            default_sub_agent: None,
            compaction_threshold_tokens: None,
        }
    }

    /// Register a sub-agent configuration.
    pub fn register(&mut self, config: SubAgentConfig) {
        self.sub_agents.insert(config.name.clone(), config);
    }

    /// Set the default sub-agent name used when no explicit sub-agent
    /// is specified in the input prompt.
    pub fn with_default(mut self, name: impl Into<String>) -> Self {
        self.default_sub_agent = Some(name.into());
        self
    }

    /// Set the compaction token threshold for all sub-agent contexts.
    pub fn with_compaction_threshold(mut self, tokens: u32) -> Self {
        self.compaction_threshold_tokens = Some(tokens);
        self
    }

    /// Parse the input prompt to extract sub-agent name and goal.
    /// Returns (sub_agent_name, goal_text).
    fn parse_input(prompt: &str) -> (Option<String>, String) {
        let mut sub_agent: Option<String> = None;
        let mut goal_start = 0usize;

        for line in prompt.lines() {
            let trimmed = line.trim();
            if let Some(name) = trimmed.strip_prefix("sub_agent:") {
                sub_agent = Some(name.trim().to_string());
            } else if let Some(_goal) = trimmed.strip_prefix("goal:") {
                goal_start = prompt.find("goal:").unwrap_or(0);
                break;
            }
        }

        let goal = if goal_start > 0 {
            prompt[goal_start..].trim().to_string()
        } else {
            // If no structured format, use the full prompt as the goal
            prompt.to_string()
        };

        (sub_agent, goal)
    }

    /// Resolve the sub-agent to use. Falls back to default or error.
    fn resolve_sub_agent(&self, name: Option<String>) -> anyhow::Result<&SubAgentConfig> {
        if let Some(ref n) = name {
            self.sub_agents
                .get(n)
                .ok_or_else(|| anyhow::anyhow!("unknown sub-agent: '{n}'"))
        } else if let Some(ref default) = self.default_sub_agent {
            self.sub_agents
                .get(default)
                .ok_or_else(|| anyhow::anyhow!("default sub-agent '{default}' not registered"))
        } else {
            anyhow::bail!(
                "no sub-agent specified and no default configured. \
                 Use 'sub_agent:<name>' in the prompt or register a default."
            )
        }
    }
}

#[async_trait::async_trait]
impl Runner for SubAgentRunner {
    async fn run_stream(&self, input: RunInput) -> anyhow::Result<RunEventStream> {
        let (tx, rx) = mpsc::channel(64);

        // Parse input: extract sub-agent name and goal
        let (sub_agent_name, goal) = Self::parse_input(&input.prompt);

        // Resolve sub-agent config
        let config = self.resolve_sub_agent(sub_agent_name)?;

        // Clone what the spawned task needs
        let provider = Arc::clone(&self.provider);
        let tools = config.tools.clone();
        let max_steps = config.max_steps;
        let system_prompt = config.system_prompt.clone();
        let compaction_threshold = self.compaction_threshold_tokens;

        // Each sub-agent invocation gets fully independent memory
        let mut memory = Memory::new();

        // Inject the sub-agent's own system prompt
        memory.add_message(Message {
            role: Role::System,
            content: system_prompt,
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });

        info!(
            sub_agent = %config.name,
            goal = %goal,
            max_steps = max_steps,
            "dispatching sub-agent"
        );

        tokio::spawn(async move {
            if let Err(e) = run_sub_agent_loop(
                provider,
                tools,
                max_steps,
                compaction_threshold,
                &mut memory,
                goal,
                &tx,
            )
            .await
            {
                warn!("sub-agent loop error: {e}");
                let _ = tx.send(Err(e)).await;
            }
        });

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}

// ---------------------------------------------------------------------------
// Sub-agent loop — runs in a spawned task with independent context
// ---------------------------------------------------------------------------

async fn run_sub_agent_loop(
    provider: Arc<dyn Provider>,
    tools: Vec<Arc<dyn Tool>>,
    max_steps: usize,
    compaction_threshold: Option<u32>,
    memory: &mut Memory,
    goal: String,
    tx: &mpsc::Sender<anyhow::Result<RunEvent>>,
) -> anyhow::Result<()> {
    let cancel = CancellationToken::new();

    // Add user goal as the first user message
    memory.add_message(Message {
        role: Role::User,
        content: goal.clone(),
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

        info!("sub-agent step {}/{}", step + 1, max_steps);

        // Compact if needed
        if let Some(threshold) = compaction_threshold {
            let all_msgs = memory.get_all();
            let tokens = estimate_tokens(&all_msgs);
            if tokens > threshold {
                let before = tokens;
                match compact_with_provider(provider.as_ref(), &all_msgs).await {
                    Ok(digest) => {
                        memory.compact(digest, None);
                        let after = estimate_tokens(&memory.get_all());
                        info!("compacted {before} → {after} tokens");
                    }
                    Err(e) => {
                        warn!("compaction failed: {e}, using simple fallback");
                        let digest = format!(
                            "Conversation summary ({} messages). Content truncated due to length.",
                            all_msgs.len()
                        );
                        memory.compact(digest, None);
                    }
                }
            }
        }

        // Build tool refs for provider
        let tool_refs: Vec<&dyn Tool> = tools.iter().map(|t| t.as_ref()).collect();
        let messages = memory.get_all();

        // DeepSeek V4 protocol — ValidatedRequest::new fails early with
        // structured violations instead of corrupting provider state
        let validated = dpronix_provider::ValidatedRequest::new(&messages, &tool_refs).map_err(
            |violations| {
                for v in &violations {
                    tracing::error!(?v, "replay invariant violation in sub-agent");
                }
                anyhow::anyhow!(
                    "history replay invariant violated in sub-agent: {} violation(s)",
                    violations.len()
                )
            },
        )?;

        // Stream from provider
        let mut stream = provider.stream(validated).await?;
        let mut text_buf = String::new();
        let mut reasoning_buf = String::new();
        let mut usage: Option<Usage> = None;

        while let Some(chunk) = stream.next().await {
            match chunk? {
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

        // If the model returned text (not tool calls), we are done
        if !text_buf.is_empty() && usage.is_some() {
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
            reasoning_content: if reasoning_buf.is_empty() {
                None
            } else {
                Some(reasoning_buf.clone())
            },
        });
    }

    warn!("sub-agent reached max steps ({max_steps})");
    Err(anyhow::anyhow!(
        "sub-agent reached max steps ({max_steps}) without completing the task"
    ))
}

// ---------------------------------------------------------------------------
// Token estimation helpers
// ---------------------------------------------------------------------------

/// Rough token count estimate from message content length.
fn estimate_tokens(messages: &[Message]) -> u32 {
    let char_count: usize = messages
        .iter()
        .map(|m| m.content.len() + m.reasoning_content.as_ref().map(|r| r.len()).unwrap_or(0))
        .sum();
    (char_count as f32 / CHARS_PER_TOKEN).ceil() as u32
}

/// Build a compaction digest by asking the provider to summarize old messages.
async fn compact_with_provider(
    provider: &dyn Provider,
    messages: &[Message],
) -> anyhow::Result<String> {
    let conversation_text: String = messages
        .iter()
        .map(|m| format!("[{}]: {}", format_role(m.role.clone()), m.content))
        .collect::<Vec<_>>()
        .join("\n\n");

    let summary_prompt = format!(
        "Summarize the following conversation into a concise digest. \
         Keep key decisions, action items, and context. \
         The summary will replace these messages to save context space.\n\n\
         <conversation>
{conversation_text}
</conversation>

\
         Provide a compact summary (under 500 words)."
    );

    let summary_msgs = vec![Message {
        role: Role::User,
        content: summary_prompt,
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }];

    let validated = dpronix_provider::ValidatedRequest::new(&summary_msgs, &[])
        .map_err(|v| anyhow::anyhow!("invariant violation in sub-agent summarize: {:?}", v))?;
    let result = provider.generate(validated).await?;
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

    // --- SubAgentConfig tests ---

    #[test]
    fn config_default_max_steps() {
        let config = SubAgentConfig::new("test", "you are a test agent");
        assert_eq!(config.max_steps, 10);
        assert_eq!(config.name, "test");
        assert!(config.tools.is_empty());
    }

    #[test]
    fn config_custom_max_steps() {
        let config = SubAgentConfig::new("test", "prompt").with_max_steps(5);
        assert_eq!(config.max_steps, 5);
    }

    #[test]
    fn config_zero_max_steps_clamped_to_10() {
        let config = SubAgentConfig::new("test", "prompt").with_max_steps(0);
        assert_eq!(config.max_steps, 10);
    }

    #[test]
    fn config_with_tools() {
        use dpronix_core::{Tool, ToolContext};
        use serde_json::json;

        struct DummyTool;
        #[async_trait::async_trait]
        impl Tool for DummyTool {
            fn schema(&self) -> dpronix_core::ToolSchema {
                dpronix_core::ToolSchema {
                    name: "dummy".to_string(),
                    description: "a dummy tool".to_string(),
                    parameters: json!({}),
                }
            }
            async fn execute(&self, _ctx: &ToolContext, _args: &str) -> anyhow::Result<String> {
                Ok("done".to_string())
            }
        }

        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(DummyTool)];
        let config = SubAgentConfig::new("test", "prompt").with_tools(tools);
        assert_eq!(config.tools.len(), 1);
        assert_eq!(config.tools[0].schema().name, "dummy");
    }

    // --- Input parsing tests ---

    #[test]
    fn parse_input_structured() {
        let prompt = "sub_agent:researcher\ngoal:find all Rust files";
        let (name, goal) = SubAgentRunner::parse_input(prompt);
        assert_eq!(name, Some("researcher".to_string()));
        assert_eq!(goal, "goal:find all Rust files");
    }

    #[test]
    fn parse_input_just_goal() {
        let prompt = "goal:analyze this codebase";
        let (name, goal) = SubAgentRunner::parse_input(prompt);
        assert_eq!(name, None);
        assert_eq!(goal, "goal:analyze this codebase");
    }

    #[test]
    fn parse_input_plain_text() {
        let prompt = "just a plain prompt with no structure";
        let (name, goal) = SubAgentRunner::parse_input(prompt);
        assert_eq!(name, None);
        assert_eq!(goal, prompt);
    }

    #[test]
    fn parse_input_only_sub_agent() {
        let prompt = "sub_agent:reviewer\nsome free text here";
        let (name, goal) = SubAgentRunner::parse_input(prompt);
        assert_eq!(name, Some("reviewer".to_string()));
        assert_eq!(goal, prompt);
    }

    #[test]
    fn parse_input_whitespace_handling() {
        let prompt = "sub_agent:  security-auditor  \ngoal:  scan for vulnerabilities  ";
        let (name, goal) = SubAgentRunner::parse_input(prompt);
        assert_eq!(name, Some("security-auditor".to_string()));
        assert!(goal.starts_with("goal:"));
    }

    // --- SubAgentRunner registration tests ---

    /// A minimal mock Provider for unit tests that don't exercise the agent loop.
    struct MockProvider;
    #[async_trait::async_trait]
    impl Provider for MockProvider {
        async fn generate(
            &self,
            _validated: dpronix_provider::ValidatedRequest<'_>,
        ) -> anyhow::Result<Message> {
            Ok(Message {
                role: Role::Assistant,
                content: "mock".to_string(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            })
        }
    }

    #[test]
    fn resolve_by_name() {
        let provider = Arc::new(MockProvider);
        let mut runner = SubAgentRunner::new(provider);
        runner.register(SubAgentConfig::new("coder", "you are a coder"));
        runner.register(SubAgentConfig::new("reviewer", "you are a reviewer"));

        let resolved = runner.resolve_sub_agent(Some("coder".to_string())).unwrap();
        assert_eq!(resolved.name, "coder");
        assert_eq!(resolved.system_prompt, "you are a coder");
    }

    #[test]
    fn resolve_unknown_errors() {
        let provider = Arc::new(MockProvider);
        let runner = SubAgentRunner::new(provider);

        let err = runner
            .resolve_sub_agent(Some("nonexistent".to_string()))
            .unwrap_err();
        assert!(err.to_string().contains("unknown sub-agent"));
    }

    #[test]
    fn resolve_falls_back_to_default() {
        let provider = Arc::new(MockProvider);
        let mut runner = SubAgentRunner::new(provider).with_default("orchestrator");
        runner.register(SubAgentConfig::new("orchestrator", "you orchestrate"));
        runner.register(SubAgentConfig::new("worker", "you do work"));

        // No explicit sub-agent -> uses default
        let resolved = runner.resolve_sub_agent(None).unwrap();
        assert_eq!(resolved.name, "orchestrator");
    }

    #[test]
    fn resolve_no_default_errors() {
        let provider = Arc::new(MockProvider);
        let runner = SubAgentRunner::new(provider);

        let err = runner.resolve_sub_agent(None).unwrap_err();
        assert!(err.to_string().contains("no sub-agent specified"));
    }

    // --- Token estimation tests ---

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
}

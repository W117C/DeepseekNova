//! Shared test utilities for deepseeknova-agent integration tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use deepseeknova_core::chunk::Chunk;
use deepseeknova_core::{Message, Role, RunInput, RunOutput};
use deepseeknova_provider::{Provider, ValidatedRequest};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// MockProvider — controllable LLM provider with sequential response support
// ---------------------------------------------------------------------------

/// A mock provider that returns pre-defined chunks. Supports multiple
/// sequential responses: each call to `stream()` pops the next response
/// from an internal queue. This prevents infinite loops when the agent
/// re-invokes the provider after tool execution.
pub struct MockProvider {
    /// Queue of responses. Each element is one turn's worth of chunks.
    responses: Mutex<Vec<Vec<Chunk>>>,
    tools: HashMap<String, Arc<dyn deepseeknova_core::Tool>>,
    /// Total number of `generate` + `stream` invocations.
    calls: AtomicUsize,
    /// Last user-message text seen by `stream` (for prompt-content assertions).
    last_prompt: Mutex<Option<String>>,
}

impl MockProvider {
    /// Create a provider that returns the given chunks on every call.
    /// For single-turn scenarios only; for multi-turn use [Self::sequential].
    pub fn new(chunks: Vec<Chunk>) -> Self {
        Self {
            responses: Mutex::new(vec![chunks]),
            tools: HashMap::new(),
            calls: AtomicUsize::new(0),
            last_prompt: Mutex::new(None),
        }
    }

    /// Create a provider that returns different chunks on each successive
    /// call to `stream()`. Useful for testing tool call → tool result cycles.
    pub fn sequential(responses: Vec<Vec<Chunk>>) -> Self {
        Self {
            responses: Mutex::new(responses),
            tools: HashMap::new(),
            calls: AtomicUsize::new(0),
            last_prompt: Mutex::new(None),
        }
    }

    /// Single text response (convenience).
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            responses: Mutex::new(vec![vec![
                Chunk::TextDelta(text.into()),
                Chunk::Usage(deepseeknova_core::chunk::Usage::default()),
                Chunk::Done,
            ]]),
            tools: HashMap::new(),
            calls: AtomicUsize::new(0),
            last_prompt: Mutex::new(None),
        }
    }

    /// Simulate a tool call followed by a final text answer (two-turn).
    pub fn tool_call(tool_name: &str, args: &str, _result: &str, final_text: &str) -> Self {
        let call_id = "call_mock_1";
        Self {
            responses: Mutex::new(vec![
                // Turn 1: tool call
                vec![
                    Chunk::ToolCallStart {
                        id: call_id.to_string(),
                        name: tool_name.to_string(),
                    },
                    Chunk::ToolCallEnd {
                        id: call_id.to_string(),
                        name: tool_name.to_string(),
                        arguments: args.to_string(),
                    },
                    Chunk::Done,
                ],
                // Turn 2: final text
                vec![
                    Chunk::TextDelta(final_text.to_string()),
                    Chunk::Usage(deepseeknova_core::chunk::Usage::default()),
                    Chunk::Done,
                ],
            ]),
            tools: HashMap::new(),
            calls: AtomicUsize::new(0),
            last_prompt: Mutex::new(None),
        }
    }

    /// Register tools that the mock will "use" (return results from).
    pub fn with_tools(mut self, tools: Vec<Arc<dyn deepseeknova_core::Tool>>) -> Self {
        for t in tools {
            self.tools.insert(t.schema().name.clone(), t);
        }
        self
    }

    /// Total number of `generate` + `stream` calls made against this mock.
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Last user-message text observed by `stream` (None if never streamed).
    pub fn last_prompt(&self) -> Option<String> {
        self.last_prompt.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Provider for MockProvider {
    async fn generate(
        &self,
        _validated: ValidatedRequest<'_>,
    ) -> Result<Message, deepseeknova_core::DeepseeknovaError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // 与 stream 同口径记录最后一条 user 消息，便于断言非流式路径的
        // prompt 内容（coordinator think/reflect 走 generate）。
        if let Some(last) = _validated
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
        {
            *self.last_prompt.lock().unwrap() = Some(last.content.clone());
        }
        Ok(Message {
            role: Role::Assistant,
            content: "mock response".to_string(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        })
    }

    async fn stream(
        &self,
        validated: ValidatedRequest<'_>,
    ) -> Result<deepseeknova_core::chunk::ChunkStream, deepseeknova_core::DeepseeknovaError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // Record the last user message for prompt-content assertions.
        if let Some(last) = validated
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
        {
            *self.last_prompt.lock().unwrap() = Some(last.content.clone());
        }
        let mut lock = self.responses.lock().unwrap();
        let chunks = if lock.len() > 1 {
            lock.remove(0)
        } else if lock.len() == 1 {
            // Re-use the last response (single-response / legacy mode)
            lock[0].clone()
        } else {
            // Fallback: empty done
            vec![Chunk::Done]
        };

        let result: Vec<Result<Chunk, deepseeknova_core::DeepseeknovaError>> =
            chunks.into_iter().map(Ok).collect();
        Ok(Box::pin(tokio_stream::iter(result)))
    }
}

// ---------------------------------------------------------------------------
// MockRunner — controllable Runner for downstream tests
// ---------------------------------------------------------------------------

/// A controllable [`Runner`](deepseeknova_core::runner::Runner) that replays a
/// fixed list of `RunEvent`s instead of driving a real model.
pub struct MockRunner {
    events: Vec<deepseeknova_core::RunEvent>,
}

impl MockRunner {
    /// Create a runner that replays the given events verbatim.
    pub fn new(events: Vec<deepseeknova_core::RunEvent>) -> Self {
        Self { events }
    }

    /// Create a runner that emits a single text delta followed by `Done` with
    /// the given text.
    pub fn text(text: &str) -> Self {
        Self {
            events: vec![
                deepseeknova_core::RunEvent::TextDelta(text.to_string()),
                deepseeknova_core::RunEvent::Done(RunOutput {
                    text: text.to_string(),
                    tool_calls: vec![],
                    usage: None,
                }),
            ],
        }
    }
}

#[async_trait::async_trait]
impl deepseeknova_core::Runner for MockRunner {
    async fn run_stream(
        &self,
        _input: RunInput,
    ) -> Result<deepseeknova_core::RunEventStream, deepseeknova_core::DeepseeknovaError> {
        let events: Vec<Result<deepseeknova_core::RunEvent, deepseeknova_core::DeepseeknovaError>> =
            self.events.iter().map(|e| Ok(e.clone())).collect();
        Ok(Box::pin(tokio_stream::iter(events)))
    }
}

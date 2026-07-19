//! Shared test utilities for deepseeknova-agent integration tests.

use deepseeknova_core::chunk::Chunk;
use deepseeknova_core::{Message, Role, RunInput, RunOutput};
use deepseeknova_provider::{Provider, ValidatedRequest};
use std::collections::HashMap;
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
}

impl MockProvider {
    /// Create a provider that returns the given chunks on every call.
    /// For single-turn scenarios only; for multi-turn use [Self::sequential].
    pub fn new(chunks: Vec<Chunk>) -> Self {
        Self {
            responses: Mutex::new(vec![chunks]),
            tools: HashMap::new(),
        }
    }

    /// Create a provider that returns different chunks on each successive
    /// call to `stream()`. Useful for testing tool call → tool result cycles.
    pub fn sequential(responses: Vec<Vec<Chunk>>) -> Self {
        Self {
            responses: Mutex::new(responses),
            tools: HashMap::new(),
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
        }
    }

    /// Register tools that the mock will "use" (return results from).
    pub fn with_tools(mut self, tools: Vec<Arc<dyn deepseeknova_core::Tool>>) -> Self {
        for t in tools {
            self.tools.insert(t.schema().name.clone(), t);
        }
        self
    }
}

#[async_trait::async_trait]
impl Provider for MockProvider {
    async fn generate(&self, _validated: ValidatedRequest<'_>) -> anyhow::Result<Message> {
        Ok(Message {
            role: Role::Assistant,
            content: "mock response".to_string(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        })
    }

    async fn stream(
        &self,
        _validated: ValidatedRequest<'_>,
    ) -> anyhow::Result<deepseeknova_core::chunk::ChunkStream> {
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

        let result: Vec<anyhow::Result<Chunk>> = chunks.into_iter().map(Ok).collect();
        Ok(Box::pin(tokio_stream::iter(result)))
    }
}

// ---------------------------------------------------------------------------
// MockRunner — controllable Runner for downstream tests
// ---------------------------------------------------------------------------

pub struct MockRunner {
    events: Vec<deepseeknova_core::RunEvent>,
}

impl MockRunner {
    pub fn new(events: Vec<deepseeknova_core::RunEvent>) -> Self {
        Self { events }
    }

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
    ) -> anyhow::Result<deepseeknova_core::RunEventStream> {
        let events: Vec<anyhow::Result<deepseeknova_core::RunEvent>> =
            self.events.iter().map(|e| Ok(e.clone())).collect();
        Ok(Box::pin(tokio_stream::iter(events)))
    }
}

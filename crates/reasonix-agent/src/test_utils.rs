//! Shared test utilities for reasonix-agent integration tests.

use reasonix_core::chunk::Chunk;
use reasonix_core::{Message, Role, RunInput, RunOutput};
use reasonix_provider::Provider;
use std::collections::HashMap;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// MockProvider — controllable LLM provider
// ---------------------------------------------------------------------------

pub struct MockProvider {
    chunks: Vec<Chunk>,
    #[allow(dead_code)]
    tools: HashMap<String, Arc<dyn reasonix_core::Tool>>,
}

impl MockProvider {
    #[allow(dead_code)]
    pub fn new(chunks: Vec<Chunk>) -> Self {
        Self {
            chunks,
            tools: HashMap::new(),
        }
    }

    pub fn text(text: impl Into<String>) -> Self {
        Self {
            chunks: vec![
                Chunk::TextDelta(text.into()),
                Chunk::Usage(reasonix_core::chunk::Usage::default()),
                Chunk::Done,
            ],
            tools: HashMap::new(),
        }
    }

    #[allow(dead_code)]
    pub fn tool_call(tool_name: &str, args: &str, _result: &str, final_text: &str) -> Self {
        let call_id = "call_mock_1";
        Self {
            chunks: vec![
                Chunk::ToolCallStart {
                    id: call_id.to_string(),
                    name: tool_name.to_string(),
                },
                Chunk::ToolCallDelta {
                    id: call_id.to_string(),
                    args_delta: args.to_string(),
                },
                Chunk::ToolCallEnd {
                    id: call_id.to_string(),
                    name: tool_name.to_string(),
                    arguments: args.to_string(),
                },
                Chunk::Done,
                Chunk::TextDelta(final_text.to_string()),
                Chunk::Done,
            ],
            tools: HashMap::new(),
        }
    }

    #[allow(dead_code)]
    pub fn with_tools(mut self, tools: Vec<Arc<dyn reasonix_core::Tool>>) -> Self {
        for t in tools {
            self.tools.insert(t.schema().name.clone(), t);
        }
        self
    }
}

#[async_trait::async_trait]
impl Provider for MockProvider {
    async fn generate(
        &self,
        _messages: &[Message],
        _tools: &[&dyn reasonix_core::Tool],
    ) -> anyhow::Result<Message> {
        Ok(Message {
            role: Role::Assistant,
            content: "mock response".to_string(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        })
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _tools: &[&dyn reasonix_core::Tool],
    ) -> anyhow::Result<reasonix_core::chunk::ChunkStream> {
        let chunks: Vec<anyhow::Result<Chunk>> =
            self.chunks.clone().into_iter().map(Ok).collect();
        Ok(Box::pin(tokio_stream::iter(chunks)))
    }
}

// ---------------------------------------------------------------------------
// MockRunner — controllable Runner for downstream tests
// ---------------------------------------------------------------------------

pub struct MockRunner {
    events: Vec<reasonix_core::RunEvent>,
}

impl MockRunner {
    pub fn new(events: Vec<reasonix_core::RunEvent>) -> Self {
        Self { events }
    }

    pub fn text(text: &str) -> Self {
        Self {
            events: vec![
                reasonix_core::RunEvent::TextDelta(text.to_string()),
                reasonix_core::RunEvent::Done(RunOutput {
                    text: text.to_string(),
                    tool_calls: vec![],
                    usage: None,
                }),
            ],
        }
    }
}

#[async_trait::async_trait]
impl reasonix_core::Runner for MockRunner {
    async fn run_stream(
        &self,
        _input: RunInput,
    ) -> anyhow::Result<reasonix_core::RunEventStream> {
        let events: Vec<anyhow::Result<reasonix_core::RunEvent>> =
            self.events.iter().map(|e| Ok(e.clone())).collect();
        Ok(Box::pin(tokio_stream::iter(events)))
    }
}

use crate::chunk::Usage;
use crate::types::ToolCall;
use futures_core::Stream;
use std::pin::Pin;
use tokio_stream::StreamExt;

/// Runner is the execution abstraction. Agent, Planner, Coordinator,
/// SubAgent, and ServerRunner all implement it. Runtime doesn't
/// discriminate between them.
#[async_trait::async_trait]
pub trait Runner: Send + Sync {
    /// Streaming run — returns a stream of events.
    async fn run_stream(&self, input: RunInput) -> anyhow::Result<RunEventStream>;

    /// Convenience: collect the stream into a final output.
    async fn run(&self, input: RunInput) -> anyhow::Result<RunOutput> {
        let mut stream = self.run_stream(input).await?;
        let mut text = String::new();
        let mut tool_calls = Vec::new();
        let mut usage = None;
        while let Some(event) = stream.next().await {
            match event? {
                RunEvent::TextDelta(delta) => text.push_str(&delta),
                RunEvent::ToolCallEnd {
                    id,
                    name,
                    arguments,
                } => {
                    tool_calls.push(ToolCall {
                        id,
                        ty: "function".to_string(),
                        function: crate::types::FunctionCall { name, arguments },
                    });
                }
                RunEvent::Usage(u) => usage = Some(u),
                RunEvent::Done(output) => return Ok(output),
                _ => {}
            }
        }
        Ok(RunOutput {
            text,
            tool_calls,
            usage,
        })
    }
}

#[derive(Debug, Clone)]
pub struct RunInput {
    pub prompt: String,
    pub images: Vec<String>, // data: URLs for vision-capable models
    pub model_override: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RunOutput {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Option<Usage>,
}

pub type RunEventStream = Pin<Box<dyn Stream<Item = anyhow::Result<RunEvent>> + Send>>;

/// RunEvent has no Error variant — errors ride the Stream's Result.
#[derive(Debug, Clone)]
pub enum RunEvent {
    TextDelta(String),
    ReasoningDelta {
        text: String,
        signature: Option<String>,
    },
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallDelta {
        id: String,
        args_delta: String,
    },
    ToolCallEnd {
        id: String,
        name: String,
        arguments: String,
    },
    ToolResult {
        call_id: String,
        result: String,
    },
    Usage(Usage),
    TurnComplete,
    Done(RunOutput),
}

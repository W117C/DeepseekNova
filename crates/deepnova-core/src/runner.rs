use crate::chunk::Usage;
use crate::types::ToolCall;
use futures_core::Stream;
use serde::{Deserialize, Serialize};
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
    ApprovalRequest {
        id: String,
        title: String,
        description: Option<String>,
    },
    Done(RunOutput),
}

// ---------------------------------------------------------------------------
// WireEvent — cross-frontend serializable event format
// Shared by Desktop (Tauri Channel), Serve (SSE), and CLI/TUI
// ---------------------------------------------------------------------------

/// A single event serialized for frontend consumption.
/// The `kind` field discriminates the event type.
/// This is the standard wire format that all frontends consume.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireEvent {
    TextDelta {
        text: String,
    },
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
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        cache_hit_tokens: u32,
        cache_miss_tokens: u32,
        /// DeepSeek-V4 billed reasoning (chain-of-thought) tokens.
        reasoning_tokens: u32,
        session_cache_hit_tokens: u32,
        session_cache_miss_tokens: u32,
    },
    TurnComplete,
    ApprovalRequest {
        id: String,
        title: String,
        description: Option<String>,
    },
    Done {
        text: String,
        usage: Option<WireUsageInfo>,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireUsageInfo {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub cache_hit_tokens: u32,
    pub cache_miss_tokens: u32,
    /// DeepSeek-V4 billed reasoning (chain-of-thought) tokens.
    pub reasoning_tokens: u32,
    pub session_cache_hit_tokens: u32,
    pub session_cache_miss_tokens: u32,
}

impl From<Usage> for WireUsageInfo {
    fn from(u: Usage) -> Self {
        Self {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
            cache_hit_tokens: u.cache_hit_tokens,
            cache_miss_tokens: u.cache_miss_tokens,
            reasoning_tokens: u.reasoning_tokens,
            session_cache_hit_tokens: 0,
            session_cache_miss_tokens: 0,
        }
    }
}

impl From<RunEvent> for WireEvent {
    fn from(event: RunEvent) -> Self {
        match event {
            RunEvent::TextDelta(text) => WireEvent::TextDelta { text },
            RunEvent::ReasoningDelta { text, signature } => {
                WireEvent::ReasoningDelta { text, signature }
            }
            RunEvent::ToolCallStart { id, name } => WireEvent::ToolCallStart { id, name },
            RunEvent::ToolCallDelta { id, args_delta } => {
                WireEvent::ToolCallDelta { id, args_delta }
            }
            RunEvent::ToolCallEnd {
                id,
                name,
                arguments,
            } => WireEvent::ToolCallEnd {
                id,
                name,
                arguments,
            },
            RunEvent::ToolResult { call_id, result } => WireEvent::ToolResult { call_id, result },
            RunEvent::Usage(u) => {
                let usage_info: WireUsageInfo = u.into();
                WireEvent::Usage {
                    prompt_tokens: usage_info.prompt_tokens,
                    completion_tokens: usage_info.completion_tokens,
                    total_tokens: usage_info.total_tokens,
                    cache_hit_tokens: usage_info.cache_hit_tokens,
                    cache_miss_tokens: usage_info.cache_miss_tokens,
                    reasoning_tokens: usage_info.reasoning_tokens,
                    session_cache_hit_tokens: usage_info.session_cache_hit_tokens,
                    session_cache_miss_tokens: usage_info.session_cache_miss_tokens,
                }
            }
            RunEvent::TurnComplete => WireEvent::TurnComplete,
            RunEvent::ApprovalRequest {
                id,
                title,
                description,
            } => WireEvent::ApprovalRequest {
                id,
                title,
                description,
            },
            RunEvent::Done(output) => WireEvent::Done {
                text: output.text,
                usage: output.usage.map(|u| u.into()),
            },
        }
    }
}

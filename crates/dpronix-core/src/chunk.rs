use futures_core::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

/// Usage tracks token accounting for a completion.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub cache_hit_tokens: u32,
    pub cache_miss_tokens: u32,
    pub reasoning_tokens: u32,
}

/// ChunkStream — a stream of Chunks from a provider.
pub type ChunkStream = Pin<Box<dyn Stream<Item = anyhow::Result<Chunk>> + Send>>;

/// Chunk is a single streamed event from a provider.
/// No Error variant — errors ride the Stream's Result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Chunk {
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
    Usage(Usage),
    Done,
}

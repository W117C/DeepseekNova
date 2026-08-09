use crate::DeepseeknovaError;
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

/// Usage tracks token accounting for a completion.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    /// 提示词 token 数。
    pub prompt_tokens: u32,
    /// 生成 token 数。
    pub completion_tokens: u32,
    /// 总 token 数。
    pub total_tokens: u32,
    /// 命中缓存的 token 数。
    pub cache_hit_tokens: u32,
    /// 未命中缓存的 token 数。
    pub cache_miss_tokens: u32,
    /// 推理 token 数。
    pub reasoning_tokens: u32,
}

/// ChunkStream — a stream of Chunks from a provider.
pub type ChunkStream = Pin<Box<dyn Stream<Item = Result<Chunk, DeepseeknovaError>> + Send>>;

/// Chunk is a single streamed event from a provider.
/// No Error variant — errors ride the Stream's Result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Chunk {
    /// 文本增量。
    TextDelta(String),
    /// 推理内容增量。
    ReasoningDelta {
        /// 推理文本片段。
        text: String,
        /// 推理签名（部分模型用于验签）。
        signature: Option<String>,
    },
    /// 工具调用开始事件。
    ToolCallStart {
        /// 工具调用 ID。
        id: String,
        /// 工具/函数名。
        name: String,
    },
    /// 工具调用参数增量。
    ToolCallDelta {
        /// 工具调用 ID。
        id: String,
        /// 参数 JSON 增量片段。
        args_delta: String,
    },
    /// 工具调用结束事件，携带完整参数。
    ToolCallEnd {
        /// 工具调用 ID。
        id: String,
        /// 工具/函数名。
        name: String,
        /// 完整参数 JSON 字符串。
        arguments: String,
    },
    /// 该次完成的 token 用量统计。
    Usage(Usage),
    /// 流结束标记。
    Done,
}

use dpronix_core::Message;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Request types (owned — no borrows across await)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<OpenAIRequestTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    pub stream: bool,
    /// DeepSeek reasoning effort: "low" | "medium" | "high"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Extra body fields passed through to the API.
    /// DeepSeek thinking mode requires: {"thinking": {"type": "enabled"}}
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub extra_body: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct OpenAIRequestTool {
    #[serde(rename = "type")]
    pub ty: String,
    pub function: OpenAIFunction,
}

#[derive(Debug, Serialize)]
pub struct OpenAIFunction {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Non-streaming response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub choices: Vec<Choice>,
    pub usage: Option<ResponseUsage>,
}

#[derive(Debug, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: Message,
    pub finish_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Streaming response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct StreamResponse {
    pub choices: Vec<StreamChoice>,
    pub usage: Option<ResponseUsage>,
}

#[derive(Debug, Deserialize)]
pub struct StreamChoice {
    pub index: u32,
    pub delta: Option<StreamDelta>,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StreamDelta {
    pub content: Option<String>,
    /// DeepSeek reasoning content (streamed in parallel with content)
    #[serde(default)]
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<StreamToolCall>>,
}

#[derive(Debug, Deserialize)]
pub struct StreamToolCall {
    pub index: u32,
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<StreamFunction>,
}

#[derive(Debug, Deserialize)]
pub struct StreamFunction {
    pub name: Option<String>,
    pub arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ResponseUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    /// DeepSeek context cache hit tokens (prompt_cache_hit_tokens in API response)
    #[serde(rename = "prompt_cache_hit_tokens", default)]
    pub cache_hit_tokens: u32,
    /// DeepSeek context cache miss tokens (prompt_cache_miss_tokens in API response)
    #[serde(rename = "prompt_cache_miss_tokens", default)]
    pub cache_miss_tokens: u32,
    /// DeepSeek reasoning-token accounting. The reasoning token count is
    /// nested here under `completion_tokens_details.reasoning_tokens` — these
    /// tokens are billed, so they must be surfaced for cost tracking.
    #[serde(default)]
    pub completion_tokens_details: Option<CompletionTokensDetails>,
}

/// Nested token-accounting detail returned by DeepSeek (and OpenAI o-series).
#[derive(Debug, Deserialize, Default)]
pub struct CompletionTokensDetails {
    /// Tokens spent on the model's internal reasoning chain (billed).
    #[serde(default)]
    pub reasoning_tokens: u32,
}

impl ResponseUsage {
    /// Reasoning token count, or `0` when the provider omits the nested
    /// `completion_tokens_details` object.
    pub fn reasoning_tokens(&self) -> u32 {
        self.completion_tokens_details
            .as_ref()
            .map(|d| d.reasoning_tokens)
            .unwrap_or(0)
    }

    /// Map this provider-native usage into the core [`Usage`] accounting type,
    /// preserving every DeepSeek-specific field (context-cache hit/miss and
    /// reasoning tokens). Centralising this mapping keeps the streaming and
    /// non-streaming paths from drifting apart.
    pub fn to_usage(&self) -> dpronix_core::chunk::Usage {
        dpronix_core::chunk::Usage {
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
            total_tokens: self.total_tokens,
            cache_hit_tokens: self.cache_hit_tokens,
            cache_miss_tokens: self.cache_miss_tokens,
            reasoning_tokens: self.reasoning_tokens(),
        }
    }
}

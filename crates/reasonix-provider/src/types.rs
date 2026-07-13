use reasonix_core::Message;
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
}

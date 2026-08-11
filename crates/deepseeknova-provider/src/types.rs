use deepseeknova_core::Message;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Request types (owned — no borrows across await)
// ---------------------------------------------------------------------------

/// OpenAI-compatible chat-completions request body (owned, no borrows across
/// `await`). Serialised and sent to `/v1/chat/completions`.
#[derive(Debug, Serialize)]
pub struct ChatCompletionRequest {
    /// Model name to call.
    pub model: String,
    /// Conversation history in provider order.
    pub messages: Vec<Message>,
    /// Tools offered to the model; omitted from the body when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<OpenAIRequestTool>>,
    /// Sampling temperature; omitted when `None` (provider default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Upper bound on generated tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Whether the response is streamed token-by-token.
    pub stream: bool,
    /// DeepSeek reasoning effort: "low" | "medium" | "high"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Extra body fields passed through to the API.
    /// DeepSeek thinking mode requires: {"thinking": {"type": "enabled"}}
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub extra_body: Option<serde_json::Value>,
}

/// A single tool exposed to the model in the request `tools` array.
#[derive(Debug, Serialize)]
pub struct OpenAIRequestTool {
    /// Tool kind — always `"function"`.
    #[serde(rename = "type")]
    pub ty: String,
    /// The function schema for this tool.
    pub function: OpenAIFunction,
}

/// JSON function schema describing one callable tool.
#[derive(Debug, Serialize)]
pub struct OpenAIFunction {
    /// Tool name shown to the model.
    pub name: String,
    /// Natural-language description of what the tool does.
    pub description: String,
    /// JSON-schema object describing the tool arguments.
    pub parameters: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Non-streaming response types
// ---------------------------------------------------------------------------

/// Non-streaming chat-completions response body.
#[derive(Debug, Deserialize)]
pub struct ChatCompletionResponse {
    /// Provider-assigned response id.
    pub id: String,
    /// Candidate completions (normally one).
    pub choices: Vec<Choice>,
    /// Token-usage accounting, when reported.
    pub usage: Option<ResponseUsage>,
}

/// One candidate completion in a non-streaming response.
#[derive(Debug, Deserialize)]
pub struct Choice {
    /// Zero-based choice index.
    pub index: u32,
    /// The generated assistant message.
    pub message: Message,
    /// Why generation stopped (e.g. `"stop"`, `"tool_calls"`).
    pub finish_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Streaming response types
// ---------------------------------------------------------------------------

/// One chunk of a streaming chat-completions response.
#[derive(Debug, Deserialize)]
pub struct StreamResponse {
    /// Partial deltas accumulated per choice.
    pub choices: Vec<StreamChoice>,
    /// Final token-usage accounting, if reported.
    pub usage: Option<ResponseUsage>,
}

/// A single streaming choice with its incremental delta.
#[derive(Debug, Deserialize)]
pub struct StreamChoice {
    /// Zero-based choice index.
    pub index: u32,
    /// Incremental delta for this chunk, if any.
    pub delta: Option<StreamDelta>,
    /// Set on the final chunk of a choice.
    pub finish_reason: Option<String>,
}

/// Incremental token delta within a streaming response.
#[derive(Debug, Deserialize)]
pub struct StreamDelta {
    /// Text tokens generated so far.
    pub content: Option<String>,
    /// DeepSeek reasoning content (streamed in parallel with content)
    #[serde(default)]
    pub reasoning_content: Option<String>,
    /// Incremental tool-call deltas.
    #[serde(default)]
    pub tool_calls: Option<Vec<StreamToolCall>>,
}

/// Incremental tool-call fragment streamed across chunks.
#[derive(Debug, Deserialize)]
pub struct StreamToolCall {
    /// Zero-based index identifying the call.
    pub index: u32,
    /// Call id, set on the first fragment.
    pub id: Option<String>,
    /// Function name / arguments deltas.
    #[serde(default)]
    pub function: Option<StreamFunction>,
}

/// Function fragments streamed for a tool call.
#[derive(Debug, Deserialize)]
pub struct StreamFunction {
    /// Function name delta.
    pub name: Option<String>,
    /// JSON-arguments delta (may arrive split across chunks).
    pub arguments: Option<String>,
}

/// Token-usage accounting reported by the provider.
#[derive(Debug, Deserialize)]
pub struct ResponseUsage {
    /// Prompt tokens consumed.
    pub prompt_tokens: u32,
    /// Completion (output) tokens generated.
    pub completion_tokens: u32,
    /// Total tokens billed.
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

    /// Map this provider-native usage into the core `Usage` accounting type,
    /// preserving every DeepSeek-specific field (context-cache hit/miss and
    /// reasoning tokens). Centralising this mapping keeps the streaming and
    /// non-streaming paths from drifting apart.
    pub fn to_usage(&self) -> deepseeknova_core::chunk::Usage {
        deepseeknova_core::chunk::Usage {
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
            total_tokens: self.total_tokens,
            cache_hit_tokens: self.cache_hit_tokens,
            cache_miss_tokens: self.cache_miss_tokens,
            reasoning_tokens: self.reasoning_tokens(),
        }
    }
}

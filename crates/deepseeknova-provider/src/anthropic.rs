use crate::retry::{retry_with_backoff, HttpAttempt};
use crate::tool_cache::ToolSchemaCache;
use crate::{Provider, ValidatedRequest};
use async_trait::async_trait;
use deepseeknova_core::chunk::{Chunk, ChunkStream, Usage};
use deepseeknova_core::types::{FunctionCall, ToolCall};
use deepseeknova_core::{DeepseeknovaError, Message, Role, Tool};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// AnthropicProvider — Anthropic Messages API
// ---------------------------------------------------------------------------

/// [`Provider`] implementation speaking the Anthropic Messages API, used for
/// Claude backends and DeepSeek's Anthropic-compatible endpoint (extended
/// thinking, reasoning effort, prompt caching, streaming, tool calling).
pub struct AnthropicProvider {
    client: Client,
    base_url: String,
    model: String,
    api_key: String,
    api_version: String,
    /// Max retries for transient HTTP failures.
    max_retries: u32,
    /// Enable DeepSeek/Anthropic extended-thinking mode.
    /// Sends `thinking: {"type": "enabled"}` on every request.
    thinking_enabled: bool,
    /// DeepSeek reasoning effort. On the Anthropic-compatible endpoint this is
    /// carried in `output_config: {"effort": "..."}` (the only output_config
    /// sub-field DeepSeek honours).
    reasoning_effort: Option<String>,
    /// Upper bound on generated tokens (Anthropic requires an explicit value).
    max_tokens: u32,
    /// Cache of built Anthropic tool payloads, keyed by tool identity, so the
    /// per-request collect + clone is skipped when the tool set is unchanged.
    tool_cache: ToolSchemaCache<Vec<AnthropicTool>>,
    /// Sampling temperature written into the request body when set.
    temperature: Option<f32>,
    /// Whether to inject `cache_control: {"type":"ephemeral"}` breakpoints
    /// (system block + last tool) so the stable prefix is explicitly cached.
    cache_control: bool,
}

impl AnthropicProvider {
    /// Build a new provider for `base_url` / `model`. The API key is resolved
    /// from the `api_key_env` environment variable at construction time.
    ///
    /// # Errors
    ///
    /// Returns [`DeepseeknovaError`] if the API key environment variable is
    /// unset or the HTTP client cannot be constructed.
    pub fn new(
        base_url: &str,
        model: &str,
        api_key_env: &str,
        timeout_secs: u64,
        max_retries: u32,
    ) -> Result<Self, DeepseeknovaError> {
        let api_key = crate::resolve_api_key_env_value(api_key_env)?;

        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .map_err(|e| {
                DeepseeknovaError::provider(format!("failed to build HTTP client: {e}"))
            })?;

        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            api_key,
            api_version: "2023-06-01".to_string(),
            max_retries,
            thinking_enabled: false,
            reasoning_effort: None,
            max_tokens: 4096,
            tool_cache: ToolSchemaCache::with_capacity(16),
            temperature: None,
            cache_control: true,
        })
    }

    /// Enable DeepSeek/Anthropic extended-thinking mode.
    pub fn with_thinking(mut self, enabled: bool) -> Self {
        self.thinking_enabled = enabled;
        self
    }

    /// Set the DeepSeek reasoning effort ("low" | "medium" | "high" | "max").
    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        self.reasoning_effort = Some(effort.into());
        self
    }

    /// Override the maximum number of generated tokens.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Set the sampling temperature written into every request body.
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Toggle Anthropic prompt-caching breakpoint injection
    /// (`cache_control: {"type":"ephemeral"}` on the system block and the
    /// last tool). Enabled by default; disable to keep the request body
    /// byte-identical to pre-wiring versions.
    pub fn with_cache_control(mut self, enabled: bool) -> Self {
        self.cache_control = enabled;
        self
    }

    /// Build the Anthropic request body shared by both `generate` and
    /// `stream`, injecting DeepSeek thinking mode and reasoning effort.
    fn build_request(
        &self,
        messages: &[Message],
        tools: &[&dyn Tool],
        stream: bool,
    ) -> AnthropicRequest {
        let system = Self::extract_system(messages).map(|s| {
            if self.cache_control {
                // Anthropic prompt-caching 断点：system 以 blocks 数组形式携带
                // cache_control，使稳定前缀（system + tools）显式缓存。
                serde_json::json!([{
                    "type": "text",
                    "text": s,
                    "cache_control": {"type": "ephemeral"}
                }])
            } else {
                serde_json::Value::String(s)
            }
        });
        let conversation: Vec<&Message> =
            messages.iter().filter(|m| m.role != Role::System).collect();

        let thinking = if self.thinking_enabled {
            Some(serde_json::json!({"type": "enabled"}))
        } else {
            None
        };
        let output_config = self
            .reasoning_effort
            .as_ref()
            .map(|effort| serde_json::json!({"effort": effort}));

        AnthropicRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            system,
            messages: conversation
                .iter()
                .map(|m| AnthropicMessage {
                    role: match m.role {
                        Role::User => "user",
                        Role::Assistant => "assistant",
                        // Tool results are sent as user-role `tool_result`
                        // content blocks in the Anthropic Messages API.
                        _ => "user",
                    }
                    .to_string(),
                    content: anthropic_message_content(m, self.thinking_enabled),
                })
                .collect(),
            tools: self.build_tools(tools),
            stream,
            thinking,
            output_config,
            temperature: self.temperature,
        }
    }

    /// Extract the system prompt from messages (Anthropic uses a top-level
    /// system field, not a message with role=system).
    fn extract_system(messages: &[Message]) -> Option<String> {
        messages
            .iter()
            .filter(|m| m.role == Role::System)
            .map(|m| m.content.clone())
            .reduce(|a, b| format!("{a}\n\n{b}"))
    }

    /// Build Anthropic-formatted tools, cached by tool identity so an
    /// unchanged registry skips the per-request collect + clone.
    fn build_tools(&self, tools: &[&dyn Tool]) -> Option<Vec<AnthropicTool>> {
        let cache_control = self.cache_control;
        self.tool_cache.get_or_build(tools, move |ts| {
            let schemas: Vec<_> = ts.iter().map(|t| t.schema()).collect();
            let mut built: Vec<AnthropicTool> = schemas
                .iter()
                .map(|s| AnthropicTool {
                    name: s.name.clone(),
                    description: s.description.clone(),
                    input_schema: s.parameters.clone(),
                    cache_control: None,
                })
                .collect();
            // Anthropic 惯例：最后一个 tool 携带 cache_control 断点，标记
            // 可缓存前缀（system + tools 段）的结尾。
            if cache_control {
                if let Some(last) = built.last_mut() {
                    last.cache_control = Some(serde_json::json!({"type": "ephemeral"}));
                }
            }
            built
        })
    }

    /// Test-only serialisation of a request body so factory-level tests can
    /// assert the request payload (temperature / reasoning effort / thinking)
    /// without a live HTTP round-trip.
    #[cfg(test)]
    pub(crate) fn build_request_json(
        &self,
        messages: &[Message],
        tools: &[&dyn Tool],
        stream: bool,
    ) -> serde_json::Value {
        serde_json::to_value(self.build_request(messages, tools, stream)).unwrap()
    }

    /// Send an HTTP request to the Anthropic API with retry logic.
    async fn send_request(
        &self,
        body: &AnthropicRequest,
        stream: bool,
    ) -> Result<reqwest::Response, DeepseeknovaError> {
        let url = format!("{}/v1/messages", self.base_url);
        info!("POST {} (stream={})", url, stream);

        let max_retries = self.max_retries;
        let client = self.client.clone();
        let api_key = self.api_key.clone();
        let api_version = self.api_version.clone();
        let body = body.clone();

        let result = retry_with_backoff(max_retries, || {
            let client = client.clone();
            let api_key = api_key.clone();
            let api_version = api_version.clone();
            let body = body.clone();
            let url = url.clone();
            Box::pin(async move {
                match client
                    .post(&url)
                    .header("x-api-key", &api_key)
                    .header("anthropic-version", &api_version)
                    .json(&body)
                    .send()
                    .await
                {
                    Ok(response) => {
                        let status = response.status();
                        if status.is_success() {
                            HttpAttempt::Success(response)
                        } else if crate::retry::is_retryable_status(status.as_u16()) {
                            let error_text = response.text().await.unwrap_or_default();
                            HttpAttempt::Retryable(format!("HTTP {status}: {error_text}"))
                        } else {
                            let error_text = response.text().await.unwrap_or_default();
                            HttpAttempt::Fatal(format!("HTTP {status}: {error_text}"))
                        }
                    }
                    Err(e) => {
                        let err_str = e.to_string();
                        if crate::retry::is_retryable_error(&err_str) {
                            HttpAttempt::Retryable(err_str)
                        } else {
                            HttpAttempt::Fatal(err_str)
                        }
                    }
                }
            })
        })
        .await;

        match result {
            HttpAttempt::Success(response) => Ok(response),
            HttpAttempt::Retryable(msg) => Err(DeepseeknovaError::provider_retryable(format!(
                "request failed after retries: {msg}"
            ))),
            HttpAttempt::Fatal(msg) => Err(DeepseeknovaError::provider(msg)),
        }
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn generate(
        &self,
        validated: ValidatedRequest<'_>,
    ) -> Result<Message, DeepseeknovaError> {
        let messages = validated.messages;
        let tools = validated.tools;
        let body = self.build_request(messages, tools, false);
        let response = self.send_request(&body, false).await?;

        let resp: AnthropicResponse = response.json().await.map_err(|e| {
            DeepseeknovaError::provider(format!("failed to parse Anthropic response: {e}"))
        })?;

        // Surface DeepSeek token accounting (context-cache read/write) for this
        // non-streaming path so cache efficiency stays observable.
        if let Some(ref u) = resp.usage {
            info!(
                input_tokens = u.input_tokens,
                output_tokens = u.output_tokens,
                cache_read_input_tokens = u.cache_read_input_tokens,
                cache_creation_input_tokens = u.cache_creation_input_tokens,
                "deepseek-anthropic usage (non-streaming generate)"
            );
        }

        // --- Extract text content ---
        let content: String = resp
            .content
            .iter()
            .filter_map(|c| match c {
                AnthropicContent::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect();

        // --- Extract reasoning (thinking blocks) + opaque signature ---
        // 多轮回放必须原样携带 signature（Anthropic/DeepSeek 兼容端点校验，
        // 缺 signature → HTTP 400）。signature 取首个非空值（thinking 块
        // 级字段，DeepSeek 通常单块）。
        let mut reasoning = String::new();
        let mut reasoning_signature: Option<String> = None;
        for c in &resp.content {
            if let AnthropicContent::Thinking {
                thinking,
                signature,
            } = c
            {
                reasoning.push_str(thinking);
                if reasoning_signature.is_none() {
                    reasoning_signature = signature.clone();
                }
            }
        }

        // --- Extract tool_use blocks (non-streaming path) ---
        let tool_calls: Vec<ToolCall> = resp
            .content
            .iter()
            .filter_map(|c| match c {
                AnthropicContent::ToolUse { name, input } => Some(ToolCall {
                    id: format!(
                        "toolu_{}",
                        hex::encode(
                            &sha2::Sha256::digest(format!("{name}:{input}").as_bytes())[..8]
                        )
                    ),
                    ty: "function".to_string(),
                    function: FunctionCall {
                        name: name.clone(),
                        arguments: input.to_string(),
                    },
                }),
                _ => None,
            })
            .collect();

        Ok(Message {
            role: Role::Assistant,
            content,
            name: None,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            tool_call_id: None,
            reasoning_content: if reasoning.is_empty() {
                None
            } else {
                Some(reasoning)
            },
            reasoning_signature,
        })
    }

    async fn stream(
        &self,
        validated: ValidatedRequest<'_>,
    ) -> Result<ChunkStream, DeepseeknovaError> {
        let messages = validated.messages;
        let tools = validated.tools;
        let body = self.build_request(messages, tools, true);

        // P0.4：body-read 阶段重试（与 `send_request` 内的 header 阶段重试
        // 分离）。网关常在 SSE body 中途断流；仅当**未发出任何内容**时
        // 重试安全——一旦有 text/tool/usage chunk 到达调用方，重试会重复
        // 输出，错误直接上抛（与 OpenAI provider 行为对齐）。
        let mut attempt = 0u32;
        loop {
            let response = self.send_request(&body, true).await?;

            let (tx, mut rx) = mpsc::channel::<Result<Chunk, DeepseeknovaError>>(64);

            tokio::spawn(async move {
                if let Err(e) = stream_anthropic_sse(response, &tx).await {
                    let _ = tx.send(Err(e)).await;
                }
            });

            // Peek at the first item to decide retry vs. hand-off.
            match rx.recv().await {
                Some(Err(e)) if attempt < self.max_retries => {
                    let delay = crate::retry::backoff_duration(attempt + 1);
                    warn!(
                        "anthropic stream disconnected before any content, retry {}/{}: {e:?}",
                        attempt + 1,
                        self.max_retries
                    );
                    drop(rx);
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Some(first) => {
                    // First item arrived — forward it, then the live rest.
                    let rest = tokio_stream::wrappers::ReceiverStream::new(rx);
                    let chained = tokio_stream::iter(std::iter::once(first)).chain(rest);
                    return Ok(Box::pin(chained));
                }
                None => {
                    return Err(DeepseeknovaError::provider(
                        "anthropic stream ended before producing any event",
                    ));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Anthropic API types
// ---------------------------------------------------------------------------

/// Build the Anthropic `content` payload for a core [`Message`].
///
/// * Plain user/assistant text — the existing string form.
/// * Assistant `tool_calls` — `tool_use` content blocks (`id`/`name`/`input`),
///   with the assistant text emitted as a `text` block when structured blocks
///   are present.
/// * Non-empty `reasoning_content` — a `thinking` block, replayed on subsequent
///   turns (DeepSeek V4 returns HTTP 400 if reasoning is dropped while the
///   paired tool calls remain).
/// * [`Role::Tool`] results — `tool_result` content blocks carrying the
///   `tool_use_id` of the call they answer, as a user-role message.
fn anthropic_message_content(m: &Message, thinking_enabled: bool) -> AnthropicMessageContent {
    // Tool results: user-role `tool_result` content blocks in the API.
    // 合成 Tool 消息（压缩摘要等）无 tool_call_id 时回落纯文本，避免孤儿
    // tool_result（空 tool_use_id 会被 Anthropic API 以 HTTP 400 拒绝）。
    if m.role == Role::Tool {
        return match &m.tool_call_id {
            Some(id) => AnthropicMessageContent::Blocks(vec![serde_json::json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": m.content,
            })]),
            None => AnthropicMessageContent::Text(m.content.clone()),
        };
    }

    let mut blocks: Vec<serde_json::Value> = Vec::new();

    // Replay chain-of-thought first (matches the wire order of DeepSeek/
    // Anthropic responses: thinking → text → tool_use)。仅当 extended
    // thinking 已启用时发射 thinking 块：未启用时 Anthropic API 会拒绝
    // thinking 块（HTTP 400）。完整回放仍需 API 返回的 signature（core
    // `Message` 未存储，属已知限制，见 REVIEW.md 2026-08-11 轮次）。
    if thinking_enabled {
        if let Some(reasoning) = m.reasoning_content.as_ref() {
            if !reasoning.is_empty() {
                let mut block = serde_json::json!({
                    "type": "thinking",
                    "thinking": reasoning,
                });
                // T12 收尾：thinking 块原样回放（含 signature）——
                // Anthropic/DeepSeek 兼容端点要求，缺 signature → HTTP 400。
                if let Some(sig) = m.reasoning_signature.as_ref() {
                    block["signature"] = serde_json::Value::String(sig.clone());
                }
                blocks.push(block);
            }
        }
    }

    let has_tool_calls = m
        .tool_calls
        .as_ref()
        .map(|tc| !tc.is_empty())
        .unwrap_or(false);

    if (has_tool_calls || !blocks.is_empty()) && !m.content.is_empty() {
        blocks.push(serde_json::json!({"type": "text", "text": m.content}));
    }

    if has_tool_calls {
        if let Some(tool_calls) = m.tool_calls.as_ref() {
            for tc in tool_calls {
                // `arguments` is a JSON string in core; parse it back into the
                // `input` object the Anthropic API requires.
                let input = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or_else(|_| serde_json::json!({}));
                blocks.push(serde_json::json!({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": tc.function.name,
                    "input": input,
                }));
            }
        }
    }

    if blocks.is_empty() {
        AnthropicMessageContent::Text(m.content.clone())
    } else {
        AnthropicMessageContent::Blocks(blocks)
    }
}

#[derive(Debug, Clone, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<serde_json::Value>,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
    stream: bool,
    /// DeepSeek/Anthropic extended-thinking toggle: `{"type": "enabled"}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<serde_json::Value>,
    /// DeepSeek reasoning effort carrier: `{"effort": "high"}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<serde_json::Value>,
    /// Sampling temperature (Anthropic Messages API supports 0.0–1.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

/// Content payload of an Anthropic message: either a plain string (single text
/// message) or an array of typed content blocks (`tool_use` / `tool_result` /
/// `thinking`). Serialised as a JSON string in the former case and a JSON array
/// in the latter via `#[serde(untagged)]`.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum AnthropicMessageContent {
    /// A single plain-text message (`"content": "hi"`).
    Text(String),
    /// A list of typed content blocks (`"content": [...]`).
    Blocks(Vec<serde_json::Value>),
}

#[derive(Debug, Clone, Serialize)]
struct AnthropicMessage {
    role: String,
    content: AnthropicMessageContent,
}

#[derive(Debug, Clone, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
    /// Anthropic prompt-caching breakpoint; set on the last tool when
    /// cache_control is enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    #[serde(default)]
    content: Vec<AnthropicContent>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicContent {
    #[serde(rename = "text")]
    Text { text: String },
    /// DeepSeek/Anthropic chain-of-thought block。
    ///
    /// `signature`（opaque）：Anthropic/DeepSeek 兼容端点多轮回放 thinking
    /// 块必须原样携带——缺 signature 回放会被 HTTP 400 拒绝
    /// （"The content[].thinking in the thinking mode must be passed back
    /// to the API."）。
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        name: String,
        input: serde_json::Value,
    },
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    #[serde(rename = "input_tokens", default)]
    input_tokens: u32,
    #[serde(rename = "output_tokens", default)]
    output_tokens: u32,
    /// Context-cache read tokens (billed at the discounted cache-hit rate).
    #[serde(rename = "cache_read_input_tokens", default)]
    cache_read_input_tokens: u32,
    /// Context-cache write/creation tokens (billed as cache misses).
    #[serde(rename = "cache_creation_input_tokens", default)]
    cache_creation_input_tokens: u32,
    /// Billed reasoning tokens (thinking mode), nested under
    /// `output_tokens_details` — DeepSeek's Anthropic-compatible endpoint
    /// reports them here (mirrors OpenAI's `completion_tokens_details`).
    #[serde(rename = "output_tokens_details", default)]
    output_tokens_details: Option<AnthropicOutputTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct AnthropicOutputTokensDetails {
    #[serde(rename = "reasoning_tokens", default)]
    reasoning_tokens: u32,
}

// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Anthropic SSE streaming types — supports text + tool_use blocks
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AnthropicSseEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    delta: Option<AnthropicDelta>,
    #[serde(default)]
    content_block: Option<AnthropicContentBlock>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
    /// Present on `message_start`; carries the initial usage (input +
    /// context-cache tokens) that `message_delta` does not repeat.
    #[serde(default)]
    message: Option<AnthropicStreamMessage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicStreamMessage {
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicDelta {
    #[serde(rename = "type", default)]
    #[expect(dead_code)]
    delta_type: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(rename = "partial_json", default)]
    partial_json: Option<String>,
    /// `thinking_delta` payload — DeepSeek chain-of-thought token.
    #[serde(default)]
    thinking: Option<String>,
    /// `signature_delta` payload — opaque signature for the thinking block.
    #[serde(default)]
    signature: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

// ---------------------------------------------------------------------------
// Tool call accumulator for Anthropic streaming
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct AccToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
    started: bool,
}

/// Accumulates streaming usage across events: Anthropic reports input and
/// context-cache tokens on `message_start` and cumulative output tokens on
/// `message_delta`, so they must be merged into a single [`Usage`].
#[derive(Debug, Default)]
struct AnthropicUsageAcc {
    input_tokens: u32,
    output_tokens: u32,
    cache_read: u32,
    cache_creation: u32,
    reasoning_tokens: u32,
}

impl AnthropicUsageAcc {
    fn absorb(&mut self, u: &AnthropicUsage) {
        // Fields are only present when non-zero; prefer the latest non-zero
        // value so a later event that omits input/cache tokens (e.g. the final
        // message_delta) does not clobber values seen at message_start.
        if u.input_tokens > 0 {
            self.input_tokens = u.input_tokens;
        }
        if u.output_tokens > 0 {
            self.output_tokens = u.output_tokens;
        }
        if u.cache_read_input_tokens > 0 {
            self.cache_read = u.cache_read_input_tokens;
        }
        if u.cache_creation_input_tokens > 0 {
            self.cache_creation = u.cache_creation_input_tokens;
        }
        if let Some(ref details) = u.output_tokens_details {
            if details.reasoning_tokens > 0 {
                self.reasoning_tokens = details.reasoning_tokens;
            }
        }
    }

    fn to_usage(&self) -> Usage {
        let prompt_tokens = self.input_tokens + self.cache_read + self.cache_creation;
        Usage {
            prompt_tokens,
            completion_tokens: self.output_tokens,
            total_tokens: prompt_tokens + self.output_tokens,
            cache_hit_tokens: self.cache_read,
            cache_miss_tokens: self.cache_creation,
            reasoning_tokens: self.reasoning_tokens,
        }
    }
}

// ---------------------------------------------------------------------------
// True SSE streaming — bytes_stream() with incremental event parsing
// ---------------------------------------------------------------------------

async fn stream_anthropic_sse(
    response: reqwest::Response,
    tx: &mpsc::Sender<Result<Chunk, DeepseeknovaError>>,
) -> Result<(), DeepseeknovaError> {
    let mut line_bytes: Vec<u8> = Vec::new();
    let mut current_event_type: Option<String> = None;
    let mut current_data: Option<String> = None;
    let mut tool_acc: Vec<AccToolCall> = Vec::new();
    let mut usage_acc = AnthropicUsageAcc::default();

    let mut byte_stream = response.bytes_stream();

    while let Some(chunk_result) = byte_stream.next().await {
        let bytes = chunk_result.map_err(|e| {
            DeepseeknovaError::provider(format!("failed to read chunk from Anthropic stream: {e}"))
        })?;

        for &b in bytes.iter() {
            match b {
                b'\n' => {
                    let line_str = String::from_utf8(line_bytes.clone()).map_err(|e| {
                        DeepseeknovaError::provider(format!("invalid UTF-8 in Anthropic SSE: {e}"))
                    })?;
                    line_bytes.clear();

                    let trimmed = line_str.trim().to_string();
                    if trimmed.is_empty() {
                        if let Some(ref data) = current_data {
                            process_anthropic_event(
                                current_event_type.as_deref(),
                                data,
                                tx,
                                &mut tool_acc,
                                &mut usage_acc,
                            )
                            .await?;
                        }
                        current_event_type = None;
                        current_data = None;
                        continue;
                    }

                    if let Some(event_type) = trimmed.strip_prefix("event: ") {
                        current_event_type = Some(event_type.trim().to_string());
                    } else if let Some(data) = trimmed.strip_prefix("data: ") {
                        current_data = Some(data.trim().to_string());
                    }
                }
                b'\r' => {}
                _ => line_bytes.push(b),
            }
        }
    }

    if let Some(ref data) = current_data {
        process_anthropic_event(
            current_event_type.as_deref(),
            data,
            tx,
            &mut tool_acc,
            &mut usage_acc,
        )
        .await?;
    }

    flush_anthropic_tool_calls(tx, &mut tool_acc).await?;
    let _ = tx.send(Ok(Chunk::Done)).await;

    Ok(())
}

#[allow(clippy::ptr_arg)]
async fn process_anthropic_event(
    _event_type: Option<&str>,
    data: &str,
    tx: &mpsc::Sender<Result<Chunk, DeepseeknovaError>>,
    tool_acc: &mut Vec<AccToolCall>,
    usage_acc: &mut AnthropicUsageAcc,
) -> Result<(), DeepseeknovaError> {
    let Ok(event) = serde_json::from_str::<AnthropicSseEvent>(data) else {
        return Ok(());
    };

    match event.event_type.as_str() {
        "content_block_start" => {
            if let Some(block) = event.content_block {
                if block.block_type == "tool_use" {
                    let idx = event.index.unwrap_or(tool_acc.len());
                    while tool_acc.len() <= idx {
                        tool_acc.push(AccToolCall::default());
                    }
                    tool_acc[idx].id = block.id;
                    tool_acc[idx].name = block.name;
                }
            }
        }
        "content_block_delta" => {
            if let Some(delta) = event.delta {
                let idx = event.index.unwrap_or(0);

                if let Some(text) = delta.text {
                    if !text.is_empty() {
                        let _ = tx.send(Ok(Chunk::TextDelta(text))).await;
                    }
                }

                // DeepSeek chain-of-thought streamed as thinking_delta blocks.
                if let Some(thinking) = delta.thinking {
                    if !thinking.is_empty() {
                        let _ = tx
                            .send(Ok(Chunk::ReasoningDelta {
                                text: thinking,
                                signature: None,
                            }))
                            .await;
                    }
                }

                // signature_delta carries the opaque signature for the block.
                if let Some(signature) = delta.signature {
                    if !signature.is_empty() {
                        let _ = tx
                            .send(Ok(Chunk::ReasoningDelta {
                                text: String::new(),
                                signature: Some(signature),
                            }))
                            .await;
                    }
                }

                if let Some(partial) = delta.partial_json {
                    while tool_acc.len() <= idx {
                        tool_acc.push(AccToolCall::default());
                    }
                    let tc = &mut tool_acc[idx];
                    if !tc.started {
                        tc.started = true;
                        let _ = tx
                            .send(Ok(Chunk::ToolCallStart {
                                id: tc.id.clone().unwrap_or_default(),
                                name: tc.name.clone().unwrap_or_default(),
                            }))
                            .await;
                    }
                    tc.arguments.push_str(&partial);
                    let _ = tx
                        .send(Ok(Chunk::ToolCallDelta {
                            id: tc.id.clone().unwrap_or_default(),
                            args_delta: partial,
                        }))
                        .await;
                }
            }
        }
        "content_block_stop" => {
            let idx = event.index.unwrap_or(0);
            if idx < tool_acc.len() && tool_acc[idx].started {
                let tc = &mut tool_acc[idx];
                let _ = tx
                    .send(Ok(Chunk::ToolCallEnd {
                        id: tc.id.clone().unwrap_or_default(),
                        name: tc.name.clone().unwrap_or_default(),
                        arguments: std::mem::take(&mut tc.arguments),
                    }))
                    .await;
                tc.started = false;
            }
        }
        "message_start" => {
            if let Some(usage) = event.message.and_then(|m| m.usage) {
                usage_acc.absorb(&usage);
            }
        }
        "message_delta" => {
            if let Some(usage) = event.usage {
                usage_acc.absorb(&usage);
            }
            // P0.2：不再在此发射 Usage——message_delta 一轮可能多次，改为
            // message_stop 时发射一次最终合并值（前端无需幂等去重）。
        }
        "message_stop" => {
            // Emit the merged accounting exactly once per turn (input +
            // context-cache + output + reasoning).
            let _ = tx.send(Ok(Chunk::Usage(usage_acc.to_usage()))).await;
        }
        "ping" => {}
        _ => {}
    }

    Ok(())
}

#[allow(clippy::ptr_arg)]
async fn flush_anthropic_tool_calls(
    tx: &mpsc::Sender<Result<Chunk, DeepseeknovaError>>,
    tool_acc: &mut Vec<AccToolCall>,
) -> Result<(), DeepseeknovaError> {
    for tc in tool_acc.iter_mut() {
        if tc.started {
            let _ = tx
                .send(Ok(Chunk::ToolCallEnd {
                    id: tc.id.clone().unwrap_or_default(),
                    name: tc.name.clone().unwrap_or_default(),
                    arguments: std::mem::take(&mut tc.arguments),
                }))
                .await;
            tc.started = false;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{clear_proxy_env, ENV_LOCK};

    /// Minimal tool for request-body assertions (cache_control breakpoints).
    struct TestTool;
    #[async_trait::async_trait]
    impl Tool for TestTool {
        fn schema(&self) -> deepseeknova_core::types::ToolSchema {
            deepseeknova_core::types::ToolSchema {
                name: "test_tool".into(),
                description: "does nothing".into(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }
        async fn execute(
            &self,
            _ctx: &deepseeknova_core::tool::ToolContext,
            _args: &str,
        ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
            Ok("ok".into())
        }
    }

    /// The DeepSeek-anthropic request must carry the thinking toggle and the
    /// reasoning effort (via output_config) when configured.
    #[test]
    fn build_request_injects_thinking_and_effort() {
        std::env::set_var("TEST_ANTHRO_KEY_1", "dummy");
        let provider = AnthropicProvider::new(
            "https://api.deepseek.com/anthropic",
            "deepseek-v4-flash",
            "TEST_ANTHRO_KEY_1",
            30,
            0,
        )
        .unwrap()
        .with_thinking(true)
        .with_reasoning_effort("high");

        let msgs = vec![Message {
            role: Role::User,
            content: "hi".into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        }];
        let body = provider.build_request(&msgs, &[], false);
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["thinking"]["type"], "enabled");
        assert_eq!(v["output_config"]["effort"], "high");
    }

    /// Without thinking enabled, neither the thinking toggle nor output_config
    /// should be serialised.
    #[test]
    fn build_request_omits_thinking_when_disabled() {
        std::env::set_var("TEST_ANTHRO_KEY_2", "dummy");
        let provider = AnthropicProvider::new(
            "https://api.deepseek.com/anthropic",
            "deepseek-v4-flash",
            "TEST_ANTHRO_KEY_2",
            30,
            0,
        )
        .unwrap();

        let msgs = vec![Message {
            role: Role::User,
            content: "hi".into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        }];
        let body = provider.build_request(&msgs, &[], false);
        let v = serde_json::to_value(&body).unwrap();
        assert!(v.get("thinking").is_none());
        assert!(v.get("output_config").is_none());
    }

    /// P0.1：cache_control 断点注入开启时，system 以 blocks 数组携带
    /// ephemeral 标记、tools 末项携带标记（前缀显式缓存）。
    #[test]
    fn build_request_injects_cache_control_breakpoints() {
        std::env::set_var("TEST_ANTHRO_KEY_CACHE", "dummy");
        let provider = AnthropicProvider::new(
            "https://api.deepseek.com/anthropic",
            "deepseek-v4-flash",
            "TEST_ANTHRO_KEY_CACHE",
            30,
            0,
        )
        .unwrap();

        let msgs = vec![
            Message {
                role: Role::System,
                content: "You are a helpful agent.".into(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                reasoning_signature: None,
            },
            Message {
                role: Role::User,
                content: "hi".into(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                reasoning_signature: None,
            },
        ];

        let tool = TestTool;
        let tools: [&dyn Tool; 1] = [&tool];
        let body = provider.build_request(&msgs, &tools, false);
        let v = serde_json::to_value(&body).unwrap();
        let sys = v["system"].as_array().expect("system must be blocks array");
        assert_eq!(sys[0]["type"], "text");
        assert_eq!(sys[0]["cache_control"]["type"], "ephemeral");
        let ts = v["tools"].as_array().expect("tools array present");
        assert_eq!(ts[0]["cache_control"]["type"], "ephemeral");
    }

    /// P0.1：cache_control 关闭时请求体与旧版本逐字节一致——system 为纯
    /// 字符串、tools 无断点字段（向后兼容开关）。
    #[test]
    fn build_request_omits_cache_control_when_disabled() {
        std::env::set_var("TEST_ANTHRO_KEY_CACHE_OFF", "dummy");
        let provider = AnthropicProvider::new(
            "https://api.deepseek.com/anthropic",
            "deepseek-v4-flash",
            "TEST_ANTHRO_KEY_CACHE_OFF",
            30,
            0,
        )
        .unwrap()
        .with_cache_control(false);

        let msgs = vec![Message {
            role: Role::System,
            content: "You are a helpful agent.".into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        }];
        let body = provider.build_request(&msgs, &[], false);
        let v = serde_json::to_value(&body).unwrap();
        assert!(v["system"].is_string(), "system must stay a plain string");
        assert!(v.get("tools").is_none(), "no tools → no breakpoint");
    }

    /// A configured temperature must reach the serialised Anthropic request.
    #[test]
    fn build_request_injects_temperature_when_set() {
        std::env::set_var("TEST_ANTHRO_KEY_TEMP", "dummy");
        let provider = AnthropicProvider::new(
            "https://api.anthropic.com",
            "claude-sonnet-5-20251001",
            "TEST_ANTHRO_KEY_TEMP",
            30,
            0,
        )
        .unwrap()
        .with_temperature(0.2);

        let msgs = vec![Message {
            role: Role::User,
            content: "hi".into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        }];
        let body = provider.build_request(&msgs, &[], false);
        let v = serde_json::to_value(&body).unwrap();
        let temp = v["temperature"].as_f64().unwrap();
        assert!(
            (temp - 0.2).abs() < 1e-6,
            "temperature must reach the body, got {temp}"
        );
    }

    /// Unset temperature must not be serialised.
    #[test]
    fn build_request_omits_temperature_when_unset() {
        std::env::set_var("TEST_ANTHRO_KEY_TEMP2", "dummy");
        let provider = AnthropicProvider::new(
            "https://api.anthropic.com",
            "claude-sonnet-5-20251001",
            "TEST_ANTHRO_KEY_TEMP2",
            30,
            0,
        )
        .unwrap();

        let msgs = vec![Message {
            role: Role::User,
            content: "hi".into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        }];
        let body = provider.build_request(&msgs, &[], false);
        let v = serde_json::to_value(&body).unwrap();
        assert!(v.get("temperature").is_none(), "unset temperature omitted");
    }

    /// A multi-turn assistant(tool_calls) → tool_result sequence with
    /// reasoning must serialise into Anthropic content blocks (tool_use /
    /// tool_result / thinking) instead of being flattened into a string.
    #[test]
    fn build_request_emits_tool_use_tool_result_and_thinking_blocks() {
        std::env::set_var("TEST_ANTHRO_KEY_MULTI", "dummy");
        let provider = AnthropicProvider::new(
            "https://api.deepseek.com/anthropic",
            "deepseek-v4-flash",
            "TEST_ANTHRO_KEY_MULTI",
            30,
            0,
        )
        .unwrap()
        .with_thinking(true);

        let msgs = vec![
            Message {
                role: Role::User,
                content: "杭州今天天气怎么样？".into(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                reasoning_signature: None,
            },
            Message {
                role: Role::Assistant,
                content: "我来查一下。".into(),
                name: None,
                tool_calls: Some(vec![ToolCall {
                    id: "toolu_abc123".into(),
                    ty: "function".into(),
                    function: FunctionCall {
                        name: "get_weather".into(),
                        arguments: r#"{"city":"杭州"}"#.into(),
                    },
                }]),
                tool_call_id: None,
                reasoning_content: Some("用户想查杭州天气，需要调用工具。".into()),
                reasoning_signature: None,
            },
            Message {
                role: Role::Tool,
                content: "24°C，多云".into(),
                name: None,
                tool_calls: None,
                tool_call_id: Some("toolu_abc123".into()),
                reasoning_content: None,
                reasoning_signature: None,
            },
        ];

        let body = provider.build_request(&msgs, &[], false);
        let v = serde_json::to_value(&body).unwrap();
        let messages = v["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);

        // 1. Plain user text keeps the existing string form.
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "杭州今天天气怎么样？");

        // 2. Assistant: thinking → text → tool_use content blocks.
        let asst = &messages[1];
        assert_eq!(asst["role"], "assistant");
        let asst_blocks = asst["content"]
            .as_array()
            .expect("assistant content must be a block array");
        assert_eq!(asst_blocks.len(), 3);
        assert_eq!(asst_blocks[0]["type"], "thinking");
        assert!(
            asst_blocks[0]["thinking"]
                .as_str()
                .unwrap()
                .contains("需要调用工具"),
            "reasoning_content must be replayed as a thinking block"
        );
        assert_eq!(asst_blocks[1]["type"], "text");
        assert_eq!(asst_blocks[1]["text"], "我来查一下。");
        assert_eq!(asst_blocks[2]["type"], "tool_use");
        assert_eq!(asst_blocks[2]["id"], "toolu_abc123");
        assert_eq!(asst_blocks[2]["name"], "get_weather");
        assert_eq!(asst_blocks[2]["input"]["city"], "杭州");

        // 3. Tool result: user role + tool_result block with the call id.
        let tool = &messages[2];
        assert_eq!(tool["role"], "user");
        let tool_blocks = tool["content"]
            .as_array()
            .expect("tool result content must be a block array");
        assert_eq!(tool_blocks[0]["type"], "tool_result");
        assert_eq!(tool_blocks[0]["tool_use_id"], "toolu_abc123");
        assert_eq!(tool_blocks[0]["content"], "24°C，多云");

        std::env::remove_var("TEST_ANTHRO_KEY_MULTI");
    }

    /// An assistant message that only carries tool calls (no reasoning) must
    /// still emit text + tool_use blocks in the canonical order.
    #[test]
    fn build_request_emits_tool_use_without_thinking() {
        std::env::set_var("TEST_ANTHRO_KEY_TOOLONLY", "dummy");
        let provider = AnthropicProvider::new(
            "https://api.anthropic.com",
            "claude-sonnet-5-20251001",
            "TEST_ANTHRO_KEY_TOOLONLY",
            30,
            0,
        )
        .unwrap();

        let msgs = vec![Message {
            role: Role::Assistant,
            content: "calling".into(),
            name: None,
            tool_calls: Some(vec![ToolCall {
                id: "toolu_1".into(),
                ty: "function".into(),
                function: FunctionCall {
                    name: "noop".into(),
                    arguments: r#"{"x":1}"#.into(),
                },
            }]),
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        }];

        let body = provider.build_request(&msgs, &[], false);
        let v = serde_json::to_value(&body).unwrap();
        let asst = &v["messages"][0];
        let blocks = asst["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2, "text + tool_use");
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["input"]["x"], 1);
        std::env::remove_var("TEST_ANTHRO_KEY_TOOLONLY");
    }

    /// P1 回归（code_review 2026-08-11）：无 `tool_call_id` 的合成 Tool 消息
    /// （如压缩摘要 `[Compaction digest]`、`[Compacted turn]`）不得序列化为
    /// 孤儿 `tool_result` 块（空 `tool_use_id` 会被 Anthropic API 以 HTTP 400
    /// 拒绝），须回落纯文本（修复前行为）。
    #[test]
    fn synthetic_tool_message_without_call_id_stays_plain_text() {
        std::env::set_var("TEST_ANTHRO_KEY_SYNTH", "dummy");
        let provider = AnthropicProvider::new(
            "https://api.deepseek.com/anthropic",
            "deepseek-v4-flash",
            "TEST_ANTHRO_KEY_SYNTH",
            30,
            0,
        )
        .unwrap();

        let msgs = vec![Message {
            role: Role::Tool,
            content: "[Compaction digest] 前面的对话已被摘要。".into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        }];

        let body = provider.build_request(&msgs, &[], false);
        let v = serde_json::to_value(&body).unwrap();
        let m = &v["messages"][0];
        assert_eq!(m["role"], "user");
        // 必须是纯文本形态（字符串），而不是带空 tool_use_id 的块数组。
        assert!(
            m["content"].is_string(),
            "synthetic Tool message must stay plain text: {m}"
        );
        assert_eq!(m["content"], "[Compaction digest] 前面的对话已被摘要。");

        std::env::remove_var("TEST_ANTHRO_KEY_SYNTH");
    }

    /// T12 收尾：thinking 块回放必须原样携带 signature（Anthropic/DeepSeek
    /// 兼容端点校验，缺 signature → HTTP 400）。
    #[test]
    fn thinking_block_replays_with_signature() {
        std::env::set_var("TEST_ANTHRO_KEY_SIG", "dummy");
        let provider = AnthropicProvider::new(
            "https://api.deepseek.com/anthropic",
            "deepseek-v4-pro",
            "TEST_ANTHRO_KEY_SIG",
            30,
            0,
        )
        .unwrap()
        .with_thinking(true);

        let msgs = vec![Message {
            role: Role::Assistant,
            content: "done".into(),
            name: None,
            tool_calls: Some(vec![ToolCall {
                id: "toolu_1".into(),
                ty: "function".into(),
                function: FunctionCall {
                    name: "noop".into(),
                    arguments: r#"{"x":1}"#.into(),
                },
            }]),
            tool_call_id: None,
            reasoning_content: Some("thinking text".into()),
            reasoning_signature: Some("sig-abc123".into()),
        }];

        let body = provider.build_request(&msgs, &[], false);
        let v = serde_json::to_value(&body).unwrap();
        let blocks = v["messages"][0]["content"].as_array().unwrap();
        let thinking = blocks
            .iter()
            .find(|b| b["type"] == "thinking")
            .expect("thinking block must be emitted");
        assert_eq!(thinking["thinking"], "thinking text");
        assert_eq!(
            thinking["signature"], "sig-abc123",
            "signature 必须原样回放（缺 signature → API 400）"
        );

        std::env::remove_var("TEST_ANTHRO_KEY_SIG");
    }

    /// T12 收尾：无 signature 的 thinking 块回放时不带 signature 字段
    /// （既有行为不变，OpenAI 等无签名端点不受影响）。
    #[test]
    fn thinking_block_omits_signature_when_absent() {
        std::env::set_var("TEST_ANTHRO_KEY_NOSIG", "dummy");
        let provider = AnthropicProvider::new(
            "https://api.deepseek.com/anthropic",
            "deepseek-v4-pro",
            "TEST_ANTHRO_KEY_NOSIG",
            30,
            0,
        )
        .unwrap()
        .with_thinking(true);

        let msgs = vec![Message {
            role: Role::Assistant,
            content: "done".into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: Some("plain reasoning".into()),
            reasoning_signature: None,
        }];

        let body = provider.build_request(&msgs, &[], false);
        let v = serde_json::to_value(&body).unwrap();
        let blocks = v["messages"][0]["content"].as_array().unwrap();
        let thinking = blocks
            .iter()
            .find(|b| b["type"] == "thinking")
            .expect("thinking block must be emitted");
        assert!(
            thinking.get("signature").is_none(),
            "无 signature 时不得携带空 signature 字段: {thinking}"
        );

        std::env::remove_var("TEST_ANTHRO_KEY_NOSIG");
    }

    /// T12 收尾：响应解析必须保留 thinking 块的 opaque signature
    /// （AnthropicResponse serde 层，非流式路径的提取链起点）。
    #[test]
    fn thinking_block_signature_is_deserialized() {
        let json =
            r#"{"content":[{"type":"thinking","thinking":"t","signature":"s1"}],"usage":null}"#;
        let resp: AnthropicResponse = serde_json::from_str(json).unwrap();
        match &resp.content[0] {
            AnthropicContent::Thinking {
                thinking,
                signature,
            } => {
                assert_eq!(thinking, "t");
                assert_eq!(signature.as_deref(), Some("s1"));
            }
            _ => panic!("expected thinking block"),
        }
    }

    /// build_tools must produce one AnthropicTool per tool and `None` for an
    /// empty set (cache must not change the observable payload).
    #[test]
    fn build_tools_payload_is_correct_and_cached() {
        use deepseeknova_core::tool::ToolContext;
        use deepseeknova_core::types::ToolSchema;
        use deepseeknova_core::Tool;

        struct NoopTool;
        #[async_trait::async_trait]
        impl Tool for NoopTool {
            fn schema(&self) -> ToolSchema {
                ToolSchema {
                    name: "noop".into(),
                    description: "does nothing".into(),
                    parameters: serde_json::json!({"type": "object"}),
                }
            }
            async fn execute(
                &self,
                _ctx: &ToolContext,
                _args: &str,
            ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
                Ok("ok".into())
            }
        }

        std::env::set_var("TEST_ANTHRO_KEY_TOOLS", "dummy");
        let provider = AnthropicProvider::new(
            "https://api.anthropic.com",
            "claude-sonnet-5-20251001",
            "TEST_ANTHRO_KEY_TOOLS",
            30,
            0,
        )
        .unwrap();

        let tool_a = NoopTool;
        let tool_b = NoopTool; // distinct object → distinct identity
        let set_a: Vec<&dyn Tool> = vec![&tool_a];
        let set_ab: Vec<&dyn Tool> = vec![&tool_a, &tool_b];

        let v_a = provider.build_tools(&set_a).expect("tools should be built");
        let v_ab = provider
            .build_tools(&set_ab)
            .expect("tools should be built");
        assert_eq!(v_a.len(), 1);
        assert_eq!(v_ab.len(), 2);
        assert_eq!(v_ab[0].name, "noop");

        let empty: Vec<&dyn Tool> = Vec::new();
        assert!(provider.build_tools(&empty).is_none(), "empty set → None");
        std::env::remove_var("TEST_ANTHRO_KEY_TOOLS");
    }

    /// Non-streaming responses must surface DeepSeek `thinking` content blocks
    /// as reasoning_content, separate from the visible answer text.
    #[test]
    fn response_parses_thinking_block_as_reasoning() {
        let json = r#"{"content":[{"type":"thinking","thinking":"reasoning here"},{"type":"text","text":"answer"}],"usage":{"input_tokens":5,"output_tokens":3}}"#;
        let resp: AnthropicResponse = serde_json::from_str(json).unwrap();
        let text: String = resp
            .content
            .iter()
            .filter_map(|c| match c {
                AnthropicContent::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        let reasoning: String = resp
            .content
            .iter()
            .filter_map(|c| match c {
                AnthropicContent::Thinking { thinking, .. } => Some(thinking.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "answer");
        assert_eq!(reasoning, "reasoning here");
    }

    /// A streaming thinking_delta must be emitted as a ReasoningDelta chunk.
    #[tokio::test]
    async fn stream_thinking_delta_becomes_reasoning() {
        let (tx, mut rx) = mpsc::channel(64);
        let mut tool_acc = Vec::new();
        let mut usage_acc = AnthropicUsageAcc::default();

        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"let me think"}}"#;
        process_anthropic_event(
            Some("content_block_delta"),
            data,
            &tx,
            &mut tool_acc,
            &mut usage_acc,
        )
        .await
        .unwrap();
        drop(tx);

        let mut chunks = Vec::new();
        while let Some(c) = rx.recv().await {
            chunks.push(c.unwrap());
        }
        assert!(
            chunks
                .iter()
                .any(|c| matches!(c, Chunk::ReasoningDelta { text, .. } if text == "let me think")),
            "thinking_delta should become a ReasoningDelta"
        );
    }

    /// Context-cache tokens reported at message_start must be merged with the
    /// output tokens from message_delta into a single Usage chunk.
    #[tokio::test]
    async fn stream_usage_merges_cache_tokens() {
        let (tx, mut rx) = mpsc::channel(64);
        let mut tool_acc = Vec::new();
        let mut usage_acc = AnthropicUsageAcc::default();

        let start = r#"{"type":"message_start","message":{"usage":{"input_tokens":20,"cache_read_input_tokens":80,"cache_creation_input_tokens":10,"output_tokens":1}}}"#;
        process_anthropic_event(
            Some("message_start"),
            start,
            &tx,
            &mut tool_acc,
            &mut usage_acc,
        )
        .await
        .unwrap();

        let delta = r#"{"type":"message_delta","usage":{"output_tokens":42,"output_tokens_details":{"reasoning_tokens":25}}}"#;
        process_anthropic_event(
            Some("message_delta"),
            delta,
            &tx,
            &mut tool_acc,
            &mut usage_acc,
        )
        .await
        .unwrap();

        // P0.2：message_delta 不发射 Usage，message_stop 才发射最终合并值。
        let stop = r#"{"type":"message_stop"}"#;
        process_anthropic_event(
            Some("message_stop"),
            stop,
            &tx,
            &mut tool_acc,
            &mut usage_acc,
        )
        .await
        .unwrap();
        drop(tx);

        let mut usage = None;
        while let Some(c) = rx.recv().await {
            if let Chunk::Usage(u) = c.unwrap() {
                usage = Some(u);
            }
        }
        let u = usage.expect("a Usage chunk should be emitted on message_stop");
        assert_eq!(u.cache_hit_tokens, 80, "cache_read maps to cache_hit");
        assert_eq!(u.cache_miss_tokens, 10, "cache_creation maps to cache_miss");
        assert_eq!(u.completion_tokens, 42, "output tokens from message_delta");
        assert_eq!(u.prompt_tokens, 110, "input + cache read + cache creation");
        assert_eq!(u.total_tokens, 152);
        assert_eq!(
            u.reasoning_tokens, 25,
            "billed reasoning tokens from output_tokens_details must survive"
        );
    }

    // env 串行化锁与代理清除函数见 crate::test_util（跨 openai/embeddings
    // 共享，避免 std::env 并发修改 UB）。

    /// Mock Anthropic SSE 服务器：第 1 次请求返回 200 但 body 中途断开
    /// （模拟网关断流），第 2 次返回完整 SSE。返回 base_url 与请求记录。
    fn serve_anthropic_stream_retry() -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for i in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                loop {
                    let n = stream.read(&mut tmp).unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                tx.send(String::from_utf8_lossy(&buf).to_string()).unwrap();
                if i == 0 {
                    // 声明 1024 字节但 body 一个字节都不写就断开 →
                    // 读 body 第一块即报错（零输出断流）。
                    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 1024\r\n\r\n";
                    let _ = stream.write_all(head.as_bytes());
                    drop(stream);
                } else {
                    let body = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"id\":\"cb1\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\nevent: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":5}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    drop(stream);
                }
            }
        });
        (format!("http://{addr}"), rx)
    }

    /// P0.4：Anthropic 流式在发出任何内容前断流时必须重试；重试后拿到
    /// 完整输出（对齐 OpenAI provider 行为）。
    #[tokio::test]
    async fn stream_retries_zero_output_disconnect() {
        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
        std::env::set_var("TEST_ANTHRO_RETRY_KEY", "dummy");

        let (base, rx) = serve_anthropic_stream_retry();
        let provider =
            AnthropicProvider::new(&base, "deepseek-v4-flash", "TEST_ANTHRO_RETRY_KEY", 30, 2)
                .unwrap();

        let msgs = vec![Message {
            role: Role::User,
            content: "hi".into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        }];
        let tool_refs: Vec<&dyn Tool> = Vec::new();
        let validated = ValidatedRequest::new(&msgs, &tool_refs).unwrap();

        let mut stream = provider.stream(validated).await.unwrap();
        let mut text = String::new();
        while let Some(item) = stream.next().await {
            if let Ok(Chunk::TextDelta(t)) = item {
                text.push_str(&t);
            }
        }

        assert_eq!(text, "hello", "重试后应拿到完整输出");
        assert_eq!(
            rx.try_iter().count(),
            2,
            "应发生 2 次 HTTP 请求（首次断流 + 重试）"
        );
        std::env::remove_var("TEST_ANTHRO_RETRY_KEY");
    }

    /// P0.4：已发出过内容后断流不得重试（重试会重复输出）——错误直接上抛。
    #[tokio::test]
    async fn stream_does_not_retry_after_partial_output() {
        use std::io::{Read, Write};
        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
        std::env::set_var("TEST_ANTHRO_PARTIAL_KEY", "dummy");

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            loop {
                let n = stream.read(&mut tmp).unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            // 先发一个 content_block_delta（有内容），随后断流。
            let partial = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                partial.len(),
                partial
            );
            let _ = stream.write_all(resp.as_bytes());
            drop(stream);
        });

        let provider = AnthropicProvider::new(
            &format!("http://{addr}"),
            "deepseek-v4-flash",
            "TEST_ANTHRO_PARTIAL_KEY",
            30,
            2,
        )
        .unwrap();

        let msgs = vec![Message {
            role: Role::User,
            content: "hi".into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        }];
        let tool_refs: Vec<&dyn Tool> = Vec::new();
        let validated = ValidatedRequest::new(&msgs, &tool_refs).unwrap();

        let mut stream = provider.stream(validated).await.unwrap();
        let mut text = String::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(Chunk::TextDelta(t)) => text.push_str(&t),
                Ok(Chunk::Done) => break,
                Err(e) => {
                    // 已发出部分内容后的断流错误直接上抛，不得重试。
                    assert!(e.to_string().contains("stream"), "got: {e}");
                    break;
                }
                Ok(_) => {}
            }
        }
        assert_eq!(text, "partial", "部分输出必须先到达调用方");
        std::env::remove_var("TEST_ANTHRO_PARTIAL_KEY");
    }
}

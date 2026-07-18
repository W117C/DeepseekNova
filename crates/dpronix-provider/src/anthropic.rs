use crate::{Provider, ProviderError, ValidatedRequest};
use anyhow::Context;
use async_trait::async_trait;
use dpronix_core::chunk::{Chunk, ChunkStream, Usage};
use dpronix_core::{Message, Role, Tool};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::env;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tracing::info;

// ---------------------------------------------------------------------------
// AnthropicProvider — Anthropic Messages API
// ---------------------------------------------------------------------------

pub struct AnthropicProvider {
    client: Client,
    base_url: String,
    model: String,
    api_key: String,
    api_version: String,
    /// Enable DeepSeek/Anthropic extended-thinking mode.
    /// Sends `thinking: {"type": "enabled"}` on every request.
    thinking_enabled: bool,
    /// DeepSeek reasoning effort. On the Anthropic-compatible endpoint this is
    /// carried in `output_config: {"effort": "..."}` (the only output_config
    /// sub-field DeepSeek honours).
    reasoning_effort: Option<String>,
    /// Upper bound on generated tokens (Anthropic requires an explicit value).
    max_tokens: u32,
}

impl AnthropicProvider {
    pub fn new(
        base_url: &str,
        model: &str,
        api_key_env: &str,
        timeout_secs: u64,
        _max_retries: u32,
    ) -> anyhow::Result<Self> {
        let api_key = env::var(api_key_env)
            .with_context(|| format!("environment variable {api_key_env} is not set"))?;

        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            api_key,
            api_version: "2023-06-01".to_string(),
            thinking_enabled: false,
            reasoning_effort: None,
            max_tokens: 4096,
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

    /// Build the Anthropic request body shared by both `generate` and
    /// `stream`, injecting DeepSeek thinking mode and reasoning effort.
    fn build_request(
        &self,
        messages: &[Message],
        tools: &[&dyn Tool],
        stream: bool,
    ) -> AnthropicRequest {
        let system = Self::extract_system(messages);
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
                        _ => "user",
                    }
                    .to_string(),
                    content: m.content.clone(),
                })
                .collect(),
            tools: self.build_tools(tools),
            stream,
            thinking,
            output_config,
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

    /// Build Anthropic-formatted tools.
    fn build_tools(&self, tools: &[&dyn Tool]) -> Option<Vec<AnthropicTool>> {
        let schemas: Vec<_> = tools.iter().map(|t| t.schema()).collect();
        if schemas.is_empty() {
            return None;
        }
        let at: Vec<AnthropicTool> = schemas
            .iter()
            .map(|s| AnthropicTool {
                name: s.name.clone(),
                description: s.description.clone(),
                input_schema: s.parameters.clone(),
            })
            .collect();
        Some(at)
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn generate(&self, validated: ValidatedRequest<'_>) -> anyhow::Result<Message> {
        let messages = validated.messages;
        let tools = validated.tools;
        let url = format!("{}/v1/messages", self.base_url);

        let body = self.build_request(messages, tools, false);

        info!("POST {}", url);

        let response = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", &self.api_version)
            .json(&body)
            .send()
            .await
            .context("failed to send request to Anthropic")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Http {
                status: status.as_u16(),
                body: error_text,
            }
            .into());
        }

        let resp: AnthropicResponse = response
            .json()
            .await
            .context("failed to parse Anthropic response")?;

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

        let content: String = resp
            .content
            .iter()
            .filter_map(|c| match c {
                AnthropicContent::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect();

        // DeepSeek returns its chain-of-thought as `thinking` content blocks.
        // Preserve them as reasoning_content so the replay invariant and any
        // downstream reasoning consumers behave the same as the OpenAI path.
        let reasoning: String = resp
            .content
            .iter()
            .filter_map(|c| match c {
                AnthropicContent::Thinking { thinking } => Some(thinking.clone()),
                _ => None,
            })
            .collect();

        Ok(Message {
            role: Role::Assistant,
            content,
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: if reasoning.is_empty() {
                None
            } else {
                Some(reasoning)
            },
        })
    }

    async fn stream(&self, validated: ValidatedRequest<'_>) -> anyhow::Result<ChunkStream> {
        let messages = validated.messages;
        let tools = validated.tools;
        let url = format!("{}/v1/messages", self.base_url);

        let body = self.build_request(messages, tools, true);

        info!("POST {} (stream)", url);

        let response = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", &self.api_version)
            .json(&body)
            .send()
            .await
            .context("failed to send streaming request to Anthropic")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Http {
                status: status.as_u16(),
                body: error_text,
            }
            .into());
        }

        // True streaming via bytes_stream — same pattern as OpenAI provider
        let (tx, rx) = mpsc::channel::<anyhow::Result<Chunk>>(64);

        tokio::spawn(async move {
            if let Err(e) = stream_anthropic_sse(response, &tx).await {
                let _ = tx.send(Err(e)).await;
            }
        });

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}

// ---------------------------------------------------------------------------
// Anthropic API types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
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
}

#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    #[serde(default)]
    content: Vec<AnthropicContent>,
    #[serde(default)]
    #[allow(dead_code)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicContent {
    #[serde(rename = "text")]
    Text { text: String },
    /// DeepSeek/Anthropic chain-of-thought block.
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "tool_use")]
    #[allow(dead_code)]
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
    }

    fn to_usage(&self) -> Usage {
        let prompt_tokens = self.input_tokens + self.cache_read + self.cache_creation;
        Usage {
            prompt_tokens,
            completion_tokens: self.output_tokens,
            total_tokens: prompt_tokens + self.output_tokens,
            cache_hit_tokens: self.cache_read,
            cache_miss_tokens: self.cache_creation,
            reasoning_tokens: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// True SSE streaming — bytes_stream() with incremental event parsing
// ---------------------------------------------------------------------------

async fn stream_anthropic_sse(
    response: reqwest::Response,
    tx: &mpsc::Sender<anyhow::Result<Chunk>>,
) -> anyhow::Result<()> {
    let mut line_bytes: Vec<u8> = Vec::new();
    let mut current_event_type: Option<String> = None;
    let mut current_data: Option<String> = None;
    let mut tool_acc: Vec<AccToolCall> = Vec::new();
    let mut usage_acc = AnthropicUsageAcc::default();

    let mut byte_stream = response.bytes_stream();

    while let Some(chunk_result) = byte_stream.next().await {
        let bytes = chunk_result.context("failed to read chunk from Anthropic stream")?;

        for &b in bytes.iter() {
            match b {
                b'\n' => {
                    let line_str = String::from_utf8(line_bytes.clone())
                        .map_err(|e| anyhow::anyhow!("invalid UTF-8 in Anthropic SSE: {e}"))?;
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
    tx: &mpsc::Sender<anyhow::Result<Chunk>>,
    tool_acc: &mut Vec<AccToolCall>,
    usage_acc: &mut AnthropicUsageAcc,
) -> anyhow::Result<()> {
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
            // Emit the merged accounting (input + context-cache + output).
            let _ = tx.send(Ok(Chunk::Usage(usage_acc.to_usage()))).await;
        }
        "message_stop" | "ping" => {}
        _ => {}
    }

    Ok(())
}

#[allow(clippy::ptr_arg)]
async fn flush_anthropic_tool_calls(
    tx: &mpsc::Sender<anyhow::Result<Chunk>>,
    tool_acc: &mut Vec<AccToolCall>,
) -> anyhow::Result<()> {
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
        }];
        let body = provider.build_request(&msgs, &[], false);
        let v = serde_json::to_value(&body).unwrap();
        assert!(v.get("thinking").is_none());
        assert!(v.get("output_config").is_none());
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
                AnthropicContent::Thinking { thinking } => Some(thinking.clone()),
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

        let delta = r#"{"type":"message_delta","usage":{"output_tokens":42}}"#;
        process_anthropic_event(
            Some("message_delta"),
            delta,
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
        let u = usage.expect("a Usage chunk should be emitted on message_delta");
        assert_eq!(u.cache_hit_tokens, 80, "cache_read maps to cache_hit");
        assert_eq!(u.cache_miss_tokens, 10, "cache_creation maps to cache_miss");
        assert_eq!(u.completion_tokens, 42, "output tokens from message_delta");
        assert_eq!(u.prompt_tokens, 110, "input + cache read + cache creation");
        assert_eq!(u.total_tokens, 152);
    }
}

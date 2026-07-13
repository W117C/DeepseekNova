use crate::{Provider, ProviderError};
use anyhow::Context;
use async_trait::async_trait;
use reasonix_core::chunk::{Chunk, ChunkStream, Usage};
use reasonix_core::{Message, Role, Tool};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::env;
use std::time::Duration;
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
        })
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
    async fn generate(&self, messages: &[Message], tools: &[&dyn Tool]) -> anyhow::Result<Message> {
        let url = format!("{}/v1/messages", self.base_url);
        let system = Self::extract_system(messages);

        // Filter out system messages from the messages array
        let conversation: Vec<&Message> =
            messages.iter().filter(|m| m.role != Role::System).collect();

        let body = AnthropicRequest {
            model: self.model.clone(),
            max_tokens: 4096,
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
            stream: false,
        };

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

        let content: String = resp
            .content
            .iter()
            .filter_map(|c| match c {
                AnthropicContent::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect();

        Ok(Message {
            role: Role::Assistant,
            content,
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        })
    }

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[&dyn Tool],
    ) -> anyhow::Result<ChunkStream> {
        let url = format!("{}/v1/messages", self.base_url);
        let system = Self::extract_system(messages);

        let conversation: Vec<&Message> =
            messages.iter().filter(|m| m.role != Role::System).collect();

        let body = AnthropicRequest {
            model: self.model.clone(),
            max_tokens: 4096,
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
            stream: true,
        };

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

        let text = response
            .text()
            .await
            .context("failed to read stream body")?;
        let chunks = parse_anthropic_sse(&text);
        Ok(Box::pin(tokio_stream::iter(chunks)))
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
}

// ---------------------------------------------------------------------------
// Anthropic SSE streaming parse
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AnthropicStreamEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    delta: Option<AnthropicStreamDelta>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicStreamDelta {
    #[serde(rename = "type", default)]
    #[allow(dead_code)]
    delta_type: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(rename = "partial_json", default)]
    #[allow(dead_code)]
    partial_json: Option<String>,
}

fn parse_anthropic_sse(text: &str) -> Vec<anyhow::Result<Chunk>> {
    let mut chunks = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(data) = line.strip_prefix("data: ") {
            match serde_json::from_str::<AnthropicStreamEvent>(data) {
                Ok(event) => match event.event_type.as_str() {
                    "content_block_delta" => {
                        if let Some(delta) = event.delta {
                            if let Some(text) = delta.text {
                                if !text.is_empty() {
                                    chunks.push(Ok(Chunk::TextDelta(text)));
                                }
                            }
                        }
                    }
                    "message_delta" => {
                        if let Some(usage) = event.usage {
                            chunks.push(Ok(Chunk::Usage(Usage {
                                prompt_tokens: usage.input_tokens,
                                completion_tokens: usage.output_tokens,
                                total_tokens: usage.input_tokens + usage.output_tokens,
                                cache_hit_tokens: 0,
                                cache_miss_tokens: 0,
                                reasoning_tokens: 0,
                            })));
                        }
                    }
                    "message_stop" => {
                        chunks.push(Ok(Chunk::Done));
                    }
                    _ => {}
                },
                Err(e) => {
                    // Not a stream event — might be a ping or other
                    if data != "[DONE]" && !data.starts_with('{') {
                        continue;
                    }
                    if data != "[DONE]" {
                        chunks.push(Err(anyhow::anyhow!("Anthropic SSE parse error: {e}")));
                    }
                }
            }
        }
    }

    if chunks.is_empty() || !matches!(chunks.last(), Some(Ok(Chunk::Done))) {
        chunks.push(Ok(Chunk::Done));
    }

    chunks
}

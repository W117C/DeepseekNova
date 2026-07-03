use crate::types::{
    ChatCompletionRequest, ChatCompletionResponse, OpenAIFunction, OpenAIRequestTool,
    StreamResponse,
};
use crate::{Provider, ProviderError};
use anyhow::Context;
use async_trait::async_trait;
use reasonix_core::chunk::{Chunk, ChunkStream, Usage};
use reasonix_core::{Message, Tool};
use reqwest::Client;
use std::env;
use std::time::Duration;
use tracing::info;

pub struct OpenAIProvider {
    client: Client,
    base_url: String,
    model: String,
    api_key: String,
}

impl OpenAIProvider {
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
        })
    }

    fn build_tools(&self, tools: &[&dyn Tool]) -> Option<Vec<OpenAIRequestTool>> {
        let schemas: Vec<_> = tools.iter().map(|t| t.schema()).collect();
        if schemas.is_empty() {
            return None;
        }
        let oai_tools: Vec<OpenAIRequestTool> = schemas
            .iter()
            .map(|s| OpenAIRequestTool {
                ty: "function".to_string(),
                function: OpenAIFunction {
                    name: s.name.clone(),
                    description: s.description.clone(),
                    parameters: s.parameters.clone(),
                },
            })
            .collect();
        Some(oai_tools)
    }
}

#[async_trait]
impl Provider for OpenAIProvider {
    async fn generate(&self, messages: &[Message], tools: &[&dyn Tool]) -> anyhow::Result<Message> {
        let url = format!("{}/chat/completions", self.base_url);

        let body = ChatCompletionRequest {
            model: self.model.clone(),
            messages: messages.to_vec(),
            tools: self.build_tools(tools),
            temperature: None,
            max_tokens: None,
            stream: false,
        };

        info!("POST {}", url);

        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .context("failed to send request to provider")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Http {
                status: status.as_u16(),
                body: error_text,
            }
            .into());
        }

        let resp_body: ChatCompletionResponse = response
            .json()
            .await
            .context("failed to parse provider response")?;

        let choice = resp_body
            .choices
            .into_iter()
            .next()
            .ok_or(ProviderError::NoChoices)?;

        Ok(choice.message)
    }

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[&dyn Tool],
    ) -> anyhow::Result<ChunkStream> {
        let url = format!("{}/chat/completions", self.base_url);

        let body = ChatCompletionRequest {
            model: self.model.clone(),
            messages: messages.to_vec(),
            tools: self.build_tools(tools),
            temperature: None,
            max_tokens: None,
            stream: true,
        };

        info!("POST {} (stream)", url);

        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .context("failed to send streaming request")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Http {
                status: status.as_u16(),
                body: error_text,
            }
            .into());
        }

        // Read SSE response and parse into Chunks
        let text = response
            .text()
            .await
            .context("failed to read stream body")?;
        let chunks: Vec<anyhow::Result<Chunk>> = parse_sse_text(&text);
        Ok(Box::pin(tokio_stream::iter(chunks)))
    }
}

/// Parse SSE text into Chunks. Each "data: ..." line becomes a Chunk.
fn parse_sse_text(text: &str) -> Vec<anyhow::Result<Chunk>> {
    let mut chunks = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "data: [DONE]" {
            chunks.push(Ok(Chunk::Done));
            continue;
        }
        if let Some(data) = line.strip_prefix("data: ") {
            match serde_json::from_str::<StreamResponse>(data) {
                Ok(resp) => {
                    if let Some(u) = resp.usage {
                        chunks.push(Ok(Chunk::Usage(Usage {
                            prompt_tokens: u.prompt_tokens,
                            completion_tokens: u.completion_tokens,
                            total_tokens: u.total_tokens,
                            cache_hit_tokens: 0,
                            cache_miss_tokens: 0,
                            reasoning_tokens: 0,
                        })));
                    }
                    for choice in resp.choices {
                        if choice.finish_reason.as_deref() == Some("stop") {
                            continue;
                        }
                        if let Some(delta) = choice.delta {
                            if let Some(content) = delta.content {
                                if !content.is_empty() {
                                    chunks.push(Ok(Chunk::TextDelta(content)));
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    chunks.push(Err(anyhow::anyhow!("SSE parse error: {e}")));
                }
            }
        }
    }

    if chunks.is_empty() {
        chunks.push(Ok(Chunk::Done));
    }

    chunks
}

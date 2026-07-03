use async_trait::async_trait;
use reasonix_core::chunk::ChunkStream;
use reasonix_core::Message;

pub mod anthropic;
pub mod openai;
pub mod types;

// ---------------------------------------------------------------------------
// ProviderError
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },

    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),

    #[error("no choices returned")]
    NoChoices,

    #[error("stream error: {0}")]
    Stream(String),

    #[error("timeout after {0:?}")]
    Timeout(std::time::Duration),

    #[error("rate limited — retry after {retry_after:?}")]
    RateLimited {
        retry_after: Option<std::time::Duration>,
    },

    #[error("authentication failed: {0}")]
    Auth(String),

    #[error("invalid request: {0}")]
    InvalidRequest(String),
}

// ---------------------------------------------------------------------------
// Provider trait — now with streaming
// ---------------------------------------------------------------------------

#[async_trait]
pub trait Provider: Send + Sync {
    /// Non-streaming generate — returns a complete Message.
    async fn generate(
        &self,
        messages: &[Message],
        tools: &[&dyn reasonix_core::Tool],
    ) -> anyhow::Result<Message>;

    /// Streaming generate — returns a ChunkStream.
    /// Default implementation falls back to non-streaming generate()
    /// and emits a single TextDelta + Done.
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[&dyn reasonix_core::Tool],
    ) -> anyhow::Result<ChunkStream> {
        let msg = self.generate(messages, tools).await?;
        use reasonix_core::chunk::Chunk;

        let chunks: Vec<anyhow::Result<Chunk>> =
            vec![Ok(Chunk::TextDelta(msg.content)), Ok(Chunk::Done)];
        Ok(Box::pin(tokio_stream::iter(chunks)))
    }
}

// ---------------------------------------------------------------------------
// Provider-specific helpers
// ---------------------------------------------------------------------------

/// Build an OpenAI-compatible provider from config.
pub mod factory {
    use super::Provider;
    use reasonix_config::ProviderConfig;

    /// Create a Provider from a ProviderConfig.
    pub fn create_provider(cfg: &ProviderConfig) -> anyhow::Result<Box<dyn Provider>> {
        match cfg.kind.as_str() {
            "openai" | "openai-compatible" | "" => {
                let provider = crate::openai::OpenAIProvider::new(
                    cfg.base_url
                        .as_deref()
                        .unwrap_or("https://api.deepseek.com"),
                    cfg.model.as_deref().unwrap_or("deepseek-chat"),
                    cfg.api_key_env.as_deref().unwrap_or("DEEPSEEK_API_KEY"),
                    cfg.timeout_secs,
                    cfg.max_retries,
                )?;
                Ok(Box::new(provider))
            }
            "anthropic" => {
                let provider = crate::anthropic::AnthropicProvider::new(
                    cfg.base_url
                        .as_deref()
                        .unwrap_or("https://api.anthropic.com"),
                    cfg.model.as_deref().unwrap_or("claude-sonnet-5-20251001"),
                    cfg.api_key_env.as_deref().unwrap_or("ANTHROPIC_API_KEY"),
                    cfg.timeout_secs,
                    cfg.max_retries,
                )?;
                Ok(Box::new(provider))
            }
            "ollama" | "local" => {
                let provider = crate::openai::OpenAIProvider::new(
                    cfg.base_url
                        .as_deref()
                        .unwrap_or("http://localhost:11434/v1"),
                    cfg.model.as_deref().unwrap_or("llama3.2"),
                    cfg.api_key_env.as_deref().unwrap_or("OLLAMA"),
                    cfg.timeout_secs,
                    cfg.max_retries,
                )?;
                Ok(Box::new(provider))
            }
            other => anyhow::bail!("unknown provider kind: {other}"),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn unknown_kind_errors() {
            let cfg = reasonix_config::ProviderConfig {
                kind: "nonexistent".to_string(),
                name: "test".to_string(),
                model: Some("gpt-4".to_string()),
                base_url: None,
                api_key: None,
                api_key_env: None,
                timeout_secs: 30,
                max_retries: 3,
                headers: vec![],
            };
            let result = create_provider(&cfg);
            let err = match result {
                Err(e) => e,
                Ok(_) => panic!("expected error for unknown provider kind"),
            };
            assert!(err.to_string().contains("unknown provider kind"));
            assert!(err.to_string().contains("nonexistent"));
        }
    }
}

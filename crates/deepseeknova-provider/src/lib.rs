//! # Provider — LLM provider abstraction
//!
//! Unified interface for LLM backends (OpenAI-compatible, Anthropic).
//! Supports streaming, tool calling, and DeepSeek-V4 thinking mode
//! with reasoning_effort and prompt caching.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use async_trait::async_trait;
use deepseeknova_core::chunk::ChunkStream;
use deepseeknova_core::Message;

pub mod anthropic;
pub mod auto;
pub mod cost;
pub mod embeddings;
pub mod openai;
pub mod retry;
pub mod router;
pub mod scavenge;
pub mod tool_cache;
pub mod types;

// ---------------------------------------------------------------------------
// ValidatedRequest — compile-time guard for DeepSeek V4 invariant
// ---------------------------------------------------------------------------

/// A request payload that has passed [`validate_replay_invariant`][vi] *by
/// construction* — you cannot obtain an instance without the check succeeding.
///
/// This eliminates the "forgot to call validate_* before provider" class of
/// bugs: the `Provider` trait now only accepts `ValidatedRequest`, and the
/// only way to create one is through [`ValidatedRequest::new`], which runs the
/// full invariant check.
///
/// [vi]: deepseeknova_context::history::validate_replay_invariant
#[allow(clippy::manual_non_exhaustive)]
pub struct ValidatedRequest<'a> {
    pub messages: &'a [Message],
    pub tools: &'a [&'a dyn deepseeknova_core::Tool],
    // A private zero-sized field so the outer world cannot destructure or
    // reconstruct this token without calling ::new() (which runs the check).
    _invariant_token: (),
}

impl<'a> ValidatedRequest<'a> {
    /// Validate `messages` against the DeepSeek V4 replay invariant and, if
    /// successful, return a `ValidatedRequest` token.
    ///
    /// # Errors
    ///
    /// Returns a structured error listing the violations if any invariant
    /// rule is broken.
    pub fn new(
        messages: &'a [Message],
        tools: &'a [&'a dyn deepseeknova_core::Tool],
    ) -> Result<Self, Vec<String>> {
        deepseeknova_context::history::validate_replay_invariant(messages).map_err(
            |violations| {
                violations
                    .iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<String>>()
            },
        )?;
        Ok(Self {
            messages,
            tools,
            _invariant_token: (),
        })
    }
}

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
    /// Accepts only a [`ValidatedRequest`] whose messages have already
    /// passed the DeepSeek V4 replay invariant check.
    async fn generate(&self, validated: ValidatedRequest<'_>) -> anyhow::Result<Message>;

    /// Streaming generate — returns a ChunkStream.
    /// Accepts only a [`ValidatedRequest`] — same invariant guarantee.
    /// Default implementation falls back to non-streaming generate()
    /// and emits a single TextDelta + Done.
    async fn stream(&self, validated: ValidatedRequest<'_>) -> anyhow::Result<ChunkStream> {
        let msg = self.generate(validated).await?;
        use deepseeknova_core::chunk::Chunk;

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
    use deepseeknova_config::ProviderConfig;

    /// Create a Provider from a ProviderConfig (no task classification —
    /// reasoning effort falls back to the config default).
    pub fn create_provider(cfg: &ProviderConfig) -> anyhow::Result<Box<dyn Provider>> {
        create_provider_for_task(cfg, None)
    }

    /// Resolve the reasoning effort for a provider given an optional task
    /// classification. Priority (highest → lowest): explicit (n/a here) >
    /// task classification > config factory default > built-in `High`.
    pub fn resolve_effort(
        cfg: &ProviderConfig,
        task_classification: Option<ReasoningEffort>,
    ) -> ReasoningEffort {
        let factory_default = cfg
            .reasoning_effort
            .as_deref()
            .and_then(ReasoningEffort::from_config_str);
        ReasoningEffortResolver::new(factory_default).resolve(None, task_classification)
    }

    /// Create a Provider, applying a task-classification hint to the DeepSeek
    /// reasoning effort. A `Disabled` result switches DeepSeek thinking mode
    /// off so mechanical / low-value calls stop paying for reasoning tokens,
    /// while a `High` classification caps an otherwise `Max` config default
    /// (e.g. per-node executor calls in the two-model coordinator).
    pub fn create_provider_for_task(
        cfg: &ProviderConfig,
        task_classification: Option<ReasoningEffort>,
    ) -> anyhow::Result<Box<dyn Provider>> {
        build_provider(cfg, None, task_classification)
    }

    /// Create a Provider for a specific model name, overriding the provider
    /// config's default `model`. Used by the ModelRouter so one provider
    /// entry can serve multiple named models.
    pub fn create_provider_with_model(
        cfg: &ProviderConfig,
        model_name: &str,
        task_classification: Option<ReasoningEffort>,
    ) -> anyhow::Result<Box<dyn Provider>> {
        create_provider_with_model_temperature(cfg, model_name, None, task_classification)
    }

    /// Create a Provider for a specific model name with an explicit per-model
    /// sampling `temperature` (wired from `[[models]].temperature` by the
    /// [`crate::router::ModelRouter`]). `None` leaves the provider default —
    /// no `temperature` field is written to the request body.
    pub fn create_provider_with_model_temperature(
        cfg: &ProviderConfig,
        model_name: &str,
        temperature: Option<f32>,
        task_classification: Option<ReasoningEffort>,
    ) -> anyhow::Result<Box<dyn Provider>> {
        let mut cfg = cfg.clone();
        cfg.model = Some(model_name.to_string());
        build_provider(&cfg, temperature, task_classification)
    }

    /// Shared construction path: resolves effort/thinking once, applies the
    /// reasoning-effort behaviour uniformly across the anthropic-compatible
    /// kinds, and injects the optional per-model temperature.
    fn build_provider(
        cfg: &ProviderConfig,
        temperature: Option<f32>,
        task_classification: Option<ReasoningEffort>,
    ) -> anyhow::Result<Box<dyn Provider>> {
        let effort = resolve_effort(cfg, task_classification);
        let thinking = effort.thinking();
        let effort_str = effort.effort_str();
        let provider: Box<dyn Provider> = match cfg.kind.as_str() {
            "openai" | "openai-compatible" | "" => {
                let mut p = crate::openai::OpenAIProvider::new(
                    cfg.base_url
                        .as_deref()
                        .unwrap_or("https://api.deepseek.com"),
                    cfg.model.as_deref().unwrap_or("deepseek-v4-flash"),
                    cfg.api_key_env.as_deref().unwrap_or("DEEPSEEK_API_KEY"),
                    cfg.timeout_secs,
                    cfg.max_retries,
                )?
                .with_thinking(thinking)
                .with_extra_body(cfg.extra_body.clone());
                if let Some(effort) = effort_str {
                    p = p.with_reasoning_effort(effort);
                }
                if let Some(temp) = temperature {
                    p = p.with_temperature(temp);
                }
                Box::new(p)
            }
            // Anthropic Messages API. reasoning_effort is carried in the
            // DeepSeek-only `output_config` field, so it is applied exactly
            // like the deepseek-anthropic kind but ONLY when the config
            // explicitly requests one — a bare `kind = "anthropic"` config
            // (real Claude) must stay request-identical to before, because
            // sending `output_config` to api.anthropic.com is rejected (400).
            "anthropic" => Box::new(build_anthropic(cfg, temperature, effort)?),
            // DeepSeek V4 Anthropic-compatible endpoint.
            // Uses the same Anthropic Messages API format but routes to DeepSeek.
            // Reasoning content is natively handled as thinking blocks — no manual
            // reasoning_content passthrough needed (unlike the OpenAI-compatible path).
            "deepseek-anthropic" => Box::new(build_deepseek_anthropic(cfg, temperature, effort)?),
            "ollama" | "local" => {
                let mut p = crate::openai::OpenAIProvider::new(
                    cfg.base_url
                        .as_deref()
                        .unwrap_or("http://localhost:11434/v1"),
                    cfg.model.as_deref().unwrap_or("llama3.2"),
                    cfg.api_key_env.as_deref().unwrap_or("OLLAMA"),
                    cfg.timeout_secs,
                    cfg.max_retries,
                )?
                .with_thinking(cfg.thinking_enabled)
                .with_extra_body(cfg.extra_body.clone());
                if let Some(temp) = temperature {
                    p = p.with_temperature(temp);
                }
                Box::new(p)
            }
            other => anyhow::bail!("unknown provider kind: {other}"),
        };
        Ok(provider)
    }

    /// Anthropic Messages API provider. Reasoning effort (DeepSeek's
    /// `output_config`) is applied only when the config explicitly sets
    /// `reasoning_effort` — a bare `kind = "anthropic"` config stays
    /// request-identical to pre-wiring behaviour (backward compatibility).
    fn build_anthropic(
        cfg: &ProviderConfig,
        temperature: Option<f32>,
        effort: ReasoningEffort,
    ) -> anyhow::Result<crate::anthropic::AnthropicProvider> {
        let mut p = crate::anthropic::AnthropicProvider::new(
            cfg.base_url
                .as_deref()
                .unwrap_or("https://api.anthropic.com"),
            cfg.model.as_deref().unwrap_or("claude-sonnet-5-20251001"),
            cfg.api_key_env.as_deref().unwrap_or("ANTHROPIC_API_KEY"),
            cfg.timeout_secs,
            cfg.max_retries,
        )?;
        if cfg.reasoning_effort.is_some() {
            p = p.with_thinking(effort.thinking());
            if let Some(e) = effort.effort_str() {
                p = p.with_reasoning_effort(e);
            }
        }
        if let Some(temp) = temperature {
            p = p.with_temperature(temp);
        }
        Ok(p)
    }

    /// DeepSeek V4 Anthropic-compatible provider — thinking and reasoning
    /// effort are always applied (existing behaviour, unchanged).
    fn build_deepseek_anthropic(
        cfg: &ProviderConfig,
        temperature: Option<f32>,
        effort: ReasoningEffort,
    ) -> anyhow::Result<crate::anthropic::AnthropicProvider> {
        let mut p = crate::anthropic::AnthropicProvider::new(
            cfg.base_url
                .as_deref()
                .unwrap_or("https://api.deepseek.com/anthropic"),
            cfg.model.as_deref().unwrap_or("deepseek-v4-flash"),
            cfg.api_key_env.as_deref().unwrap_or("DEEPSEEK_API_KEY"),
            cfg.timeout_secs,
            cfg.max_retries,
        )?
        .with_thinking(effort.thinking());
        if let Some(e) = effort.effort_str() {
            p = p.with_reasoning_effort(e);
        }
        if let Some(temp) = temperature {
            p = p.with_temperature(temp);
        }
        Ok(p)
    }

    // -----------------------------------------------------------------------
    // ReasoningEffortResolver — 三层层优先级解析
    // -----------------------------------------------------------------------

    /// Reasoning effort with explicit priority resolution.
    /// Priority (highest → lowest):
    /// 1. Explicit per-call override
    /// 2. Task classification (Swarm/Coordinator)
    /// 3. Provider factory default (config file)
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ReasoningEffort {
        Disabled,
        High,
        Max,
    }

    impl ReasoningEffort {
        /// Parse a config string into an effort level. Recognises disable
        /// aliases and the DeepSeek effort levels; unknown values return
        /// `None` so the resolver falls back to its own default.
        pub fn from_config_str(s: &str) -> Option<Self> {
            match s.trim().to_ascii_lowercase().as_str() {
                "disabled" | "disable" | "none" | "off" | "false" => Some(Self::Disabled),
                "max" | "maximum" => Some(Self::Max),
                "high" | "medium" | "low" => Some(Self::High),
                _ => None,
            }
        }

        /// The `reasoning_effort` string sent to DeepSeek, or `None` when
        /// reasoning is disabled.
        pub fn effort_str(self) -> Option<&'static str> {
            match self {
                Self::Disabled => None,
                Self::High => Some("high"),
                Self::Max => Some("max"),
            }
        }

        /// Whether DeepSeek thinking mode should be enabled for this effort.
        pub fn thinking(self) -> bool {
            !matches!(self, Self::Disabled)
        }
    }

    /// Resolves reasoning effort with three-layer priority:
    /// explicit > task_classification > factory_default.
    #[derive(Debug, Clone, Default)]
    pub struct ReasoningEffortResolver {
        factory_default: Option<ReasoningEffort>,
    }

    impl ReasoningEffortResolver {
        pub fn new(factory_default: Option<ReasoningEffort>) -> Self {
            Self { factory_default }
        }

        pub fn resolve(
            &self,
            explicit: Option<ReasoningEffort>,
            task_classification: Option<ReasoningEffort>,
        ) -> ReasoningEffort {
            explicit
                .or(task_classification)
                .or(self.factory_default)
                .unwrap_or(ReasoningEffort::High)
        }
    }

    #[cfg(test)]
    mod effort_tests {
        use super::*;

        /// 锁定：Trivial 任务分类出的 Disabled 不能被工厂默认的 High 覆盖
        #[test]
        fn task_classification_overrides_factory_default() {
            let resolver = ReasoningEffortResolver::new(Some(ReasoningEffort::High));
            assert_eq!(
                resolver.resolve(None, Some(ReasoningEffort::Disabled)),
                ReasoningEffort::Disabled
            );
        }

        #[test]
        fn factory_default_applies_when_no_task_classification() {
            let resolver = ReasoningEffortResolver::new(Some(ReasoningEffort::High));
            assert_eq!(resolver.resolve(None, None), ReasoningEffort::High);
        }

        #[test]
        fn explicit_effort_wins_over_everything() {
            let resolver = ReasoningEffortResolver::new(Some(ReasoningEffort::High));
            assert_eq!(
                resolver.resolve(Some(ReasoningEffort::Max), Some(ReasoningEffort::Disabled)),
                ReasoningEffort::Max
            );
        }

        fn cfg_with_effort(effort: &str) -> ProviderConfig {
            ProviderConfig {
                kind: "openai".to_string(),
                name: "test".to_string(),
                model: None,
                base_url: None,
                api_key: None,
                api_key_env: None,
                timeout_secs: 30,
                max_retries: 3,
                context_window: None,
                headers: vec![],
                thinking_enabled: false,
                reasoning_effort: Some(effort.to_string()),
                extra_body: None,
            }
        }

        #[test]
        fn config_str_parses_disable_and_levels() {
            assert_eq!(
                ReasoningEffort::from_config_str("disabled"),
                Some(ReasoningEffort::Disabled)
            );
            assert_eq!(
                ReasoningEffort::from_config_str("OFF"),
                Some(ReasoningEffort::Disabled)
            );
            assert_eq!(
                ReasoningEffort::from_config_str("max"),
                Some(ReasoningEffort::Max)
            );
            assert_eq!(
                ReasoningEffort::from_config_str("medium"),
                Some(ReasoningEffort::High)
            );
            assert_eq!(ReasoningEffort::from_config_str("garbage"), None);
        }

        #[test]
        fn disabled_effort_turns_thinking_off() {
            assert!(!ReasoningEffort::Disabled.thinking());
            assert_eq!(ReasoningEffort::Disabled.effort_str(), None);
            assert!(ReasoningEffort::High.thinking());
            assert_eq!(ReasoningEffort::High.effort_str(), Some("high"));
            assert_eq!(ReasoningEffort::Max.effort_str(), Some("max"));
        }

        /// The load-bearing wiring: task classification actually shapes the
        /// effort the factory will apply to a real provider config.
        #[test]
        fn resolve_effort_applies_task_classification() {
            let cfg = cfg_with_effort("max");
            // Planner (no classification) keeps the full config default.
            assert_eq!(resolve_effort(&cfg, None), ReasoningEffort::Max);
            // Executor node capped at High even though config default is Max.
            assert_eq!(
                resolve_effort(&cfg, Some(ReasoningEffort::High)),
                ReasoningEffort::High
            );
            // Trivial task disables reasoning entirely, overriding the default.
            assert_eq!(
                resolve_effort(&cfg, Some(ReasoningEffort::Disabled)),
                ReasoningEffort::Disabled
            );
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn unknown_kind_errors() {
            let cfg = deepseeknova_config::ProviderConfig {
                kind: "nonexistent".to_string(),
                name: "test".to_string(),
                model: Some("gpt-4".to_string()),
                base_url: None,
                api_key: None,
                api_key_env: None,
                timeout_secs: 30,
                max_retries: 3,
                context_window: None,
                headers: vec![],
                thinking_enabled: false,
                reasoning_effort: None,
                extra_body: None,
            };
            let result = create_provider(&cfg);
            let err = match result {
                Err(e) => e,
                Ok(_) => panic!("expected error for unknown provider kind"),
            };
            assert!(err.to_string().contains("unknown provider kind"));
            assert!(err.to_string().contains("nonexistent"));
        }

        fn user_message(content: &str) -> deepseeknova_core::Message {
            deepseeknova_core::Message {
                role: deepseeknova_core::Role::User,
                content: content.to_string(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }
        }

        fn anthropic_cfg(reasoning_effort: Option<&str>) -> ProviderConfig {
            ProviderConfig {
                kind: "anthropic".to_string(),
                name: "claude".to_string(),
                model: Some("claude-sonnet-5-20251001".to_string()),
                base_url: None,
                api_key: None,
                api_key_env: Some("DPNOVA_ANTHRO_EFFORT_KEY".to_string()),
                timeout_secs: 30,
                max_retries: 0,
                context_window: None,
                headers: vec![],
                thinking_enabled: false,
                reasoning_effort: reasoning_effort.map(str::to_string),
                extra_body: None,
            }
        }

        /// reasoning_effort 必须像 deepseek-anthropic 一样在 anthropic 分支生效
        /// （对齐行为），且 temperature 一并写入请求体。
        #[test]
        fn anthropic_branch_applies_reasoning_effort_when_configured() {
            std::env::set_var("DPNOVA_ANTHRO_EFFORT_KEY", "test");
            let cfg = anthropic_cfg(Some("high"));
            let provider = build_anthropic(&cfg, Some(0.3), ReasoningEffort::High).unwrap();
            let msgs = [user_message("hi")];
            let tools: Vec<&dyn deepseeknova_core::Tool> = Vec::new();
            let v = provider.build_request_json(&msgs, &tools, false);
            assert_eq!(v["thinking"]["type"], "enabled", "thinking must be on");
            assert_eq!(
                v["output_config"]["effort"], "high",
                "reasoning_effort must reach the anthropic request body"
            );
            let temp = v["temperature"].as_f64().unwrap();
            assert!(
                (temp - 0.3).abs() < 1e-6,
                "temperature must be applied, got {temp}"
            );
        }

        /// 未配置 reasoning_effort 的裸 anthropic 配置必须保持请求不变
        /// （向后兼容：不向真实 Anthropic 发送 DeepSeek 专属的 output_config）。
        #[test]
        fn anthropic_branch_without_effort_stays_request_identical() {
            std::env::set_var("DPNOVA_ANTHRO_EFFORT_KEY", "test");
            let cfg = anthropic_cfg(None);
            let provider = build_anthropic(&cfg, None, ReasoningEffort::High).unwrap();
            let msgs = [user_message("hi")];
            let tools: Vec<&dyn deepseeknova_core::Tool> = Vec::new();
            let v = provider.build_request_json(&msgs, &tools, false);
            assert!(
                v.get("thinking").is_none(),
                "bare anthropic must not enable thinking"
            );
            assert!(
                v.get("output_config").is_none(),
                "bare anthropic must not send DeepSeek-only output_config"
            );
        }

        /// 工厂的 anthropic 分支（kind="anthropic" + 显式 reasoning_effort）
        /// 能正常构造 provider（走 build_anthropic 接线）。
        #[test]
        fn factory_builds_anthropic_kind_with_effort() {
            std::env::set_var("DPNOVA_ANTHRO_EFFORT_KEY", "test");
            let cfg = anthropic_cfg(Some("high"));
            let provider = create_provider_for_task(&cfg, None);
            assert!(provider.is_ok(), "{:?}", provider.err());
        }
    }
}

#[cfg(test)]
mod factory_tests {
    use deepseeknova_config::ProviderConfig;

    fn provider_cfg() -> ProviderConfig {
        toml::from_str(
            r#"
            name = "deepseek"
            kind = "openai"
            api_key_env = "DPNOVA_TEST_KEY"
        "#,
        )
        .unwrap()
    }

    #[test]
    fn create_provider_with_model_overrides_model() {
        std::env::set_var("DPNOVA_TEST_KEY", "test");
        let cfg = provider_cfg();
        // 构建成功即可 —— 模型名注入路径由 router 缓存键测试进一步覆盖
        let p = crate::factory::create_provider_with_model(&cfg, "my-model", None);
        assert!(p.is_ok(), "{:?}", p.err());
    }
}

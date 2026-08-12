//! # Provider — LLM provider abstraction
//!
//! Unified interface for LLM backends (OpenAI-compatible, Anthropic).
//! Supports streaming, tool calling, and DeepSeek-V4 thinking mode
//! with reasoning_effort and prompt caching.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        clippy::dbg_macro
    )
)]

use async_trait::async_trait;
use deepseeknova_core::chunk::ChunkStream;
use deepseeknova_core::Message;

/// Anthropic Messages API provider: streaming, tool calling, and DeepSeek
/// extended-thinking / reasoning-effort support.
pub mod anthropic;
pub mod auto;
pub mod cost;
pub mod embeddings;
/// OpenAI-compatible chat-completions provider with streaming, tool calling,
/// and DeepSeek thinking-mode passthrough.
pub mod openai;
pub mod retry;
pub mod router;
/// 共享 SSE（Server-Sent Events）行切分，OpenAI 兼容与 Anthropic 端点
/// 流式解析共用的字节级帧切分。
pub mod sse;
pub mod tool_cache;
/// Owned wire request/response types shared by the provider backends.
pub mod types;

/// 读取 API key 环境变量：默认名 `DEEPSEEKNOVA_API_KEY` 缺失时回退旧名
/// `DEEPSEEK_API_KEY`；显式配置的其它变量名缺失时直接报错（不隐式回退，
/// 避免拼错变量名被旧变量悄悄顶替）。
pub(crate) fn resolve_api_key_env_value(
    api_key_env: &str,
) -> Result<String, deepseeknova_core::DeepseeknovaError> {
    match std::env::var(api_key_env) {
        Ok(k) => Ok(k),
        Err(_) if api_key_env == "DEEPSEEKNOVA_API_KEY" => std::env::var("DEEPSEEK_API_KEY")
            .map_err(|_| {
                deepseeknova_core::DeepseeknovaError::config(
                    "environment variable DEEPSEEKNOVA_API_KEY is not set \
                     (legacy DEEPSEEK_API_KEY also unset)"
                        .to_string(),
                )
            }),
        Err(_) => Err(deepseeknova_core::DeepseeknovaError::config(format!(
            "environment variable {api_key_env} is not set"
        ))),
    }
}

/// 测试共享工具：跨模块统一的环境变量串行化锁与代理清除函数。
///
/// `openai` 与 `embeddings` 测试模块都需清除代理环境变量（reqwest 默认尊重
/// HTTP_PROXY/HTTPS_PROXY，会把请求转发到代理，代理无法连本地 mock 端口导致
/// Connect 失败）。`std::env` 的 `set_var`/`remove_var` 非线程安全，并发调用
/// 是 UB（Rust 2024 起标 `unsafe`），因此所有修改 env 或构建 reqwest::Client
/// 的测试必须用同一把锁串行化。
#[cfg(test)]
mod test_util {
    /// 跨模块共享的 env 串行化锁。异步测试用 `.lock().await`，同步测试用
    /// `.blocking_lock()`（在非异步上下文安全；在异步上下文会 panic）。
    pub static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 清除全部代理环境变量。须在 `ENV_LOCK` guard 内调用。
    pub fn clear_proxy_env() {
        for v in &[
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "http_proxy",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            std::env::remove_var(v);
        }
    }
}

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
    /// The conversation history, guaranteed to satisfy the DeepSeek V4 replay
    /// invariant.
    pub messages: &'a [Message],
    /// Tool schemas offered to the provider for the call.
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

/// Errors returned by LLM providers. Carries the retryability category so a
/// caller can decide whether to retry without text-matching messages.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// A non-2xx HTTP response; retryable for status 429 / 5xx.
    #[error("HTTP {status}: {body}")]
    Http {
        /// HTTP status code returned by the provider.
        status: u16,
        /// Raw response body for diagnostics.
        body: String,
    },

    /// The underlying HTTP request failed (network / transport).
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),

    /// The response contained no choices to return.
    #[error("no choices returned")]
    NoChoices,

    /// The response stream failed mid-flight.
    #[error("stream error: {0}")]
    Stream(String),

    /// The request exceeded its configured timeout.
    #[error("timeout after {0:?}")]
    Timeout(std::time::Duration),

    /// The provider rate-limited the request.
    #[error("rate limited — retry after {retry_after:?}")]
    RateLimited {
        /// Server-provided retry-after hint, when available.
        retry_after: Option<std::time::Duration>,
    },

    /// Authentication failed (invalid or missing API key).
    #[error("authentication failed: {0}")]
    Auth(String),

    /// The request itself was malformed or rejected by the provider.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
}

impl ProviderError {
    /// 该错误是否属于可重试类别（限流、超时、5xx、瞬时网络故障）。
    ///
    /// `Http` 按状态码判定（429/5xx）；`Request` / `Stream` 复用
    /// [`crate::retry::is_retryable_error`] 的网络故障词表；`Timeout` /
    /// `RateLimited` 恒为可重试；认证、参数非法、无结果类为确定性错误。
    /// 转换到 [`deepseeknova_core::DeepseeknovaError`] 时该结果写入
    /// `Provider` 变体的 `retryable` 字段。
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Http { status, .. } => crate::retry::is_retryable_status(*status),
            Self::Request(e) => crate::retry::is_retryable_error(&e.to_string()),
            Self::Stream(msg) => crate::retry::is_retryable_error(msg),
            Self::Timeout(_) | Self::RateLimited { .. } => true,
            Self::NoChoices | Self::Auth(_) | Self::InvalidRequest(_) => false,
        }
    }
}

/// 把 [`ProviderError`] 转换为 [`deepseeknova_core::DeepseeknovaError`]。
///
/// orphan rule：impl 放在拥有 `ProviderError` 的本 crate。`?` 可直接把
/// `Result<_, ProviderError>` 用于返回 `Result<_, DeepseeknovaError>` 的函数。
///
/// **重试语义保留**：`ProviderError::RateLimited` 与 `ProviderError::Timeout`
/// 等可重试子类别经 [`ProviderError::is_retryable`] 判定后写入
/// `DeepseeknovaError::Provider.retryable` 字段，`is_retryable()` 直接读取
/// 结构化标志，不依赖消息文本匹配。
impl From<ProviderError> for deepseeknova_core::DeepseeknovaError {
    fn from(err: ProviderError) -> Self {
        let message = err.to_string();
        if err.is_retryable() {
            deepseeknova_core::DeepseeknovaError::provider_retryable(message)
        } else {
            deepseeknova_core::DeepseeknovaError::provider(message)
        }
    }
}

// ---------------------------------------------------------------------------
// Provider trait — now with streaming
// ---------------------------------------------------------------------------

/// A unified LLM backend: non-streaming [`Provider::generate`] and streaming
/// [`Provider::stream`]. All methods accept only a [`ValidatedRequest`], so the
/// DeepSeek V4 replay invariant is enforced by construction.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Non-streaming generate — returns a complete Message.
    /// Accepts only a [`ValidatedRequest`] whose messages have already
    /// passed the DeepSeek V4 replay invariant check.
    async fn generate(
        &self,
        validated: ValidatedRequest<'_>,
    ) -> Result<Message, deepseeknova_core::DeepseeknovaError>;

    /// Streaming generate — returns a ChunkStream.
    /// Accepts only a [`ValidatedRequest`] — same invariant guarantee.
    /// Default implementation falls back to non-streaming generate()
    /// and emits a single TextDelta + Done.
    async fn stream(
        &self,
        validated: ValidatedRequest<'_>,
    ) -> Result<ChunkStream, deepseeknova_core::DeepseeknovaError> {
        let msg = self.generate(validated).await?;
        use deepseeknova_core::chunk::Chunk;

        let chunks: Vec<Result<Chunk, deepseeknova_core::DeepseeknovaError>> =
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
    pub fn create_provider(
        cfg: &ProviderConfig,
    ) -> Result<Box<dyn Provider>, deepseeknova_core::DeepseeknovaError> {
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
    ) -> Result<Box<dyn Provider>, deepseeknova_core::DeepseeknovaError> {
        build_provider(cfg, None, task_classification)
    }

    /// Create a Provider for a specific model name, overriding the provider
    /// config's default `model`. Used by the ModelRouter so one provider
    /// entry can serve multiple named models.
    pub fn create_provider_with_model(
        cfg: &ProviderConfig,
        model_name: &str,
        task_classification: Option<ReasoningEffort>,
    ) -> Result<Box<dyn Provider>, deepseeknova_core::DeepseeknovaError> {
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
    ) -> Result<Box<dyn Provider>, deepseeknova_core::DeepseeknovaError> {
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
    ) -> Result<Box<dyn Provider>, deepseeknova_core::DeepseeknovaError> {
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
                    cfg.api_key_env.as_deref().unwrap_or("DEEPSEEKNOVA_API_KEY"),
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
            other => {
                return Err(deepseeknova_core::DeepseeknovaError::config(format!(
                    "unknown provider kind: {other}"
                )))
            }
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
    ) -> Result<crate::anthropic::AnthropicProvider, deepseeknova_core::DeepseeknovaError> {
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
        p = p.with_cache_control(cfg.cache_control.unwrap_or(true));
        Ok(p)
    }

    /// DeepSeek V4 Anthropic-compatible provider — thinking and reasoning
    /// effort are always applied (existing behaviour, unchanged).
    fn build_deepseek_anthropic(
        cfg: &ProviderConfig,
        temperature: Option<f32>,
        effort: ReasoningEffort,
    ) -> Result<crate::anthropic::AnthropicProvider, deepseeknova_core::DeepseeknovaError> {
        let mut p = crate::anthropic::AnthropicProvider::new(
            cfg.base_url
                .as_deref()
                .unwrap_or("https://api.deepseek.com/anthropic"),
            cfg.model.as_deref().unwrap_or("deepseek-v4-flash"),
            cfg.api_key_env.as_deref().unwrap_or("DEEPSEEKNOVA_API_KEY"),
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
        p = p.with_cache_control(cfg.cache_control.unwrap_or(true));
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
        /// Reasoning disabled (DeepSeek thinking mode off).
        Disabled,
        /// High reasoning effort ("high").
        High,
        /// Maximum reasoning effort ("max").
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
        /// Create a resolver with the given config-level factory default.
        pub fn new(factory_default: Option<ReasoningEffort>) -> Self {
            Self { factory_default }
        }

        /// Resolve the effective effort by priority: explicit > task
        /// classification > factory default, falling back to `High`.
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
                cache_control: None,
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
                cache_control: None,
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
                reasoning_signature: None,
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
                cache_control: None,
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

#[cfg(test)]
mod deepseeknova_error_tests {
    use super::*;

    /// 验证 `From<ProviderError> for DeepseeknovaError` 让 `?` 直接把
    /// `Result<_, ProviderError>` 用于返回 `Result<_, DeepseeknovaError>` 的函数。
    #[test]
    fn provider_error_converts_via_question_mark() {
        fn inner() -> Result<(), ProviderError> {
            Err(ProviderError::NoChoices)
        }
        fn outer() -> Result<(), deepseeknova_core::DeepseeknovaError> {
            inner()?;
            Ok(())
        }
        let err = outer().unwrap_err();
        assert!(
            matches!(err, deepseeknova_core::DeepseeknovaError::Provider { .. }),
            "应映射到 Provider 类别"
        );
    }

    /// 验证 `RateLimited` 与 `Timeout` 子类别的可重试语义在转换后保留：
    /// 结构化 `retryable` 字段正确写入。
    #[test]
    fn rate_limited_and_timeout_preserve_retryability() {
        let rl: deepseeknova_core::DeepseeknovaError =
            ProviderError::RateLimited { retry_after: None }.into();
        assert!(rl.is_retryable(), "RateLimited 应可重试");

        let t: deepseeknova_core::DeepseeknovaError =
            ProviderError::Timeout(std::time::Duration::from_secs(30)).into();
        assert!(t.is_retryable(), "Timeout 应可重试");
    }

    /// 验证 `Auth` 与 `InvalidRequest` 不可重试。
    #[test]
    fn auth_and_invalid_request_are_not_retryable() {
        let a: deepseeknova_core::DeepseeknovaError = ProviderError::Auth("bad key".into()).into();
        assert!(!a.is_retryable(), "Auth 不应可重试");

        let ir: deepseeknova_core::DeepseeknovaError =
            ProviderError::InvalidRequest("bad".into()).into();
        assert!(!ir.is_retryable(), "InvalidRequest 不应可重试");
    }

    /// 验证 HTTP 状态码（429/5xx）与网络故障在转换后保留可重试语义——
    /// 这是原先 `contains("5xx")` 消息匹配无法覆盖的真实消息形态。
    #[test]
    fn http_status_and_network_errors_preserve_retryability() {
        let h429: deepseeknova_core::DeepseeknovaError = ProviderError::Http {
            status: 429,
            body: "rate limit reached".into(),
        }
        .into();
        assert!(h429.is_retryable(), "429 应可重试");

        let h503: deepseeknova_core::DeepseeknovaError = ProviderError::Http {
            status: 503,
            body: "service unavailable".into(),
        }
        .into();
        assert!(h503.is_retryable(), "5xx 应可重试");

        let h404: deepseeknova_core::DeepseeknovaError = ProviderError::Http {
            status: 404,
            body: "not found".into(),
        }
        .into();
        assert!(!h404.is_retryable(), "4xx（除 429）不应可重试");

        let net: deepseeknova_core::DeepseeknovaError =
            ProviderError::Stream("connection refused".into()).into();
        assert!(net.is_retryable(), "网络瞬时故障应可重试");

        let fatal_net: deepseeknova_core::DeepseeknovaError =
            ProviderError::Stream("permission denied".into()).into();
        assert!(!fatal_net.is_retryable(), "确定性网络错误不应可重试");
    }
}

#[cfg(test)]
mod api_key_env_tests {
    use super::resolve_api_key_env_value;

    #[test]
    fn api_key_env_primary_then_legacy_fallback_then_error() {
        let _guard = crate::test_util::ENV_LOCK.blocking_lock();
        std::env::set_var("DEEPSEEKNOVA_API_KEY", "sk-new");
        std::env::set_var("DEEPSEEK_API_KEY", "sk-legacy");

        // 新名优先。
        assert_eq!(
            resolve_api_key_env_value("DEEPSEEKNOVA_API_KEY").unwrap(),
            "sk-new"
        );

        // 新名缺失 → 回退旧名。
        std::env::remove_var("DEEPSEEKNOVA_API_KEY");
        assert_eq!(
            resolve_api_key_env_value("DEEPSEEKNOVA_API_KEY").unwrap(),
            "sk-legacy"
        );

        // 两者都缺 → 报错且指明主变量名。
        std::env::remove_var("DEEPSEEK_API_KEY");
        let err = resolve_api_key_env_value("DEEPSEEKNOVA_API_KEY").unwrap_err();
        assert!(
            err.to_string().contains("DEEPSEEKNOVA_API_KEY"),
            "got: {err}"
        );

        // 显式配置的其它变量名缺失时不隐式回退旧名。
        let err2 = resolve_api_key_env_value("CUSTOM_KEY").unwrap_err();
        assert!(err2.to_string().contains("CUSTOM_KEY"), "got: {err2}");
    }
}

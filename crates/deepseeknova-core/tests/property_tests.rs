//! Property-based tests for the error model.
//!
//! 验证 [`DeepseeknovaError`] 的不变量（`is_retryable` / `redacted`）在
//! 任意输入下成立，覆盖确定性变体的 retryable 语义与脱敏的泄漏防护。

use deepseeknova_core::DeepseeknovaError;
use proptest::prelude::*;

/// `Cancelled` 是确定性状态，任意条件下都不可重试。
#[test]
fn cancelled_never_retryable() {
    assert!(!DeepseeknovaError::Cancelled.is_retryable());
}

proptest! {
    /// `Config` 错误确定性，任意消息不可重试。
    #[test]
    fn config_never_retryable(msg in ".{0,100}") {
        prop_assert!(!DeepseeknovaError::config(msg).is_retryable());
    }

    /// `Tool` 错误确定性，任意消息不可重试。
    #[test]
    fn tool_never_retryable(msg in ".{0,100}") {
        prop_assert!(!DeepseeknovaError::tool(msg).is_retryable());
    }

    /// `Runner` 错误确定性，任意消息不可重试。
    #[test]
    fn runner_never_retryable(msg in ".{0,100}") {
        prop_assert!(!DeepseeknovaError::runner(msg).is_retryable());
    }

    /// `Permission` 错误确定性，任意自由格式消息不可重试。
    #[test]
    fn permission_free_form_never_retryable(msg in ".{0,100}") {
        prop_assert!(!DeepseeknovaError::permission(msg).is_retryable());
    }

    /// `Agent` 错误确定性，任意自由格式消息不可重试。
    #[test]
    fn agent_free_form_never_retryable(msg in ".{0,100}") {
        prop_assert!(!DeepseeknovaError::agent(msg).is_retryable());
    }

    /// `Storage` 自由格式错误（source=None）确定性，任意消息不可重试。
    #[test]
    fn storage_free_form_never_retryable(msg in ".{0,100}") {
        prop_assert!(!DeepseeknovaError::storage(msg).is_retryable());
    }

    /// `Provider` 的 `retryable` 标志是权威来源：`true` → 可重试，
    /// `false` → 不可重试，不依赖消息文本。
    #[test]
    fn provider_retryable_flag_is_authoritative(
        msg in ".{0,100}",
        retryable in any::<bool>()
    ) {
        let err = if retryable {
            DeepseeknovaError::provider_retryable(msg)
        } else {
            DeepseeknovaError::provider(msg)
        };
        prop_assert_eq!(err.is_retryable(), retryable);
    }

    /// `Io` 错误的 retryable 仅对瞬时 ErrorKind 为 true，对确定性
    /// ErrorKind（NotFound / PermissionDenied / AlreadyExists）为 false。
    #[test]
    fn io_retryable_only_for_transient_kinds(
        kind in prop::sample::select(vec![
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::AlreadyExists,
            std::io::ErrorKind::TimedOut,
            std::io::ErrorKind::ConnectionRefused,
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::BrokenPipe,
            std::io::ErrorKind::NetworkUnreachable,
        ])
    ) {
        let err = DeepseeknovaError::from(std::io::Error::new(kind, "test"));
        let transient = matches!(
            kind,
            std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::NetworkUnreachable
        );
        prop_assert_eq!(err.is_retryable(), transient, "kind={:?}", kind);
    }

    // ── redacted 不变量 ─────────────────────────────────────────────

    /// `sk-` 前缀的 API key 在任意后缀下必须被脱敏。
    #[test]
    fn redacted_never_leaks_sk_key(suffix in "[A-Za-z0-9_\\-]{20,50}") {
        let key = format!("sk-{suffix}");
        let err = DeepseeknovaError::provider(key.clone());
        let redacted = err.redacted();
        prop_assert!(
            !redacted.contains(&key),
            "sk- key leaked into redacted output: {redacted}"
        );
        prop_assert!(redacted.contains("[REDACTED]"));
    }

    /// `Bearer <token>` 形式的认证头在任意 token 下必须被脱敏。
    #[test]
    fn redacted_never_leaks_bearer_token(token in "[A-Za-z0-9_\\-\\.]{20,50}") {
        let bearer = format!("Bearer {token}");
        let err = DeepseeknovaError::provider(bearer.clone());
        let redacted = err.redacted();
        prop_assert!(
            !redacted.contains(&bearer),
            "Bearer token leaked into redacted output: {redacted}"
        );
        prop_assert!(redacted.contains("[REDACTED]"));
    }

    /// `api_key=<value>` 形式的查询参数在任意 value 下必须被脱敏。
    #[test]
    fn redacted_never_leaks_api_key_param(value in "[A-Za-z0-9_\\-]{20,50}") {
        let param = format!("api_key={value}");
        let err = DeepseeknovaError::config(param.clone());
        let redacted = err.redacted();
        prop_assert!(
            !redacted.contains(&param),
            "api_key param leaked into redacted output: {redacted}"
        );
        prop_assert!(redacted.contains("[REDACTED]"));
    }

    /// 脱敏后的输出长度不超过原始长度（替换只会缩短或等长，因为
    /// `[REDACTED]` 比典型 key/token 短）。
    #[test]
    fn redacted_output_never_longer_than_original(
        suffix in "[A-Za-z0-9_\\-]{30,100}"
    ) {
        let key = format!("sk-{suffix}");
        let err = DeepseeknovaError::provider(key);
        let raw = err.to_string();
        let redacted = err.redacted();
        prop_assert!(
            redacted.len() <= raw.len(),
            "redacted ({}) longer than raw ({})",
            redacted.len(),
            raw.len()
        );
    }
}

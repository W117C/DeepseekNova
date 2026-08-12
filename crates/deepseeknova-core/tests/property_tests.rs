//! Property-based tests for the error model.
//!
//! 验证 [`DeepseeknovaError`] 的不变量（`is_retryable` / `redacted`）在
//! 任意输入下成立，覆盖确定性变体的 retryable 语义与脱敏的泄漏防护。

use deepseeknova_core::memory::lifecycle::{LifecycleMeta, MemoryLifecycleStage};
use deepseeknova_core::tokens::{chars_for_tokens, estimate_text_tokens};
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

    // ── token 估算不变量 ─────────────────────────────────────────────

    /// 追加字符不减少 token 估算（单调性）。
    #[test]
    fn estimate_text_tokens_is_monotonic(prefix in ".{0,60}", suffix in ".{0,10}") {
        let before = estimate_text_tokens(&prefix);
        let after = estimate_text_tokens(&format!("{prefix}{suffix}"));
        prop_assert!(
            after >= before,
            "appending shrank estimate: {before} > {after}"
        );
    }

    /// 超可加性：拼接两段文本的估算不超过分别估算之和（ceil 求和只会更大）。
    #[test]
    fn estimate_text_tokens_is_superadditive(a in ".{0,40}", b in ".{0,40}") {
        let sum = estimate_text_tokens(&a) + estimate_text_tokens(&b);
        let joined = estimate_text_tokens(&format!("{a}{b}"));
        prop_assert!(sum >= joined, "superadditivity violated: {sum} < {joined}");
    }

    /// chars 预算保守：`tokens×4` 的字符预算必不小于任意文本的实际字符数
    /// （截断安全性的充分条件）。
    #[test]
    fn chars_budget_covers_any_text(text in ".{0,100}") {
        let tokens = estimate_text_tokens(&text) as usize;
        let budget = chars_for_tokens(tokens);
        let chars = text.chars().count();
        prop_assert!(budget >= chars, "budget {budget} < chars {chars}");
    }

    /// 每字符 token 上界 ceil(0.6×chars)（全 CJK 最重形态）。
    #[test]
    fn estimate_text_tokens_bounded_by_chars(text in ".{0,100}") {
        let chars = text.chars().count();
        let upper = (chars * 6).div_ceil(10) as u32;
        let est = estimate_text_tokens(&text);
        prop_assert!(est <= upper, "estimate {est} exceeds ceil(0.6×{chars})={upper}");
    }

    // ── 记忆生命周期不变量 ───────────────────────────────────────────

    /// 阶段字符串 roundtrip 稳定。
    #[test]
    fn stage_as_str_parse_roundtrip(s in "candidate|verified|permanent|archived") {
        let stage = MemoryLifecycleStage::parse(&s);
        prop_assert_eq!(stage.as_str(), s);
    }

    /// 新条目（age≈0）上 evaluate：recall≥1 至少 Verified、绝不到 Permanent
    /// （需要 >7 天）；recall==0 保持 Candidate（不可能 30 天即归档）。
    #[test]
    fn evaluate_on_fresh_entry_never_jumps_to_permanent(recalls in 0u32..=10) {
        let mut meta = LifecycleMeta::new(0.5);
        for _ in 0..recalls {
            meta.record_recall();
        }
        prop_assert_eq!(meta.recall_count, recalls);
        if recalls >= 1 {
            prop_assert_eq!(meta.stage, MemoryLifecycleStage::Verified);
        } else {
            prop_assert_eq!(meta.stage, MemoryLifecycleStage::Candidate);
        }
    }

    /// apply_decay 不增加 importance 且不低于 0。
    #[test]
    fn decay_never_increases_importance(importance in 0.0f32..=1.0, rate in 0.0f32..=0.1) {
        let mut meta = LifecycleMeta::new(importance);
        let before = meta.importance;
        let _ = meta.apply_decay(rate);
        prop_assert!(
            meta.importance <= before + 1e-6,
            "decay increased importance"
        );
        prop_assert!(meta.importance >= 0.0, "decay went negative");
    }

    /// Permanent 免疫衰减：importance 不变且返回 false。
    #[test]
    fn permanent_never_decays(importance in 0.0f32..=1.0, rate in 0.0f32..=0.1) {
        let mut meta = LifecycleMeta::new(importance);
        meta.stage = MemoryLifecycleStage::Permanent;
        let before = meta.importance;
        let archived = meta.apply_decay(rate);
        prop_assert!(!archived);
        prop_assert_eq!(meta.importance, before, "Permanent decayed");
    }

    /// reinforce 只升不降且封顶 1.0。
    #[test]
    fn reinforce_bounds_importance(importance in 0.0f32..=1.0, boost in 0.0f32..=0.5) {
        let mut meta = LifecycleMeta::new(importance);
        let before = meta.importance;
        meta.reinforce(boost);
        prop_assert!(
            meta.importance >= before - 1e-6,
            "reinforce lowered importance"
        );
        prop_assert!(meta.importance <= 1.0, "reinforce exceeded cap");
    }
}

//! # Token estimation facade (P3.1)
//!
//! 统一口径由 `deepseeknova_core::tokens` 提供（依据 DeepSeek 官方换算：
//! 1 个英文字符 ≈ 0.3 token，1 个中文字符 ≈ 0.6 token）。本模块只做
//! agent 侧的薄封装，避免各调用点各自实现造成口径漂移。

pub use deepseeknova_core::tokens::{estimate_text_tokens, has_cjk, is_cjk_char};
use deepseeknova_core::Message;

/// 估算消息序列的 token 数（含 reasoning_content）。
pub fn estimate_tokens(messages: &[Message]) -> u32 {
    estimate_messages_iter(messages.iter())
}

/// 估算消息**迭代器**的 token 数（含 reasoning_content）。与
/// `estimate_tokens(&[Message])` 完全同口径，供零拷贝接口
/// （如 [`crate::memory::Memory::estimate_tokens`]）在不构造 `Vec<Message>`
/// 的前提下复用同一换算逻辑，避免口径漂移。
pub fn estimate_messages_iter<'a>(messages: impl IntoIterator<Item = &'a Message>) -> u32 {
    messages
        .into_iter()
        .map(|m| {
            estimate_text_tokens(&m.content)
                + m.reasoning_content
                    .as_deref()
                    .map(estimate_text_tokens)
                    .unwrap_or(0)
        })
        .sum()
}

/// 单字符 CJK 判定（兼容旧名）。
pub fn is_cjk(c: char) -> bool {
    is_cjk_char(c)
}

/// 把 token 预算按文本自身的字符构成换算为 **字符预算**（用于截断/头尾保留）。
///
/// 纯 ASCII：≈ cap×4 字符；纯 CJK：≈ cap 字符；混合按比例折算。至少返回 80
/// 字符，避免极端短预算把上下文完全截没。
pub fn char_budget_for_tokens(text: &str, cap_tokens: u32) -> usize {
    let est = estimate_text_tokens(text).max(1);
    let chars = text.chars().count();
    if est <= cap_tokens {
        return chars;
    }
    let scaled = (chars as u64 * cap_tokens as u64 / est as u64) as usize;
    scaled.max(80)
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_core::Role;

    #[test]
    fn ascii_estimation_follows_deepseek_ratio() {
        assert_eq!(estimate_text_tokens(""), 0);
        // 4 字符 ≈ 1.2 token → ceil 2
        assert_eq!(estimate_text_tokens("abcd"), 2);
        // 10 字符 ≈ 3 token
        assert_eq!(estimate_text_tokens("abcdefghij"), 3);
    }

    #[test]
    fn cjk_counts_follow_deepseek_ratio() {
        // 每个汉字 ≈ 0.6 token；10 字 ≈ 6 token
        let s = "中文压缩阈值校准测试文本用于校验计量";
        assert_eq!(estimate_text_tokens(s), 11); // 18 字 → ceil(10.8) = 11
    }

    #[test]
    fn mixed_text_keeps_cjk_weight() {
        // 100 汉字 ≈ 60 token，纯 ASCII 100 字符 ≈ 30 token
        let s = "汉".repeat(100);
        assert_eq!(estimate_text_tokens(&s), 60);
    }

    #[test]
    fn char_budget_scales_with_estimate() {
        let s = "测".repeat(1000);
        let budget = char_budget_for_tokens(&s, 100);
        // 1000 字 ≈ 600 token → 100 token 预算对应 ≈ 166 字
        assert_eq!(budget, 166);
        let ascii = "a".repeat(1000);
        let budget = char_budget_for_tokens(&ascii, 100);
        // 1000 字符 ≈ 300 token → 100 token ≈ 333 字符
        assert_eq!(budget, 333);
    }

    #[test]
    fn estimate_messages_includes_reasoning() {
        let m = Message {
            role: Role::Assistant,
            content: "abcd".into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: Some("中文".into()),
            reasoning_signature: None,
            usage: None,
        };
        // 2 + 2
        assert_eq!(estimate_tokens(&[m]), 4);
    }

    #[test]
    fn estimate_messages_iter_matches_slice_estimate() {
        let msgs = vec![
            Message {
                role: Role::User,
                content: "hello".into(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                reasoning_signature: None,
                usage: None,
            },
            Message {
                role: Role::Assistant,
                content: "abcd".into(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: Some("中文".into()),
                reasoning_signature: None,
                usage: None,
            },
        ];
        assert_eq!(estimate_messages_iter(msgs.iter()), estimate_tokens(&msgs));
        assert_eq!(estimate_messages_iter(msgs.iter()), 6);
        assert_eq!(estimate_messages_iter(std::iter::empty()), 0);
    }

    #[test]
    fn facade_has_cjk_matches() {
        assert!(has_cjk("中文"));
        assert!(!has_cjk("english"));
        assert!(is_cjk('中'));
        assert!(!is_cjk('a'));
    }
}

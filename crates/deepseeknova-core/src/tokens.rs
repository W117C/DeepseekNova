//! # Token estimation (CJK-aware heuristic)
//!
//! DeepSeek's official token-usage guidance
//! (<https://api-docs.deepseek.com/quick_start/token_usage/>) gives the
//! approximate per-character conversion:
//!
//! - 1 English character ≈ 0.3 token
//! - 1 Chinese character ≈ 0.6 token
//!
//! The exact split differs per model, so this module exposes a deterministic
//! heuristic that removes the old "chars / 4 for everything" bias — most
//! importantly for Chinese-heavy conversations, where the old estimate
//! under-counted tokens by ~2.4×. When a provider returns real `usage`
//! numbers, those take precedence; this module is only for pre-flight
//! budgeting (compaction thresholds, injection caps, prefix-shape diffs).

/// Token estimate for a single text (content only, no message overhead).
pub fn estimate_text_tokens(text: &str) -> u32 {
    let (cjk, other) = text.chars().fold((0usize, 0usize), |(cjk, other), c| {
        if is_cjk_char(c) {
            (cjk + 1, other)
        } else {
            (cjk, other + 1)
        }
    });
    // ceil(0.6 * cjk + 0.3 * other), computed in integer math:
    (6 * cjk + 3 * other).div_ceil(10) as u32
}

/// Whether the text contains any CJK (Chinese/Japanese/Korean) character.
/// Used by retrieval layers to pick a CJK-friendly FTS strategy.
pub fn has_cjk(text: &str) -> bool {
    text.chars().any(is_cjk_char)
}

/// Single-character CJK test (exported for facades that need per-char logic).
pub fn is_cjk_char(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xF900..=0xFAFF
        | 0xFF00..=0xFFEF
    )
}

/// Token estimate for a message sequence: content + reasoning, no tool-call
/// JSON overhead (same fields the previous heuristic counted).
pub fn estimate_messages_tokens(messages: &[crate::Message]) -> u32 {
    messages
        .iter()
        .map(|m| {
            estimate_text_tokens(&m.content)
                + m.reasoning_content
                    .as_deref()
                    .map(estimate_text_tokens)
                    .unwrap_or(0)
        })
        .sum()
}

/// Conservative char budget for a token cap: `tokens * 4` chars.
///
/// This preserves the previous truncation semantics (a 4-char-per-token
/// allowance) while the *counting* side now uses the CJK-aware estimator.
/// Truncating at this budget is safe for both English and Chinese because it
/// over-allocates chars relative to the token estimate in every case.
pub fn chars_for_tokens(tokens: usize) -> usize {
    tokens.saturating_mul(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_is_zero_tokens() {
        assert_eq!(estimate_text_tokens(""), 0);
    }

    #[test]
    fn english_follows_deepseek_ratio() {
        // 4 English chars ≈ 1.2 tokens → ceil = 2
        assert_eq!(estimate_text_tokens("abcd"), 2);
        // 10 English chars ≈ 3 tokens
        assert_eq!(estimate_text_tokens("abcdefghij"), 3);
    }

    #[test]
    fn chinese_follows_deepseek_ratio() {
        // 1 Chinese char ≈ 0.6 token → ceil = 1
        assert_eq!(estimate_text_tokens("中"), 1);
        // 6 Chinese chars ≈ 3.6 → ceil = 4
        assert_eq!(estimate_text_tokens("中文测试文本"), 4);
    }

    #[test]
    fn mixed_text_is_heavier_than_old_estimate() {
        // Pure Chinese at 13 chars: old chars/4 = 4, new = 8 (2x).
        let pure = estimate_text_tokens("这是一个很长很长的中文句子");
        assert!(pure >= 7);
        // Mixed English + Chinese should be at least the English-only floor.
        let mixed = estimate_text_tokens("hello 世界");
        assert!(mixed >= 2);
    }

    #[test]
    fn full_width_and_cjk_punctuation_count_as_cjk() {
        assert_eq!(estimate_text_tokens("，。！"), 2); // 3 chars ≈ 1.8 → ceil 2
        assert_eq!(estimate_text_tokens("ＡＢＣ"), 2); // full-width ASCII ≈ 1.8 → ceil 2
    }

    #[test]
    fn has_cjk_detects_chinese_only() {
        assert!(has_cjk("中文"));
        assert!(has_cjk("mixed 中文 text"));
        assert!(!has_cjk("english only"));
        assert!(!has_cjk("123!@#"));
    }

    #[test]
    fn chars_for_tokens_is_conservative() {
        assert_eq!(chars_for_tokens(100), 400);
        assert_eq!(chars_for_tokens(0), 0);
    }

    #[test]
    fn estimate_messages_sums_content_and_reasoning() {
        use crate::{Message, Role};
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
        // "abcd" ≈ 2 tokens（ceil 0.3×4）+ "中文" ≈ 2 tokens（ceil 0.6×2）
        assert_eq!(estimate_messages_tokens(&[m]), 4);
    }
}

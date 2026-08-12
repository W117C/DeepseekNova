//! 工具相关辅助：召回上下文注入。
//!
//! 从 `agent.rs` 拆分（M7）：本模块保持纯搬移，不改行为/签名/逻辑。

use crate::agent::RecallProvider;
use crate::memory::Memory;
use deepseeknova_core::{Message, Role};

/// 召回注入：把命中块作为 volatile User 消息插入（不触碰 system 前缀，
/// 保住 DeepSeek-V4 前缀缓存）。返回是否实际注入。
///
/// B4：按 `max_chars` 预算裁剪命中块（默认由调用方传
/// [`DEFAULT_RECALL_MAX_CHARS`]），防止记忆块反噬上下文（弱模型尤甚）。
pub(crate) fn inject_recall(
    provider: &RecallProvider,
    memory: &mut Memory,
    query: &str,
    max_chars: usize,
) -> bool {
    let Some(block) = provider(query) else {
        return false;
    };
    if block.is_empty() {
        return false;
    }
    let capped = crate::contract::truncate_front(block, max_chars);
    memory.add_message(Message {
        role: Role::User,
        content: format!("<recalled-memory>\n{capped}\n</recalled-memory>"),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        reasoning_signature: None,
    });
    true
}

/// B4：召回注入的默认 token/字符预算。
pub(crate) const DEFAULT_RECALL_MAX_CHARS: usize = 2000;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn inject_recall_adds_volatile_user_message() {
        let mut memory = Memory::new();
        let rp: RecallProvider = Arc::new(|_| Some("hit".to_string()));
        assert!(inject_recall(
            &rp,
            &mut memory,
            "query",
            DEFAULT_RECALL_MAX_CHARS
        ));
        let msgs = memory.get_all();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::User);
        assert!(msgs[0].content.contains("hit"));

        let empty: RecallProvider = Arc::new(|_| None);
        assert!(!inject_recall(
            &empty,
            &mut memory,
            "query",
            DEFAULT_RECALL_MAX_CHARS
        ));
        assert_eq!(memory.get_all().len(), 1);
    }

    /// B4：召回块超过预算时被裁剪（保留尾部 + 截断标记）。
    #[test]
    fn inject_recall_caps_oversized_block() {
        let mut memory = Memory::new();
        let long = "x".repeat(100);
        let rp: RecallProvider = Arc::new(move |_| Some(long.clone()));
        assert!(inject_recall(&rp, &mut memory, "query", 50));
        let msgs = memory.get_all();
        assert_eq!(msgs.len(), 1);
        assert!(
            msgs[0].content.contains("[truncated, 50 chars kept]"),
            "oversized recall must be capped: {}",
            msgs[0].content
        );
    }
}

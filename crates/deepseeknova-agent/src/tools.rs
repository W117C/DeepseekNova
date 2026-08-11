//! 工具相关辅助：召回上下文注入。
//!
//! 从 `agent.rs` 拆分（M7）：本模块保持纯搬移，不改行为/签名/逻辑。

use crate::agent::RecallProvider;
use crate::memory::Memory;
use deepseeknova_core::{Message, Role};

/// 召回注入：把命中块作为 volatile User 消息插入（不触碰 system 前缀，
/// 保住 DeepSeek-V4 前缀缓存）。返回是否实际注入。
pub(crate) fn inject_recall(provider: &RecallProvider, memory: &mut Memory, query: &str) -> bool {
    let Some(block) = provider(query) else {
        return false;
    };
    if block.is_empty() {
        return false;
    }
    memory.add_message(Message {
        role: Role::User,
        content: format!("<recalled-memory>\n{block}\n</recalled-memory>"),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        reasoning_signature: None,
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn inject_recall_adds_volatile_user_message() {
        let mut memory = Memory::new();
        let rp: RecallProvider = Arc::new(|_| Some("hit".to_string()));
        assert!(inject_recall(&rp, &mut memory, "query"));
        let msgs = memory.get_all();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::User);
        assert!(msgs[0].content.contains("hit"));

        let empty: RecallProvider = Arc::new(|_| None);
        assert!(!inject_recall(&empty, &mut memory, "query"));
        assert_eq!(memory.get_all().len(), 1);
    }
}

use deepseeknova_core::{Message, Role};
use std::collections::{HashMap, HashSet, VecDeque};

const TRUNCATION_HEAD_RATIO: f32 = 0.2;

/// In-memory conversation history.
#[derive(Default)]
pub struct Memory {
    messages: VecDeque<Message>,

    /// Side-band storage for original, un-truncated full tool results.
    /// Keyed by tool_call_id.
    full_results: HashMap<String, String>,

    /// Side-band set tracking which messages (by tool_call_id) have been shrunk,
    /// ensuring idempotency without modifying the Message structure sent to the provider.
    shrunk_messages: HashSet<String>,

    /// Pinned messages never removed by compaction (system prompt, first user turn).
    pinned: Vec<Message>,
}

impl Memory {
    /// Create an empty conversation history.
    pub fn new() -> Self {
        Self {
            messages: VecDeque::new(),
            full_results: HashMap::new(),
            shrunk_messages: HashSet::new(),
            pinned: Vec::new(),
        }
    }

    /// Append a message to the conversation history.
    pub fn add_message(&mut self, message: Message) {
        self.messages.push_back(message);
    }

    /// Return a cloned snapshot of all messages (pinned first, then the rest).
    pub fn get_all(&self) -> Vec<Message> {
        let mut out = Vec::new();
        out.extend(self.pinned.iter().cloned());
        out.extend(self.messages.iter().cloned());
        out
    }

    // -----------------------------------------------------------------------
    // A1 热路径：零拷贝只读接口（避免为仅需统计/末条/近条的场景全量 clone）
    // -----------------------------------------------------------------------

    /// 会话历史全部消息的可借用视图（pinned + messages），**不克隆**。
    /// 与 `get_all()` 顺序一致（先 pinned，后 messages）。
    pub fn iter_all(&self) -> impl Iterator<Item = &Message> {
        self.pinned.iter().chain(self.messages.iter())
    }

    /// 会话历史消息总数（零拷贝，等价 `get_all().len()`）。
    pub fn len(&self) -> usize {
        self.pinned.len() + self.messages.len()
    }

    /// 会话历史是否为空（零拷贝，等价 `get_all().is_empty()`）。
    pub fn is_empty(&self) -> bool {
        self.pinned.is_empty() && self.messages.is_empty()
    }

    /// 最后一条消息（只读借用，零拷贝；等价 `get_all().last()`）。
    pub fn last_message(&self) -> Option<&Message> {
        self.messages.back().or_else(|| self.pinned.last())
    }

    /// 迭代最近 `n` 条消息（保持从旧到新的顺序，零拷贝）；不足 `n` 条时
    /// 返回全部。与 `get_all()` 的尾部 `n` 条一致。
    pub fn iter_recent(&self, n: usize) -> impl Iterator<Item = &Message> {
        let skip = self.len().saturating_sub(n);
        self.iter_all().skip(skip)
    }

    /// 估算会话历史 token 数（含 reasoning_content），**零拷贝**。与
    /// [`crate::tokens::estimate_tokens`] 同口径（同一换算函数）。
    pub fn estimate_tokens(&self) -> u32 {
        crate::tokens::estimate_messages_iter(self.iter_all())
    }

    /// Clear all messages, cached full results, and shrink markers (pinned
    /// messages are retained).
    pub fn clear(&mut self) {
        self.messages.clear();
        self.full_results.clear();
        self.shrunk_messages.clear();
    }

    /// Retrieve full original result if truncated.
    pub fn get_full_result(&self, id: &str) -> Option<&String> {
        self.full_results.get(id)
    }

    /// Compact the conversation by replacing all messages with a single
    /// summary digest. Useful when the working set grows beyond the
    /// context window and a full history is no longer helpful.
    ///
    /// `reasoning_summary` optionally preserves a condensed version of the
    /// model's thinking from the compacted turns, which helps maintain
    /// DeepSeek thinking mode continuity across compaction boundaries.
    pub fn compact(&mut self, digest: String, reasoning_summary: Option<String>) {
        // Safety: check for unresolved must_replay turns before compacting.
        // If any assistant message with tool_calls still has reasoning that
        // hasn't been consumed, compaction would break the DeepSeek V4
        // reasoning_content contract, causing HTTP 400 on the next request.
        let pending_replay: Vec<&Message> = self
            .messages
            .iter()
            .filter(|m| {
                m.reasoning_block()
                    .map(|rb| rb.must_replay)
                    .unwrap_or(false)
            })
            .collect();
        if !pending_replay.is_empty() {
            tracing::warn!(
                count = pending_replay.len(),
                "compacting while must_replay reasoning blocks exist — \
                 this may break DeepSeek V4 tool call continuity"
            );
        }

        self.messages.clear();
        self.shrunk_messages.clear();

        // Prepend the digest as a tool message the model can read.
        self.messages.push_back(Message {
            role: Role::Tool,
            content: format!("[Compaction digest] {digest}"),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: reasoning_summary,
        });
    }

    /// Check whether the conversation has any unresolved must_replay
    /// reasoning blocks that must not be compacted away.
    pub fn has_pending_must_replay(&self) -> bool {
        self.messages.iter().any(|m| {
            m.reasoning_block()
                .map(|rb| rb.must_replay)
                .unwrap_or(false)
        })
    }

    /// Turn-end compaction: shrink large tool results (Head/Tail Truncation).
    /// Does not summarize the entire log, preserving LLM Prefix Caches.
    ///
    /// # P0 Fix: Bounds-checked string slicing with UTF-8 boundary awareness.
    /// Previously, `head_len` and `tail_len` could exceed `content.len()`, causing a panic.
    /// Now clamped to valid boundaries and uses floor_char_boundary to avoid splitting
    /// multi-byte UTF-8 characters.
    /// 按 **token 预算** 收缩大工具结果。预算先经 [`crate::tokens::char_budget_for_tokens`]
    /// 按消息自身的 CJK/ASCII 构成换算为字符预算，再换算为字节上限做头尾截断，
    /// 保证中文长结果不会被误判为"超限 4 倍"。
    pub fn shrink_large_results(&mut self, threshold_tokens: u32) {
        for msg in self.messages.iter_mut().rev() {
            if msg.role != Role::Tool {
                continue;
            }

            let call_id = match &msg.tool_call_id {
                Some(id) => id,
                None => continue,
            };

            if self.shrunk_messages.contains(call_id) {
                continue;
            }

            let tlen = msg.content.len();
            let budget_chars =
                crate::tokens::char_budget_for_tokens(&msg.content, threshold_tokens);
            // 字符预算 → 字节预算：ASCII 1:1，CJK 1:3，取 4 为安全上界。
            let threshold_chars = budget_chars.saturating_mul(4);

            // P0 FIX: Only truncate if content is actually larger than threshold
            // and ensure head_len + tail_len never exceeds content length.
            if tlen > threshold_chars {
                self.full_results
                    .insert(call_id.clone(), msg.content.clone());

                let head_len =
                    ((threshold_chars as f32 * TRUNCATION_HEAD_RATIO) as usize).min(tlen);
                let tail_len = threshold_chars
                    .saturating_sub(head_len)
                    .min(tlen - head_len);

                // P0 FIX: Use floor_char_boundary to avoid splitting UTF-8 characters
                let head_end = floor_char_boundary_safe(&msg.content, head_len);
                let tail_start = tlen.saturating_sub(tail_len);
                let tail_start = floor_char_boundary_safe_from_end(&msg.content, tail_start);

                let head = &msg.content[..head_end];
                let tail = &msg.content[tail_start..];

                let omitted = tlen - head_end - (tlen - tail_start);

                msg.content = format!(
                    "{}\n\n... [{} bytes omitted, use fetch_full_result(\"{}\") to retrieve] ...\n\n{}",
                    head, omitted, call_id, tail
                );

                self.shrunk_messages.insert(call_id.clone());
            }
        }
    }

    /// Atomic sliding window fallback.
    /// Drops the oldest contiguous "Turn Chunk" (User -> Assistant -> ToolResults)
    /// to avoid breaking provider API tool_use invariants.
    ///
    /// # P1 Fix: Preserve tool_call/tool_result pairing.
    /// Now tracks tool_call_ids in the Assistant message and ensures
    /// corresponding Tool messages are dropped together.
    pub fn slide_window(&mut self) {
        // System 消息（system prompt + repo map）是 DeepSeek-V4 prefix cache
        // 的基础，slide_window 绝不能弹掉它。先暂存开头的所有 System 消息，
        // 处理完后续 Turn Chunk 后放回队列最前。
        let mut saved_system: Vec<Message> = Vec::new();
        while self
            .messages
            .front()
            .is_some_and(|m| m.role == Role::System)
        {
            // front() 刚返回 Some(System)，pop_front() 必返回 Some；
            // 用 if let 满足 clippy::unwrap_used。
            if let Some(msg) = self.messages.pop_front() {
                saved_system.push(msg);
            } else {
                break; // 防御性：理论上不可达
            }
        }

        let mut dropped_ids = Vec::new();

        while let Some(front) = self.messages.front() {
            if front.role == Role::User && !dropped_ids.is_empty() {
                break;
            }

            // P1 FIX: When dropping an Assistant message with tool_calls,
            // collect all tool_call_ids so we can also drop their Tool results
            if front.role == Role::Assistant {
                if let Some(ref tool_calls) = front.tool_calls {
                    for tc in tool_calls {
                        dropped_ids.push(tc.id.clone());
                    }
                }
            }

            // Track tool_call_id of Tool messages being dropped
            if let Some(id) = &front.tool_call_id {
                dropped_ids.push(id.clone());
            }

            self.messages.pop_front();
        }

        // P1 FIX: After sliding, check if any remaining Tool messages
        // have lost their corresponding Assistant tool_call.
        // If so, drop those orphaned Tool messages too.
        let remaining_call_ids: HashSet<String> = self
            .messages
            .iter()
            .filter_map(|m| {
                if m.role == Role::Assistant {
                    m.tool_calls
                        .as_ref()
                        .map(|tcs| tcs.iter().map(|tc| tc.id.clone()).collect::<Vec<_>>())
                } else {
                    None
                }
            })
            .flatten()
            .collect();

        self.messages.retain(|m| {
            if m.role == Role::Tool {
                if let Some(ref id) = m.tool_call_id {
                    // Keep only if the corresponding Assistant tool_call still exists
                    return remaining_call_ids.contains(id);
                }
            }
            true
        });

        for id in dropped_ids {
            self.full_results.remove(&id);
            self.shrunk_messages.remove(&id);
        }

        // 把暂存的 System 消息放回队列最前，保持 prefix cache 稳定性。
        for sm in saved_system.into_iter().rev() {
            self.messages.push_front(sm);
        }
    }

    /// Pin a message so compaction never removes it (e.g. system prompt,
    /// first user turn).
    pub fn pin_message(&mut self, message: Message) {
        self.pinned.push(message);
    }
}

/// Find the largest UTF-8 character boundary at or before `max`.
/// Equivalent to the unstable `str::floor_char_boundary` but works on stable Rust.
fn floor_char_boundary_safe(s: &str, max: usize) -> usize {
    if max >= s.len() {
        return s.len();
    }
    let mut idx = max;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Find the smallest UTF-8 character boundary at or after `min`.
fn floor_char_boundary_safe_from_end(s: &str, min: usize) -> usize {
    if min >= s.len() {
        return s.len();
    }
    let mut idx = min;
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: Role, content: &str) -> Message {
        Message {
            role,
            content: content.into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    fn seed_memory() -> Memory {
        let mut m = Memory::new();
        m.pin_message(msg(Role::System, "system"));
        m.add_message(msg(Role::User, "first"));
        m.add_message(msg(Role::Assistant, "second"));
        m
    }

    #[test]
    fn zero_copy_views_match_get_all() {
        let m = seed_memory();
        let all = m.get_all();
        // iter_all 顺序与 get_all 一致（按内容比较；Message 无 PartialEq）
        let iter_contents: Vec<&str> = m.iter_all().map(|x| x.content.as_str()).collect();
        let all_contents: Vec<&str> = all.iter().map(|x| x.content.as_str()).collect();
        assert_eq!(iter_contents, all_contents);
        // len / is_empty / last_message 与 get_all 派生一致
        assert_eq!(m.len(), all.len());
        assert_eq!(m.last_message().map(|x| x.content.as_str()), Some("second"));
        assert!(!m.is_empty());
        assert!(Memory::new().is_empty());
        assert!(Memory::new().last_message().is_none());
        // iter_recent：最近 n 条、旧→新顺序；不足 n 时返回全部
        let recent: Vec<&str> = m.iter_recent(2).map(|x| x.content.as_str()).collect();
        assert_eq!(recent, vec!["first", "second"]);
        let recent_all: Vec<&str> = m.iter_recent(10).map(|x| x.content.as_str()).collect();
        assert_eq!(recent_all, vec!["system", "first", "second"]);
        assert_eq!(m.iter_recent(0).count(), 0);
    }

    #[test]
    fn estimate_tokens_zero_copy_matches_get_all_slice() {
        let m = seed_memory();
        let all = m.get_all();
        assert_eq!(m.estimate_tokens(), crate::tokens::estimate_tokens(&all));
        assert_eq!(Memory::new().estimate_tokens(), 0);
    }

    /// 构造带 tool_calls 的 Assistant 消息（模拟真实工具调用回合）。
    fn msg_with_calls(content: &str, ids: &[&str]) -> Message {
        use deepseeknova_core::{FunctionCall, ToolCall};
        Message {
            role: Role::Assistant,
            content: content.into(),
            name: None,
            tool_calls: Some(
                ids.iter()
                    .map(|id| ToolCall {
                        id: id.to_string(),
                        ty: "function".into(),
                        function: FunctionCall {
                            name: "tool".into(),
                            arguments: "{}".into(),
                        },
                    })
                    .collect(),
            ),
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    /// 构造带 tool_call_id 的 Tool 结果消息。
    fn msg_tool_result(id: &str, content: &str) -> Message {
        Message {
            role: Role::Tool,
            content: content.into(),
            name: None,
            tool_calls: None,
            tool_call_id: Some(id.to_string()),
            reasoning_content: None,
        }
    }

    /// System 消息（system prompt + repo map）经 add_message 进入 messages 队列
    /// （agent 主路径 [mod.rs] 的真实路径），slide_window 绝不能弹掉它，
    /// 否则破坏 DeepSeek-V4 prefix cache 并丢失身份/规则。
    #[test]
    fn slide_window_preserves_system_message_in_queue() {
        let mut m = Memory::new();
        // 模拟 agent 主路径：System 消息经 add_message 进入队列（非 pinned）
        m.add_message(msg(Role::System, "system prompt + repo map"));
        m.add_message(msg(Role::User, "turn1"));
        m.add_message(msg_with_calls("resp1", &["tc1"]));
        m.add_message(msg_tool_result("tc1", "tool1"));
        m.add_message(msg(Role::User, "turn2"));
        m.add_message(msg(Role::Assistant, "resp2"));

        m.slide_window();

        let all = m.get_all();
        // System 消息必须保留在队列最前
        assert_eq!(all[0].role, Role::System);
        assert_eq!(all[0].content, "system prompt + repo map");
        // 第一个 Turn Chunk（User+Assistant+Tool）应被滑出，turn2 应保留
        let contents: Vec<_> = all.iter().map(|x| x.content.as_str()).collect();
        assert!(!contents.contains(&"turn1"));
        assert!(!contents.contains(&"resp1"));
        assert!(!contents.contains(&"tool1"));
        assert!(contents.contains(&"turn2"));
        assert!(contents.contains(&"resp2"));
    }
}

use dpronix_core::{Message, Role};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

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

    /// Last activity instant — used by idle-compaction.
    last_activity: Option<Instant>,

    /// Counter tracking consecutive identical tool calls for repeat-guard.
    call_counts: HashMap<String, u32>,

    /// Previous tool call key for repeat-guard detection.
    last_call_key: Option<String>,
}

impl Memory {
    pub fn new() -> Self {
        Self {
            messages: VecDeque::new(),
            full_results: HashMap::new(),
            shrunk_messages: HashSet::new(),
            pinned: Vec::new(),
            last_activity: None,
            call_counts: HashMap::new(),
            last_call_key: None,
        }
    }

    pub fn add_message(&mut self, message: Message) {
        self.messages.push_back(message);
        self.bump_activity();
    }

    pub fn get_all(&self) -> Vec<Message> {
        let mut out = Vec::new();
        out.extend(self.pinned.iter().cloned());
        out.extend(self.messages.iter().cloned());
        out
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.full_results.clear();
        self.shrunk_messages.clear();
        self.call_counts.clear();
        self.last_call_key = None;
        self.bump_activity();
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
        self.call_counts.clear();
        self.last_call_key = None;

        // Prepend the digest as a tool message the model can read.
        self.messages.push_back(Message {
            role: Role::Tool,
            content: format!("[Compaction digest] {digest}"),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: reasoning_summary,
        });

        self.bump_activity();
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

    /// Return the duration since last activity, used by idle-compaction.
    pub fn idle_duration(&self) -> Option<Duration> {
        self.last_activity.map(|t| t.elapsed())
    }

    /// Record a tool call for repeat-guard detection. Returns the current
    /// repeat count for this tool+args key.
    pub fn record_call(&mut self, tool_name: &str, args_key: &str) -> u32 {
        let key = format!("{tool_name}:{args_key}");
        let count = if self.last_call_key.as_deref() == Some(key.as_str()) {
            self.call_counts.get(&key).copied().unwrap_or(0) + 1
        } else {
            1
        };
        self.call_counts.insert(key.clone(), count);
        self.last_call_key = Some(key);
        self.bump_activity();
        count
    }

    /// Reset the repeat-guard counter (e.g. after a successful non-repeated action).
    pub fn reset_repeat_guard(&mut self) {
        self.call_counts.clear();
        self.last_call_key = None;
    }

    /// Turn-end compaction: shrink large tool results (Head/Tail Truncation).
    /// Does not summarize the entire log, preserving LLM Prefix Caches.
    pub fn shrink_large_results(&mut self, threshold_chars: usize) {
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

            if msg.content.len() > threshold_chars {
                self.full_results
                    .insert(call_id.clone(), msg.content.clone());

                let head_len = (threshold_chars as f32 * TRUNCATION_HEAD_RATIO) as usize;
                let tail_len = threshold_chars - head_len;

                let tlen = msg.content.len();
                let head = &msg.content[..head_len];
                let tail = &msg.content[tlen - tail_len..];

                let omitted = msg.content.len() - head_len - tail_len;
                msg.content = format!(
                    "{}\n\n... [{} bytes omitted, use fetch_full_result(\"{}\") to retrieve] ...\n\n{}",
                    head, omitted, call_id, tail
                );

                self.shrunk_messages.insert(call_id.clone());
            }
        }
        self.bump_activity();
    }

    /// Atomic sliding window fallback.
    /// Drops the oldest contiguous "Turn Chunk" (User -> Assistant -> ToolResults)
    /// to avoid breaking provider API tool_use invariants.
    pub fn slide_window(&mut self) {
        let mut dropped_ids = Vec::new();

        while let Some(front) = self.messages.front() {
            if front.role == Role::User && !dropped_ids.is_empty() {
                break;
            }

            if let Some(id) = &front.tool_call_id {
                dropped_ids.push(id.clone());
            }

            self.messages.pop_front();
        }

        for id in dropped_ids {
            self.full_results.remove(&id);
            self.shrunk_messages.remove(&id);
        }
        self.bump_activity();
    }

    pub fn pin_message(&mut self, message: Message) {
        self.pinned.push(message);
        self.bump_activity();
    }

    fn bump_activity(&mut self) {
        self.last_activity = Some(Instant::now());
    }
}

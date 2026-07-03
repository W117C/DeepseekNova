use reasonix_core::Message;
use std::collections::VecDeque;

/// In-memory conversation history.
#[derive(Default)]
pub struct Memory {
    messages: VecDeque<Message>,
    /// Stack of compaction digests (oldest first).
    compaction_digests: Vec<String>,
    /// Pinned messages never removed by compaction (system prompt, first user turn).
    pinned: Vec<Message>,
}

impl Memory {
    pub fn new() -> Self {
        Self {
            messages: VecDeque::new(),
            compaction_digests: Vec::new(),
            pinned: Vec::new(),
        }
    }

    pub fn add_message(&mut self, message: Message) {
        self.messages.push_back(message);
    }

    pub fn get_all(&self) -> Vec<Message> {
        let mut out = Vec::new();

        // Pinned messages first
        out.extend(self.pinned.iter().cloned());

        // Compaction digests as system messages
        for digest in &self.compaction_digests {
            out.push(Message {
                role: reasonix_core::Role::User,
                content: format!("<conversation-history>\n{digest}\n</conversation-history>"),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            });
        }

        // Current window
        out.extend(self.messages.iter().cloned());

        out
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.compaction_digests.clear();
    }

    /// Remove last `count` messages from the active window.
    pub fn rewind(&mut self, count: usize) {
        for _ in 0..count {
            self.messages.pop_back();
        }
    }

    /// Compact: fold current messages into a digest, keep last N messages.
    pub fn compact(&mut self, digest: String) {
        // Move all current messages except the last 4 into a digest
        let keep_count = 4usize.min(self.messages.len());
        let to_summarize: Vec<Message> = self
            .messages
            .drain(0..self.messages.len().saturating_sub(keep_count))
            .collect();

        if !to_summarize.is_empty() {
            self.compaction_digests.push(digest);
        }
    }

    /// Pin a message so it survives compaction.
    pub fn pin_message(&mut self, message: Message) {
        self.pinned.push(message);
    }
}

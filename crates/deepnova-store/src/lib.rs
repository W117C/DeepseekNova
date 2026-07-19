//! # Store — Session persistence
//!
//! JSONL-based session recording: persists every agent turn to disk
//! for replay, debugging, and analytics.
//! Supports rotation and compaction.

use deepnova_core::{Message, Role, RunInput};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// SessionStore — JSONL-based session persistence
// ---------------------------------------------------------------------------

/// A single persisted turn in the session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTurn {
    /// Monotonic turn counter (1-based).
    pub turn: u64,
    /// ISO-8601 timestamp.
    pub timestamp: String,
    /// The user's input for this turn.
    pub input: StoredInput,
    /// The agent's final output (collected text, tool calls).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<StoredOutput>,
    /// All messages exchanged during this turn.
    pub messages: Vec<StoredMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredInput {
    pub prompt: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub images: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_override: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredOutput {
    pub text: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tool_calls: Vec<StoredToolCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredToolCall {
    pub name: String,
    pub arguments: String,
    pub result: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

// ---------------------------------------------------------------------------
// SessionStore
// ---------------------------------------------------------------------------

/// JSONL-based session store. Each session is a directory containing a
/// `turns.jsonl` file with one JSON object per line (one turn per line).
pub struct SessionStore {
    root: PathBuf,
}

impl SessionStore {
    /// Create a new store rooted at `root`. Creates the directory if needed.
    pub fn new(root: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// The path to the JSONL file for a session.
    fn session_path(&self, session_id: &str) -> PathBuf {
        self.root.join(session_id).with_extension("jsonl")
    }

    /// Load all turns from a session. Returns an empty Vec if the file
    /// doesn't exist.
    pub fn load(&self, session_id: &str) -> anyhow::Result<Vec<StoredTurn>> {
        let path = self.session_path(session_id);
        if !path.exists() {
            return Ok(Vec::new());
        }

        let content = std::fs::read_to_string(&path)?;
        let mut turns = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let turn: StoredTurn = serde_json::from_str(line)?;
            turns.push(turn);
        }
        Ok(turns)
    }

    /// Append a single turn to the session file. Creates the file if needed.
    pub fn append(&self, session_id: &str, turn: &StoredTurn) -> anyhow::Result<()> {
        let path = self.session_path(session_id);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        serde_json::to_writer(&mut file, turn)?;
        use std::io::Write;
        writeln!(file)?;
        Ok(())
    }

    /// Append multiple turns at once.
    pub fn append_all(&self, session_id: &str, turns: &[StoredTurn]) -> anyhow::Result<()> {
        for turn in turns {
            self.append(session_id, turn)?;
        }
        Ok(())
    }

    /// Count the number of turns stored for a session.
    pub fn len(&self, session_id: &str) -> anyhow::Result<usize> {
        Ok(self.load(session_id)?.len())
    }

    /// Whether the session file is empty or missing.
    pub fn is_empty(&self, session_id: &str) -> anyhow::Result<bool> {
        Ok(self.len(session_id)? == 0)
    }

    /// Delete a session file.
    pub fn delete(&self, session_id: &str) -> anyhow::Result<()> {
        let path = self.session_path(session_id);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// List all session IDs (filenames without extension).
    pub fn list_sessions(&self) -> anyhow::Result<Vec<String>> {
        let mut sessions = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "jsonl") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    sessions.push(stem.to_string());
                }
            }
        }
        Ok(sessions)
    }

    /// Get the last N turns from a session.
    pub fn last_n(&self, session_id: &str, n: usize) -> anyhow::Result<Vec<StoredTurn>> {
        let mut turns = self.load(session_id)?;
        let start = turns.len().saturating_sub(n);
        Ok(turns.split_off(start))
    }

    /// Build a new turn from the input.
    pub fn build_turn(
        input: &RunInput,
        turn_number: u64,
        messages: Vec<Message>,
        output: Option<StoredOutput>,
    ) -> StoredTurn {
        StoredTurn {
            turn: turn_number,
            timestamp: chrono::Utc::now().to_rfc3339(),
            input: StoredInput {
                prompt: input.prompt.clone(),
                images: input.images.clone(),
                model_override: input.model_override.clone(),
            },
            output,
            messages: messages
                .into_iter()
                .map(|m| StoredMessage {
                    role: role_to_str(&m.role),
                    content: m.content,
                    name: m.name,
                    tool_call_id: m.tool_call_id,
                })
                .collect(),
        }
    }
}

// ---------------------------------------------------------------------------
// Converters
// ---------------------------------------------------------------------------

fn role_to_str(role: &Role) -> String {
    match role {
        Role::System => "system".to_string(),
        Role::User => "user".to_string(),
        Role::Assistant => "assistant".to_string(),
        Role::Tool => "tool".to_string(),
    }
}

impl From<&StoredInput> for RunInput {
    fn from(si: &StoredInput) -> Self {
        RunInput {
            prompt: si.prompt.clone(),
            images: si.images.clone(),
            model_override: si.model_override.clone(),
        }
    }
}

impl From<&StoredMessage> for Message {
    fn from(sm: &StoredMessage) -> Self {
        Message {
            role: match sm.role.as_str() {
                "system" => Role::System,
                "assistant" => Role::Assistant,
                "tool" => Role::Tool,
                _ => Role::User,
            },
            content: sm.content.clone(),
            name: sm.name.clone(),
            tool_calls: None,
            tool_call_id: sm.tool_call_id.clone(),
            reasoning_content: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root() -> PathBuf {
        std::env::temp_dir().join(format!("deepnova-store-test-{}", std::process::id()))
    }

    fn sample_input() -> RunInput {
        RunInput {
            prompt: "hello world".to_string(),
            images: vec![],
            model_override: None,
        }
    }

    fn sample_messages() -> Vec<Message> {
        vec![
            Message {
                role: Role::User,
                content: "hello".to_string(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            Message {
                role: Role::Assistant,
                content: "hi there".to_string(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
        ]
    }

    #[test]
    fn build_turn_roundtrips() {
        let input = sample_input();
        let messages = sample_messages();
        let turn = SessionStore::build_turn(
            &input,
            1,
            messages.clone(),
            Some(StoredOutput {
                text: "hi there".into(),
                tool_calls: vec![],
            }),
        );

        assert_eq!(turn.turn, 1);
        assert_eq!(turn.input.prompt, "hello world");
        assert_eq!(turn.messages.len(), 2);
        assert_eq!(turn.messages[0].role, "user");
        assert_eq!(turn.messages[1].role, "assistant");
        assert!(turn.output.is_some());
        assert_eq!(turn.output.as_ref().unwrap().text, "hi there");
    }

    #[test]
    fn append_and_load_roundtrips() {
        let root = test_root();
        let store = SessionStore::new(root.clone()).unwrap();

        let input = sample_input();
        let turn = SessionStore::build_turn(&input, 1, sample_messages(), None);
        store.append("test-session", &turn).unwrap();

        let loaded = store.load("test-session").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].turn, 1);
        assert_eq!(loaded[0].input.prompt, "hello world");

        // Cleanup
        let _ = store.delete("test-session");
    }

    #[test]
    fn load_missing_session_returns_empty() {
        let root = test_root();
        let store = SessionStore::new(root).unwrap();
        let turns = store.load("nonexistent").unwrap();
        assert!(turns.is_empty());
    }

    #[test]
    fn append_multiple_turns() {
        let root = test_root();
        let store = SessionStore::new(root.clone()).unwrap();

        for i in 1..=5 {
            let input = RunInput {
                prompt: format!("turn {i}"),
                images: vec![],
                model_override: None,
            };
            let turn = SessionStore::build_turn(&input, i, vec![], None);
            store.append("multi", &turn).unwrap();
        }

        let loaded = store.load("multi").unwrap();
        assert_eq!(loaded.len(), 5);
        assert_eq!(loaded[0].turn, 1);
        assert_eq!(loaded[4].turn, 5);

        let _ = store.delete("multi");
    }

    #[test]
    fn last_n_returns_tail() {
        let root = test_root();
        let store = SessionStore::new(root.clone()).unwrap();

        for i in 1..=10 {
            let input = RunInput {
                prompt: format!("turn {i}"),
                images: vec![],
                model_override: None,
            };
            let turn = SessionStore::build_turn(&input, i, vec![], None);
            store.append("tail", &turn).unwrap();
        }

        let last = store.last_n("tail", 3).unwrap();
        assert_eq!(last.len(), 3);
        assert_eq!(last[0].turn, 8);
        assert_eq!(last[2].turn, 10);

        let _ = store.delete("tail");
    }

    #[test]
    fn list_sessions() {
        let root = test_root();
        let store = SessionStore::new(root.clone()).unwrap();

        let input = sample_input();
        let turn = SessionStore::build_turn(&input, 1, vec![], None);
        store.append("session-a", &turn).unwrap();
        store.append("session-b", &turn).unwrap();

        let sessions = store.list_sessions().unwrap();
        assert!(sessions.contains(&"session-a".to_string()));
        assert!(sessions.contains(&"session-b".to_string()));

        let _ = store.delete("session-a");
        let _ = store.delete("session-b");
    }

    #[test]
    fn delete_session() {
        let root = test_root();
        let store = SessionStore::new(root.clone()).unwrap();

        let input = sample_input();
        let turn = SessionStore::build_turn(&input, 1, vec![], None);
        store.append("temp", &turn).unwrap();
        assert!(!store.is_empty("temp").unwrap());

        store.delete("temp").unwrap();
        assert!(store.is_empty("temp").unwrap());
    }

    #[test]
    fn stored_message_conversion() {
        let sm = StoredMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
            name: None,
            tool_call_id: None,
        };
        let msg: Message = (&sm).into();
        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.content, "hello");

        let sm = StoredMessage {
            role: "system".to_string(),
            content: "you are helpful".to_string(),
            name: None,
            tool_call_id: None,
        };
        let msg: Message = (&sm).into();
        assert_eq!(msg.role, Role::System);
    }

    #[test]
    fn stored_input_to_run_input() {
        let si = StoredInput {
            prompt: "test".into(),
            images: vec!["img1".into()],
            model_override: Some("gpt-4".into()),
        };
        let ri: RunInput = (&si).into();
        assert_eq!(ri.prompt, "test");
        assert_eq!(ri.images.len(), 1);
        assert_eq!(ri.model_override, Some("gpt-4".into()));
    }
}

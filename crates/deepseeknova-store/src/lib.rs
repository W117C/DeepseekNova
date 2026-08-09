//! # Store — Session persistence
//!
//! JSONL-based session recording: persists every agent turn to disk
//! for replay, debugging, and analytics.
//! Supports rotation and compaction.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use deepseeknova_core::{DeepseeknovaError, Message, Role, RunInput};
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
    /// 工作区根路径（会话所在项目；`None` = 全局/未知）。serde default
    /// 保持旧会话文件可读（v0.5.x 前的 JSONL 无此字段）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
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
    /// Assistant tool calls (schema v2). `serde(default)` keeps old files readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<deepseeknova_core::types::ToolCall>>,
    /// DeepSeek-V4 reasoning content (schema v2), required for replay fidelity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
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
    pub fn new(root: PathBuf) -> Result<Self, DeepseeknovaError> {
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// The path to the JSONL file for a session.
    fn session_path(&self, session_id: &str) -> PathBuf {
        self.root.join(session_id).with_extension("jsonl")
    }

    /// Load all turns from a session. Returns an empty Vec if the file
    /// doesn't exist.
    pub fn load(&self, session_id: &str) -> Result<Vec<StoredTurn>, DeepseeknovaError> {
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
    pub fn append(&self, session_id: &str, turn: &StoredTurn) -> Result<(), DeepseeknovaError> {
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
    pub fn append_all(
        &self,
        session_id: &str,
        turns: &[StoredTurn],
    ) -> Result<(), DeepseeknovaError> {
        for turn in turns {
            self.append(session_id, turn)?;
        }
        Ok(())
    }

    /// Count the number of turns stored for a session.
    pub fn len(&self, session_id: &str) -> Result<usize, DeepseeknovaError> {
        Ok(self.load(session_id)?.len())
    }

    /// Whether the session file is empty or missing.
    pub fn is_empty(&self, session_id: &str) -> Result<bool, DeepseeknovaError> {
        Ok(self.len(session_id)? == 0)
    }

    /// Create the session file if it does not exist yet (a zero-turn session),
    /// so it shows up in [`SessionStore::list_sessions`] before the first turn
    /// is appended. Existing files are left untouched.
    ///
    /// 注意：`session_id` 由调用方保证可信（本 crate 不做白名单校验）；暴露给
    /// 外部输入的调用点（如 HTTP 端点）必须先做 id 白名单过滤再传入。
    pub fn touch(&self, session_id: &str) -> Result<(), DeepseeknovaError> {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.session_path(session_id))?;
        Ok(())
    }

    /// Delete a session file.
    pub fn delete(&self, session_id: &str) -> Result<(), DeepseeknovaError> {
        let path = self.session_path(session_id);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// List all session IDs (filenames without extension).
    pub fn list_sessions(&self) -> Result<Vec<String>, DeepseeknovaError> {
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

    /// List sessions with display metadata (turn count, first prompt as
    /// title, file mtime), newest first. Reads every session file in full;
    /// intended for local stores with a modest number of sessions.
    pub fn list_summaries(&self) -> Result<Vec<SessionSummary>, DeepseeknovaError> {
        let mut summaries = Vec::new();
        for id in self.list_sessions()? {
            let path = self.session_path(&id);
            let updated_at_ms = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            // 单个损坏文件不应让整个列表失败：解析失败按零回合会话展示。
            let turns = self.load(&id).unwrap_or_default();
            let title = turns
                .first()
                .map(|t| t.input.prompt.chars().take(80).collect::<String>());
            summaries.push(SessionSummary {
                id,
                turns: turns.len(),
                updated_at_ms,
                title,
                workspace: turns.first().and_then(|t| t.workspace.clone()),
            });
        }
        summaries.sort_by_key(|s| std::cmp::Reverse(s.updated_at_ms));
        Ok(summaries)
    }

    /// 首条用户输入的可读预览：读会话文件**首行**解析出 `input.prompt`，
    /// 去换行、截断到 `max_chars`。空/损坏/不存在文件返回空串。
    ///
    /// 相对 [`SessionStore::list_summaries`] 的轻量替代：TUI 侧边栏每 2s 刷新
    /// 会话列表，全量读每个文件只为拿首句不划算。
    pub fn preview_first_prompt(&self, session_id: &str, max_chars: usize) -> String {
        let path = self.session_path(session_id);
        let Ok(content) = std::fs::read_to_string(&path) else {
            return String::new();
        };
        let Some(first) = content.lines().find(|l| !l.trim().is_empty()) else {
            return String::new();
        };
        let Ok(turn) = serde_json::from_str::<StoredTurn>(first) else {
            return String::new();
        };
        let prompt = turn.input.prompt.replace(['\n', '\r'], "");
        prompt.chars().take(max_chars).collect()
    }

    /// 会话的工作区根路径：读**首行**回合的 `workspace` 字段。轻量替代
    /// [`Self::list_summaries`]（TUI 侧边栏按工作区分组用）。空/损坏/不存在
    /// 返回 `None`（旧格式会话无 workspace 字段）。
    pub fn session_workspace(&self, session_id: &str) -> Option<String> {
        let path = self.session_path(session_id);
        let Ok(content) = std::fs::read_to_string(&path) else {
            return None;
        };
        let first = content.lines().find(|l| !l.trim().is_empty())?;
        let turn: StoredTurn = serde_json::from_str(first).ok()?;
        turn.workspace
    }

    /// 会话 id + 首句预览（TUI 侧边栏用），最新优先。
    /// 预览截断 24 字符；空/无首句的会话预览为空串（渲染回退 id）。
    pub fn list_sessions_with_preview(&self) -> Result<Vec<(String, String)>, DeepseeknovaError> {
        let mut ids = self.list_sessions()?;
        ids.sort();
        ids.reverse(); // id 按时间字典序，最新优先
        Ok(ids
            .into_iter()
            .map(|id| {
                let preview = self.preview_first_prompt(&id, 24);
                (id, preview)
            })
            .collect())
    }

    /// Get the last N turns from a session.
    pub fn last_n(&self, session_id: &str, n: usize) -> Result<Vec<StoredTurn>, DeepseeknovaError> {
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
        Self::build_turn_with_workspace(input, turn_number, messages, output, None)
    }

    /// [`Self::build_turn`] + 记录工作区根路径（会话聚合/按项目查看用）。
    /// `None` 表示未知/全局会话。
    pub fn build_turn_with_workspace(
        input: &RunInput,
        turn_number: u64,
        messages: Vec<Message>,
        output: Option<StoredOutput>,
        workspace: Option<&str>,
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
                    tool_calls: m.tool_calls,
                    reasoning_content: m.reasoning_content,
                })
                .collect(),
            workspace: workspace.map(str::to_string),
        }
    }
}

/// Display metadata for one stored session, see [`SessionStore::list_summaries`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    /// Session id (filename stem).
    pub id: String,
    /// Number of persisted turns.
    pub turns: usize,
    /// File modification time in Unix milliseconds (0 when unavailable).
    pub updated_at_ms: u64,
    /// First turn's prompt truncated to 80 chars; `None` for empty sessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 工作区根路径（首回合记录；`None` = 旧会话/未知）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

/// Generate a fresh chat session id of the form `chat-YYYYMMDD-HHMMSS` (UTC).
///
/// The timestamp layout is lexicographically ordered, so sorting session ids
/// as strings yields chronological order — callers pick the newest session
/// with a plain `max()` without touching the filesystem.
pub fn new_session_id() -> String {
    format!("chat-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S"))
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
            tool_calls: sm.tool_calls.clone(),
            tool_call_id: sm.tool_call_id.clone(),
            reasoning_content: sm.reasoning_content.clone(),
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
        std::env::temp_dir().join(format!("deepseeknova-store-test-{}", std::process::id()))
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
    fn preview_first_prompt_reads_only_first_line() {
        let root = test_root();
        let store = SessionStore::new(root.clone()).unwrap();

        // 多行 prompt：预览去换行。
        let input = RunInput {
            prompt: "第一行\n第二行，这是一个很长的提示用来测试截断".to_string(),
            images: vec![],
            model_override: None,
        };
        let turn = SessionStore::build_turn(&input, 1, vec![], None);
        store.append("pv", &turn).unwrap();

        assert_eq!(store.preview_first_prompt("pv", 10), "第一行第二行，这是一");
        assert_eq!(
            store.preview_first_prompt("pv", 100),
            "第一行第二行，这是一个很长的提示用来测试截断"
        );
        // 不存在/空文件 → 空串。
        assert_eq!(store.preview_first_prompt("nope", 10), "");
        // 最新优先排序 + 预览。
        store.append("pv-old", &turn).unwrap();
        let with_preview = store.list_sessions_with_preview().unwrap();
        assert!(
            with_preview
                .iter()
                .any(|(id, p)| id == "pv" && !p.is_empty()),
            "预览非空: {with_preview:?}"
        );

        let _ = store.delete("pv");
        let _ = store.delete("pv-old");
    }

    #[test]
    fn workspace_round_trips_and_defaults_to_none() {
        let root = test_root();
        let store = SessionStore::new(root.clone()).unwrap();
        let input = RunInput {
            prompt: "hi".into(),
            images: vec![],
            model_override: None,
        };
        // 记录工作区 → 落盘、读回、summary 均可见。
        let turn =
            SessionStore::build_turn_with_workspace(&input, 1, vec![], None, Some("/proj/a"));
        store.append("ws-a", &turn).unwrap();
        assert_eq!(
            store.session_workspace("ws-a").as_deref(),
            Some("/proj/a"),
            "session_workspace 读回工作区"
        );
        let summaries = store.list_summaries().unwrap();
        let sa = summaries.iter().find(|s| s.id == "ws-a").unwrap();
        assert_eq!(sa.workspace.as_deref(), Some("/proj/a"), "summary 带工作区");
        // 旧格式（build_turn 无 workspace）→ None，向后兼容。
        let old = SessionStore::build_turn(&input, 1, vec![], None);
        store.append("ws-old", &old).unwrap();
        assert_eq!(
            store.session_workspace("ws-old"),
            None,
            "旧会话 workspace=None"
        );
        let _ = store.delete("ws-a");
        let _ = store.delete("ws-old");
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
            tool_calls: None,
            reasoning_content: None,
        };
        let msg: Message = (&sm).into();
        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.content, "hello");

        let sm = StoredMessage {
            role: "system".to_string(),
            content: "you are helpful".to_string(),
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: None,
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

    #[test]
    fn new_session_id_has_expected_shape() {
        let id = new_session_id();
        assert!(id.starts_with("chat-"), "unexpected prefix: {id}");
        // chat- (5) + YYYYMMDD (8) + - (1) + HHMMSS (6) = 20 chars.
        assert_eq!(id.len(), 20, "unexpected length: {id}");
        assert!(id[5..].chars().all(|c| c.is_ascii_digit() || c == '-'));
    }

    #[test]
    fn append_then_load_round_trips() {
        let root = test_root().join("roundtrip");
        let _ = std::fs::remove_dir_all(&root);
        let store = SessionStore::new(root.clone()).unwrap();
        let sid = "chat-roundtrip";
        let turn = SessionStore::build_turn(&sample_input(), 1, sample_messages(), None);
        store.append(sid, &turn).unwrap();

        let loaded = store.load(sid).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].turn, 1);
        assert_eq!(loaded[0].input.prompt, "hello world");
        assert_eq!(loaded[0].messages.len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stored_message_roundtrips_tool_calls_and_reasoning() {
        use deepseeknova_core::types::{FunctionCall, ToolCall};
        let msg = Message {
            role: Role::Assistant,
            content: String::new(),
            name: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                ty: "function".into(),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: "{\"path\":\"src/lib.rs\"}".into(),
                },
            }]),
            tool_call_id: None,
            reasoning_content: Some("I should read the file first.".into()),
        };
        let turn = SessionStore::build_turn(&sample_input(), 1, vec![msg], None);
        let json = serde_json::to_string(&turn).unwrap();
        let parsed: StoredTurn = serde_json::from_str(&json).unwrap();
        let restored: Message = (&parsed.messages[0]).into();
        let tcs = restored.tool_calls.expect("tool_calls must survive");
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].id, "call_1");
        assert_eq!(tcs[0].function.name, "read_file");
        assert_eq!(
            restored.reasoning_content.as_deref(),
            Some("I should read the file first.")
        );
    }

    #[test]
    fn legacy_stored_message_without_new_fields_still_parses() {
        let legacy = "{\"role\":\"user\",\"content\":\"hi\"}";
        let sm: StoredMessage = serde_json::from_str(legacy).unwrap();
        assert!(sm.tool_calls.is_none());
        assert!(sm.reasoning_content.is_none());
        let m: Message = (&sm).into();
        assert!(m.tool_calls.is_none());
    }

    #[test]
    fn touch_creates_listable_empty_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf()).unwrap();

        store.touch("chat-empty").unwrap();
        assert!(store
            .list_sessions()
            .unwrap()
            .contains(&"chat-empty".to_string()));
        assert!(store.load("chat-empty").unwrap().is_empty());

        // touch 已有会话不得清空内容。
        let turn = SessionStore::build_turn(&sample_input(), 1, vec![], None);
        store.append("chat-empty", &turn).unwrap();
        store.touch("chat-empty").unwrap();
        assert_eq!(store.load("chat-empty").unwrap().len(), 1);
    }

    #[test]
    fn list_summaries_reports_title_turns_and_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf()).unwrap();

        store.touch("chat-a").unwrap();
        let input = RunInput {
            prompt: "第一轮的提问内容".to_string(),
            images: vec![],
            model_override: None,
        };
        let turn = SessionStore::build_turn(&input, 1, vec![], None);
        store.append("chat-b", &turn).unwrap();

        let summaries = store.list_summaries().unwrap();
        assert_eq!(summaries.len(), 2);
        let a = summaries.iter().find(|s| s.id == "chat-a").unwrap();
        assert_eq!(a.turns, 0);
        assert!(a.title.is_none());
        let b = summaries.iter().find(|s| s.id == "chat-b").unwrap();
        assert_eq!(b.turns, 1);
        assert_eq!(b.title.as_deref(), Some("第一轮的提问内容"));
        assert!(b.updated_at_ms > 0);
    }

    #[test]
    fn resume_primitives_degrade_gracefully_when_empty() {
        // The `--resume` path relies on these two behaviours to fall back to a
        // fresh session instead of erroring when nothing is saved yet.
        let root = test_root().join("empty-resume");
        let _ = std::fs::remove_dir_all(&root);
        let store = SessionStore::new(root.clone()).unwrap();

        // No sessions yet: listing is empty and max() yields no candidate.
        let ids = store.list_sessions().unwrap();
        assert!(ids.is_empty());
        assert!(ids.into_iter().max().is_none());

        // Loading a non-existent session is Ok(empty), not an error.
        let loaded = store.load("chat-does-not-exist").unwrap();
        assert!(loaded.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}

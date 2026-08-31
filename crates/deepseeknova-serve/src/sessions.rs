//! Multi-turn session endpoints for the HTTP server.
//!
//! Sessions reuse [`deepseeknova_store::SessionStore`] (the same JSONL store
//! the CLI/TUI persist chats into, default `~/.deepseeknova/sessions`), so a
//! desktop client sees the sessions created in the terminal and vice versa.
//!
//! Each live session owns one runner built by a [`SessionRunnerFactory`] with
//! a shared conversation history, mirroring the ACP adapter's per-session
//! pattern: consecutive prompts build on prior turns inside the agent, and
//! only one prompt may run per session at a time (concurrent chats get 409).
//!
//! 落盘口径与 TUI 的 `SessionController::record_turn` 一致：每回合只持久化
//! 「用户 prompt + 助手最终正文」两条消息（不含 tool_calls 序列），冷恢复
//! 重放时天然满足消息序列不变量（不会出现孤儿 Tool 结果）。

use deepseeknova_core::runner::Runner;
use deepseeknova_core::{Message, Role, RunInput};
use deepseeknova_store::{SessionStore, StoredOutput, StoredTurn};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Builds the runner for one live session. The `history` argument is the
/// session's shared multi-turn conversation store; wire it into the agent
/// (e.g. via `Agent::with_conversation_history`) before wrapping it as a
/// [`Runner`], so consecutive prompts share context.
pub type SessionRunnerFactory = Arc<
    dyn Fn(
            Arc<Mutex<Vec<Message>>>,
        ) -> Result<Arc<dyn Runner>, deepseeknova_core::DeepseeknovaError>
        + Send
        + Sync,
>;

/// One in-memory session: a runner bound to a shared history plus turn
/// bookkeeping. Kept in [`SessionManager::live`] across requests.
pub(crate) struct LiveSession {
    pub(crate) runner: Arc<dyn Runner>,
    /// Kept alive for the lifetime of the session so multi-turn memory does
    /// not drop after the factory returns (same pattern as ACP's
    /// `AcpSession::_history`).
    _history: Arc<Mutex<Vec<Message>>>,
    /// Guards "one in-flight prompt per session" (claimed via
    /// compare-exchange; released by [`BusyGuard`] even on panic).
    busy: AtomicBool,
    /// Next turn number (1-based), seeded from the stored turn count.
    turn: AtomicU64,
    /// 会话级（跨轮次）prefix cache 累计命中 token。`/v1/sessions/{id}/chat`
    /// 的 SSE `usage` 事件 session_* 字段数据源；仅内存态，会话存活期内
    /// 累计（durable 存储不落这两项，重启会话后从 0 重新累计）。
    pub(crate) session_cache_hit: AtomicU64,
    /// 会话级（跨轮次）prefix cache 累计未命中 token（见上）。
    pub(crate) session_cache_miss: AtomicU64,
}

/// RAII release for [`LiveSession::busy`] so a panicking run task cannot
/// permanently wedge its session.
pub(crate) struct BusyGuard {
    session: Arc<LiveSession>,
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.session.busy.store(false, Ordering::Release);
    }
}

/// Manages persisted sessions and their live runners for the HTTP API.
pub struct SessionManager {
    store: SessionStore,
    factory: SessionRunnerFactory,
    live: Mutex<HashMap<String, Arc<LiveSession>>>,
}

impl SessionManager {
    /// Open (or create) the session store at `dir` and wrap `factory`.
    pub fn open(
        dir: PathBuf,
        factory: SessionRunnerFactory,
    ) -> Result<Self, deepseeknova_core::DeepseeknovaError> {
        Ok(Self {
            store: SessionStore::new(dir)?,
            factory,
            live: Mutex::new(HashMap::new()),
        })
    }

    /// List stored sessions (newest first) with display metadata.
    pub fn list(
        &self,
    ) -> Result<Vec<deepseeknova_store::SessionSummary>, deepseeknova_core::DeepseeknovaError> {
        self.store.list_summaries()
    }

    /// Create a fresh empty session and return its id. Same-second collisions
    /// with existing ids get a numeric suffix so two rapid creates never
    /// share a file.
    pub async fn create(&self) -> Result<String, deepseeknova_core::DeepseeknovaError> {
        let base = deepseeknova_store::new_session_id();
        let live = self.live.lock().await;
        let existing: std::collections::HashSet<String> =
            self.store.list_sessions()?.into_iter().collect();
        let mut id = base.clone();
        let mut n = 0u32;
        while existing.contains(&id) || live.contains_key(&id) {
            n += 1;
            id = format!("{base}-{n}");
        }
        self.store.touch(&id)?;
        Ok(id)
    }

    /// Load a session's persisted turns.
    pub fn history(
        &self,
        id: &str,
    ) -> Result<Option<Vec<StoredTurn>>, deepseeknova_core::DeepseeknovaError> {
        if !self.exists(id)? {
            return Ok(None);
        }
        Ok(Some(self.store.load(id)?))
    }

    /// Delete a session (its live runner and its file). Returns `false` when
    /// the session does not exist. A busy session cannot be deleted.
    pub async fn delete(
        &self,
        id: &str,
    ) -> Result<Result<bool, Busy>, deepseeknova_core::DeepseeknovaError> {
        let mut live = self.live.lock().await;
        if let Some(session) = live.get(id) {
            if session.busy.load(Ordering::Acquire) {
                return Ok(Err(Busy));
            }
        }
        if !self.exists(id)? {
            live.remove(id);
            return Ok(Ok(false));
        }
        live.remove(id);
        self.store.delete(id)?;
        Ok(Ok(true))
    }

    fn exists(&self, id: &str) -> Result<bool, deepseeknova_core::DeepseeknovaError> {
        Ok(self.store.list_sessions()?.iter().any(|s| s == id))
    }

    /// Fetch (or lazily restore) the live session for `id`, claiming its
    /// busy flag for one prompt. `None` when the session does not exist on
    /// disk; `Err(Busy)` when another prompt is already in flight.
    pub(crate) async fn claim_for_chat(
        &self,
        id: &str,
    ) -> Result<
        Option<Result<(Arc<LiveSession>, BusyGuard), Busy>>,
        deepseeknova_core::DeepseeknovaError,
    > {
        let mut live = self.live.lock().await;
        let session = match live.get(id) {
            Some(s) => Arc::clone(s),
            None => {
                if !self.exists(id)? {
                    return Ok(None);
                }
                let turns = self.store.load(id)?;
                let mut messages = Vec::new();
                for t in &turns {
                    for m in &t.messages {
                        messages.push(Message::from(m));
                    }
                }
                let history = Arc::new(Mutex::new(messages));
                let runner = (self.factory)(Arc::clone(&history))?;
                let session = Arc::new(LiveSession {
                    runner,
                    _history: history,
                    busy: AtomicBool::new(false),
                    turn: AtomicU64::new(turns.len() as u64),
                    session_cache_hit: AtomicU64::new(0),
                    session_cache_miss: AtomicU64::new(0),
                });
                live.insert(id.to_string(), Arc::clone(&session));
                session
            }
        };
        drop(live);

        match session
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => {
                let guard = BusyGuard {
                    session: Arc::clone(&session),
                };
                Ok(Some(Ok((session, guard))))
            }
            Err(_) => Ok(Some(Err(Busy))),
        }
    }

    /// Persist one completed turn (mirrors the TUI controller's record shape:
    /// user prompt + assistant final text only).
    pub(crate) fn record_turn(
        &self,
        session: &LiveSession,
        id: &str,
        prompt: &str,
        output_text: &str,
        model: Option<String>,
    ) {
        let turn_no = session.turn.fetch_add(1, Ordering::AcqRel) + 1;
        let messages = vec![
            Message {
                role: Role::User,
                content: prompt.to_string(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                reasoning_signature: None,
                usage: None,
            },
            Message {
                role: Role::Assistant,
                content: output_text.to_string(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                reasoning_signature: None,
                usage: None,
            },
        ];
        let input = RunInput {
            prompt: prompt.to_string(),
            images: Vec::new(),
            model_override: model,
        };
        let turn = SessionStore::build_turn(
            &input,
            turn_no,
            messages,
            Some(StoredOutput {
                text: output_text.to_string(),
                tool_calls: Vec::new(),
            }),
        );
        if let Err(e) = self.store.append(id, &turn) {
            tracing::warn!("session {id}: failed to persist turn {turn_no}: {e}");
        }
    }
}

/// Marker for "another prompt is already running in this session".
#[derive(Debug)]
pub struct Busy;

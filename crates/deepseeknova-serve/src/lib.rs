//! HTTP server for deepseeknova — exposes Runner via a REST + SSE API.
//!
//! ```no_run
//! use deepseeknova_serve::Server;
//! # use std::sync::Arc;
//! # struct DummyRunner;
//! # #[async_trait::async_trait]
//! # impl deepseeknova_core::runner::Runner for DummyRunner {
//! #     async fn run_stream(&self, _input: deepseeknova_core::runner::RunInput) -> Result<deepseeknova_core::runner::RunEventStream, deepseeknova_core::DeepseeknovaError> {
//! #         unreachable!()
//! #     }
//! # }
//! # #[tokio::main]
//! # async fn main() -> Result<(), deepseeknova_core::DeepseeknovaError> {
//! # let runner = Arc::new(DummyRunner);
//! let server = Server::new(runner);
//! server.serve("127.0.0.1:3000").await?;
//! # Ok(())
//! # }
//! ```

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        clippy::dbg_macro
    )
)]

mod acp;
mod sessions;

pub use acp::{run_acp_io, serve_acp, AcpRunnerFactory};
pub use sessions::{Busy, SessionManager, SessionRunnerFactory};

use axum::extract::State;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::http::{HeaderValue, Method};
use axum::response::sse::{Event, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use deepseeknova_core::runner::{ApprovalResponder, RunEvent, RunInput, Runner};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{oneshot, Mutex};
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};

// ── Durable run store ─────────────────────────────────────────

/// Status of a persisted run.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    /// The run is currently executing.
    Running,
    /// The run completed successfully.
    Done,
    /// The run failed with an error.
    Failed,
    /// The run was paused and can be resumed later.
    Paused,
    /// Server restarted while the run was still running; it can be resumed.
    Interrupted,
}

/// A run record persisted to disk so long tasks survive process restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    /// Unique run identifier (also used as the persisted file name).
    pub id: String,
    /// The original user prompt that started the run.
    pub prompt: String,
    /// Model requested for the run, if any.
    pub model: Option<String>,
    /// Creation timestamp in milliseconds since the Unix epoch.
    pub created_at_ms: u64,
    /// Current lifecycle status of the run.
    pub status: RunStatus,
    /// Short result summary produced when the run completes, if any.
    pub summary: Option<String>,
    /// Error message when the run failed, else `None`.
    pub error: Option<String>,
}

/// JSON-file-backed store (`<dir>/<id>.json`), atomic via temp+rename.
pub struct DurableRuns {
    dir: PathBuf,
    /// 串行化“状态迁移”（claim / mark_interrupted），避免并发 resume 的
    /// check-then-act 竞态。std Mutex 只做短临界区，不跨 await。
    state: std::sync::Mutex<()>,
}

impl DurableRuns {
    /// Open (creating it if needed) the directory backing the run store.
    pub fn open(dir: PathBuf) -> Result<Self, deepseeknova_core::DeepseeknovaError> {
        fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            state: std::sync::Mutex::new(()),
        })
    }

    /// The directory where run records are persisted.
    pub fn dir(&self) -> &PathBuf {
        &self.dir
    }

    fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// Atomically persist a run record (write temp file, then rename).
    pub fn save(&self, record: &RunRecord) -> Result<(), deepseeknova_core::DeepseeknovaError> {
        let path = self.path(&record.id);
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(record)?)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Load a run record by id; returns `None` if no such record exists.
    pub fn load(
        &self,
        id: &str,
    ) -> Result<Option<RunRecord>, deepseeknova_core::DeepseeknovaError> {
        match fs::read_to_string(self.path(id)) {
            Ok(text) => Ok(Some(serde_json::from_str(&text)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// List all persisted run records, newest first.
    pub fn list(&self) -> Result<Vec<RunRecord>, deepseeknova_core::DeepseeknovaError> {
        let mut records = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json")
                || path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(".json.tmp"))
            {
                continue;
            }
            if let Ok(text) = fs::read_to_string(&path) {
                if let Ok(record) = serde_json::from_str::<RunRecord>(&text) {
                    records.push(record);
                }
            }
        }
        records.sort_by_key(|r| std::cmp::Reverse(r.created_at_ms));
        Ok(records)
    }

    /// On server startup: mark records left in `Running` as `Interrupted`.
    /// Returns the number of records touched.
    pub fn mark_interrupted(&self) -> Result<usize, deepseeknova_core::DeepseeknovaError> {
        let _guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut touched = 0;
        for mut record in self.list()? {
            if record.status == RunStatus::Running {
                record.status = RunStatus::Interrupted;
                record.error = Some("server restarted while run was in progress".to_string());
                self.save(&record)?;
                touched += 1;
            }
        }
        Ok(touched)
    }
}

/// Outcome of atomically claiming a run for (re-)execution.
#[derive(Debug)]
pub enum ClaimResult {
    /// Successfully transitioned to `Running`; caller may start the run.
    Claimed(RunRecord),
    /// The run already exists and is `Running`.
    AlreadyRunning,
    /// No record with this id exists.
    NotFound,
}

impl DurableRuns {
    /// Atomically transition a non-running record to `Running`. This is the
    /// only path that should start a run, so two concurrent resumes cannot
    /// both pass the status check.
    pub fn claim(&self, id: &str) -> Result<ClaimResult, deepseeknova_core::DeepseeknovaError> {
        let _guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(mut record) = self.load(id)? else {
            return Ok(ClaimResult::NotFound);
        };
        if record.status == RunStatus::Running {
            return Ok(ClaimResult::AlreadyRunning);
        }
        record.status = RunStatus::Running;
        record.error = None;
        self.save(&record)?;
        Ok(ClaimResult::Claimed(record))
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ── Public API ──────────────────────────────────────────────────

/// Shared map of pending approval requests keyed by request id. The agent's
/// [`ServerApprovalResponder`] inserts a `oneshot` sender per `Ask`; the
/// `POST /v1/approval` route resolves it when the client answers.
pub type PendingApprovals = Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>;

/// Create an empty pending-approvals map to share between a [`Server`] and the
/// [`ServerApprovalResponder`] attached to its runner.
pub fn new_pending_approvals() -> PendingApprovals {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Approval responder bridging the agent's permission-gate `Ask` decisions to
/// HTTP clients: registers a `oneshot` in the shared map and awaits the
/// `POST /v1/approval` answer. A dropped stream resolves to deny (no hang).
pub struct ServerApprovalResponder {
    pending: PendingApprovals,
}

impl ServerApprovalResponder {
    /// Create a responder that registers pending approvals in the shared map.
    pub fn new(pending: PendingApprovals) -> Self {
        Self { pending }
    }
}

#[async_trait::async_trait]
impl ApprovalResponder for ServerApprovalResponder {
    async fn request(&self, id: &str, _title: &str, _description: Option<&str>) -> bool {
        let (tx, rx) = oneshot::channel::<bool>();
        self.pending.lock().await.insert(id.to_string(), tx);
        rx.await.unwrap_or(false)
    }
}

/// An HTTP server that wraps a [`Runner`] and exposes it via REST + SSE.
pub struct Server {
    /// The agent runner this server wraps and exposes over HTTP.
    pub runner: Arc<dyn Runner>,
    pending: PendingApprovals,
    /// Metrics/diagnose 输出目录（`<dir>/<id>.scorecard.json` 与
    /// `<dir>/diagnose/<id>.json` 的读取根目录）。`None` 时相关端点返回 404。
    metrics_dir: Option<PathBuf>,
    /// Durable run store; `None` disables `/v1/runs` and run persistence.
    runs: Option<Arc<DurableRuns>>,
    /// Multi-turn session manager; `None` disables `/v1/sessions*` endpoints.
    sessions: Option<Arc<SessionManager>>,
    /// Bearer token required on every `/v1/*` route; `None` = no auth.
    /// `/health` stays unauthenticated so liveness probes work.
    auth_token: Option<Arc<str>>,
}

impl Server {
    /// Create a server with default options (no auth, no persistence, no sessions).
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self {
            runner,
            pending: new_pending_approvals(),
            metrics_dir: None,
            runs: None,
            sessions: None,
            auth_token: None,
        }
    }

    /// Create a server sharing an existing pending-approvals map, so the
    /// runner's [`ServerApprovalResponder`] and the `/v1/approval` route
    /// resolve against the same map.
    pub fn with_pending(runner: Arc<dyn Runner>, pending: PendingApprovals) -> Self {
        Self {
            runner,
            pending,
            metrics_dir: None,
            runs: None,
            sessions: None,
            auth_token: None,
        }
    }

    /// Point the server at the metrics/diagnose output directory (the same
    /// `[metrics] dir` the runtime writes scorecards and diagnose reports
    /// into). Enables `GET /v1/sessions/{id}/diagnose`,
    /// `GET /v1/sessions/{id}/scorecard` and `GET /v1/metrics/scorecards`.
    /// Defaults to `None` (endpoints return 404).
    ///
    /// 注意：三个 metrics 端点默认无 token 时开放，仅供本地/可信网络使用；
    /// 配置 [`Self::with_auth_token`]（CLI `--token`）后随 `/v1` 全部受
    /// bearer token 保护，装配到公网地址时应启用 token。
    pub fn with_metrics_dir(mut self, dir: PathBuf) -> Self {
        self.metrics_dir = Some(dir);
        self
    }

    /// Enable durable run persistence + `/v1/runs` endpoints. Running records
    /// left by a previous process are marked `interrupted` so they can be
    /// resumed. Failure to open the directory only warns (server still runs).
    pub fn with_runs_dir(mut self, dir: PathBuf) -> Self {
        match DurableRuns::open(dir.clone()) {
            Ok(store) => {
                match store.mark_interrupted() {
                    Ok(n) if n > 0 => {
                        tracing::info!("durable runs: marked {n} interrupted run(s) for resume")
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("durable runs: mark_interrupted failed: {e}"),
                }
                self.runs = Some(Arc::new(store));
            }
            Err(e) => tracing::warn!(
                "durable runs: cannot open {} ({e}); run persistence disabled",
                dir.display()
            ),
        }
        self
    }

    /// Configured metrics/diagnose output directory, if any.
    pub fn metrics_dir(&self) -> Option<&PathBuf> {
        self.metrics_dir.as_ref()
    }

    /// Enable multi-turn session endpoints backed by a [`SessionManager`]
    /// (persisted in the same JSONL store the CLI/TUI use). The factory
    /// builds a fresh runner per session with its shared conversation
    /// history, so consecutive prompts in one session keep context.
    pub fn with_sessions(mut self, sessions: Arc<SessionManager>) -> Self {
        self.sessions = Some(sessions);
        self
    }

    /// Require `Authorization: Bearer <token>` on every `/v1/*` route.
    /// `None` (default) keeps the server open — only use on trusted
    /// loopback. `/health` is exempt so probes keep working.
    pub fn with_auth_token(mut self, token: Option<String>) -> Self {
        self.auth_token = token.map(Arc::from);
        self
    }

    /// Start the server and block until it shuts down.
    pub async fn serve(self, addr: &str) -> Result<(), deepseeknova_core::DeepseeknovaError> {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!("deepseeknova serve listening on {addr}");
        axum::serve(listener, self.into_router()).await?;
        Ok(())
    }

    /// Build the axum router for this server (no listener). Extracted from
    /// [`Self::serve`] so integration tests can bind an ephemeral port.
    pub fn into_router(self) -> axum::Router {
        let auth = self.auth_token.clone();
        // /v1 子路由被 auth layer 包裹；/health 留在外侧，保持探活免认证。
        let v1 = Router::new()
            .route("/v1/chat", post(chat))
            .route("/v1/runs", get(list_runs))
            .route("/v1/runs/{id}/resume", post(resume_run))
            .route("/v1/approval", post(approval))
            .route("/v1/sessions/{id}/diagnose", get(session_diagnose))
            .route("/v1/sessions/{id}/scorecard", get(session_scorecard))
            .route("/v1/metrics/scorecards", get(metrics_scorecards))
            .route("/v1/sessions", get(sessions_list).post(sessions_create))
            .route(
                "/v1/sessions/{id}",
                get(sessions_history).delete(sessions_delete),
            )
            .route("/v1/sessions/{id}/chat", post(sessions_chat))
            .layer(loopback_cors_layer())
            .with_state(Arc::new(self));

        let v1 = match auth {
            Some(token) => v1.layer(axum::middleware::from_fn_with_state(
                token,
                require_bearer_token,
            )),
            None => v1,
        };

        Router::new().route("/health", get(health)).merge(v1)
    }
}

/// CORS 策略：仅放行 loopback 来源（`localhost` / `127.0.0.1` / `::1`），
/// 端口不限（本机任意 dev server 端口均视为可信 loopback）。
///
/// 背景（P0）：serve 默认无 token 开放于 127.0.0.1。此前 `allow_origin(Any)`
/// 使任意网页可跨源读取 SSE/会话/评分卡，并可经 `/v1/approval` 自答审批。
/// 收窄后：
/// - 无 `Origin` 头的请求（同源页面加载、curl、非浏览器客户端）不受影响；
/// - 跨源浏览器请求（`https://evil.example` 等）的响应不含
///   `Access-Control-Allow-Origin`，浏览器拒绝读取 —— 关闭自审批与数据外带窗口；
/// - loopback 来源放行（本地 CLI/TUI/浏览器 dev server 均可直接消费）。
///
/// 更严格的做法（精确 origin 白名单 + 配置化）留作后续收紧项；若未来引入
/// 非 loopback 的受信客户端，需在此加入白名单。
fn loopback_cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(
            |origin: &HeaderValue, _parts: &Parts| is_loopback_origin(origin),
        ))
        .allow_methods(AllowMethods::list([
            Method::GET,
            Method::POST,
            Method::DELETE,
        ]))
        .allow_headers(AllowHeaders::list([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
        ]))
}

/// `Origin` 头是否为可信 loopback 来源。格式 `scheme://host[:port]`，仅接受
/// http/https；host 白名单为 `localhost` / `127.0.0.1` / IPv6 `::1`（含 `[::1]`
/// 括号形态）。`null` 与空 origin 一律拒绝（非浏览器/异常形态按不可信处理）。
fn is_loopback_origin(origin: &HeaderValue) -> bool {
    origin
        .to_str()
        .ok()
        .and_then(origin_host)
        .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "::1"))
}

/// 从 `Origin` 头值解析出裸 host（去掉 scheme、端口、路径）。非法 scheme、
/// 无 `://` 分隔时返回 `None`。
fn origin_host(origin: &str) -> Option<&str> {
    let (scheme, rest) = origin.split_once("://")?;
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    let authority = rest.split('/').next().unwrap_or("");
    Some(if let Some(inner) = authority.strip_prefix('[') {
        // IPv6：`[::1]:port` → `::1`
        inner.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    })
}

/// Middleware guarding every `/v1/*` route when `--token` is configured.
/// `/health` sits outside the layered router, so it stays probe-friendly.
async fn require_bearer_token(
    State(token): State<Arc<str>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let ok = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == format!("Bearer {token}"));
    if ok {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "unauthorized" })),
        )
            .into_response()
    }
}

// ── Routes ─────────────────────────────────────────────────────

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

/// Resolve a pending approval request (paired with the `approval_request` SSE
/// event emitted during a `/v1/chat` stream).
async fn approval(
    State(state): State<Arc<Server>>,
    Json(req): Json<ApprovalRequestBody>,
) -> Json<serde_json::Value> {
    let mut pending = state.pending.lock().await;
    if let Some(tx) = pending.remove(&req.id) {
        let _ = tx.send(req.approved);
        Json(serde_json::json!({ "status": "ok" }))
    } else {
        Json(serde_json::json!({ "status": "not_found" }))
    }
}

#[derive(Debug, Deserialize)]
struct ApprovalRequestBody {
    id: String,
    approved: bool,
}

/// Concrete SSE stream type shared by `/v1/chat` and `/v1/runs/{id}/resume`.
type RunSse = Sse<futures::channel::mpsc::UnboundedReceiver<Result<Event, Infallible>>>;

async fn chat(State(state): State<Arc<Server>>, Json(req): Json<ChatRequest>) -> RunSse {
    // Validate prompt
    if req.prompt.trim().is_empty() {
        let error_event = Event::default()
            .event("error")
            .data("prompt must not be empty");
        let (tx, rx) = futures::channel::mpsc::unbounded();
        let _ = tx.unbounded_send(Ok(error_event));
        return Sse::new(rx);
    }
    const MAX_PROMPT_LEN: usize = 32_000;
    if req.prompt.len() > MAX_PROMPT_LEN {
        let error_event = Event::default().event("error").data(format!(
            "prompt exceeds max length ({MAX_PROMPT_LEN} chars)"
        ));
        let (tx, rx) = futures::channel::mpsc::unbounded();
        let _ = tx.unbounded_send(Ok(error_event));
        return Sse::new(rx);
    }

    let input = RunInput {
        prompt: req.prompt,
        images: req.images.unwrap_or_default(),
        model_override: req.model,
    };

    let run_id = state
        .runs
        .as_ref()
        .map(|_| uuid::Uuid::new_v4().to_string());
    stream_input(state, input, run_id)
}

/// Shared SSE runner used by both `/v1/chat` and `/v1/runs/{id}/resume`.
/// When a durable store is configured the run is persisted as `running`
/// before launch and updated to done/failed/paused when the stream ends.
fn stream_input(state: Arc<Server>, input: RunInput, run_id: Option<String>) -> RunSse {
    start_run_record(state.runs.as_ref(), &input, run_id.as_deref());
    let (tx, rx) = futures::channel::mpsc::unbounded::<Result<Event, Infallible>>();

    tokio::spawn(async move {
        let mut done_text = String::new();
        let mut failed: Option<String> = None;
        let mut paused = false;
        let mut client_gone = false;
        match state.runner.run_stream(input.clone()).await {
            Ok(mut stream) => {
                use tokio_stream::StreamExt;
                while let Some(event) = stream.next().await {
                    let sse_event = match map_run_event(
                        event,
                        &mut done_text,
                        &mut failed,
                        &mut paused,
                        run_id.as_deref(),
                    ) {
                        Some(e) => e,
                        None => continue, // skipped (accumulated) events
                    };
                    if !client_gone && tx.unbounded_send(sse_event).is_err() {
                        // 客户端断开：停止发送但继续消费 stream，让 run 跑完并
                        // 正确落盘，而不是取消任务、把半截结果标成 Done。
                        client_gone = true;
                    }
                }
            }
            Err(e) => {
                let text = e.to_string();
                failed = Some(text.clone());
                let _ = tx.unbounded_send(Ok(Event::default().event("error").data(text)));
            }
        }
        finish_run_record(
            state.runs.as_ref(),
            run_id.as_deref(),
            &input,
            done_text,
            failed,
            paused,
        );
        // Channel closed when tx is dropped — SSE stream ends.
    });

    Sse::new(rx)
}

/// Map one runner event to its SSE wire form. `None` marks events that are
/// accumulated into a later event (`TurnComplete`, `ToolCallDelta`) and
/// should be skipped by the caller. State (`done_text` / `failed` / `paused`)
/// is threaded through so stream enders can finalize durable records.
///
/// `session_id` 是当前 run 的关联键（`/v1/sessions/{id}/chat` 为会话 id，
/// `/v1/chat` 与 `/v1/runs/{id}/resume` 为 durable run id），透传进 `done`
/// 事件，供前端据此拉取该 run 的评分卡/诊断。
fn map_run_event(
    event: Result<RunEvent, deepseeknova_core::DeepseeknovaError>,
    done_text: &mut String,
    failed: &mut Option<String>,
    paused: &mut bool,
    session_id: Option<&str>,
) -> Option<Result<Event, Infallible>> {
    match event {
        Ok(RunEvent::TextDelta(text)) => Some(Ok(Event::default().event("text").data(text))),
        Ok(RunEvent::ReasoningDelta { text, .. }) => {
            Some(Ok(Event::default().event("reasoning").data(text)))
        }
        Ok(RunEvent::ToolCallStart { id, name }) => Some(Ok(Event::default()
            .event("tool_start")
            .data(serde_json::json!({ "id": id, "name": name }).to_string()))),
        Ok(RunEvent::ToolCallEnd {
            id,
            name,
            arguments,
        }) => Some(Ok(Event::default().event("tool_end").data(
            serde_json::json!({ "id": id, "name": name, "arguments": arguments }).to_string(),
        ))),
        Ok(RunEvent::ToolResult { call_id, result }) => Some(Ok(Event::default()
            .event("tool_result")
            .data(serde_json::json!({ "call_id": call_id, "result": result }).to_string()))),
        Ok(RunEvent::Usage(u)) => Some(Ok(Event::default()
            .event("usage")
            .data(serde_json::to_string(&u).unwrap_or_default()))),
        Ok(RunEvent::Done(output)) => {
            *done_text = output.text.clone();
            let json = serde_json::json!({
                "text": output.text,
                "tool_calls": output.tool_calls.iter().map(|tc| serde_json::json!({
                    "id": tc.id,
                    "name": tc.function.name,
                    "arguments": tc.function.arguments,
                })).collect::<Vec<_>>(),
                "usage": output.usage,
                "session_id": session_id,
            });
            Some(Ok(Event::default().event("done").data(json.to_string())))
        }
        Ok(RunEvent::TurnComplete) => None,
        Ok(RunEvent::ToolCallDelta { .. }) => None, // accumulated into ToolCallEnd
        Ok(RunEvent::ApprovalRequest {
            id,
            title,
            description,
        }) => {
            let json = serde_json::json!({
                "id": id,
                "title": title,
                "description": description,
            });
            Some(Ok(Event::default()
                .event("approval_request")
                .data(json.to_string())))
        }
        Ok(RunEvent::Paused { reason, session_id }) => {
            *paused = true;
            let json = serde_json::json!({
                "reason": reason,
                "session_id": session_id,
            });
            Some(Ok(Event::default().event("paused").data(json.to_string())))
        }
        Ok(RunEvent::Verification {
            command,
            passed,
            summary,
        }) => {
            let json = serde_json::json!({
                "command": command,
                "passed": passed,
                "summary": summary,
            });
            Some(Ok(Event::default()
                .event("verification")
                .data(json.to_string())))
        }
        Ok(RunEvent::QualityFinding(finding)) => Some(Ok(Event::default()
            .event("quality_finding")
            .data(serde_json::to_string(&finding).unwrap_or_default()))),
        // 协议增强：阶段迁移 / 门控违规 / drift 事件最小序列化透传
        // （前端可按 kind 渲染；WireEvent 字段名为
        // transition/violation/drift，见 core::runner）。
        Ok(RunEvent::PhaseTransition { transition }) => Some(Ok(Event::default()
            .event("phase_transition")
            .data(serde_json::to_string(&transition).unwrap_or_default()))),
        Ok(RunEvent::GateViolation(violation)) => Some(Ok(Event::default()
            .event("gate_violation")
            .data(serde_json::to_string(&violation).unwrap_or_default()))),
        Ok(RunEvent::DriftFinding(drift)) => Some(Ok(Event::default()
            .event("drift_finding")
            .data(serde_json::to_string(&drift).unwrap_or_default()))),
        Err(e) => {
            let text = e.to_string();
            *failed = Some(text.clone());
            Some(Ok(Event::default().event("error").data(text)))
        }
    }
}

/// Write a `running` record before the run starts (or reset a resumed one).
fn start_run_record(runs: Option<&Arc<DurableRuns>>, input: &RunInput, run_id: Option<&str>) {
    let (Some(runs), Some(id)) = (runs, run_id) else {
        return;
    };
    let mut record = runs.load(id).ok().flatten().unwrap_or_else(|| RunRecord {
        id: id.to_string(),
        prompt: input.prompt.clone(),
        model: input.model_override.clone(),
        created_at_ms: now_ms(),
        status: RunStatus::Running,
        summary: None,
        error: None,
    });
    record.prompt = input.prompt.clone();
    record.model = input.model_override.clone();
    record.status = RunStatus::Running;
    record.error = None;
    if let Err(e) = runs.save(&record) {
        tracing::warn!("durable runs: failed to persist run {id}: {e}");
    }
}

/// Update the persisted record at stream end.
fn finish_run_record(
    runs: Option<&Arc<DurableRuns>>,
    run_id: Option<&str>,
    input: &RunInput,
    done_text: String,
    failed: Option<String>,
    paused: bool,
) {
    let (Some(runs), Some(id)) = (runs, run_id) else {
        return;
    };
    let status = if failed.is_some() {
        RunStatus::Failed
    } else if paused {
        RunStatus::Paused
    } else {
        RunStatus::Done
    };
    let summary = if done_text.is_empty() {
        None
    } else {
        Some(done_text.chars().take(2000).collect())
    };
    let Some(mut record) = runs.load(id).ok().flatten() else {
        return;
    };
    record.prompt = input.prompt.clone();
    record.model = input.model_override.clone();
    record.status = status;
    record.summary = summary;
    record.error = failed;
    if let Err(e) = runs.save(&record) {
        tracing::warn!("durable runs: failed to finalize run {id}: {e}");
    }
}

/// `GET /v1/runs` — list persisted runs (newest first).
async fn list_runs(
    State(state): State<Arc<Server>>,
) -> Result<Json<Vec<RunRecord>>, (StatusCode, String)> {
    let runs = state.runs.as_ref().ok_or((
        StatusCode::NOT_FOUND,
        "durable runs not configured".to_string(),
    ))?;
    runs.list()
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// `POST /v1/runs/{id}/resume` — re-run a persisted run from its stored
/// prompt/model, streaming the same SSE event shape as `/v1/chat`.
async fn resume_run(
    State(state): State<Arc<Server>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<RunSse, (StatusCode, String)> {
    let runs = state.runs.as_ref().ok_or((
        StatusCode::NOT_FOUND,
        "durable runs not configured".to_string(),
    ))?;
    if !valid_session_id(&id) {
        return Err((StatusCode::NOT_FOUND, "not found".to_string()));
    }
    let record = match runs
        .claim(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    {
        ClaimResult::Claimed(record) => record,
        ClaimResult::AlreadyRunning => {
            return Err((StatusCode::CONFLICT, "run already running".to_string()));
        }
        ClaimResult::NotFound => return Err((StatusCode::NOT_FOUND, "run not found".to_string())),
    };
    let input = RunInput {
        prompt: record.prompt.clone(),
        images: Vec::new(),
        model_override: record.model.clone(),
    };
    Ok(stream_input(state, input, Some(record.id)))
}

/// 会话 id 合法性校验：复用 `SessionStore` 的共享校验（`[A-Za-z0-9_-]`，
/// 长度 ≤ 128），防 URL path 拼接路径穿越，与 CLI/存储层同一契约。
fn valid_session_id(id: &str) -> bool {
    deepseeknova_store::is_valid_session_id(id)
}

/// 读取 metrics 根目录下单个文件并解析为 JSON。`metrics_dir` 未配置、
/// 文件缺失或 id 非法 → 404；文件内容非法 JSON → 500。
fn read_metrics_json(
    dir: Option<&PathBuf>,
    rel: PathBuf,
) -> Result<serde_json::Value, (StatusCode, String)> {
    let Some(dir) = dir else {
        return Err((StatusCode::NOT_FOUND, "not found".into()));
    };
    match std::fs::read_to_string(dir.join(rel)) {
        Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(v) => Ok(v),
            Err(_) => Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "stored file is not valid JSON".into(),
            )),
        },
        Err(_) => Err((StatusCode::NOT_FOUND, "not found".into())),
    }
}

/// `GET /v1/sessions/{id}/diagnose` — 读 `<dir>/diagnose/<id>.json`。
/// 返回 200 JSON 或 404（未配置 metrics dir / 无该会话诊断报告）。
///
/// 注意：配置 `--token` 后受保护；默认（无 token）仅供本地/可信网络使用。
async fn session_diagnose(
    State(state): State<Arc<Server>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !valid_session_id(&id) {
        return Err((StatusCode::NOT_FOUND, "not found".into()));
    }
    let value = read_metrics_json(
        state.metrics_dir.as_ref(),
        PathBuf::from("diagnose").join(format!("{id}.json")),
    )?;
    Ok(Json(value))
}

/// `GET /v1/sessions/{id}/scorecard` — 读 `<dir>/<id>.scorecard.json`。
/// 返回 200 JSON 或 404（未配置 metrics dir / 无该会话评分卡）。
///
/// 注意：配置 `--token` 后受保护；默认（无 token）仅供本地/可信网络使用。
async fn session_scorecard(
    State(state): State<Arc<Server>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !valid_session_id(&id) {
        return Err((StatusCode::NOT_FOUND, "not found".into()));
    }
    let value = read_metrics_json(
        state.metrics_dir.as_ref(),
        PathBuf::from(format!("{id}.scorecard.json")),
    )?;
    Ok(Json(value))
}

/// `GET /v1/metrics/scorecards` — 扫描 `<dir>/*.scorecard.json` 并返回聚合
/// （count / 各维均值 / overall / 最差维度）。未配置 metrics dir → 404。
///
/// 注意：配置 `--token` 后受保护；默认（无 token）仅供本地/可信网络使用。
async fn metrics_scorecards(
    State(state): State<Arc<Server>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let Some(dir) = &state.metrics_dir else {
        return Err((StatusCode::NOT_FOUND, "metrics dir not configured".into()));
    };
    let cards = deepseeknova_metrics::list_scorecards(dir);
    let aggregate = deepseeknova_metrics::aggregate_scorecards(&cards);
    Ok(Json(serde_json::json!({
        "count": aggregate.count,
        "aggregate": aggregate,
    })))
}

// ── Multi-turn session endpoints ──────────────────────────────

/// Resolve the session manager or 404 when sessions are not configured.
fn sessions_state(state: &Server) -> Result<Arc<SessionManager>, (StatusCode, String)> {
    state
        .sessions
        .clone()
        .ok_or((StatusCode::NOT_FOUND, "sessions not configured".into()))
}

/// `GET /v1/sessions` — list stored sessions (newest first).
async fn sessions_list(
    State(state): State<Arc<Server>>,
) -> Result<Json<Vec<deepseeknova_store::SessionSummary>>, (StatusCode, String)> {
    let sessions = sessions_state(&state)?;
    sessions
        .list()
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// `POST /v1/sessions` — create a fresh empty session, returns its id.
async fn sessions_create(
    State(state): State<Arc<Server>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let sessions = sessions_state(&state)?;
    let id = sessions
        .create()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "id": id })))
}

/// `GET /v1/sessions/{id}` — stored turns of one session (oldest first).
async fn sessions_history(
    State(state): State<Arc<Server>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<Vec<deepseeknova_store::StoredTurn>>, (StatusCode, String)> {
    let sessions = sessions_state(&state)?;
    let turns = sessions
        .history(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "session not found".into()))?;
    Ok(Json(turns))
}

/// `DELETE /v1/sessions/{id}` — delete a session. 409 when a prompt is
/// currently in flight.
async fn sessions_delete(
    State(state): State<Arc<Server>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let sessions = sessions_state(&state)?;
    let deleted = sessions
        .delete(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|_| (StatusCode::CONFLICT, "session busy".into()))?;
    if deleted {
        Ok(Json(serde_json::json!({ "deleted": true })))
    } else {
        Err((StatusCode::NOT_FOUND, "session not found".into()))
    }
}

/// `POST /v1/sessions/{id}/chat` — run one prompt in a session, streaming the
/// same SSE event set as `/v1/chat` but bound to the session's runner (which
/// carries the shared conversation history). On `done` the turn is persisted
/// (user prompt + assistant final text, mirroring the TUI controller).
/// 409 when another prompt is already running in this session.
async fn sessions_chat(
    State(state): State<Arc<Server>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<ChatRequest>,
) -> Result<RunSse, (StatusCode, String)> {
    if req.prompt.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "prompt must not be empty".into()));
    }
    const MAX_PROMPT_LEN: usize = 32_000;
    if req.prompt.len() > MAX_PROMPT_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("prompt exceeds max length ({MAX_PROMPT_LEN} chars)"),
        ));
    }

    let sessions = sessions_state(&state)?;
    let claimed = sessions
        .claim_for_chat(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let Some(claimed) = claimed else {
        return Err((StatusCode::NOT_FOUND, "session not found".into()));
    };
    let (session, guard) = claimed.map_err(|_| (StatusCode::CONFLICT, "session busy".into()))?;

    let input = RunInput {
        prompt: req.prompt,
        images: req.images.unwrap_or_default(),
        model_override: req.model,
    };
    Ok(stream_session_input(state, id, session, guard, input))
}

/// SSE runner for one prompt inside a live session. The [`sessions::BusyGuard`]
/// is held for the whole stream (released when it ends), and the completed
/// turn is persisted so the session survives restarts.
fn stream_session_input(
    state: Arc<Server>,
    id: String,
    session: Arc<sessions::LiveSession>,
    guard: sessions::BusyGuard,
    input: RunInput,
) -> RunSse {
    let (tx, rx) = futures::channel::mpsc::unbounded::<Result<Event, Infallible>>();
    let sessions = state.sessions.clone();

    tokio::spawn(async move {
        let mut done_text = String::new();
        let mut failed: Option<String> = None;
        let mut paused = false;
        let mut client_gone = false;
        match session.runner.run_stream(input.clone()).await {
            Ok(mut stream) => {
                use tokio_stream::StreamExt;
                while let Some(event) = stream.next().await {
                    let sse_event = match map_run_event(
                        event,
                        &mut done_text,
                        &mut failed,
                        &mut paused,
                        Some(&id),
                    ) {
                        Some(e) => e,
                        None => continue,
                    };
                    if !client_gone && tx.unbounded_send(sse_event).is_err() {
                        client_gone = true;
                    }
                }
            }
            Err(e) => {
                let text = e.to_string();
                failed = Some(text.clone());
                let _ = tx.unbounded_send(Ok(Event::default().event("error").data(text)));
            }
        }
        // 回合完成时落盘（口径与 TUI controller 一致：仅成功回合记录）。
        if failed.is_none() && !paused {
            if let Some(manager) = sessions {
                manager.record_turn(
                    &session,
                    &id,
                    &input.prompt,
                    &done_text,
                    input.model_override.clone(),
                );
            }
        }
        drop(guard); // release the session busy flag
    });

    Sse::new(rx)
}

// ── Request / Response types ───────────────────────────────────

/// Incoming chat prompt body accepted by the `/v1/chat` and
/// `/v1/sessions/{id}/chat` endpoints.
#[derive(Debug, Deserialize, Serialize)]
pub struct ChatRequest {
    /// The user's prompt text.
    pub prompt: String,
    /// Optional image attachments (e.g. data URLs or file paths).
    #[serde(default)]
    pub images: Option<Vec<String>>,
    /// Optional model override.
    #[serde(default)]
    pub model: Option<String>,
}

/// Streaming chat response event emitted over SSE.
#[derive(Debug, Serialize)]
pub struct ChatResponse {
    /// The assistant's reply text for this event.
    pub text: String,
    /// Token usage counters, when reported by the provider.
    pub usage: Option<deepseeknova_core::chunk::Usage>,
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_request_deserializes_minimal() {
        let json = r#"{"prompt": "hello"}"#;
        let req: ChatRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.prompt, "hello");
        assert!(req.images.is_none());
        assert!(req.model.is_none());
    }

    #[test]
    fn chat_request_deserializes_full() {
        let json = r#"{"prompt":"hi","images":["data:img"],"model":"gpt-4"}"#;
        let req: ChatRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.prompt, "hi");
        assert_eq!(req.images.unwrap(), vec!["data:img"]);
        assert_eq!(req.model.unwrap(), "gpt-4");
    }

    #[test]
    fn durable_runs_roundtrip_list_and_interrupt() {
        let dir = tempfile::tempdir().unwrap();
        let store = DurableRuns::open(dir.path().to_path_buf()).unwrap();
        let record = RunRecord {
            id: "run-1".to_string(),
            prompt: "hello".to_string(),
            model: None,
            created_at_ms: 42,
            status: RunStatus::Running,
            summary: None,
            error: None,
        };
        store.save(&record).unwrap();
        assert_eq!(
            store.load("run-1").unwrap().unwrap().status,
            RunStatus::Running
        );
        assert_eq!(store.list().unwrap().len(), 1);

        assert_eq!(store.mark_interrupted().unwrap(), 1);
        let loaded = store.load("run-1").unwrap().unwrap();
        assert_eq!(loaded.status, RunStatus::Interrupted);
        assert!(loaded.error.is_some());

        // 再次 mark 不再重复计数。
        assert_eq!(store.mark_interrupted().unwrap(), 0);
    }

    #[test]
    fn durable_runs_missing_id_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = DurableRuns::open(dir.path().to_path_buf()).unwrap();
        assert!(store.load("nope").unwrap().is_none());
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn durable_runs_claim_is_atomic_and_excludes_running() {
        let dir = tempfile::tempdir().unwrap();
        let store = DurableRuns::open(dir.path().to_path_buf()).unwrap();
        store
            .save(&RunRecord {
                id: "run-1".to_string(),
                prompt: "hello".to_string(),
                model: None,
                created_at_ms: 1,
                status: RunStatus::Interrupted,
                summary: None,
                error: None,
            })
            .unwrap();

        match store.claim("run-1").unwrap() {
            ClaimResult::Claimed(record) => assert_eq!(record.status, RunStatus::Running),
            other => panic!("expected Claimed, got {other:?}"),
        }
        assert!(matches!(
            store.claim("run-1").unwrap(),
            ClaimResult::AlreadyRunning
        ));
        assert!(matches!(
            store.claim("missing").unwrap(),
            ClaimResult::NotFound
        ));
    }
}

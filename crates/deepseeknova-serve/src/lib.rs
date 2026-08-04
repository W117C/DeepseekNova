//! HTTP server for deepseeknova — exposes Runner via a REST + SSE API.
//!
//! ```no_run
//! use deepseeknova_serve::Server;
//! # use std::sync::Arc;
//! # struct DummyRunner;
//! # #[async_trait::async_trait]
//! # impl deepseeknova_core::runner::Runner for DummyRunner {
//! #     async fn run_stream(&self, _input: deepseeknova_core::runner::RunInput) -> anyhow::Result<deepseeknova_core::runner::RunEventStream> {
//! #         unreachable!()
//! #     }
//! # }
//! # #[tokio::main]
//! # async fn main() -> anyhow::Result<()> {
//! # let runner = Arc::new(DummyRunner);
//! let server = Server::new(runner);
//! server.serve("127.0.0.1:3000").await?;
//! # Ok(())
//! # }
//! ```

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use deepseeknova_core::runner::{ApprovalResponder, RunEvent, RunInput, Runner};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex};
use tower_http::cors::{Any, CorsLayer};

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
    pub runner: Arc<dyn Runner>,
    pending: PendingApprovals,
    /// Metrics/diagnose 输出目录（`<dir>/<id>.scorecard.json` 与
    /// `<dir>/diagnose/<id>.json` 的读取根目录）。`None` 时相关端点返回 404。
    metrics_dir: Option<PathBuf>,
}

impl Server {
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self {
            runner,
            pending: new_pending_approvals(),
            metrics_dir: None,
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
        }
    }

    /// Point the server at the metrics/diagnose output directory (the same
    /// `[metrics] dir` the runtime writes scorecards and diagnose reports
    /// into). Enables `GET /v1/sessions/{id}/diagnose`,
    /// `GET /v1/sessions/{id}/scorecard` and `GET /v1/metrics/scorecards`.
    /// Defaults to `None` (endpoints return 404).
    ///
    /// 注意：三个 metrics 端点均无认证，仅供本地/可信网络使用；装配到公网
    /// 地址时需自行加认证层（本实现不做认证，属刻意范围裁剪）。
    pub fn with_metrics_dir(mut self, dir: PathBuf) -> Self {
        self.metrics_dir = Some(dir);
        self
    }

    /// Configured metrics/diagnose output directory, if any.
    pub fn metrics_dir(&self) -> Option<&PathBuf> {
        self.metrics_dir.as_ref()
    }

    /// Start the server and block until it shuts down.
    pub async fn serve(self, addr: &str) -> anyhow::Result<()> {
        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any);

        let app = Router::new()
            .route("/health", get(health))
            .route("/v1/chat", post(chat))
            .route("/v1/approval", post(approval))
            .route("/v1/sessions/{id}/diagnose", get(session_diagnose))
            .route("/v1/sessions/{id}/scorecard", get(session_scorecard))
            .route("/v1/metrics/scorecards", get(metrics_scorecards))
            .layer(cors)
            .with_state(Arc::new(self));

        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!("deepseeknova serve listening on {addr}");
        axum::serve(listener, app).await?;
        Ok(())
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

async fn chat(
    State(state): State<Arc<Server>>,
    Json(req): Json<ChatRequest>,
) -> Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>> {
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

    let (tx, rx) = futures::channel::mpsc::unbounded::<Result<Event, Infallible>>();

    tokio::spawn(async move {
        match state.runner.run_stream(input).await {
            Ok(mut stream) => {
                use tokio_stream::StreamExt;
                while let Some(event) = stream.next().await {
                    let sse_event = match event {
                        Ok(RunEvent::TextDelta(text)) => {
                            Ok(Event::default().event("text").data(text))
                        }
                        Ok(RunEvent::ReasoningDelta { text, .. }) => {
                            Ok(Event::default().event("reasoning").data(text))
                        }
                        Ok(RunEvent::ToolCallStart { id, name }) => Ok(Event::default()
                            .event("tool_start")
                            .data(serde_json::json!({ "id": id, "name": name }).to_string())),
                        Ok(RunEvent::ToolCallEnd {
                            id,
                            name,
                            arguments,
                        }) => Ok(Event::default().event("tool_end").data(
                            serde_json::json!({ "id": id, "name": name, "arguments": arguments })
                                .to_string(),
                        )),
                        Ok(RunEvent::ToolResult { call_id, result }) => {
                            Ok(Event::default().event("tool_result").data(
                                serde_json::json!({ "call_id": call_id, "result": result })
                                    .to_string(),
                            ))
                        }
                        Ok(RunEvent::Usage(u)) => Ok(Event::default()
                            .event("usage")
                            .data(serde_json::to_string(&u).unwrap_or_default())),
                        Ok(RunEvent::Done(output)) => {
                            let json = serde_json::json!({
                                "text": output.text,
                                "tool_calls": output.tool_calls.iter().map(|tc| serde_json::json!({
                                    "id": tc.id,
                                    "name": tc.function.name,
                                    "arguments": tc.function.arguments,
                                })).collect::<Vec<_>>(),
                                "usage": output.usage,
                            });
                            Ok(Event::default().event("done").data(json.to_string()))
                        }
                        Ok(RunEvent::TurnComplete) => {
                            continue;
                        }
                        Ok(RunEvent::ToolCallDelta { .. }) => {
                            continue; // accumulated into ToolCallEnd
                        }
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
                            Ok(Event::default()
                                .event("approval_request")
                                .data(json.to_string()))
                        }
                        Ok(RunEvent::Paused { reason, session_id }) => {
                            let json = serde_json::json!({
                                "reason": reason,
                                "session_id": session_id,
                            });
                            Ok(Event::default().event("paused").data(json.to_string()))
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
                            Ok(Event::default()
                                .event("verification")
                                .data(json.to_string()))
                        }
                        Ok(RunEvent::QualityFinding(finding)) => Ok(Event::default()
                            .event("quality_finding")
                            .data(serde_json::to_string(&finding).unwrap_or_default())),
                        // 协议增强：阶段迁移 / 门控违规 / drift 事件最小序列化透传
                        // （前端可按 kind 渲染；WireEvent 字段名为
                        // transition/violation/drift，见 core::runner）。
                        Ok(RunEvent::PhaseTransition { transition }) => Ok(Event::default()
                            .event("phase_transition")
                            .data(serde_json::to_string(&transition).unwrap_or_default())),
                        Ok(RunEvent::GateViolation(violation)) => Ok(Event::default()
                            .event("gate_violation")
                            .data(serde_json::to_string(&violation).unwrap_or_default())),
                        Ok(RunEvent::DriftFinding(drift)) => Ok(Event::default()
                            .event("drift_finding")
                            .data(serde_json::to_string(&drift).unwrap_or_default())),
                        Err(e) => Ok(Event::default().event("error").data(e.to_string())),
                    };
                    if tx.unbounded_send(sse_event).is_err() {
                        break;
                    }
                }
            }
            Err(e) => {
                let _ = tx.unbounded_send(Ok(Event::default().event("error").data(e.to_string())));
            }
        }
        // Channel closed when tx is dropped — SSE stream ends.
    });

    Sse::new(rx)
}

/// 会话 id 合法性校验：仅允许 `[A-Za-z0-9_-]`，防 URL path 拼接路径穿越。
fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
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
/// 注意：无认证，仅供本地/可信网络使用；装配到公网地址需自行加认证层。
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
/// 注意：无认证，仅供本地/可信网络使用；装配到公网地址需自行加认证层。
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
/// 注意：无认证，仅供本地/可信网络使用；装配到公网地址需自行加认证层。
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

// ── Request / Response types ───────────────────────────────────

#[derive(Debug, Deserialize, Serialize)]
pub struct ChatRequest {
    pub prompt: String,
    #[serde(default)]
    pub images: Option<Vec<String>>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChatResponse {
    pub text: String,
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
}

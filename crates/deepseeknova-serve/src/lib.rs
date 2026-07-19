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
use axum::response::sse::{Event, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use deepseeknova_core::runner::{RunEvent, RunInput, Runner};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

// ── Public API ──────────────────────────────────────────────────

/// An HTTP server that wraps a [`Runner`] and exposes it via REST + SSE.
pub struct Server {
    pub runner: Arc<dyn Runner>,
}

impl Server {
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self { runner }
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

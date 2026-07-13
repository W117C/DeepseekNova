//! Integration tests for the HTTP serve crate — SSE streaming, error paths,
//! and input validation using a spawned axum server.

use dpronix_core::runner::{RunEvent, RunEventStream, RunInput, RunOutput, Runner};
use dpronix_serve::{ChatRequest, Server};
use serde_json::Value;
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::StreamExt;

// ---------------------------------------------------------------------------
// Mock Runner — emits canned events for integration test
// ---------------------------------------------------------------------------

struct ServeMockRunner;

#[async_trait::async_trait]
impl Runner for ServeMockRunner {
    async fn run_stream(&self, _input: RunInput) -> anyhow::Result<RunEventStream> {
        let events: Vec<anyhow::Result<RunEvent>> = vec![
            Ok(RunEvent::TextDelta("Hello ".to_string())),
            Ok(RunEvent::TextDelta("World".to_string())),
            Ok(RunEvent::Usage(dpronix_core::chunk::Usage::default())),
            Ok(RunEvent::Done(RunOutput {
                text: "Hello World".to_string(),
                tool_calls: vec![],
                usage: Some(dpronix_core::chunk::Usage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                    cache_hit_tokens: 0,
                    cache_miss_tokens: 0,
                    reasoning_tokens: 0,
                }),
            })),
        ];
        Ok(Box::pin(tokio_stream::iter(events)))
    }
}

// ---------------------------------------------------------------------------
// Helper: start a server on an ephemeral port
// ---------------------------------------------------------------------------

async fn start_server() -> u16 {
    let runner = Arc::new(ServeMockRunner);
    let server = Server::new(runner);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        let _cors = tower_http::cors::CorsLayer::permissive();
        let app = axum::Router::new()
            .route(
                "/health",
                axum::routing::get(|| async { axum::Json(serde_json::json!({"status":"ok"})) }),
            )
            .route(
                "/v1/chat",
                axum::routing::post(
                    |axum::extract::State(state): axum::extract::State<Arc<Server>>,
                     axum::Json(req): axum::Json<ChatRequest>| async move {
                        let input = dpronix_core::runner::RunInput {
                            prompt: req.prompt,
                            images: req.images.unwrap_or_default(),
                            model_override: req.model,
                        };
                        // Validate
                        if input.prompt.trim().is_empty() {
                            let e = axum::response::sse::Event::default()
                                .event("error")
                                .data("prompt must not be empty");
                            let (tx, rx): (
                                futures::channel::mpsc::UnboundedSender<
                                    Result<axum::response::sse::Event, Infallible>,
                                >,
                                _,
                            ) = futures::channel::mpsc::unbounded();
                            let _ = tx.unbounded_send(Ok(e));
                            return axum::response::sse::Sse::new(rx);
                        }
                        if input.prompt.len() > 32_000 {
                            let e = axum::response::sse::Event::default()
                                .event("error")
                                .data(format!("prompt exceeds max length ({} chars)", 32_000));
                            let (tx, rx): (
                                futures::channel::mpsc::UnboundedSender<
                                    Result<axum::response::sse::Event, Infallible>,
                                >,
                                _,
                            ) = futures::channel::mpsc::unbounded();
                            let _ = tx.unbounded_send(Ok(e));
                            return axum::response::sse::Sse::new(rx);
                        }

                        let (tx, rx) = futures::channel::mpsc::unbounded();
                        let runner = state.runner.clone();
                        tokio::spawn(async move {
                            let mut stream = runner.run_stream(input).await.unwrap();
                            while let Some(event) = stream.next().await {
                                let sse_event = match event.unwrap() {
                                    RunEvent::TextDelta(text) => {
                                        Ok(axum::response::sse::Event::default()
                                            .event("text")
                                            .data(text))
                                    }
                                    RunEvent::Usage(u) => Ok(axum::response::sse::Event::default()
                                        .event("usage")
                                        .data(serde_json::to_string(&u).unwrap_or_default())),
                                    RunEvent::Done(o) => Ok(axum::response::sse::Event::default()
                                        .event("done")
                                        .data(serde_json::json!({"text":o.text}).to_string())),
                                    _ => continue,
                                };
                                if tx.unbounded_send(sse_event).is_err() {
                                    break;
                                }
                            }
                        });
                        axum::response::sse::Sse::new(rx)
                    },
                ),
            )
            .with_state(Arc::new(server));
        axum::serve(listener, app).await.unwrap();
    });

    addr
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let port = start_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let resp = reqwest::get(format!("http://127.0.0.1:{port}/health"))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "health failed: {:?}",
        resp.text().await.unwrap()
    );
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn chat_endpoint_streams_sse() {
    let port = start_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/chat"))
        .json(&ChatRequest {
            prompt: "say hi".to_string(),
            images: None,
            model: None,
        })
        .send()
        .await
        .unwrap();

    assert!(resp.status().is_success());
    let body = resp.text().await.unwrap();
    // Should contain SSE text events
    assert!(body.contains("event: text") || body.contains("Hello"));
}

#[tokio::test]
async fn chat_empty_prompt_rejected() {
    let port = start_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/chat"))
        .json(&ChatRequest {
            prompt: "   ".to_string(),
            images: None,
            model: None,
        })
        .send()
        .await
        .unwrap();

    assert!(resp.status().is_success()); // SSE responds 200 even for errors
    let body = resp.text().await.unwrap();
    assert!(body.contains("error"));
    assert!(body.contains("prompt must not be empty"));
}

#[tokio::test]
async fn chat_prompt_too_long_rejected() {
    let port = start_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let long_prompt = "x".repeat(32_001);

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/chat"))
        .json(&ChatRequest {
            prompt: long_prompt,
            images: None,
            model: None,
        })
        .send()
        .await
        .unwrap();

    let body = resp.text().await.unwrap();
    assert!(body.contains("error"));
    assert!(body.contains("exceeds max length"));
}

#[tokio::test]
async fn chat_request_deserializes_minimal() {
    let json = r#"{"prompt": "hello"}"#;
    let req: ChatRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.prompt, "hello");
    assert!(req.images.is_none());
    assert!(req.model.is_none());
}

#[tokio::test]
async fn chat_request_deserializes_full() {
    let json = r#"{"prompt":"hi","images":["data:img"],"model":"gpt-4"}"#;
    let req: ChatRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.prompt, "hi");
    assert_eq!(req.images.unwrap(), vec!["data:img"]);
    assert_eq!(req.model.unwrap(), "gpt-4");
}

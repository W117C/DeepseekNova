//! Integration tests for the HTTP serve crate — SSE streaming, error paths,
//! and input validation using a spawned axum server.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]

use deepseeknova_core::runner::{RunEvent, RunEventStream, RunInput, RunOutput, Runner};
use deepseeknova_serve::{ChatRequest, Server};
use serde_json::Value;
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::StreamExt;

mod helpers;

// ---------------------------------------------------------------------------
// Mock Runner — emits canned events for integration test
// ---------------------------------------------------------------------------

struct ServeMockRunner;

#[async_trait::async_trait]
impl Runner for ServeMockRunner {
    async fn run_stream(
        &self,
        _input: RunInput,
    ) -> Result<RunEventStream, deepseeknova_core::DeepseeknovaError> {
        let events: Vec<Result<RunEvent, deepseeknova_core::DeepseeknovaError>> = vec![
            Ok(RunEvent::TextDelta("Hello ".to_string())),
            Ok(RunEvent::TextDelta("World".to_string())),
            Ok(RunEvent::Usage(deepseeknova_core::chunk::Usage::default())),
            Ok(RunEvent::Done(RunOutput {
                text: "Hello World".to_string(),
                tool_calls: vec![],
                usage: Some(deepseeknova_core::chunk::Usage {
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
                        let input = deepseeknova_core::runner::RunInput {
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
// Helper: start a server with the metrics/diagnose endpoints mirrored
// (same pattern as start_server: routes mirror Server::serve, state is
// Arc<Server> with an optional metrics dir).
// ---------------------------------------------------------------------------

/// 与 `Server::serve` 同款路由镜像（既有测试模式）：仅挂 metrics 三端点。
async fn start_metrics_server(dir: Option<std::path::PathBuf>) -> u16 {
    let runner = Arc::new(ServeMockRunner);
    let server = match dir {
        Some(d) => Server::new(runner).with_metrics_dir(d),
        None => Server::new(runner),
    };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        let _cors = tower_http::cors::CorsLayer::permissive();
        // 镜像 lib.rs 的会话 id 校验与文件读取语义。
        fn valid_session_id(id: &str) -> bool {
            !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        }
        let app = axum::Router::new()
            .route(
                "/v1/sessions/{id}/diagnose",
                axum::routing::get(
                    |axum::extract::State(state): axum::extract::State<Arc<Server>>,
                     axum::extract::Path(id): axum::extract::Path<String>| async move {
                        if !valid_session_id(&id) {
                            return Err((
                                axum::http::StatusCode::NOT_FOUND,
                                "not found".to_string(),
                            ));
                        }
                        let Some(dir) = state.metrics_dir() else {
                            return Err((
                                axum::http::StatusCode::NOT_FOUND,
                                "not found".to_string(),
                            ));
                        };
                        let path = dir.join("diagnose").join(format!("{id}.json"));
                        match std::fs::read_to_string(path) {
                            Ok(text) => match serde_json::from_str::<Value>(&text) {
                                Ok(v) => Ok(axum::Json(v)),
                                Err(_) => Err((
                                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                                    "stored file is not valid JSON".to_string(),
                                )),
                            },
                            Err(_) => {
                                Err((axum::http::StatusCode::NOT_FOUND, "not found".to_string()))
                            }
                        }
                    },
                ),
            )
            .route(
                "/v1/sessions/{id}/scorecard",
                axum::routing::get(
                    |axum::extract::State(state): axum::extract::State<Arc<Server>>,
                     axum::extract::Path(id): axum::extract::Path<String>| async move {
                        if !valid_session_id(&id) {
                            return Err((
                                axum::http::StatusCode::NOT_FOUND,
                                "not found".to_string(),
                            ));
                        }
                        let Some(dir) = state.metrics_dir() else {
                            return Err((
                                axum::http::StatusCode::NOT_FOUND,
                                "not found".to_string(),
                            ));
                        };
                        let path = dir.join(format!("{id}.scorecard.json"));
                        match std::fs::read_to_string(path) {
                            Ok(text) => match serde_json::from_str::<Value>(&text) {
                                Ok(v) => Ok(axum::Json(v)),
                                Err(_) => Err((
                                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                                    "stored file is not valid JSON".to_string(),
                                )),
                            },
                            Err(_) => {
                                Err((axum::http::StatusCode::NOT_FOUND, "not found".to_string()))
                            }
                        }
                    },
                ),
            )
            .route(
                "/v1/metrics/scorecards",
                axum::routing::get(
                    |axum::extract::State(state): axum::extract::State<Arc<Server>>| async move {
                        let Some(dir) = state.metrics_dir() else {
                            return Err((
                                axum::http::StatusCode::NOT_FOUND,
                                "metrics dir not configured".to_string(),
                            ));
                        };
                        let cards = deepseeknova_metrics::list_scorecards(dir);
                        let aggregate = deepseeknova_metrics::aggregate_scorecards(&cards);
                        Ok(axum::Json(serde_json::json!({
                            "count": aggregate.count,
                            "aggregate": aggregate,
                        })))
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

    let client = helpers::http::localhost_client();
    let resp = client
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
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

    let client = helpers::http::localhost_client();
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

    let client = helpers::http::localhost_client();
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

    let client = helpers::http::localhost_client();
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

// ---------------------------------------------------------------------------
// Metrics / diagnose endpoints
// ---------------------------------------------------------------------------

/// 建唯一临时目录并写入一份 diagnose 报告 + 两张评分卡。
fn write_metrics_fixture() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dsn-serve-metrics-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("diagnose")).unwrap();
    std::fs::write(
        dir.join("diagnose/sess-a.json"),
        r#"{"session_id":"sess-a","outcome":"paused"}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("sess-a.scorecard.json"),
        r#"{"session_id":"sess-a","started_at_ms":1,"dimensions":{"governance":1.0,"verification":1.0,"reflection":1.0,"review":1.0}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("sess-b.scorecard.json"),
        r#"{"session_id":"sess-b","started_at_ms":2,"dimensions":{"governance":0.5,"verification":1.0,"reflection":1.0,"review":1.0}}"#,
    )
    .unwrap();
    dir
}

#[tokio::test]
async fn diagnose_endpoint_returns_report_and_404s() {
    let dir = write_metrics_fixture();
    let port = start_metrics_server(Some(dir.clone())).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = helpers::http::localhost_client();

    // 存在 → 200 JSON（diagnose 文件按 serde_json::Value 通用解析）。
    let resp = client
        .get(format!(
            "http://127.0.0.1:{port}/v1/sessions/sess-a/diagnose"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["session_id"], "sess-a");
    assert_eq!(body["outcome"], "paused");

    // 文件缺失 → 404。
    let resp = client
        .get(format!(
            "http://127.0.0.1:{port}/v1/sessions/missing/diagnose"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    // 非法 id（路径穿越尝试：`..` 含 `.`，不在 `[A-Za-z0-9_-]` 白名单）→ 404。
    for bad in ["..", "a/b", "a%2Fb"] {
        let resp = client
            .get(format!(
                "http://127.0.0.1:{port}/v1/sessions/{bad}/diagnose"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::NOT_FOUND,
            "id '{bad}' must be rejected"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn diagnose_endpoint_404_without_metrics_dir() {
    let port = start_metrics_server(None).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let client = helpers::http::localhost_client();
    let resp = client
        .get(format!(
            "http://127.0.0.1:{port}/v1/sessions/sess-a/diagnose"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn scorecard_endpoint_returns_card_and_404s() {
    let dir = write_metrics_fixture();
    let port = start_metrics_server(Some(dir.clone())).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = helpers::http::localhost_client();

    // 存在 → 200 JSON。
    let resp = client
        .get(format!(
            "http://127.0.0.1:{port}/v1/sessions/sess-a/scorecard"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["session_id"], "sess-a");
    assert_eq!(body["dimensions"]["governance"], 1.0);

    // 新格式（含 protocol/composite 维）→ HTTP 响应透出这两维（F10：
    // 存在性 + 数值区间断言；具体数值语义由 metrics 单测覆盖，避免与
    // 并行 metrics 改动耦合）。注意旧格式文件经本端点原样透传（raw
    // Value 直读，不经 Scorecard 结构反序列化），字段补齐发生在 metrics
    // 聚合侧（list_scorecards），见 legacy 测试与聚合测试。
    std::fs::write(
        dir.join("sess-new.scorecard.json"),
        r#"{"session_id":"sess-new","started_at_ms":3,"dimensions":{"governance":0.9,"verification":1.0,"reflection":1.0,"review":1.0,"protocol":0.8,"composite":0.95}}"#,
    )
    .unwrap();
    let resp = client
        .get(format!(
            "http://127.0.0.1:{port}/v1/sessions/sess-new/scorecard"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    for dim in ["protocol", "composite"] {
        let v = body["dimensions"][dim].as_f64().unwrap_or_else(|| {
            panic!("scorecard response must contain dimensions.{dim}");
        });
        assert!(
            (0.0..=1.0).contains(&v),
            "dimensions.{dim} must be in [0,1], got {v}"
        );
    }

    // 缺失 → 404。
    let resp = client
        .get(format!(
            "http://127.0.0.1:{port}/v1/sessions/missing/scorecard"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    // 非法 id → 404。
    let resp = client
        .get(format!("http://127.0.0.1:{port}/v1/sessions/../scorecard"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    let _ = std::fs::remove_dir_all(&dir);
}

/// F10：旧格式评分卡文件（无 protocol/composite 字段）兼容性——
/// 单卡端点原样透传（200 + 既有字段保留，raw Value 直读不经结构反序列化，
/// 不合成缺失字段）；反序列化补全（serde default）发生在 metrics 聚合侧，
/// 聚合响应中 protocol/composite 必存在且 ∈[0,1]（存在性断言，不锁具体值，
/// 避免与并行 metrics 缺省值改动冲突）。
#[tokio::test]
async fn scorecard_endpoint_serves_legacy_format_with_default_dimensions() {
    let dir = std::env::temp_dir().join(format!(
        "dsn-serve-legacycard-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // 旧格式：仅四维，无 protocol/composite。
    std::fs::write(
        dir.join("legacy.scorecard.json"),
        r#"{"session_id":"legacy","started_at_ms":1,"dimensions":{"governance":0.8,"verification":1.0,"reflection":1.0,"review":1.0}}"#,
    )
    .unwrap();

    let port = start_metrics_server(Some(dir.clone())).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = helpers::http::localhost_client();
    // 单卡端点：旧格式文件仍正常服务，既有字段原样保留。
    let resp = client
        .get(format!(
            "http://127.0.0.1:{port}/v1/sessions/legacy/scorecard"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "legacy scorecard file must still be served"
    );
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["session_id"], "legacy");
    assert_eq!(body["dimensions"]["governance"], 0.8);

    // 聚合端点：经 Scorecard 结构反序列化，serde default 补全
    // protocol/composite → 响应必含且 ∈[0,1]（旧格式兼容的字段补齐点）。
    let resp = client
        .get(format!("http://127.0.0.1:{port}/v1/metrics/scorecards"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    for dim in ["protocol", "composite"] {
        let v = body["aggregate"]["avg"][dim].as_f64().unwrap_or_else(|| {
            panic!("aggregate avg must contain {dim}");
        });
        assert!(
            (0.0..=1.0).contains(&v),
            "aggregate avg.{dim} must be in [0,1], got {v}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// F13a：metrics dir 中存在 `<id>.scorecard.json` 但内容非合法 JSON →
/// scorecard 端点返回 500（`read_metrics_json` 的坏 JSON 分支）。
#[tokio::test]
async fn scorecard_endpoint_returns_500_on_invalid_json() {
    let dir = std::env::temp_dir().join(format!(
        "dsn-serve-badcard-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bad.scorecard.json"), "not json at all").unwrap();

    let port = start_metrics_server(Some(dir.clone())).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = helpers::http::localhost_client();
    let resp = client
        .get(format!("http://127.0.0.1:{port}/v1/sessions/bad/scorecard"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        "非合法 JSON 的评分卡文件应返回 500"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// F13b：URL 编码的路径穿越 id（`%2e`=`.`、`%2f`=`/`）→ 404（serve 的
/// `valid_session_id` 白名单拒绝；锁定行为防回归，即使未来路由层不再解码）。
#[tokio::test]
async fn scorecard_endpoint_rejects_url_encoded_traversal_id() {
    let dir = write_metrics_fixture();
    let port = start_metrics_server(Some(dir.clone())).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = helpers::http::localhost_client();
    for bad in [
        "%2e%2e%2f..%2fsecret",
        "%2e%2e%2fsecret",
        "%2e%2e%5csecret",
        "..%2f..%2fsecret",
    ] {
        let resp = client
            .get(format!(
                "http://127.0.0.1:{port}/v1/sessions/{bad}/scorecard"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::NOT_FOUND,
            "encoded traversal id '{bad}' must be rejected"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn metrics_scorecards_endpoint_aggregates() {
    let dir = write_metrics_fixture();
    let port = start_metrics_server(Some(dir.clone())).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = helpers::http::localhost_client();
    let resp = client
        .get(format!("http://127.0.0.1:{port}/v1/metrics/scorecards"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["count"], 2, "两张评分卡都应被聚合");
    assert!(body["aggregate"]["avg_overall"].is_number());
    assert_eq!(body["aggregate"]["worst_dimension"], "governance");
    // F10：聚合响应同样须含 protocol/composite 均维（存在性断言即可）。
    for dim in ["protocol", "composite"] {
        let v = body["aggregate"]["avg"][dim].as_f64().unwrap_or_else(|| {
            panic!("aggregate avg must contain {dim}");
        });
        assert!(
            (0.0..=1.0).contains(&v),
            "aggregate avg.{dim} must be in [0,1], got {v}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn metrics_scorecards_endpoint_404_without_metrics_dir() {
    let port = start_metrics_server(None).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let client = helpers::http::localhost_client();
    let resp = client
        .get(format!("http://127.0.0.1:{port}/v1/metrics/scorecards"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn scorecard_endpoint_500_on_invalid_json() {
    // F13a：metrics dir 中存在 `<id>.scorecard.json` 但内容非合法 JSON →
    // 500（read_metrics_json 对解析失败返回 INTERNAL_SERVER_ERROR）。
    let dir = std::env::temp_dir().join(format!(
        "dsn-serve-badjson-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bad.scorecard.json"), "this is {not json").unwrap();

    let port = start_metrics_server(Some(dir.clone())).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let client = helpers::http::localhost_client();
    let resp = client
        .get(format!("http://127.0.0.1:{port}/v1/sessions/bad/scorecard"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::INTERNAL_SERVER_ERROR);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn scorecard_endpoint_rejects_encoded_path_traversal() {
    // F13b：URL 编码的 `..` / 斜杠（`%2e%2e%2f`）经 axum 解码后落在
    // valid_session_id 白名单之外 → 404。锁定行为防回归（路径穿越在
    // 白名单层被拒，不会到达文件系统）。
    let dir = write_metrics_fixture();
    let port = start_metrics_server(Some(dir.clone())).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = helpers::http::localhost_client();
    for bad in [
        "%2e%2e%2f..%2fsecret",
        "%2e%2e/secret",
        "..%2fsecret",
        "%2e%2e%2f%2e%2e%2fsecret",
    ] {
        let resp = client
            .get(format!(
                "http://127.0.0.1:{port}/v1/sessions/{bad}/scorecard"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::NOT_FOUND,
            "encoded traversal '{bad}' must be rejected"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Multi-turn session endpoints (real Server router)
// ---------------------------------------------------------------------------

/// Start the real server router (no metrics/runs) on an ephemeral port, with
/// the given session manager and optional auth token.
async fn start_sessions_server(
    manager: Option<Arc<deepseeknova_serve::SessionManager>>,
    token: Option<String>,
) -> u16 {
    let runner: Arc<dyn Runner> = Arc::new(ServeMockRunner);
    let mut server = Server::new(runner);
    if let Some(manager) = manager {
        server = server.with_sessions(manager);
    }
    server = server.with_auth_token(token);
    let app = server.into_router();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

/// A session manager whose factory returns the canned mock runner.
fn mock_sessions_manager(dir: &std::path::Path) -> Arc<deepseeknova_serve::SessionManager> {
    let factory: deepseeknova_serve::SessionRunnerFactory =
        Arc::new(|_history| Ok(Arc::new(ServeMockRunner) as Arc<dyn Runner>));
    Arc::new(deepseeknova_serve::SessionManager::open(dir.to_path_buf(), factory).unwrap())
}

#[tokio::test]
async fn sessions_endpoints_disabled_without_manager() {
    let port = start_sessions_server(None, None).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let client = helpers::http::localhost_client();
    let resp = client
        .get(format!("http://127.0.0.1:{port}/v1/sessions"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sessions_crud_create_list_history_delete() {
    let dir = tempfile::tempdir().unwrap();
    let port = start_sessions_server(Some(mock_sessions_manager(dir.path())), None).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let client = helpers::http::localhost_client();

    // 初始为空列表。
    let resp = client
        .get(format!("http://127.0.0.1:{port}/v1/sessions"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let list: Vec<Value> = resp.json().await.unwrap();
    assert!(list.is_empty(), "fresh store must list no sessions");

    // 创建两个会话，返回 id。
    let mut ids = Vec::new();
    for _ in 0..2 {
        let resp = client
            .post(format!("http://127.0.0.1:{port}/v1/sessions"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: Value = resp.json().await.unwrap();
        let id = body["id"].as_str().unwrap().to_string();
        assert!(!id.is_empty());
        ids.push(id);
    }
    assert_ne!(ids[0], ids[1], "two creates must yield distinct ids");

    // 列表返回两个 summary（新会话 0 回合、无标题）。
    let resp = client
        .get(format!("http://127.0.0.1:{port}/v1/sessions"))
        .send()
        .await
        .unwrap();
    let list: Vec<Value> = resp.json().await.unwrap();
    assert_eq!(list.len(), 2);
    for s in &list {
        assert_eq!(s["turns"], 0);
    }

    // 历史为空数组。
    let resp = client
        .get(format!("http://127.0.0.1:{port}/v1/sessions/{}", ids[0]))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let turns: Vec<Value> = resp.json().await.unwrap();
    assert!(turns.is_empty());

    // 删除 → 列表只剩一个；重复删除 → 404。
    let resp = client
        .delete(format!("http://127.0.0.1:{port}/v1/sessions/{}", ids[0]))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let resp = client
        .delete(format!("http://127.0.0.1:{port}/v1/sessions/{}", ids[0]))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    // 未知会话的历史 → 404。
    let resp = client
        .get(format!(
            "http://127.0.0.1:{port}/v1/sessions/does-not-exist"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sessions_chat_streams_and_persists_turn() {
    let dir = tempfile::tempdir().unwrap();
    let manager = mock_sessions_manager(dir.path());
    let port = start_sessions_server(Some(manager.clone()), None).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let client = helpers::http::localhost_client();

    // 创建会话。
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/sessions"))
        .send()
        .await
        .unwrap();
    let id: String = resp.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // 发一回合：mock runner 产出 text + done。
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/sessions/{id}/chat"))
        .json(&ChatRequest {
            prompt: "hello session".to_string(),
            images: None,
            model: None,
        })
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("event: done"),
        "SSE must carry a done event: {body}"
    );

    // 历史已落盘：1 回合，用户 prompt + 助手最终正文。
    let resp = client
        .get(format!("http://127.0.0.1:{port}/v1/sessions/{id}"))
        .send()
        .await
        .unwrap();
    let turns: Vec<Value> = resp.json().await.unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0]["input"]["prompt"], "hello session");
    assert_eq!(turns[0]["output"]["text"], "Hello World");

    // 列表 summary 反映回合数。
    let resp = client
        .get(format!("http://127.0.0.1:{port}/v1/sessions"))
        .send()
        .await
        .unwrap();
    let list: Vec<Value> = resp.json().await.unwrap();
    assert_eq!(list[0]["turns"], 1);
    assert_eq!(list[0]["title"], "hello session");
}

#[tokio::test]
async fn sessions_chat_validates_and_404s() {
    let dir = tempfile::tempdir().unwrap();
    let manager = mock_sessions_manager(dir.path());
    let port = start_sessions_server(Some(manager.clone()), None).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let client = helpers::http::localhost_client();

    // 未知会话 → 404。
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/sessions/nope/chat"))
        .json(&ChatRequest {
            prompt: "hi".to_string(),
            images: None,
            model: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    // 空 prompt → 400。
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/sessions/anything/chat"))
        .json(&ChatRequest {
            prompt: "   ".to_string(),
            images: None,
            model: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn sessions_chat_busy_returns_conflict() {
    // 阻塞型 runner：持有 channel，直到测试放行。
    struct BlockingRunner(tokio::sync::mpsc::UnboundedSender<()>);
    #[async_trait::async_trait]
    impl Runner for BlockingRunner {
        async fn run_stream(
            &self,
            _input: RunInput,
        ) -> Result<RunEventStream, deepseeknova_core::DeepseeknovaError> {
            let _ = self.0.send(());
            // 挂起直到测试结束（oneshot 永不完成）。
            let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
            let _ = rx.await;
            unreachable!()
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let (release_tx, _release_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let blocking_runner = Arc::new(BlockingRunner(release_tx));
    let factory: deepseeknova_serve::SessionRunnerFactory =
        Arc::new(move |_history| Ok(Arc::clone(&blocking_runner) as Arc<dyn Runner>));
    let manager = Arc::new(
        deepseeknova_serve::SessionManager::open(dir.path().to_path_buf(), factory).unwrap(),
    );
    let port = start_sessions_server(Some(manager.clone()), None).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let client = helpers::http::localhost_client();

    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/sessions"))
        .send()
        .await
        .unwrap();
    let id: String = resp.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // 第一发挂起（占用 busy）。
    let first = tokio::spawn({
        let client = client.clone();
        let url = format!("http://127.0.0.1:{port}/v1/sessions/{id}/chat");
        async move {
            client
                .post(&url)
                .json(&ChatRequest {
                    prompt: "first".to_string(),
                    images: None,
                    model: None,
                })
                .send()
                .await
                .unwrap()
        }
    });

    // 等第一发进入 run_stream。
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // 第二发 → 409。
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/sessions/{id}/chat"))
        .json(&ChatRequest {
            prompt: "second".to_string(),
            images: None,
            model: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);

    // 释放挂起连接，避免泄漏。
    first.abort();
}

#[tokio::test]
async fn auth_token_guards_v1_but_not_health() {
    let dir = tempfile::tempdir().unwrap();
    let port = start_sessions_server(
        Some(mock_sessions_manager(dir.path())),
        Some("sekret".to_string()),
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let client = helpers::http::localhost_client();

    // 无 token → 401。
    let resp = client
        .get(format!("http://127.0.0.1:{port}/v1/sessions"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    // 错误 token → 401。
    let resp = client
        .get(format!("http://127.0.0.1:{port}/v1/sessions"))
        .bearer_auth("wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    // 正确 token → 200。
    let resp = client
        .get(format!("http://127.0.0.1:{port}/v1/sessions"))
        .bearer_auth("sekret")
        .send()
        .await
        .unwrap();
    if resp.status() != reqwest::StatusCode::OK {
        panic!(
            "auth GET failed: {} {:?}",
            resp.status(),
            resp.text().await.unwrap()
        );
    }
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 写路径同样受保护。
    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/sessions"))
        .bearer_auth("sekret")
        .send()
        .await
        .unwrap();
    if resp.status() != reqwest::StatusCode::OK {
        panic!(
            "auth POST failed: {} {:?}",
            resp.status(),
            resp.text().await.unwrap()
        );
    }
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // /health 免认证。
    let resp = client
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
}

// ---------------------------------------------------------------------------
// CORS / done session_id 契约（B2 serve 暴露面加固）
// ---------------------------------------------------------------------------

/// 从 SSE body 提取 `event: done` 的 `data:` JSON 中 `session_id` 字段。
fn sse_done_session_id(body: &str) -> Option<String> {
    let mut in_done = false;
    for line in body.lines() {
        if let Some(ev) = line.strip_prefix("event: ") {
            in_done = ev.trim() == "done";
            continue;
        }
        if in_done {
            if let Some(data) = line.strip_prefix("data: ") {
                if let Ok(v) = serde_json::from_str::<Value>(data) {
                    if let Some(id) = v.get("session_id").and_then(|s| s.as_str()) {
                        return Some(id.to_string());
                    }
                }
            }
            if line.is_empty() {
                in_done = false;
            }
        }
    }
    None
}

/// B2-P0：CORS 收窄 —— 恶意 Origin 的请求响应不得带 `Access-Control-Allow-Origin`
/// （浏览器拒绝跨源读取，关闭经 `/v1/approval` 自答审批与 SSE/会话/评分卡数据
/// 外带窗口）；loopback 来源与无 Origin 请求不受影响。
#[tokio::test]
async fn cors_blocks_malicious_origins() {
    let dir = tempfile::tempdir().unwrap();
    let port = start_sessions_server(Some(mock_sessions_manager(dir.path())), None).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let client = helpers::http::localhost_client();

    // 恶意 / 非 loopback 来源：GET 简单请求被处理但响应无 ACAO → 浏览器拒读。
    for origin in [
        "https://evil.example",
        "http://192.168.1.10:8080",
        "http://10.0.0.5:1234",
        "null",
    ] {
        let resp = client
            .get(format!("http://127.0.0.1:{port}/v1/sessions"))
            .header(reqwest::header::ORIGIN, origin)
            .send()
            .await
            .unwrap();
        let acao = resp
            .headers()
            .get(reqwest::header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .cloned();
        assert!(
            acao.is_none(),
            "origin {origin} must NOT get ACAO, got {acao:?}"
        );
    }

    // 跨源预检（POST /v1/chat 带 JSON body 属非 simple 请求）：恶意 origin 预检
    // 也不得携带 ACAO → 实际请求不会被发出。
    let resp = client
        .request(
            reqwest::Method::OPTIONS,
            format!("http://127.0.0.1:{port}/v1/chat"),
        )
        .header(reqwest::header::ORIGIN, "https://evil.example")
        .header("Access-Control-Request-Method", "POST")
        .send()
        .await
        .unwrap();
    assert!(
        resp.headers()
            .get(reqwest::header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none(),
        "preflight from malicious origin must be rejected"
    );

    // 正向对照：loopback 来源放行（端口不限，webview/dev server 可用）。
    for origin in ["http://127.0.0.1:8787", "http://localhost:3000"] {
        let resp = client
            .get(format!("http://127.0.0.1:{port}/v1/sessions"))
            .header(reqwest::header::ORIGIN, origin)
            .send()
            .await
            .unwrap();
        let acao = resp
            .headers()
            .get(reqwest::header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        assert_eq!(
            acao.as_deref(),
            Some(origin),
            "loopback {origin} must be allowed"
        );
    }

    // 无 Origin 头（curl / 同源 / 非浏览器客户端）→ 正常处理。
    let resp = client
        .get(format!("http://127.0.0.1:{port}/v1/sessions"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
}

/// B2：`/v1/sessions/{id}/chat` 的 `done` 事件携带该会话 id（前端据 session_id
/// 拉取该 run 的评分卡/诊断的关联键）。
#[tokio::test]
async fn sessions_chat_done_event_carries_session_id() {
    let dir = tempfile::tempdir().unwrap();
    let manager = mock_sessions_manager(dir.path());
    let port = start_sessions_server(Some(manager.clone()), None).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let client = helpers::http::localhost_client();

    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/sessions"))
        .send()
        .await
        .unwrap();
    let id: String = resp.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/sessions/{id}/chat"))
        .json(&ChatRequest {
            prompt: "hello".to_string(),
            images: None,
            model: None,
        })
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert!(
        body.contains(&format!("\"session_id\":\"{id}\"")),
        "done event must carry session_id {id}: {body}"
    );
    assert_eq!(sse_done_session_id(&body).as_deref(), Some(id.as_str()));
}

/// B2：`/v1/chat` 配置 durable runs 时，`done` 事件携带与持久化 run 一致的 id
/// （run/聊天/会话/metrics 三套 id 可关联）。
#[tokio::test]
async fn chat_done_event_carries_durable_run_id() {
    let dir = tempfile::tempdir().unwrap();
    let runner: Arc<dyn Runner> = Arc::new(ServeMockRunner);
    let server = Server::new(runner).with_runs_dir(dir.path().to_path_buf());
    let app = server.into_router();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let client = helpers::http::localhost_client();

    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/chat"))
        .json(&ChatRequest {
            prompt: "hi".to_string(),
            images: None,
            model: None,
        })
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    let run_id = sse_done_session_id(&body).expect("done must carry the run id");
    assert!(!run_id.is_empty());

    // 该 id 同时落为 durable run record（run/SSE 关联键一致）。
    let resp = client
        .get(format!("http://127.0.0.1:{port}/v1/runs"))
        .send()
        .await
        .unwrap();
    let runs: Vec<Value> = resp.json().await.unwrap();
    assert!(
        runs.iter()
            .any(|r| r["id"].as_str() == Some(run_id.as_str())),
        "durable run {run_id} must be listed: {runs:?}"
    );
}

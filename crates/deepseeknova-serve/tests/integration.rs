//! Integration tests for the HTTP serve crate — SSE streaming, error paths,
//! and input validation using a spawned axum server.

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
    async fn run_stream(&self, _input: RunInput) -> anyhow::Result<RunEventStream> {
        let events: Vec<anyhow::Result<RunEvent>> = vec![
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

//! # RemoteEmbedder — OpenAI 兼容嵌入后端
//!
//! 把文本映射为向量，供 [`MemoryEngine`](deepseeknova_core::memory::engine::MemoryEngine)
//! 的混合检索（bm25 + 余弦）使用。协议对齐 OpenAI `/v1/embeddings`：
//! `POST {base_url}/embeddings`，`Authorization: Bearer <api_key>`，
//! body `{"model": ..., "input": text}`，取 `data[0].embedding`。
//!
//! 双路径：
//! - **同步** [`EmbeddingProvider::embed`]（兼容既有调用方）：经进程级共享
//!   多线程 runtime `block_on`，不持独立 runtime（async 上下文 drop 安全）、
//!   不嵌套调用方 runtime。
//! - **异步** [`EmbeddingProvider::embed_async`]（async 调用链推荐）：直接 await
//!   reqwest，无 runtime、不占用 tokio worker 线程。
//!
//! 任何失败都以 [`deepseeknova_core::DeepseeknovaError`] 返回，由调用方 fail-open（回落纯 FTS）。
//! 超时由构造时 `timeout_secs` 配置的 HTTP client 兜底。

use crate::ProviderError;
use deepseeknova_config::MemoryConfig;
use deepseeknova_core::memory::embedding::{EmbedAsyncFuture, EmbeddingProvider};
use deepseeknova_core::DeepseeknovaError;
use reqwest::Client;
use serde::Deserialize;
use std::env;
use std::sync::Arc;
use std::time::Duration;

/// 嵌入 API key 首选环境变量，缺失时回落通用 OpenAI key。
pub const EMBED_API_KEY_ENV: &str = "DEEPSEEKNOVA_EMBED_API_KEY";
pub const FALLBACK_API_KEY_ENV: &str = "OPENAI_API_KEY";

/// 同步 [`EmbeddingProvider::embed`] 的共享执行 runtime：进程级懒加载多线程
/// runtime。不持有在 [`RemoteEmbedder`] 内，避免在 async 上下文中 drop 一个
/// tokio runtime 触发 `Cannot drop a runtime in a context where blocking is not
/// allowed`（async 调用方持有/释放 embedder 均安全）。
fn shared_sync_runtime() -> Result<&'static tokio::runtime::Runtime, DeepseeknovaError> {
    static RT: std::sync::LazyLock<Result<tokio::runtime::Runtime, std::io::Error>> =
        std::sync::LazyLock::new(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
        });
    RT.as_ref().map_err(|e| {
        DeepseeknovaError::runner(format!(
            "failed to build shared embedding sync runtime: {e}"
        ))
    })
}

/// OpenAI 兼容远程嵌入提供器（`[memory] embedder = "remote"`）。
#[derive(Debug)]
pub struct RemoteEmbedder {
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
    /// 客户端超时时长（用于超时错误归类到 ProviderError::Timeout）。
    timeout_secs: u64,
}

impl RemoteEmbedder {
    /// 直接装配（测试/自定义调用方用）。
    pub fn new(
        base_url: &str,
        api_key: &str,
        model: &str,
        timeout_secs: u64,
    ) -> Result<Self, DeepseeknovaError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .map_err(|e| {
                DeepseeknovaError::provider(format!("failed to build embedding HTTP client: {e}"))
            })?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            timeout_secs,
        })
    }

    /// 从 `[memory]` 配置装配：要求 `embedder == "remote"`、`embed_model` 非空、
    /// 环境变量有 key（DEEPSEEKNOVA_EMBED_API_KEY，回落 OPENAI_API_KEY）。
    pub fn from_memory_config(config: &MemoryConfig) -> Result<Self, DeepseeknovaError> {
        if config.embedder != "remote" {
            return Err(DeepseeknovaError::config(format!(
                "[memory] embedder must be \"remote\" (current: {:?})",
                config.embedder
            )));
        }
        if config.embed_model.trim().is_empty() {
            return Err(DeepseeknovaError::config(
                "[memory] embed_model is required when embedder=remote".to_string(),
            ));
        }
        let api_key = env::var(EMBED_API_KEY_ENV)
            .or_else(|_| env::var(FALLBACK_API_KEY_ENV))
            .map_err(|_| {
                DeepseeknovaError::config(format!(
                    "embed API key missing: set {EMBED_API_KEY_ENV} or {FALLBACK_API_KEY_ENV}"
                ))
            })?;
        Self::new(
            &config.embed_base_url,
            &api_key,
            &config.embed_model,
            config.embed_timeout_secs,
        )
    }

    /// 单条文本的真实 HTTP 请求：POST `/embeddings`、解析首个向量。
    /// 仅做网络往返，不含 runtime 管理——同步路径在共享 runtime 上 `block_on`，
    /// 异步路径由调用方直接 await。
    async fn request_embedding(&self, text: &str) -> Result<Vec<f32>, DeepseeknovaError> {
        let url = format!("{}/embeddings", self.base_url);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({"model": self.model, "input": text}))
            .send()
            .await
            .map_err(|e| {
                // reqwest 超时错误的 Display 不含 "timed out" 字样，需按
                // is_timeout() 归类到 ProviderError::Timeout 以保留重试语义。
                if e.is_timeout() {
                    ProviderError::Timeout(std::time::Duration::from_secs(self.timeout_secs))
                } else {
                    ProviderError::from(e)
                }
            })?;
        let status = resp.status();
        let body = resp.text().await.map_err(ProviderError::from)?;
        if !status.is_success() {
            // 走 ProviderError::Http 让 429/5xx 按状态码获得可重试分类，
            // 与其他 provider 路径口径一致。
            return Err(ProviderError::Http {
                status: status.as_u16(),
                body,
            }
            .into());
        }
        let parsed: EmbeddingResponse =
            serde_json::from_str(&body).map_err(DeepseeknovaError::Serde)?;
        let first = parsed
            .data
            .into_iter()
            .next()
            .ok_or_else(|| DeepseeknovaError::provider("embedding response has no data"))?;
        if first.embedding.is_empty() {
            return Err(DeepseeknovaError::provider(
                "embedding response has empty vector",
            ));
        }
        Ok(first.embedding)
    }
}

impl EmbeddingProvider for RemoteEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, DeepseeknovaError> {
        // 同步兼容路径：共享进程级 runtime block_on（见 [`shared_sync_runtime`]）。
        // async 调用链请使用 embed_async（直接 await reqwest，不占用 worker）。
        shared_sync_runtime()?.block_on(self.request_embedding(text))
    }

    fn embed_async(self: Arc<Self>, text: String) -> EmbedAsyncFuture {
        // 真实 async：直接 await reqwest，无独立 runtime、不占用 worker 线程。
        Box::pin(async move { self.request_embedding(&text).await })
    }
}

#[derive(Debug, Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

/// 从 `[memory]` 配置构造嵌入后端；不可用（none/未知后端/缺 key/配置错）
/// 时 warn 并返回 None——调用方保持纯 FTS 行为（fail-open）。
pub fn try_memory_embedder(config: &MemoryConfig) -> Option<Arc<dyn EmbeddingProvider>> {
    match config.embedder.as_str() {
        "none" => None,
        "remote" => match RemoteEmbedder::from_memory_config(config) {
            Ok(e) => Some(Arc::new(e)),
            Err(err) => {
                tracing::warn!("memory semantic embeddings disabled: {err:#}");
                None
            }
        },
        other => {
            tracing::warn!(
                "[memory] embedder {other:?} is not implemented; falling back to FTS-only"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{clear_proxy_env, ENV_LOCK};
    use std::io::{Read, Write};

    // env 串行化锁与代理清除函数见 crate::test_util（跨 openai/embeddings
    // 共享，避免 std::env 并发修改 UB）。

    /// 起一个一次性 std HTTP 服务：捕获请求原文，回指定状态与 body（零延迟）。
    fn serve_once(
        status: &'static str,
        response_body: &'static str,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        serve_with_delay(status, response_body, Duration::ZERO)
    }

    /// 同 [`serve_once`]，但服务端在回包前先睡眠 `delay`（测客户端超时兜底用）。
    fn serve_with_delay(
        status: &'static str,
        response_body: &'static str,
        delay: Duration,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            // 1) 读至 headers 结束（\r\n\r\n）
            loop {
                let n = stream.read(&mut tmp).unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            // 2) POST body 在 \r\n\r\n 之后；若未随 headers 同包到达，需按
            //    Content-Length 继续读取，否则捕获的请求不含 body（model /
            //    input 断言 flaky 失败）。
            let header_end = buf
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .unwrap_or(buf.len());
            let header_str = String::from_utf8_lossy(&buf[..header_end]);
            let content_length: usize = header_str
                .lines()
                .find_map(|line| {
                    line.to_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse().ok())
                })
                .unwrap_or(0);
            let body_start = header_end + 4;
            let already_have = buf.len().saturating_sub(body_start);
            let remaining = content_length.saturating_sub(already_have);
            if remaining > 0 {
                let mut body_tmp = vec![0u8; remaining];
                let mut read_total = 0;
                while read_total < remaining {
                    let n = stream.read(&mut body_tmp[read_total..]).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    read_total += n;
                }
                buf.extend_from_slice(&body_tmp[..read_total]);
            }
            tx.send(String::from_utf8_lossy(&buf).to_string()).unwrap();
            std::thread::sleep(delay);
            // 客户端可能已超时断开，写失败属预期，忽略。
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                    response_body.len()
                )
                .as_bytes(),
            );
        });
        (format!("http://{addr}/v1"), rx)
    }

    #[test]
    fn embed_posts_correct_request_and_parses_response() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_proxy_env();
        let body = r#"{"object":"list","data":[{"object":"embedding","index":0,"embedding":[0.1,0.2,0.3]}]}"#;
        let (base, rx) = serve_once("200 OK", body);
        let e = RemoteEmbedder::new(&base, "sk-test", "text-embedding-3-small", 10).unwrap();
        let v = e.embed("hello world").unwrap();
        assert_eq!(v, vec![0.1, 0.2, 0.3]);
        let req = rx.recv().unwrap();
        assert!(
            req.starts_with("POST /v1/embeddings HTTP/1.1"),
            "请求行不对: {req}"
        );
        let lower = req.to_lowercase();
        assert!(lower.contains("bearer sk-test"), "Bearer 头缺失: {req}");
        assert!(req.contains("text-embedding-3-small"), "model 缺失: {req}");
        assert!(req.contains("hello world"), "input 缺失: {req}");
    }

    #[test]
    fn embed_http_error_is_err() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_proxy_env();
        let (base, _rx) = serve_once("500 Internal Server Error", "boom");
        let e = RemoteEmbedder::new(&base, "sk-test", "m", 10).unwrap();
        let err = e.embed("hello").unwrap_err();
        assert!(
            format!("{err:#}").contains("500"),
            "错误必须带状态码: {err:#}"
        );
    }

    #[test]
    fn embed_bad_json_is_err() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_proxy_env();
        let (base, _rx) = serve_once("200 OK", "not json");
        let e = RemoteEmbedder::new(&base, "sk-test", "m", 10).unwrap();
        assert!(e.embed("hello").is_err());
    }

    // ---- 异步路径（embed_async）：真实 await reqwest，不占用 worker ----

    #[tokio::test]
    async fn embed_async_posts_request_and_parses_response() {
        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
        let body = r#"{"object":"list","data":[{"object":"embedding","index":0,"embedding":[0.1,0.2,0.3]}]}"#;
        let (base, rx) = serve_once("200 OK", body);
        let e =
            Arc::new(RemoteEmbedder::new(&base, "sk-test", "text-embedding-3-small", 10).unwrap());
        let v = e.embed_async("hello async".to_string()).await.unwrap();
        assert_eq!(v, vec![0.1, 0.2, 0.3]);
        let req = rx.recv().unwrap();
        assert!(
            req.starts_with("POST /v1/embeddings HTTP/1.1"),
            "请求行不对: {req}"
        );
        let lower = req.to_lowercase();
        assert!(lower.contains("bearer sk-test"), "Bearer 头缺失: {req}");
        assert!(req.contains("text-embedding-3-small"), "model 缺失: {req}");
        assert!(req.contains("hello async"), "input 缺失: {req}");
    }

    #[tokio::test]
    async fn embed_async_http_error_is_err() {
        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
        let (base, _rx) = serve_once("500 Internal Server Error", "boom");
        let e = Arc::new(RemoteEmbedder::new(&base, "sk-test", "m", 10).unwrap());
        let err = e.embed_async("hello".to_string()).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("500"),
            "错误必须带状态码: {err:#}"
        );
    }

    #[tokio::test]
    async fn embed_async_bad_json_is_err() {
        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
        let (base, _rx) = serve_once("200 OK", "not json");
        let e = Arc::new(RemoteEmbedder::new(&base, "sk-test", "m", 10).unwrap());
        assert!(e.embed_async("hello".to_string()).await.is_err());
    }

    /// 超时兜底：服务端比客户端超时更慢，必须在配置时限附近报错，
    /// 而非等满服务端延迟（3s）。
    #[tokio::test]
    async fn embed_async_times_out_with_client_timeout() {
        let _guard = ENV_LOCK.lock().await;
        clear_proxy_env();
        let (base, _rx) = serve_with_delay("200 OK", "{}", Duration::from_secs(3));
        let e = Arc::new(RemoteEmbedder::new(&base, "sk-test", "m", 1).unwrap());
        let start = std::time::Instant::now();
        let err = e.embed_async("hello".to_string()).await.unwrap_err();
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "超时兜底必须按客户端超时触发（配置 1s），而非等满服务端 3s 延迟"
        );
        assert!(
            format!("{err:#}").contains("timeout"),
            "必须报超时错误: {err:#}"
        );
    }

    fn config_with_embedder(embedder: &str, model: &str) -> MemoryConfig {
        let toml = format!("[memory]\nembedder = {embedder:?}\nembed_model = {model:?}\n");
        let cfg: deepseeknova_config::Config = toml::from_str(&toml).unwrap();
        cfg.memory
    }

    #[test]
    fn from_memory_config_validates_embedder_model_and_env() {
        let _g = ENV_LOCK.blocking_lock();
        clear_proxy_env();
        let prev_embed = env::var(EMBED_API_KEY_ENV).ok();
        let prev_openai = env::var(FALLBACK_API_KEY_ENV).ok();
        env::remove_var(EMBED_API_KEY_ENV);
        env::remove_var(FALLBACK_API_KEY_ENV);
        let restore = |prev_embed: Option<String>, prev_openai: Option<String>| {
            match prev_embed {
                Some(v) => env::set_var(EMBED_API_KEY_ENV, v),
                None => env::remove_var(EMBED_API_KEY_ENV),
            }
            match prev_openai {
                Some(v) => env::set_var(FALLBACK_API_KEY_ENV, v),
                None => env::remove_var(FALLBACK_API_KEY_ENV),
            }
        };

        // 未启用 remote：必须拒绝。
        let none = config_with_embedder("none", "");
        assert!(RemoteEmbedder::from_memory_config(&none).is_err());

        // remote 但无 key：必须报缺 key。
        let remote = config_with_embedder("remote", "text-embedding-3-small");
        let err = RemoteEmbedder::from_memory_config(&remote).unwrap_err();
        assert!(
            format!("{err:#}").contains("API key missing"),
            "必须报缺 key: {err:#}"
        );

        // remote + 首选 env：装配成功。
        env::set_var(EMBED_API_KEY_ENV, "sk-first");
        assert!(RemoteEmbedder::from_memory_config(&remote).is_ok());

        // remote + 仅回落 env：装配成功。
        env::remove_var(EMBED_API_KEY_ENV);
        env::set_var(FALLBACK_API_KEY_ENV, "sk-fallback");
        assert!(RemoteEmbedder::from_memory_config(&remote).is_ok());

        // remote + 空 model：必须报错（即使有 key）。
        let empty_model = config_with_embedder("remote", "");
        assert!(RemoteEmbedder::from_memory_config(&empty_model).is_err());

        restore(prev_embed, prev_openai);
    }

    #[test]
    fn try_memory_embedder_returns_none_when_disabled_or_missing_key() {
        let _g = ENV_LOCK.blocking_lock();
        clear_proxy_env();
        let prev_embed = env::var(EMBED_API_KEY_ENV).ok();
        let prev_openai = env::var(FALLBACK_API_KEY_ENV).ok();
        env::remove_var(EMBED_API_KEY_ENV);
        env::remove_var(FALLBACK_API_KEY_ENV);

        assert!(try_memory_embedder(&config_with_embedder("none", "")).is_none());
        assert!(
            try_memory_embedder(&config_with_embedder("local", "")).is_none(),
            "未实现后端必须 fail-open 到 None"
        );
        let remote = config_with_embedder("remote", "text-embedding-3-small");
        assert!(
            try_memory_embedder(&remote).is_none(),
            "缺 key 必须 fail-open"
        );
        env::set_var(EMBED_API_KEY_ENV, "sk-test");
        assert!(try_memory_embedder(&remote).is_some());

        match prev_embed {
            Some(v) => env::set_var(EMBED_API_KEY_ENV, v),
            None => env::remove_var(EMBED_API_KEY_ENV),
        }
        match prev_openai {
            Some(v) => env::set_var(FALLBACK_API_KEY_ENV, v),
            None => env::remove_var(FALLBACK_API_KEY_ENV),
        }
    }
}

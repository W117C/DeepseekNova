//! # RemoteEmbedder — OpenAI 兼容嵌入后端
//!
//! 把文本映射为向量，供 [`MemoryEngine`](deepseeknova_core::memory::engine::MemoryEngine)
//! 的混合检索（bm25 + 余弦）使用。协议对齐 OpenAI `/v1/embeddings`：
//! `POST {base_url}/embeddings`，`Authorization: Bearer <api_key>`，
//! body `{"model": ..., "input": text}`，取 `data[0].embedding`。
//!
//! 同步 trait 内做 HTTP：实例持有一个独立 tokio runtime 并在其中 `block_on`，
//! 不阻塞/不嵌套调用方 runtime。任何失败都以 [`anyhow::Error`] 返回，
//! 由调用方 fail-open（回落纯 FTS）。

use anyhow::{anyhow, Context, Result};
use deepseeknova_config::MemoryConfig;
use deepseeknova_core::memory::embedding::EmbeddingProvider;
use reqwest::Client;
use serde::Deserialize;
use std::env;
use std::sync::Arc;
use std::time::Duration;

/// 嵌入 API key 首选环境变量，缺失时回落通用 OpenAI key。
pub const EMBED_API_KEY_ENV: &str = "DEEPSEEKNOVA_EMBED_API_KEY";
pub const FALLBACK_API_KEY_ENV: &str = "OPENAI_API_KEY";

/// OpenAI 兼容远程嵌入提供器（`[memory] embedder = "remote"`）。
#[derive(Debug)]
pub struct RemoteEmbedder {
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
    rt: tokio::runtime::Runtime,
}

impl RemoteEmbedder {
    /// 直接装配（测试/自定义调用方用）。
    pub fn new(base_url: &str, api_key: &str, model: &str, timeout_secs: u64) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .context("failed to build embedding HTTP client")?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("failed to build embedding runtime")?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            rt,
        })
    }

    /// 从 `[memory]` 配置装配：要求 `embedder == "remote"`、`embed_model` 非空、
    /// 环境变量有 key（DEEPSEEKNOVA_EMBED_API_KEY，回落 OPENAI_API_KEY）。
    pub fn from_memory_config(config: &MemoryConfig) -> Result<Self> {
        if config.embedder != "remote" {
            anyhow::bail!(
                "[memory] embedder must be \"remote\" (current: {:?})",
                config.embedder
            );
        }
        if config.embed_model.trim().is_empty() {
            anyhow::bail!("[memory] embed_model is required when embedder=remote");
        }
        let api_key = env::var(EMBED_API_KEY_ENV)
            .or_else(|_| env::var(FALLBACK_API_KEY_ENV))
            .with_context(|| {
                format!("embed API key missing: set {EMBED_API_KEY_ENV} or {FALLBACK_API_KEY_ENV}")
            })?;
        Self::new(
            &config.embed_base_url,
            &api_key,
            &config.embed_model,
            config.embed_timeout_secs,
        )
    }

    async fn embed_async(&self, text: &str) -> Result<Vec<f32>> {
        let url = format!("{}/embeddings", self.base_url);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({"model": self.model, "input": text}))
            .send()
            .await
            .context("embedding request failed")?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .context("embedding response read failed")?;
        if !status.is_success() {
            anyhow::bail!("embedding HTTP {status}: {body}");
        }
        let parsed: EmbeddingResponse =
            serde_json::from_str(&body).context("embedding response parse failed")?;
        let first = parsed
            .data
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("embedding response has no data"))?;
        if first.embedding.is_empty() {
            anyhow::bail!("embedding response has empty vector");
        }
        Ok(first.embedding)
    }
}

impl EmbeddingProvider for RemoteEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        // 独立 runtime block_on：同步 trait 内做 HTTP，不阻塞调用方 runtime。
        self.rt.block_on(self.embed_async(text))
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
    use std::io::{Read, Write};

    /// env 相关测试串行化，避免并行测试互相污染环境变量。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 起一个一次性 std HTTP 服务：捕获请求原文，回指定状态与 body。
    fn serve_once(
        status: &'static str,
        response_body: &'static str,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
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
            tx.send(String::from_utf8_lossy(&buf).to_string()).unwrap();
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                response_body.len()
            );
            stream.write_all(resp.as_bytes()).unwrap();
        });
        (format!("http://{addr}/v1"), rx)
    }

    #[test]
    fn embed_posts_correct_request_and_parses_response() {
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
        let (base, _rx) = serve_once("200 OK", "not json");
        let e = RemoteEmbedder::new(&base, "sk-test", "m", 10).unwrap();
        assert!(e.embed("hello").is_err());
    }

    fn config_with_embedder(embedder: &str, model: &str) -> MemoryConfig {
        let toml = format!("[memory]\nembedder = {embedder:?}\nembed_model = {model:?}\n");
        let cfg: deepseeknova_config::Config = toml::from_str(&toml).unwrap();
        cfg.memory
    }

    #[test]
    fn from_memory_config_validates_embedder_model_and_env() {
        let _g = ENV_LOCK.lock().unwrap();
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
        let _g = ENV_LOCK.lock().unwrap();
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

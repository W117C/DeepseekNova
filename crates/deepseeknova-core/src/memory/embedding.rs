//! # 嵌入检索支撑（P3.3）
//!
//! 记忆库混合检索的嵌入后端抽象与余弦工具。当前项目默认 `embedder = "none"`，
//! 不内置任何模型；接入方（如本地/远程嵌入服务）实现 `EmbeddingProvider` 后
//! 经 `MemoryStore::search_hybrid`（见 `super::store`）使用。
//!
//! 双路径：
//! - **同步** `EmbeddingProvider::embed`：兼容既有调用方。远程 HTTP 实现内部
//!   自行阻塞等待，在 async 调用链（graph refresh / memory 写入路径）中会占用
//!   tokio worker 线程。
//! - **异步** `EmbeddingProvider::embed_async`：async 调用链使用。默认实现经
//!   `spawn_blocking` 桥接到同步 `EmbeddingProvider::embed`，await 时
//!   不阻塞 worker 线程（仅占用 blocking 线程池）；远程 HTTP 实现应覆写为真实
//!   async（直接 await 网络）。

use crate::DeepseeknovaError;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// 异步嵌入返回的 future：`Send + 'static`，可安全 `tokio::spawn` 或直接 await。
pub type EmbedAsyncFuture =
    Pin<Box<dyn Future<Output = Result<Vec<f32>, DeepseeknovaError>> + Send + 'static>>;

/// 嵌入提供器：把文本映射为归一化向量（余弦相似度）。
/// `'static` 超 trait 保证默认 [`Self::embed_async`] 的 `spawn_blocking`
/// 桥接可用（同步实现无需覆写即可获得非阻塞异步语义）。
pub trait EmbeddingProvider: Send + Sync + 'static {
    /// 同步嵌入（兼容既有调用方）。远程 HTTP 实现在此内部阻塞等待；在
    /// async 调用链中建议改走 [`Self::embed_async`] 避免阻塞 worker 线程。
    fn embed(&self, text: &str) -> Result<Vec<f32>, DeepseeknovaError>;

    /// 异步嵌入路径。默认实现经 `tokio::task::spawn_blocking` 桥接到同步
    /// [`Self::embed`]，调用方 await 时不阻塞 tokio worker 线程（仅占用
    /// blocking 线程池）。需要真正非阻塞语义的实现（如远程 HTTP）应覆写
    /// 此方法直接 await 网络。
    ///
    /// `self` 与 `text` 均取拥有权，返回 future 为 `Send + 'static`，
    /// 可安全用于 `tokio::spawn`。
    fn embed_async(self: Arc<Self>, text: String) -> EmbedAsyncFuture {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || self.embed(&text))
                .await
                .map_err(|e| {
                    DeepseeknovaError::runner(format!("embedding spawn_blocking task failed: {e}"))
                })?
        })
    }
}

/// 余弦相似度；任一侧为零向量返回 0.0（无方向可比）。
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += f64::from(*x) * f64::from(*y);
        na += f64::from(*x) * f64::from(*x);
        nb += f64::from(*y) * f64::from(*y);
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    (dot / (na.sqrt() * nb.sqrt())) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_same_vector_is_one() {
        let v = [1.0, 2.0, 3.0];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        assert!((cosine(&[1.0, 0.0], &[0.0, 1.0]) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_mismatched_or_empty_is_zero() {
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
        assert_eq!(cosine(&[], &[]), 0.0);
    }

    /// 默认桥接：同步实现经 embed_async 走 spawn_blocking，必须运行在
    /// blocking 线程池而非 tokio worker（证明 async 调用链不阻塞 worker）。
    #[tokio::test]
    async fn default_embed_async_bridges_sync_via_spawn_blocking() {
        use std::sync::Mutex;

        struct SyncOnly {
            embed_thread: Mutex<Option<std::thread::ThreadId>>,
        }
        impl EmbeddingProvider for SyncOnly {
            fn embed(&self, _text: &str) -> Result<Vec<f32>, DeepseeknovaError> {
                *self.embed_thread.lock().unwrap() = Some(std::thread::current().id());
                Ok(vec![0.25, 0.75])
            }
        }

        let inner = Arc::new(SyncOnly {
            embed_thread: Mutex::new(None),
        });
        let p: Arc<dyn EmbeddingProvider> = inner.clone();
        let worker_id = std::thread::current().id();
        let v = p.embed_async("hello".to_string()).await.unwrap();
        assert_eq!(v, vec![0.25, 0.75]);
        let blocking_id = inner.embed_thread.lock().unwrap().unwrap();
        assert_ne!(
            blocking_id, worker_id,
            "同步 embed 必须运行在 blocking 线程池，而非 tokio worker"
        );
    }
}

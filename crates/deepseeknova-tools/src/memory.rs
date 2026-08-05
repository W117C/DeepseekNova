//! 记忆工具：remember / recall / forget。
//! 持久引擎句柄经 `ToolContext.extensions` 注入（`MemoryHandle`），缺失时优雅降级。
//! 相比旧实现，写入落到跨会话持久的 SQLite 引擎，而非进程内易失 HashMap。

use async_trait::async_trait;
use deepseeknova_core::memory::engine::MemoryEngine;
use deepseeknova_core::{Tool, ToolContext, ToolSchema};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

/// 共享持久记忆引擎句柄（runtime 注入，对称于 GraphHandle）。
pub type MemoryHandle = Arc<MemoryEngine>;

/// 召回排序生命周期权重扩展（runtime 注入，RecallTool 读取；
/// 缺失时回落引擎默认 0.3，行为不变）。
#[derive(Debug, Clone, Copy)]
pub struct MemoryRankWeight(pub f64);

/// 引擎未装配时的降级提示（不打断 run）。
const NO_MEMORY_MSG: &str = "记忆引擎未启用（[memory] enabled=false 或未装配），无法读写记忆。";

fn handle(ctx: &ToolContext) -> Option<MemoryHandle> {
    ctx.extensions.get::<MemoryHandle>().cloned()
}

// ---------------------------------------------------------------------------
// RememberTool
// ---------------------------------------------------------------------------

pub struct RememberTool;

#[derive(Deserialize)]
struct RememberArgs {
    key: String,
    value: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[async_trait]
impl Tool for RememberTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "remember".to_string(),
            description: "Persists a memory (key overwrites).".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "key": {"type": "string", "description": "Key."},
                    "value": {"type": "string", "description": "Value."},
                    "tags": {"type": "array", "items": {"type": "string"}, "description": "Tags."}
                },
                "required": ["key", "value"]
            }),
        }
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let parsed: RememberArgs = serde_json::from_str(args)?;
        let h = match handle(ctx) {
            Some(h) => h,
            None => return Ok(NO_MEMORY_MSG.to_string()),
        };
        let existed = h.remember(&parsed.key, &parsed.value, parsed.tags)?;
        Ok(if existed {
            format!("updated memory '{}'", parsed.key)
        } else {
            format!("stored memory '{}'", parsed.key)
        })
    }
}

// ---------------------------------------------------------------------------
// ForgetTool
// ---------------------------------------------------------------------------

pub struct ForgetTool;

#[derive(Deserialize)]
struct ForgetArgs {
    key: String,
}

#[async_trait]
impl Tool for ForgetTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "forget".to_string(),
            description: "Deletes a memory.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {"key": {"type": "string", "description": "Key."}},
                "required": ["key"]
            }),
        }
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let parsed: ForgetArgs = serde_json::from_str(args)?;
        let h = match handle(ctx) {
            Some(h) => h,
            None => return Ok(NO_MEMORY_MSG.to_string()),
        };
        if h.forget(&parsed.key)? {
            Ok(format!("removed memory '{}'", parsed.key))
        } else {
            Ok(format!("memory '{}' not found", parsed.key))
        }
    }
}

// ---------------------------------------------------------------------------
// RecallTool
// ---------------------------------------------------------------------------

pub struct RecallTool;

#[derive(Deserialize)]
struct RecallArgs {
    query: String,
    #[serde(default = "default_top_k")]
    top_k: usize,
}

const fn default_top_k() -> usize {
    10
}

#[async_trait]
impl Tool for RecallTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "recall".to_string(),
            description: "Searches memories.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Query."},
                    "top_k": {"type": "integer", "description": "Max results (default 10).", "default": 10}
                },
                "required": ["query"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let parsed: RecallArgs = serde_json::from_str(args)?;
        let h = match handle(ctx) {
            Some(h) => h,
            None => return Ok(NO_MEMORY_MSG.to_string()),
        };
        // C3：工具侧接 `[memory] rank_lifecycle_weight`（runtime 装配时经
        // MemoryRankWeight 扩展注入）；缺失时回落引擎默认权重 0.3，行为不变。
        let results = match ctx.extensions.get::<MemoryRankWeight>() {
            Some(w) => h.recall_with_weight(&parsed.query, parsed.top_k, w.0)?,
            None => h.recall(&parsed.query, parsed.top_k)?,
        };
        if results.is_empty() {
            return Ok(format!("no matches for '{}'", parsed.query));
        }
        let mut out = format!(
            "found {} match(es) for '{}':\n",
            results.len(),
            parsed.query
        );
        for (i, r) in results.iter().enumerate() {
            let preview: String = r.entry.content.chars().take(200).collect();
            out.push_str(&format!("  {}. [{}] {}\n", i + 1, r.entry.id, preview));
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with_engine() -> (ToolContext, MemoryHandle) {
        let engine: MemoryHandle = Arc::new(MemoryEngine::open_in_memory(true).unwrap());
        let ctx = ToolContext::new("t").with_extension(engine.clone());
        (ctx, engine)
    }

    #[tokio::test]
    async fn remember_recall_forget_roundtrip() {
        let (ctx, _e) = ctx_with_engine();
        RememberTool
            .execute(
                &ctx,
                r#"{"key":"greeting","value":"hello from the rust language"}"#,
            )
            .await
            .unwrap();
        let out = RecallTool
            .execute(&ctx, r#"{"query":"rust","top_k":5}"#)
            .await
            .unwrap();
        assert!(out.contains("greeting"), "recall should find it: {out}");
        let f = ForgetTool
            .execute(&ctx, r#"{"key":"greeting"}"#)
            .await
            .unwrap();
        assert!(f.contains("removed"));
    }

    #[tokio::test]
    async fn degrades_without_handle() {
        let ctx = ToolContext::new("t");
        let out = RecallTool.execute(&ctx, r#"{"query":"x"}"#).await.unwrap();
        assert!(out.contains("未启用"), "should degrade: {out}");
    }

    #[tokio::test]
    async fn recall_finds_semantic_match_with_embedder() {
        use deepseeknova_core::memory::embedding::EmbeddingProvider;

        /// 确定性测试替身：语义命中不需 FTS 共词。
        struct FakeEmbed;
        impl EmbeddingProvider for FakeEmbed {
            fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
                if text.contains("ferris") {
                    Ok(vec![0.9, 0.1])
                } else if text.contains("rust") {
                    Ok(vec![1.0, 0.0])
                } else {
                    Ok(vec![0.0, 1.0])
                }
            }
        }

        let engine: MemoryHandle = Arc::new(
            MemoryEngine::open_in_memory_with_embedder(
                true,
                Some(Arc::new(FakeEmbed)),
                Some("test-model".to_string()),
            )
            .unwrap(),
        );
        let ctx = ToolContext::new("t").with_extension(engine);
        RememberTool
            .execute(&ctx, r#"{"key":"k","value":"ferris crab language"}"#)
            .await
            .unwrap();
        let out = RecallTool
            .execute(&ctx, r#"{"query":"rust","top_k":5}"#)
            .await
            .unwrap();
        assert!(out.contains("k"), "语义独有命中必须被召回: {out}");
    }
}

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
            description:
                "持久记住一条信息（跨会话/重启保留），带唯一 key 与可选 tags。相同 key 覆盖更新。"
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "key": {"type": "string", "description": "Unique identifier for this memory."},
                    "value": {"type": "string", "description": "Content to store."},
                    "tags": {"type": "array", "items": {"type": "string"}, "description": "Optional tags."}
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
            description: "按 key 删除一条持久记忆。".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {"key": {"type": "string", "description": "Key to remove."}},
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
            description: "在持久记忆库中按相关度检索（跨会话），返回最匹配的条目。".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search query."},
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
        let results = h.recall(&parsed.query, parsed.top_k)?;
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
}

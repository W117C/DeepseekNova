//! delegate 工具：把子任务委派给独立子代理（explorer/coder/tester/reviewer）。
//! 引擎句柄经 `ToolContext.extensions` 注入（`DelegateHandle`），缺失时优雅降级。

use async_trait::async_trait;
use deepseeknova_agent::DelegateEngine;
use deepseeknova_core::{Tool, ToolContext, ToolSchema};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

/// 共享委派引擎句柄（runtime 注入，对称于 Graph/MemoryHandle）。
pub type DelegateHandle = Arc<DelegateEngine>;

const NO_DELEGATE_MSG: &str = "委派引擎未启用（[delegate] enabled=false 或未装配）。";

fn handle(ctx: &ToolContext) -> Option<DelegateHandle> {
    ctx.extensions.get::<DelegateHandle>().cloned()
}

pub struct DelegateTool;

#[derive(Deserialize)]
struct DelegateArgs {
    agent: String,
    goal: String,
    #[serde(default)]
    context: Option<String>,
}

#[async_trait]
impl Tool for DelegateTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "delegate".to_string(),
            description: "Delegates a subtask to a sub-agent; no re-delegation.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "enum": ["explorer", "coder", "tester", "reviewer"],
                        "description": "Agent."
                    },
                    "goal": {"type": "string", "description": "Goal."},
                    "context": {"type": "string", "description": "Context."}
                },
                "required": ["agent", "goal"]
            }),
        }
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let parsed: DelegateArgs = serde_json::from_str(args)?;
        let h = match handle(ctx) {
            Some(h) => h,
            None => return Ok(NO_DELEGATE_MSG.to_string()),
        };
        let goal = match parsed.context {
            Some(c) if !c.is_empty() => format!("{c}\n\n{}", parsed.goal),
            _ => parsed.goal.clone(),
        };
        match h.run(&parsed.agent, &goal).await {
            Ok(text) => Ok(format!("[delegate:{}] {text}", parsed.agent)),
            Err(e) => Ok(format!(
                "delegate to '{}' failed: {e}. Available agents: {}",
                parsed.agent,
                h.agent_names().join(", ")
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_agent::{Agent, DelegateEngine};
    use std::collections::HashMap;

    fn ctx_with_engine() -> ToolContext {
        let mut agents: HashMap<String, Arc<Agent>> = HashMap::new();
        agents.insert(
            "explorer".into(),
            Arc::new(
                Agent::new(
                    Arc::new(deepseeknova_agent::test_utils::MockProvider::text(
                        "found the config in lib.rs",
                    )),
                    3,
                )
                .with_system_prompt("explorer"),
            ),
        );
        let engine: DelegateHandle = Arc::new(DelegateEngine::new(agents, 2, 2000));
        ToolContext::new("t").with_extension(engine)
    }

    #[tokio::test]
    async fn delegate_runs_named_agent() {
        let ctx = ctx_with_engine();
        let out = DelegateTool
            .execute(&ctx, r#"{"agent":"explorer","goal":"find config"}"#)
            .await
            .unwrap();
        assert!(out.contains("[delegate:explorer]"), "got: {out}");
        assert!(out.contains("found the config"));
    }

    #[tokio::test]
    async fn delegate_unknown_agent_is_friendly() {
        let ctx = ctx_with_engine();
        let out = DelegateTool
            .execute(&ctx, r#"{"agent":"nope","goal":"x"}"#)
            .await
            .unwrap();
        assert!(out.contains("failed"), "got: {out}");
        assert!(out.contains("Available agents: explorer"));
    }

    #[tokio::test]
    async fn delegate_degrades_without_handle() {
        let ctx = ToolContext::new("t");
        let out = DelegateTool
            .execute(&ctx, r#"{"agent":"explorer","goal":"x"}"#)
            .await
            .unwrap();
        assert!(out.contains("未启用"), "got: {out}");
    }
}

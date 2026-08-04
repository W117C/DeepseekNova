//! delegate 工具：把子任务委派给独立子代理（explorer/coder/tester/reviewer）。
//! 引擎句柄经 `ToolContext.extensions` 注入（`DelegateHandle`），缺失时优雅降级。
//! 参数化任务书：`inputs` 传值（`${{ inputs.x }}` 占位符），仅对已声明 inputs
//! 的预设生效；simple 预设的多余键被忽略。

use async_trait::async_trait;
use deepseeknova_agent::task_spec::InputValues;
use deepseeknova_agent::DelegateEngine;
use deepseeknova_core::{Tool, ToolContext, ToolSchema};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
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
    /// 参数化任务书传值（`${{ inputs.<name> }}` 占位符），仅对已声明 inputs 的预设生效。
    #[serde(default)]
    inputs: Option<HashMap<String, String>>,
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
                    "context": {"type": "string", "description": "Context."},
                    "inputs": {
                        "type": "object",
                        "additionalProperties": {"type": "string"},
                        "description": "Values for task-spec placeholders (${{ inputs.x }}). Ignored for presets without declared inputs."
                    }
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
        let values = InputValues::from(parsed.inputs.unwrap_or_default());
        match h.run_with_inputs(&parsed.agent, &goal, values).await {
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

    #[tokio::test]
    async fn delegate_inputs_supply_required() {
        use deepseeknova_agent::task_spec::{InputSpec, InputType, InputValues, TaskSpec};
        use std::collections::HashMap;

        let mut agents: HashMap<String, Arc<Agent>> = HashMap::new();
        agents.insert(
            "reviewer".into(),
            Arc::new(
                Agent::new(
                    Arc::new(deepseeknova_agent::test_utils::MockProvider::text(
                        "reviewed",
                    )),
                    3,
                )
                .with_system_prompt("reviewer"),
            ),
        );
        let mut engine = DelegateEngine::new(agents, 2, 2000);
        engine.register_spec(
            "reviewer".into(),
            TaskSpec {
                name: "reviewer".into(),
                task: "Review ${{ inputs.path }}".into(),
                rules: vec![],
                inputs: vec![InputSpec {
                    name: "path".into(),
                    ty: InputType::String,
                    required: true,
                    default: None,
                }],
                tools: vec!["read_file".into()],
                max_steps: 10,
            },
            InputValues::new(),
        );
        let ctx = ToolContext::new("t").with_extension(Arc::new(engine) as DelegateHandle);
        let out = DelegateTool
            .execute(
                &ctx,
                r#"{"agent":"reviewer","goal":"go","inputs":{"path":"src/lib.rs"}}"#,
            )
            .await
            .unwrap();
        assert!(out.contains("[delegate:reviewer]"), "got: {out}");
        assert!(out.contains("reviewed"), "got: {out}");
    }

    #[tokio::test]
    async fn delegate_missing_required_is_friendly() {
        use deepseeknova_agent::task_spec::{InputSpec, InputType, InputValues, TaskSpec};
        use std::collections::HashMap;

        let mut agents: HashMap<String, Arc<Agent>> = HashMap::new();
        agents.insert(
            "reviewer".into(),
            Arc::new(
                Agent::new(
                    Arc::new(deepseeknova_agent::test_utils::MockProvider::text(
                        "reviewed",
                    )),
                    3,
                )
                .with_system_prompt("reviewer"),
            ),
        );
        let mut engine = DelegateEngine::new(agents, 2, 2000);
        engine.register_spec(
            "reviewer".into(),
            TaskSpec {
                name: "reviewer".into(),
                task: "Review ${{ inputs.path }}".into(),
                rules: vec![],
                inputs: vec![InputSpec {
                    name: "path".into(),
                    ty: InputType::String,
                    required: true,
                    default: None,
                }],
                tools: vec!["read_file".into()],
                max_steps: 10,
            },
            InputValues::new(),
        );
        let ctx = ToolContext::new("t").with_extension(Arc::new(engine) as DelegateHandle);
        let out = DelegateTool
            .execute(&ctx, r#"{"agent":"reviewer","goal":"go"}"#)
            .await
            .unwrap();
        assert!(out.contains("missing required input 'path'"), "got: {out}");
        assert!(out.contains("Available agents: reviewer"), "got: {out}");
    }
}

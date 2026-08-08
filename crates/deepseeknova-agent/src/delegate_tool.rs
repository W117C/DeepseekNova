//! delegate 工具：把子任务委派给独立子代理（explorer/coder/tester/reviewer）。
//! 引擎句柄经 `ToolContext.extensions` 注入（`DelegateHandle`），缺失时优雅降级。
//! 参数化任务书：`inputs` 传值（`${{ inputs.x }}` 占位符），仅对已声明 inputs
//! 的预设生效；simple 预设的多余键被忽略。
//!
//! 递归：本工具支持深度受限的再派发。当前深度读 `ToolContext` 注入的
//! [`DelegateDepth`]（Agent 主循环注入根深度 1），
//! 本次派发深度 = current + 1，经 [`DelegateEngine::run_at_depth`] 派发；超过引擎
//! 深度上限时优雅返回错误文本（不硬失败），语义与本 crate 的 `RecursiveDelegateTool`
//! 对齐。
//!
//! 注：本模块原属 `deepseeknova-tools` crate，因对 `DelegateEngine` /
//! `DelegateDepth` / `InputValues` 的依赖形成 `tools → agent` 反向依赖，
//! 于 2026-08-08 移入 agent crate 以消除该依赖 inversion。

use crate::recursion::DelegateDepth;
use crate::task_spec::InputValues;
use crate::DelegateEngine;
use async_trait::async_trait;
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
            description:
                "Delegates a subtask to a sub-agent; recursion is allowed up to a depth limit."
                    .to_string(),
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
        // 能力门禁（L4）：裸装配 DelegateEngine（SecurityContext 缺失或未授予
        // CommandExecute）时拒绝，防库级装配绕过能力系统。生产 CLI 路径继承
        // 共享 gate+security（默认全能力），无提权；子代理递归走
        // RecursiveDelegateTool（独立深度守门），不在此工具覆盖范围内。
        deepseeknova_security::context::enforce_capability(
            ctx,
            "delegate",
            deepseeknova_security::capability::Capability::CommandExecute,
        )?;
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
        // 递归深度：读执行上下文注入的 DelegateDepth（缺失按 0 处理），
        // 本次派发深度 = current + 1 —— 与 RecursiveDelegateTool 的
        // "current 0 → next 1" 语义对齐。超深由 run_at_depth 守门拒绝。
        let current = ctx
            .extensions
            .get::<DelegateDepth>()
            .map(|d| d.0)
            .unwrap_or(0);
        let next = current + 1;
        match h.run_at_depth(&parsed.agent, &goal, values, next).await {
            Ok(text) => Ok(format!("[delegate:{}] {text}", parsed.agent)),
            Err(e) => Ok(format!(
                "delegate to '{}' failed at depth {next}: {e}. Available agents: {}",
                parsed.agent,
                h.agent_names().join(", ")
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::MockProvider;
    use crate::{Agent, DelegateEngine};
    use deepseeknova_security::context::SecurityContext;
    use std::collections::HashMap;

    fn ctx_with_engine() -> ToolContext {
        let mut agents: HashMap<String, Arc<Agent>> = HashMap::new();
        agents.insert(
            "explorer".into(),
            Arc::new(
                Agent::new(
                    Arc::new(MockProvider::text("found the config in lib.rs")),
                    3,
                )
                .with_system_prompt("explorer"),
            ),
        );
        let engine: DelegateHandle = Arc::new(DelegateEngine::new(agents, 2, 2000));
        // 生产路径的 ToolContext 恒注入 SecurityContext（默认全能力含
        // CommandExecute）；测试对齐该装配，能力门禁（L4）方能放行。
        ToolContext::new("t")
            .with_extension(SecurityContext::with_safe_defaults())
            .with_extension(engine)
    }

    /// 构造带一个 explorer 子代理的引擎（指定递归深度上限）。
    fn engine_with_explorer(max_depth: usize) -> DelegateHandle {
        let mut agents: HashMap<String, Arc<Agent>> = HashMap::new();
        agents.insert(
            "explorer".into(),
            Arc::new(
                Agent::new(
                    Arc::new(MockProvider::text("found the config in lib.rs")),
                    3,
                )
                .with_system_prompt("explorer"),
            ),
        );
        Arc::new(DelegateEngine::new(agents, 2, 2000).with_max_depth(max_depth))
    }

    #[tokio::test]
    async fn delegate_reads_injected_depth() {
        // 注入当前深度 2 → 本次派发 3；引擎 max_depth=2 → 守门拒绝并暴露
        // 请求深度（证明 DelegateTool 读取扩展并递增）。
        let engine = engine_with_explorer(2);
        let mut ctx = ToolContext::new("t")
            .with_extension(SecurityContext::with_safe_defaults())
            .with_extension(engine);
        ctx.extensions.insert(DelegateDepth(2));
        let out = DelegateTool
            .execute(&ctx, r#"{"agent":"explorer","goal":"find config"}"#)
            .await
            .unwrap();
        assert!(out.contains("recursion depth exceeded"), "got: {out}");
        assert!(out.contains("depth requested: 3"), "got: {out}");
    }

    #[tokio::test]
    async fn delegate_missing_depth_uses_root_depth() {
        // 未注入 DelegateDepth → current 0 → next 1（根深度，与
        // RecursiveDelegateTool "current 0 → next 1" 语义对齐）；max_depth=1
        // 放行即证明本次派发深度恰为 1。
        let engine = engine_with_explorer(1);
        let ctx = ToolContext::new("t")
            .with_extension(SecurityContext::with_safe_defaults())
            .with_extension(engine);
        let out = DelegateTool
            .execute(&ctx, r#"{"agent":"explorer","goal":"find config"}"#)
            .await
            .unwrap();
        assert!(out.contains("[delegate:explorer]"), "got: {out}");
        assert!(out.contains("found the config"), "got: {out}");
    }

    #[tokio::test]
    async fn delegate_over_depth_is_graceful() {
        // 注入当前深度 3 → next 4 > max_depth=3 → 优雅错误文本（Ok 结果，
        // 不硬失败），模型可据此降级。
        let engine = engine_with_explorer(3);
        let mut ctx = ToolContext::new("t")
            .with_extension(SecurityContext::with_safe_defaults())
            .with_extension(engine);
        ctx.extensions.insert(DelegateDepth(3));
        let out = DelegateTool
            .execute(&ctx, r#"{"agent":"explorer","goal":"x"}"#)
            .await
            .unwrap();
        assert!(out.contains("recursion depth exceeded"), "got: {out}");
        assert!(out.contains("depth requested: 4"), "got: {out}");
        assert!(out.contains("Available agents: explorer"), "got: {out}");
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
        let ctx = ToolContext::new("t").with_extension(SecurityContext::with_safe_defaults());
        let out = DelegateTool
            .execute(&ctx, r#"{"agent":"explorer","goal":"x"}"#)
            .await
            .unwrap();
        assert!(out.contains("未启用"), "got: {out}");
    }

    #[tokio::test]
    async fn delegate_denied_without_command_execute_capability() {
        // L4 门禁：裸装配 + 受限能力上下文（仅 FileRead，未授予
        // CommandExecute）→ 拒绝。生产 CLI 路径无提权（继承共享 gate+security），
        // 此门禁防库级裸装配绕过能力系统。
        let engine = engine_with_explorer(1);
        let mut sec = SecurityContext::with_safe_defaults();
        sec.capabilities = {
            let mut caps = std::collections::HashSet::new();
            caps.insert(deepseeknova_security::capability::Capability::FileRead);
            caps
        };
        let ctx = ToolContext::new("t")
            .with_extension(sec)
            .with_extension(engine);
        let err = DelegateTool
            .execute(&ctx, r#"{"agent":"explorer","goal":"x"}"#)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("CommandExecute"),
            "restricted context must be denied: {err}"
        );

        // 完全无 SecurityContext 的裸装配 → 同样拒绝（能力系统强制存在）。
        let bare = ToolContext::new("t").with_extension(engine_with_explorer(1));
        let err = DelegateTool
            .execute(&bare, r#"{"agent":"explorer","goal":"x"}"#)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("SecurityContext"),
            "bare assembly must be denied: {err}"
        );
    }

    #[tokio::test]
    async fn delegate_inputs_supply_required() {
        use crate::task_spec::{InputSpec, InputType, InputValues, TaskSpec};
        use std::collections::HashMap;

        let mut agents: HashMap<String, Arc<Agent>> = HashMap::new();
        agents.insert(
            "reviewer".into(),
            Arc::new(
                Agent::new(Arc::new(MockProvider::text("reviewed")), 3)
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
        let ctx = ToolContext::new("t")
            .with_extension(SecurityContext::with_safe_defaults())
            .with_extension(Arc::new(engine) as DelegateHandle);
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
        use crate::task_spec::{InputSpec, InputType, InputValues, TaskSpec};
        use std::collections::HashMap;

        let mut agents: HashMap<String, Arc<Agent>> = HashMap::new();
        agents.insert(
            "reviewer".into(),
            Arc::new(
                Agent::new(Arc::new(MockProvider::text("reviewed")), 3)
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
        let ctx = ToolContext::new("t")
            .with_extension(SecurityContext::with_safe_defaults())
            .with_extension(Arc::new(engine) as DelegateHandle);
        let out = DelegateTool
            .execute(&ctx, r#"{"agent":"reviewer","goal":"go"}"#)
            .await
            .unwrap();
        assert!(out.contains("missing required input 'path'"), "got: {out}");
        assert!(out.contains("Available agents: reviewer"), "got: {out}");
    }
}

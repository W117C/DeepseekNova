//! P1 确定性 Verify：文件写入轮完成时，按配置命令经 `bash` 工具验证。
//!
//! 复用与普通工具调用相同的 SecurityContext（沙箱、命令白名单、资源限制），
//! 失败结果以 User 消息回喂循环（不能伪装成 Tool 结果——无对应 tool_call_id
//! 会破坏 DeepSeek V4 replay 不变量）；超过 max_cycles 后优雅 Paused。

use deepseeknova_core::Tool;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::agent::build_tool_context;

/// 验证配置（runtime 从 `[verify]` 配置段装配）。
#[derive(Debug, Clone)]
pub(crate) struct VerifySettings {
    pub commands: Vec<String>,
    pub max_cycles: usize,
}

/// 一轮验证的结果。
#[derive(Debug, PartialEq)]
pub(crate) enum VerifyOutcome {
    /// 全部命令通过。
    Pass,
    /// 至少一条命令失败（含退出码/超时/安全拦截），附失败摘要。
    Fail(String),
    /// bash 工具未注册等降级情况：不阻断 Done。
    Skipped,
}

/// 失败摘要注入上下文的上限（字符），防止验证输出撑爆上下文。
const FAILURE_CAP_CHARS: usize = 2000;

/// 逐条运行验证命令。任一命令失败即返回 `Fail`；`bash` 未注册返回 `Skipped`。
pub(crate) async fn run_verify_pass(
    tool_map: &HashMap<String, Arc<dyn Tool>>,
    settings: &VerifySettings,
    workspace_root: &Path,
    security: &deepseeknova_security::context::SecurityContext,
    extensions: &[Arc<crate::agent::ExtensionApplier>],
    cancel: &CancellationToken,
) -> VerifyOutcome {
    let Some(bash) = tool_map.get("bash") else {
        warn!("verify skipped: bash tool not registered");
        return VerifyOutcome::Skipped;
    };
    if settings.commands.is_empty() {
        return VerifyOutcome::Pass;
    }
    for cmd in &settings.commands {
        if cancel.is_cancelled() {
            return VerifyOutcome::Skipped;
        }
        let ctx = build_tool_context(
            &format!("verify_{}", uuid::Uuid::new_v4()),
            cancel.child_token(),
            workspace_root,
            security,
            extensions,
        );
        let args = serde_json::json!({ "command": cmd }).to_string();
        match bash.execute(&ctx, &args).await {
            Ok(_) => {}
            Err(e) => {
                let msg = format!("command `{cmd}` failed: {e:#}");
                let capped: String = msg.chars().take(FAILURE_CAP_CHARS).collect();
                return VerifyOutcome::Fail(capped);
            }
        }
    }
    VerifyOutcome::Pass
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_core::tool::ToolContext;
    use deepseeknova_core::types::ToolSchema;
    use deepseeknova_security::context::SecurityContext;

    struct FakeBash {
        fail_on: Option<String>,
    }

    #[async_trait::async_trait]
    impl Tool for FakeBash {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: "bash".to_string(),
                description: "fake bash".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }
        async fn execute(&self, _ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
            let v: serde_json::Value = serde_json::from_str(args)?;
            let cmd = v["command"].as_str().unwrap_or_default();
            if self.fail_on.as_deref() == Some(cmd) {
                anyhow::bail!("command exited with code 1");
            }
            Ok("ok".to_string())
        }
    }

    fn settings(cmds: &[&str]) -> VerifySettings {
        VerifySettings {
            commands: cmds.iter().map(|s| s.to_string()).collect(),
            max_cycles: 1,
        }
    }

    #[tokio::test]
    async fn verify_passes_when_all_commands_succeed() {
        let map: HashMap<String, Arc<dyn Tool>> = HashMap::from([(
            "bash".to_string(),
            Arc::new(FakeBash { fail_on: None }) as Arc<dyn Tool>,
        )]);
        let sec = SecurityContext::with_safe_defaults();
        let cancel = CancellationToken::new();
        let outcome = run_verify_pass(
            &map,
            &settings(&["cargo check --quiet", "cargo test --quiet"]),
            Path::new("."),
            &sec,
            &[],
            &cancel,
        )
        .await;
        assert_eq!(outcome, VerifyOutcome::Pass);
    }

    #[tokio::test]
    async fn verify_fails_on_first_failing_command() {
        let map: HashMap<String, Arc<dyn Tool>> = HashMap::from([(
            "bash".to_string(),
            Arc::new(FakeBash {
                fail_on: Some("cargo test --quiet".to_string()),
            }) as Arc<dyn Tool>,
        )]);
        let sec = SecurityContext::with_safe_defaults();
        let cancel = CancellationToken::new();
        let outcome = run_verify_pass(
            &map,
            &settings(&["cargo check --quiet", "cargo test --quiet"]),
            Path::new("."),
            &sec,
            &[],
            &cancel,
        )
        .await;
        match outcome {
            VerifyOutcome::Fail(msg) => {
                assert!(msg.contains("cargo test --quiet"), "msg: {msg}");
                assert!(msg.contains("code 1"), "msg: {msg}");
            }
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_skips_when_bash_missing() {
        let map: HashMap<String, Arc<dyn Tool>> = HashMap::new();
        let sec = SecurityContext::with_safe_defaults();
        let cancel = CancellationToken::new();
        let outcome = run_verify_pass(
            &map,
            &settings(&["cargo check"]),
            Path::new("."),
            &sec,
            &[],
            &cancel,
        )
        .await;
        assert_eq!(outcome, VerifyOutcome::Skipped);
    }
}

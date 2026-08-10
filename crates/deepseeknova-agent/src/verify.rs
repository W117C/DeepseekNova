//! P1 确定性 Verify：文件写入轮完成时，按配置命令经 `bash` 工具验证。
//!
//! 复用与普通工具调用相同的 SecurityContext（沙箱、命令白名单、资源限制），
//! 失败结果以 User 消息回喂循环（不能伪装成 Tool 结果——无对应 tool_call_id
//! 会破坏 DeepSeek V4 replay 不变量）；超过 max_cycles 后优雅 Paused。

use deepseeknova_core::RunEvent;
use deepseeknova_core::Tool;
use deepseeknova_core::{Message, Role};
use deepseeknova_provider::{Provider, ValidatedRequest};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::agent::build_tool_context;

/// 验证配置（runtime 从 `[verify]` 配置段装配）。
#[derive(Clone)]
pub(crate) struct VerifySettings {
    pub commands: Vec<String>,
    pub max_cycles: usize,
    /// 可选的 LLM 验证 provider（`[verify] llm = true` 时装配）。
    pub llm_provider: Option<Arc<dyn Provider>>,
    /// LLM 验证的完成文本输入上限（字符）。
    pub llm_max_chars: usize,
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
    tx: &mpsc::Sender<Result<RunEvent, deepseeknova_core::DeepseeknovaError>>,
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
            Ok(_) => {
                tx.send(Ok(RunEvent::Verification {
                    command: cmd.clone(),
                    passed: true,
                    summary: "ok".to_string(),
                }))
                .await
                .ok();
            }
            Err(e) => {
                let msg = format!("command `{cmd}` failed: {e:#}");
                let capped: String = msg.chars().take(FAILURE_CAP_CHARS).collect();
                tx.send(Ok(RunEvent::Verification {
                    command: cmd.clone(),
                    passed: false,
                    summary: capped.clone(),
                }))
                .await
                .ok();
                return VerifyOutcome::Fail(capped);
            }
        }
    }
    VerifyOutcome::Pass
}

/// 渲染 LLM 验证 prompt：任务 + 完成声明 → 严格要求 JSON 判定。
pub(crate) fn render_verify_prompt(task: &str, completion: &str) -> String {
    format!(
        "You are a verifier in the Verify phase of the \
         Observe → Plan → Tool → Verify → Reflect → Next Action loop. The agent \
         claims the task is complete. Determine whether the completion actually \
         satisfies the task; verify correctness against the task, do not invent \
         failures. Respond with ONLY a JSON object: {{\"passed\": true}} or \
         {{\"passed\": false, \"reason\": \"...\"}}.\n\n\
         # Task\n{task}\n\n# Completion\n{completion}"
    )
}

/// 宽松解析 LLM 验证判定：passed=true → Pass；passed=false → Fail(reason)；
/// 缺失/非法 → None（调用方按降级跳过，绝不阻断 Done）。
pub(crate) fn parse_verify_outcome(raw: &str) -> Option<VerifyOutcome> {
    let json_str = crate::review::extract_json(raw)?;
    let v: serde_json::Value = serde_json::from_str(&json_str).ok()?;
    match v.get("passed")?.as_bool()? {
        true => Some(VerifyOutcome::Pass),
        false => {
            let reason = v
                .get("reason")
                .and_then(|r| r.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| "no reason given".to_string());
            let capped: String = reason.chars().take(FAILURE_CAP_CHARS).collect();
            Some(VerifyOutcome::Fail(capped))
        }
    }
}

/// 单次 LLM 验证（复用 review 同款 ValidatedRequest 通路）。调用/解析失败
/// 一律优雅降级为 Skipped（warn），不阻断 Done；只有模型明确判定失败才 Fail。
pub(crate) async fn run_llm_verify_pass(
    provider: &dyn Provider,
    task: &str,
    completion: &str,
    max_chars: usize,
) -> VerifyOutcome {
    let completion_capped: String = completion.chars().take(max_chars).collect();
    let prompt = render_verify_prompt(task, &completion_capped);
    let msgs = vec![Message {
        role: Role::User,
        content: prompt,
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }];
    let validated = match ValidatedRequest::new(&msgs, &[]) {
        Ok(v) => v,
        Err(violations) => {
            warn!(
                "invalid llm verify request ({}); skipping verify",
                violations.join("; ")
            );
            return VerifyOutcome::Skipped;
        }
    };
    match provider.generate(validated).await {
        Ok(out) => parse_verify_outcome(&out.content).unwrap_or_else(|| {
            warn!("llm verify response unparseable; skipping verify");
            VerifyOutcome::Skipped
        }),
        Err(e) => {
            warn!("llm verify call failed ({e}); skipping verify");
            VerifyOutcome::Skipped
        }
    }
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
        async fn execute(
            &self,
            _ctx: &ToolContext,
            args: &str,
        ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
            let v: serde_json::Value = serde_json::from_str(args)?;
            let cmd = v["command"].as_str().unwrap_or_default();
            if self.fail_on.as_deref() == Some(cmd) {
                return Err(deepseeknova_core::DeepseeknovaError::tool(
                    "command exited with code 1".to_string(),
                ));
            }
            Ok("ok".to_string())
        }
    }

    fn settings(cmds: &[&str]) -> VerifySettings {
        VerifySettings {
            commands: cmds.iter().map(|s| s.to_string()).collect(),
            max_cycles: 1,
            llm_provider: None,
            llm_max_chars: 0,
        }
    }

    fn channel() -> mpsc::Sender<Result<RunEvent, deepseeknova_core::DeepseeknovaError>> {
        let (tx, _rx) = mpsc::channel(8);
        tx
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
            &channel(),
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
            &channel(),
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
            &channel(),
        )
        .await;
        assert_eq!(outcome, VerifyOutcome::Skipped);
    }

    #[test]
    fn parses_llm_verify_passed_and_failed() {
        assert_eq!(
            parse_verify_outcome(r#"{"passed": true}"#),
            Some(VerifyOutcome::Pass)
        );
        assert_eq!(
            parse_verify_outcome(
                r#"```json
{"passed": false, "reason": "missing tests"}
```"#
            ),
            Some(VerifyOutcome::Fail("missing tests".into()))
        );
        // passed=false 无 reason 给占位原因
        match parse_verify_outcome(r#"{"passed": false}"#) {
            Some(VerifyOutcome::Fail(reason)) => assert!(!reason.is_empty()),
            other => panic!("expected Fail, got {other:?}"),
        }
        // 缺失/非法判定一律 None（调用方降级跳过）
        assert_eq!(parse_verify_outcome("not json"), None);
        assert_eq!(parse_verify_outcome(r#"{"verdict":"approve"}"#), None);
        assert_eq!(parse_verify_outcome(r#"{"passed":"yes"}"#), None);
    }

    #[test]
    fn verify_prompt_keeps_verify_phase_and_contract() {
        let p = render_verify_prompt("fix auth", "done");
        for s in [
            "Verify phase",
            "# Task",
            "# Completion",
            "{\"passed\": true}",
        ] {
            assert!(p.contains(s), "prompt 缺少 {s}");
        }
    }

    struct FixedProvider {
        content: String,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl Provider for FixedProvider {
        async fn generate(
            &self,
            _validated: ValidatedRequest<'_>,
        ) -> Result<Message, deepseeknova_core::DeepseeknovaError> {
            if self.fail {
                return Err(deepseeknova_core::DeepseeknovaError::provider(
                    "provider down",
                ));
            }
            Ok(Message {
                role: Role::Assistant,
                content: self.content.clone(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            })
        }
    }

    #[tokio::test]
    async fn llm_verify_routes_pass_fail_and_skip() {
        let pass = FixedProvider {
            content: r#"{"passed": true}"#.into(),
            fail: false,
        };
        assert_eq!(
            run_llm_verify_pass(&pass, "task", "output", 4000).await,
            VerifyOutcome::Pass
        );

        let fail = FixedProvider {
            content: r#"{"passed": false, "reason": "API 行为不符合任务"}"#.into(),
            fail: false,
        };
        assert_eq!(
            run_llm_verify_pass(&fail, "task", "output", 4000).await,
            VerifyOutcome::Fail("API 行为不符合任务".into())
        );

        // provider 失败与不可解析响应都优雅降级为 Skipped
        let down = FixedProvider {
            content: String::new(),
            fail: true,
        };
        assert_eq!(
            run_llm_verify_pass(&down, "task", "output", 4000).await,
            VerifyOutcome::Skipped
        );
        let garbage = FixedProvider {
            content: "I think it's fine".into(),
            fail: false,
        };
        assert_eq!(
            run_llm_verify_pass(&garbage, "task", "output", 4000).await,
            VerifyOutcome::Skipped
        );
    }

    #[tokio::test]
    async fn llm_verify_caps_completion_chars() {
        let cap = FixedProvider {
            content: r#"{"passed": true}"#.into(),
            fail: false,
        };
        let long = "x".repeat(10_000);
        assert_eq!(
            run_llm_verify_pass(&cap, "task", &long, 64).await,
            VerifyOutcome::Pass
        );
    }
}

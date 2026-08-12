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

/// A5：判断 verify 失败是否为瞬态类别（超时/网络/暂时不可用）。
/// 仅这类错误值得确定性重试；命令/参数错误重试无效，直接回炉模型
/// （计划书反例：重试条件必须限定为可重试错误类别）。
fn is_transient_verify_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.contains("timeout")
        || m.contains("timed out")
        || m.contains("connection")
        || m.contains("temporarily")
}

/// A5：瞬态失败自动重试次数上限。
const TRANSIENT_RETRY_LIMIT: usize = 1;

/// A8：从会话工具结果中推断验证命令（验证命令发现）。
///
/// 扫描工具结果文本，识别 agent 已运行过的构建/测试命令形态
/// （cargo check / cargo test / tsc --noEmit / go test / make check /
/// pytest 等），返回去重后的候选命令。显式配置 `[verify] commands` 仍优先
/// ——本函数仅在配置命令为空时作为兜底（§4.8 差距②：自动化验证闭环的
/// "验证命令发现"能力，CC 从工具结果/命令历史推断验证命令）。
///
/// 保守策略：只识别明确、无副作用的验证命令形态（check/test/build），
/// 不做危险命令推断；未见任何候选时返回空。
pub(crate) fn infer_verify_commands(tool_results: &[String]) -> Vec<String> {
    // 候选：正则匹配命令前缀 → 归一化命令。按优先级排序（check 先于 test）。
    const CANDIDATES: &[(&str, &str)] = &[
        (r"cargo check", "cargo check"),
        (r"cargo test", "cargo test"),
        (r"cargo build", "cargo build"),
        (r"cargo clippy", "cargo clippy"),
        (r"tsc --noEmit", "tsc --noEmit"),
        (r"tsc --noemit", "tsc --noEmit"),
        (r"go test", "go test"),
        (r"go build", "go build"),
        (r"make check", "make check"),
        (r"make test", "make test"),
        (r"pytest", "pytest"),
        (r"npm test", "npm test"),
        (r"npm run check", "npm run check"),
        (r"pnpm test", "pnpm test"),
        (r"yarn test", "yarn test"),
    ];
    let mut seen: Vec<String> = Vec::new();
    for result in tool_results {
        for (pat, cmd) in CANDIDATES {
            // 单词边界匹配：防止 `cargo test` 命中 "go test"（"car-go test"）。
            if contains_word(result, pat) && !seen.iter().any(|s| s == *cmd) {
                seen.push((*cmd).to_string());
            }
        }
    }
    seen
}

/// 单词边界匹配：`needle` 在 `haystack` 中出现且前后不是字母/数字
/// （避免 `cargo test` 里的子串 "go test" 被误判为 go 命令）。
fn contains_word(haystack: &str, needle: &str) -> bool {
    haystack.match_indices(needle).any(|(idx, _)| {
        let before_ok = idx == 0
            || !haystack[..idx]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric());
        let after = idx + needle.len();
        let after_ok = after >= haystack.len()
            || !haystack[after..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric());
        before_ok && after_ok
    })
}

/// 逐条运行验证命令。任一命令失败即返回 `Fail`；`bash` 未注册返回 `Skipped`。
/// A5：瞬态失败（超时/网络）自动重试一次（确定性恢复优先，先于模型反思）。
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
        // A5：瞬态失败重试（同一条命令，最多重试一次）。
        let mut transient_retries = 0usize;
        loop {
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
                    break;
                }
                Err(e) => {
                    let msg = format!("command `{cmd}` failed: {e:#}");
                    // A5：瞬态错误自动重试，仍失败才回炉（模型反思在循环层）。
                    if transient_retries < TRANSIENT_RETRY_LIMIT && is_transient_verify_error(&msg)
                    {
                        transient_retries += 1;
                        warn!(
                            transient_retries,
                            "verify command failed transiently, retrying: {msg}"
                        );
                        continue;
                    }
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
    // 结构化契约校验：passed 必须是布尔字段（缺失/类型不符 → 降级）。
    match crate::contract::require_bool(&v, "passed").ok()? {
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
///
/// A2：接入 `contract::retry_parsed` —— 非法 JSON 判定自动回显重试 ≤1 次，
/// 弱模型输出不合规时不直接降级跳过（提升 verify 判定可用率）。
pub(crate) async fn run_llm_verify_pass(
    provider: &dyn Provider,
    task: &str,
    completion: &str,
    max_chars: usize,
) -> VerifyOutcome {
    use crate::contract::{default_echo, retry_parsed, ContractOutcome};

    const MAX_RETRIES: u32 = 1;
    let completion_capped: String = completion.chars().take(max_chars).collect();
    let initial = render_verify_prompt(task, &completion_capped);

    let outcome = retry_parsed(
        MAX_RETRIES,
        |p| {
            let msgs = vec![Message {
                role: Role::User,
                content: p.to_string(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                reasoning_signature: None,
            }];
            async move {
                let validated = match ValidatedRequest::new(&msgs, &[]) {
                    Ok(v) => v,
                    Err(violations) => {
                        warn!(
                            "invalid llm verify request ({}); skipping verify",
                            violations.join("; ")
                        );
                        return None;
                    }
                };
                match provider.generate(validated).await {
                    Ok(out) => Some(out.content),
                    Err(e) => {
                        warn!("llm verify call failed ({e}); skipping verify");
                        None
                    }
                }
            }
        },
        |raw| {
            parse_verify_outcome(raw)
                .ok_or_else(|| "missing/invalid `passed` field in verify JSON".to_string())
        },
        |last, reason| match (last, reason) {
            (Some(raw), Some(hint)) => default_echo(&initial, raw, hint),
            _ => initial.clone(),
        },
    )
    .await;

    match outcome {
        ContractOutcome::Ok(v) => v,
        ContractOutcome::Fallback => {
            warn!("llm verify response unparseable after retries; skipping verify");
            VerifyOutcome::Skipped
        }
        ContractOutcome::CallError(e) => {
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

    /// A8：从工具结果识别验证命令（去重、按优先级）。
    #[test]
    fn infers_verify_commands_from_tool_results() {
        let results = vec![
            "running `cargo check`...".to_string(),
            "running `cargo test -- --nocapture`...".to_string(),
            "cargo check finished".to_string(), // 重复 → 去重
        ];
        let cmds = infer_verify_commands(&results);
        assert!(cmds.contains(&"cargo check".to_string()));
        assert!(cmds.contains(&"cargo test".to_string()));
        assert_eq!(
            cmds.len(),
            2,
            "duplicate candidates must be de-duplicated: {cmds:?}"
        );
    }

    /// A8：无候选命令 → 空（不做危险推断）。
    #[test]
    fn infers_nothing_without_candidates() {
        assert!(infer_verify_commands(&["just some text".to_string()]).is_empty());
        assert!(infer_verify_commands(&[]).is_empty());
    }

    /// A8：tsc --noEmit 大小写归一。
    #[test]
    fn infers_tsc_noemit_case_insensitive() {
        let cmds = infer_verify_commands(&["run: tsc --noemit".to_string()]);
        assert_eq!(cmds, vec!["tsc --noEmit".to_string()]);
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
                reasoning_signature: None,
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

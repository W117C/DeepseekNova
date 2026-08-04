//! 任务质量钩子（任务质量闭环 A 阶段）。
//!
//! [`QualityHook`] 实现 [`ToolHook`]：对写类工具（`write_file` / `edit_file` /
//! `move_file` / `bash`）在 `before` 阶段按禁写路径策略拦截，在 `after` 阶段
//! 对结果文本运行内置质量策略并产出 findings。

use deepseeknova_core::tool_hook::{
    FindingSeverity, HookVerdict, QualityFinding, ToolHook, ToolHookCtx,
};
use deepseeknova_core::types::ToolCall;
use deepseeknova_security::quality::{extract_shell_write_paths, QualityPolicy};
use std::path::{Path, PathBuf};

/// 写类工具名单（与 agent 主循环判写名单一致，见 agent.rs 写回循环）。
const WRITE_TOOL_NAMES: &[&str] = &["write_file", "edit_file", "move_file", "bash"];

/// 从工具调用参数中解析目标路径。
///
/// - `write_file` / `edit_file`：`path`
/// - `move_file`：`source` / `destination`（destination 为写入目标）
/// - `bash` 等其余工具：无目标路径，返回 `None`
fn parse_target_path(tool_name: &str, args: &str) -> Option<PathBuf> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    let path = match tool_name {
        "write_file" | "edit_file" => v.get("path")?.as_str()?,
        "move_file" => v
            .get("destination")
            .or_else(|| v.get("source"))
            .and_then(|s| s.as_str())?,
        _ => return None,
    };
    Some(PathBuf::from(path))
}

/// 归一化：绝对路径若位于 workspace 内则剥掉前缀，便于策略 glob 匹配。
fn normalize_path<'a>(path: &'a Path, workspace_root: &'a Path) -> &'a Path {
    if path.is_absolute() {
        path.strip_prefix(workspace_root).unwrap_or(path)
    } else {
        path
    }
}

/// 质量策略钩子：`before` 拦截禁写路径，`after` 跑写后策略评估。
pub struct QualityHook {
    policy: QualityPolicy,
}

impl QualityHook {
    /// 构造钩子并持有给定策略。
    pub fn new(policy: QualityPolicy) -> Self {
        Self { policy }
    }
}

impl ToolHook for QualityHook {
    fn name(&self) -> &str {
        "quality"
    }

    /// 只对写类工具感兴趣（名单判定；这四个工具 `read_only() == false`，
    /// 与 agent 主循环的判写名单一致）。
    fn interested(&self, call: &ToolCall) -> bool {
        WRITE_TOOL_NAMES.contains(&call.function.name.as_str())
    }

    /// 预检：解析目标路径，命中 PathGlob deny 规则时拒绝执行。
    fn before(&self, ctx: &ToolHookCtx, call: &ToolCall) -> HookVerdict {
        if let Some(path) = parse_target_path(&call.function.name, &call.function.arguments) {
            let norm = normalize_path(&path, ctx.workspace_root);
            if let Some(rule_id) = self.policy.denied_path(norm) {
                return HookVerdict::Deny(format!("blocked by quality policy: {rule_id}"));
            }
        }
        // F1：bash 无结构化目标路径（parse_target_path 恒 None），改用轻量命令
        // 启发式提取疑似写入路径（重定向/tee/cp/mv/install），任一命中 deny →
        // 拒绝。相对路径直接按 workspace 相对语义判定（glob_matches 负责归一）。
        if call.function.name == "bash" {
            let command = serde_json::from_str::<serde_json::Value>(&call.function.arguments)
                .ok()
                .and_then(|v| v.get("command").and_then(|c| c.as_str()).map(str::to_owned))
                .unwrap_or_default();
            for p in extract_shell_write_paths(&command) {
                if let Some(rule_id) = self.policy.denied_path(Path::new(&p)) {
                    return HookVerdict::Deny(format!(
                        "blocked by quality policy: {rule_id} (bash write target `{p}`)"
                    ));
                }
            }
        }
        HookVerdict::Allow
    }

    /// 写后评估：对结果文本跑策略（正则/体积），变更路径从参数解析。
    fn after(&self, ctx: &ToolHookCtx, call: &ToolCall, result: &str) -> Vec<QualityFinding> {
        let changed: Vec<PathBuf> =
            parse_target_path(&call.function.name, &call.function.arguments)
                .into_iter()
                .collect();
        let mut findings = self.policy.evaluate(result, &changed, ctx.workspace_root);
        // F1：bash 结果文本启发式——输出文本含命中 deny glob 的路径痕迹
        // （文本行本身匹配 deny 模式，如 `> .env` 回显或 `cp ... x.pem`
        // 的输出）→ Warning 级 finding。命令可能只是读取/回显，故不升级
        // Blocking；真正写回由 before 的写入路径提取兜底拒绝。
        if call.function.name == "bash" {
            for line in result.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let mut candidates = vec![PathBuf::from(trimmed)];
                // 行内也可能是整条命令（bash -c 回显），再跑一次命令提取。
                candidates.extend(
                    extract_shell_write_paths(trimmed)
                        .into_iter()
                        .map(PathBuf::from),
                );
                candidates.dedup();
                if let Some(rule_id) = candidates
                    .iter()
                    .find_map(|p| self.policy.denied_path(p.as_path()))
                {
                    findings.push(QualityFinding {
                        rule: rule_id.to_string(),
                        severity: FindingSeverity::Warning,
                        passed: false,
                        evidence: format!("bash output mentions denied path: {trimmed}"),
                    });
                    break; // 每行至多一条，避免刷屏
                }
            }
        }
        findings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::MockProvider;
    use crate::Agent;
    use deepseeknova_core::tool::{Tool, ToolContext};
    use deepseeknova_core::types::ToolSchema;
    use deepseeknova_core::{Message, Role, RunEvent, RunInput, Runner};
    use deepseeknova_provider::{Provider, ValidatedRequest};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio_stream::StreamExt;

    /// 记录执行次数的假写工具（read_only=false，名字可指定）。
    struct RecordingTool {
        name: &'static str,
        result: String,
        calls: Arc<Mutex<usize>>,
    }

    #[async_trait::async_trait]
    impl Tool for RecordingTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: self.name.to_string(),
                description: "recording write tool".to_string(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            }
        }
        fn read_only(&self) -> bool {
            false
        }
        async fn execute(&self, _ctx: &ToolContext, _args: &str) -> anyhow::Result<String> {
            *self.calls.lock().unwrap() += 1;
            Ok(self.result.clone())
        }
    }

    /// 审查 provider mock：`generate` 计数并返回 verdict JSON。
    struct CountingReviewProvider {
        calls: Arc<AtomicUsize>,
        verdict: String,
    }

    #[async_trait::async_trait]
    impl Provider for CountingReviewProvider {
        async fn generate(&self, _v: ValidatedRequest<'_>) -> anyhow::Result<Message> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Message {
                role: Role::Assistant,
                content: self.verdict.clone(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            })
        }
        async fn stream(
            &self,
            _v: ValidatedRequest<'_>,
        ) -> anyhow::Result<deepseeknova_core::chunk::ChunkStream> {
            anyhow::bail!("CountingReviewProvider is generate-only (review path)")
        }
    }

    /// 若被调用即 panic 的审查 provider：断言 review 未触发。
    struct BombReviewProvider;

    #[async_trait::async_trait]
    impl Provider for BombReviewProvider {
        async fn generate(&self, _v: ValidatedRequest<'_>) -> anyhow::Result<Message> {
            panic!("review must not be triggered without a blocking finding")
        }
        async fn stream(
            &self,
            _v: ValidatedRequest<'_>,
        ) -> anyhow::Result<deepseeknova_core::chunk::ChunkStream> {
            anyhow::bail!("BombReviewProvider is generate-only")
        }
    }

    async fn drain(agent: Agent, prompt: &str) -> Vec<RunEvent> {
        let mut stream = agent
            .run_stream(RunInput {
                prompt: prompt.into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            events.push(ev.unwrap());
        }
        events
    }

    /// 带未暂存改动的临时 git 仓库（`git diff HEAD` 非空），供 review 触发。
    fn temp_git_repo(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dnv-quality-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "// v1\n").unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .expect("git must run in tests");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "test"]);
        git(&["add", "."]);
        git(&["commit", "-qm", "initial"]);
        std::fs::write(dir.join("src/lib.rs"), "// v2 modified\n").unwrap();
        dir
    }

    fn quality_agent(provider: MockProvider, workspace: std::path::PathBuf) -> Agent {
        Agent::new(Arc::new(provider), 5)
            .with_workspace_root(workspace)
            .with_tool_hook(Arc::new(QualityHook::new(QualityPolicy::builtin())))
    }

    #[tokio::test]
    async fn quality_hook_denies_forbidden_path_before_execution() {
        let workspace = std::env::temp_dir().join(format!(
            "dnv-quality-deny-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let calls = Arc::new(Mutex::new(0usize));
        let provider = MockProvider::tool_call(
            "write_file",
            r#"{"path":".env","content":"SECRET=1"}"#,
            "ignored",
            "done",
        );
        let mut agent = quality_agent(provider, workspace.clone());
        agent.register_tool(Arc::new(RecordingTool {
            name: "write_file",
            result: "written".into(),
            calls: calls.clone(),
        }));

        let events = drain(agent, "write .env").await;
        // before Deny → 工具未执行。
        assert_eq!(*calls.lock().unwrap(), 0, "write tool must NOT execute");
        // 拒绝原因进入 ToolResult。
        let blocked = events.iter().find_map(|e| match e {
            RunEvent::ToolResult { result, .. } if result.contains("blocked by quality policy") => {
                Some(result.clone())
            }
            _ => None,
        });
        assert!(
            blocked.is_some(),
            "expected blocked ToolResult, events: {events:?}"
        );
        assert!(
            blocked.unwrap().contains("no-forbidden-path"),
            "deny must cite rule id"
        );
        assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[tokio::test]
    async fn quality_hook_after_emits_warning_finding_on_oversized_result() {
        let workspace = std::env::temp_dir().join(format!(
            "dnv-quality-oversize-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let calls = Arc::new(Mutex::new(0usize));
        let provider = MockProvider::tool_call("bash", r#"{"command":"cat big"}"#, "", "done");
        let mut agent = quality_agent(provider, workspace.clone());
        agent.register_tool(Arc::new(RecordingTool {
            name: "bash",
            result: "x".repeat(1024 * 1024 + 1),
            calls: calls.clone(),
        }));

        let events = drain(agent, "run big output").await;
        assert_eq!(*calls.lock().unwrap(), 1, "bash tool must execute");
        let finding = events.iter().find_map(|e| match e {
            RunEvent::QualityFinding(f) => Some(f.clone()),
            _ => None,
        });
        let finding = finding.expect("expected QualityFinding event");
        assert_eq!(finding.rule, "oversized-write");
        assert_eq!(
            finding.severity,
            deepseeknova_core::tool_hook::FindingSeverity::Warning
        );
        assert!(!finding.passed);
        assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[tokio::test]
    async fn quality_blocking_finding_sets_flag_and_triggers_review() {
        // bash 输出含私钥 → Blocking finding → 置位 → 进入 review。
        let workspace = temp_git_repo("secret");
        let calls = Arc::new(Mutex::new(0usize));
        let reviewer_calls = Arc::new(AtomicUsize::new(0));
        let provider = MockProvider::tool_call("bash", r#"{"command":"cat key"}"#, "", "all done");
        let mut agent = quality_agent(provider, workspace.clone());
        agent.register_tool(Arc::new(RecordingTool {
            name: "bash",
            result: "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAK...\n".into(),
            calls: calls.clone(),
        }));
        agent = agent.with_review(
            Arc::new(CountingReviewProvider {
                calls: reviewer_calls.clone(),
                verdict: r#"{"verdict":"approve"}"#.into(),
            }),
            4000,
            2,
        );

        let events = drain(agent, "print the key").await;
        assert_eq!(*calls.lock().unwrap(), 1);
        // Blocking finding 进事件流。
        let blocking = events.iter().find_map(|e| match e {
            RunEvent::QualityFinding(f) if f.rule == "no-commit-secret" => Some(f.clone()),
            _ => None,
        });
        let blocking = blocking.expect("expected no-commit-secret finding");
        assert_eq!(
            blocking.severity,
            deepseeknova_core::tool_hook::FindingSeverity::Blocking
        );
        // blocking 置位 → review 进入（provider 恰好被调用一次）→ approve → Done。
        assert_eq!(
            reviewer_calls.load(Ordering::SeqCst),
            1,
            "review must run exactly once on blocking finding"
        );
        assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[tokio::test]
    async fn quality_review_short_circuits_without_blocking_finding() {
        // 普通写（无任何 finding）→ review 短路，审查 provider 绝不触发。
        let workspace = temp_git_repo("noblock");
        let calls = Arc::new(Mutex::new(0usize));
        let provider = MockProvider::tool_call(
            "write_file",
            r#"{"path":"src/main.rs","content":"fn main() {}"}"#,
            "ignored",
            "done",
        );
        let mut agent = quality_agent(provider, workspace.clone());
        agent.register_tool(Arc::new(RecordingTool {
            name: "write_file",
            result: "written 12 bytes".into(),
            calls: calls.clone(),
        }));
        agent = agent.with_review(Arc::new(BombReviewProvider), 4000, 2);

        let events = drain(agent, "write main.rs").await;
        assert_eq!(*calls.lock().unwrap(), 1, "write tool must execute");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, RunEvent::QualityFinding(_))),
            "no findings expected for benign write"
        );
        // 无 Blocking finding → review 被短路 → BombProvider 未被调用 → 正常 Done。
        assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[tokio::test]
    async fn review_triggers_without_tool_hooks_when_files_written() {
        // 回归：质量系统缺席（未注册任何 tool_hook）时 quality_blocked 永远为
        // false，短路门 `wrote_files && (tool_hooks.is_empty() || quality_blocked)`
        // 必须仍让 B3 完成前自审触发（此前被注入 hook 的测试掩盖）。
        let workspace = temp_git_repo("nohook");
        let calls = Arc::new(Mutex::new(0usize));
        let reviewer_calls = Arc::new(AtomicUsize::new(0));
        let provider = MockProvider::tool_call(
            "write_file",
            r#"{"path":"src/main.rs","content":"fn main() {}"}"#,
            "ignored",
            "done",
        );
        // 刻意不调用 with_tool_hook：无任何 hook 注册。
        let mut agent = Agent::new(Arc::new(provider), 5)
            .with_workspace_root(workspace.clone())
            .with_review(
                Arc::new(CountingReviewProvider {
                    calls: reviewer_calls.clone(),
                    verdict: r#"{"verdict":"approve"}"#.into(),
                }),
                4000,
                2,
            );
        agent.register_tool(Arc::new(RecordingTool {
            name: "write_file",
            result: "written 12 bytes".into(),
            calls: calls.clone(),
        }));

        let events = drain(agent, "write main.rs").await;
        assert_eq!(*calls.lock().unwrap(), 1, "write tool must execute");
        assert_eq!(
            reviewer_calls.load(Ordering::SeqCst),
            1,
            "review must run exactly once without tool hooks on file write"
        );
        assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[tokio::test]
    async fn quality_hook_panic_fails_closed_on_before() {
        // F3 契约变更：before panic → Deny（安全判定 fail-closed，工具不执行）；
        // after panic → 无 findings、不崩溃（fail-open 不影响执行）。
        let workspace = std::env::temp_dir().join(format!(
            "dnv-quality-panic-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&workspace).unwrap();

        struct PanicHook;
        impl ToolHook for PanicHook {
            fn name(&self) -> &str {
                "panic"
            }
            fn before(&self, _ctx: &ToolHookCtx, _call: &ToolCall) -> HookVerdict {
                panic!("before panic")
            }
            fn after(
                &self,
                _ctx: &ToolHookCtx,
                _call: &ToolCall,
                _result: &str,
            ) -> Vec<QualityFinding> {
                panic!("after panic")
            }
        }

        let calls = Arc::new(Mutex::new(0usize));
        let provider = MockProvider::tool_call(
            "write_file",
            r#"{"path":"src/main.rs","content":"fn main() {}"}"#,
            "ignored",
            "done",
        );
        let mut agent = Agent::new(Arc::new(provider), 5)
            .with_workspace_root(workspace.clone())
            .with_tool_hook(Arc::new(PanicHook));
        agent.register_tool(Arc::new(RecordingTool {
            name: "write_file",
            result: "written".into(),
            calls: calls.clone(),
        }));

        let events = drain(agent, "write main.rs").await;
        // fail-closed: before panic → 工具被拒绝执行。
        assert_eq!(
            *calls.lock().unwrap(),
            0,
            "fail-closed: tool must NOT execute after before panic"
        );
        // 拒绝原因进入 ToolResult（fail-closed deny 注明 panic 来源）。
        assert!(events.iter().any(|e| match e {
            RunEvent::ToolResult { result, .. } => {
                result.contains("fail-closed deny") || result.contains("panicked")
            }
            _ => false,
        }));
        // after 未执行（工具被拒）→ 无 findings。
        assert!(!events
            .iter()
            .any(|e| matches!(e, RunEvent::QualityFinding(_))));
        assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
        let _ = std::fs::remove_dir_all(&workspace);
    }

    /// after panic 时工具已执行成功：结果正常返回、无 finding、不崩溃（fail-open）。
    #[tokio::test]
    async fn quality_hook_after_panic_fails_open() {
        let workspace = std::env::temp_dir().join(format!(
            "dnv-quality-afterpanic-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&workspace).unwrap();

        struct AfterPanicHook;
        impl ToolHook for AfterPanicHook {
            fn name(&self) -> &str {
                "after-panic"
            }
            fn after(
                &self,
                _ctx: &ToolHookCtx,
                _call: &ToolCall,
                _result: &str,
            ) -> Vec<QualityFinding> {
                panic!("after panic")
            }
        }

        let calls = Arc::new(Mutex::new(0usize));
        let provider = MockProvider::tool_call(
            "write_file",
            r#"{"path":"src/main.rs","content":"fn main() {}"}"#,
            "ignored",
            "done",
        );
        let mut agent = Agent::new(Arc::new(provider), 5)
            .with_workspace_root(workspace.clone())
            .with_tool_hook(Arc::new(AfterPanicHook));
        agent.register_tool(Arc::new(RecordingTool {
            name: "write_file",
            result: "written".into(),
            calls: calls.clone(),
        }));

        let events = drain(agent, "write main.rs").await;
        assert_eq!(*calls.lock().unwrap(), 1, "tool must execute");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, RunEvent::QualityFinding(_))),
            "fail-open: no findings from panicking after"
        );
        assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn parse_target_path_extracts_writer_paths() {
        assert_eq!(
            parse_target_path("write_file", r#"{"path":"a/b.txt","content":"x"}"#),
            Some(PathBuf::from("a/b.txt"))
        );
        assert_eq!(
            parse_target_path("edit_file", r#"{"path":"c.rs","snippet_id":"s1"}"#),
            Some(PathBuf::from("c.rs"))
        );
        assert_eq!(
            parse_target_path("move_file", r#"{"source":"a","destination":"b"}"#),
            Some(PathBuf::from("b"))
        );
        assert_eq!(parse_target_path("bash", r#"{"command":"ls"}"#), None);
        assert_eq!(parse_target_path("write_file", "not json"), None);
    }

    #[test]
    fn interested_only_for_write_tools() {
        let hook = QualityHook::new(QualityPolicy::builtin());
        let call = |name: &str| ToolCall {
            id: "c1".into(),
            ty: "function".into(),
            function: deepseeknova_core::types::FunctionCall {
                name: name.into(),
                arguments: "{}".into(),
            },
        };
        for name in ["write_file", "edit_file", "move_file", "bash"] {
            assert!(hook.interested(&call(name)), "{name} must be interesting");
        }
        for name in ["read_file", "grep", "web_fetch"] {
            assert!(
                !hook.interested(&call(name)),
                "{name} must NOT be interesting"
            );
        }
    }

    #[test]
    fn hook_verdict_denies_forbidden_path_directly() {
        let hook = QualityHook::new(QualityPolicy::builtin());
        let ctx = ToolHookCtx {
            workspace_root: std::path::Path::new("/workspace"),
        };
        let call = ToolCall {
            id: "c1".into(),
            ty: "function".into(),
            function: deepseeknova_core::types::FunctionCall {
                name: "write_file".into(),
                arguments: r#"{"path":"secrets/id_rsa","content":"x"}"#.into(),
            },
        };
        match hook.before(&ctx, &call) {
            HookVerdict::Deny(reason) => {
                assert!(reason.contains("no-forbidden-path"), "reason: {reason}")
            }
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // F1：bash 写路径启发式（before Deny + after Warning）
    // -----------------------------------------------------------------------

    fn bash_call(command: &str) -> ToolCall {
        ToolCall {
            id: "b1".into(),
            ty: "function".into(),
            function: deepseeknova_core::types::FunctionCall {
                name: "bash".into(),
                arguments: serde_json::json!({ "command": command }).to_string(),
            },
        }
    }

    #[test]
    fn bash_before_denies_redirect_write_target() {
        let hook = QualityHook::new(QualityPolicy::builtin());
        let ctx = ToolHookCtx {
            workspace_root: std::path::Path::new("/workspace"),
        };
        // 重定向写 .env → Deny。
        match hook.before(&ctx, &bash_call("echo SECRET=1 > .env")) {
            HookVerdict::Deny(reason) => {
                assert!(reason.contains("no-forbidden-path"), "reason: {reason}");
                assert!(reason.contains(".env"), "reason must cite target: {reason}");
            }
            other => panic!("expected Deny, got {other:?}"),
        }
        // cp 到 .pem → Deny。
        match hook.before(&ctx, &bash_call("cp ~/.ssh/id_rsa x.pem")) {
            HookVerdict::Deny(reason) => {
                assert!(reason.contains("no-forbidden-path"), "reason: {reason}");
                assert!(
                    reason.contains("x.pem"),
                    "reason must cite target: {reason}"
                );
            }
            other => panic!("expected Deny, got {other:?}"),
        }
        // 纯读取命令 → Allow。
        assert_eq!(hook.before(&ctx, &bash_call("ls -la")), HookVerdict::Allow);
    }

    #[tokio::test]
    async fn quality_hook_denies_bash_write_before_execution() {
        let workspace = std::env::temp_dir().join(format!(
            "dnv-quality-bashdeny-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let calls = Arc::new(Mutex::new(0usize));
        let provider = MockProvider::tool_call(
            "bash",
            r#"{"command":"echo SECRET=1 > .env"}"#,
            "ignored",
            "done",
        );
        let mut agent = quality_agent(provider, workspace.clone());
        agent.register_tool(Arc::new(RecordingTool {
            name: "bash",
            result: "written".into(),
            calls: calls.clone(),
        }));

        let events = drain(agent, "write .env via bash").await;
        assert_eq!(*calls.lock().unwrap(), 0, "bash tool must NOT execute");
        let blocked = events.iter().find_map(|e| match e {
            RunEvent::ToolResult { result, .. } if result.contains("blocked by quality policy") => {
                Some(result.clone())
            }
            _ => None,
        });
        assert!(
            blocked.is_some(),
            "expected blocked ToolResult, events: {events:?}"
        );
        assert!(blocked.unwrap().contains("no-forbidden-path"));
        assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn bash_after_emits_warning_on_denied_path_trace() {
        let hook = QualityHook::new(QualityPolicy::builtin());
        let ctx = ToolHookCtx {
            workspace_root: std::path::Path::new("/workspace"),
        };
        // 输出含 `> .env` 痕迹 → Warning（不阻断）。
        let findings = hook.after(&ctx, &bash_call("echo x"), "wrote: echo x > .env\n");
        let hit = findings
            .iter()
            .find(|f| f.rule == "no-forbidden-path")
            .expect("must find no-forbidden-path warning");
        assert_eq!(
            hit.severity,
            deepseeknova_core::tool_hook::FindingSeverity::Warning,
            "trace heuristic must be Warning, not Blocking"
        );
        // 纯文本输出无痕迹 → 无 finding。
        assert!(hook
            .after(&ctx, &bash_call("ls"), "src/main.rs\n")
            .is_empty());
    }
}

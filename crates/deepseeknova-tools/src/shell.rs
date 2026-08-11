use async_trait::async_trait;
use deepseeknova_core::{DeepseeknovaError, Tool, ToolContext, ToolSchema};
use deepseeknova_sandbox::{NoOpSandbox, Sandbox};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

const DEFAULT_SHELL_TIMEOUT: Duration = Duration::from_secs(120);

/// ShellTool executes arbitrary shell commands, optionally inside a sandbox.
///
/// By default it uses `NoOpSandbox`. Pass `Arc<dyn Sandbox>` to the constructor
/// to enable platform-specific isolation (macOS seatbelt or Linux bubblewrap).
///
/// ## 沙箱降级边界（T3）
///
/// 用户显式请求禁网（`allow_network=false`）或只读档、而后端无法强制（如
/// Windows JobSandbox、NoOpSandbox）时，命令仍可联网 / 写任意路径。ShellTool
/// 只持有 `Arc<dyn Sandbox>`，无法得知用户策略中的显式请求，故**不在此处
/// 拒绝执行**（会误伤仅需进程树隔离的合法用法）；它改为在执行路径发出
/// target=`sandbox_degradation` 的结构化告警（[`Sandbox::enforced_network`]
/// 能力位为 `false` 且 [`Sandbox::requires_isolation`] 为真时），使降级可被
/// 程序检测。精确的 fail-closed 决策在 runtime 装配点完成（启动时
/// `sandbox_enforcement_gaps` 检测并告警）。
pub struct ShellTool {
    sandbox: Arc<dyn Sandbox>,
}

impl Default for ShellTool {
    fn default() -> Self {
        Self {
            sandbox: Arc::new(NoOpSandbox),
        }
    }
}

impl ShellTool {
    /// Create a new ShellTool with the given sandbox.
    pub fn new(sandbox: Arc<dyn Sandbox>) -> Self {
        Self { sandbox }
    }
}

#[derive(Deserialize)]
struct ShellArgs {
    command: String,
}

#[async_trait]
impl Tool for ShellTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "bash".to_string(),
            description: "Runs a shell command with read-only classification and sandbox isolation. Dangerous commands (injection, UNC/URL paths, git -c/--config-env) are rejected. Read-only commands may skip approval; write commands and shell combinations (chain/redirect/substitution) require permission.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Shell command. Read-only commands (e.g. `git status`, `ls`) may skip approval; write commands require permission."
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(
        &self,
        ctx: &ToolContext,
        args: &str,
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
        deepseeknova_security::context::enforce_capability(
            ctx,
            &self.schema().name,
            deepseeknova_security::capability::Capability::CommandExecute,
        )?;
        let parsed: ShellArgs = serde_json::from_str(args)?;

        // 只读分类器：注入/危险命令（UNC/URL 路径形态、git 全局
        // `-c`/`--config-env` 配置注入、git 格式串注入等）在执行前直接拒绝；
        // 普通链式/重定向/命令替换归 NotReadOnly，由权限门/安全策略裁决。
        if deepseeknova_security::readonly::classify_readonly(&parsed.command)
            == deepseeknova_security::readonly::ReadOnlyKind::Dangerous
        {
            return Err(deepseeknova_core::DeepseeknovaError::tool(format!(
                "Security violation: command rejected by read-only classifier: {}",
                parsed.command
            )));
        }

        let sec = ctx
            .extensions
            .get::<deepseeknova_security::context::SecurityContext>();

        if let Some(sec) = sec {
            if !sec.policy.is_command_allowed(&parsed.command) {
                return Err(deepseeknova_core::DeepseeknovaError::tool(format!(
                    "Security violation: command '{}' is blocked by security policy",
                    parsed.command
                )));
            }
        }

        // Resource limits from SecurityContext (fallback to defaults when absent).
        let exec_timeout = sec
            .map(|s| s.limits.max_execution_time)
            .unwrap_or(DEFAULT_SHELL_TIMEOUT);
        let max_output = sec.map(|s| s.limits.max_output_bytes as usize);

        if ctx.cancellation.is_cancelled() {
            return Err(deepseeknova_core::DeepseeknovaError::Cancelled);
        }

        let shell = platform_shell();
        let cmd_args: Vec<String> = vec![shell.1.to_string(), parsed.command.clone()];

        // Fail-closed：必须隔离的平台沙箱后端缺失时拒绝执行，绝不静默降级。
        if self.sandbox.requires_isolation() && !self.sandbox.backend_available() {
            return Err(deepseeknova_core::DeepseeknovaError::tool(format!(
                "sandbox backend '{}' unavailable (sandbox-exec/bwrap not found); \
                 refusing to run command without isolation. Install the backend or \
                 set [sandbox] enabled=false.",
                self.sandbox.name()
            )));
        }

        // T3 降级可检测告警：必须隔离的后端却无法强制网络限制（如 Windows
        // JobSandbox / NoOp）时，命令仍可联网。这里打一条可订阅的结构化告警
        // （target=sandbox_degradation），不拒绝执行——ShellTool 只持有
        // `Arc<dyn Sandbox>`，无法得知用户是否显式请求禁网；"显式禁网但后端
        // 无法强制"的精确 fail-closed 决策在 runtime 装配点完成
        // （`deepseeknova_runtime::sandbox_enforcement_gaps`）。文件系统写限制
        // 同理：零强制后端由装配点告警，本处只发网络侧告警避免重复刷屏。
        if self.sandbox.requires_isolation() && !self.sandbox.enforced_network() {
            tracing::warn!(
                target: "sandbox_degradation",
                "shell: sandbox backend '{}' cannot enforce network restrictions; \
                 sandboxed commands still have network access",
                self.sandbox.name()
            );
        }

        let (sandbox_bin, sandbox_args) = self.sandbox.sandbox(shell.0, &cmd_args);

        let mut cmd = Command::new(&sandbox_bin);
        cmd.args(&sandbox_args);
        // 固定工作目录为工作区根：防止命令 `cd` 后（或继承进程 CWD 时）
        // 后续文件操作落在工作区外。
        cmd.current_dir(&ctx.workspace_root);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        // Windows JobSandbox 覆盖 spawn：CREATE_SUSPENDED → 挂入 Job →
        // 恢复主线程；其余平台走默认 spawn。
        let child = self.sandbox.spawn(cmd)?;

        let result = timeout(exec_timeout, child.wait_with_output()).await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                if output.status.success() {
                    Ok(cap_output(stdout.to_string(), max_output))
                } else {
                    let code = output.status.code().unwrap_or(-1);
                    let mut msg = format!("command exited with code {code}");
                    if !stdout.is_empty() {
                        msg.push_str(&format!("\nSTDOUT:\n{stdout}"));
                    }
                    if !stderr.is_empty() {
                        msg.push_str(&format!("\nSTDERR:\n{stderr}"));
                    }
                    Err(DeepseeknovaError::tool(cap_output(msg, max_output)))
                }
            }
            Ok(Err(e)) => Err(DeepseeknovaError::tool(format!("command failed: {e}"))),
            Err(_elapsed) => Err(DeepseeknovaError::tool(format!(
                "command timed out after {:?}",
                exec_timeout
            ))),
        }
    }
}

/// Cap a string to `max` bytes (on a char boundary), appending a truncation note.
fn cap_output(s: String, max: Option<usize>) -> String {
    match max {
        Some(m) if s.len() > m => {
            let end = s.floor_char_boundary(m);
            format!("{}... [truncated {} bytes]", &s[..end], s.len() - end)
        }
        _ => s,
    }
}

/// Returns (shell, flag) for the current platform.
fn platform_shell() -> (&'static str, &'static str) {
    if cfg!(target_os = "windows") {
        ("cmd", "/C")
    } else {
        ("sh", "-c")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use deepseeknova_sandbox::Sandbox;
    #[cfg(unix)]
    use deepseeknova_security::context::SecurityContext;

    // --- cap_output ---

    #[test]
    fn cap_output_unchanged_when_no_limit() {
        // 负例：无 SecurityContext 限额时不截断
        let s = "a".repeat(1000);
        assert_eq!(cap_output(s.clone(), None), s);
    }

    #[test]
    fn cap_output_unchanged_when_under_limit() {
        // 负例：未超限时原样返回，不得附加截断标记
        let out = cap_output("short".to_string(), Some(64));
        assert_eq!(out, "short");
        assert!(!out.contains("[truncated"));
    }

    #[test]
    fn cap_output_truncates_and_notes_bytes() {
        let out = cap_output("abcdefghij".to_string(), Some(4));
        assert!(out.starts_with("abcd"));
        assert!(out.contains("[truncated 6 bytes]"));
    }

    #[test]
    fn cap_output_respects_utf8_boundary() {
        // 限额落在多字节字符中间时不得 panic，回退到字符边界
        let s = "中文输出".to_string(); // 每字 3 字节
        let out = cap_output(s, Some(4));
        assert!(out.starts_with('中'));
        assert!(out.contains("[truncated"));
    }

    // --- execute with SecurityContext-driven limits (unix only) ---

    #[cfg(unix)]
    fn ctx_with(sec: SecurityContext) -> ToolContext {
        ToolContext::new("shell-test").with_extension(sec)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_times_out_using_security_limit() {
        let mut sec = SecurityContext::with_safe_defaults();
        sec.limits.max_execution_time = std::time::Duration::from_millis(200);
        let ctx = ctx_with(sec);

        let err = ShellTool::default()
            .execute(&ctx, r#"{"command":"sleep 5"}"#)
            .await
            .expect_err("200ms 限额下 sleep 5 必须超时");
        assert!(err.to_string().contains("timed out"), "got: {err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_refuses_when_required_sandbox_backend_missing() {
        struct MissingBackend;
        impl Sandbox for MissingBackend {
            fn sandbox(&self, exe: &str, args: &[String]) -> (String, Vec<String>) {
                (exe.to_string(), args.to_vec())
            }
            fn name(&self) -> &str {
                "missing-backend"
            }
            fn requires_isolation(&self) -> bool {
                true
            }
            fn backend_available(&self) -> bool {
                false
            }
        }

        let tool = ShellTool::new(Arc::new(MissingBackend));
        let ctx = ctx_with(SecurityContext::with_safe_defaults());
        let err = tool
            .execute(&ctx, r#"{"command":"echo hi"}"#)
            .await
            .expect_err("后端缺失且必须隔离时必须拒绝");
        assert!(
            err.to_string()
                .contains("refusing to run command without isolation"),
            "错误信息应说明拒绝原因: {err}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_runs_but_warns_when_isolation_required_and_network_not_enforced() {
        // T3 边界：必须隔离的后端可用但无法强制网络限制（能力位
        // enforced_network=false，如 Windows JobSandbox）时，ShellTool **不**
        // 拒绝执行——它无法得知用户是否显式禁网，精确 fail-closed 决策在
        // runtime 装配点；本测试守护"不误拒绝"，降级由 target=
        // sandbox_degradation 的结构化告警承载。
        struct NonEnforcing;
        impl Sandbox for NonEnforcing {
            fn sandbox(&self, exe: &str, args: &[String]) -> (String, Vec<String>) {
                (exe.to_string(), args.to_vec())
            }
            fn name(&self) -> &str {
                "non-enforcing"
            }
            fn requires_isolation(&self) -> bool {
                true
            }
        }

        let tool = ShellTool::new(Arc::new(NonEnforcing));
        let ctx = ctx_with(SecurityContext::with_safe_defaults());
        let out = tool
            .execute(&ctx, r#"{"command":"echo hi"}"#)
            .await
            .expect("后端可用且必须隔离时不应拒绝执行（降级由告警承载）");
        assert!(out.contains("hi"), "got: {out}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_does_not_time_out_within_limit() {
        // 负例：限额内的命令正常返回
        let sec = SecurityContext::with_safe_defaults();
        let ctx = ctx_with(sec);

        let out = ShellTool::default()
            .execute(&ctx, r#"{"command":"echo ok"}"#)
            .await
            .expect("echo 应成功");
        assert!(out.contains("ok"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_caps_output_using_security_limit() {
        let mut sec = SecurityContext::with_safe_defaults();
        sec.limits.max_output_bytes = 32;
        let ctx = ctx_with(sec);

        // 产生远超 32 字节的输出（直接使用 dd，避免命令替换形态
        // 引入 shell 组合语义，测试目标只验证输出截断）
        let out = ShellTool::default()
            .execute(&ctx, r#"{"command":"dd if=/dev/zero bs=1 count=500"}"#)
            .await
            .expect("命令应成功");
        assert!(out.contains("[truncated"), "got: {out}");
        assert!(out.len() < 500, "输出应被截断，got len {}", out.len());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_blocks_command_outside_allowlist() {
        // 负例：白名单之外的命令被安全策略拦截
        let mut sec = SecurityContext::with_safe_defaults();
        sec.policy.allowed_commands = vec!["echo".to_string()];
        let ctx = ctx_with(sec);

        let err = ShellTool::default()
            .execute(&ctx, r#"{"command":"printf hi"}"#)
            .await
            .expect_err("非白名单命令必须被拒");
        assert!(
            err.to_string().contains("blocked by security policy"),
            "got: {err}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_allows_command_in_allowlist() {
        let mut sec = SecurityContext::with_safe_defaults();
        sec.policy.allowed_commands = vec!["echo".to_string()];
        let ctx = ctx_with(sec);

        let out = ShellTool::default()
            .execute(&ctx, r#"{"command":"echo hi"}"#)
            .await
            .expect("白名单前缀命令应放行");
        assert!(out.contains("hi"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_requires_security_context() {
        // 负例：缺少 SecurityContext 时能力门禁直接拒绝执行
        let ctx = ToolContext::new("shell-test");
        let err = ShellTool::default()
            .execute(&ctx, r#"{"command":"echo hi"}"#)
            .await
            .expect_err("无安全上下文必须报错");
        assert!(
            err.to_string()
                .contains("SecurityContext extension not found"),
            "got: {err}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_runs_in_workspace_root() {
        // 回归：Command 未设 current_dir 时继承进程 CWD；现在必须固定在
        // 工作区根（防 `cd` 逃逸后文件操作落在工作区外）。
        let dir = std::env::temp_dir().join(format!("dnv-shell-cwd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        let ctx = ToolContext::new("shell-cwd")
            .with_workspace(dir.clone())
            .with_extension(SecurityContext::with_safe_defaults());

        let out = ShellTool::default()
            .execute(&ctx, r#"{"command":"pwd"}"#)
            .await
            .expect("pwd 应成功");
        assert!(
            out.trim().ends_with(dir.to_string_lossy().as_ref()),
            "shell 必须运行在工作区根，got: {out}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

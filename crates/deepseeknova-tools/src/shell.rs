use async_trait::async_trait;
use deepseeknova_core::{Tool, ToolContext, ToolSchema};
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
            description: "Runs a command.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Command."
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        deepseeknova_security::context::enforce_capability(
            ctx,
            deepseeknova_security::capability::Capability::CommandExecute,
        )?;
        let parsed: ShellArgs = serde_json::from_str(args)?;

        let sec = ctx
            .extensions
            .get::<deepseeknova_security::context::SecurityContext>();

        if let Some(sec) = sec {
            if !sec.policy.is_command_allowed(&parsed.command) {
                anyhow::bail!(
                    "Security violation: command '{}' is blocked by security policy",
                    parsed.command
                );
            }
        }

        // Resource limits from SecurityContext (fallback to defaults when absent).
        let exec_timeout = sec
            .map(|s| s.limits.max_execution_time)
            .unwrap_or(DEFAULT_SHELL_TIMEOUT);
        let max_output = sec.map(|s| s.limits.max_output_bytes as usize);

        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let shell = platform_shell();
        let cmd_args: Vec<String> = vec![shell.1.to_string(), parsed.command.clone()];

        let (sandbox_bin, sandbox_args) = self.sandbox.sandbox(shell.0, &cmd_args);

        let mut cmd = Command::new(&sandbox_bin);
        cmd.args(&sandbox_args);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let child = cmd.spawn()?;

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
                    Err(anyhow::anyhow!("{}", cap_output(msg, max_output)))
                }
            }
            Ok(Err(e)) => Err(anyhow::anyhow!("command failed: {e}")),
            Err(_elapsed) => Err(anyhow::anyhow!(
                "command timed out after {:?}",
                exec_timeout
            )),
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

        // 产生远超 32 字节的输出
        let out = ShellTool::default()
            .execute(&ctx, r#"{"command":"printf 'a%.0s' $(seq 1 500)"}"#)
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
}

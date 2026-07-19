use async_trait::async_trait;
use deepnova_core::{Tool, ToolContext, ToolSchema};
use deepnova_sandbox::{NoOpSandbox, Sandbox};
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
            description: "Executes a shell command and returns stdout and stderr.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute."
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        deepnova_security::context::enforce_capability(
            ctx,
            deepnova_security::capability::Capability::CommandExecute,
        )?;
        let parsed: ShellArgs = serde_json::from_str(args)?;

        if let Some(sec) = ctx
            .extensions
            .get::<deepnova_security::context::SecurityContext>()
        {
            if !sec.policy.is_command_allowed(&parsed.command) {
                anyhow::bail!(
                    "Security violation: command '{}' is blocked by security policy",
                    parsed.command
                );
            }
        }

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

        let result = timeout(DEFAULT_SHELL_TIMEOUT, child.wait_with_output()).await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                if output.status.success() {
                    Ok(stdout.to_string())
                } else {
                    let code = output.status.code().unwrap_or(-1);
                    let mut msg = format!("command exited with code {code}");
                    if !stdout.is_empty() {
                        msg.push_str(&format!("\nSTDOUT:\n{stdout}"));
                    }
                    if !stderr.is_empty() {
                        msg.push_str(&format!("\nSTDERR:\n{stderr}"));
                    }
                    Err(anyhow::anyhow!("{msg}"))
                }
            }
            Ok(Err(e)) => Err(anyhow::anyhow!("command failed: {e}")),
            Err(_elapsed) => Err(anyhow::anyhow!(
                "command timed out after {:?}",
                DEFAULT_SHELL_TIMEOUT
            )),
        }
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

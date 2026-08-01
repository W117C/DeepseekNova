//! `/mcp` 实时连接探测：短超时按真实 argv 启动 stdio MCP 命令，进程存活视为
//! 已连接（MCP stdio 服务器会阻塞等待 stdin，能撑过超时即说明进程正常）。
//! 直接用 `Command::new(command).args(args)`，避免 shell 重新解析参数。

use async_trait::async_trait;
use deepseeknova_tui::{McpProbe, McpServerInfo, McpStatus};
use std::time::Duration;
use tokio::process::Command;

pub struct CliMcpProbe {
    pub timeout: Duration,
}

impl Default for CliMcpProbe {
    fn default() -> Self {
        Self {
            timeout: Duration::from_millis(1500),
        }
    }
}

#[async_trait]
impl McpProbe for CliMcpProbe {
    async fn probe(&self, servers: &[McpServerInfo]) -> Vec<McpStatus> {
        let mut out = Vec::with_capacity(servers.len());
        for server in servers {
            out.push(self.probe_one(&server.command, &server.args).await);
        }
        out
    }
}

impl CliMcpProbe {
    async fn probe_one(&self, command: &str, args: &[String]) -> McpStatus {
        let mut child = match Command::new(command)
            .args(args)
            // 保持 stdin 打开：模拟 MCP 服务器等待输入，避免假阴性。
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return McpStatus::Disconnected(format!("spawn failed: {e}")),
        };
        match tokio::time::timeout(self.timeout, child.wait()).await {
            Err(_) => {
                // 超时仍存活 = 服务器在等 stdin
                let _ = child.kill().await;
                let _ = child.wait().await;
                McpStatus::Connected
            }
            Ok(Ok(status)) => McpStatus::Disconnected(format!(
                "exit {}",
                status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".to_string())
            )),
            Ok(Err(e)) => McpStatus::Disconnected(format!("wait failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[cfg(unix)]
    async fn probe_marks_long_running_stdio_server_connected() {
        let probe = CliMcpProbe {
            timeout: Duration::from_millis(300),
        };
        assert_eq!(
            probe.probe_one("sleep", &["5".to_string()]).await,
            McpStatus::Connected
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn probe_marks_exited_process_disconnected() {
        let probe = CliMcpProbe {
            timeout: Duration::from_millis(300),
        };
        match probe
            .probe_one("sh", &["-c".to_string(), "exit 3".to_string()])
            .await
        {
            McpStatus::Disconnected(reason) => assert!(reason.contains("exit 3"), "{reason}"),
            other => panic!("expected disconnected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn probe_marks_bad_command_disconnected() {
        let probe = CliMcpProbe {
            timeout: Duration::from_millis(300),
        };
        // 不存在的命令 → spawn 失败或 shell 立即退出，都算未连接
        match probe
            .probe_one("definitely-no-such-command-xyz", &[])
            .await
        {
            McpStatus::Disconnected(_) => {}
            other => panic!("expected disconnected, got {other:?}"),
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn probe_keeps_args_with_spaces_intact() {
        let probe = CliMcpProbe {
            timeout: Duration::from_millis(300),
        };
        // sh -c "printf ok; sleep 5"：整个字符串是单个 argv，不能被 shell 再拆
        match probe
            .probe_one(
                "sh",
                &["-c".to_string(), "printf ok; sleep 5".to_string()],
            )
            .await
        {
            McpStatus::Connected => {}
            other => panic!("expected connected, got {other:?}"),
        }
    }
}

//! 工具生命周期钩子（任务质量闭环 A 阶段：ToolHook 链）。
//!
//! 钩子在工具调用前后被 agent 主循环调用：`before` 返回放行/询问/拒绝
//! 决策，`after` 对工具结果文本做写后策略评估并产出 [`QualityFinding`]。
//! panic 契约（fail-closed 安全判定）：
//! `before`/`interested` panic 按 [`HookVerdict::Deny`] 处理（安全判定
//! fail-closed），`after` panic 按空 findings 处理（fail-open，不阻断执行）。

use crate::types::ToolCall;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 钩子对一次工具调用的放行决策。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookVerdict {
    /// 放行。
    Allow,
    /// 放行并附带说明（本阶段仅记录语义，不改变执行）。
    AllowWith(String),
    /// 需要用户确认；由调用方走 approval 桥（与 permission gate 的 Ask 同路径）。
    Ask(String),
    /// 拒绝执行，附拒绝原因。
    Deny(String),
}

/// 质量 finding 的严重级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FindingSeverity {
    /// 仅记录，不阻断。
    Info,
    /// 警告，不阻断但应关注。
    Warning,
    /// 阻断级：置位会话 blocking 标志（B3 review 短路的触发条件）。
    Blocking,
}

/// 一条质量策略评估结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityFinding {
    /// 命中/评估的规则 id（如 `no-commit-secret`）。
    pub rule: String,
    /// 严重级别。
    pub severity: FindingSeverity,
    /// `false` = 违规（命中规则）；`true` = 仅审计用（本阶段未使用）。
    pub passed: bool,
    /// 命中摘要（正则命中片段 / 违规路径 / 字节数等）。
    pub evidence: String,
}

/// 传给钩子方法的只读上下文（最小化：仅工作区根）。
#[derive(Debug, Clone, Copy)]
pub struct ToolHookCtx<'a> {
    /// 工作区根目录，用于解析相对路径。
    pub workspace_root: &'a Path,
}

/// 工具生命周期钩子。实现须 `Send + Sync`；所有方法为同步调用，
/// 由 agent 主循环在 await 点之间串行执行。
///
/// panic 契约：实现 panic 时调用方以 `catch_unwind` 捕获——`before` 与
/// `interested` 按 [`HookVerdict::Deny`] 处理（安全判定 fail-closed，warn
/// 注明），`after` 按空 findings 处理（fail-open，不阻断执行）。
pub trait ToolHook: Send + Sync {
    /// 钩子名称（日志/诊断用）。
    fn name(&self) -> &str;

    /// 是否对本次调用感兴趣。默认对所有调用感兴趣。
    fn interested(&self, _call: &ToolCall) -> bool {
        true
    }

    /// 工具执行前的预检。默认放行。
    fn before(&self, _ctx: &ToolHookCtx, _call: &ToolCall) -> HookVerdict {
        HookVerdict::Allow
    }

    /// 工具执行成功后的写后评估。默认无 findings。
    fn after(&self, _ctx: &ToolHookCtx, _call: &ToolCall, _result: &str) -> Vec<QualityFinding> {
        Vec::new()
    }
}

/// 空实现：全放行、零 findings。用作默认/测试桩。
pub struct NoopToolHook;

impl ToolHook for NoopToolHook {
    /// 返回固定名 `"noop"`。
    fn name(&self) -> &str {
        "noop"
    }
}

// ---------------------------------------------------------------------------
// 用户级外部 hooks（用户面扩展）
// ---------------------------------------------------------------------------

/// 外部 hook 命令的默认超时。
pub const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(30);

/// 用户 hooks 事件类型。`as_str()` 输出与配置 `[hooks]` 段事件名、
/// JSON 协议（stdin 载荷）的 `event` 字段一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    /// 工具调用前（可阻断：非 0 退出或裁决 `allowed=false` → 阻止执行）。
    ToolBefore,
    /// 工具调用后（工具已执行；失败仅 warn，不阻断）。
    ToolAfter,
    /// 会话启动。
    SessionStart,
    /// 会话结束。
    SessionEnd,
    /// 失败诊断时（run 以非成功终点结束，如 max_steps Paused / 异常返回）。
    Failure,
}

impl HookEvent {
    /// 事件名字符串（JSON 协议 `event` 字段）。
    pub fn as_str(&self) -> &'static str {
        match self {
            HookEvent::ToolBefore => "tool_before",
            HookEvent::ToolAfter => "tool_after",
            HookEvent::SessionStart => "session_start",
            HookEvent::SessionEnd => "session_end",
            HookEvent::Failure => "failure",
        }
    }
}

/// 单条用户 hook 命令的运行时规格（装配层从配置 `[hooks]` 构建；
/// 配置的 `disabled` 开关在装配层过滤，运行时无该字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserHookCommand {
    /// 外部命令（可执行文件路径或 PATH 查找名）。
    pub command: String,
    /// 命令参数。
    pub args: Vec<String>,
    /// 超时；`None` = 使用 [`DEFAULT_HOOK_TIMEOUT`]。
    pub timeout: Option<Duration>,
}

/// 单条外部命令的执行结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookExecResult {
    /// 退出码 0。
    Success,
    /// 非 0 退出（含 stderr 尾部，诊断用）。
    Failed {
        /// 退出码（信号终止时为 `None`）。
        exit_code: Option<i32>,
        /// stderr 尾部。
        stderr: String,
    },
    /// 无法启动 / 超时 / 等待 IO 失败（按拒绝处理，fail-closed）。
    Error {
        /// 失败说明。
        message: String,
    },
}

impl HookExecResult {
    /// fail-closed 放行判定：仅 exit 0 视为放行。
    pub fn is_allowed(&self) -> bool {
        matches!(self, HookExecResult::Success)
    }
}

/// 传给外部命令的结构化上下文（JSON 协议：序列化后写入 stdin）。
#[derive(Debug, Clone, Serialize)]
pub struct HookPayload<'a> {
    /// 事件名（`HookEvent::as_str()`）。
    pub event: &'a str,
    /// 工具名（仅 tool_before / tool_after 携带）。
    pub tool: Option<&'a str>,
    /// 工具参数 JSON 字符串（仅 tool_before / tool_after 携带）。
    pub arguments: Option<&'a str>,
    /// 工作区根目录。
    pub workspace: &'a Path,
    /// 会话 id。
    pub session_id: &'a str,
}

/// 外部命令 stdout 的 JSON 裁决（tool_before 语义；缺省放行）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookVerdictJson {
    /// 是否允许工具执行（缺省 `true`）。
    #[serde(default = "default_hook_allowed")]
    pub allowed: bool,
    /// 拒绝原因（`allowed=false` 时透传给调用方）。
    #[serde(default)]
    pub reason: String,
}

fn default_hook_allowed() -> bool {
    true
}

/// 一条外部命令的执行结果 + 解析出的 JSON 裁决。
#[derive(Debug, Clone)]
pub struct HookRun {
    /// 底层命令结果。
    pub exec: HookExecResult,
    /// stdout 解析出的裁决（解析失败为 `None`）。
    pub verdict: Option<HookVerdictJson>,
}

/// 用户级外部 hooks 集合（事件 → 命令列表）。由装配层挂载到 agent；
/// 全部为空时不 spawn 任何进程（零开销）。
#[derive(Debug, Clone, Default)]
pub struct UserHooks {
    /// 工具调用前预检命令（AND 链，全过才执行）。
    pub tool_before: Vec<UserHookCommand>,
    /// 工具调用后通知命令（失败仅 warn）。
    pub tool_after: Vec<UserHookCommand>,
    /// 会话启动通知命令。
    pub session_start: Vec<UserHookCommand>,
    /// 会话结束通知命令。
    pub session_end: Vec<UserHookCommand>,
    /// 失败诊断通知命令。
    pub failure: Vec<UserHookCommand>,
}

impl UserHooks {
    /// 是否未挂载任何命令（装配层据此跳过挂载，零进程开销）。
    pub fn is_empty(&self) -> bool {
        self.tool_before.is_empty()
            && self.tool_after.is_empty()
            && self.session_start.is_empty()
            && self.session_end.is_empty()
            && self.failure.is_empty()
    }
}

/// 执行单条外部命令：stdin 写入 JSON 载荷，捕获 stdout/stderr，带超时。
/// 超时 / 无法启动 / IO 失败按 [`HookExecResult::Error`] 返回（fail-closed：
/// 调用方一律视为拒绝）。不引入新依赖（`std::process::Command`）。
pub fn run_user_hook(command: &UserHookCommand, payload: &HookPayload) -> HookRun {
    let timeout = command.timeout.unwrap_or(DEFAULT_HOOK_TIMEOUT);
    let mut child = match Command::new(&command.command)
        .args(&command.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return HookRun {
                exec: HookExecResult::Error {
                    message: format!("failed to spawn '{}': {e}", command.command),
                },
                verdict: None,
            };
        }
    };

    // stdin 载荷写入：子线程写并关闭（EOF），避免大载荷或子进程不读 stdin
    // 时阻塞主线程。
    if let Some(mut stdin) = child.stdin.take() {
        let body = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());
        std::thread::spawn(move || {
            let _ = stdin.write_all(body.as_bytes());
        });
    }

    // stdout/stderr 子线程读取，避免子进程写满管道导致死锁。
    let out_thread = child.stdout.take().map(|mut out| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = out.read_to_string(&mut buf);
            buf
        })
    });
    let err_thread = child.stderr.take().map(|mut err| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = err.read_to_string(&mut buf);
            buf
        })
    });

    // 带超时轮询退出状态；超时 kill（视为拒绝）。
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return HookRun {
                    exec: HookExecResult::Error {
                        message: format!("wait failed: {e}"),
                    },
                    verdict: None,
                };
            }
        }
    };

    let stdout = out_thread.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr = err_thread.and_then(|h| h.join().ok()).unwrap_or_default();

    match status {
        Some(status) if status.success() => HookRun {
            exec: HookExecResult::Success,
            verdict: serde_json::from_str(&stdout).ok(),
        },
        Some(status) => HookRun {
            exec: HookExecResult::Failed {
                exit_code: status.code(),
                stderr: truncate_hook_output(&stderr),
            },
            verdict: None,
        },
        None => HookRun {
            exec: HookExecResult::Error {
                message: format!("timed out after {timeout:?}"),
            },
            verdict: None,
        },
    }
}

/// stderr 尾部截断（诊断日志防刷屏）。
fn truncate_hook_output(s: &str) -> String {
    const MAX: usize = 4000;
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let tail: String = s
        .chars()
        .rev()
        .take(MAX)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("[truncated {} chars] {tail}", s.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_verdict_serde_roundtrip() {
        for verdict in [
            HookVerdict::Allow,
            HookVerdict::AllowWith("note".to_string()),
            HookVerdict::Ask("confirm?".to_string()),
            HookVerdict::Deny("blocked".to_string()),
        ] {
            let json = serde_json::to_string(&verdict).unwrap();
            let back: HookVerdict = serde_json::from_str(&json).unwrap();
            assert_eq!(back, verdict);
        }
    }

    #[test]
    fn quality_finding_serde_roundtrip() {
        let f = QualityFinding {
            rule: "no-commit-secret".to_string(),
            severity: FindingSeverity::Blocking,
            passed: false,
            evidence: "-----BEGIN RSA PRIVATE KEY-----".to_string(),
        };
        let json = serde_json::to_string(&f).unwrap();
        let back: QualityFinding = serde_json::from_str(&json).unwrap();
        assert_eq!(back, f);
        assert!(json.contains("\"severity\":\"Blocking\""));
    }

    #[test]
    fn finding_severity_serde_roundtrip() {
        for s in [
            FindingSeverity::Info,
            FindingSeverity::Warning,
            FindingSeverity::Blocking,
        ] {
            let json = serde_json::to_string(&s).unwrap();
            let back: FindingSeverity = serde_json::from_str(&json).unwrap();
            assert_eq!(back, s);
        }
    }

    #[test]
    fn noop_hook_defaults_allow_and_empty_findings() {
        let hook = NoopToolHook;
        assert_eq!(hook.name(), "noop");
        let call = ToolCall {
            id: "call_1".into(),
            ty: "function".into(),
            function: crate::types::FunctionCall {
                name: "write_file".into(),
                arguments: "{}".into(),
            },
        };
        let ctx = ToolHookCtx {
            workspace_root: std::path::Path::new("/tmp"),
        };
        // 默认实现：interested = true、before = Allow、after = 空 findings。
        assert!(hook.interested(&call));
        assert_eq!(hook.before(&ctx, &call), HookVerdict::Allow);
        assert!(hook.after(&ctx, &call, "ok").is_empty());
    }

    // ── 用户级外部 hooks ──

    fn hook_payload<'a>(event: &'a str, tool: Option<&'a str>) -> HookPayload<'a> {
        HookPayload {
            event,
            tool,
            arguments: tool.map(|_| "{\"path\":\"/tmp/x\"}"),
            workspace: std::path::Path::new("/ws"),
            session_id: "sess-1",
        }
    }

    #[test]
    fn hook_event_serde_and_as_str() {
        for e in [
            HookEvent::ToolBefore,
            HookEvent::ToolAfter,
            HookEvent::SessionStart,
            HookEvent::SessionEnd,
            HookEvent::Failure,
        ] {
            let json = serde_json::to_string(&e).unwrap();
            let back: HookEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(back, e);
        }
        assert_eq!(HookEvent::ToolBefore.as_str(), "tool_before");
        assert_eq!(HookEvent::ToolAfter.as_str(), "tool_after");
        assert_eq!(HookEvent::SessionStart.as_str(), "session_start");
        assert_eq!(HookEvent::SessionEnd.as_str(), "session_end");
        assert_eq!(HookEvent::Failure.as_str(), "failure");
    }

    #[test]
    fn run_user_hook_success_exit_zero() {
        let cmd = UserHookCommand {
            command: "true".into(),
            args: vec![],
            timeout: Some(Duration::from_secs(5)),
        };
        let run = run_user_hook(&cmd, &hook_payload("tool_before", Some("read_file")));
        assert!(run.exec.is_allowed(), "exit 0 → Success");
        assert!(run.verdict.is_none(), "非 JSON 输出 → verdict None");
    }

    #[test]
    fn run_user_hook_nonzero_exit_fails_closed() {
        let cmd = UserHookCommand {
            command: "false".into(),
            args: vec![],
            timeout: Some(Duration::from_secs(5)),
        };
        let run = run_user_hook(&cmd, &hook_payload("tool_before", Some("bash")));
        match run.exec {
            HookExecResult::Failed { exit_code, .. } => assert_eq!(exit_code, Some(1)),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(!run.exec.is_allowed(), "非 0 退出必须视为拒绝");
    }

    #[test]
    fn run_user_hook_parses_json_verdict() {
        let script = "cat >/dev/null; echo '{\"allowed\":false,\"reason\":\"blocked by gate\"}'";
        let cmd = UserHookCommand {
            command: "sh".into(),
            args: vec!["-c".into(), script.into()],
            timeout: Some(Duration::from_secs(5)),
        };
        let run = run_user_hook(&cmd, &hook_payload("tool_before", Some("bash")));
        assert!(
            run.exec.is_allowed(),
            "exit 0 但裁决 allowed=false 由调用方判定"
        );
        let v = run.verdict.expect("JSON 裁决应被解析");
        assert!(!v.allowed);
        assert_eq!(v.reason, "blocked by gate");
    }

    #[test]
    fn run_user_hook_timeout_is_error() {
        let cmd = UserHookCommand {
            command: "sleep".into(),
            args: vec!["5".into()],
            timeout: Some(Duration::from_millis(150)),
        };
        let start = Instant::now();
        let run = run_user_hook(&cmd, &hook_payload("tool_before", None));
        assert!(
            matches!(run.exec, HookExecResult::Error { .. }),
            "超时必须视为 Error（fail-closed），got {:?}",
            run.exec
        );
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "超时后必须提前返回，而非等 sleep 结束"
        );
    }

    #[test]
    fn run_user_hook_missing_command_is_error() {
        let cmd = UserHookCommand {
            command: "definitely-not-a-real-command-xyz".into(),
            args: vec![],
            timeout: None,
        };
        let run = run_user_hook(&cmd, &hook_payload("failure", None));
        assert!(
            matches!(run.exec, HookExecResult::Error { .. }),
            "spawn 失败必须视为 Error（fail-closed）"
        );
        assert!(!run.exec.is_allowed());
    }

    #[test]
    fn run_user_hook_stdin_receives_payload() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("hook-in.json");
        let script = format!("cat > '{}'", out.display());
        let cmd = UserHookCommand {
            command: "sh".into(),
            args: vec!["-c".into(), script],
            timeout: Some(Duration::from_secs(5)),
        };
        let run = run_user_hook(&cmd, &hook_payload("tool_before", Some("write_file")));
        assert!(run.exec.is_allowed());
        let written = std::fs::read_to_string(&out).unwrap();
        let v: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(v["event"], "tool_before");
        assert_eq!(v["tool"], "write_file");
        assert_eq!(v["workspace"], "/ws");
        assert_eq!(v["session_id"], "sess-1");
    }

    #[test]
    fn user_hooks_is_empty_default() {
        assert!(UserHooks::default().is_empty());
        let mut h = UserHooks::default();
        h.tool_before.push(UserHookCommand {
            command: "true".into(),
            args: vec![],
            timeout: None,
        });
        assert!(!h.is_empty());
    }

    #[test]
    fn hook_verdict_json_allowed_defaults_true() {
        let v: HookVerdictJson = serde_json::from_str("{}").unwrap();
        assert!(v.allowed, "缺省 allowed=true（未拒绝即放行）");
        let v: HookVerdictJson =
            serde_json::from_str(r#"{"allowed":false,"reason":"no"}"#).unwrap();
        assert!(!v.allowed);
        assert_eq!(v.reason, "no");
    }
}

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
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
    /// 确定性验证门单条命令结果（passed 见 detail JSON）。
    Verification,
    /// 一次 run 正常完成（Done 终点；Paused/Failure 不触发本事件）。
    RunDone,
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
            HookEvent::Verification => "verification",
            HookEvent::RunDone => "run_done",
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
    /// 事件细节 JSON（verification/run_done 携带；其余事件 None → 序列化
    /// 省略该字段，不向外部命令泄露 schema 噪音）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<&'a str>,
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
    /// 确定性验证门结果通知命令（每条验证命令触发一次；失败仅 warn）。
    pub verification: Vec<UserHookCommand>,
    /// run 正常完成通知命令（Done 终点；失败仅 warn）。
    pub run_done: Vec<UserHookCommand>,
}

impl UserHooks {
    /// 是否未挂载任何命令（装配层据此跳过挂载，零进程开销）。
    pub fn is_empty(&self) -> bool {
        self.tool_before.is_empty()
            && self.tool_after.is_empty()
            && self.session_start.is_empty()
            && self.session_end.is_empty()
            && self.failure.is_empty()
            && self.verification.is_empty()
            && self.run_done.is_empty()
    }
}

/// 执行单条外部命令：stdin 写入 JSON 载荷，捕获 stdout/stderr，带超时。
/// 超时 / 无法启动 / IO 失败按 [`HookExecResult::Error`] 返回（fail-closed：
/// 调用方一律视为拒绝）。
///
/// **同步阻塞版本**：`std::process::Command` + 轮询等待，阻塞当前线程直至
/// 命令退出或超时（最长 [`DEFAULT_HOOK_TIMEOUT`]）。供无 async 上下文或需要
/// 阻塞完成语义的路径使用（如会话结束 / failure 通知经 `Drop` 触发时）。
/// 阻塞期间不占用 tokio worker 的调用方应优先使用异步版本
/// [`run_user_hook`]，或把本函数放入 `tokio::task::spawn_blocking` /
/// `block_in_place`。
pub fn run_user_hook_sync(command: &UserHookCommand, payload: &HookPayload) -> HookRun {
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

/// 执行单条外部命令：stdin 写入 JSON 载荷，异步捕获 stdout/stderr，带超时。
/// 超时 / 无法启动 / IO 失败按 [`HookExecResult::Error`] 返回（fail-closed：
/// 调用方一律视为拒绝）。
///
/// **异步实现**（[`tokio::process::Command`]）：等待子进程期间让出当前
/// tokio worker，不占用运行时线程（T19）。stdin 载荷写入在独立任务中完成，
/// 子进程不读 stdin 时仅该任务滞留（不 join、不阻塞本函数）；stdout/stderr
/// 异步读取，子进程退出即 EOF。
pub async fn run_user_hook(command: &UserHookCommand, payload: &HookPayload<'_>) -> HookRun {
    let timeout = command.timeout.unwrap_or(DEFAULT_HOOK_TIMEOUT);
    let mut child = match tokio::process::Command::new(&command.command)
        .args(&command.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
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

    let body = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());

    // stdout/stderr：异步读取（子进程退出→EOF），不占 worker。
    let mut stdout = child.stdout.take();
    let out_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut out) = stdout.take() {
            let _ = out.read_to_end(&mut buf).await;
        }
        String::from_utf8_lossy(&buf).into_owned()
    });
    let mut stderr = child.stderr.take();
    let err_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut err) = stderr.take() {
            let _ = err.read_to_end(&mut buf).await;
        }
        String::from_utf8_lossy(&buf).into_owned()
    });

    // stdin 载荷写入：独立任务写并关闭（EOF）。子进程不读 stdin 时该任务
    // 滞留（不阻塞本函数）；子进程退出/被杀后管道断开即结束。
    let mut stdin = child.stdin.take();
    tokio::spawn(async move {
        if let Some(mut s) = stdin.take() {
            let _ = s.write_all(body.as_bytes()).await;
            let _ = s.shutdown().await;
        }
    });

    // 带超时等待退出；超时或 wait 失败先 kill 并回收（fail-closed 视为
    // Error）。注意：必须在读取 stdout/stderr **之前** kill——否则子进程
    // 继续运行导致管道不 EOF，后续读取会阻塞到子进程自然退出。
    let status = tokio::select! {
        status = child.wait() => Some(status),
        _ = tokio::time::sleep(timeout) => None,
    };
    let needs_kill = status.is_none() || matches!(status, Some(Err(_)));
    if needs_kill {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    let stdout = out_task.await.unwrap_or_default();
    let stderr = err_task.await.unwrap_or_default();

    match status {
        Some(Ok(status)) if status.success() => HookRun {
            exec: HookExecResult::Success,
            verdict: serde_json::from_str(&stdout).ok(),
        },
        Some(Ok(status)) => HookRun {
            exec: HookExecResult::Failed {
                exit_code: status.code(),
                stderr: truncate_hook_output(&stderr),
            },
            verdict: None,
        },
        Some(Err(e)) => HookRun {
            exec: HookExecResult::Error {
                message: format!("wait failed: {e}"),
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
            detail: None,
        }
    }

    #[test]
    fn hook_payload_detail_serializes_into_stdin_json() {
        // detail 字段:verification/run_done 事件细节 JSON 原样透传;None 时字段缺席。
        let with = hook_payload(HookEvent::Verification.as_str(), None);
        let payload_with = HookPayload {
            detail: Some("{\"passed\":true}"),
            ..with
        };
        let s = serde_json::to_string(&payload_with).unwrap();
        assert!(s.contains("\"detail\""), "got: {s}");
        assert!(s.contains("passed"), "got: {s}");
        let without = HookPayload {
            detail: None,
            ..hook_payload(HookEvent::ToolBefore.as_str(), Some("read_file"))
        };
        let s2 = serde_json::to_string(&without).unwrap();
        assert!(!s2.contains("detail"), "None 时应省略字段: {s2}");
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
        assert_eq!(HookEvent::Verification.as_str(), "verification");
        assert_eq!(HookEvent::RunDone.as_str(), "run_done");
    }

    #[tokio::test]
    async fn run_user_hook_success_exit_zero() {
        let cmd = UserHookCommand {
            command: "true".into(),
            args: vec![],
            timeout: Some(Duration::from_secs(5)),
        };
        let run = run_user_hook(&cmd, &hook_payload("tool_before", Some("read_file"))).await;
        assert!(run.exec.is_allowed(), "exit 0 → Success");
        assert!(run.verdict.is_none(), "非 JSON 输出 → verdict None");
    }

    #[tokio::test]
    async fn run_user_hook_nonzero_exit_fails_closed() {
        let cmd = UserHookCommand {
            command: "false".into(),
            args: vec![],
            timeout: Some(Duration::from_secs(5)),
        };
        let run = run_user_hook(&cmd, &hook_payload("tool_before", Some("bash"))).await;
        match run.exec {
            HookExecResult::Failed { exit_code, .. } => assert_eq!(exit_code, Some(1)),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(!run.exec.is_allowed(), "非 0 退出必须视为拒绝");
    }

    #[tokio::test]
    async fn run_user_hook_parses_json_verdict() {
        let script = "cat >/dev/null; echo '{\"allowed\":false,\"reason\":\"blocked by gate\"}'";
        let cmd = UserHookCommand {
            command: "sh".into(),
            args: vec!["-c".into(), script.into()],
            timeout: Some(Duration::from_secs(5)),
        };
        let run = run_user_hook(&cmd, &hook_payload("tool_before", Some("bash"))).await;
        assert!(
            run.exec.is_allowed(),
            "exit 0 但裁决 allowed=false 由调用方判定"
        );
        let v = run.verdict.expect("JSON 裁决应被解析");
        assert!(!v.allowed);
        assert_eq!(v.reason, "blocked by gate");
    }

    #[tokio::test]
    async fn run_user_hook_timeout_is_error() {
        let cmd = UserHookCommand {
            command: "sleep".into(),
            args: vec!["5".into()],
            timeout: Some(Duration::from_millis(150)),
        };
        let start = Instant::now();
        let run = run_user_hook(&cmd, &hook_payload("tool_before", None)).await;
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

    #[tokio::test]
    async fn run_user_hook_missing_command_is_error() {
        let cmd = UserHookCommand {
            command: "definitely-not-a-real-command-xyz".into(),
            args: vec![],
            timeout: None,
        };
        let run = run_user_hook(&cmd, &hook_payload("failure", None)).await;
        assert!(
            matches!(run.exec, HookExecResult::Error { .. }),
            "spawn 失败必须视为 Error（fail-closed）"
        );
        assert!(!run.exec.is_allowed());
    }

    #[tokio::test]
    async fn run_user_hook_stdin_receives_payload() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("hook-in.json");
        let script = format!("cat > '{}'", out.display());
        let cmd = UserHookCommand {
            command: "sh".into(),
            args: vec!["-c".into(), script],
            timeout: Some(Duration::from_secs(5)),
        };
        let run = run_user_hook(&cmd, &hook_payload("tool_before", Some("write_file"))).await;
        assert!(run.exec.is_allowed());
        let written = std::fs::read_to_string(&out).unwrap();
        let v: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(v["event"], "tool_before");
        assert_eq!(v["tool"], "write_file");
        assert_eq!(v["workspace"], "/ws");
        assert_eq!(v["session_id"], "sess-1");
    }

    /// T19 回归：钩子等待子进程期间不占用 tokio worker。若 `run_user_hook`
    /// 同步阻塞（旧实现 20ms 轮询 + sleep），单 worker runtime 下并发定时
    /// 器只能在钩子返回后才有机会推进，从而晚于钩子完成；异步实现下两者
    /// 并发推进，定时器先于钩子返回触发。
    #[tokio::test(flavor = "current_thread")]
    async fn run_user_hook_does_not_block_worker() {
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done_task = std::sync::Arc::clone(&done);
        let cmd = UserHookCommand {
            command: "sleep".into(),
            args: vec!["1".into()],
            timeout: Some(Duration::from_secs(5)),
        };
        let hook = tokio::spawn(async move {
            let run = run_user_hook(&cmd, &hook_payload("tool_before", None)).await;
            done_task.store(true, std::sync::atomic::Ordering::SeqCst);
            run
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !done.load(std::sync::atomic::Ordering::SeqCst),
            "200ms 时钩子仍在等待子进程（sleep 1s）：worker 不应被同步占用"
        );
        let run = hook.await.unwrap();
        assert!(run.exec.is_allowed(), "sleep 1 正常退出 → Success");
    }

    /// 同步版本（通知型路径：session / failure / tool_after）必须保持
    /// fail-closed 语义：非 0 退出 / 超时 / 无法启动 → 拒绝。
    #[test]
    fn run_user_hook_sync_preserves_fail_closed_semantics() {
        let cmd = UserHookCommand {
            command: "false".into(),
            args: vec![],
            timeout: Some(Duration::from_secs(5)),
        };
        let run = run_user_hook_sync(&cmd, &hook_payload("tool_before", Some("bash")));
        match run.exec {
            HookExecResult::Failed { exit_code, .. } => assert_eq!(exit_code, Some(1)),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(!run.exec.is_allowed());

        let timeout_cmd = UserHookCommand {
            command: "sleep".into(),
            args: vec!["5".into()],
            timeout: Some(Duration::from_millis(150)),
        };
        let start = Instant::now();
        let run = run_user_hook_sync(&timeout_cmd, &hook_payload("tool_before", None));
        assert!(
            matches!(run.exec, HookExecResult::Error { .. }),
            "同步版本超时也必须视为 Error（fail-closed）"
        );
        assert!(start.elapsed() < Duration::from_secs(2), "超时后应提前返回");
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

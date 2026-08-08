//! 诊断/对抗审查触发辅助：敏感性判定与错误信号检测。
//!
//! 从 `agent.rs` 拆分（M7）：本模块保持纯搬移，不改行为/签名/逻辑。
//!
//! 注：计划名 `diagnose.rs` 被现有公开模块 `crate::diagnose`（DiagnoseGuard /
//! DiagnoseHook / DiagnoseReport）占用，故更名 `agent_diag` 以避开同名冲突。

use deepseeknova_core::tool_hook::{FindingSeverity, QualityFinding};
use deepseeknova_core::{Message, Role};

/// 触发条件判定（纯函数，供测试）：(a) 会话 QualityFinding 存在 Blocking 级；
/// (b) 工具调用命中 security/sandbox/permission 相关路径（工具名或参数）。
pub fn adversarial_review_needed(findings: &[QualityFinding], messages: &[Message]) -> bool {
    if findings
        .iter()
        .any(|f| f.severity == FindingSeverity::Blocking)
    {
        return true;
    }
    messages
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .filter_map(|m| m.tool_calls.as_ref())
        .flatten()
        .any(|tc| tool_call_touches_sensitive_path(&tc.function.name, &tc.function.arguments))
}

/// 敏感工具/参数启发式（Bugbot #9 收紧）：删除/移动类工具无条件敏感
/// （本身高风险）；bash/shell 命令执行类必须同时命中命令内容 marker 才敏感；
/// write/edit 写类必须命中目标路径/内容 marker 才敏感（避免任意 bash 调用、
/// 任意文件写都烧子代理 token）；其余工具参数命中安全边界关键词仍判敏感
/// （如 read_file 读 /etc/passwd）。
pub(crate) fn tool_call_touches_sensitive_path(name: &str, args: &str) -> bool {
    /// 无条件敏感：删除/移动本身高风险。
    const UNCONDITIONALLY_SENSITIVE: [&str; 2] = ["delete_file", "move_file"];
    /// 需叠加 marker 才敏感的工具。
    const MARKER_GATED_TOOLS: [&str; 4] = ["bash", "shell", "write_file", "edit_file"];
    // marker 小写匹配（安全审查 S2）：args 与 marker 都 to_lowercase 后匹配，
    // 防 `Sudo -n`、`Chmod 777`、`/Etc/Passwd` 等大小写变体绕过。
    // 补充常见敏感路径变体：~/.ssh、authorized_keys、crontab、/private/
    // （macOS 上 /etc 为符号链接，真实路径是 /private/etc）、passwd/shadow。
    const SENSITIVE_MARKERS: [&str; 12] = [
        "security",
        "sandbox",
        "permission",
        "sudo",
        "chmod",
        "chown",
        "/etc/",
        "/private/",
        "~/.ssh",
        "authorized_keys",
        "crontab",
        "passwd",
    ];
    if UNCONDITIONALLY_SENSITIVE.contains(&name) {
        return true;
    }
    let lowered = args.to_lowercase();
    let hit = |m: &str| lowered.contains(m);
    if MARKER_GATED_TOOLS.contains(&name) {
        return SENSITIVE_MARKERS.iter().any(|m| hit(m));
    }
    SENSITIVE_MARKERS.iter().any(|m| hit(m))
}

/// 结果文本是否含错误指示（宽松 contains 语义，供机械续步分类，与
/// `is_tool_error_result` 的整体判定互补）：大小写不敏感的 `error:` 片段、
/// JSON `"error"` 键出现、以及 `{"success": false}` 显式 false 值。
/// 宁可多判错误（→ high，更强模型），不漏判错误走 quick。
pub(crate) fn contains_error_signal(text: &str) -> bool {
    if text.to_ascii_lowercase().contains("error:") {
        return true;
    }
    // JSON `{"error": ...}` 形态：`"error"` 键（后随 `:`）出现即视为错误指示
    // （宽松判定，null/false 值也判错；与 is_tool_error_result 的首字段
    // null/false 特判互补）。字符串值位置的 `"error"`（后随 `}`/`,`）不判。
    let mut rest = text;
    while let Some(idx) = rest.find("\"error\"") {
        let after = rest[idx + "\"error\"".len()..].trim_start();
        if after.starts_with(':') {
            return true;
        }
        rest = after;
    }
    // `{"success": false, ...}`：`"success"` 键后紧跟 `: false`。
    let mut rest = text;
    while let Some(idx) = rest.find("\"success\"") {
        let after = &rest[idx + "\"success\"".len()..];
        let after = after.trim_start();
        if let Some(after) = after.strip_prefix(':') {
            return after.trim_start().starts_with("false");
        }
        rest = after;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_core::types::{FunctionCall, ToolCall};

    #[test]
    fn adversarial_review_needed_blocks_on_blocking_finding() {
        let findings = vec![QualityFinding {
            rule: "no-commit-secret".into(),
            severity: FindingSeverity::Blocking,
            passed: false,
            evidence: "AKIA...".into(),
        }];
        assert!(adversarial_review_needed(&findings, &[]));
    }

    #[test]
    fn adversarial_review_needed_ignores_non_blocking_findings() {
        let findings = vec![QualityFinding {
            rule: "oversized-write".into(),
            severity: FindingSeverity::Warning,
            passed: false,
            evidence: "1024 bytes".into(),
        }];
        assert!(!adversarial_review_needed(&findings, &[]));
    }

    #[test]
    fn adversarial_review_needed_triggers_on_sensitive_tool_call() {
        let msg = Message {
            role: Role::Assistant,
            content: String::new(),
            name: None,
            tool_calls: Some(vec![ToolCall {
                id: "t1".into(),
                ty: "function".into(),
                function: FunctionCall {
                    name: "bash".into(),
                    arguments: r#"{"command":"chmod 777 /etc/hosts"}"#.into(),
                },
            }]),
            tool_call_id: None,
            reasoning_content: None,
        };
        assert!(adversarial_review_needed(&[], &[msg]));
    }

    #[test]
    fn adversarial_review_needed_skips_benign_session() {
        let msg = Message {
            role: Role::Assistant,
            content: String::new(),
            name: None,
            tool_calls: Some(vec![ToolCall {
                id: "t1".into(),
                ty: "function".into(),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"src/main.rs"}"#.into(),
                },
            }]),
            tool_call_id: None,
            reasoning_content: None,
        };
        assert!(!adversarial_review_needed(&[], &[msg]));
    }

    /// Bugbot #9 负例：bash/shell 类无 SENSITIVE_MARKERS 命中 → 不触发
    /// （任意 bash 调用不得烧子代理 token）；write_file 写普通路径 → 不
    /// 触发、写安全边界路径（/etc/）→ 触发；delete_file 保持无条件敏感。
    #[test]
    fn adversarial_review_needed_marker_gating_on_bash_and_write() {
        let call = |name: &str, args: &str| Message {
            role: Role::Assistant,
            content: String::new(),
            name: None,
            tool_calls: Some(vec![ToolCall {
                id: "t1".into(),
                ty: "function".into(),
                function: FunctionCall {
                    name: name.into(),
                    arguments: args.into(),
                },
            }]),
            tool_call_id: None,
            reasoning_content: None,
        };
        // bash 无 marker → 不触发。
        assert!(!adversarial_review_needed(
            &[],
            &[call("bash", r#"{"command":"ls -la"}"#)]
        ));
        // bash 命中 marker（chmod /etc/）→ 触发。
        assert!(adversarial_review_needed(
            &[],
            &[call("bash", r#"{"command":"chmod 777 /etc/hosts"}"#)]
        ));
        // write_file 普通源码路径 → 不触发。
        assert!(!adversarial_review_needed(
            &[],
            &[call(
                "write_file",
                r#"{"path":"src/main.rs","content":"fn main() {}"}"#
            )]
        ));
        // write_file 命中路径 marker（/etc/）→ 触发。
        assert!(adversarial_review_needed(
            &[],
            &[call(
                "write_file",
                r#"{"path":"/etc/hosts","content":"127.0.0.1 x"}"#
            )]
        ));
        // delete_file 无条件敏感（不依赖 marker）。
        assert!(adversarial_review_needed(
            &[],
            &[call("delete_file", r#"{"path":"src/main.rs"}"#)]
        ));
    }
}

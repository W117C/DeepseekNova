//! 分类/统计辅助：步级 effort 分类、历史工具活动判定、并发分段、run 唯一标注。
//!
//! 从 `agent.rs` 拆分（M7）：本模块保持纯搬移，不改行为/签名/逻辑。

use crate::agent::PendingToolCall;
use crate::agent_diag::contains_error_signal;
use deepseeknova_core::{Message, Role};

/// P2.1 每步分类：上一条消息是工具结果且不含错误信号 → 机械续步（quick）；
/// 其余（首步、出错、回炉反馈）→ high。错误识别与 `is_tool_error_result`
/// 语义一致（大小写不敏感 `error:` + 错误 JSON 形态），但保持 contains
/// 语义（长输出中任何位置出现即算错误信号）。
/// A1：入参改为消息序列快照（步内复用同一快照，不再各自克隆内存）。
pub(crate) fn classify_quick_step(messages: &[Message]) -> bool {
    match messages.last() {
        Some(m) if m.role == Role::Tool => !contains_error_signal(&m.content),
        _ => false,
    }
}

/// 上一轮是否有工具活动：从历史末尾向前扫，遇到 Tool 消息 → true；
/// 遇到 User 边界 → false（说明上一轮没有工具调用）。
pub(crate) fn history_last_turn_used_tools(messages: &[Message]) -> bool {
    for m in messages.iter().rev() {
        match m.role {
            Role::Tool => return true,
            Role::User => return false,
            _ => continue,
        }
    }
    false
}

/// 将允许执行的下标分组：连续只读调用并入并发段；写类调用独占一段，保序。
/// 未知工具按写（保守）处理，避免并发读写竞争。
pub(crate) fn group_call_indices(
    calls: &[PendingToolCall],
    allowed: &[usize],
    is_read: impl Fn(&str) -> bool,
) -> Vec<Vec<usize>> {
    let mut segments: Vec<Vec<usize>> = Vec::new();
    let mut reads: Vec<usize> = Vec::new();
    for &i in allowed {
        if is_read(&calls[i].name) {
            reads.push(i);
        } else {
            if !reads.is_empty() {
                segments.push(std::mem::take(&mut reads));
            }
            segments.push(vec![i]);
        }
    }
    if !reads.is_empty() {
        segments.push(reads);
    }
    segments
}

/// 生成 run 级唯一会话标注（`session-<epoch毫秒>-<进程内序号>`，仅含
/// `[A-Za-z0-9_-]`，serve 路径白名单安全）。serve 多会话共享同一 Agent
/// 且未显式标注时，每次 run 必须拿到独立 id，否则 Paused 事件的
/// `session_id` 与诊断报告文件名会互相覆盖。
pub(crate) fn unique_run_label() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!(
        "session-{ms}-{}",
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Memory;

    #[test]
    fn classify_quick_step_flags_error_signals_case_insensitive() {
        // 错误指示（含小写 error、JSON 形态）→ 非 quick（high）。
        for content in [
            "Error: boom",
            "error: boom",
            "lots of text then Error: boom",
            r#"{"error": "boom"}"#,
            r#"{"error":null}"#,
            r#"{"success": false, "detail": "x"}"#,
            "prefix\n{\"error\": 1}\nsuffix",
        ] {
            let mut mem = Memory::new();
            mem.add_message(Message {
                role: Role::Tool,
                content: content.to_string(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                reasoning_signature: None,
                usage: None,
            });
            assert!(
                !classify_quick_step(&mem.get_all()),
                "含错误指示应判 high（非 quick）: {content}"
            );
        }
    }

    #[test]
    fn classify_quick_step_normal_output_stays_quick() {
        // 正常工具输出 → quick（机械续步）。
        for content in [
            "all good",
            "42 lines read",
            r#"{"success": true, "data": 1}"#,
            r#"{"status": "error"}"#, // 非 error/success 键名不判错
            "errorless result",
        ] {
            let mut mem = Memory::new();
            mem.add_message(Message {
                role: Role::Tool,
                content: content.to_string(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                reasoning_signature: None,
                usage: None,
            });
            assert!(
                classify_quick_step(&mem.get_all()),
                "正常输出应判 quick: {content}"
            );
        }
    }

    #[test]
    fn classify_quick_step_non_tool_last_message_is_not_quick() {
        let mut mem = Memory::new();
        mem.add_message(Message {
            role: Role::User,
            content: "Error: boom".to_string(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
            usage: None,
        });
        assert!(!classify_quick_step(&mem.get_all()), "非工具消息不判 quick");
        assert!(
            !classify_quick_step(&Memory::new().get_all()),
            "空记忆不判 quick"
        );
    }

    #[test]
    fn group_call_indices_segments_reads_and_writes_in_order() {
        let calls = vec![
            PendingToolCall {
                id: "a".into(),
                name: "read_file".into(),
                arguments: String::new(),
            },
            PendingToolCall {
                id: "b".into(),
                name: "grep".into(),
                arguments: String::new(),
            },
            PendingToolCall {
                id: "c".into(),
                name: "write_file".into(),
                arguments: String::new(),
            },
            PendingToolCall {
                id: "d".into(),
                name: "read_file".into(),
                arguments: String::new(),
            },
        ];
        let allowed: Vec<usize> = (0..calls.len()).collect();
        let segs = group_call_indices(&calls, &allowed, |n| n != "write_file");
        assert_eq!(segs, vec![vec![0, 1], vec![2], vec![3]]);

        // 全读（或并发关闭）→ 单段，保持原始顺序。
        let segs = group_call_indices(&calls, &allowed, |_| true);
        assert_eq!(segs, vec![vec![0, 1, 2, 3]]);

        // 被权限拦截的下标不参与分段。
        let segs = group_call_indices(&calls, &[1, 3], |n| n != "write_file");
        assert_eq!(segs, vec![vec![1, 3]]);
    }

    #[test]
    fn unique_run_label_is_unique_and_serve_safe() {
        let a = unique_run_label();
        let b = unique_run_label();
        assert_ne!(a, b, "每次 run 必须拿到唯一标注");
        for label in [&a, &b] {
            assert!(label.starts_with("session-"), "unexpected label: {label}");
            assert!(
                label
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "label must only contain [A-Za-z0-9_-]: {label}"
            );
        }
    }
}

//! RunEvent → 消息树的单一增量入口。
//!
//! 核心不变量：推理增量只追加 `pending_reasoning`，直到正文或工具调用开始时
//! 才整段提交为 `Segment::Reasoning`。任何时刻 pending_reasoning 与已落段不
//! 交错，从根上消除"推理被工具调用从中间拆断"的乱序问题。

use deepseeknova_core::runner::RunEvent;

use super::conversation::{
    AssistantTurn, Conversation, Segment, SystemKind, ToolStatus, TurnStatus,
};

/// 工具参数单行预览上限（字符）。
const ARGS_PREVIEW: usize = 200;
/// 工具结果单行预览上限（字符）。
const RESULT_PREVIEW: usize = 400;

/// 把 agent 的 Paused reason 转成对用户可读的中文说明（保留原始原因信息）。
///
/// 已知两种 reason 形态来自 `deepseeknova-agent`：
/// - `reached max steps (N)`：步数上限，属"任务做了一半被保护性暂停"
/// - `budget: <why>`：预算上限
///
/// 其余形态原样保留，不猜测。文案必须包含"任务未完成"的因果提示，让用户知道
/// 这不是 bug，而是保护机制，且是可继续的（/resume）。
pub fn friendly_pause_reason(reason: &str) -> String {
    let trimmed = reason.trim();
    if let Some(n) = trimmed.strip_prefix("reached max steps (") {
        if let Some(digits) = n.strip_suffix(')') {
            if digits.chars().all(|c| c.is_ascii_digit()) {
                return format!("已达步骤上限（{digits}），任务未完成");
            }
        }
    }
    if let Some(why) = trimmed.strip_prefix("budget:") {
        return format!("已达预算上限：{}", why.trim());
    }
    if trimmed == "max steps" {
        return "已达步骤上限，任务未完成".to_string();
    }
    format!("任务暂停：{trimmed}")
}

/// 消息树的单一变更入口。`ConversationApply` 是 [Conversation] 的行为扩展。
pub trait ConversationApply {
    /// 增量消费一个 RunEvent，更新当前回合的消息树。
    fn apply(&mut self, ev: RunEvent);
}

impl ConversationApply for Conversation {
    fn apply(&mut self, ev: RunEvent) {
        match ev {
            RunEvent::TextDelta(text) => {
                let Some(turn) = self.current_mut() else {
                    return;
                };
                // 正文增量整段累积：先落段之前到达的推理（保持 r→t 段序），
                // 正文本身留在 pending_text，等推理/工具/回合结束时再整段
                // 提交。绝不按 delta 逐段落——流式响应逐 token 到达时，
                // 那样会把一段正文切成几十个独立段（渲染竖排）。
                turn.assistant.flush_reasoning();
                turn.assistant.text_delta_seen = true;
                turn.assistant.pending_text.push_str(&text);
            }
            RunEvent::ReasoningDelta { text, .. } => {
                if let Some(turn) = self.current_mut() {
                    // 推理增量整段累积：先落段之前到达的正文（保持 t→r 段序）。
                    turn.assistant.flush_text();
                    turn.assistant.pending_reasoning.push_str(&text);
                }
            }
            RunEvent::ToolCallStart { id, name } => {
                let Some(turn) = self.current_mut() else {
                    return;
                };
                turn.assistant.flush_text();
                turn.assistant.flush_reasoning();
                turn.assistant.segments.push(Segment::ToolCall {
                    call_id: id,
                    name,
                    arguments: String::new(),
                    result: None,
                    status: ToolStatus::Running,
                });
            }
            RunEvent::ToolCallDelta { id, args_delta } => {
                let Some(turn) = self.current_mut() else {
                    return;
                };
                if let Some(Segment::ToolCall { arguments, .. }) = turn
                    .assistant
                    .segments
                    .iter_mut()
                    .rev()
                    .find(|s| matches!(s, Segment::ToolCall { call_id, .. } if *call_id == id))
                {
                    arguments.push_str(&args_delta);
                }
            }
            RunEvent::ToolCallEnd { id, arguments, .. } => {
                let Some(turn) = self.current_mut() else {
                    return;
                };
                if let Some(Segment::ToolCall {
                    arguments: args, ..
                }) = turn
                    .assistant
                    .segments
                    .iter_mut()
                    .rev()
                    .find(|s| matches!(s, Segment::ToolCall { call_id, .. } if *call_id == id))
                {
                    *args = truncate_str(&arguments, ARGS_PREVIEW);
                }
            }
            RunEvent::ToolResult { call_id, result } => {
                let Some(turn) = self.current_mut() else {
                    return;
                };
                if let Some(Segment::ToolCall {
                    result: r, status, ..
                }) =
                    turn.assistant.segments.iter_mut().rev().find(
                        |s| matches!(s, Segment::ToolCall { call_id: c, .. } if *c == call_id),
                    )
                {
                    *r = Some(truncate_str(&result, RESULT_PREVIEW));
                    *status = ToolStatus::Ok;
                }
            }
            RunEvent::Verification {
                command,
                passed,
                summary,
            } => {
                let Some(turn) = self.current_mut() else {
                    return;
                };
                turn.assistant.flush_text();
                turn.assistant.flush_reasoning();
                turn.assistant.segments.push(Segment::Verification {
                    command,
                    passed,
                    summary,
                });
            }
            RunEvent::Usage(_) => {
                // usage 由 AppState 单独消费（状态行），不落消息树。
            }
            RunEvent::QualityFinding(finding) => {
                // 任务质量闭环：finding 以系统段呈现（与 Verification 同级）。
                let Some(turn) = self.current_mut() else {
                    return;
                };
                turn.assistant.flush_text();
                turn.assistant.flush_reasoning();
                let severity = match finding.severity {
                    deepseeknova_core::tool_hook::FindingSeverity::Blocking => "🔴",
                    deepseeknova_core::tool_hook::FindingSeverity::Warning => "🟡",
                    deepseeknova_core::tool_hook::FindingSeverity::Info => "🔵",
                };
                let passed = if finding.passed { "pass" } else { "fail" };
                turn.assistant.segments.push(Segment::System {
                    kind: SystemKind::Info,
                    text: format!(
                        "🔎 质量检查 [{severity} {passed}] {}: {}",
                        finding.rule, finding.evidence
                    ),
                });
            }
            // 协议增强：阶段迁移 / 门控违规 / drift 以最小可见系统段呈现。
            // 渲染为文本即可（完整协议状态面板属范围外，见设计 §11）。
            RunEvent::PhaseTransition { transition } => {
                let Some(turn) = self.current_mut() else {
                    return;
                };
                turn.assistant.flush_text();
                turn.assistant.flush_reasoning();
                turn.assistant.segments.push(Segment::System {
                    kind: SystemKind::Info,
                    text: format!(
                        "🔁 阶段迁移: {:?} ({:?})",
                        transition.phase, transition.outcome
                    ),
                });
            }
            RunEvent::GateViolation(violation) => {
                let Some(turn) = self.current_mut() else {
                    return;
                };
                turn.assistant.flush_text();
                turn.assistant.flush_reasoning();
                turn.assistant.segments.push(Segment::System {
                    kind: SystemKind::Info,
                    text: format!("🚧 门控违规: {} — {}", violation.gate, violation.detail),
                });
            }
            RunEvent::DriftFinding(drift) => {
                let Some(turn) = self.current_mut() else {
                    return;
                };
                turn.assistant.flush_text();
                turn.assistant.flush_reasoning();
                turn.assistant.segments.push(Segment::System {
                    kind: SystemKind::Info,
                    text: format!(
                        "🧭 drift: {} 连续失败 {} 次 — {}",
                        drift.tool_family, drift.failures, drift.detail
                    ),
                });
            }
            RunEvent::TurnComplete => {
                if let Some(turn) = self.current_mut() {
                    turn.assistant.flush_all();
                }
            }
            RunEvent::ApprovalRequest {
                title, description, ..
            } => {
                let Some(turn) = self.current_mut() else {
                    return;
                };
                turn.assistant.flush_text();
                turn.assistant.flush_reasoning();
                let desc = description
                    .as_deref()
                    .map(|d| truncate_str(d, 120))
                    .unwrap_or_default();
                let text = if desc.is_empty() {
                    format!("🔒 请求授权: {title}")
                } else {
                    format!("🔒 请求授权: {title} — {desc}")
                };
                turn.assistant.segments.push(Segment::System {
                    kind: SystemKind::Approval,
                    text,
                });
            }
            RunEvent::Paused { reason, session_id } => {
                let Some(turn) = self.current_mut() else {
                    return;
                };
                turn.assistant.flush_all();
                turn.assistant.mark_unfinished_tools_failed();
                turn.assistant.segments.push(Segment::System {
                    kind: SystemKind::Paused,
                    text: format!("⏸ {}", friendly_pause_reason(&reason)),
                });
                if let Some(id) = session_id {
                    turn.assistant.segments.push(Segment::System {
                        kind: SystemKind::Info,
                        text: format!("输入 /resume {id} 继续任务，或直接输入新指令"),
                    });
                }
                turn.status = TurnStatus::Paused;
            }
            RunEvent::Done(output) => {
                let Some(turn) = self.current_mut() else {
                    return;
                };
                // `output.text` 是流式全文汇总：已收到过正文 delta 时跳过，
                // 避免整段重复；纯非流式（仅 Done 携带文本）才追加。
                if !output.text.is_empty() && !turn.assistant.text_delta_seen {
                    turn.assistant.flush_reasoning();
                    turn.assistant.pending_text.push_str(&output.text);
                }
                turn.assistant.flush_all();
                turn.assistant.mark_unfinished_tools_failed();
                turn.status = TurnStatus::Done;
            }
        }
        self.enforce_cap();
    }
}

impl AssistantTurn {
    /// 回合结束时把仍处于 Running 的工具调用标记为 Failed
    /// （无结果返回即视为中断/失败，供渲染与侧边栏工具活动统计）。
    pub fn mark_unfinished_tools_failed(&mut self) {
        for seg in &mut self.segments {
            if let Segment::ToolCall { status, .. } = seg {
                if *status == ToolStatus::Running {
                    *status = ToolStatus::Failed;
                }
            }
        }
    }
}

/// 按字符边界截断并追加省略号。
pub fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::conversation::{Conversation, Segment};

    fn new_turn(c: &mut Conversation, prompt: &str) {
        c.begin_turn(prompt.to_string());
    }

    #[test]
    fn reasoning_flushed_as_whole_segment_before_text() {
        let mut c = Conversation::default();
        new_turn(&mut c, "q");
        c.apply(RunEvent::ReasoningDelta {
            text: "think-".into(),
            signature: None,
        });
        c.apply(RunEvent::ReasoningDelta {
            text: "more".into(),
            signature: None,
        });
        c.apply(RunEvent::TextDelta("answer".into()));
        c.apply(RunEvent::Done(crate::model::conversation::done_output("")));

        let segs: Vec<&Segment> = c.current().unwrap().assistant.segments.iter().collect();
        assert_eq!(segs.len(), 2, "推理整段 + 正文整段");
        assert!(matches!(
            segs[0],
            Segment::Reasoning { text } if text == "think-more"
        ));
        assert!(matches!(segs[1], Segment::Text { text } if text == "answer"));
    }

    #[test]
    fn interleaved_reasoning_text_never_crosses() {
        // 核心乱序场景：推理 → 正文 → 推理 → 正文。
        let mut c = Conversation::default();
        new_turn(&mut c, "q");
        c.apply(RunEvent::ReasoningDelta {
            text: "r1".into(),
            signature: None,
        });
        c.apply(RunEvent::TextDelta("t1".into()));
        c.apply(RunEvent::ReasoningDelta {
            text: "r2".into(),
            signature: None,
        });
        c.apply(RunEvent::TextDelta("t2".into()));
        c.apply(RunEvent::Done(crate::model::conversation::done_output("")));

        let segs: Vec<&Segment> = c.current().unwrap().assistant.segments.iter().collect();
        assert_eq!(segs.len(), 4);
        assert!(matches!(segs[0], Segment::Reasoning { text } if text == "r1"));
        assert!(matches!(segs[1], Segment::Text { text } if text == "t1"));
        assert!(matches!(segs[2], Segment::Reasoning { text } if text == "r2"));
        assert!(matches!(segs[3], Segment::Text { text } if text == "t2"));
    }

    #[test]
    fn consecutive_text_deltas_merge_into_one_segment() {
        // 回归：流式逐 token delta 不得把正文切成碎片（曾导致 TUI 竖排）。
        let mut c = Conversation::default();
        new_turn(&mut c, "q");
        for tok in ["你", "好", "！", "我是", "Deep", "seek"] {
            c.apply(RunEvent::TextDelta(tok.into()));
        }
        c.apply(RunEvent::Done(crate::model::conversation::done_output("")));
        let segs: Vec<&Segment> = c.current().unwrap().assistant.segments.iter().collect();
        assert_eq!(segs.len(), 1, "连续正文合成一个 Text 段");
        assert!(matches!(
            &segs[0],
            Segment::Text { text } if text == "你好！我是Deepseek"
        ));
    }

    #[test]
    fn text_then_reasoning_flushes_text_before_reasoning() {
        // t1 → r2 切换：正文先落段，推理段序在正文之后。
        let mut c = Conversation::default();
        new_turn(&mut c, "q");
        c.apply(RunEvent::TextDelta("t1".into()));
        c.apply(RunEvent::ReasoningDelta {
            text: "r2".into(),
            signature: None,
        });
        c.apply(RunEvent::Done(crate::model::conversation::done_output("")));
        let segs: Vec<&Segment> = c.current().unwrap().assistant.segments.iter().collect();
        assert_eq!(segs.len(), 2);
        assert!(matches!(segs[0], Segment::Text { text } if text == "t1"));
        assert!(matches!(segs[1], Segment::Reasoning { text } if text == "r2"));
    }

    #[test]
    fn tool_call_flushes_reasoning_and_carries_result() {
        let mut c = Conversation::default();
        new_turn(&mut c, "q");
        c.apply(RunEvent::ReasoningDelta {
            text: "prep".into(),
            signature: None,
        });
        c.apply(RunEvent::ToolCallStart {
            id: "1".into(),
            name: "grep".into(),
        });
        c.apply(RunEvent::ToolCallDelta {
            id: "1".into(),
            args_delta: "{\"pat".into(),
        });
        c.apply(RunEvent::ToolCallEnd {
            id: "1".into(),
            name: "grep".into(),
            arguments: "{\"pattern\":\"x\"}".into(),
        });
        c.apply(RunEvent::ToolResult {
            call_id: "1".into(),
            result: "hit".into(),
        });
        c.apply(RunEvent::Done(crate::model::conversation::done_output("")));

        let segs: Vec<&Segment> = c.current().unwrap().assistant.segments.iter().collect();
        assert_eq!(segs.len(), 2, "推理先落段，工具调用随后");
        assert!(matches!(segs[0], Segment::Reasoning { text } if text == "prep"));
        match segs[1] {
            Segment::ToolCall {
                call_id,
                name,
                arguments,
                result,
                status,
            } => {
                assert_eq!(call_id, "1");
                assert_eq!(name, "grep");
                assert_eq!(arguments, "{\"pattern\":\"x\"}");
                assert_eq!(result.as_deref(), Some("hit"));
                assert_eq!(*status, ToolStatus::Ok);
            }
            _ => panic!("expected tool call segment"),
        }
    }

    #[test]
    fn done_marks_turn_done_and_flushes() {
        let mut c = Conversation::default();
        new_turn(&mut c, "q");
        c.apply(RunEvent::TextDelta("final ".into()));
        c.apply(RunEvent::Done(crate::model::conversation::done_output(
            "done",
        )));
        let turn = c.current().unwrap();
        assert_eq!(turn.status, TurnStatus::Done);
        assert_eq!(turn.assistant.segments.len(), 1);
        assert!(matches!(
            &turn.assistant.segments[0],
            Segment::Text { text } if text == "final "
        ));
    }

    #[test]
    fn done_does_not_duplicate_streamed_text() {
        // 回归：provider 的 Done.output.text 是流式全文汇总；已流式过
        // 正文时不得再追加（曾导致消息内容翻倍、块高膨胀）。
        let mut c = Conversation::default();
        new_turn(&mut c, "q");
        let full = "你好！我是 DeepseekNova，一个终端原生的软件工程代理。";
        for tok in [
            "你好！",
            "我是 ",
            "DeepseekNova，",
            "一个终端原生的软件工程代理。",
        ] {
            c.apply(RunEvent::TextDelta(tok.into()));
        }
        c.apply(RunEvent::Done(crate::model::conversation::done_output(
            full,
        )));
        let turn = c.current().unwrap();
        assert_eq!(turn.assistant.segments.len(), 1, "只落一个正文段");
        assert!(matches!(
            &turn.assistant.segments[0],
            Segment::Text { text } if text == full
        ));
    }

    #[test]
    fn done_text_applied_when_no_stream() {
        // 纯非流式（无 TextDelta，仅 Done 携带文本）→ 正常追加。
        let mut c = Conversation::default();
        new_turn(&mut c, "q");
        c.apply(RunEvent::Done(crate::model::conversation::done_output(
            "非流式全文",
        )));
        let turn = c.current().unwrap();
        assert!(matches!(
            &turn.assistant.segments[0],
            Segment::Text { text } if text == "非流式全文"
        ));
    }

    #[test]
    fn paused_flushes_and_marks_status() {
        let mut c = Conversation::default();
        new_turn(&mut c, "q");
        c.apply(RunEvent::ReasoningDelta {
            text: "r".into(),
            signature: None,
        });
        c.apply(RunEvent::Paused {
            reason: "reached max steps (10)".into(),
            session_id: Some("abc".into()),
        });
        let turn = c.current().unwrap();
        assert_eq!(turn.status, TurnStatus::Paused);
        assert_eq!(turn.assistant.segments.len(), 3);
        assert!(matches!(
            &turn.assistant.segments[0],
            Segment::Reasoning { text } if text == "r"
        ));
        assert!(matches!(
            &turn.assistant.segments[1],
            Segment::System { kind: SystemKind::Paused, text } if text == "⏸ 已达步骤上限（10），任务未完成"
        ));
        assert!(matches!(
            &turn.assistant.segments[2],
            Segment::System { kind: SystemKind::Info, text } if text == "输入 /resume abc 继续任务，或直接输入新指令"
        ));
    }

    #[test]
    fn friendly_pause_reason_maps_known_and_passes_through() {
        assert_eq!(
            friendly_pause_reason("reached max steps (40)"),
            "已达步骤上限（40），任务未完成"
        );
        assert_eq!(
            friendly_pause_reason("budget: 预算超限"),
            "已达预算上限：预算超限"
        );
        assert_eq!(friendly_pause_reason("unexpected"), "任务暂停：unexpected");
        // 非数字的 max steps 形态不猜测，原样保留。
        assert_eq!(
            friendly_pause_reason("reached max steps (x)"),
            "任务暂停：reached max steps (x)"
        );
    }

    #[test]
    fn approval_request_renders_as_system() {
        let mut c = Conversation::default();
        new_turn(&mut c, "q");
        c.apply(RunEvent::ApprovalRequest {
            id: "a1".into(),
            title: "run shell".into(),
            description: Some("rm -rf".into()),
        });
        let turn = c.current().unwrap();
        assert!(matches!(
            &turn.assistant.segments[0],
            Segment::System { kind: SystemKind::Approval, text } if text == "🔒 请求授权: run shell — rm -rf"
        ));
    }

    #[test]
    fn verification_flushes_reasoning() {
        let mut c = Conversation::default();
        new_turn(&mut c, "q");
        c.apply(RunEvent::ReasoningDelta {
            text: "r".into(),
            signature: None,
        });
        c.apply(RunEvent::Verification {
            command: "cargo check".into(),
            passed: true,
            summary: "ok".into(),
        });
        let segs: Vec<&Segment> = c.current().unwrap().assistant.segments.iter().collect();
        assert_eq!(segs.len(), 2);
        assert!(matches!(segs[0], Segment::Reasoning { .. }));
        assert!(matches!(
            segs[1],
            Segment::Verification { command, passed: true, .. } if command == "cargo check"
        ));
    }

    #[test]
    fn events_without_current_turn_are_ignored() {
        let mut c = Conversation::default();
        c.apply(RunEvent::TextDelta("orphan".into()));
        c.apply(RunEvent::Done(crate::model::conversation::done_output("x")));
        assert_eq!(c.turn_count(), 0);
        assert_eq!(c.segment_count(), 0);
    }

    #[test]
    fn truncate_keeps_utf8_boundary() {
        assert_eq!(truncate_str("你好世界", 4), "你…");
        assert_eq!(truncate_str("hello", 100), "hello");
        let s = "a".repeat(300);
        assert_eq!(truncate_str(&s, 200).len(), 203);
    }

    #[test]
    fn tool_result_truncates_long_output() {
        let mut c = Conversation::default();
        new_turn(&mut c, "q");
        c.apply(RunEvent::ToolCallStart {
            id: "1".into(),
            name: "cat".into(),
        });
        c.apply(RunEvent::ToolResult {
            call_id: "1".into(),
            result: "z".repeat(1000),
        });
        let turn = c.current().unwrap();
        match &turn.assistant.segments[0] {
            Segment::ToolCall { result, status, .. } => {
                assert_eq!(*status, ToolStatus::Ok);
                assert!(result.as_ref().unwrap().ends_with('…'));
                assert!(result.as_ref().unwrap().len() <= RESULT_PREVIEW + 3);
            }
            _ => panic!("expected tool call"),
        }
    }
}

//! 会话消息树：Turn / AssistantTurn / Segment 的纯数据结构。
//!
//! 会话内容（用户输入、助手正文、推理、工具调用、验证、系统事件）是会话的
//! 唯一真相源，全部由 [`Conversation::apply`](crate::model::apply::ConversationApply)
//! 增量构建。折叠状态与渲染层不内嵌于此（见 `app::state` 与 `render`）。

use deepseeknova_core::runner::RunOutput;

/// 会话树的段数上限（对原 2000 行回看上限的语义等价迁移：按段而非按行截断）。
pub const MAX_SEGMENTS: usize = 2000;

/// UI 行类型：渲染样式映射与显示模式过滤的依据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    User,
    Agent,
    Reasoning,
    Tool,
    ToolResult,
    Verification { passed: bool },
    System,
    Error,
    Paused,
}

/// 稳定消息 id：(turn_id, 段序)。折叠、导航、复制均引用它。
pub type SegId = (u64, usize);

/// 一个回合（一次提交的完整生成）的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStatus {
    Running,
    Done,
    Cancelled,
    Paused,
}

/// 工具调用的执行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Ok,
    Failed,
}

/// 系统事件的具体类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemKind {
    Paused,
    Approval,
    Info,
    Error,
}

/// 助手侧的一段消息。
#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    /// 推理：流式增量整段提交，默认折叠。
    Reasoning { text: String },
    /// 助手正文：流式增量，整段提交。
    Text { text: String },
    /// 工具调用：含参数预览与结果预览（均已截断）。
    ToolCall {
        call_id: String,
        name: String,
        arguments: String,
        result: Option<String>,
        status: ToolStatus,
    },
    /// 确定性验证结果。
    Verification {
        command: String,
        passed: bool,
        summary: String,
    },
    /// 系统事件（暂停/授权请求/信息/错误），文本已预格式化。
    System { kind: SystemKind, text: String },
}

impl Segment {
    /// 该段的渲染字符数（折叠摘要头 `N 字符` 用）。
    pub fn char_len(&self) -> usize {
        match self {
            Segment::Reasoning { text } => text.chars().count(),
            Segment::Text { text } => text.chars().count(),
            Segment::ToolCall {
                arguments, result, ..
            } => {
                arguments.chars().count() + result.as_ref().map(|r| r.chars().count()).unwrap_or(0)
            }
            Segment::Verification {
                command, summary, ..
            } => command.chars().count() + summary.chars().count(),
            Segment::System { text, .. } => text.chars().count(),
        }
    }

    /// 段 → UI 行类型（渲染样式映射）。
    pub fn line_kind(&self) -> LineKind {
        match self {
            Segment::Reasoning { .. } => LineKind::Reasoning,
            Segment::Text { .. } => LineKind::Agent,
            Segment::ToolCall { .. } => LineKind::Tool,
            Segment::Verification { passed, .. } => LineKind::Verification { passed: *passed },
            Segment::System { kind, .. } => match kind {
                SystemKind::Paused => LineKind::Paused,
                SystemKind::Error => LineKind::Error,
                SystemKind::Approval | SystemKind::Info => LineKind::System,
            },
        }
    }
}

/// 助手侧消息序列：已落段 + 流式中的待提交增量。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AssistantTurn {
    pub segments: Vec<Segment>,
    pub pending_reasoning: String,
    pub pending_text: String,
    /// 本回合是否收到过正文 delta。`Done` 事件的 `output.text` 是流式
    /// 全文的汇总（provider 会重复），已流式过正文就不再追加，否则消息
    /// 内容翻倍（曾导致 TUI 块高膨胀、后续消息被挤出视口）。
    pub text_delta_seen: bool,
}

impl AssistantTurn {
    /// 提交待提交推理为整段（推理增量不落段，直到正文/工具开始）。
    pub fn flush_reasoning(&mut self) {
        if !self.pending_reasoning.is_empty() {
            self.segments.push(Segment::Reasoning {
                text: std::mem::take(&mut self.pending_reasoning),
            });
        }
    }

    /// 提交待提交正文为整段。
    pub fn flush_text(&mut self) {
        if !self.pending_text.is_empty() {
            self.segments.push(Segment::Text {
                text: std::mem::take(&mut self.pending_text),
            });
        }
    }

    /// 提交全部待提交增量。
    pub fn flush_all(&mut self) {
        self.flush_reasoning();
        self.flush_text();
    }
}

/// 一个完整的生成回合。
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub id: u64,
    pub user_text: String,
    pub assistant: AssistantTurn,
    pub status: TurnStatus,
}

/// 会话：回合序列（增量构建，段数有界）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Conversation {
    turns: Vec<Turn>,
    current: Option<usize>,
    next_turn_id: u64,
}

impl Conversation {
    /// 开始新回合，返回 turn id。清空流式残留。
    pub fn begin_turn(&mut self, user_text: String) -> u64 {
        self.next_turn_id += 1;
        let id = self.next_turn_id;
        self.turns.push(Turn {
            id,
            user_text,
            assistant: AssistantTurn::default(),
            status: TurnStatus::Running,
        });
        self.current = Some(self.turns.len() - 1);
        id
    }

    /// 当前回合的可变引用；无当前回合返回 None。
    pub fn current_mut(&mut self) -> Option<&mut Turn> {
        self.current.map(|i| &mut self.turns[i])
    }

    /// 当前回合引用。
    pub fn current(&self) -> Option<&Turn> {
        self.current.map(|i| &self.turns[i])
    }

    /// 会话中的回合数。
    pub fn turn_count(&self) -> usize {
        self.turns.len()
    }

    /// 当前待提交正文（流式中）。
    pub fn pending_text(&self) -> &str {
        self.current()
            .map(|t| t.assistant.pending_text.as_str())
            .unwrap_or("")
    }

    /// 当前待提交推理（流式中）。
    pub fn pending_reasoning(&self) -> &str {
        self.current()
            .map(|t| t.assistant.pending_reasoning.as_str())
            .unwrap_or("")
    }

    /// 按序迭代全部段（跨回合），附带稳定 id。
    pub fn iter_segments(&self) -> impl Iterator<Item = (SegId, &Segment)> {
        self.turns.iter().flat_map(|turn| {
            turn.assistant
                .segments
                .iter()
                .enumerate()
                .map(move |(i, seg)| ((turn.id, i), seg))
        })
    }

    /// 按 id 查段的行类型（折叠切换需要有效状态判定）。
    pub fn segment_kind(&self, seg: SegId) -> Option<LineKind> {
        self.iter_segments()
            .find(|(id, _)| *id == seg)
            .map(|(_, s)| s.line_kind())
    }

    /// 指定回合的用户输入文本（渲染用户回合头用）。
    pub fn user_text_of(&self, turn_id: u64) -> Option<&str> {
        self.turns
            .iter()
            .find(|t| t.id == turn_id)
            .map(|t| t.user_text.as_str())
    }

    /// 追加一条系统事件段到当前回合（暂停/授权/信息/错误）。
    pub fn push_system(&mut self, kind: SystemKind, text: String) {
        let Some(turn) = self.current_mut() else {
            return;
        };
        turn.assistant.flush_reasoning();
        turn.assistant.segments.push(Segment::System { kind, text });
    }

    /// 把当前回合标记为已取消（Ctrl+C 取消接线）。
    pub fn mark_current_cancelled(&mut self) {
        if let Some(turn) = self.current_mut() {
            turn.assistant.flush_all();
            turn.status = TurnStatus::Cancelled;
        }
    }

    /// 段数（含各回合已落段，不含 pending）。
    pub fn segment_count(&self) -> usize {
        self.turns.iter().map(|t| t.assistant.segments.len()).sum()
    }

    /// 在段数超过上限时丢弃最老的完整回合（保留当前回合）。
    pub(crate) fn enforce_cap(&mut self) {
        while self.segment_count() > MAX_SEGMENTS && self.turns.len() > 1 {
            self.turns.remove(0);
            if let Some(i) = self.current {
                self.current = Some(i.saturating_sub(1));
            }
        }
    }
}

/// `Done` 事件的输出文本：构造辅助（仅测试与示例使用，避免 TUI 暴露
/// core 内部类型构造）。
#[cfg_attr(not(test), allow(dead_code))]
pub fn done_output(text: impl Into<String>) -> RunOutput {
    RunOutput {
        text: text.into(),
        tool_calls: vec![],
        usage: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_turn_assigns_incrementing_ids() {
        let mut c = Conversation::default();
        assert_eq!(c.begin_turn("a".into()), 1);
        assert_eq!(c.begin_turn("b".into()), 2);
        assert_eq!(c.turn_count(), 2);
    }

    #[test]
    fn segment_char_len_counts_preview_text() {
        let reasoning = Segment::Reasoning {
            text: "你好 world".into(),
        };
        assert_eq!(reasoning.char_len(), 8); // 你(1) 好(1) 空格(1) world(5)
        let tool = Segment::ToolCall {
            call_id: "1".into(),
            name: "grep".into(),
            arguments: "{\"p\":\"x\"}".into(),
            result: Some("ok".into()),
            status: ToolStatus::Ok,
        };
        assert_eq!(tool.char_len(), 11); // arguments 9 字符 + result 2 字符
    }

    #[test]
    fn line_kind_maps_segment_variants() {
        assert_eq!(
            Segment::Reasoning { text: "".into() }.line_kind(),
            LineKind::Reasoning
        );
        assert_eq!(
            Segment::Text { text: "".into() }.line_kind(),
            LineKind::Agent
        );
        assert_eq!(
            Segment::Verification {
                command: "".into(),
                passed: true,
                summary: "".into()
            }
            .line_kind(),
            LineKind::Verification { passed: true }
        );
        assert_eq!(
            Segment::System {
                kind: SystemKind::Paused,
                text: "".into()
            }
            .line_kind(),
            LineKind::Paused
        );
        assert_eq!(
            Segment::System {
                kind: SystemKind::Error,
                text: "".into()
            }
            .line_kind(),
            LineKind::Error
        );
    }

    #[test]
    fn cap_drops_oldest_turns_keeps_current() {
        let mut c = Conversation::default();
        // 每回合压 MAX_SEGMENTS 段：首个回合超限后应被丢弃，当前回合保留。
        let ids: Vec<u64> = (0..4)
            .map(|i| {
                let id = c.begin_turn(format!("u{i}"));
                for _ in 0..(MAX_SEGMENTS / 2) {
                    c.current_mut()
                        .unwrap()
                        .assistant
                        .segments
                        .push(Segment::Text { text: "x".into() });
                }
                c.enforce_cap();
                id
            })
            .collect();
        assert_eq!(ids, vec![1, 2, 3, 4]);
        assert_eq!(
            c.turn_count(),
            2,
            "4×1000 段 > 2000 上限，丢弃 2 个最老回合"
        );
        assert_eq!(c.current().unwrap().id, 4, "当前回合保留");
        assert_eq!(c.segment_count(), 2000);
    }

    #[test]
    fn iter_segments_crosses_turns_with_stable_ids() {
        let mut c = Conversation::default();
        c.begin_turn("q1".into());
        c.current_mut()
            .unwrap()
            .assistant
            .segments
            .push(Segment::Text { text: "a".into() });
        c.begin_turn("q2".into());
        c.current_mut()
            .unwrap()
            .assistant
            .segments
            .push(Segment::Text { text: "b".into() });
        let got: Vec<(SegId, &str)> = c
            .iter_segments()
            .map(|(id, s)| match s {
                Segment::Text { text } => (id, text.as_str()),
                _ => (id, ""),
            })
            .collect();
        assert_eq!(got, vec![((1, 0), "a"), ((2, 0), "b")]);
    }

    #[test]
    fn done_output_builds_run_output() {
        let out = done_output("done");
        assert_eq!(out.text, "done");
        assert!(out.tool_calls.is_empty());
        assert!(out.usage.is_none());
    }
}

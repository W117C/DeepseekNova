//! 对话消息流渲染：从消息树生成渲染行（折叠/选中/diff 高亮/pending/echo）。
//!
//! 渲染 = 树渲染 → pending → 命令反馈 echo；样式全部经 [`Theme`]。

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::state::{AppState, DisplayMode};
use crate::model::conversation::{LineKind, Segment};
use crate::theme::Theme;

/// 段 → 纯文本（复制用，`AppState::copy_selected` 调用）。
pub fn segment_plain_text(seg: &Segment) -> String {
    match seg {
        Segment::Reasoning { text } => text.clone(),
        Segment::Text { text } => text.clone(),
        Segment::ToolCall {
            name,
            arguments,
            result,
            status,
            ..
        } => {
            let status_mark = match status {
                crate::model::conversation::ToolStatus::Running => "…",
                crate::model::conversation::ToolStatus::Ok => "✓",
                crate::model::conversation::ToolStatus::Failed => "✗",
            };
            match result {
                Some(r) => format!("[{status_mark}] {name}({arguments})\n  → {r}"),
                None => format!("[{status_mark}] {name}({arguments})"),
            }
        }
        Segment::Verification {
            command,
            passed,
            summary,
        } => {
            let mark = if *passed { "✓" } else { "✗" };
            if *passed || summary.is_empty() {
                format!("{mark} 验证: {command}")
            } else {
                format!("{mark} 验证: {command} — {summary}")
            }
        }
        Segment::System { text, .. } => text.clone(),
    }
}

/// 段类型标签（Raw 模式前缀）。
pub fn kind_tag(kind: LineKind) -> &'static str {
    match kind {
        LineKind::User => "user",
        LineKind::Agent => "agent",
        LineKind::Reasoning => "reasoning",
        LineKind::Tool => "tool",
        LineKind::ToolResult => "tool_result",
        LineKind::Verification { passed } => {
            if passed {
                "verify_ok"
            } else {
                "verify_fail"
            }
        }
        LineKind::System => "system",
        LineKind::Error => "error",
        LineKind::Paused => "paused",
    }
}

/// 折叠摘要文本。
fn folded_summary(seg: &Segment) -> String {
    match seg {
        Segment::Reasoning { .. } => {
            format!("[推理 ▸ 折叠 {} 字符 · Enter 展开]", seg.char_len())
        }
        Segment::ToolCall { name, .. } => format!("[工具 ▸ {name} 已折叠 · Enter 展开]"),
        _ => "[已折叠 · Enter 展开]".to_string(),
    }
}

/// 从消息树生成对话区渲染行（含用户回合头、折叠、选中、pending、echo）。
pub fn render_conversation_lines(app: &AppState, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    let mut last_turn: Option<u64> = None;

    for (seg_id, seg) in app.conversation.iter_segments() {
        let (turn_id, _) = seg_id;
        // 新回合：先渲染用户输入头。
        if last_turn != Some(turn_id) {
            last_turn = Some(turn_id);
            if let Some(user_text) = app.conversation.user_text_of(turn_id) {
                let text = if app.display_mode == DisplayMode::Raw {
                    format!("[user] 你: {user_text}")
                } else {
                    format!("你: {user_text}")
                };
                lines.push(Line::from(Span::styled(text, theme.user)));
            }
        }
        let kind = seg.line_kind();
        // Lite 模式隐藏推理。
        if app.display_mode == DisplayMode::Lite && kind == LineKind::Reasoning {
            continue;
        }
        let folded = app.is_folded(seg_id, kind);
        let base = if folded {
            theme.system
        } else {
            theme.style_for(kind)
        };
        let text = if folded {
            folded_summary(seg)
        } else {
            segment_display_text(seg, app.display_mode)
        };
        // diff 行级高亮（工具结果/模型正文沿用旧行为）。
        let styled: Line = if !folded && matches!(kind, LineKind::ToolResult | LineKind::Agent) {
            Line::from(theme.diff_spans(&text, base))
        } else {
            Line::from(Span::styled(text, base))
        };
        let styled = if app.selected == Some(seg_id) {
            styled.patch_style(theme.selection)
        } else {
            styled
        };
        lines.push(styled);
    }

    // pending（流式中未提交段）。
    if !app.conversation.pending_reasoning().is_empty() && app.display_mode != DisplayMode::Lite {
        let text = if app.display_mode == DisplayMode::Raw {
            format!("[reasoning] {}", app.conversation.pending_reasoning())
        } else {
            app.conversation.pending_reasoning().to_string()
        };
        lines.push(Line::from(Span::styled(
            text,
            theme.style_for(LineKind::Reasoning),
        )));
    }
    if !app.conversation.pending_text().is_empty() {
        let text = if app.display_mode == DisplayMode::Raw {
            format!("[agent] {}", app.conversation.pending_text())
        } else {
            app.conversation.pending_text().to_string()
        };
        lines.push(Line::from(Span::styled(
            text,
            theme.style_for(LineKind::Agent),
        )));
    }

    // 命令反馈 echo。
    for ui in &app.echo {
        let text = if app.display_mode == DisplayMode::Raw {
            format!("[{}] {}", kind_tag(ui.kind), ui.text)
        } else {
            ui.text.clone()
        };
        lines.push(Line::from(Span::styled(text, theme.style_for(ui.kind))));
    }
    lines
}

/// 段 → 显示文本（展开态，含 Raw 前缀与工具调用内联）。
fn segment_display_text(seg: &Segment, mode: DisplayMode) -> String {
    let raw = |kind: LineKind, text: String| -> String {
        if mode == DisplayMode::Raw {
            format!("[{}] {text}", kind_tag(kind))
        } else {
            text
        }
    };
    match seg {
        Segment::Reasoning { text } => raw(LineKind::Reasoning, text.clone()),
        Segment::Text { text } => raw(LineKind::Agent, text.clone()),
        Segment::ToolCall {
            name,
            arguments,
            result,
            status,
            ..
        } => {
            let status_mark = match status {
                crate::model::conversation::ToolStatus::Running => "…",
                crate::model::conversation::ToolStatus::Ok => "✓",
                crate::model::conversation::ToolStatus::Failed => "✗",
            };
            let head = format!("⚙ {status_mark} {name}({arguments})");
            let mut text = raw(LineKind::Tool, head);
            if let Some(r) = result {
                text.push('\n');
                text.push_str(&raw(LineKind::ToolResult, format!("  → {r}")));
            }
            text
        }
        Segment::Verification {
            command,
            passed,
            summary,
        } => {
            let mark = if *passed { "✓" } else { "✗" };
            let mut text = format!("{mark} 验证: {command}");
            if !passed && !summary.is_empty() {
                text.push_str(&format!(" — {summary}"));
            }
            raw(LineKind::Verification { passed: *passed }, text)
        }
        Segment::System { kind, text } => {
            let line_kind = match kind {
                crate::model::conversation::SystemKind::Paused => LineKind::Paused,
                crate::model::conversation::SystemKind::Error => LineKind::Error,
                crate::model::conversation::SystemKind::Approval
                | crate::model::conversation::SystemKind::Info => LineKind::System,
            };
            raw(line_kind, text.clone())
        }
    }
}

/// 命令面板/补全浮层在对话区上方绘制（Clear + 面板）。
fn overlay(area: Rect) -> Rect {
    let w = area.width.min(60);
    let h = area.height.min(14);
    Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + 2,
        width: w,
        height: h,
    }
}

impl AppState {
    /// 全量渲染入口（每帧由事件循环调用）。
    pub fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        let theme = self.theme.clone();
        let full = area;

        // 横向：侧边栏可见时切分。
        let (main_area, sidebar_area) = if crate::render::layout::sidebar_visible(full.width, self)
        {
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(0), Constraint::Length(26)])
                .split(full);
            (chunks[0], Some(chunks[1]))
        } else {
            (full, None)
        };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(crate::render::layout::layout_constraints())
            .split(main_area);
        let conv_area = chunks[0];
        let status_area = chunks[1];
        let input_area = chunks[2];
        let hint_area = chunks[3];

        let viewport = conv_area.height.saturating_sub(2) as usize;
        self.clamp_scroll(viewport.max(1));

        // ── 对话区 ────────────────────────────────────────
        let title = if self.running {
            "运行中…"
        } else {
            "就绪"
        };
        let conv_block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border)
            .title(Line::from(Span::styled(title, theme.title)));
        let text_lines = crate::render::message::render_conversation_lines(self, &theme);
        let paragraph = Paragraph::new(text_lines)
            .block(conv_block)
            .wrap(Wrap { trim: false })
            .scroll((self.scroll_offset as u16, 0));
        f.render_widget(paragraph, conv_area);

        // ── 状态行 ────────────────────────────────────────
        let scroll_pct = if self.render_line_count() == 0 {
            0
        } else {
            self.scroll_offset * 100 / self.render_line_count()
        };
        let status = Paragraph::new(Line::from(crate::render::status::status_segments(
            self, &theme, scroll_pct,
        )));
        f.render_widget(status, status_area);

        // ── 输入区 ────────────────────────────────────────
        crate::render::input::render_input(self, &theme, f, input_area);

        // ── 提示行 ────────────────────────────────────────
        let hint = Paragraph::new(Span::styled(
            crate::render::status::hint_for(self.focus),
            Style::default().add_modifier(Modifier::DIM),
        ));
        f.render_widget(hint, hint_area);

        // ── 命令面板（浮层）────────────────────────────────
        if self.palette.is_some() {
            crate::render::palette::render_palette(self, &theme, f, overlay(conv_area));
        } else if self.completion.is_some() {
            crate::render::input::render_completion(self, &theme, f, overlay(conv_area));
        }

        // ── 侧边栏 ────────────────────────────────────────
        if let Some(side) = sidebar_area {
            crate::render::sidebar::render_sidebar(self, &theme, f, side);
        }

        // ── 输入可见光标（空闲时）──────────────────────────
        if !self.running && self.focus == crate::app::focus::Focus::Input && self.palette.is_none()
        {
            let pane_width = input_area.width.saturating_sub(2) as usize;
            let pane_rows = input_area.height.saturating_sub(2) as usize;
            let view = crate::input::editor::input_view(
                &self.input.text,
                self.input.cursor,
                pane_width.max(1),
                pane_rows.max(1),
            );
            let col = view.cursor_col.min(pane_width as u16);
            let row = (view.cursor_row - view.scroll_row) as u16;
            f.set_cursor_position((input_area.x + 1 + col, input_area.y + 1 + row));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::conversation::{done_output, ToolStatus};
    use deepseeknova_core::runner::RunEvent;

    #[test]
    fn segment_plain_text_formats_variants() {
        assert_eq!(
            segment_plain_text(&Segment::Text { text: "hi".into() }),
            "hi"
        );
        assert_eq!(
            segment_plain_text(&Segment::Reasoning { text: "r".into() }),
            "r"
        );
        let tool = Segment::ToolCall {
            call_id: "1".into(),
            name: "grep".into(),
            arguments: "x".into(),
            result: Some("hit".into()),
            status: ToolStatus::Ok,
        };
        assert_eq!(segment_plain_text(&tool), "[✓] grep(x)\n  → hit");
    }

    #[test]
    fn render_lines_include_user_head_and_echo() {
        let mut app = AppState::default();
        app.conversation.begin_turn("问题".into());
        app.apply_run_event(RunEvent::TextDelta("答案".into()));
        app.apply_run_event(RunEvent::Done(done_output("")));
        app.echo_line(LineKind::System, "已处理");
        let theme = Theme::default();
        let lines = render_conversation_lines(&app, &theme);
        let texts: Vec<String> = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(texts.iter().any(|t| t.contains("你: 问题")));
        assert!(texts.iter().any(|t| t.contains("答案")));
        assert!(texts.iter().any(|t| t.contains("已处理")));
    }

    #[test]
    fn lite_mode_hides_reasoning() {
        let mut app = AppState::default();
        app.conversation.begin_turn("q".into());
        app.apply_run_event(RunEvent::ReasoningDelta {
            text: "think".into(),
            signature: None,
        });
        app.apply_run_event(RunEvent::TextDelta("ans".into()));
        app.apply_run_event(RunEvent::Done(done_output("")));
        app.display_mode = DisplayMode::Lite;
        let theme = Theme::default();
        let lines = render_conversation_lines(&app, &theme);
        let texts: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(!texts.contains("think"), "lite 隐藏推理");
        assert!(texts.contains("ans"));
    }

    #[test]
    fn folded_reasoning_renders_summary() {
        let mut app = AppState::default();
        let id = app.conversation.begin_turn("q".into());
        app.apply_run_event(RunEvent::ReasoningDelta {
            text: "推理内容".into(),
            signature: None,
        });
        app.apply_run_event(RunEvent::TextDelta("ans".into()));
        app.apply_run_event(RunEvent::Done(done_output("")));
        app.fold.insert((id, 0), true);
        let theme = Theme::default();
        let lines = render_conversation_lines(&app, &theme);
        let texts: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(texts.contains("推理 ▸ 折叠"), "折叠摘要");
        assert!(!texts.contains("推理内容"), "折叠态不显示正文");
    }

    #[test]
    fn kind_tag_maps_all_kinds() {
        assert_eq!(kind_tag(LineKind::User), "user");
        assert_eq!(kind_tag(LineKind::Reasoning), "reasoning");
        assert_eq!(
            kind_tag(LineKind::Verification { passed: true }),
            "verify_ok"
        );
        assert_eq!(
            kind_tag(LineKind::Verification { passed: false }),
            "verify_fail"
        );
        assert_eq!(kind_tag(LineKind::Paused), "paused");
    }

    #[test]
    fn raw_mode_prefixes_kind() {
        let mut app = AppState::default();
        app.conversation.begin_turn("q".into());
        app.apply_run_event(RunEvent::TextDelta("a".into()));
        app.apply_run_event(RunEvent::Done(done_output("")));
        app.display_mode = DisplayMode::Raw;
        let theme = Theme::default();
        let lines = render_conversation_lines(&app, &theme);
        let texts: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(texts.contains("[agent] a"));
    }
}

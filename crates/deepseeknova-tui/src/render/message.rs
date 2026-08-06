//! 对话消息流渲染：从消息树生成渲染行（折叠/选中/diff 高亮/pending/echo）。
//!
//! 渲染 = 树渲染 → pending → 命令反馈 echo；样式全部经 [`Theme`]。

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::state::{AppState, DisplayMode};
use crate::model::conversation::{LineKind, Segment};
use crate::render::input::spinner_frame;
use crate::theme::Theme;

/// 估算 `Paragraph::wrap` 后的物理行数（滚动钳制/百分比据此而非段数）。
///
/// 直接模拟 ratatui 0.30 `WordWrapper` 的真实行为：Span 内嵌的 `\n` 会被
/// 当作控制字符过滤（不是硬换行），长词整词换行/溢出。消息构建时已用
/// [`span_lines`] 把 `\n` 展开成独立 `Line`，此函数兜底处理遗漏路径，
/// 并保证与渲染端逐字节一致——估算虚高会让贴底视图出现大片空白。
pub fn estimate_wrapped_lines(lines: &[Line<'_>], width: usize) -> usize {
    let width = width.max(1);
    lines
        .iter()
        .map(|line| {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            ratatui_wrapped_rows(&text, width)
        })
        .sum()
}

/// 模拟 ratatui 0.30 `WordWrapper`（`trim: false`）对单行文本产出的物理行数。
///
/// naive `ceil(总显示宽 / 行宽)` 与真实行为的关键差异：
/// - 空格分隔的“词”整词换行；超长词**不拆分**，整词占一行（可溢出）；
/// - 换行后行首空白会被丢弃；
/// - 控制字符（含 `\n`）在 grapheme 阶段就被过滤，不参与换行。
fn ratatui_wrapped_rows(text: &str, width: usize) -> usize {
    let width = width.max(1);
    let mut rows = 0usize;
    let mut line_width = 0usize;
    let mut word_width = 0usize;
    let mut ws_width = 0usize;
    let mut line_has_word = false;
    let mut prev_non_ws = false;

    for ch in text.chars().filter(|c| !c.is_control()) {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        let is_ws = ch.is_whitespace() && ch != '\u{00A0}';
        let word_found = prev_non_ws && is_ws;
        let untrimmed_overflow = !line_has_word && word_width + ws_width + w > width;

        if word_found || untrimmed_overflow {
            line_has_word = line_has_word || ws_width > 0 || word_width > 0;
            if line_has_word {
                line_width += ws_width;
            }
            line_width += word_width;
            ws_width = 0;
            word_width = 0;
        }

        let line_full = line_width >= width;
        let word_overflow = w > 0 && line_width + ws_width + word_width >= width;
        if line_full || word_overflow {
            rows += 1;
            line_has_word = false;
            let remaining = width.saturating_sub(line_width);
            line_width = 0;
            if ws_width > remaining {
                ws_width -= remaining;
            } else {
                ws_width = 0;
            }
            if is_ws && ws_width == 0 {
                continue;
            }
        }

        if is_ws {
            ws_width += w;
        } else {
            word_width += w;
        }
        prev_non_ws = !is_ws;
    }

    if line_has_word || ws_width > 0 || word_width > 0 {
        rows + 1
    } else if rows == 0 {
        1
    } else {
        rows
    }
}

/// 把可能含 `\n` 的 Span 序列展开成多条物理行（每行一条 `Line`）。
///
/// ratatui 0.30 的 `WordWrapper` 把 Span 内嵌的 `\n` 当作空白分隔（不是硬换行）。
/// 若把整段文本塞进单条 `Line`，渲染会忽略换行，而高度估算却按 `\n` 断行，
/// 导致 `rendered_lines` 严重虚高：自动贴底时视口被推过头，最后一轮下面
/// 出现大片空白（“两轮对话间隔过大”的根因）。
fn span_lines(spans: Vec<Span<'static>>) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    for span in spans {
        let mut first = true;
        for part in span.content.split('\n') {
            if !first {
                out.push(Line::from(std::mem::take(&mut cur)));
            }
            cur.push(Span::styled(part.to_string(), span.style));
            first = false;
        }
    }
    if !cur.is_empty() {
        out.push(Line::from(cur));
    }
    out
}

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

/// 一条带边框的消息块（用户回合 / agent 回合 / 系统反馈）。
pub struct MessageBlock {
    /// 角色前缀行（"你" / 模型名）；无前缀的块（echo 反馈）为 None。
    pub header: Option<String>,
    /// 前缀行样式（用户=accent、agent=agent 色）。
    pub header_style: Style,
    /// 内容行（含折叠摘要、选中高亮）。
    pub lines: Vec<Line<'static>>,
}

/// 把会话消息树按「回合」分组成消息块（Claude Code 风格：无边框，
/// 角色前缀行区分归属，正文安静展开）：
/// 每回合一个用户块 + 一个 agent 块。
pub fn build_conversation_blocks(app: &AppState, theme: &Theme) -> Vec<MessageBlock> {
    let mut blocks: Vec<MessageBlock> = Vec::new();
    // 首次启动（还没有任何回合）显示欢迎卡，替代“空面板 + 加载中噪声”。
    if app.conversation.turn_count() == 0 && !app.running {
        blocks.push(welcome_block(app, theme));
    }
    let mut last_turn: Option<u64> = None;
    let mut agent_lines: Vec<Line<'static>> = Vec::new();

    let flush_agent =
        |blocks: &mut Vec<MessageBlock>, lines: &mut Vec<Line<'static>>, title: &str| {
            if !lines.is_empty() {
                blocks.push(MessageBlock {
                    header: Some(title.to_string()),
                    header_style: theme.agent,
                    lines: std::mem::take(lines),
                });
            }
        };

    for (seg_id, seg) in app.conversation.iter_segments() {
        let (turn_id, _) = seg_id;
        if last_turn != Some(turn_id) {
            // 回合切换：落盘上一个 agent 块，开新用户块。
            flush_agent(&mut blocks, &mut agent_lines, app.model_label.as_str());
            last_turn = Some(turn_id);
            if let Some(user_text) = app.conversation.user_text_of(turn_id) {
                let text = if app.display_mode == DisplayMode::Raw {
                    format!("[user] {user_text}")
                } else {
                    user_text.to_string()
                };
                blocks.push(MessageBlock {
                    header: Some("你".to_string()),
                    header_style: Style::default().fg(theme.accent),
                    lines: span_lines(vec![Span::styled(text, theme.user)]),
                });
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
        // diff 行级高亮仅限工具调用段（含结果预览）：代码改动信息（git
        // diff 等）来自工具输出；模型正文不染色——正常聊天的回答里
        // `+`/`-` 开头行（如 markdown 列表）不应被误判为 diff。
        let spans = if !folded && kind == LineKind::Tool {
            theme.diff_spans(&text, base)
        } else {
            vec![Span::styled(text, base)]
        };
        let mut lines = span_lines(spans);
        if app.selected == Some(seg_id) {
            for line in &mut lines {
                *line = line.clone().patch_style(theme.selection);
            }
        }
        agent_lines.extend(lines);
    }
    // 流式 pending 与当前回合段合并，再统一落盘，避免分离的空块。
    if !app.conversation.pending_reasoning().is_empty() && app.display_mode != DisplayMode::Lite {
        let text = if app.display_mode == DisplayMode::Raw {
            format!("[reasoning] {}", app.conversation.pending_reasoning())
        } else {
            app.conversation.pending_reasoning().to_string()
        };
        agent_lines.extend(span_lines(vec![Span::styled(
            text,
            theme.style_for(LineKind::Reasoning),
        )]));
    }
    if !app.conversation.pending_text().is_empty() {
        let text = if app.display_mode == DisplayMode::Raw {
            format!("[agent] {}", app.conversation.pending_text())
        } else {
            app.conversation.pending_text().to_string()
        };
        agent_lines.extend(span_lines(vec![Span::styled(
            text,
            theme.style_for(LineKind::Agent),
        )]));
    }
    // 等待 agent 首批 delta：在对话区（agent 位置）显示转圈，
    // 而不是只有输入框里的“等待响应”。
    if app.running
        && app.conversation.current().is_some()
        && app.conversation.pending_reasoning().is_empty()
        && app.conversation.pending_text().is_empty()
    {
        let frame = app
            .run_started_at
            .map(|t| spinner_frame(t.elapsed()))
            .unwrap_or_else(|| spinner_frame(std::time::Duration::ZERO));
        agent_lines.push(Line::from(Span::styled(
            format!("{frame} 正在思考… Ctrl+C 取消"),
            theme.system,
        )));
    }
    if !agent_lines.is_empty() {
        blocks.push(MessageBlock {
            header: Some(app.model_label.clone()),
            header_style: theme.agent,
            lines: std::mem::take(&mut agent_lines),
        });
    }

    // 命令反馈 echo：无前缀轻量块。
    for ui in &app.echo {
        let text = if app.display_mode == DisplayMode::Raw {
            format!("[{}] {}", kind_tag(ui.kind), ui.text)
        } else {
            ui.text.clone()
        };
        blocks.push(MessageBlock {
            header: None,
            header_style: Style::default(),
            lines: span_lines(vec![Span::styled(text, theme.style_for(ui.kind))]),
        });
    }
    blocks
}

/// 首次启动欢迎卡：仿 Hermes 风格的无边框圆角卡片，
/// 替代“空空如也 + 一堆加载提示”的初始页面。
fn welcome_block(app: &AppState, theme: &Theme) -> MessageBlock {
    const W: usize = 50;
    let sessions = if app.sessions_loaded {
        format!(
            "最近保存会话: {} 个（侧边栏/会话 面板恢复）",
            app.saved_sessions.len()
        )
    } else {
        "最近保存会话: 侧边栏/会话 面板查看".to_string()
    };
    let rule = "─".repeat(W);
    let body = |s: &str| Span::styled(s.to_string(), theme.system);
    let title = Span::styled(
        "DeepseekNova",
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    );
    let welcome_row = |content: Vec<Span<'static>>| {
        let text_width: usize = content.iter().map(|s| s.content.width()).sum();
        let pad = (W + 2).saturating_sub(text_width + 3);
        let mut spans = vec![Span::styled("│ ".to_string(), theme.border)];
        spans.extend(content);
        spans.push(Span::styled(format!("{}│", " ".repeat(pad)), theme.border));
        Line::from(spans)
    };
    let lines = vec![
        Line::from(Span::styled(format!("╭{rule}╮"), theme.border)),
        welcome_row(vec![title]),
        welcome_row(vec![body("AI Agent 终端 · 会话自动持久化")]),
        Line::from(Span::styled(format!("│{}│", " ".repeat(W)), theme.border)),
        welcome_row(vec![body("输入 /help 查看全部命令")]),
        welcome_row(vec![body(
            "Tab 切换焦点 · Ctrl+\\ 侧边栏 · 鼠标滚轮滚动历史",
        )]),
        welcome_row(vec![body(&sessions)]),
        Line::from(Span::styled(format!("╰{rule}╯"), theme.border)),
    ];
    MessageBlock {
        header: None,
        header_style: Style::default(),
        lines,
    }
}

/// 消息块在给定面板宽度下的物理高度：角色前缀行 1 行 + 内容 wrap 行数。
/// 无前缀块（echo）只算内容。
///
/// 高度估算必须与 [`render_blocks`] 的渲染宽度一致（无边框全宽 Paragraph）：
/// 若估算按 `width - 2` 折行，内容宽度落在 `(w-2, w]` 区间时估算高 1 行，
/// 块与块之间会留下实际空白行——视觉上就是「两轮对话间隔过大」。
pub fn block_height(block: &MessageBlock, width: usize) -> usize {
    let inner = estimate_wrapped_lines(&block.lines, width.max(1)).max(1);
    if block.header.is_some() {
        inner + 1
    } else {
        inner
    }
}

/// 对话区叠放渲染：按 `offset`（物理行）裁剪可见窗口，逐块绘制。
/// 每块一个独立 Paragraph（无边框，角色前缀行 + 内容），块内用 `scroll`
/// 跳过窗口上方的行，与整区滚动语义一致。
pub fn render_blocks(f: &mut Frame, area: Rect, blocks: &[MessageBlock], offset: usize) {
    let pane_width = area.width as usize;
    let mut y = 0usize; // 全局物理行游标
    for block in blocks {
        let bh = block_height(block, pane_width);
        if y + bh <= offset {
            // 整块在窗口上方：跳过。
            y += bh;
            continue;
        }
        let visible_start = y.saturating_sub(offset);
        if visible_start >= area.height as usize {
            // 整块在窗口下方：后续块更远，直接结束。
            break;
        }
        let skip_in_block = offset.saturating_sub(y); // 块内跳过行数
        let visible = bh.saturating_sub(skip_in_block);
        let height = visible.min(area.height as usize - visible_start);
        if height == 0 {
            y += bh;
            continue;
        }
        let rect = Rect {
            x: area.x,
            y: area.y + visible_start as u16,
            width: area.width,
            height: height as u16,
        };
        let mut paragraph = Paragraph::new(block.lines.clone()).wrap(Wrap { trim: false });
        // 角色前缀行：作为内容首行渲染（无边框），前缀行样式 = header_style。
        if let Some(header) = &block.header {
            // 前缀行右侧铺一条细分隔线（Hermes 式卡片头），让会话归属更醒目。
            let header_w = header.width();
            let sep_len = area.width.saturating_sub(header_w as u16).saturating_sub(3) as usize;
            let mut lines = vec![Line::from(vec![
                Span::styled(format!("{header}:"), block.header_style),
                Span::styled(
                    format!(" {}", "─".repeat(sep_len)),
                    block.header_style.add_modifier(Modifier::DIM),
                ),
            ])];
            lines.extend(block.lines.clone());
            paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        }
        paragraph = paragraph.scroll((skip_in_block as u16, 0));
        f.render_widget(paragraph, rect);
        y += bh;
    }
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
                .constraints([Constraint::Min(0), Constraint::Length(30)])
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

        // 消息块布局：用户/agent 各带边框标题，归属一眼可辨。
        // 先估算总物理高度（含边框）再钳制滚动。
        let blocks = crate::render::message::build_conversation_blocks(self, &theme);
        let pane_width = conv_area.width as usize;
        self.rendered_lines = blocks
            .iter()
            .map(|b| crate::render::message::block_height(b, pane_width))
            .sum();
        let viewport = conv_area.height as usize;
        self.clamp_scroll(viewport.max(1));
        crate::render::message::render_blocks(f, conv_area, &blocks, self.scroll_offset);

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
        // 斜杠命令行内候选：画在状态行上方（输入区正上方），就地展开。
        // 高度 = 候选行数 + 边框 2 行（render_command_hint 的 Block 边框）。
        if self.command_hint.is_some() {
            let hint_h = self
                .command_hint
                .as_ref()
                .map(|h| h.visible_rows() + 2)
                .unwrap_or(3);
            // 短终端保护：候选浮层高度不超过状态行上方可用行数，
            // 避免浮层 Rect 溢出终端底部导致绘制异常。
            let hint_h = hint_h.min(status_area.y.max(1) as usize).max(1);
            let hint_area = Rect {
                x: status_area.x,
                y: status_area.y.saturating_sub(hint_h as u16),
                width: status_area.width,
                height: hint_h as u16,
            };
            crate::render::input::render_command_hint(self, &theme, f, hint_area);
        }

        // ── 提示行 ────────────────────────────────────────
        let hint = Paragraph::new(Span::styled(
            crate::render::status::hint_for(self.focus),
            Style::default().add_modifier(Modifier::DIM),
        ));
        f.render_widget(hint, hint_area);

        // ── 浮层（审批 / @ 补全）──────────────────────────
        if let Some(approval) = &self.pending_approval {
            crate::render::approval::render_approval(approval, &theme, f, overlay(conv_area));
        } else if self.completion.is_some() {
            crate::render::input::render_completion(self, &theme, f, overlay(conv_area));
        }

        // ── 侧边栏 ────────────────────────────────────────
        if let Some(side) = sidebar_area {
            crate::render::sidebar::render_sidebar(self, &theme, f, side);
        }

        // ── 输入可见光标（空闲时）──────────────────────────
        if !self.running && self.focus == crate::app::focus::Focus::Input {
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
    use ratatui::style::Color;

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
    fn blocks_include_user_agent_and_echo() {
        let mut app = AppState::default();
        app.conversation.begin_turn("问题".into());
        app.apply_run_event(RunEvent::TextDelta("答案".into()));
        app.apply_run_event(RunEvent::Done(done_output("")));
        app.echo_line(LineKind::System, "已处理");
        let theme = Theme::default();
        let blocks = build_conversation_blocks(&app, &theme);
        assert_eq!(blocks.len(), 3, "用户块 + agent 块 + echo 块");
        assert_eq!(blocks[0].header.as_deref(), Some("你"));
        assert!(block_texts(&blocks[0]).iter().any(|t| t.contains("问题")));
        assert!(block_texts(&blocks[1]).iter().any(|t| t.contains("答案")));
        assert!(block_texts(&blocks[2]).iter().any(|t| t.contains("已处理")));
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
        let blocks = build_conversation_blocks(&app, &theme);
        let texts: String = blocks.iter().flat_map(block_texts).collect();
        assert!(!texts.contains("think"), "lite 隐藏推理");
        assert!(texts.contains("ans"));
    }

    #[test]
    fn folded_reasoning_renders_summary() {
        let mut app = AppState::default();
        app.conversation.begin_turn("q".into());
        app.apply_run_event(RunEvent::ReasoningDelta {
            text: "推理内容".into(),
            signature: None,
        });
        app.apply_run_event(RunEvent::TextDelta("ans".into()));
        app.apply_run_event(RunEvent::Done(done_output("")));
        let theme = Theme::default();
        let blocks = build_conversation_blocks(&app, &theme);
        let texts: String = blocks.iter().flat_map(block_texts).collect();
        assert!(texts.contains("推理 ▸ 折叠"), "推理默认折叠摘要");
        assert!(!texts.contains("推理内容"), "折叠态不显示正文");
    }

    fn block_texts(block: &MessageBlock) -> Vec<String> {
        block
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect()
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
        let blocks = build_conversation_blocks(&app, &theme);
        let texts: String = blocks.iter().flat_map(block_texts).collect();
        assert!(texts.contains("[agent] a"));
    }

    #[test]
    fn estimate_wrapped_lines_counts_physical_rows() {
        use ratatui::text::Span;
        // 纯 ASCII：宽 10 的 26 字符行 → 3 物理行。
        let lines = vec![Line::from(Span::raw("abcdefghijklmnopqrstuvwxyz"))];
        assert_eq!(estimate_wrapped_lines(&lines, 10), 3);
        // 中文宽字符：宽 4 的「你好世界」(8 列) → 2 物理行。
        let lines = vec![Line::from(Span::raw("你好世界"))];
        assert_eq!(estimate_wrapped_lines(&lines, 4), 2);
        // 行内 \n 会被 ratatui 当控制字符过滤（不产生换行）：
        // "abcde\nfghij" 实际按 "abcdefghij" 连续折行，宽 10 → 1 行。
        let lines = vec![Line::from(Span::raw("abcde\nfghij"))];
        assert_eq!(estimate_wrapped_lines(&lines, 10), 1);
        // 空行计 1 行。
        let lines = vec![Line::from(Span::raw("")), Line::from(Span::raw("x"))];
        assert_eq!(estimate_wrapped_lines(&lines, 10), 2);
        // 宽度 0 保护：按 1 列处理（不除零），"abc" → 3 物理行。
        let lines = vec![Line::from(Span::raw("abc"))];
        assert_eq!(estimate_wrapped_lines(&lines, 0), 3);
    }

    #[test]
    fn estimate_filters_newlines_like_ratatui() {
        use ratatui::text::Span;
        // ratatui 0.30 的 WordWrapper 会把 Span 内嵌的 \n 作为控制字符过滤，
        // 不产生硬换行。估算必须与渲染一致（消息构建时已用 span_lines
        // 把 \n 展开成独立 Line，这里兜底处理遗漏路径）。
        let lines = vec![Line::from(Span::raw("abc\n"))];
        assert_eq!(estimate_wrapped_lines(&lines, 10), 1, "尾部换行被过滤");
        // 多段各带尾部换行：1 行 + 1 行 = 2。
        let lines = vec![
            Line::from(Span::raw("abc\n")),
            Line::from(Span::raw("def\n")),
        ];
        assert_eq!(estimate_wrapped_lines(&lines, 10), 2);
        // 空串：渲染 1 行，估算 1 行。
        let lines = vec![Line::from(Span::raw(""))];
        assert_eq!(estimate_wrapped_lines(&lines, 10), 1);
        // 连续换行被过滤："a\n\nb\n" → "ab" → 1 行。
        let lines = vec![Line::from(Span::raw("a\n\nb\n"))];
        assert_eq!(estimate_wrapped_lines(&lines, 10), 1);
    }

    #[test]
    fn multi_turn_conversation_blocks_stay_visible_after_scroll() {
        // 回归：多轮对话（agent 正文以 \n 结尾）在贴底滚动后，
        // 早期回合内容必须仍可通过滚动看到（未被后续块覆盖/挤出）。
        // 此前估算丢尾部换行 → 总高低估 → 贴底 offset 偏小，尾部
        // 块溢出视口、中间块被前块实际渲染覆盖。
        let mut app = AppState::default();
        let theme = Theme::default();
        for i in 0..6 {
            app.conversation.begin_turn(format!("问题{i}"));
            app.apply_run_event(RunEvent::TextDelta(format!(
                "第 {i} 轮回答内容尾部标记轮{i}\n"
            )));
            app.apply_run_event(RunEvent::Done(done_output("")));
        }
        let blocks = build_conversation_blocks(&app, &theme);
        let pane_width = 60;
        let total: usize = blocks.iter().map(|b| block_height(b, pane_width)).sum();
        // 视口 15 行：内容总高必须超过视口（保证滚动场景有效），
        // 且估算总高与逐块渲染高度一致——贴底后最后一轮可见。
        assert!(total > 15, "构造内容需超过测试视口: {total}");
        let buf = ratatui::backend::TestBackend::new(60, 15);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        let offset = total.saturating_sub(15);
        terminal
            .draw(|f| {
                let area = f.area();
                render_blocks(f, area, &blocks, offset);
            })
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        let flat: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            flat.contains("第5轮回答内容尾部标记轮5"),
            "贴底后最后一轮必须可见\n{content}"
        );
        // 滚回顶部：第一轮内容必须可达。
        terminal
            .draw(|f| {
                let area = f.area();
                render_blocks(f, area, &blocks, 0);
            })
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        let flat: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(flat.contains("问题0"), "滚动到顶部第一轮可见\n{content}");
        assert!(
            flat.contains("第0轮回答内容尾部标记轮0"),
            "滚动到顶部第一轮回答可见\n{content}"
        );
    }

    #[test]
    fn agent_text_is_not_diff_colored() {
        // 回归：普通聊天的模型正文（+ 开头行）不得 diff 染色。
        use ratatui::style::Color;
        let mut app = AppState::default();
        app.conversation.begin_turn("q".into());
        app.apply_run_event(RunEvent::TextDelta("+ 读代码\n- 写测试".into()));
        app.apply_run_event(RunEvent::Done(done_output("")));
        let theme = Theme::default();
        let blocks = build_conversation_blocks(&app, &theme);
        let agent_block = &blocks[1]; // 块 0 是用户块
        for span in agent_block.lines.iter().flat_map(|l| l.spans.iter()) {
            assert_ne!(
                span.style.fg,
                Some(theme.verification_ok.fg.unwrap_or(Color::Green)),
                "Agent 正文不得按 diff 染色"
            );
        }
    }

    #[test]
    fn tool_result_keeps_diff_highlight() {
        // 工具调用默认折叠；显式展开后 diff 染色可见。
        let mut app = AppState::default();
        let id = app.conversation.begin_turn("q".into());
        app.apply_run_event(RunEvent::ToolCallStart {
            id: "1".into(),
            name: "git".into(),
        });
        app.apply_run_event(RunEvent::ToolResult {
            call_id: "1".into(),
            result: "+fn new() {}\n-fn old() {}".into(),
        });
        app.apply_run_event(RunEvent::Done(done_output("")));
        // 工具段是当前回合第 0 段。
        app.fold.insert((id, 0), false);
        let theme = Theme::default();
        let blocks = build_conversation_blocks(&app, &theme);
        let tool_block = &blocks[1];
        let plus_span = tool_block
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content.contains("+fn new() {}"));
        assert!(plus_span.is_some(), "工具结果的 + 行应保留");
        assert_eq!(
            plus_span.unwrap().style.fg,
            Some(theme.verification_ok.fg.unwrap_or(Color::Green)),
            "工具结果 + 行按 diff 染色（剥掉 `  → ` 前缀后判定）"
        );
    }

    #[test]
    fn tool_call_folds_by_default() {
        // 工具调用默认折叠为摘要行，agent 输出保持整洁。
        let mut app = AppState::default();
        app.conversation.begin_turn("q".into());
        app.apply_run_event(RunEvent::ToolCallStart {
            id: "1".into(),
            name: "grep".into(),
        });
        app.apply_run_event(RunEvent::ToolResult {
            call_id: "1".into(),
            result: "a".repeat(500),
        });
        app.apply_run_event(RunEvent::Done(done_output("")));
        let theme = Theme::default();
        let blocks = build_conversation_blocks(&app, &theme);
        let texts: String = blocks.iter().flat_map(block_texts).collect();
        assert!(texts.contains("工具 ▸ grep 已折叠"), "工具默认折叠摘要");
        assert!(!texts.contains(&"a".repeat(500)), "折叠态不显示结果");
    }

    #[test]
    fn welcome_block_shown_before_first_turn() {
        let theme = Theme::default();
        let app = AppState::default();
        let blocks = build_conversation_blocks(&app, &theme);
        assert!(!blocks.is_empty());
        let texts: String = blocks.iter().flat_map(block_texts).collect();
        assert!(texts.contains("DeepseekNova"), "欢迎卡标题: {texts}");
        assert!(texts.contains("/help"), "欢迎卡命令提示: {texts}");
        assert!(texts.contains('╭'), "欢迎卡圆角边框: {texts}");

        let mut app = AppState::default();
        app.conversation.begin_turn("你好".into());
        let blocks = build_conversation_blocks(&app, &theme);
        let texts: String = blocks.iter().flat_map(block_texts).collect();
        assert!(!texts.contains("DeepseekNova"), "首轮开始后欢迎卡消失");
    }

    #[test]
    fn waiting_spinner_shown_in_conversation_while_running() {
        let theme = Theme::default();
        let mut app = AppState {
            running: true,
            run_started_at: Some(std::time::Instant::now() - std::time::Duration::from_millis(350)),
            ..Default::default()
        };
        app.conversation.begin_turn("q".into());
        let blocks = build_conversation_blocks(&app, &theme);
        let texts: String = blocks.iter().flat_map(block_texts).collect();
        assert!(texts.contains("正在思考"), "对话区等待提示: {texts}");
        assert!(texts.contains('⠸'), "350ms 推进到第 3 帧: {texts}");

        // 首批文本到达后，等待提示消失、正文出现。
        app.apply_run_event(RunEvent::TextDelta("hi".into()));
        let blocks = build_conversation_blocks(&app, &theme);
        let texts: String = blocks.iter().flat_map(block_texts).collect();
        assert!(!texts.contains("正在思考"), "有内容后不再显示等待");
        assert!(texts.contains("hi"));
    }

    #[test]
    fn estimated_height_matches_actual_render_for_realistic_turns() {
        let mut app = AppState::default();
        let theme = Theme::default();
        app.conversation.begin_turn("你好".into());
        app.apply_run_event(RunEvent::TextDelta("你好！我是 DeepseekNova，一个终端里的软件工程代理，可以在你的仓库里读代码、跑命令、改文件、做验证。\n\n有什么需要我帮忙的吗？比如：\n- 修复 bug 或实现某个功能\n- 理解某段代码的行为\n- 跑测试或检查构建\n- 重构或评估改动的影响面\n\n告诉我任务，我就开始干活。".into()));
        app.apply_run_event(RunEvent::Done(done_output("")));
        app.conversation.begin_turn("你可以干什么".into());
        app.apply_run_event(RunEvent::TextDelta("我能做的，都是围绕\"在仓库里干活\"这件事：\n\n**代码探索与理解**\n- 搜索符号、追踪调用链（谁调用了谁、动态分发、依赖关系）\n- 评估改动的影响范围（改一个函数会波及哪些地方）\n- 阅读任意文件、列出目录结构\n\n**开发与修改**\n- 读代码 → 定位问题 → 修改实现\n- 新增功能、修 bug、重构\n- 写文件、移动文件、运行命令\n\n**验证与测试**\n- 跑构建、跑测试、检查退出码\n- 修复失败的实际原因，再重新验证，直到通过\n\n**其他辅助**\n- 查阅第三方库文档（Context7）\n- 抓取网页内容\n- 把重要的项目状态记到长期记忆里，方便后续任务复用\n\n**我的工作方式**：低成本、高频率的小循环——观察 → 计划 → 执行 → 验证 → 反思，一步一步来，不会一次性猜答案。遇到破坏性/不可逆的操作，我会先停下来问你，而不是擅自执行。\n\n给我一个具体任务就行，比如：\"看看这个 bug 在哪\"、\"给 X 加个 Y 功能\"、\"跑一下测试并修复失败项\"。你想做什么？".into()));
        app.apply_run_event(RunEvent::Done(done_output("")));
        let blocks = build_conversation_blocks(&app, &theme);
        let total: usize = blocks.iter().map(|b| block_height(b, 90)).sum();
        let buf = ratatui::backend::TestBackend::new(90, 1000);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_blocks(f, area, &blocks, 0);
            })
            .unwrap();
        let mut last_y = 0usize;
        for y in 0..1000 {
            let non_empty = (0..90).any(|x| {
                terminal
                    .backend()
                    .buffer()
                    .cell((x, y))
                    .map(|c| c.symbol() != " " && !c.symbol().is_empty())
                    .unwrap_or(false)
            });
            if non_empty {
                last_y = y as usize;
            }
        }
        let actual_total = last_y + 1;
        assert_eq!(
            total, actual_total,
            "估算行数必须与 ratatui 实际渲染一致（否则贴底视图出现大片空白）"
        );
    }

    #[test]
    fn block_height_counts_borders() {
        // 带前缀块：前缀 1 行 + 内容行；无前缀块：仅内容行。
        let block = MessageBlock {
            header: Some("你".into()),
            header_style: Style::default(),
            lines: vec![Line::from(Span::raw("abc"))],
        };
        assert_eq!(block_height(&block, 10), 2);
        let echo = MessageBlock {
            header: None,
            header_style: Style::default(),
            lines: vec![Line::from(Span::raw("abc"))],
        };
        assert_eq!(block_height(&echo, 10), 1);
    }

    #[test]
    fn block_height_matches_render_width_no_phantom_gap() {
        // 回归：块间不得出现「幽灵间距」。此前估算按 width-2 折行而渲染
        // 是全宽，内容宽度落在 (w-2, w] 区间时会高估 1 行，导致相邻消息
        // 块之间留下空白行（两轮对话间隔过大的观感来源）。
        // 宽 10 面板 + 9 列内容：按 10 折行 = 1 行，按 8 折行 = 2 行。
        let block = MessageBlock {
            header: Some("你".into()),
            header_style: Style::default(),
            lines: vec![Line::from(Span::raw("123456789"))],
        };
        assert_eq!(block_height(&block, 10), 2, "前缀 1 + 内容 1，无幽灵行");
        // 与 render_blocks 的裁剪一致性：两轮对话 4 块的总高应等于逐块和。
        let blocks = [
            MessageBlock {
                header: Some("你".into()),
                header_style: Style::default(),
                lines: vec![Line::from(Span::raw("123456789"))],
            },
            MessageBlock {
                header: Some("m".into()),
                header_style: Style::default(),
                lines: vec![Line::from(Span::raw("123456789"))],
            },
            MessageBlock {
                header: Some("你".into()),
                header_style: Style::default(),
                lines: vec![Line::from(Span::raw("x"))],
            },
        ];
        let total: usize = blocks.iter().map(|b| block_height(b, 10)).sum();
        assert_eq!(total, 2 + 2 + 2, "估算总高不得凭空多出行");
    }

    #[test]
    fn render_blocks_scrolls_to_later_content() {
        // 回归：长会话滚动（offset>0）必须能看到后续块的内容，
        // 否则"看不到新消息"。用 TestBackend 实测 ratatui 渲染结果。
        let blocks = vec![
            MessageBlock {
                header: Some("你".into()),
                header_style: Style::default(),
                lines: vec![Line::from(Span::raw("第一轮问题"))],
            },
            MessageBlock {
                header: Some("default".into()),
                header_style: Style::default(),
                lines: vec![Line::from(Span::raw("第一轮回答".repeat(100)))],
            },
            MessageBlock {
                header: Some("你".into()),
                header_style: Style::default(),
                lines: vec![Line::from(Span::raw("第二轮问题"))],
            },
            MessageBlock {
                header: Some("default".into()),
                header_style: Style::default(),
                lines: vec![Line::from(Span::raw("第二轮回答尾部标记"))],
            },
        ];
        let total: usize = blocks.iter().map(|b| block_height(b, 40)).sum();
        assert!(total > 30, "构造内容需超过测试视口");
        let buf = ratatui::backend::TestBackend::new(40, 10);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        // 滚到接近底部：最后一个块的尾部标记必须可见。
        let offset = total.saturating_sub(10);
        terminal
            .draw(|f| {
                let area = f.area();
                render_blocks(f, area, &blocks, offset);
            })
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        // TestBackend 中宽字符按 cell 拆开（"第 二 轮"），去空格后断言。
        let flat: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            flat.contains("第二轮回答尾部标记"),
            "offset={offset} total={total} 时底部块应可见\n{content}"
        );
        // 顶部块不应出现在视口（被滚出）。
        assert!(!flat.contains("第一轮问题"), "顶部块已被滚出");
    }
}

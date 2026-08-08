//! 对话消息流渲染：从消息树生成渲染行（折叠/选中/diff 高亮/pending/echo）。
//!
//! 渲染 = 树渲染 → pending → 命令反馈 echo；样式全部经 [`Theme`]。

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::focus::HelpOverlay;
use unicode_width::UnicodeWidthChar;

use crate::app::state::{AppState, DisplayMode};
use crate::i18n::{Key, Tr};
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
/// 验证行文案按 `tr` 语言取词表。
pub fn segment_plain_text(seg: &Segment, tr: Tr) -> String {
    match seg {
        Segment::Reasoning { text } => text.clone(),
        Segment::Text { text } => text.clone(),
        Segment::ToolCall {
            name,
            arguments,
            result,
            ..
        } => match result {
            Some(r) => format!("⏺ {name}({arguments})\n  ⎿  {r}"),
            None => format!("⏺ {name}({arguments})"),
        },
        Segment::Verification {
            command,
            passed,
            summary,
        } => {
            let mark = if *passed { "✓" } else { "✗" };
            if *passed || summary.is_empty() {
                tr.t_args(
                    Key::VerificationLabel,
                    &[("mark", mark), ("command", command)],
                )
            } else {
                tr.t_args(
                    Key::VerificationWithSummary,
                    &[("mark", mark), ("command", command), ("summary", summary)],
                )
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
fn folded_summary(seg: &Segment, tr: Tr) -> String {
    match seg {
        Segment::Reasoning { text } => {
            // 折叠摘要带首句预览（去换行、截断 40 字符），让用户不展开也知道
            // 推理在讲什么——只显示字符数对长推理几乎无信息量。
            let first = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let preview: String = first.chars().take(40).collect();
            let preview = if first.chars().count() > 40 {
                format!("{preview}…")
            } else {
                preview
            };
            if preview.is_empty() {
                tr.t_args(Key::FoldedReasoning, &[("n", &seg.char_len().to_string())])
            } else {
                tr.t_args(
                    Key::FoldedReasoningPreview,
                    &[("n", &seg.char_len().to_string()), ("preview", &preview)],
                )
            }
        }
        Segment::ToolCall { name, .. } => tr.t_args(Key::FoldedTool, &[("name", name)]),
        _ => tr.t(Key::FoldedGeneric).to_string(),
    }
}

/// 一条消息块（用户回合 / agent 回合 / 系统反馈）。
/// Claude Code 风格：无边框无角色头，归属靠 `❯`（用户）/`⏺`（agent）标记。
pub struct MessageBlock {
    /// 内容行（含 ⏺/⎿ 前缀、折叠摘要、选中高亮）。
    pub lines: Vec<Line<'static>>,
}

/// 给行组首行加标记前缀、续行加缩进（对齐到标记后的正文列）。
fn prefix_lines(
    lines: &mut [Line<'static>],
    first: &'static str,
    cont: &'static str,
    style: Style,
) {
    for (i, line) in lines.iter_mut().enumerate() {
        let prefix = if i == 0 { first } else { cont };
        let mut spans = vec![Span::styled(prefix, style)];
        spans.append(&mut line.spans);
        *line = Line::from(spans);
    }
}

/// agent 正文行组：首行 `⏺ `（accent），续行缩进 2 列（Claude Code 风格）。
fn agent_marked_lines(text: &str, text_style: Style, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = span_lines(vec![Span::styled(text.to_string(), text_style)]);
    prefix_lines(&mut lines, "⏺ ", "  ", Style::default().fg(theme.accent));
    lines
}

/// 运行态随机动词：词表来自 i18n（`|` 分隔），每 4s 轮转——
/// 同一轮次内稳定（逐帧随机会闪烁），长跑也有变化感（Claude Code 风格）。
fn thinking_verb(tr: Tr, elapsed: std::time::Duration) -> &'static str {
    let verbs: Vec<&'static str> = tr.t(Key::ThinkingVerbs).split('|').collect();
    let idx = (elapsed.as_secs() / 4) as usize % verbs.len().max(1);
    verbs.get(idx).copied().unwrap_or("Thinking")
}

/// 把会话消息树按「回合」分组成消息块（Claude Code 风格：无边框无角色头，
/// `❯` 标用户输入、`⏺` 标 agent 输出、`  ⎿  ` 缩进树形展示工具结果）：
/// 每回合一个用户块 + 一个 agent 块，回合间空行分隔。
pub fn build_conversation_blocks(app: &AppState, theme: &Theme) -> Vec<MessageBlock> {
    let mut blocks: Vec<MessageBlock> = Vec::new();
    // 首次启动（还没有任何回合）显示欢迎区，替代“空面板 + 加载中噪声”。
    // /help 浮层打开时不显示欢迎区：浮层锚定在输入框上方，但欢迎区仍占
    // 对话区顶部，两者同屏显得拥挤（C1）。
    if app.conversation.turn_count() == 0 && !app.running && app.help_overlay.is_none() {
        blocks.push(welcome_block(app, theme));
    }
    let mut last_turn: Option<u64> = None;
    let mut agent_lines: Vec<Line<'static>> = Vec::new();

    let flush_agent = |blocks: &mut Vec<MessageBlock>, lines: &mut Vec<Line<'static>>| {
        if !lines.is_empty() {
            blocks.push(MessageBlock {
                lines: std::mem::take(lines),
            });
        }
    };

    for (seg_id, seg) in app.conversation.iter_segments() {
        let (turn_id, _) = seg_id;
        if last_turn != Some(turn_id) {
            // 回合切换：落盘上一个 agent 块，开新用户块。
            flush_agent(&mut blocks, &mut agent_lines);
            last_turn = Some(turn_id);
            if let Some(user_text) = app.conversation.user_text_of(turn_id) {
                let mut lines: Vec<Line<'static>> = Vec::new();
                // 回合间空行分隔（首个块前不加）。
                if !blocks.is_empty() {
                    lines.push(Line::default());
                }
                if app.display_mode == DisplayMode::Raw {
                    lines.extend(span_lines(vec![Span::styled(
                        format!("[user] {user_text}"),
                        theme.user,
                    )]));
                } else {
                    let mut body =
                        span_lines(vec![Span::styled(user_text.to_string(), theme.user)]);
                    prefix_lines(&mut body, "❯ ", "  ", theme.system);
                    lines.extend(body);
                }
                blocks.push(MessageBlock { lines });
            }
        }
        let kind = seg.line_kind();
        // Lite 模式隐藏推理。
        if app.display_mode == DisplayMode::Lite && kind == LineKind::Reasoning {
            continue;
        }
        let folded = app.is_folded(seg_id, kind);
        let mut lines = if folded {
            let summary = folded_summary(seg, app.tr);
            let mut folded_lines = span_lines(vec![Span::styled(summary, theme.system)]);
            if kind == LineKind::Tool {
                prefix_lines(&mut folded_lines, "⏺ ", "  ", theme.system);
            }
            folded_lines
        } else {
            segment_lines(seg, app.display_mode, app.tr, theme)
        };
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
        if app.display_mode == DisplayMode::Raw {
            agent_lines.extend(span_lines(vec![Span::styled(
                format!("[agent] {}", app.conversation.pending_text()),
                theme.style_for(LineKind::Agent),
            )]));
        } else {
            agent_lines.extend(agent_marked_lines(
                app.conversation.pending_text(),
                theme.style_for(LineKind::Agent),
                theme,
            ));
        }
    }
    // 等待 agent 首批 delta：在对话区（agent 位置）显示转圈 + 随机动词
    // + 已耗时间（Claude Code 风格），而不是只有输入框里的“等待响应”。
    if app.running
        && app.conversation.current().is_some()
        && app.conversation.pending_reasoning().is_empty()
        && app.conversation.pending_text().is_empty()
    {
        let elapsed = app.run_started_at.map(|t| t.elapsed()).unwrap_or_default();
        let frame = spinner_frame(elapsed);
        let verb = thinking_verb(app.tr, elapsed);
        agent_lines.push(Line::from(Span::styled(
            app.tr.t_args(
                Key::ThinkingWait,
                &[
                    ("frame", &frame.to_string()),
                    ("verb", verb),
                    ("secs", &elapsed.as_secs().to_string()),
                ],
            ),
            theme.system,
        )));
    }
    if !agent_lines.is_empty() {
        blocks.push(MessageBlock {
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
            lines: span_lines(vec![Span::styled(text, theme.style_for(ui.kind))]),
        });
    }
    blocks
}

/// 首次启动欢迎区：Claude Code 风格的简洁文字块（logo + 副标题 + 关键提示
/// + 工作目录），无圆角卡片边框，替代“空空如也 + 一堆加载提示”的初始页面。
fn welcome_block(app: &AppState, theme: &Theme) -> MessageBlock {
    let sessions = if app.sessions_loaded {
        app.tr.t_args(
            Key::WelcomeSessionsCount,
            &[("n", &app.saved_sessions.len().to_string())],
        )
    } else {
        app.tr.t(Key::WelcomeSessionsHint).to_string()
    };
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let hint = |s: String| Line::from(Span::styled(s, theme.system));
    let warn = |s: String| {
        Line::from(Span::styled(
            s,
            Style::default()
                .fg(theme
                    .verification_fail
                    .fg
                    .unwrap_or(ratatui::style::Color::Red))
                .add_modifier(Modifier::BOLD),
        ))
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                "⌒ ",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "DeepseekNova",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        hint(app.tr.t(Key::WelcomeSubtitle).to_string()),
        Line::default(),
        hint(app.tr.t(Key::WelcomeHelp).to_string()),
        hint(app.tr.t(Key::WelcomeTips).to_string()),
        hint(sessions),
        hint(app.tr.t_args(Key::WelcomeCwd, &[("path", &cwd)])),
    ];
    // 配置状态警示：CLI 入口已拦截未配置场景，这里作为库级嵌入/边界兜底，
    // 让「打开就能看到缺什么、怎么补」成为欢迎块的默认行为。
    if !app.provider_configured {
        lines.push(warn(app.tr.t(Key::WelcomeNoProvider).to_string()));
    } else if !app.api_key_configured {
        lines.push(warn(app.tr.t(Key::WelcomeNoApiKey).to_string()));
    }
    MessageBlock { lines }
}

/// 消息块在给定面板宽度下的物理高度（内容 wrap 行数）。
///
/// 高度估算必须与 [`render_blocks`] 的渲染宽度一致（无边框全宽 Paragraph）：
/// 若估算按 `width - 2` 折行，内容宽度落在 `(w-2, w]` 区间时估算高 1 行，
/// 块与块之间会留下实际空白行——视觉上就是「两轮对话间隔过大」。
pub fn block_height(block: &MessageBlock, width: usize) -> usize {
    estimate_wrapped_lines(&block.lines, width.max(1)).max(1)
}

/// 对话区叠放渲染：按 `offset`（物理行）裁剪可见窗口，逐块绘制。
/// 每块一个独立 Paragraph（无边框，内容行自带 ❯/⏺ 前缀），块内用 `scroll`
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
        let paragraph = Paragraph::new(block.lines.clone())
            .wrap(Wrap { trim: false })
            .scroll((skip_in_block as u16, 0));
        f.render_widget(paragraph, rect);
        y += bh;
    }
}

/// 段 → 显示行组（展开态，Claude Code 风格 ⏺/⎿ 标记；Raw 模式退化为
/// `[kind]` 纯文本前缀，便于复制与解析）。验证行文案按 `tr` 语言取词表。
fn segment_lines(seg: &Segment, mode: DisplayMode, tr: Tr, theme: &Theme) -> Vec<Line<'static>> {
    let raw_mode = mode == DisplayMode::Raw;
    let plain = |kind: LineKind, text: String, style: Style| -> Vec<Line<'static>> {
        let text = if raw_mode {
            format!("[{}] {text}", kind_tag(kind))
        } else {
            text
        };
        span_lines(vec![Span::styled(text, style)])
    };
    match seg {
        Segment::Reasoning { text } => plain(LineKind::Reasoning, text.clone(), theme.reasoning),
        Segment::Text { text } => {
            if raw_mode {
                plain(LineKind::Agent, text.clone(), theme.agent)
            } else {
                agent_marked_lines(text, theme.agent, theme)
            }
        }
        Segment::ToolCall {
            name,
            arguments,
            result,
            status,
            ..
        } => {
            use crate::model::conversation::ToolStatus;
            if raw_mode {
                let mut lines = plain(LineKind::Tool, format!("{name}({arguments})"), theme.tool);
                if let Some(r) = result {
                    lines.extend(plain(LineKind::ToolResult, r.clone(), theme.tool_result));
                }
                return lines;
            }
            // ⏺ 颜色编码状态：运行中=dim、成功=accent、失败=红。
            let dot_style = match status {
                ToolStatus::Running => theme.system,
                ToolStatus::Ok => Style::default().fg(theme.accent),
                ToolStatus::Failed => theme.verification_fail,
            };
            let mut lines = vec![Line::from(vec![
                Span::styled("⏺ ", dot_style),
                Span::styled(name.clone(), Style::default()),
                Span::styled(format!("({arguments})"), theme.tool),
            ])];
            if let Some(r) = result {
                // diff 行级高亮作用于干净结果文本（不含 UI 前缀）：代码改动
                // 信息（git diff 等）来自工具输出；模型正文不染色——正常聊天
                // 的回答里 `+`/`-` 开头行（如 markdown 列表）不应被误判为 diff。
                let styled = theme.diff_spans(r, theme.tool_result);
                let mut body = span_lines(styled);
                prefix_lines(&mut body, "  ⎿  ", "     ", theme.system);
                lines.extend(body);
            }
            lines
        }
        Segment::Verification {
            command,
            passed,
            summary,
        } => {
            let mark = if *passed { "✓" } else { "✗" };
            let text = if *passed || summary.is_empty() {
                tr.t_args(
                    Key::VerificationLabel,
                    &[("mark", mark), ("command", command)],
                )
            } else {
                tr.t_args(
                    Key::VerificationWithSummary,
                    &[("mark", mark), ("command", command), ("summary", summary)],
                )
            };
            let kind = LineKind::Verification { passed: *passed };
            plain(kind, text, theme.style_for(kind))
        }
        Segment::System { kind, text } => {
            let line_kind = match kind {
                crate::model::conversation::SystemKind::Paused => LineKind::Paused,
                crate::model::conversation::SystemKind::Error => LineKind::Error,
                crate::model::conversation::SystemKind::Approval
                | crate::model::conversation::SystemKind::Info => LineKind::System,
            };
            plain(line_kind, text.clone(), theme.style_for(line_kind))
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

/// 输入相关浮层（@ 补全、/help）锚定到**状态行上方**（输入区正上方），
/// 与斜杠命令浮层同一位置——避免居中在对话区中部造成与正文/欢迎卡叠层。
/// `height` 是期望高度，实际钳制在 `[1, status_area.y]` 内，永不溢出到状态行。
fn input_overlay(status_area: Rect, height: u16) -> Rect {
    let h = height.clamp(1, status_area.y.max(1));
    Rect {
        x: status_area.x,
        y: status_area.y.saturating_sub(h),
        width: status_area.width,
        height: h,
    }
}

/// /help 帮助浮层：可滚动面板（Esc/q 关闭，j/k、↑/↓、PageUp/Down 滚动）。
/// 先用 `Clear` 擦除底下内容，避免与对话文本交叉（@ 补全浮层曾出现
/// 边框与正文混叠的伪影）。标题与翻页器文案按 `tr` 语言取词表。
fn render_help_overlay(help: &HelpOverlay, theme: &Theme, tr: Tr, f: &mut Frame, area: Rect) {
    if area.width < 20 || area.height < 4 {
        return;
    }
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Line::from(Span::styled(tr.t(Key::HelpTitle), theme.title)));
    let inner = block.inner(area);
    let inner_h = inner.height as usize;
    let start = help.scroll.min(help.lines.len().saturating_sub(inner_h));
    let end = (start + inner_h).min(help.lines.len());
    let lines: Vec<Line> = help.lines[start..end]
        .iter()
        .map(|l| {
            Line::from(Span::styled(
                if l.is_empty() {
                    " ".to_string()
                } else {
                    l.clone()
                },
                theme.system,
            ))
        })
        .collect();
    f.render_widget(block, area);
    let pager = if help.lines.len() > inner_h {
        tr.t_args(
            Key::HelpPager,
            &[
                ("start", &(start + 1).to_string()),
                ("end", &end.to_string()),
                ("total", &help.lines.len().to_string()),
            ],
        )
    } else {
        tr.t(Key::HelpPagerShort).to_string()
    };
    let body = Paragraph::new(lines);
    f.render_widget(body, inner);
    let hint = Paragraph::new(Span::styled(
        pager,
        Style::default().add_modifier(Modifier::DIM),
    ));
    f.render_widget(
        hint,
        Rect {
            x: inner.x,
            y: inner.y + inner.height.saturating_sub(1),
            width: inner.width,
            height: 1,
        },
    );
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

        // 消息块布局：无边框无角色头（Claude Code 风格），归属靠 ❯/⏺ 标记。
        // 先估算总物理高度再钳制滚动。
        let blocks = crate::render::message::build_conversation_blocks(self, &theme);
        let pane_width = conv_area.width as usize;
        self.rendered_lines = blocks
            .iter()
            .map(|b| crate::render::message::block_height(b, pane_width))
            .sum();
        let viewport = conv_area.height as usize;
        self.clamp_scroll(viewport.max(1));
        crate::render::message::render_blocks(f, conv_area, &blocks, self.scroll_offset);

        // 滚动位置指示：用户上滚（非贴底）时在对话区右上角画 ▍N%
        //（Claude Code 同款），比例 = scroll_offset / 总物理行数。
        if !self.auto_scroll && self.rendered_lines > viewport {
            let pct = self
                .scroll_offset
                .saturating_mul(100)
                .checked_div(self.rendered_lines)
                .unwrap_or(0)
                .min(100);
            let marker = format!("▍{pct}%");
            let marker_w = marker.chars().count() as u16 + 1;
            f.render_widget(
                Paragraph::new(Span::styled(marker, theme.system)),
                Rect {
                    x: conv_area.right().saturating_sub(marker_w),
                    y: conv_area.y,
                    width: marker_w,
                    height: 1,
                },
            );
        }

        // ── 状态行 ────────────────────────────────────────
        let status = Paragraph::new(crate::render::status::fit_status_line(
            self,
            &theme,
            status_area.width as usize,
        ));
        f.render_widget(status, status_area);

        // 临时命令反馈（状态变更类命令）：画在状态行上方，超时自动消失，
        // 不进入对话面板的永久 echo 通道。
        if let Some((text, _)) = &self.notice {
            let lines: Vec<Line> = text
                .lines()
                .map(|l| Line::from(Span::styled(l.to_string(), theme.system)))
                .collect();
            let height = lines.len().min(status_area.y.max(1) as usize).max(1);
            let notice_area = Rect {
                x: status_area.x,
                y: status_area.y.saturating_sub(height as u16),
                width: status_area.width,
                height: height as u16,
            };
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), notice_area);
        }

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
            crate::render::status::hint_for(self.focus, self.tr),
            Style::default().add_modifier(Modifier::DIM),
        ));
        f.render_widget(hint, hint_area);

        // ── 浮层（信任确认 / 审批居中 / @ 补全与 /help 锚定输入框上方）────
        if let Some(trust) = &self.trust_prompt {
            crate::render::trust::render_trust_prompt(
                trust,
                self.tr,
                &theme,
                f,
                overlay(conv_area),
            );
        } else if let Some(approval) = &self.pending_approval {
            crate::render::approval::render_approval(
                approval,
                self.permission_mode,
                self.tr,
                &theme,
                f,
                overlay(conv_area),
            );
        } else if self.completion.is_some() {
            // 高度随候选数收缩（最多 10 候选 + 2 边框），候选少不占整屏。
            let n = self
                .completion
                .as_ref()
                .map(|c| c.candidates.len().min(10))
                .unwrap_or(3);
            crate::render::input::render_completion(
                self,
                &theme,
                f,
                input_overlay(status_area, (n + 2) as u16),
            );
        } else if let Some(help) = &self.help_overlay {
            render_help_overlay(help, &theme, self.tr, f, input_overlay(status_area, 20));
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
        let tr = Tr::new(crate::i18n::Lang::En);
        assert_eq!(
            segment_plain_text(&Segment::Text { text: "hi".into() }, tr),
            "hi"
        );
        assert_eq!(
            segment_plain_text(&Segment::Reasoning { text: "r".into() }, tr),
            "r"
        );
        let tool = Segment::ToolCall {
            call_id: "1".into(),
            name: "grep".into(),
            arguments: "x".into(),
            result: Some("hit".into()),
            status: ToolStatus::Ok,
        };
        assert_eq!(segment_plain_text(&tool, tr), "⏺ grep(x)\n  ⎿  hit");
        // 验证行走词表：英文默认 / 中文可选。
        let ver = Segment::Verification {
            command: "cargo check".into(),
            passed: true,
            summary: "ok".into(),
        };
        assert_eq!(segment_plain_text(&ver, tr), "✓ Verify: cargo check");
        assert_eq!(
            segment_plain_text(&ver, Tr::new(crate::i18n::Lang::Zh)),
            "✓ 验证: cargo check"
        );
    }

    #[test]
    fn blocks_include_user_agent_and_echo() {
        let mut app = AppState {
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        app.conversation.begin_turn("问题".into());
        app.apply_run_event(RunEvent::TextDelta("答案".into()));
        app.apply_run_event(RunEvent::Done(done_output("")));
        app.echo_line(LineKind::System, "已处理");
        let theme = Theme::default();
        let blocks = build_conversation_blocks(&app, &theme);
        assert_eq!(blocks.len(), 3, "用户块 + agent 块 + echo 块");
        // Claude Code 风格：用户块 `❯ ` 前缀、agent 块 `⏺ ` 前缀，无角色头。
        let user_texts = block_texts(&blocks[0]);
        assert!(
            user_texts.iter().any(|t| t == "❯ "),
            "用户块 ❯ 前缀: {user_texts:?}"
        );
        assert!(user_texts.iter().any(|t| t.contains("问题")));
        let agent_texts = block_texts(&blocks[1]);
        assert!(
            agent_texts.iter().any(|t| t == "⏺ "),
            "agent 块 ⏺ 前缀: {agent_texts:?}"
        );
        assert!(agent_texts.iter().any(|t| t.contains("答案")));
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
        let mut app = AppState {
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        app.conversation.begin_turn("q".into());
        let long = format!("推理内容 {}", "x".repeat(80));
        app.apply_run_event(RunEvent::ReasoningDelta {
            text: long.clone(),
            signature: None,
        });
        app.apply_run_event(RunEvent::TextDelta("ans".into()));
        app.apply_run_event(RunEvent::Done(done_output("")));
        let theme = Theme::default();
        let blocks = build_conversation_blocks(&app, &theme);
        let texts: String = blocks.iter().flat_map(block_texts).collect();
        assert!(texts.contains("推理 ▸ 折叠"), "推理默认折叠摘要");
        assert!(
            texts.contains("「推理内容 xxxx"),
            "折叠摘要带首句预览: {texts}"
        );
        // 预览截断在 40 字符内，不显示长正文的尾部。
        assert!(!texts.contains(&"x".repeat(80)), "折叠态不显示完整正文");
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
        // 工具调用默认展开（Claude Code 风格），diff 染色直接可见。
        let mut app = AppState::default();
        app.conversation.begin_turn("q".into());
        app.apply_run_event(RunEvent::ToolCallStart {
            id: "1".into(),
            name: "git".into(),
        });
        app.apply_run_event(RunEvent::ToolResult {
            call_id: "1".into(),
            result: "+fn new() {}\n-fn old() {}".into(),
        });
        app.apply_run_event(RunEvent::Done(done_output("")));
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
            "工具结果 + 行按 diff 染色（⎿ 前缀在独立 span，不参与判定）"
        );
        // 结果树前缀：首行 `  ⎿  `、续行缩进对齐。
        let texts = block_texts(tool_block);
        assert!(texts.iter().any(|t| t == "  ⎿  "), "结果 ⎿ 前缀: {texts:?}");
    }

    #[test]
    fn tool_call_expands_by_default() {
        // 工具调用默认展开（Claude Code 风格）：⏺ 头 + ⎿ 结果直接可见；
        // 显式折叠后退化为摘要行。
        let mut app = AppState {
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        let id = app.conversation.begin_turn("q".into());
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
        assert!(texts.contains("⏺ grep("), "工具默认展开为 ⏺ 头: {texts}");
        assert!(texts.contains("  ⎿  "), "结果带 ⎿ 前缀: {texts}");
        assert!(texts.contains(&"a".repeat(400)), "展开态显示截断结果");

        // 显式折叠后回退摘要行（⏺ + 折叠摘要）。
        app.fold.insert((id, 0), true);
        let blocks = build_conversation_blocks(&app, &theme);
        let texts: String = blocks.iter().flat_map(block_texts).collect();
        assert!(
            texts.contains("工具 ▸ grep 已折叠"),
            "显式折叠摘要: {texts}"
        );
        assert!(!texts.contains(&"a".repeat(500)), "折叠态不显示结果");
    }

    #[test]
    fn welcome_block_shown_before_first_turn() {
        let theme = Theme::default();
        let app = AppState::default();
        let blocks = build_conversation_blocks(&app, &theme);
        assert!(!blocks.is_empty());
        let texts: String = blocks.iter().flat_map(block_texts).collect();
        assert!(texts.contains("DeepseekNova"), "欢迎区标题: {texts}");
        assert!(texts.contains('⌒'), "欢迎区圆顶字形: {texts}");
        assert!(texts.contains("/help"), "欢迎区命令提示: {texts}");
        // Claude Code 式简洁欢迎区：无圆角卡片边框。
        assert!(!texts.contains('╭'), "欢迎区不再带圆角边框: {texts}");

        let mut app = AppState::default();
        app.conversation.begin_turn("你好".into());
        let blocks = build_conversation_blocks(&app, &theme);
        let texts: String = blocks.iter().flat_map(block_texts).collect();
        assert!(!texts.contains("DeepseekNova"), "首轮开始后欢迎区消失");
    }

    #[test]
    fn welcome_shows_setup_warning_when_provider_unconfigured() {
        let theme = Theme::default();
        // 未配置 provider：欢迎块出现红色 setup 引导。
        let app = AppState {
            provider_configured: false,
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        let blocks = build_conversation_blocks(&app, &theme);
        let texts: String = blocks.iter().flat_map(block_texts).collect();
        assert!(texts.contains("setup"), "未配置提示含 setup: {texts}");
        // 配置了 provider 但 key 缺失：提示 API key。
        let app = AppState {
            provider_configured: true,
            api_key_configured: false,
            tr: Tr::new(crate::i18n::Lang::En),
            ..Default::default()
        };
        let blocks = build_conversation_blocks(&app, &theme);
        let texts: String = blocks.iter().flat_map(block_texts).collect();
        assert!(texts.contains("API key"), "缺 key 提示: {texts}");
        // 全部就绪：不出现任何警示行。
        let app = AppState {
            provider_configured: true,
            api_key_configured: true,
            tr: Tr::new(crate::i18n::Lang::En),
            ..Default::default()
        };
        let blocks = build_conversation_blocks(&app, &theme);
        let texts: String = blocks.iter().flat_map(block_texts).collect();
        assert!(!texts.contains('⚠'), "配置就绪时无警示: {texts}");
    }

    #[test]
    fn waiting_spinner_shown_in_conversation_while_running() {
        let theme = Theme::default();
        let mut app = AppState {
            running: true,
            run_started_at: Some(std::time::Instant::now() - std::time::Duration::from_millis(350)),
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        app.conversation.begin_turn("q".into());
        let blocks = build_conversation_blocks(&app, &theme);
        let texts: String = blocks.iter().flat_map(block_texts).collect();
        // Claude Code 风格：spinner + 随机动词 + 已耗时间（350ms → 动词表首项「思考」，0s）。
        assert!(texts.contains("思考…（0s"), "对话区等待提示: {texts}");
        assert!(texts.contains('⠸'), "350ms 推进到第 3 帧: {texts}");

        // 首批文本到达后，等待提示消失、正文出现。
        app.apply_run_event(RunEvent::TextDelta("hi".into()));
        let blocks = build_conversation_blocks(&app, &theme);
        let texts: String = blocks.iter().flat_map(block_texts).collect();
        assert!(!texts.contains("思考…"), "有内容后不再显示等待");
        assert!(texts.contains("hi"));
        assert!(texts.contains("⏺"), "流式正文带 ⏺ 前缀: {texts}");
    }

    #[test]
    fn thinking_verb_rotates_every_four_seconds() {
        let tr = Tr::new(crate::i18n::Lang::En);
        assert_eq!(thinking_verb(tr, std::time::Duration::ZERO), "Thinking");
        assert_eq!(
            thinking_verb(tr, std::time::Duration::from_secs(4)),
            "Pondering",
            "4s 轮转到下一个动词"
        );
        assert_eq!(
            thinking_verb(tr, std::time::Duration::from_millis(3999)),
            "Thinking",
            "4s 内动词稳定"
        );
        let tr_zh = Tr::new(crate::i18n::Lang::Zh);
        assert_eq!(thinking_verb(tr_zh, std::time::Duration::ZERO), "思考");
        // 词表长度取模循环，不越界。
        let _ = thinking_verb(tr, std::time::Duration::from_secs(3600));
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
    fn block_height_counts_content_only() {
        // 无角色头（Claude Code 风格）：块高 = 内容行数。
        let block = MessageBlock {
            lines: vec![Line::from(Span::raw("abc"))],
        };
        assert_eq!(block_height(&block, 10), 1);
        let two = MessageBlock {
            lines: vec![Line::from(Span::raw("abc")), Line::from(Span::raw("def"))],
        };
        assert_eq!(block_height(&two, 10), 2);
    }

    #[test]
    fn block_height_matches_render_width_no_phantom_gap() {
        // 回归：块间不得出现「幽灵间距」。此前估算按 width-2 折行而渲染
        // 是全宽，内容宽度落在 (w-2, w] 区间时会高估 1 行，导致相邻消息
        // 块之间留下空白行（两轮对话间隔过大的观感来源）。
        // 宽 10 面板 + 9 列内容：按 10 折行 = 1 行，按 8 折行 = 2 行。
        let block = MessageBlock {
            lines: vec![Line::from(Span::raw("123456789"))],
        };
        assert_eq!(block_height(&block, 10), 1, "9 列内容宽 10 面板 1 行");
        // 与 render_blocks 的裁剪一致性：多块的总高应等于逐块和。
        let blocks = [
            MessageBlock {
                lines: vec![Line::from(Span::raw("123456789"))],
            },
            MessageBlock {
                lines: vec![Line::from(Span::raw("123456789"))],
            },
            MessageBlock {
                lines: vec![Line::from(Span::raw("x"))],
            },
        ];
        let total: usize = blocks.iter().map(|b| block_height(b, 10)).sum();
        assert_eq!(total, 1 + 1 + 1, "估算总高不得凭空多出行");
    }

    #[test]
    fn render_blocks_scrolls_to_later_content() {
        // 回归：长会话滚动（offset>0）必须能看到后续块的内容，
        // 否则"看不到新消息"。用 TestBackend 实测 ratatui 渲染结果。
        let blocks = vec![
            MessageBlock {
                lines: vec![Line::from(Span::raw("❯ 第一轮问题"))],
            },
            MessageBlock {
                lines: vec![Line::from(Span::raw(
                    "⏺ ".to_string() + &"第一轮回答".repeat(100),
                ))],
            },
            MessageBlock {
                lines: vec![Line::from(Span::raw("❯ 第二轮问题"))],
            },
            MessageBlock {
                lines: vec![Line::from(Span::raw("⏺ 第二轮回答尾部标记"))],
            },
        ];
        let total: usize = blocks.iter().map(|b| block_height(b, 40)).sum();
        assert!(total > 10, "构造内容需超过测试视口");
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

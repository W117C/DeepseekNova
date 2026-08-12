//! 输入区渲染（grok build prompt_widget 对齐）：`┃` 强调竖线前缀 + 圆角边框 +
//! 会话标题内联在边框标题位 + md 着色 + 可见光标 + 空输入灰色 placeholder +
//! 多行模式右侧标记。
//! 斜杠命令行内候选、历史搜索、Paste Chip 浮层均在输入区上方就地展开。

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::state::{AppState, PasteChipKind};
use crate::i18n::Key;
use crate::input::editor::input_view;
use crate::input::md_highlight::md_spans;
use crate::theme::Theme;

/// spinner 帧字符序列（Braille 转圈，10 帧）。
const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// 等待动画帧：按经过毫秒推导帧索引（每 100ms 换一帧，一轮 1s）。
/// 纯时间推导，不依赖事件循环节奏。
pub fn spinner_frame(elapsed: std::time::Duration) -> char {
    let idx = (elapsed.as_millis() / 100) as usize % SPINNER_FRAMES.len();
    SPINNER_FRAMES[idx]
}

/// 输入区渲染（grok build prompt_widget 对齐）。
///
/// chrome = 圆角边框 + 会话标题（或 model 标签）内联在边框标题位 + `┃` 强调
/// 竖线前缀（bash 模式 `! ` 黄色）+ 空输入灰色 placeholder + 多行模式右侧标记。
pub fn render_input(app: &AppState, theme: &Theme, f: &mut Frame, area: Rect) {
    let input_style = if app.running {
        theme.system
    } else {
        Style::default()
    };
    // 高度自适应：4 行区（主布局）= 边框 2 + 输入 1（多行滚动）+ info 行 1
    // （grok PromptInfo）；3 行区（欢迎屏 prompt，grok welcome PROMPT_HEIGHT=3）
    // 无 info 行。info 行在底部渲染，见下方分支。
    let has_info_row = area.height >= 4;
    let pane_width = area.width.saturating_sub(2) as usize;
    let pane_rows = area.height.saturating_sub(if has_info_row { 3 } else { 2 }) as usize;
    let view = input_view(
        &app.input.text,
        app.input.cursor,
        pane_width.max(1),
        pane_rows.max(1),
    );
    // 首行前缀：bash 模式 `! `（黄色，grok prefix_override 对齐）；
    // 否则 `❯ `（grok DEFAULT_PROMPT，accent_user 强调）。
    // 其余行（多行输入）无前缀。
    let prefix_span = if app.bash_mode {
        Span::styled(
            "! ",
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            "❯ ",
            Style::default()
                .fg(theme.accent_user)
                .add_modifier(Modifier::BOLD),
        )
    };
    let input_lines: Vec<Line> = if app.running {
        // 运行中：对话面板 agent 位置已有转圈动画，输入区不再重复 spinner，
        // 改为静态提示（避免"双转圈"冗余）。spinner_frame 仍供消息面板使用。
        vec![Line::from(vec![
            prefix_span,
            Span::styled(app.tr.t(Key::InputRunning), theme.system),
        ])]
    } else if app.input.text.is_empty() {
        // 空输入：灰色 placeholder（PromptPlaceholder 键文案），只做视觉提示，
        // 不写入输入流——用户开始键入后即被真实文本替换。
        vec![Line::from(vec![
            prefix_span,
            Span::styled(app.tr.t(Key::PromptPlaceholder), theme.system),
        ])]
    } else {
        view.rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let mut spans = Vec::new();
                if i == 0 {
                    spans.push(prefix_span.clone());
                }
                spans.extend(md_spans(row, theme));
                Line::from(spans)
            })
            .collect()
    };
    // 边框标题位：仅会话标题（grok 把 session title 内联在边框位）。
    // model 名由 info 行（4 行区）或欢迎屏 top_bar 展示，避免重复显示。
    let title_text = app.session_title.clone().unwrap_or_default();
    // grok 对齐：圆角边框（`╭─╮│╰─╯`，prompt_widget border box）+
    // prompt_border_active 边框色 + accent_user 粗体标题（session title）。
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::default().fg(if app.running {
            theme.prompt_border
        } else {
            theme.prompt_border_active
        }))
        .title(Line::from(Span::styled(
            title_text,
            Style::default()
                .fg(theme.accent_user)
                .add_modifier(Modifier::BOLD),
        )));
    let block = if app.multiline_mode {
        // 多行模式指示：边框右侧标题（MultilineOn 文案，提示 Enter 换行语义）。
        block.title(
            Line::from(Span::styled(app.tr.t(Key::MultilineOn), theme.system)).right_aligned(),
        )
    } else {
        block
    };
    let input_widget = Paragraph::new(input_lines)
        .style(input_style)
        .scroll((view.scroll_row as u16, 0))
        .block(block);
    f.render_widget(input_widget, area);

    // grok 对齐：prompt info 行（PromptInfo）——边框内底部一行：
    // 左侧 model 标签（accent 加粗）· flags（multiline 时显示），
    // 右侧 usage 警告（ctx 高占用时显示，critical 用 warning 色）。
    // 仅 4 行输入区渲染（欢迎屏 3 行 prompt 不渲染，grok 同款）。
    let inner = ratatui::widgets::Block::default()
        .borders(Borders::ALL)
        .inner(area);
    if has_info_row && inner.height >= 1 {
        let mut info_spans: Vec<Span<'static>> = Vec::new();
        if !app.model_label.is_empty() {
            info_spans.push(Span::styled(
                app.model_label.clone(),
                Style::default()
                    .fg(theme.accent_user)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if app.multiline_mode {
            if !info_spans.is_empty() {
                info_spans.push(Span::styled(" · ", Style::default().fg(theme.gray)));
            }
            info_spans.push(Span::styled(
                app.tr.t(Key::MultilineOn),
                Style::default().fg(theme.gray),
            ));
        }
        // usage 警告右对齐（grok PromptInfo：model 左、usage_warning 右）：
        // ctx 占用 ≥80% 时显示（critical=95%+ 用 warning 色）。
        let mut usage_span: Option<Span<'static>> = None;
        if let Some((used, window)) = app.context_usage {
            let pct = used.saturating_mul(100).checked_div(window).unwrap_or(0);
            if pct >= 80 {
                let critical = pct >= 95;
                usage_span = Some(Span::styled(
                    format!(" {}% usage left", 100 - pct),
                    Style::default().fg(if critical {
                        theme.warning
                    } else {
                        theme.text_secondary
                    }),
                ));
            }
        }
        if !info_spans.is_empty() || usage_span.is_some() {
            let mut line = Line::from(info_spans);
            if let Some(usage) = usage_span {
                // 右侧对齐：左侧内容宽度 + 右侧内容宽度 < 面板宽时补空格，
                // 把 usage 推到行尾（grok PromptInfo 右对齐语义）。
                let left_w = line.width();
                let usage_w = usage.width();
                let pad = inner.width.saturating_sub((left_w + usage_w) as u16);
                line.spans
                    .push(Span::styled(" ".repeat(pad as usize), Style::default()));
                line.spans.push(usage);
            }
            f.render_widget(
                Paragraph::new(line),
                Rect {
                    x: inner.x,
                    y: inner.y + inner.height.saturating_sub(1),
                    width: inner.width,
                    height: 1,
                },
            );
        }
    }

    // grok 对齐：Paste Chip 行与历史搜索浮层随输入区一起渲染。
    // 主布局里状态行（1 行）夹在对话区与输入区之间，浮层锚定到状态行上方
    // （输入区上方 1 行起），与斜杠候选浮层同一位置。
    if !app.paste_chips.is_empty() {
        let chips_area = Rect {
            x: area.x,
            y: area.y.saturating_sub(1 + 3),
            width: area.width,
            height: 3,
        };
        render_paste_chips(app, theme, f, chips_area);
    }
    if app.history_search.is_some() {
        let hs_h = app
            .history_search
            .as_ref()
            .map(|hs| hs.matches.len().min(8) + 2)
            .unwrap_or(3);
        let hs_area = Rect {
            x: area.x,
            y: area.y.saturating_sub(1 + hs_h as u16),
            width: area.width,
            height: hs_h as u16,
        };
        render_history_search(app, theme, f, hs_area);
    }
    // grok 对齐：对话内搜索条（Ctrl+F）——matches 计算已接线，但此前
    // 渲染层没有任何搜索条 UI，用户按 Ctrl+F 后屏幕无反馈（实测看不到）。
    // 在输入区上方渲染搜索条：查询文本 + 命中数 + 导航提示。
    if app.search.is_some() {
        let sb_area = Rect {
            x: area.x,
            y: area.y.saturating_sub(3),
            width: area.width,
            height: 3,
        };
        render_search_bar(app, theme, f, sb_area);
    }
}

/// 对话内搜索条（Ctrl+F，grok scrollback search 对齐）：在输入区上方
/// 渲染搜索条——查询文本 + 命中数（n/m）+ 导航提示。matches 计算由
/// `AppState::recompute_search` 完成；此函数只负责让用户**看到**搜索态。
pub fn render_search_bar(app: &AppState, theme: &Theme, f: &mut Frame, area: Rect) {
    let Some(s) = &app.search else {
        return;
    };
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.prompt_border_active))
        .title(Line::from(Span::styled(
            app.tr.t(Key::SearchPlaceholder),
            Style::default()
                .fg(theme.accent_user)
                .add_modifier(Modifier::BOLD),
        )));
    // 命中导航：`n/m` + 提示（Enter/n 下一个、Shift+Enter/N 上一个、Esc 关闭）。
    let hit = s
        .matches
        .get(s.selected)
        .map(|_| (s.selected + 1).to_string())
        .unwrap_or_else(|| "0".to_string());
    let total = if s.matches.is_empty() {
        "0".to_string()
    } else {
        s.matches.len().to_string()
    };
    let mut spans = vec![
        Span::styled(
            format!("  🔍 {}  ", s.query),
            Style::default().fg(theme.text_primary),
        ),
        Span::styled(
            format!("{hit}/{total}"),
            Style::default().fg(if s.matches.is_empty() {
                theme.warning
            } else {
                theme.accent_user
            }),
        ),
    ];
    if !s.matches.is_empty() {
        spans.push(Span::styled(
            "  Enter/n 下一个 · Shift+Enter/N 上一个 · Esc 关闭",
            Style::default().fg(theme.gray_dim),
        ));
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: inner.height,
        },
    );
}

/// 斜杠命令行内候选渲染（纯 `/` 触发，输入区上方就地展开）。
/// 带边框 + 标题（操作提示）+ `▸` 选中指示，交互感明确：
/// ↑↓/j/k 选择、Enter 执行、Tab 补全、Esc 关闭。
pub fn render_command_hint(app: &AppState, theme: &Theme, f: &mut Frame, area: Rect) {
    let Some(hint) = &app.command_hint else {
        return;
    };
    let n = hint.visible_rows();
    if n == 0 {
        return;
    }
    let title = if hint.arg_options.is_some() {
        Key::CommandHintTitleArg
    } else {
        Key::CommandHintTitleCmd
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Line::from(Span::styled(app.tr.t(title), theme.title)));
    let lines: Vec<Line> = if let Some(opts) = &hint.arg_options {
        // 参数模式：展示该命令的枚举/用法候选（`/fold ` → all|none|reset）。
        let (start, count) = hint_window(opts.len(), hint.selected);
        opts.iter()
            .enumerate()
            .skip(start)
            .take(count)
            .map(|(i, opt)| candidate_row(opt.to_string(), i == hint.selected, theme))
            .collect()
    } else {
        let name_w = hint
            .candidates
            .iter()
            .map(|c| c.name.len())
            .max()
            .unwrap_or(8)
            .min(18);
        let (start, count) = hint_window(hint.candidates.len(), hint.selected);
        hint.candidates
            .iter()
            .enumerate()
            .skip(start)
            .take(count)
            .map(|(i, cmd)| {
                candidate_row(
                    format!("/{:<name_w$}  {}", cmd.name, app.tr.t(*cmd.desc)),
                    i == hint.selected,
                    theme,
                )
            })
            .collect()
    };
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// 候选浮层的可见窗口：候选多于 8 个时跟随选中项滚动，
/// 选中项始终可见（此前固定取前 8 个，选中第 9 个起高亮“消失”，
/// 直到绕回第一项才重新出现——用户实测的“按到底光标不见了”）。
fn hint_window(len: usize, selected: usize) -> (usize, usize) {
    let rows = len.min(8);
    if len <= 8 {
        return (0, rows);
    }
    let start = selected.saturating_sub(3).min(len - 8);
    (start, rows)
}

/// Ctrl+P 命令面板渲染：模态浮层（全命令模糊搜索 + 最近使用排序）。
/// 复用斜杠候选的候选行/窗口样式；标题带当前查询文本。
pub fn render_command_palette(app: &AppState, theme: &Theme, f: &mut Frame, area: Rect) {
    let Some(pal) = &app.command_palette else {
        return;
    };
    if pal.candidates.is_empty() {
        return;
    }
    // 先 Clear：浮层锚定在输入框上方，不擦除会与对话正文叠层。
    f.render_widget(ratatui::widgets::Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Line::from(Span::styled(
            format!("  / {} ", pal.query),
            theme.title,
        )));
    let name_w = pal
        .candidates
        .iter()
        .map(|c| c.name.len())
        .max()
        .unwrap_or(8)
        .min(18);
    let (start, count) = hint_window(pal.candidates.len(), pal.selected);
    let lines: Vec<Line> = pal
        .candidates
        .iter()
        .enumerate()
        .skip(start)
        .take(count)
        .map(|(i, cmd)| {
            candidate_row(
                format!("/{:<name_w$}  {}", cmd.name, app.tr.t(*cmd.desc)),
                i == pal.selected,
                theme,
            )
        })
        .collect();
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// 候选行：选中项 `▸` 前缀 + selection 高亮，未选中项占位对齐。
fn candidate_row(text: String, selected: bool, theme: &Theme) -> Line<'static> {
    let style = if selected {
        theme.selection
    } else {
        theme.system
    };
    let marker = if selected { "▸ " } else { "  " };
    Line::from(Span::styled(format!("{marker}{text}"), style))
}

/// @ 补全浮层渲染（候选列表）。
pub fn render_completion(app: &AppState, theme: &Theme, f: &mut Frame, area: Rect) {
    let Some(comp) = &app.completion else {
        return;
    };
    let candidates: Vec<&String> = comp.candidates.iter().take(10).collect();
    if candidates.is_empty() {
        return;
    }
    // 先 Clear：浮层锚定在输入框上方，不擦除会与对话正文/欢迎卡叠层。
    f.render_widget(ratatui::widgets::Clear, area);
    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(theme.border)
        .title(Line::from(Span::styled(
            app.tr.t(Key::CompletionTitle),
            theme.title,
        )));
    let lines: Vec<Line> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let style = if i == comp.selected {
                theme.selection
            } else {
                theme.system
            };
            Line::from(Span::styled(c.as_str(), style))
        })
        .collect();
    let widget = Paragraph::new(lines).block(block);
    f.render_widget(widget, area);
}

/// 历史搜索浮层渲染（Ctrl+R 打开；输入区上方就地展开）。
///
/// 候选 = `app.history` 中 `history_search.matches` 命中的下标（倒序：最近在
/// 先），选中项复用斜杠候选的 `▸` 指示 + `theme.selection` 高亮。
pub fn render_history_search(app: &AppState, theme: &Theme, f: &mut Frame, area: Rect) {
    let Some(hs) = &app.history_search else {
        return;
    };
    if hs.matches.is_empty() {
        return;
    }
    // 先 Clear：浮层锚定在输入框上方，不擦除会与对话正文叠层。
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Line::from(Span::styled(
            app.tr.t(Key::HistorySearchPlaceholder),
            theme.title,
        )));
    let (start, count) = hint_window(hs.matches.len(), hs.selected);
    let lines: Vec<Line> = hs
        .matches
        .iter()
        .enumerate()
        .skip(start)
        .take(count)
        .map(|(i, &idx)| {
            let text = app.history.get(idx).cloned().unwrap_or_default();
            candidate_row(text, i == hs.selected, theme)
        })
        .collect();
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// Paste Chip 渲染（粘贴内容元素化；输入区上方 chips 行，随输入区一起渲染）。
///
/// 每个 chip 一个圆角边框小盒，横向顺排：文本 chip 显示 `[Text: label]`
/// （accent），图片 chip 显示 `[Image #N]`（N 为在 paste_chips 中的 1 起序号，
/// info 色），放不下的 chip 截断在 area 右缘内。
pub fn render_paste_chips(app: &AppState, theme: &Theme, f: &mut Frame, area: Rect) {
    if app.paste_chips.is_empty() {
        return;
    }
    // 先 Clear：chips 行锚定在输入框上方，避免与对话正文叠层残留。
    f.render_widget(Clear, area);
    let mut x = area.x;
    for (i, chip) in app.paste_chips.iter().enumerate() {
        // 内容文案：图片 chip 忽略 label 统一 `[Image #N]`，文本 chip 带摘要。
        let label = match chip.kind {
            PasteChipKind::Image => format!(" [Image #{}] ", i + 1),
            PasteChipKind::Text => format!(" [Text: {}] ", chip.label),
        };
        // 圆角边框小盒：左右边框各 1 列。
        let w = label.chars().count() as u16 + 2;
        if x + w > area.right() {
            break;
        }
        let style = match chip.kind {
            PasteChipKind::Image => Style::default().fg(theme.info).add_modifier(Modifier::BOLD),
            PasteChipKind::Text => Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        };
        let box_widget = Paragraph::new(Span::styled(label, style)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(theme.border),
        );
        f.render_widget(
            box_widget,
            Rect {
                x,
                y: area.y,
                width: w,
                height: area.height.min(3),
            },
        );
        // chip 间距 1 列。
        x += w + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn spinner_frame_advances_and_cycles() {
        // 每 100ms 推进一帧，10 帧一轮（1s 一圈）。
        assert_eq!(spinner_frame(Duration::from_millis(0)), '⠋');
        assert_eq!(spinner_frame(Duration::from_millis(99)), '⠋');
        assert_eq!(spinner_frame(Duration::from_millis(100)), '⠙');
        assert_eq!(spinner_frame(Duration::from_millis(900)), '⠏');
        assert_eq!(
            spinner_frame(Duration::from_millis(1000)),
            '⠋',
            "1s 后回到首帧"
        );
        assert_eq!(spinner_frame(Duration::from_millis(1099)), '⠋');
        assert_eq!(spinner_frame(Duration::from_millis(1100)), '⠙');
        // 长运行不越界（取模保护）。
        assert!(SPINNER_FRAMES.contains(&spinner_frame(Duration::from_secs(3600 * 24))));
    }

    #[test]
    fn running_input_shows_static_hint() {
        // 运行中输入区显示静态"运行中"提示（转圈动画只在对话面板 agent 位置，
        // 输入区不重复 spinner，避免双转圈）。
        let theme = Theme::default();
        let started = std::time::Instant::now() - Duration::from_millis(350);
        let app = AppState {
            running: true,
            run_started_at: Some(started),
            tr: crate::i18n::Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        let buf = ratatui::backend::TestBackend::new(40, 3);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_input(&app, &theme, f, area);
            })
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        // TestBackend 中宽字符按 cell 拆开（"等 待 响 应"），去空格后断言。
        let flat: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        // 运行中输入区改为静态提示（对话面板有转圈动画，不重复 spinner）。
        assert!(!flat.contains('⠸'), "输入区不再显示转圈帧: {content}");
        assert!(flat.contains("运行中"), "运行中提示渲染: {content}");
        // 无 run_started_at（异常态）回退首帧，不 panic。
        let app = AppState {
            running: true,
            run_started_at: None,
            ..Default::default()
        };
        let buf = ratatui::backend::TestBackend::new(40, 3);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_input(&app, &theme, f, area);
            })
            .unwrap();
    }

    #[test]
    fn command_hint_renders_border_and_selection_marker() {
        // 斜杠候选浮层：边框 + 标题 + `▸` 选中指示，交互感明确。
        let theme = Theme::default();
        let hint = crate::commands::CommandHintState {
            candidates: crate::commands::CommandRegistry::search(""),
            selected: 1,
            arg_options: None,
        };
        assert_eq!(hint.visible_rows(), 8, "候选多时 cap 到 8 行");
        let app = AppState {
            command_hint: Some(hint),
            tr: crate::i18n::Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        let buf = ratatui::backend::TestBackend::new(60, 12);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_command_hint(&app, &theme, f, area);
            })
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        // TestBackend 中宽字符按 cell 拆开（"选 择"），去空格后断言。
        let flat: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(flat.contains('┌'), "浮层带边框: {content}");
        assert!(flat.contains("↑↓选择"), "标题含操作提示: {content}");
        assert!(flat.contains('▸'), "选中项带指示符: {content}");
        assert!(flat.contains("/help"), "命令名渲染: {content}");
    }

    #[test]
    fn command_hint_arg_mode_lists_options() {
        // 参数模式（`/fold ` 已输入）：枚举候选 + 边框。
        let theme = Theme::default();
        let hint = crate::commands::CommandHintState {
            candidates: vec![crate::commands::CommandRegistry::find("fold").unwrap()],
            selected: 0,
            arg_options: Some(vec!["all", "none", "reset"]),
        };
        assert_eq!(hint.visible_rows(), 3);
        let app = AppState {
            command_hint: Some(hint),
            tr: crate::i18n::Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        let buf = ratatui::backend::TestBackend::new(40, 8);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_command_hint(&app, &theme, f, area);
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
        assert!(flat.contains("all"), "枚举候选渲染: {content}");
        assert!(flat.contains("参数"), "参数模式标题: {content}");
    }

    #[test]
    fn hint_window_follows_selection() {
        assert_eq!(hint_window(3, 0), (0, 3));
        assert_eq!(hint_window(14, 0), (0, 8));
        assert_eq!(hint_window(14, 7), (4, 8), "选中第 8 项时窗口开始下移");
        assert_eq!(hint_window(14, 8), (5, 8), "选中第 9 项时窗口下移保持可见");
        assert_eq!(hint_window(14, 13), (6, 8), "到底时窗口停在尾部");
    }

    #[test]
    fn command_hint_window_keeps_selected_visible() {
        // 回归：候选多于 8 个（如输入 `/` 命中全部 14 条命令）时，
        // 选中第 10 项后高亮不得“消失”——此前固定取前 8 个，
        // 第 9..13 项渲染不出 ▸，直到绕回第一项才重新可见。
        let theme = Theme::default();
        let hint = crate::commands::CommandHintState {
            candidates: crate::commands::CommandRegistry::builtin().iter().collect(),
            selected: 10,
            arg_options: None,
        };
        assert_eq!(hint.visible_rows(), 8);
        let app = AppState {
            command_hint: Some(hint),
            ..Default::default()
        };
        let buf = ratatui::backend::TestBackend::new(60, 12);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_command_hint(&app, &theme, f, area);
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
        assert!(flat.contains('▸'), "选中高亮必须可见: {flat}");
        assert!(flat.contains("/raw"), "选中项在窗口内: {flat}");
        assert!(!flat.contains("/help"), "窗口已滚过前几项: {flat}");
    }

    #[test]
    fn completion_empty_candidates_renders_nothing() {
        // 无候选时不画浮层：直接返回（不 panic）。
        let mut app = AppState::default();
        app.completion = Some(crate::app::focus::CompletionState {
            start: 0,
            end: 0,
            candidates: vec![],
            selected: 0,
        });
        let theme = Theme::default();
        let buf = ratatui::backend::TestBackend::new(40, 10);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_completion(&app, &theme, f, area);
            })
            .unwrap();
    }

    #[test]
    fn input_renders_inline_markdown_highlight() {
        // 渲染接线：输入区经 md_spans 逐行着色，行内代码（反引号段）在
        // 渲染缓冲中带 tool(dim) 样式，链接 label 带 accent 前景。
        let theme = Theme::default();
        let mut app = AppState::default();
        app.input.text = "run `cargo test` see [doc](https://r.rs)".into();
        app.input.cursor = app.input.text.len();
        let buf = ratatui::backend::TestBackend::new(100, 3);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_input(&app, &theme, f, area);
            })
            .unwrap();
        let cells = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .collect::<Vec<_>>();
        // 行内代码反引号 cell 应带 tool(dim) 样式
        let tick = cells
            .iter()
            .find(|c| c.symbol() == "`")
            .expect("行内代码反引号应渲染出来");
        assert!(
            tick.modifier.contains(Modifier::DIM),
            "行内代码应带 tool(dim) 样式"
        );
        // 链接 label「doc」cell 应带 accent 前景
        let link_label = cells
            .iter()
            .enumerate()
            .find(|(_, c)| c.symbol() == "d" && c.fg == theme.accent)
            .expect("链接 label 应带 accent 前景");
        assert_eq!(link_label.1.symbol(), "d");
    }

    #[test]
    fn empty_input_shows_prompt_placeholder() {
        // grok 对齐：空输入时显示灰色 placeholder（PromptPlaceholder 键文案），
        // 不干扰实际输入（真实键入后即被替换）。
        let theme = Theme::default();
        let app = AppState {
            tr: crate::i18n::Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        let buf = ratatui::backend::TestBackend::new(60, 3);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_input(&app, &theme, f, area);
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
        assert!(flat.contains("输入任务"), "placeholder 文案渲染: {content}");
        assert!(flat.contains('❯'), "grok 强调前缀 ❯ 渲染: {content}");
        // 有输入时 placeholder 消失。
        let mut app = app;
        app.input.text = "build".into();
        app.input.cursor = 5;
        let buf = ratatui::backend::TestBackend::new(60, 3);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_input(&app, &theme, f, area);
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
        assert!(flat.contains("build"), "真实输入渲染: {content}");
        assert!(
            !flat.contains("输入任务"),
            "有输入时 placeholder 消失: {content}"
        );
    }

    #[test]
    fn bash_mode_prefix_is_yellow_exclamation() {
        // grok 对齐：bash 模式输入前缀为 `! `（黄色警示），与普通 `┃ ` 区分。
        let theme = Theme::default();
        let mut app = AppState {
            bash_mode: true,
            tr: crate::i18n::Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        app.input.text = "ls".into();
        app.input.cursor = 2;
        let buf = ratatui::backend::TestBackend::new(60, 3);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_input(&app, &theme, f, area);
            })
            .unwrap();
        let cells = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .collect::<Vec<_>>();
        let exclaim = cells
            .iter()
            .find(|c| c.symbol() == "!")
            .expect("bash 前缀 `!` 应渲染出来");
        assert_eq!(exclaim.fg, theme.warning, "bash 前缀应为黄色警示色");
        // 普通模式（非 bash）前缀为 accent 强调 `❯`（grok DEFAULT_PROMPT）。
        let mut app = app;
        app.bash_mode = false;
        let buf = ratatui::backend::TestBackend::new(60, 3);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_input(&app, &theme, f, area);
            })
            .unwrap();
        let cells = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .collect::<Vec<_>>();
        let bar = cells
            .iter()
            .find(|c| c.symbol() == "❯")
            .expect("普通模式前缀 `❯` 应渲染出来");
        assert_eq!(bar.fg, theme.accent, "普通模式前缀为 accent 色");
    }

    #[test]
    fn multiline_mode_shows_indicator() {
        // grok 对齐：multiline_mode 时边框右侧标题显示 MultilineOn 文案，
        // 提示 Enter 换行语义。
        let theme = Theme::default();
        let app = AppState {
            multiline_mode: true,
            tr: crate::i18n::Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        let buf = ratatui::backend::TestBackend::new(60, 3);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_input(&app, &theme, f, area);
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
        assert!(flat.contains("多行模式"), "多行模式指示渲染: {content}");
        assert!(flat.contains("Enter换行"), "Enter 换行语义提示: {content}");
    }

    #[test]
    fn session_title_appears_in_border_title() {
        // grok 对齐：会话标题内联在边框标题位；无标题时边框标题位留空
        //（model 由 info 行/欢迎屏 top_bar 展示，避免重复显示——截图实测问题）。
        let theme = Theme::default();
        let app = AppState {
            session_title: Some("修复登录".into()),
            tr: crate::i18n::Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        let buf = ratatui::backend::TestBackend::new(60, 3);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_input(&app, &theme, f, area);
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
            flat.contains("修复登录"),
            "边框标题位渲染会话标题: {content}"
        );
        // 无会话标题：边框标题位不再回退 model 标签（避免 model 重复）。
        let app = AppState {
            session_title: None,
            model_label: "deepseek-v4".into(),
            ..Default::default()
        };
        let buf = ratatui::backend::TestBackend::new(60, 3);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_input(&app, &theme, f, area);
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
            !flat.contains("deepseek-v4"),
            "无会话标题时边框标题位不再显示 model（去重）: {content}"
        );
    }

    #[test]
    fn history_search_renders_matches_with_selection() {
        // 历史搜索浮层：标题 = HistorySearchPlaceholder 文案，候选按 matches
        // 倒序渲染（最近在先），选中项带 ▸ 指示。
        let theme = Theme::default();
        let app = AppState {
            history: vec![
                "cargo check".into(),
                "cargo test".into(),
                "git status".into(),
            ],
            history_search: Some(crate::app::state::HistorySearchState {
                query: "cargo".into(),
                matches: vec![1, 0],
                selected: 0,
            }),
            tr: crate::i18n::Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        let buf = ratatui::backend::TestBackend::new(60, 8);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_history_search(&app, &theme, f, area);
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
        assert!(flat.contains("搜索历史"), "浮层标题渲染: {content}");
        assert!(flat.contains("cargotest"), "倒序候选渲染: {content}");
        assert!(flat.contains("cargocheck"), "第二个候选渲染: {content}");
        assert!(flat.contains('▸'), "选中项带指示符: {content}");
    }

    #[test]
    fn history_search_empty_matches_renders_nothing() {
        // 无匹配时不画浮层：直接返回（不 panic）。
        let theme = Theme::default();
        let app = AppState {
            history_search: Some(crate::app::state::HistorySearchState {
                query: "zzz".into(),
                matches: vec![],
                selected: 0,
            }),
            ..Default::default()
        };
        let buf = ratatui::backend::TestBackend::new(60, 8);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_history_search(&app, &theme, f, area);
            })
            .unwrap();
    }

    #[test]
    fn paste_chips_render_text_and_image_chips() {
        // Paste Chip：文本 chip `[Text: label]`（accent）、图片 chip `[Image #N]`
        //（info 色，N 为 1 起序号），随输入区一起渲染在输入区上方。
        let theme = Theme::default();
        let app = AppState {
            paste_chips: vec![
                crate::app::state::PasteChip {
                    kind: PasteChipKind::Text,
                    label: "hello".into(),
                },
                crate::app::state::PasteChip {
                    kind: PasteChipKind::Image,
                    label: String::new(),
                },
            ],
            ..Default::default()
        };
        let buf = ratatui::backend::TestBackend::new(60, 3);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_paste_chips(&app, &theme, f, area);
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
        assert!(flat.contains("Text:hello"), "文本 chip 渲染: {content}");
        // 图片 chip 为列表第 2 项（序号 = 1 起位置）。
        assert!(flat.contains("Image#2"), "图片 chip 序号渲染: {content}");
        let cells = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .collect::<Vec<_>>();
        // 图片 chip 的 `I` 应带 info 色（样式区分）。
        let img_i = cells
            .iter()
            .find(|c| c.symbol() == "I")
            .expect("图片 chip 内容应渲染出来");
        assert_eq!(img_i.fg, theme.info, "图片 chip 用 info 色区分");
    }
}

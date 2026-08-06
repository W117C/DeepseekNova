//! 输入区渲染：`❯` 前缀 + 无边框 + markdown 着色 + 可见光标 + 补全浮层。
//! 斜杠命令行内候选渲染在输入区上方（Claude Code 风格：就地展开）。

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::state::AppState;
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

/// 输入区渲染（`❯` 前缀 + md 高亮 + running 等待态）。
pub fn render_input(app: &AppState, theme: &Theme, f: &mut Frame, area: Rect) {
    let input_style = if app.running {
        theme.system
    } else {
        Style::default()
    };
    let pane_width = area.width.saturating_sub(2) as usize;
    let pane_rows = area.height.saturating_sub(2) as usize;
    let view = input_view(
        &app.input.text,
        app.input.cursor,
        pane_width.max(1),
        pane_rows.max(1),
    );
    // 首行前缀 `❯ `（accent），其余行（多行输入）无前缀。
    let input_lines: Vec<Line> = if app.running {
        // 帧由本轮运行起始时刻推导：事件循环 100ms tick 重绘，
        // spinner 随时间转动（此前用 `Instant::now().elapsed()` 恒为 0，
        // 动画永远停在首帧，等待效果不可见）。
        let frame = app
            .run_started_at
            .map(|t| spinner_frame(t.elapsed()))
            .unwrap_or(SPINNER_FRAMES[0]);
        vec![Line::from(vec![
            Span::styled(
                "❯ ",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{frame} 等待响应… Ctrl+C 取消"), theme.system),
        ])]
    } else {
        view.rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let mut spans = Vec::new();
                if i == 0 {
                    spans.push(Span::styled(
                        "❯ ",
                        Style::default()
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD),
                    ));
                }
                spans.extend(md_spans(row, theme));
                Line::from(spans)
            })
            .collect()
    };
    let input_widget = Paragraph::new(input_lines)
        .style(input_style)
        .scroll((view.scroll_row as u16, 0));
    f.render_widget(input_widget, area);
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
        "参数"
    } else {
        "命令"
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Line::from(Span::styled(
            format!("{title} · ↑↓ 选择 · Enter 执行 · Tab 补全 · Esc 关闭"),
            theme.title,
        )));
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
                    format!("/{:<name_w$}  {}", cmd.name, cmd.desc),
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
    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(theme.border)
        .title(Line::from(Span::styled("@ 文件补全", theme.title)));
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
    fn running_input_animates_from_run_start() {
        // 回归：spinner 帧必须由 run_started_at 推导——此前
        // `Instant::now().elapsed()` 恒为 0，动画永远停在首帧 ⠋，
        // 等待效果不可见。
        let theme = Theme::default();
        let started = std::time::Instant::now() - Duration::from_millis(350);
        let app = AppState {
            running: true,
            run_started_at: Some(started),
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
        assert!(flat.contains('⠸'), "350ms → 第 3 帧 ⠸: {content}");
        assert!(flat.contains("等待响应"), "等待文案渲染: {content}");
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
}

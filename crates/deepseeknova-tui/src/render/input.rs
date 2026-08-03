//! 输入区渲染：多行视图 + markdown 着色 + 可见光标 + 补全浮层。

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::state::AppState;
use crate::input::editor::input_view;
use crate::input::md_highlight::md_spans;
use crate::theme::Theme;

/// 输入区渲染（md 高亮 + running 等待态）。
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
    let input_lines: Vec<Line> = if app.running {
        vec![Line::from(" (等待响应… Ctrl+C 取消) ")]
    } else {
        view.rows
            .iter()
            .map(|row| Line::from(md_spans(row, theme)))
            .collect()
    };
    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Line::from(vec![
            Span::styled(
                ">",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" prompt", theme.title),
        ]));
    let input_widget = Paragraph::new(input_lines)
        .style(input_style)
        .block(input_block)
        .scroll((view.scroll_row as u16, 0));
    f.render_widget(input_widget, area);
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
    let block = Block::default()
        .borders(Borders::ALL)
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

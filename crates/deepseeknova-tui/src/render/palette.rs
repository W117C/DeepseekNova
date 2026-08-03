//! Ctrl+K 命令面板渲染：查询框 + 候选列表（模糊搜索）。

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::state::AppState;
use crate::commands::CommandRegistry;
use crate::theme::Theme;

/// 命令面板渲染。
pub fn render_palette(app: &AppState, theme: &Theme, f: &mut Frame, area: Rect) {
    let Some(pal) = &app.palette else {
        return;
    };
    let candidates = CommandRegistry::search(&pal.query);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Line::from(Span::styled("命令面板 (Ctrl+K)", theme.title)));
    let mut lines: Vec<Line> = Vec::new();
    // 查询行。
    let query_line = if pal.arg_input.is_some() {
        format!("参数: {}", pal.arg_input.as_deref().unwrap_or(""))
    } else {
        format!("> {}", pal.query)
    };
    lines.push(Line::from(Span::styled(
        query_line,
        theme.style_for(crate::model::conversation::LineKind::User),
    )));
    // 候选行（前 10）。
    for (i, cmd) in candidates.iter().take(10).enumerate() {
        let style = if i == pal.selected {
            theme.selection
        } else {
            theme.system
        };
        let args_hint = match cmd.args_spec {
            crate::commands::ArgsSpec::None => String::new(),
            crate::commands::ArgsSpec::FreeText => " <args>".to_string(),
            crate::commands::ArgsSpec::Enum(variants) => {
                format!(" <{}>", variants.join("|"))
            }
        };
        lines.push(Line::from(Span::styled(
            format!("  {} — {}{}", cmd.name, cmd.desc, args_hint),
            style,
        )));
    }
    if candidates.is_empty() {
        lines.push(Line::from(Span::styled(
            "  （无匹配命令）",
            theme.style_for(crate::model::conversation::LineKind::Error),
        )));
    }
    let widget = Paragraph::new(lines).block(block);
    f.render_widget(widget, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::focus::PaletteState;

    #[test]
    fn palette_renders_query_and_candidates() {
        let app = AppState {
            palette: Some(PaletteState {
                query: "co".into(),
                selected: 0,
                arg_input: None,
            }),
            ..Default::default()
        };
        let theme = Theme::default();
        let buf = ratatui::backend::TestBackend::new(60, 12);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                render_palette(&app, &theme, f, f.area());
            })
            .unwrap();
        let content = terminal.backend().buffer().content.clone();
        let text: String = content.iter().map(|c| c.symbol()).collect();
        assert!(text.contains("> co"), "查询行");
        assert!(text.contains("cost"), "候选命中 cost");
    }
}

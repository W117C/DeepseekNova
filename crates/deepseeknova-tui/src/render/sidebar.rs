//! 侧边栏渲染：5 个 Tab（会话/工具活动/MCP/成本/技能）。

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::focus::SidebarTab;
use crate::app::state::AppState;
use crate::model::conversation::Segment;
use crate::theme::Theme;

/// 侧边栏渲染：Tab 条 + 当前面板内容。
pub fn render_sidebar(app: &AppState, theme: &Theme, f: &mut Frame, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Line::from(Span::styled("面板", theme.title)));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);
    let tab_area = chunks[0];
    let body_area = chunks[1];

    // Tab 条（5 个，当前高亮）。
    let tab_line: Line = {
        let mut spans = Vec::new();
        for (i, tab) in SidebarTab::ALL.iter().enumerate() {
            let label = format!(
                "{}{} ",
                if *tab == app.sidebar_tab { "▸" } else { " " },
                tab.label()
            );
            let style = if *tab == app.sidebar_tab {
                theme.title
            } else {
                theme.system
            };
            spans.push(Span::styled(label, style));
            if i < SidebarTab::ALL.len() - 1 {
                spans.push(Span::styled("· ", theme.system));
            }
        }
        Line::from(spans)
    };
    f.render_widget(Paragraph::new(tab_line), tab_area);

    // 面板内容。
    let lines: Vec<Line> = match app.sidebar_tab {
        SidebarTab::Sessions => render_sessions(app, theme),
        SidebarTab::Tools => render_tools(app, theme),
        SidebarTab::Mcp => vec![Line::from(Span::styled(
            " 运行 /mcp 查看服务器状态",
            theme.system,
        ))],
        SidebarTab::Cost => render_cost(app, theme),
        SidebarTab::Skills => vec![Line::from(Span::styled(
            " 运行 /skills 查看可用技能",
            theme.system,
        ))],
    };
    f.render_widget(Paragraph::new(lines), body_area);
}

fn render_sessions(app: &AppState, theme: &Theme) -> Vec<Line<'static>> {
    let n = app.conversation.turn_count();
    if n == 0 {
        return vec![Line::from(Span::styled(" （暂无会话）", theme.system))];
    }
    let mut lines = vec![Line::from(Span::styled(
        format!(" 回合数: {n}"),
        theme.title,
    ))];
    // 最新在前：turn id 倒序（id 从 1 递增）。
    for id in (1..=n).rev() {
        let user = app
            .conversation
            .user_text_of(id as u64)
            .unwrap_or("")
            .trim();
        let preview = if user.is_empty() {
            format!("#{id}（空）")
        } else if user.chars().count() > 18 {
            format!("#{id} {}…", user.chars().take(18).collect::<String>())
        } else {
            format!("#{id} {user}")
        };
        lines.push(Line::from(Span::styled(preview, theme.system)));
    }
    lines
}

fn render_tools(app: &AppState, theme: &Theme) -> Vec<Line<'static>> {
    let mut tools: Vec<(String, &str)> = app
        .conversation
        .iter_segments()
        .filter_map(|(_, seg)| match seg {
            Segment::ToolCall { name, status, .. } => {
                let mark = match status {
                    crate::model::conversation::ToolStatus::Running => "…",
                    crate::model::conversation::ToolStatus::Ok => "✓",
                    crate::model::conversation::ToolStatus::Failed => "✗",
                };
                Some((name.clone(), mark))
            }
            _ => None,
        })
        .collect();
    tools.reverse(); // 最新在前
    if tools.is_empty() {
        return vec![Line::from(Span::styled(" （暂无工具调用）", theme.system))];
    }
    tools
        .into_iter()
        .take(10)
        .map(|(name, mark)| {
            let style = if mark == "✗" {
                theme
                    .style_for(crate::model::conversation::LineKind::Verification { passed: false })
            } else {
                theme.system
            };
            Line::from(Span::styled(format!(" {mark} {name}"), style))
        })
        .collect()
}

fn render_cost(app: &AppState, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match app.total_cost_usd {
        Some(cost) => lines.push(Line::from(Span::styled(
            format!(" 会话成本: ${cost:.6}"),
            theme.title,
        ))),
        None => lines.push(Line::from(Span::styled(
            " 成本不可用（无 router）",
            theme.system,
        ))),
    }
    if let Some(u) = &app.usage {
        lines.push(Line::from(Span::styled(
            format!(
                " ↑{} ↓{} Σ{}",
                u.prompt_tokens, u.completion_tokens, u.total_tokens
            ),
            theme.system,
        )));
    } else {
        lines.push(Line::from(Span::styled(
            " 运行 /cost 查看明细",
            theme.system,
        )));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::conversation::done_output;
    use deepseeknova_core::runner::RunEvent;

    #[test]
    fn sessions_lists_latest_first() {
        let mut app = AppState::default();
        app.conversation.begin_turn("第一个问题".into());
        app.apply_run_event(RunEvent::TextDelta("a".into()));
        app.apply_run_event(RunEvent::Done(done_output("")));
        app.conversation.begin_turn("第二个问题".into());
        app.apply_run_event(RunEvent::TextDelta("b".into()));
        app.apply_run_event(RunEvent::Done(done_output("")));
        let theme = Theme::default();
        let lines = render_sessions(&app, &theme);
        let texts: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        let pos1 = texts.find("#2").unwrap();
        let pos0 = texts.find("#1").unwrap();
        assert!(pos1 < pos0, "最新回合在前");
    }

    #[test]
    fn tools_lists_recent_calls() {
        let mut app = AppState::default();
        app.conversation.begin_turn("q".into());
        app.apply_run_event(RunEvent::ToolCallStart {
            id: "1".into(),
            name: "grep".into(),
        });
        app.apply_run_event(RunEvent::Done(done_output("")));
        let theme = Theme::default();
        let lines = render_tools(&app, &theme);
        let texts: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(texts.contains("grep"));
    }

    #[test]
    fn empty_sidebar_shows_placeholder() {
        let app = AppState::default();
        let theme = Theme::default();
        assert!(render_sessions(&app, &theme)[0].spans[0]
            .content
            .contains("暂无"));
        assert!(render_tools(&app, &theme)[0].spans[0]
            .content
            .contains("暂无"));
    }
}

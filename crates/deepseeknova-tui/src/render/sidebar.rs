//! 侧边栏渲染：5 个 Tab（会话/工具活动/MCP/成本/技能）。

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
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
    // 会话列表可见行数：优先展示磁盘保存的会话（上一轮/更早的对话），
    // 当前会话的回合折叠在下方小节，避免“看不到上个对话”。
    const MAX_SAVED_ROWS: usize = 16;
    let mut lines = Vec::new();
    // 首次加载（一两帧内）保持安静，不显示“加载中”噪声；
    // 加载完确实没有历史会话时才给一句轻提示。
    if app.saved_sessions.is_empty() {
        if app.sessions_loaded {
            lines.push(Line::from(Span::styled(" （暂无历史会话）", theme.system)));
        }
        // 首次加载（一两帧内）保持安静：不显示“加载中”噪声。
    } else {
        lines.push(Line::from(Span::styled(
            format!(" 保存的会话 · {}", app.saved_sessions.len()),
            theme.title,
        )));
        for (i, id) in app.saved_sessions.iter().take(MAX_SAVED_ROWS).enumerate() {
            let selected = i == app.saved_session_selected;
            let current = app.current_session.as_deref() == Some(id.as_str());
            let marker = if selected { "▸" } else { " " };
            let label = short_session_id(id, 18);
            let suffix = if current { " (当前)" } else { "" };
            let style = if selected {
                theme.selection
            } else {
                theme.system
            };
            lines.push(Line::from(Span::styled(
                format!("{marker} {label}{suffix}"),
                style,
            )));
        }
        if app.saved_sessions.len() > MAX_SAVED_ROWS {
            lines.push(Line::from(Span::styled(
                format!(
                    "  …还有 {} 个（/sessions 查看全部）",
                    app.saved_sessions.len() - MAX_SAVED_ROWS
                ),
                theme.system,
            )));
        }
    }

    // 当前会话内的回合（内存树；恢复/新会话后自动切换）。
    let n = app.conversation.turn_count();
    if n > 0 {
        lines.push(Line::from(Span::styled(" ── 本次会话 ──", theme.title)));
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
    }
    lines
}

/// 会话 id 的短标签：`chat-20260805-180304` → `20260805-180304`，超长截断。
fn short_session_id(id: &str, max: usize) -> String {
    let s = id.strip_prefix("chat-").unwrap_or(id);
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

fn render_tools(app: &AppState, theme: &Theme) -> Vec<Line<'static>> {
    // 聚合统计：名称 → (ok, fail, running)
    use crate::model::conversation::ToolStatus;
    use std::collections::BTreeMap;
    let mut stats: BTreeMap<String, (u32, u32, u32)> = BTreeMap::new();
    for (_, seg) in app.conversation.iter_segments() {
        if let Segment::ToolCall { name, status, .. } = seg {
            let e = stats.entry(name.clone()).or_default();
            match status {
                ToolStatus::Ok => e.0 += 1,
                ToolStatus::Failed => e.1 += 1,
                ToolStatus::Running => e.2 += 1,
            }
        }
    }
    if stats.is_empty() {
        return vec![Line::from(Span::styled(" （暂无工具调用）", theme.system))];
    }
    let mut lines = vec![Line::from(Span::styled(
        format!(" 工具活动 · {} 种工具", stats.len()),
        theme.title,
    ))];
    for (name, (ok, fail, running)) in stats {
        // 语义色：失败红 / 有运行中黄 / 正常 dim。
        let style = if fail > 0 {
            theme.style_for(crate::model::conversation::LineKind::Verification { passed: false })
        } else if running > 0 {
            Style::default().fg(ratatui::style::Color::Yellow)
        } else {
            theme.system
        };
        let mut suffix = Vec::new();
        if ok > 0 {
            suffix.push(format!("✓{ok}"));
        }
        if fail > 0 {
            suffix.push(format!("✗{fail}"));
        }
        if running > 0 {
            suffix.push("…".to_string());
        }
        let total = ok + fail + running;
        lines.push(Line::from(Span::styled(
            format!(" {name:<12} {} 次  [{}]", total, suffix.join(" ")),
            style,
        )));
    }
    lines
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
        let theme = Theme::default();
        // 首次加载（未拉取磁盘）：保持安静，不显示“加载中”噪声。
        let loading = AppState::default();
        assert!(
            render_sessions(&loading, &theme).is_empty(),
            "未加载完成不渲染占位噪声"
        );
        // 已加载但确实无会话：显示“暂无保存的会话”。
        let empty = AppState {
            sessions_loaded: true,
            ..Default::default()
        };
        assert!(render_sessions(&empty, &theme)[0].spans[0]
            .content
            .contains("暂无历史会话"));
        let app = AppState::default();
        assert!(render_tools(&app, &theme)[0].spans[0]
            .content
            .contains("暂无"));
    }

    #[test]
    fn saved_sessions_render_selection_and_current_marker() {
        let app = AppState {
            sessions_loaded: true,
            saved_sessions: vec![
                "chat-20260806-112831".to_string(),
                "chat-20260805-180304".to_string(),
            ],
            current_session: Some("chat-20260806-112831".to_string()),
            saved_session_selected: 0,
            ..Default::default()
        };
        let theme = Theme::default();
        let lines = render_sessions(&app, &theme);
        let texts: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(texts.contains("保存的会话 · 2"), "会话计数: {texts}");
        assert!(
            texts.contains("▸ 20260806-112831 (当前)"),
            "选中 + 当前标记: {texts}"
        );
        assert!(texts.contains("20260805-180304"), "列表含历史会话: {texts}");
    }

    #[test]
    fn short_session_id_strips_prefix_and_truncates() {
        assert_eq!(
            short_session_id("chat-20260805-180304", 18),
            "20260805-180304"
        );
        assert_eq!(short_session_id("chat-20260805-180304", 10), "20260805-…");
        assert_eq!(short_session_id("plain-id", 18), "plain-id");
    }
}

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
        let groups = group_by_night(&app.saved_sessions);
        let mut shown = 0usize;
        'outer: for (gi, (night, ids)) in groups.iter().enumerate() {
            if shown >= MAX_SAVED_ROWS {
                break;
            }
            lines.push(Line::from(Span::styled(
                format!(" ▾ {night} 夜 · {}", ids.len()),
                theme.title,
            )));
            for id in ids {
                if shown >= MAX_SAVED_ROWS {
                    break 'outer;
                }
                let i = app
                    .saved_sessions
                    .iter()
                    .position(|m| &m.id == id)
                    .unwrap_or(usize::MAX);
                let selected = i == app.saved_session_selected;
                let current = app.current_session.as_deref() == Some(id.as_str());
                let star = magnitude_char(current, gi);
                let marker = if selected { "▸" } else { " " };
                // 有首句预览就显示预览（会话标题），否则回退不透明 id。
                let preview = app
                    .saved_sessions
                    .get(i)
                    .map(|m| m.preview.trim().to_string())
                    .filter(|p| !p.is_empty());
                let label = match preview {
                    Some(p) => {
                        let cut: String = p.chars().take(16).collect();
                        if p.chars().count() > 16 {
                            format!("{cut}…")
                        } else {
                            cut
                        }
                    }
                    None => short_session_id(id, 16),
                };
                let suffix = if current { " (当前)" } else { "" };
                let style = if selected {
                    theme.selection
                } else {
                    theme.system
                };
                lines.push(Line::from(Span::styled(
                    format!("{marker}{star} {label}{suffix}"),
                    style,
                )));
                shown += 1;
            }
        }
        if app.saved_sessions.len() > shown {
            lines.push(Line::from(Span::styled(
                format!(
                    "  …还有 {} 个（/sessions 查看全部）",
                    app.saved_sessions.len() - shown
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

/// `chat-20260807-164211` → `08-07`；无法解析时回退 `----`。
fn night_key(id: &str) -> String {
    let s = id.strip_prefix("chat-").unwrap_or(id);
    let date = s.get(..8).unwrap_or("");
    if date.len() == 8 && date.chars().all(|c| c.is_ascii_digit()) {
        format!("{}-{}", &date[4..6], &date[6..8])
    } else {
        "----".to_string()
    }
}

/// 按夜次分组（夜次倒序、组内保持原顺序），返回（夜次, 会话 id 列表）。
fn group_by_night(metas: &[crate::app::state::SessionMeta]) -> Vec<(String, Vec<String>)> {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for m in metas {
        let key = night_key(&m.id);
        if let Some(g) = groups.iter_mut().find(|(k, _)| *k == key) {
            g.1.push(m.id.clone());
        } else {
            groups.push((key, vec![m.id.clone()]));
        }
    }
    groups.sort_by(|a, b| b.0.cmp(&a.0));
    groups
}

/// 星等三档：当前 ◉ / 本夜 ● / 更早夜次 ·。
fn magnitude_char(current: bool, group_index: usize) -> &'static str {
    if current {
        "◉"
    } else if group_index == 0 {
        "●"
    } else {
        "·"
    }
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
    // 测光·评分卡：六维 █░ 横条 + 分值；无数据时给一句引导。
    match &app.scorecard {
        Some(sc) => {
            lines.push(Line::from(Span::styled(" 测光·评分卡", theme.title)));
            for row in &sc.rows {
                let bar = crate::model::scorecard::photometry_bar(row.score);
                lines.push(Line::from(Span::styled(
                    format!(" {:<4} {bar} {:>5.1}", row.dim, row.score),
                    theme.system,
                )));
            }
        }
        None => lines.push(Line::from(Span::styled(
            " 测光待完成（运行 /scorecard）",
            theme.system,
        ))),
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
                crate::app::state::SessionMeta {
                    id: "chat-20260806-112831".to_string(),
                    preview: "查看一下这个仓库".to_string(),
                },
                crate::app::state::SessionMeta {
                    id: "chat-20260805-180304".to_string(),
                    preview: String::new(),
                },
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
        assert!(texts.contains("▾ 08-06 夜 · 1"), "夜次分组头: {texts}");
        assert!(texts.contains("▾ 08-05 夜 · 1"), "夜次分组头: {texts}");
        assert!(
            texts.contains("▸◉ 查看一下这个仓库 (当前)"),
            "有预览显示首句标题: {texts}"
        );
        assert!(
            texts.contains("· 20260805-180304"),
            "无预览回退不透明 id: {texts}"
        );
    }

    #[test]
    fn night_grouping_orders_latest_night_first_and_marks_magnitude() {
        let metas = vec![
            crate::app::state::SessionMeta {
                id: "chat-20260807-160000".to_string(),
                preview: String::new(),
            },
            crate::app::state::SessionMeta {
                id: "chat-20260807-130000".to_string(),
                preview: String::new(),
            },
            crate::app::state::SessionMeta {
                id: "chat-20260806-220000".to_string(),
                preview: String::new(),
            },
            crate::app::state::SessionMeta {
                id: "plain".to_string(),
                preview: String::new(),
            },
        ];
        let groups = group_by_night(&metas);
        assert_eq!(
            groups.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            vec!["08-07", "08-06", "----"]
        );
        assert_eq!(groups[0].1.len(), 2);
        assert_eq!(magnitude_char(true, 0), "◉");
        assert_eq!(magnitude_char(false, 0), "●");
        assert_eq!(magnitude_char(false, 1), "·");
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

    #[test]
    fn cost_panel_shows_photometry_when_scorecard_loaded() {
        let app = AppState {
            scorecard: Some(crate::model::scorecard::Scorecard::parse_json(
                r#"{"scores":{"治理":92.3,"验证":94.7,"反思":88.1,"审查":90.5,"协议":96.2,"综合":92.0}}"#,
            )
            .unwrap()),
            ..Default::default()
        };
        let theme = Theme::default();
        let lines = render_cost(&app, &theme);
        let texts: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(texts.contains("测光·评分卡"), "{texts}");
        assert!(texts.contains("治理"), "{texts}");
        assert!(texts.contains("█"), "光度条: {texts}");
        assert!(texts.contains("92.3"), "{texts}");

        let empty = AppState::default();
        let empty_lines = render_cost(&empty, &theme);
        let empty_texts: String = empty_lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(empty_texts.contains("测光待完成"), "{empty_texts}");
    }
}

//! Tasks 面板：Ctrl+G 切换的模态浮层，展示进行中的工具/子代理任务
//! （grok build tasks pane 对齐）。
//!
//! 数据源为会话消息树中的 `Segment::ToolCall { status: Running }` 段——
//! 进行中的工具调用即"任务"；无进行中任务时展示空态。渲染复用
//! `render::sidebar` 的候选行/窗口样式风格，标题带进行中任务计数。

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::state::AppState;
use crate::i18n::Key;
use crate::model::conversation::{Segment, ToolStatus};
use crate::theme::Theme;

/// 收集进行中的工具调用任务（`ToolStatus::Running` 的 `ToolCall` 段）。
/// 返回任务名列表（保持会话内出现顺序）。
pub fn running_tasks(app: &AppState) -> Vec<String> {
    let mut tasks = Vec::new();
    for (_, seg) in app.conversation.iter_segments() {
        if let Segment::ToolCall { name, status, .. } = seg {
            if *status == ToolStatus::Running {
                tasks.push(name.clone());
            }
        }
    }
    tasks
}

/// 渲染 Tasks 面板（调用方保证 `app.tasks_visible` 激活）。
/// grok 对齐：accent_user 粗体标题 + prompt_border_active 边框；
/// 任务行 ⏺ accent_user 标记 + text_primary 文本；空态 gray。
pub fn render_tasks(app: &AppState, theme: &Theme, f: &mut Frame, area: Rect) {
    let tasks = running_tasks(app);
    let n = tasks.len();
    f.render_widget(ratatui::widgets::Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.prompt_border_active))
        .title(Line::from(Span::styled(
            app.tr.t_args(Key::TasksHeader, &[("n", &n.to_string())]),
            Style::default()
                .fg(theme.accent_user)
                .add_modifier(Modifier::BOLD),
        )));
    let lines: Vec<Line> = if tasks.is_empty() {
        vec![Line::from(Span::styled(
            app.tr.t(Key::TasksNoRunning),
            Style::default().fg(theme.gray),
        ))]
    } else {
        tasks
            .iter()
            .take(8)
            .map(|name| {
                Line::from(vec![
                    Span::styled(
                        "⏺ ",
                        Style::default()
                            .fg(theme.accent_user)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        app.tr.t_args(Key::TasksToolRow, &[("name", name)]),
                        Style::default().fg(theme.text_primary),
                    ),
                ])
            })
            .collect()
    };
    f.render_widget(Paragraph::new(lines).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_app() -> AppState {
        let mut app = AppState::default();
        app.conversation.begin_turn("q".into());
        app.apply_run_event(deepseeknova_core::runner::RunEvent::ToolCallStart {
            id: "c1".into(),
            name: "bash".into(),
        });
        // 注意：不发送 Done——回合结束会把 Running 工具标记为 Failed，
        // Tasks 面板只展示进行中的任务。
        app
    }

    #[test]
    fn running_tasks_collects_only_running_tools() {
        let app = sample_app();
        assert_eq!(running_tasks(&app), vec!["bash"]);
    }

    #[test]
    fn running_tasks_empty_when_no_tools() {
        let app = AppState::default();
        assert!(running_tasks(&app).is_empty());
    }

    #[test]
    fn renders_without_panic() {
        let app = sample_app();
        let theme = Theme::default();
        let buf = ratatui::backend::TestBackend::new(60, 12);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_tasks(&app, &theme, f, area);
            })
            .unwrap();
    }
}

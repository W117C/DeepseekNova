//! 统一模态基础设施：ModalWindow chrome + 各模态内容渲染。
//!
//! 对齐 grok build 的 modal 设计（xai-grok-pager modal_window.rs）：
//! accent 方角边框 + 粗体标题 + 底部 footer shortcuts（居中内联、
//! clickable 加粗）、Clear 背景；内容区随模态类型切换（快捷键速查表 /
//! 设置占位）。调用方（`render::render_modal_overlay`）保证
//! `app.active_modal.is_some()` 才进入渲染；Esc 关闭与 j/k 滚动由
//! `app::focus` 的模态优先分支处理。

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::state::{ActiveModal, AppState};
use crate::i18n::Key;
use crate::theme::Theme;

/// 模态窗宽占所在区域的比例（约 60%）。
const MODAL_WIDTH_RATIO: u16 = 3;
const MODAL_WIDTH_DIV: u16 = 5;

/// footer shortcuts 行：`Esc 关闭` + 滚动提示（grok 模态底部快捷键栏）。
/// `mouse_pos` 命中该行时整体 hover 高亮（text_primary 加粗）。
fn footer_shortcuts_line(theme: &Theme, mouse_pos: Option<(u16, u16)>) -> Line<'static> {
    let hovered = mouse_pos.is_some();
    Line::from(vec![
        Span::styled(
            "Esc 关闭",
            Style::default()
                .fg(theme.accent_user)
                .add_modifier(if hovered {
                    Modifier::BOLD | Modifier::UNDERLINED
                } else {
                    Modifier::BOLD
                }),
        ),
        Span::styled("   j/k ↑/↓ 滚动", Style::default().fg(theme.gray)),
    ])
}

/// 渲染统一模态窗 chrome（accent 方角边框 + 标题 + footer + 内容行），居中。
///
/// 内容行超出高度时按 `app.modal_scroll` 滚动窗口显示（行数多时
/// j/k/↑/↓ 滚动，焦点层在模态优先分支处理）。选中行以 fzf 风格
/// [`Theme::fuzzy_accent`] 文字色标记（不反色）。
/// 带 tab 行的模态窗渲染（grok ModalWindow Optional tabs 对齐）。
/// `tabs` 为 tab 行 spans（当前模态已高亮），渲染在标题下方。
fn render_modal_window_tabs(
    app: &AppState,
    theme: &Theme,
    f: &mut Frame,
    area: Rect,
    title: &str,
    body: Vec<Line<'static>>,
    tabs: Vec<Span<'static>>,
) {
    f.render_widget(ratatui::widgets::Clear, area);
    let width = (area.width * MODAL_WIDTH_RATIO / MODAL_WIDTH_DIV)
        .max(44)
        .min(area.width);
    let height = (body.len() as u16 + 3).min(area.height).max(5);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let win = Rect {
        x,
        y,
        width,
        height,
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.prompt_border_active))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(theme.accent_user)
                .add_modifier(Modifier::BOLD),
        ))
        // grok 对齐：右上角 [✗] 关闭钮（modal_window.rs 同款；Esc 关闭），
        // 鼠标 hover 时 accent_user 高亮。
        .title(
            Line::from(Span::styled(
                " [✗]",
                Style::default().fg(if app.mouse_pos.is_some() {
                    theme.accent_user
                } else {
                    theme.gray
                }),
            ))
            .right_aligned(),
        );
    let inner = block.inner(win);
    f.render_widget(block, win);
    // tab 行（标题下方一行；grok ModalWindow tabs）。
    if !tabs.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(tabs)),
            Rect {
                x: inner.x,
                y: inner.y,
                width: inner.width,
                height: 1,
            },
        );
    }
    // footer shortcuts 行（固定占底部一行）。
    f.render_widget(
        Paragraph::new(footer_shortcuts_line(theme, app.mouse_pos)),
        Rect {
            x: inner.x,
            y: inner.y + inner.height.saturating_sub(1),
            width: inner.width,
            height: 1,
        },
    );
    // 滚动窗口：modal_scroll 越界钳制，显示可见切片（footer 上方）。
    let content_h = inner.height.saturating_sub(1) as usize;
    let scroll = app
        .modal_scroll
        .min(body.len().saturating_sub(content_h.max(1)));
    let visible = body
        .iter()
        .skip(scroll)
        .take(content_h.max(1))
        .cloned()
        .collect::<Vec<_>>();
    let mut lines = visible;
    if body.len() > content_h {
        lines.push(Line::from(Span::styled(
            format!("▾ {} / {} (j/k scroll)", scroll + 1, body.len()),
            Style::default().fg(theme.gray),
        )));
    }
    f.render_widget(
        Paragraph::new(lines),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: content_h.max(1) as u16,
        },
    );
}

/// 渲染当前激活模态的内容（调用方保证 `app.active_modal` 非 None）。
/// grok 对齐：模态顶部渲染 tab 行（ShortcutsHelp/Settings），当前模态
/// accent 高亮、其余 dim——ModalWindow Optional tabs 的轻量版。
pub fn render_active_modal(app: &AppState, theme: &Theme, f: &mut Frame, area: Rect) {
    let tabs = [
        (ActiveModal::ShortcutsHelp, app.tr.t(Key::ShortcutsTitle)),
        (ActiveModal::Settings, app.tr.t(Key::SettingsTitle)),
    ];
    let current = app.active_modal;
    let mut tab_spans: Vec<Span<'static>> = Vec::new();
    for (i, (modal, label)) in tabs.iter().enumerate() {
        if i > 0 {
            tab_spans.push(Span::styled("  ", Style::default()));
        }
        let active = current == Some(*modal);
        tab_spans.push(Span::styled(
            if active {
                format!(" ▸ {label} ")
            } else {
                format!("   {label} ")
            },
            Style::default()
                .fg(if active {
                    theme.accent_user
                } else {
                    theme.gray
                })
                .add_modifier(if active {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ));
    }
    match app.active_modal {
        Some(ActiveModal::ShortcutsHelp) => {
            let title = app.tr.t(Key::ShortcutsTitle);
            render_modal_window_tabs(
                app,
                theme,
                f,
                area,
                title,
                shortcuts_help_lines(app, theme),
                tab_spans,
            );
        }
        Some(ActiveModal::Settings) => {
            let title = app.tr.t(Key::SettingsTitle);
            render_modal_window_tabs(
                app,
                theme,
                f,
                area,
                title,
                vec![Line::from(Span::styled(
                    app.tr.t(Key::SettingsTitle),
                    theme.system,
                ))],
                tab_spans,
            );
        }
        None => {}
    }
}

/// 快捷键速查表内容：遍历编译期绑定表，生成 `域:动作 → 键位` 行。
fn shortcuts_help_lines(_app: &AppState, theme: &Theme) -> Vec<Line<'static>> {
    use crate::app::actions::ActionContext;
    let mut lines = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (ctx, binding, action) in crate::app::actions::BINDINGS {
        // 同一 action 在同一 context 下取首个绑定展示，避免重复。
        if !seen.insert((*ctx, *action)) {
            continue;
        }
        let ctx_name = match ctx {
            ActionContext::Input => "input",
            ActionContext::Conversation => "conv",
            ActionContext::Sidebar => "sidebar",
            ActionContext::Completion => "modal",
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<9} {}", ctx_name, binding.display()),
                Style::default().fg(theme.accent),
            ),
            Span::styled(format!("  {}", action.name()), Style::default()),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled("(no bindings)", theme.system)));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::AppState;

    fn sample_app() -> AppState {
        AppState::default()
    }

    #[test]
    fn modal_window_renders_without_panic() {
        let app = sample_app();
        let theme = Theme::default();
        let buf = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_modal_window_tabs(
                    &app,
                    &theme,
                    f,
                    area,
                    "test",
                    vec![Line::from("row1"), Line::from("row2")],
                    Vec::new(),
                );
            })
            .unwrap();
    }

    #[test]
    fn shortcuts_help_lists_bindings() {
        let app = sample_app();
        let theme = Theme::default();
        let lines = shortcuts_help_lines(&app, &theme);
        assert!(!lines.is_empty(), "快捷键表应有内容");
        // 至少包含输入提交类绑定。
        assert!(lines.iter().any(|l| l.to_string().contains("input")));
    }

    #[test]
    fn active_modal_shortcuts_renders() {
        let mut app = sample_app();
        app.active_modal = Some(ActiveModal::ShortcutsHelp);
        let theme = Theme::default();
        let buf = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_active_modal(&app, &theme, f, area);
            })
            .unwrap();
    }

    #[test]
    fn active_modal_settings_renders() {
        let mut app = sample_app();
        app.active_modal = Some(ActiveModal::Settings);
        let theme = Theme::default();
        let buf = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_active_modal(&app, &theme, f, area);
            })
            .unwrap();
    }
}

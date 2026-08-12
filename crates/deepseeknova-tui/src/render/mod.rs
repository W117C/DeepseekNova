//! 渲染层：布局、消息卡、侧边栏、输入区、状态行。
//!
//! 渲染 = 消息树渲染 → pending → 命令反馈 echo；全部经 [`crate::theme::Theme`] 取样式，
//! 不散落硬编码颜色。

pub mod approval;
pub mod input;
pub mod layout;
pub mod message;
pub mod modal;
pub mod sidebar;
pub mod status;
pub mod tasks;
pub mod trust;
pub mod welcome;

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

use crate::app::state::AppState;
use crate::theme::Theme;

/// 渲染 toast 色块（底部居中一行；TTL 已由事件循环清除，这里只渲染）。
pub fn render_toast(app: &AppState, theme: &Theme, f: &mut Frame, area: Rect) {
    let Some((text, _)) = &app.toast else {
        return;
    };
    let line = Line::from(Span::styled(
        text.as_str(),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    ));
    let width = line.width().min(area.width.saturating_sub(2) as usize) as u16;
    let x = area.x + area.width.saturating_sub(width) / 2;
    // 画在输入区（底部 3 行）上方一行：y = height-4，避免与欢迎屏/主布局
    // 底部输入框重叠（实测 toast 曾盖住输入框顶框）。
    let y = area.y + area.height.saturating_sub(4);
    let win = Rect {
        x,
        y,
        width,
        height: 1,
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent));
    f.render_widget(ratatui::widgets::Clear, win);
    f.render_widget(block, win);
    f.render_widget(
        Paragraph::new(line),
        Rect {
            x: win.x + 1,
            y: win.y,
            width: win.width.saturating_sub(2),
            height: 1,
        },
    );
}

/// 渲染模态覆盖层（`app.active_modal` 非 None 时居中弹窗）。
pub fn render_modal_overlay(app: &AppState, theme: &Theme, f: &mut Frame, area: Rect) {
    if app.active_modal.is_none() {
        return;
    }
    crate::render::modal::render_active_modal(app, theme, f, area);
}

/// 帧末总接线：模态 + toast（普通布局帧末调用；欢迎分支由 welcome 自渲染 toast）。
pub fn render_overlays(app: &AppState, theme: &Theme, f: &mut Frame, area: Rect) {
    render_modal_overlay(app, theme, f, area);
    render_toast(app, theme, f, area);
}

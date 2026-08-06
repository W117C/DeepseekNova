//! 状态行与提示行：model 标签用 accent，次要信息 dim，随焦点显示键位。

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::actions::ActionContext;
use crate::app::focus::Focus;
use crate::app::state::AppState;
use crate::theme::Theme;

/// 状态行分段（仪表盘式，3 组信息，语义色分层）：
/// 1) 运行态 + 模型（主信息，accent/bold）
/// 2) token 预算条 + 成本（资源，阈值变色）
/// 3) 计数（turn/usage/lines，dim 静默）
pub fn status_segments(app: &AppState, theme: &Theme, scroll_pct: usize) -> Vec<Span<'static>> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut segments = Vec::new();
    // ── 组 1：运行态 + 模型 ──────────────────────────────
    let state = if app.running {
        Span::styled(
            "●",
            Style::default().fg(theme
                .verification_ok
                .fg
                .unwrap_or(ratatui::style::Color::Green)),
        )
    } else {
        Span::styled("○", dim)
    };
    segments.push(state);
    segments.push(Span::styled(" ", dim));
    segments.push(Span::styled(
        app.model_label.clone(),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    ));
    // ── 组 2：token 预算条 + 成本 ────────────────────────
    if let Some((used, window)) = app.context_usage {
        let pct = used.saturating_mul(100).checked_div(window).unwrap_or(0);
        let style = if pct >= 95 {
            Style::default().fg(theme
                .verification_fail
                .fg
                .unwrap_or(ratatui::style::Color::Red))
        } else if pct >= 80 {
            Style::default().fg(ratatui::style::Color::Yellow)
        } else {
            dim
        };
        let bar = token_bar(pct);
        segments.push(Span::styled(" │ ctx ".to_string(), dim));
        segments.push(Span::styled(
            format!(
                "[{bar}] {pct}% ({} / {})",
                fmt_tokens(used),
                fmt_tokens(window)
            ),
            style,
        ));
    }
    if let Some(cost) = app.total_cost_usd {
        segments.push(Span::styled(format!(" │ ${cost:.4}"), dim));
    }
    // ── 组 3：计数（静默） ───────────────────────────────
    segments.push(Span::styled(format!(" │ turn {}", app.turn), dim));
    if let Some(u) = &app.usage {
        segments.push(Span::styled(
            format!(
                " │ ↑{} ↓{} Σ{} 推理{} 缓存hit{}",
                u.prompt_tokens,
                u.completion_tokens,
                u.total_tokens,
                u.reasoning_tokens,
                u.cache_hit_tokens
            ),
            dim,
        ));
    }
    segments.push(Span::styled(
        format!(" │ lines {} 滚动{}%", app.render_line_count(), scroll_pct),
        dim,
    ));
    // 退出确认警示（高优先级，红色加粗）。
    if app.quit_armed {
        segments.push(Span::styled(
            " │ ⚠ 再按 Esc 退出",
            Style::default()
                .fg(theme
                    .verification_fail
                    .fg
                    .unwrap_or(ratatui::style::Color::Red))
                .add_modifier(Modifier::BOLD),
        ));
    }
    segments
}

/// 上下文感知提示行：随焦点显示当前键位。键位文本从 action 注册表
/// 动态查询（Claude Code `Rw` 同构）——未来 keybindings.json 用户改键后
/// 提示自动更新，无需改动此处。
pub fn hint_for(focus: Focus) -> String {
    use crate::app::actions::{chord_for, Action};
    let chord = |action| chord_for(ctx_for(focus), action).unwrap_or_default();
    match focus {
        Focus::Conversation => format!(
            "{} 导航 · {} 折叠 · {} 复制 · {} 翻页 · {} 首尾 · Esc 回输入",
            chord(Action::ConvSelectNext),
            chord(Action::ConvToggleFold),
            chord(Action::ConvCopy),
            chord(Action::ConvScrollPageUp),
            chord(Action::ConvScrollTop),
        ),
        Focus::Input => {
            "/ 命令 · Ctrl+U 清行 · Ctrl+W 删词 · Shift+Enter 换行 · Esc 取消/再按退出".to_string()
        }
        Focus::Sidebar => format!(
            "{} 选择会话 · Enter 恢复 · {} 切面板 · Esc 关闭",
            chord(Action::SidebarSelectNext),
            chord(Action::SidebarNextTab)
        ),
        Focus::Completion => "↑↓ 选择 · Enter 插入 · Esc 关闭补全".to_string(),
        Focus::Confirm => "y 确认 · n/Esc 取消".to_string(),
    }
}

/// Focus → ActionContext 映射（提示查询用）。
fn ctx_for(focus: Focus) -> crate::app::actions::ActionContext {
    match focus {
        Focus::Input => ActionContext::Input,
        Focus::Conversation => ActionContext::Conversation,
        Focus::Sidebar => ActionContext::Sidebar,
        Focus::Completion => ActionContext::Completion,
        Focus::Confirm => ActionContext::Input,
    }
}

/// token 数的人类可读格式：>=1M 显示 `x.xM`，>=1k 显示 `x.xk`，否则原值。
pub fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// 10 格 token 预算条：占用百分比 → `████░░░░░░`（Claude Code
/// token 预算条的资源可见性设计；有占用即至少 1 格）。
fn token_bar(pct: u64) -> String {
    let filled = (pct.min(100) * 10).div_ceil(100) as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_segments_style_model_and_secondary_info() {
        let theme = Theme::default();
        let app = AppState::default();
        let segments = status_segments(&app, &theme, 0);
        // 组 1：运行态圆点 + 模型名（accent bold）。
        assert_eq!(segments[0].content, "○");
        assert!(segments[0].style.add_modifier.contains(Modifier::DIM));
        let model = segments
            .iter()
            .find(|s| s.style.fg == Some(theme.accent))
            .expect("模型 span（accent）存在");
        assert!(model.style.add_modifier.contains(Modifier::BOLD));
        // 计数段 dim。
        let turn = segments
            .iter()
            .find(|s| s.content.contains("turn"))
            .unwrap();
        assert!(turn.style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn hint_text_per_focus() {
        assert!(hint_for(Focus::Input).contains("/ 命令"));
        assert!(hint_for(Focus::Conversation).contains("导航"));
        assert!(hint_for(Focus::Conversation).contains("导航"));
        assert!(hint_for(Focus::Sidebar).contains("切面板"));
        assert!(hint_for(Focus::Completion).contains("Enter"));
    }

    #[test]
    fn context_usage_renders_pct_with_threshold_colors() {
        let theme = Theme::default();
        let dim = Style::default().add_modifier(Modifier::DIM);

        // 30%：dim 常规。
        let app = AppState {
            context_usage: Some((30_000, 100_000)),
            ..Default::default()
        };
        let segments = status_segments(&app, &theme, 0);
        let ctx = segments
            .iter()
            .find(|s| s.content.contains("30%"))
            .expect("ctx 预算段存在");
        assert!(
            ctx.content.contains("[███░░░░░░░]"),
            "bar 渲染: {}",
            ctx.content
        );
        assert_eq!(ctx.style, dim);

        // 85%：黄色警示。
        let app = AppState {
            context_usage: Some((85_000, 100_000)),
            ..Default::default()
        };
        let segments = status_segments(&app, &theme, 0);
        let ctx = segments.iter().find(|s| s.content.contains("85%")).unwrap();
        assert_eq!(ctx.style.fg, Some(ratatui::style::Color::Yellow));

        // 97%：红色（verification_fail 的 fg）。
        let app = AppState {
            context_usage: Some((97_000, 100_000)),
            ..Default::default()
        };
        let segments = status_segments(&app, &theme, 0);
        let ctx = segments.iter().find(|s| s.content.contains("97%")).unwrap();
        assert_eq!(ctx.style.fg, theme.verification_fail.fg);

        // 无 context_usage：不渲染 ctx 段。
        let app = AppState::default();
        let segments = status_segments(&app, &theme, 0);
        assert!(!segments.iter().any(|s| s.content.contains('█')));
    }

    #[test]
    fn fmt_tokens_human_readable() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1_500), "1.5k");
        assert_eq!(fmt_tokens(240_000), "240.0k");
        assert_eq!(fmt_tokens(1_200_000), "1.2M");
    }

    #[test]
    fn token_bar_reflects_percentage() {
        assert_eq!(token_bar(0), "░░░░░░░░░░");
        assert_eq!(token_bar(50), "█████░░░░░");
        assert_eq!(token_bar(100), "██████████");
        assert_eq!(token_bar(5), "█░░░░░░░░░", "5% 也至少 1 格");
        assert_eq!(token_bar(150), "██████████", "超 100% 钳制");
        assert!(token_bar(97).chars().filter(|&c| c == '█').count() >= 9);
    }

    #[test]
    fn ctx_bar_renders_in_status_line() {
        let theme = Theme::default();
        let app = AppState {
            context_usage: Some((46_000, 100_000)),
            ..Default::default()
        };
        let segments = status_segments(&app, &theme, 0);
        let ctx = segments.iter().find(|s| s.content.contains("46%")).unwrap();
        assert!(ctx.content.contains('█'), "预算条渲染: {}", ctx.content);
        assert!(ctx.content.contains("46%"));
    }
}

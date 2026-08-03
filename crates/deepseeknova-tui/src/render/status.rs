//! 状态行与提示行：model 标签用 accent，次要信息 dim，随焦点显示键位。

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::focus::Focus;
use crate::app::state::AppState;
use crate::theme::Theme;

/// 状态行分段。
pub fn status_segments(app: &AppState, theme: &Theme, scroll_pct: usize) -> Vec<Span<'static>> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut segments = vec![
        Span::styled(
            format!(" model={}", app.model_label),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(if app.running { " running" } else { " ready" }, dim),
        Span::styled(format!(" | turn {}", app.turn), dim),
    ];
    if let Some(u) = &app.usage {
        segments.push(Span::styled(
            format!(
                " | ↑{} ↓{} Σ{} 推理{} 缓存hit{}",
                u.prompt_tokens,
                u.completion_tokens,
                u.total_tokens,
                u.reasoning_tokens,
                u.cache_hit_tokens
            ),
            dim,
        ));
    }
    if let Some(cost) = app.total_cost_usd {
        segments.push(Span::styled(format!(" | ${cost:.6}"), dim));
    }
    // 上下文占用：prompt+completion ÷ window；>80% 黄、>95% 红警示。
    if let Some((used, window)) = app.context_usage {
        if let Some(pct) = used.checked_mul(100).and_then(|n| n.checked_div(window)) {
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
            segments.push(Span::styled(
                format!(
                    " | ctx {pct}% ({} / {})",
                    fmt_tokens(used),
                    fmt_tokens(window)
                ),
                style,
            ));
        }
    }
    segments.push(Span::styled(
        format!(
            " | lines {} | 滚动 {}%",
            app.render_line_count(),
            scroll_pct
        ),
        dim,
    ));
    segments
}

/// 上下文感知提示行：随焦点显示当前键位。
pub fn hint_for(focus: Focus) -> &'static str {
    match focus {
        Focus::Conversation => "j/k 消息导航 · Enter 折叠 · y 复制 · Esc 回输入 · /help",
        Focus::Input => "Ctrl+U 清行 · Ctrl+W 删词 · Shift+Enter 换行 · Home/End 行首尾 · Ctrl+K 面板 · /help · Esc 退出",
        Focus::Sidebar => "Tab/Ctrl+1..5 切换面板 · Esc 关闭侧边栏",
        Focus::Palette => "↑↓ 选择 · Enter 执行 · Esc 关闭 · 命令支持模糊搜索",
        Focus::Completion => "↑↓ 选择 · Enter 插入 · Esc 关闭补全",
        Focus::Confirm => "y 确认 · n/Esc 取消",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_segments_style_model_and_secondary_info() {
        let theme = Theme::default();
        let app = AppState::default();
        let segments = status_segments(&app, &theme, 0);
        assert_eq!(segments[0].content, " model=");
        assert_eq!(segments[0].style.fg, Some(theme.accent));
        assert!(segments[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(segments[1].content, " ready");
        assert!(segments[1].style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn hint_text_per_focus() {
        assert!(hint_for(Focus::Input).contains("Ctrl+K"));
        assert!(hint_for(Focus::Conversation).contains("j/k"));
        assert!(hint_for(Focus::Palette).contains("↑↓"));
        assert!(hint_for(Focus::Sidebar).contains("Tab"));
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
            .find(|s| s.content.starts_with(" | ctx"))
            .expect("ctx 段存在");
        assert_eq!(ctx.content, " | ctx 30% (30.0k / 100.0k)");
        assert_eq!(ctx.style, dim);

        // 85%：黄色警示。
        let app = AppState {
            context_usage: Some((85_000, 100_000)),
            ..Default::default()
        };
        let segments = status_segments(&app, &theme, 0);
        let ctx = segments
            .iter()
            .find(|s| s.content.starts_with(" | ctx"))
            .unwrap();
        assert_eq!(ctx.style.fg, Some(ratatui::style::Color::Yellow));

        // 97%：红色（verification_fail 的 fg）。
        let app = AppState {
            context_usage: Some((97_000, 100_000)),
            ..Default::default()
        };
        let segments = status_segments(&app, &theme, 0);
        let ctx = segments
            .iter()
            .find(|s| s.content.starts_with(" | ctx"))
            .unwrap();
        assert_eq!(ctx.style.fg, theme.verification_fail.fg);

        // 无 context_usage：不渲染 ctx 段。
        let app = AppState::default();
        let segments = status_segments(&app, &theme, 0);
        assert!(!segments.iter().any(|s| s.content.starts_with(" | ctx")));
    }

    #[test]
    fn fmt_tokens_human_readable() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1_500), "1.5k");
        assert_eq!(fmt_tokens(240_000), "240.0k");
        assert_eq!(fmt_tokens(1_200_000), "1.2M");
    }
}

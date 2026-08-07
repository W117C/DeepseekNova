//! 工作区信任确认浮层（首进带权限规则的项目时展示）。
//!
//! - **y**：信任该工作区（`TrustController::trust` 写入 `~/.deepseeknova/trusted.toml`，
//!   `PermissionGate::set_trusted(true)` 解锁项目层 allow 规则）；
//! - **n / Esc**：保持 untrusted（fail-closed，项目层 allow 规则继续降级为 ask）。

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::state::TrustPrompt;
use crate::i18n::{Key, Tr};
use crate::theme::Theme;

/// 信任确认浮层渲染：标题 + 正文（项目路径 + 规则数 + 后果说明）+ y/n 提示。
pub fn render_trust_prompt(prompt: &TrustPrompt, tr: Tr, theme: &Theme, f: &mut Frame, area: Rect) {
    let border = Style::default().fg(theme
        .verification_fail
        .fg
        .unwrap_or(ratatui::style::Color::Yellow));
    let mut lines = Vec::new();
    lines.push(Line::from(Span::styled(
        tr.t(Key::TrustTitle),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    for line in tr
        .t_args(
            Key::TrustPromptBody,
            &[
                ("n", &prompt.rule_count.to_string()),
                ("root", &prompt.workspace_root.display().to_string()),
            ],
        )
        .lines()
    {
        lines.push(Line::from(Span::styled(line.to_string(), theme.system)));
    }
    lines.push(Line::from(Span::styled(
        tr.t(Key::TrustHint),
        Style::default().fg(theme.accent),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(Line::from(Span::styled(tr.t(Key::TrustTitle), border)));
    let widget = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(widget, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample() -> TrustPrompt {
        TrustPrompt {
            workspace_root: PathBuf::from("/some/workspace"),
            rule_count: 3,
        }
    }

    #[test]
    fn renders_without_panic() {
        let theme = Theme::default();
        let tr = Tr::new(crate::i18n::Lang::En);
        let buf = ratatui::backend::TestBackend::new(60, 10);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_trust_prompt(&sample(), tr, &theme, f, area);
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Trust workspace"), "{text}");
        assert!(text.contains("/some/workspace"), "{text}");
        assert!(text.contains("y trust"), "{text}");
    }

    #[test]
    fn renders_zh() {
        let theme = Theme::default();
        let tr = Tr::new(crate::i18n::Lang::Zh);
        let buf = ratatui::backend::TestBackend::new(60, 10);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_trust_prompt(&sample(), tr, &theme, f, area);
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.replace(' ', "").contains("信任该工作区"), "{text}");
    }
}

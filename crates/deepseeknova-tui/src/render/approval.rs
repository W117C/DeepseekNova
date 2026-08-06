//! 权限审批确认浮层（Claude Code Confirmation context 的轻量版）：
//! 标题 + 描述 + `y 允许 / n 拒绝` 键位提示；`n` 后可输入反馈说明
//! （反馈随拒绝回给 agent 侧——本期只回 bool，反馈文案进 echo 供用户留痕）。

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::approval::ApprovalRequest;
use crate::theme::Theme;

/// 审批浮层渲染：标题「🔒 请求授权」+ 请求内容 + y/n 提示。
pub fn render_approval(req: &ApprovalRequest, theme: &Theme, f: &mut Frame, area: Rect) {
    let border = Style::default().fg(theme
        .verification_fail
        .fg
        .unwrap_or(ratatui::style::Color::Red));
    let mut lines = Vec::new();
    lines.push(Line::from(Span::styled(
        req.title.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    if let Some(desc) = &req.description {
        for line in desc.lines() {
            lines.push(Line::from(Span::styled(line.to_string(), theme.system)));
        }
    }
    lines.push(Line::from(Span::styled(
        "y 允许 · n 拒绝 · Esc 拒绝",
        Style::default().fg(theme.accent),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(Line::from(Span::styled("🔒 请求授权", border)));
    let widget = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(widget, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    fn sample() -> ApprovalRequest {
        let (reply, _rx) = oneshot::channel();
        ApprovalRequest {
            id: "a1".into(),
            title: "运行命令".into(),
            description: Some("rm -rf /tmp/x".into()),
            reply,
        }
    }

    #[test]
    fn renders_without_panic() {
        let theme = Theme::default();
        let buf = ratatui::backend::TestBackend::new(40, 10);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_approval(&sample(), &theme, f, area);
            })
            .unwrap();
    }
}

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
    // 风险标签（agent Ask 描述前缀 `[风险:只读|非只读|危险]`）。
    let mut risk: Option<String> = None;
    if let Some(desc) = &req.description {
        let mut rest = desc.lines();
        if let Some(first) = rest.next() {
            if let Some(label) = first
                .strip_prefix("[风险:")
                .and_then(|s| s.strip_suffix(']'))
            {
                risk = Some(label.to_string());
            } else {
                lines.push(Line::from(Span::styled(first.to_string(), theme.system)));
            }
        }
        for line in rest {
            // 命令/参数以等宽纪律展示（终端本就等宽，用缩进 + dim 区分）。
            lines.push(Line::from(Span::styled(format!("  {line}"), theme.system)));
        }
    }
    if let Some(label) = risk {
        let style = match label.as_str() {
            "只读" => Style::default().fg(theme.accent),
            "非只读" => Style::default().fg(ratatui::style::Color::Yellow),
            "危险" => border,
            _ => theme.system,
        };
        lines.insert(
            1,
            Line::from(Span::styled(format!(" 风险：{label}"), style)),
        );
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

    #[test]
    fn renders_risk_label_and_mono_command() {
        let (reply, _rx) = oneshot::channel();
        let req = ApprovalRequest {
            id: "a2".into(),
            title: "Allow tool: bash".into(),
            description: Some("[风险:非只读]\n{\"command\": \"rm -rf /tmp/x\"}".into()),
            reply,
        };
        let theme = Theme::default();
        let buf = ratatui::backend::TestBackend::new(50, 12);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_approval(&req, &theme, f, area);
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        // ratatui 宽字符在缓冲区占双格，去空格后做子串断言。
        let flat = text.replace(' ', "");
        assert!(flat.contains("风险：非只读"), "风险标签: {text}");
        assert!(text.contains("rm"), "mono 命令完整展示: {text}");
        assert!(text.contains("/tmp/x"), "mono 命令完整展示: {text}");
    }
}

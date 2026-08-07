//! 权限审批确认浮层（Claude Code Confirmation context 的轻量版）：
//! 标题 + 描述 + `y 允许 / n 拒绝` 键位提示；`n` 后可输入反馈说明
//! （反馈随拒绝回给 agent 侧——本期只回 bool，反馈文案进 echo 供用户留痕）。

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::approval::ApprovalRequest;
use crate::i18n::{Key, Tr};
use crate::theme::Theme;

/// 风险标签词表键（识别 agent Ask 描述前缀 `[风险:只读|非只读|危险]` 的
/// 中/英两形态）；未知标签回退 None，原样展示。
fn risk_label_key(label: &str) -> Option<Key> {
    match label {
        "只读" | "read-only" => Some(Key::RiskReadonly),
        "非只读" | "non-read-only" => Some(Key::RiskNonReadonly),
        "危险" | "dangerous" => Some(Key::RiskDanger),
        _ => None,
    }
}

/// 审批浮层渲染：标题「🔒 请求授权」+ 请求内容 + 当前权限模式 + y/n 提示。
/// 文案按 `tr` 语言取词表。`mode` 为当前权限模式预设（None = 旧行为，不显示）。
pub fn render_approval(
    req: &ApprovalRequest,
    mode: Option<deepseeknova_permission::PermissionMode>,
    tr: Tr,
    theme: &Theme,
    f: &mut Frame,
    area: Rect,
) {
    let border = Style::default().fg(theme
        .verification_fail
        .fg
        .unwrap_or(ratatui::style::Color::Red));
    let mut lines = Vec::new();
    lines.push(Line::from(Span::styled(
        req.title.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    // 当前权限模式上下文（安全可见性：审批时用户看到预设裁决强度）。
    if let Some(m) = mode {
        lines.push(Line::from(Span::styled(
            tr.t_args(
                Key::ApprovalModeLine,
                &[(
                    "mode",
                    tr.t(crate::app::state::permission_mode_label(Some(m))),
                )],
            ),
            Style::default().add_modifier(Modifier::DIM),
        )));
    }
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
        let style = match risk_label_key(&label) {
            Some(Key::RiskReadonly) => Style::default().fg(theme.accent),
            Some(Key::RiskNonReadonly) => Style::default().fg(ratatui::style::Color::Yellow),
            Some(Key::RiskDanger) => border,
            _ => theme.system,
        };
        let display = risk_label_key(&label)
            .map(|k| tr.t(k))
            .unwrap_or(label.as_str());
        lines.insert(
            1,
            Line::from(Span::styled(
                tr.t_args(Key::RiskLabel, &[("label", display)]),
                style,
            )),
        );
    }
    lines.push(Line::from(Span::styled(
        tr.t(Key::ApprovalHint),
        Style::default().fg(theme.accent),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(Line::from(Span::styled(tr.t(Key::ApprovalTitle), border)));
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
        let tr = Tr::new(crate::i18n::Lang::En);
        let buf = ratatui::backend::TestBackend::new(40, 10);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_approval(&sample(), None, tr, &theme, f, area);
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
        let tr = Tr::new(crate::i18n::Lang::Zh);
        let buf = ratatui::backend::TestBackend::new(50, 12);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_approval(&req, None, tr, &theme, f, area);
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

        // 英文模式：风险标签走英文词表。
        let tr_en = Tr::new(crate::i18n::Lang::En);
        let buf = ratatui::backend::TestBackend::new(50, 12);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_approval(&req, None, tr_en, &theme, f, area);
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            text.replace(' ', "").contains("Risk:non-read-only"),
            "英文风险标签: {text}"
        );
    }

    #[test]
    fn renders_current_mode_context() {
        let (reply, _rx) = oneshot::channel();
        let req = ApprovalRequest {
            id: "a3".into(),
            title: "Allow tool: write_file".into(),
            description: None,
            reply,
        };
        let theme = Theme::default();
        let tr = Tr::new(crate::i18n::Lang::En);
        let buf = ratatui::backend::TestBackend::new(50, 12);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_approval(
                    &req,
                    Some(deepseeknova_permission::PermissionMode::AcceptEdits),
                    tr,
                    &theme,
                    f,
                    area,
                );
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            text.replace(' ', "")
                .contains("Currentpermissionmode:accept_edits"),
            "审批浮层应显示当前模式: {text}"
        );
        // 中文模式：模式名本身中英一致（accept_edits）。
        let tr_zh = Tr::new(crate::i18n::Lang::Zh);
        let buf = ratatui::backend::TestBackend::new(50, 12);
        let mut terminal = ratatui::Terminal::new(buf).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render_approval(
                    &req,
                    Some(deepseeknova_permission::PermissionMode::AcceptEdits),
                    tr_zh,
                    &theme,
                    f,
                    area,
                );
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            text.replace(' ', "").contains("当前权限模式：accept_edits"),
            "{text}"
        );
    }
}

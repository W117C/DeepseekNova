//! 输入框 markdown 行级着色：把多行输入文本切成带样式的 Span 序列。
//!
//! 规则（逐行、不做嵌套解析）：
//! - `#` 开头的标题行 → `theme.title`；
//! - `- `/`* `/`数字. ` 列表行 → 前缀用 dim+accent，正文保持默认；
//! - `> ` 引用行 → `theme.reasoning` 风格；
//! - ` ``` ` 围栏切换代码态，围栏行用 accent，代码内容用 `theme.tool`。

use ratatui::style::Style;
use ratatui::text::Span;

use crate::theme::Theme;

/// 行级 markdown 着色：返回渲染可直接消费的 Span 序列（行间以 `\n` 分隔）。
pub fn md_spans(text: &str, theme: &Theme) -> Vec<Span<'static>> {
    let plain = Style::default();
    let mut spans = Vec::new();
    let mut in_code = false;
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            spans.push(Span::styled("\n", plain));
        }
        // 围栏行：切换代码态，行本身用 accent 强调
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            spans.push(Span::styled(
                line.to_string(),
                Style::default().fg(theme.accent),
            ));
            continue;
        }
        // 代码态内不再做任何 markdown 解析
        if in_code {
            spans.push(Span::styled(line.to_string(), theme.tool));
            continue;
        }
        // 标题
        if line.starts_with('#') {
            spans.push(Span::styled(line.to_string(), theme.title));
            continue;
        }
        // 引用
        if line.starts_with("> ") {
            spans.push(Span::styled(line.to_string(), theme.reasoning));
            continue;
        }
        // 列表：前缀 dim+accent，正文默认
        if let Some(prefix) = list_prefix(line) {
            let style = Style::default().fg(theme.accent).add_modifier(theme.dim);
            spans.push(Span::styled(prefix.to_string(), style));
            spans.push(Span::styled(line[prefix.len()..].to_string(), plain));
            continue;
        }
        spans.push(Span::styled(line.to_string(), plain));
    }
    spans
}

/// 列表前缀：`- `/`* ` 或 `数字. `，返回前缀字节区间；否则返回 None。
fn list_prefix(line: &str) -> Option<&str> {
    if line.starts_with("- ") || line.starts_with("* ") {
        return Some(&line[..2]);
    }
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && line[i..].starts_with(". ") {
        Some(&line[..i + 2])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier};

    #[test]
    fn heading_uses_title_style() {
        let t = Theme::default();
        let spans = md_spans("# 标题", &t);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "# 标题");
        assert_eq!(spans[0].style, t.title);
    }

    #[test]
    fn list_prefix_dim_accent_and_plain_body() {
        let t = Theme::default();
        for text in ["- 项目一", "* 项目二", "3. 项目三", "10. 十"] {
            let spans = md_spans(text, &t);
            assert_eq!(spans.len(), 2, "前缀 + 正文两段: {text}");
            assert_eq!(spans[0].style.fg, Some(t.accent), "{text}");
            assert!(
                spans[0].style.add_modifier.contains(t.dim),
                "{text} 前缀应带 dim"
            );
            assert!(!spans[0].style.add_modifier.contains(Modifier::BOLD));
            assert_eq!(spans[1].style, Style::default(), "{text} 正文默认");
        }
    }

    #[test]
    fn plain_line_and_numbered_prefix_distinction() {
        let t = Theme::default();
        // 无点的数字行不是列表
        let spans = md_spans("42", &t);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].style, Style::default());
        // 纯文本行
        let spans = md_spans("hello world", &t);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].style, Style::default());
    }

    #[test]
    fn quote_uses_reasoning_style() {
        let t = Theme::default();
        let spans = md_spans("> 引用内容", &t);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "> 引用内容");
        assert_eq!(spans[0].style, t.reasoning);
    }

    #[test]
    fn fenced_code_toggles_and_inlines_newlines() {
        let t = Theme::default();
        let text = "```rust\nfn main() {}\n```\n# 之后是标题";
        let spans = md_spans(text, &t);
        // 行数 5 → 4 行 + 3 个换行 = 7 段
        assert_eq!(spans.len(), 7);
        assert_eq!(spans[0].content, "```rust");
        assert_eq!(spans[0].style.fg, Some(t.accent), "围栏行 accent");
        assert_eq!(spans[1].content, "\n");
        assert_eq!(spans[2].content, "fn main() {}");
        assert_eq!(spans[2].style, t.tool, "代码内容用 tool(dim)");
        assert_eq!(spans[4].content, "```");
        assert_eq!(spans[4].style.fg, Some(t.accent));
        assert_eq!(spans[6].content, "# 之后是标题");
        assert_eq!(spans[6].style, t.title, "围栏关闭后恢复 markdown 解析");
    }

    #[test]
    fn code_fence_ignores_heading_inside() {
        let t = Theme::default();
        // 围栏内 `# 注释` 不得按标题着色
        let spans = md_spans("```\n# 注释\n```", &t);
        assert_eq!(spans.len(), 5);
        assert_eq!(spans[2].content, "# 注释");
        assert_eq!(spans[2].style, t.tool);
    }

    #[test]
    fn empty_text_yields_empty_spans() {
        let t = Theme::default();
        assert!(md_spans("", &t).is_empty());
    }

    #[test]
    fn accent_fg_is_derived_from_theme() {
        let t = Theme::default();
        let spans = md_spans("- x", &t);
        assert_eq!(spans[0].style.fg, Some(Color::Cyan), "默认 codex accent");
    }
}

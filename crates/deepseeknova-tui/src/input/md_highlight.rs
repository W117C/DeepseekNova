//! 输入框 markdown 行级着色：把多行输入文本切成带样式的 Span 序列。
//!
//! 规则（逐行、不做嵌套解析）：
//! - `#` 开头的标题行 → `theme.title`；
//! - `- `/`* `/`+ `/`数字. ` 列表行 → 前缀用 dim+accent，正文保持默认；
//! - `> ` 引用行 → `theme.reasoning` 风格；
//! - ` ``` ` 围栏切换代码态，围栏行用 accent，代码内容用 `theme.tool`；
//! - 普通行内再做 span 级着色：行内反引号段用 `theme.tool`，
//!   `[text](url)` 链接的 label 用 accent+下划线、url 用 `theme.tool`。

use ratatui::style::{Modifier, Style};
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
            spans.extend(inline_spans(line, theme, theme.title));
            continue;
        }
        // 引用
        if line.starts_with("> ") {
            spans.extend(inline_spans(line, theme, theme.reasoning));
            continue;
        }
        // 列表：前缀 dim+accent，正文默认（正文内仍做行内着色）
        if let Some(prefix) = list_prefix(line) {
            let style = Style::default().fg(theme.accent).add_modifier(theme.dim);
            spans.push(Span::styled(prefix.to_string(), style));
            spans.extend(inline_spans(&line[prefix.len()..], theme, plain));
            continue;
        }
        // 普通行：整行按行内规则拆分着色
        spans.extend(inline_spans(line, theme, plain));
    }
    spans
}

/// 行内 markdown 着色：把一行文本拆成 Span 序列。
/// 识别行内代码（一对反引号包裹的段，含反引号一起用 `theme.tool` 着色）
/// 与行链接 `[text](url)`（label 用 accent+下划线、url 用 `theme.tool`）。
/// 未闭合的反引号/链接按普通文本处理；`base` 为未命中任何规则时的底色样式。
pub fn inline_spans(line: &str, theme: &Theme, base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut plain = String::new();
    let mut i = 0;
    while i < line.len() {
        let Some(ch) = line[i..].chars().next() else {
            break;
        };
        match ch {
            // 行内代码：`...`
            '`' => {
                if let Some(rel) = line[i + 1..].find('`') {
                    let end = i + 1 + rel;
                    let end_ch = end + 1;
                    flush_plain(&mut spans, &mut plain, base);
                    spans.push(Span::styled(line[i..end_ch].to_string(), theme.tool));
                    i = end_ch;
                } else {
                    plain.push(ch);
                    i += 1;
                }
            }
            // 行链接：[text](url)
            '[' => {
                if let Some(rel) = line[i..].find("](") {
                    let close = i + rel;
                    if let Some(rel_end) = line[close + 2..].find(')') {
                        let end = close + 2 + rel_end;
                        let end_ch = end + 1;
                        flush_plain(&mut spans, &mut plain, base);
                        let label = &line[i + 1..close];
                        let url = &line[close + 2..end];
                        spans.push(Span::styled("[".to_string(), base));
                        spans.push(Span::styled(
                            label.to_string(),
                            Style::default()
                                .fg(theme.accent)
                                .add_modifier(Modifier::UNDERLINED),
                        ));
                        spans.push(Span::styled("](".to_string(), base));
                        spans.push(Span::styled(url.to_string(), theme.tool));
                        spans.push(Span::styled(")".to_string(), base));
                        i = end_ch;
                    } else {
                        plain.push(ch);
                        i += 1;
                    }
                } else {
                    plain.push(ch);
                    i += 1;
                }
            }
            _ => {
                plain.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    flush_plain(&mut spans, &mut plain, base);
    spans
}

/// 把累积的普通文本段刷成一个 Span 并清空缓冲区。
fn flush_plain(spans: &mut Vec<Span<'static>>, plain: &mut String, base: Style) {
    if !plain.is_empty() {
        spans.push(Span::styled(std::mem::take(plain), base));
    }
}

/// 列表前缀：`- `/`* `/`+ ` 或 `数字. `，返回前缀字节区间；否则返回 None。
fn list_prefix(line: &str) -> Option<&str> {
    if line.starts_with("- ") || line.starts_with("* ") || line.starts_with("+ ") {
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
        assert_eq!(
            spans[0].style.fg,
            Some(Color::Rgb(77, 107, 254)),
            "默认 DeepSeek accent"
        );
    }

    #[test]
    fn plus_list_prefix_is_dim_accent() {
        let t = Theme::default();
        let spans = md_spans("+ 加法", &t);
        assert_eq!(spans.len(), 2, "前缀 + 正文两段");
        assert_eq!(spans[0].content, "+ ");
        assert_eq!(spans[0].style.fg, Some(t.accent));
        assert!(spans[0].style.add_modifier.contains(t.dim));
        assert_eq!(spans[1].content, "加法");
        assert_eq!(spans[1].style, Style::default());
    }

    #[test]
    fn inline_code_segments_use_tool_style() {
        let t = Theme::default();
        let spans = md_spans("使用 `let x = 1;` 说明", &t);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "使用 ");
        assert_eq!(spans[0].style, Style::default());
        assert_eq!(spans[1].content, "`let x = 1;`");
        assert_eq!(spans[1].style, t.tool, "行内代码（含反引号）用 tool(dim)");
        assert_eq!(spans[2].content, " 说明");
        assert_eq!(spans[2].style, Style::default());
    }

    #[test]
    fn link_syntax_colors_label_and_url() {
        let t = Theme::default();
        let spans = md_spans("见 [文档](https://example.com) 结尾", &t);
        assert_eq!(spans.len(), 7);
        assert_eq!(spans[0].content, "见 ");
        assert_eq!(spans[2].content, "文档");
        assert_eq!(spans[2].style.fg, Some(t.accent), "链接 label 用 accent");
        assert!(
            spans[2].style.add_modifier.contains(Modifier::UNDERLINED),
            "链接 label 带下划线"
        );
        assert_eq!(spans[4].content, "https://example.com");
        assert_eq!(spans[4].style, t.tool, "url 用 tool(dim)");
        assert_eq!(spans[6].content, " 结尾");
    }

    #[test]
    fn unclosed_inline_code_and_link_stay_plain() {
        let t = Theme::default();
        // 未闭合反引号：整体按普通文本
        let spans = md_spans("孤零零的 `反引号", &t);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "孤零零的 `反引号");
        assert_eq!(spans[0].style, Style::default());
        // 未闭合链接：整体按普通文本
        let spans = md_spans("见 [文档](https://example.com 未闭合", &t);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].style, Style::default());
    }

    #[test]
    fn inline_highlight_applies_inside_list_body() {
        let t = Theme::default();
        let spans = md_spans("- 安装 `cargo` 或看 [说明](https://r.rs)", &t);
        // 前缀 + 正文三段（正文、代码段、链接 5 段 + 末尾空？）——按整行拆分：
        // [前缀] [安装 ] [`cargo`] [ 或看 ] [[] [说明] [](] [https://r.rs] [)] = 9
        assert_eq!(spans.len(), 9);
        assert_eq!(spans[0].content, "- ");
        assert_eq!(spans[0].style.fg, Some(t.accent));
        assert_eq!(spans[2].content, "`cargo`");
        assert_eq!(spans[2].style, t.tool);
        assert_eq!(spans[5].content, "说明");
        assert_eq!(spans[5].style.fg, Some(t.accent));
        assert_eq!(spans[7].content, "https://r.rs");
        assert_eq!(spans[7].style, t.tool);
    }
}

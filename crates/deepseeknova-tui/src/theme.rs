//! 语义配色主题：`DEEPSEEKNOVA_THEME` 环境变量三档（codex/dark/light）。
//!
//! 默认 `codex` 完全等价于旧版硬编码的 Codex 语义色（user/status=cyan、
//! agent=magenta、次要=dim、成功=green、失败=red），零配置行为不变。
//! 不使用自定义颜色名，深浅终端均通读。

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::model::conversation::LineKind;

/// 环境变量名。
pub const THEME_ENV: &str = "DEEPSEEKNOVA_THEME";

/// 语义色映射表：渲染样式全部经此解析，杜绝散落硬编码。
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub user: Style,
    pub agent: Style,
    pub reasoning: Style,
    pub tool: Style,
    pub tool_result: Style,
    pub verification_ok: Style,
    pub verification_fail: Style,
    pub system: Style,
    pub error: Style,
    pub paused: Style,
    /// 强调色（状态行 model 标签、输入框提示符、diff 块头）。
    pub accent: Color,
    /// 次要信息修饰符（状态行/提示行的次级文本）。
    pub dim: Modifier,
    /// 面板边框样式。
    pub border: Style,
    /// 标题强调（面板标题、选中项）。
    pub title: Style,
    /// 选中消息的背景高亮。
    pub selection: Style,
}

/// 默认 DeepSeek 色板：靛蓝主色 #4D6BFE（品牌蓝），
/// 用户/强调同色系、agent 用柔蓝紫，语义色（成功/失败/警示）保留。
impl Default for Theme {
    fn default() -> Self {
        Self {
            user: Style::default()
                .fg(Color::Rgb(77, 107, 254))
                .add_modifier(Modifier::BOLD),
            agent: Style::default().fg(Color::Rgb(122, 140, 255)),
            reasoning: Style::default()
                .add_modifier(Modifier::DIM)
                .add_modifier(Modifier::ITALIC),
            tool: Style::default().add_modifier(Modifier::DIM),
            tool_result: Style::default().add_modifier(Modifier::DIM),
            verification_ok: Style::default().fg(Color::Green),
            verification_fail: Style::default().fg(Color::Red),
            system: Style::default().add_modifier(Modifier::DIM),
            error: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            paused: Style::default().fg(Color::Rgb(77, 107, 254)),
            accent: Color::Rgb(77, 107, 254),
            dim: Modifier::DIM,
            border: Style::default().add_modifier(Modifier::DIM),
            title: Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
            selection: Style::default()
                .bg(Color::Rgb(38, 50, 100))
                .add_modifier(Modifier::REVERSED),
        }
    }
}

impl Theme {
    /// 行类型 → 样式（旧 `style_for` 的等价迁移）。
    pub fn style_for(&self, kind: LineKind) -> Style {
        match kind {
            LineKind::User => self.user,
            LineKind::Agent => self.agent,
            LineKind::Reasoning => self.reasoning,
            LineKind::Tool => self.tool,
            LineKind::ToolResult => self.tool_result,
            LineKind::Verification { passed } => {
                if passed {
                    self.verification_ok
                } else {
                    self.verification_fail
                }
            }
            LineKind::System => self.system,
            LineKind::Error => self.error,
            LineKind::Paused => self.paused,
        }
    }

    /// diff 行级高亮：`+` 新增=green、`-` 删除=red、`@@` 块头=accent，
    /// 其余行沿用 `base`；`+++`/`---` 文件头不改色。工具结果行的 UI
    /// 前缀 `  → ` 不参与行首判定（先剥掉再判断），否则 git diff 输出
    /// 永远染不上色。
    pub fn diff_spans(&self, text: &str, base: Style) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        for (i, line) in text.lines().enumerate() {
            if i > 0 {
                spans.push(Span::styled("\n", base));
            }
            let content = line.strip_prefix("  → ").unwrap_or(line);
            let style = if content.starts_with("+++") || content.starts_with("---") {
                base
            } else if content.starts_with("@@") {
                Style::default().fg(self.accent)
            } else if content.starts_with('+') {
                self.verification_ok
            } else if content.starts_with('-') {
                self.verification_fail
            } else {
                base
            };
            spans.push(Span::styled(line.to_string(), style));
        }
        spans
    }
}

/// 主题名解析结果：主题 + 可选回退提示（未知值回退 codex 并携带提示文本）。
pub fn theme_from_env() -> (Theme, Option<String>) {
    theme_from_name(&std::env::var(THEME_ENV).unwrap_or_default())
}

/// 按主题名解析（纯函数，便于测试；未知/空回退 deepseek 默认）。
pub fn theme_from_name(name: &str) -> (Theme, Option<String>) {
    match name {
        "" | "codex" | "deepseek" => (Theme::default(), None),
        "dark" => (dark_theme(), None),
        "light" => (light_theme(), None),
        other => (
            Theme::default(),
            Some(format!(
                "未知主题 '{other}'（deepseek|dark|light），已回退 deepseek"
            )),
        ),
    }
}

/// 深色终端强调版：accent 用 DeepSeek 蓝的亮化版，标题用亮白，对比度更高。
fn dark_theme() -> Theme {
    Theme {
        accent: Color::Rgb(110, 140, 255),
        title: Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
        ..Theme::default()
    }
}

/// 浅色终端版：前景改用深色系保证对比度，agent 用深品红。
fn light_theme() -> Theme {
    Theme {
        user: Style::default()
            .fg(Color::Rgb(0, 90, 150))
            .add_modifier(Modifier::BOLD),
        agent: Style::default().fg(Color::Rgb(150, 40, 120)),
        reasoning: Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::ITALIC),
        tool: Style::default().fg(Color::Gray),
        tool_result: Style::default().fg(Color::Gray),
        verification_ok: Style::default().fg(Color::Rgb(0, 120, 60)),
        verification_fail: Style::default().fg(Color::Rgb(190, 40, 40)),
        system: Style::default().fg(Color::Gray),
        error: Style::default()
            .fg(Color::Rgb(190, 40, 40))
            .add_modifier(Modifier::BOLD),
        paused: Style::default().fg(Color::Rgb(0, 90, 150)),
        accent: Color::Rgb(0, 90, 150),
        dim: Modifier::DIM,
        border: Style::default().fg(Color::Gray),
        title: Style::default()
            .fg(Color::Rgb(40, 40, 40))
            .add_modifier(Modifier::BOLD),
        selection: Style::default()
            .bg(Color::Gray)
            .add_modifier(Modifier::REVERSED),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_deepseek_semantic_palette() {
        let t = Theme::default();
        assert_eq!(t.user.fg, Some(Color::Rgb(77, 107, 254)), "DeepSeek 品牌蓝");
        assert!(t.user.add_modifier.contains(Modifier::BOLD));
        assert_eq!(t.agent.fg, Some(Color::Rgb(122, 140, 255)), "agent 柔蓝紫");
        assert_eq!(t.accent, Color::Rgb(77, 107, 254));
        assert_eq!(t.reasoning.fg, None);
        assert!(t.reasoning.add_modifier.contains(Modifier::DIM));
        assert!(t.reasoning.add_modifier.contains(Modifier::ITALIC));
        for kind in [LineKind::Tool, LineKind::ToolResult, LineKind::System] {
            let s = t.style_for(kind);
            assert_eq!(s.fg, None, "{kind:?} 应为 dim 次要样式");
            assert!(s.add_modifier.contains(Modifier::DIM));
        }
        assert_eq!(
            t.style_for(LineKind::Verification { passed: true }).fg,
            Some(Color::Green)
        );
        assert_eq!(
            t.style_for(LineKind::Verification { passed: false }).fg,
            Some(Color::Red)
        );
        assert_eq!(t.style_for(LineKind::Error).fg, Some(Color::Red));
        assert_eq!(
            t.style_for(LineKind::Paused).fg,
            Some(Color::Rgb(77, 107, 254))
        );
    }

    #[test]
    fn diff_spans_highlight_add_del_and_hunk_header() {
        let t = Theme::default();
        let base = Style::default().add_modifier(Modifier::DIM);
        let spans = t.diff_spans("a\n+b\n-c\n@@ -1,2 +1,2 @@\n context", base);
        assert_eq!(spans[0].content, "a");
        assert_eq!(spans[0].style, base);
        assert_eq!(spans[2].content, "+b");
        assert_eq!(spans[2].style, t.verification_ok);
        assert_eq!(spans[4].content, "-c");
        assert_eq!(spans[4].style, t.verification_fail);
        assert_eq!(spans[6].content, "@@ -1,2 +1,2 @@");
        assert_eq!(spans[6].style.fg, Some(t.accent));
        assert_eq!(spans[8].content, " context");
        assert_eq!(spans[8].style, base);
        let head = t.diff_spans("+++ b/a.rs\n--- a/a.rs", base);
        assert_eq!(head[0].style, base);
        assert_eq!(head[2].style, base);
    }

    #[test]
    fn diff_spans_ignores_tool_result_prefix() {
        // 工具结果行带 `  → ` UI 前缀：剥掉前缀后仍按 diff 行首染色。
        let t = Theme::default();
        let base = Style::default();
        let spans = t.diff_spans("  → +fn new() {}\n  → -fn old() {}\n  → @@ -1 +1 @@", base);
        assert_eq!(spans[0].content, "  → +fn new() {}");
        assert_eq!(spans[0].style, t.verification_ok);
        assert_eq!(spans[2].content, "  → -fn old() {}");
        assert_eq!(spans[2].style, t.verification_fail);
        assert_eq!(spans[4].style.fg, Some(t.accent));
        // 非 diff 前缀行保持 base。
        let plain = t.diff_spans("  → note\n+real", base);
        assert_eq!(plain[0].style, base);
        assert_eq!(plain[2].style, t.verification_ok);
    }

    #[test]
    fn light_theme_uses_dark_foregrounds() {
        let t = light_theme();
        assert_eq!(t.user.fg, Some(Color::Rgb(0, 90, 150)));
        assert_eq!(t.verification_ok.fg, Some(Color::Rgb(0, 120, 60)));
        assert_eq!(t.verification_fail.fg, Some(Color::Rgb(190, 40, 40)));
        assert_eq!(t.accent, Color::Rgb(0, 90, 150));
    }

    #[test]
    fn dark_theme_boosts_accent_and_title() {
        let t = dark_theme();
        assert_eq!(t.accent, Color::Rgb(110, 140, 255), "DeepSeek 蓝亮化版");
        assert_eq!(t.title.fg, Some(Color::White));
    }

    #[test]
    fn env_parsing_routes_three_presets_and_falls_back() {
        let cases: &[(&str, bool)] = &[
            ("deepseek", false),
            ("codex", false),
            ("dark", false),
            ("light", false),
            ("unknown", true),
            ("", false),
        ];
        for (val, expect_warning) in cases {
            let (theme, warning) = theme_from_name(val);
            assert_eq!(warning.is_some(), *expect_warning, "val={val:?}");
            // 有效主题应解析出非默认差异（dark/light 改变 accent），deepseek/codex 为默认。
            if *val == "dark" {
                assert_eq!(theme.accent, Color::Rgb(110, 140, 255));
            } else if *val == "light" {
                assert_eq!(theme.accent, Color::Rgb(0, 90, 150));
            } else {
                assert_eq!(theme, Theme::default());
            }
        }
        // 未设置环境变量时等价于 deepseek（env 读取壳的语义）。
        let fallback = theme_from_name("");
        assert_eq!(fallback.0, Theme::default());
        assert!(fallback.1.is_none());
    }
}

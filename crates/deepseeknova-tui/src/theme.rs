//! 语义配色主题：`DEEPSEEKNOVA_THEME` 环境变量三档（codex/deepseek/dark/light）。
//!
//! 默认档（`codex`/`deepseek` 均映射）为 Claude Code 观感（190ac01）：消息正文
//! 用终端默认前景色、归属靠 `❯`/`⏺` 标记，品牌蓝 `#4D6BFE` 只留给 accent；
//! 次要=dim、成功=green、失败=red。不使用自定义颜色名，深浅终端均通读。

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::i18n::{Key, Tr};
use crate::model::conversation::LineKind;

/// 环境变量名。
pub const THEME_ENV: &str = "DEEPSEEKNOVA_THEME";

/// 语义色映射表：渲染样式全部经此解析，杜绝散落硬编码。
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    /// 用户消息样式。
    pub user: Style,
    /// 助手消息样式。
    pub agent: Style,
    /// 推理段样式。
    pub reasoning: Style,
    /// 工具调用行（`⏺ Tool(args)`）样式。
    pub tool: Style,
    /// 工具结果段（`⎿ result`）样式。
    pub tool_result: Style,
    /// 确定性校验通过（`✓`）样式。
    pub verification_ok: Style,
    /// 确定性校验失败（`✗`）样式。
    pub verification_fail: Style,
    /// 系统消息样式。
    pub system: Style,
    /// 错误消息样式。
    pub error: Style,
    /// 暂停状态样式。
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
    /// 成功语义色（状态行运行态圆点、token 条低占用段）。
    pub success: Color,
    /// 警示语义色（ctx 阈值 ≥80%、缺 API key 等临界提示）。
    pub warning: Color,
    /// 危险语义色（ctx 阈值 ≥95%、未配置 provider、退出警示等）。
    pub danger: Color,
    /// 中性信息语义色（token 条中占用渐变段）。
    pub info: Color,
    // ── grok build 语义位（对齐 xai-grok-pager theme.rs）────────
    /// 主文本前景色（正文、hover 高亮；Reset = 终端默认）。
    pub text_primary: Color,
    /// 次文本前景色（次要信息、hover 进度条百分比）。
    pub text_secondary: Color,
    /// 用户 accent（prompt 前缀 ❯、选中强调、fuzzy 匹配）。
    pub accent_user: Color,
    /// 错误 accent（ctx 高占用端、失败强调）。
    pub accent_error: Color,
    /// 中性灰（模态提示、未聚焦 chrome）。
    pub gray: Color,
    /// 暗灰（未聚焦 prompt 前缀/accent 线）。
    pub gray_dim: Color,
    /// 背景底色（状态栏/进度条底、模态 Clear 背景）。
    pub bg_base: Color,
    /// 高亮背景（进度条轨道、hover 行）。
    pub bg_highlight: Color,
    /// 粘贴 chip 底色（paste badge）。
    pub paste_bg: Color,
    /// 模糊匹配强调色（fzf 风格选中行文字色）。
    pub fuzzy_accent: Color,
    /// prompt 边框色（未聚焦）。
    pub prompt_border: Color,
    /// prompt 边框色（聚焦态）。
    pub prompt_border_active: Color,
}

/// 语义色调分类：状态行阈值配色、token 预算条渐变分段、徽章等
/// 通用语义色的取色入口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticTone {
    /// 成功/通过（绿色系）。
    Success,
    /// 警示/临界（黄色系）。
    Warning,
    /// 危险/失败（红色系）。
    Danger,
    /// 中性信息（青色或蓝色系）。
    Info,
}

/// 默认 DeepSeek 色板（Claude Code 观感：正文用终端默认前景色，
/// 归属靠 `❯`/`⏺` 标记区分而非整行染色；品牌蓝 #4D6BFE 只留给
/// accent——提示符、⏺ 标记、模型标签；语义色（成功/失败/警示）保留）。
impl Default for Theme {
    fn default() -> Self {
        Self {
            user: Style::default(),
            agent: Style::default(),
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
            success: Color::Green,
            warning: Color::Yellow,
            danger: Color::Red,
            info: Color::Cyan,
            text_primary: Color::Reset,
            text_secondary: Color::Rgb(140, 148, 168),
            accent_user: Color::Rgb(77, 107, 254),
            accent_error: Color::Red,
            gray: Color::Rgb(120, 128, 148),
            gray_dim: Color::Rgb(90, 98, 118),
            bg_base: Color::Rgb(20, 22, 26),
            bg_highlight: Color::Rgb(38, 44, 66),
            paste_bg: Color::Rgb(48, 60, 120),
            fuzzy_accent: Color::Rgb(77, 107, 254),
            prompt_border: Color::Rgb(90, 98, 118),
            prompt_border_active: Color::Rgb(77, 107, 254),
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

    /// 语义色调 → 主题对应语义色（状态行阈值、token 渐变分段、徽章等
    /// 统一经此取色，杜绝散落硬编码）。
    pub fn semantic(&self, tone: SemanticTone) -> Color {
        match tone {
            SemanticTone::Success => self.success,
            SemanticTone::Warning => self.warning,
            SemanticTone::Danger => self.danger,
            SemanticTone::Info => self.info,
        }
    }

    /// diff 行级高亮：`+` 新增=green、`-` 删除=red、`@@` 块头=accent，
    /// 其余行沿用 `base`；`+++`/`---` 文件头不改色。工具结果行的 UI
    /// 前缀 `  ⎿  ` 不参与行首判定（先剥掉再判断），否则 git diff 输出
    /// 永远染不上色。
    pub fn diff_spans(&self, text: &str, base: Style) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        for (i, line) in text.lines().enumerate() {
            if i > 0 {
                spans.push(Span::styled("\n", base));
            }
            let content = line.strip_prefix("  ⎿  ").unwrap_or(line);
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
pub fn theme_from_env(tr: Tr) -> (Theme, Option<String>) {
    theme_from_name(&std::env::var(THEME_ENV).unwrap_or_default(), tr)
}

/// 按主题名解析（纯函数，便于测试；未知/空回退 deepseek 默认）。
/// 回退提示文案按 `tr` 语言生成。
pub fn theme_from_name(name: &str, tr: Tr) -> (Theme, Option<String>) {
    match name {
        "" | "codex" | "deepseek" => (Theme::default(), None),
        "dark" => (dark_theme(), None),
        "light" => (light_theme(), None),
        other => (
            Theme::default(),
            Some(tr.t_args(Key::ThemeUnknownFallback, &[("theme", other)])),
        ),
    }
}

/// 深色终端强调版：accent 用 DeepSeek 蓝的亮化版，标题用亮白，对比度更高。
/// 语义色沿用深色档：成功=绿、警示=黄、危险=红、信息=青。
fn dark_theme() -> Theme {
    Theme {
        accent: Color::Rgb(110, 140, 255),
        title: Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
        success: Color::Green,
        warning: Color::Yellow,
        danger: Color::Red,
        info: Color::Cyan,
        ..Theme::default()
    }
}

/// 浅色终端版（印刷星图）：纸白底 + 墨线 + 深化品牌蓝，
/// token 与 `docs/superpowers/specs/2026-08-07-observatory-frontend-design.md`
/// §1.1 浅色档逐项对齐。
/// 语义色加深对比：成功=深绿、警示=深橙、危险=深红、信息=深蓝。
fn light_theme() -> Theme {
    Theme {
        user: Style::default(),
        agent: Style::default(),
        reasoning: Style::default()
            .fg(Color::Rgb(106, 115, 144))
            .add_modifier(Modifier::ITALIC),
        tool: Style::default().fg(Color::Rgb(106, 115, 144)),
        tool_result: Style::default().fg(Color::Rgb(106, 115, 144)),
        verification_ok: Style::default().fg(Color::Rgb(14, 122, 66)),
        verification_fail: Style::default().fg(Color::Rgb(192, 48, 58)),
        system: Style::default().fg(Color::Rgb(106, 115, 144)),
        error: Style::default()
            .fg(Color::Rgb(192, 48, 58))
            .add_modifier(Modifier::BOLD),
        paused: Style::default().fg(Color::Rgb(59, 85, 217)),
        accent: Color::Rgb(59, 85, 217),
        dim: Modifier::DIM,
        border: Style::default().fg(Color::Rgb(216, 221, 236)),
        title: Style::default()
            .fg(Color::Rgb(26, 33, 56))
            .add_modifier(Modifier::BOLD),
        selection: Style::default()
            .bg(Color::Rgb(221, 228, 251))
            .add_modifier(Modifier::REVERSED),
        success: Color::Rgb(14, 122, 66),
        warning: Color::Rgb(200, 112, 0),
        danger: Color::Rgb(192, 48, 58),
        info: Color::Rgb(59, 85, 217),
        text_primary: Color::Reset,
        text_secondary: Color::Rgb(106, 115, 144),
        accent_user: Color::Rgb(59, 85, 217),
        accent_error: Color::Rgb(192, 48, 58),
        gray: Color::Rgb(106, 115, 144),
        gray_dim: Color::Rgb(150, 158, 178),
        bg_base: Color::Rgb(248, 249, 251),
        bg_highlight: Color::Rgb(221, 228, 251),
        paste_bg: Color::Rgb(221, 228, 251),
        fuzzy_accent: Color::Rgb(59, 85, 217),
        prompt_border: Color::Rgb(216, 221, 236),
        prompt_border_active: Color::Rgb(59, 85, 217),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_deepseek_semantic_palette() {
        let t = Theme::default();
        // Claude Code 观感：user/agent 正文不染色（终端默认前景），
        // 品牌蓝只留给 accent（提示符 ❯、⏺ 标记、模型标签）。
        assert_eq!(t.user.fg, None, "user 正文用终端默认色");
        assert!(!t.user.add_modifier.contains(Modifier::BOLD));
        assert_eq!(t.agent.fg, None, "agent 正文用终端默认色");
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
    fn semantic_tone_maps_to_preset_colors() {
        let t = Theme::default();
        // 默认（deepseek/dark 同源）：成功=绿、警示=黄、危险=红、信息=青。
        assert_eq!(t.semantic(SemanticTone::Success), Color::Green);
        assert_eq!(t.semantic(SemanticTone::Warning), Color::Yellow);
        assert_eq!(t.semantic(SemanticTone::Danger), Color::Red);
        assert_eq!(t.semantic(SemanticTone::Info), Color::Cyan);
        let dark = dark_theme();
        assert_eq!(dark.semantic(SemanticTone::Success), Color::Green);
        assert_eq!(dark.semantic(SemanticTone::Warning), Color::Yellow);
        assert_eq!(dark.semantic(SemanticTone::Danger), Color::Red);
        assert_eq!(dark.semantic(SemanticTone::Info), Color::Cyan);
        // 浅色档加深：深绿/深橙/深红/深蓝。
        let light = light_theme();
        assert_eq!(
            light.semantic(SemanticTone::Success),
            Color::Rgb(14, 122, 66),
            "success=深绿 #0E7A42"
        );
        assert_eq!(
            light.semantic(SemanticTone::Warning),
            Color::Rgb(200, 112, 0),
            "warning=深橙 #C87000"
        );
        assert_eq!(
            light.semantic(SemanticTone::Danger),
            Color::Rgb(192, 48, 58),
            "danger=深红 #C0303A"
        );
        assert_eq!(
            light.semantic(SemanticTone::Info),
            Color::Rgb(59, 85, 217),
            "info=深蓝 #3B55D9"
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
        // 工具结果行带 `  ⎿  ` UI 前缀：剥掉前缀后仍按 diff 行首染色。
        let t = Theme::default();
        let base = Style::default();
        let spans = t.diff_spans(
            "  ⎿  +fn new() {}\n  ⎿  -fn old() {}\n  ⎿  @@ -1 +1 @@",
            base,
        );
        assert_eq!(spans[0].content, "  ⎿  +fn new() {}");
        assert_eq!(spans[0].style, t.verification_ok);
        assert_eq!(spans[2].content, "  ⎿  -fn old() {}");
        assert_eq!(spans[2].style, t.verification_fail);
        assert_eq!(spans[4].style.fg, Some(t.accent));
        // 非 diff 前缀行保持 base。
        let plain = t.diff_spans("  ⎿  note\n+real", base);
        assert_eq!(plain[0].style, base);
        assert_eq!(plain[2].style, t.verification_ok);
    }

    #[test]
    fn light_theme_uses_dark_foregrounds() {
        let t = light_theme();
        // 印刷星图浅色档：正文默认前景（终端深色）、accent 深化品牌蓝、纸白选中底。
        assert_eq!(t.user.fg, None, "user 正文用终端默认色");
        assert_eq!(t.agent.fg, None, "agent 正文用终端默认色");
        assert_eq!(
            t.verification_ok.fg,
            Some(Color::Rgb(14, 122, 66)),
            "ok=#0E7A42"
        );
        assert_eq!(
            t.verification_fail.fg,
            Some(Color::Rgb(192, 48, 58)),
            "fail=#C0303A"
        );
        assert_eq!(t.accent, Color::Rgb(59, 85, 217), "accent=#3B55D9");
        assert_eq!(
            t.border.fg,
            Some(Color::Rgb(216, 221, 236)),
            "hairline=#D8DDEC"
        );
        assert_eq!(
            t.selection.bg,
            Some(Color::Rgb(221, 228, 251)),
            "selection=#DDE4FB"
        );
        assert_eq!(
            t.tool.fg,
            Some(Color::Rgb(106, 115, 144)),
            "ink-dim=#6A7390"
        );
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
            let (theme, warning) = theme_from_name(val, Tr::new(crate::i18n::Lang::En));
            assert_eq!(warning.is_some(), *expect_warning, "val={val:?}");
            // 有效主题应解析出非默认差异（dark/light 改变 accent），deepseek/codex 为默认。
            if *val == "dark" {
                assert_eq!(theme.accent, Color::Rgb(110, 140, 255));
            } else if *val == "light" {
                assert_eq!(theme.accent, Color::Rgb(59, 85, 217));
            } else {
                assert_eq!(theme, Theme::default());
            }
        }
        // 未设置环境变量时等价于 deepseek（env 读取壳的语义）。
        let fallback = theme_from_name("", Tr::new(crate::i18n::Lang::En));
        assert_eq!(fallback.0, Theme::default());
        assert!(fallback.1.is_none());
    }
}

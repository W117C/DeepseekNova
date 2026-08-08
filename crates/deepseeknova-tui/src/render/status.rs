//! 状态行与提示行：model 标签用 accent，次要信息 dim，随焦点显示键位。

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::app::actions::ActionContext;
use crate::app::focus::Focus;
use crate::app::state::AppState;
use crate::i18n::{Key, Tr};
use crate::theme::Theme;

/// 状态行分段优先级（数字越大越先保留）。宽度不足时按此顺序丢弃：
/// 先丢 ctx → 模型/运行态/权限模式 → 退出警示。
/// Claude Code 风格精简：只保留运行态 + 模型 + ctx 占用 + 权限模式 +
/// 退出警示；成本/turn/usage/lines 等明细挪到 `/cost` 等命令查看。
const PRIO_QUIT: u8 = 10;
const PRIO_MODEL: u8 = 9;
const PRIO_MODE: u8 = 9;
const PRIO_CTX: u8 = 8;
/// 工作区/git 分支：信息性最低优先级，窄终端最先丢弃。
const PRIO_WORKSPACE: u8 = 2;

/// 状态行分段（Claude Code 风格精简，语义色分层）：
/// 1) 运行态 + 模型（主信息，accent/bold）
/// 2) token 预算条（资源，阈值变色）
/// 3) 权限模式（安全相关，窄终端优先保留）
///
/// 仅测试使用：生产渲染走 [`fit_status_line`]（宽度感知）。保留裸段构造
/// 供测试直接断言各段内容与样式。
#[cfg(test)]
pub fn status_segments(app: &AppState, theme: &Theme) -> Vec<Span<'static>> {
    tagged_segments(app, theme)
        .into_iter()
        .map(|(_, s)| s)
        .collect()
}

/// 带优先级的 segment 构建（fit 与直接展示共用，避免两份逻辑漂移）。
fn tagged_segments(app: &AppState, theme: &Theme) -> Vec<(u8, Span<'static>)> {
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
    segments.push((PRIO_MODEL, state));
    segments.push((PRIO_MODEL, Span::styled(" ", dim)));
    // 模型 + effort 后缀（thinking off 时无后缀）。
    let model_text = if app.effort_label.is_empty() {
        app.model_label.clone()
    } else {
        format!("{}·{}", app.model_label, app.effort_label)
    };
    segments.push((
        PRIO_MODEL,
        Span::styled(
            model_text,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
    ));
    // ── 组 0：工作区 + git 分支（信息性，最优先丢弃）──
    if let Some(branch) = &app.git_branch {
        segments.push((PRIO_WORKSPACE, Span::styled(format!(" ⎇ {branch}"), dim)));
    } else if !app.workspace_cwd.is_empty() {
        // 非 git 工作区：显示目录 basename（兼容 / 与 \ 分隔符）。
        let normalized = app.workspace_cwd.replace('\\', "/");
        let name = normalized
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or(&normalized);
        segments.push((PRIO_WORKSPACE, Span::styled(format!(" {name}"), dim)));
    }
    // ── 组 2：token 预算条 ──────────────────────────────
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
        // ctx 标签 + 进度条合并为单个段：宽度不足时整体保留或整体丢弃，
        // 避免出现"进度条在、标签被丢"的残缺形态。
        segments.push((
            PRIO_CTX,
            Span::styled(
                app.tr.t_args(
                    Key::CtxUsage,
                    &[
                        ("bar", &bar),
                        ("pct", &pct.to_string()),
                        ("used", &fmt_tokens(used)),
                        ("window", &fmt_tokens(window)),
                    ],
                ),
                style,
            ),
        ));
    }
    // 权限模式预设指示：高优先级（安全相关，窄终端优先保留）。
    // gate 未注入（permission_mode=None）时不显示。
    if let Some(mode) = app.permission_mode {
        segments.push((
            PRIO_MODE,
            Span::styled(
                app.tr.t_args(
                    Key::PermModeIndicator,
                    &[(
                        "mode",
                        app.tr
                            .t(crate::app::state::permission_mode_label(Some(mode))),
                    )],
                ),
                dim,
            ),
        ));
    }
    // 配置状态警示（高优先级，红色/黄色）：CLI 入口通常已拦截，此处为
    // 库级嵌入等绕过门禁的场景兜底，让未配置一眼可见。
    let warn_style = |c: ratatui::style::Color| Style::default().fg(c).add_modifier(Modifier::BOLD);
    if !app.provider_configured {
        segments.push((
            PRIO_MODE,
            Span::styled(
                format!(" ⚠ {}", app.tr.t(Key::StatusNoProvider)),
                warn_style(
                    theme
                        .verification_fail
                        .fg
                        .unwrap_or(ratatui::style::Color::Red),
                ),
            ),
        ));
    } else if !app.api_key_configured {
        segments.push((
            PRIO_MODE,
            Span::styled(
                format!(" ⚠ {}", app.tr.t(Key::StatusNoApiKey)),
                warn_style(ratatui::style::Color::Yellow),
            ),
        ));
    }
    // 退出确认警示（最高优先级，红色加粗）。
    if app.quit_armed {
        segments.push((
            PRIO_QUIT,
            Span::styled(
                app.tr.t(Key::QuitWarning),
                Style::default()
                    .fg(theme
                        .verification_fail
                        .fg
                        .unwrap_or(ratatui::style::Color::Red))
                    .add_modifier(Modifier::BOLD),
            ),
        ));
    }
    segments
}

/// 宽度感知的状态行：总宽超过 `width` 时按优先级丢弃最次要的 segment，
/// 仍放不下则对最右侧剩余内容截断。返回可直接渲染的 [Line]。
///
/// 参考 Codex footer 的"回退链"：教学性/次要信息最先牺牲，最后只留
/// 运行态 + 模型（+ 退出警示），避免窄终端上静默截断丢失关键信息。
pub fn fit_status_line(app: &AppState, theme: &Theme, width: usize) -> Line<'static> {
    let mut segs = tagged_segments(app, theme);
    let total: usize = segs.iter().map(|(_, s)| s.content.width()).sum();
    if total <= width.max(1) {
        return Line::from(segs.into_iter().map(|(_, s)| s).collect::<Vec<_>>());
    }
    // 丢弃非核心 segment（优先级低于 PRIO_MODEL 的都可丢），按优先级升序丢最次要的，
    // 直到放得下或只剩核心（运行态 + 模型 + 退出警示）。
    loop {
        let total: usize = segs.iter().map(|(_, s)| s.content.width()).sum();
        if total <= width.max(1) {
            break;
        }
        let candidate = (0..segs.len())
            .filter(|&i| segs[i].0 < PRIO_MODEL)
            .min_by_key(|&i| (segs[i].0, i));
        let Some(idx) = candidate else {
            break;
        };
        segs.remove(idx);
    }
    // 丢弃后放得下：直接返回，不再截断/加省略号。
    let remaining: usize = segs.iter().map(|(_, s)| s.content.width()).sum();
    if remaining <= width.max(1) {
        return Line::from(segs.into_iter().map(|(_, s)| s).collect::<Vec<_>>());
    }
    // 核心仍超宽：对整行做显示宽度截断并加省略号。
    let mut line = String::new();
    let mut used = 0usize;
    let budget = width.saturating_sub(1);
    for (_, s) in &segs {
        let w = s.content.width();
        if used + w > budget && !line.is_empty() {
            break;
        }
        line.push_str(s.content.as_ref());
        used += w;
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    if used > 0 {
        spans.push(Span::styled(
            line,
            Style::default().add_modifier(Modifier::DIM),
        ));
        spans.push(Span::styled("…", Style::default()));
    } else {
        // 极端窄终端：至少保留运行态标记。
        spans.push(Span::styled(
            "○…",
            Style::default().add_modifier(Modifier::DIM),
        ));
    }
    Line::from(spans)
}

/// 上下文感知提示行：随焦点显示当前键位。键位文本从 action 注册表
/// 动态查询（Claude Code `Rw` 同构）——未来 keybindings.json 用户改键后
/// 提示自动更新，无需改动此处。模板文案按 `tr` 语言取。
pub fn hint_for(focus: Focus, tr: Tr) -> String {
    use crate::app::actions::{chord_for, Action};
    let chord = |action| chord_for(ctx_for(focus), action).unwrap_or_default();
    match focus {
        Focus::Conversation => tr.t_args(
            Key::HintConversation,
            &[
                ("nav", &chord(Action::ConvSelectNext)),
                ("fold", &chord(Action::ConvToggleFold)),
                ("copy", &chord(Action::ConvCopy)),
                ("page", &chord(Action::ConvScrollPageUp)),
                ("top", &chord(Action::ConvScrollTop)),
            ],
        ),
        Focus::Input => tr.t(Key::HintInput).to_string(),
        Focus::Sidebar => tr.t_args(
            Key::HintSidebar,
            &[
                ("nav", &chord(Action::SidebarSelectNext)),
                ("tab", &chord(Action::SidebarNextTab)),
            ],
        ),
        Focus::Completion => tr.t(Key::HintCompletion).to_string(),
        Focus::Help => tr.t(Key::HintHelp).to_string(),
        Focus::Confirm => tr.t(Key::HintConfirm).to_string(),
    }
}

/// Focus → ActionContext 映射（提示查询用）。
fn ctx_for(focus: Focus) -> crate::app::actions::ActionContext {
    match focus {
        Focus::Input => ActionContext::Input,
        Focus::Conversation => ActionContext::Conversation,
        Focus::Sidebar => ActionContext::Sidebar,
        Focus::Completion => ActionContext::Completion,
        Focus::Help => ActionContext::Conversation,
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
        let app = AppState {
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        let segments = status_segments(&app, &theme);
        // 组 1：运行态圆点 + 模型名（accent bold）。
        assert_eq!(segments[0].content, "○");
        assert!(segments[0].style.add_modifier.contains(Modifier::DIM));
        let model = segments
            .iter()
            .find(|s| s.style.fg == Some(theme.accent))
            .expect("模型 span（accent）存在");
        assert!(model.style.add_modifier.contains(Modifier::BOLD));
        // Claude Code 风格精简：成本/折叠/turn/usage/lines 不再进状态行。
        for removed in ["turn", "折叠", "$", "lines"] {
            assert!(
                !segments.iter().any(|s| s.content.contains(removed)),
                "状态行不再含 {removed} 段"
            );
        }
    }

    #[test]
    fn hint_text_per_focus() {
        let tr = Tr::new(crate::i18n::Lang::Zh);
        let input_hint = hint_for(Focus::Input, tr);
        assert!(input_hint.contains("/ 命令"));
        assert!(input_hint.contains("Ctrl+T 鼠标"));
        assert!(hint_for(Focus::Conversation, tr).contains("导航"));
        assert!(hint_for(Focus::Sidebar, tr).contains("切面板"));
        assert!(hint_for(Focus::Completion, tr).contains("Enter"));
        // 英文模式输出英文模板。
        let tr_en = Tr::new(crate::i18n::Lang::En);
        assert!(hint_for(Focus::Input, tr_en).contains("/ commands"));
        assert!(hint_for(Focus::Conversation, tr_en).contains("navigate"));
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
        let segments = status_segments(&app, &theme);
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
        let segments = status_segments(&app, &theme);
        let ctx = segments.iter().find(|s| s.content.contains("85%")).unwrap();
        assert_eq!(ctx.style.fg, Some(ratatui::style::Color::Yellow));

        // 97%：红色（verification_fail 的 fg）。
        let app = AppState {
            context_usage: Some((97_000, 100_000)),
            ..Default::default()
        };
        let segments = status_segments(&app, &theme);
        let ctx = segments.iter().find(|s| s.content.contains("97%")).unwrap();
        assert_eq!(ctx.style.fg, theme.verification_fail.fg);

        // 无 context_usage：不渲染 ctx 段。
        let app = AppState::default();
        let segments = status_segments(&app, &theme);
        assert!(!segments.iter().any(|s| s.content.contains('█')));
    }

    #[test]
    fn status_segments_show_permission_mode_when_set() {
        let theme = Theme::default();
        // 未注入 gate（None）→ 不显示权限段。
        let app = AppState::default();
        let segments = status_segments(&app, &theme);
        assert!(!segments.iter().any(|s| s.content.contains("perm")));
        // accept_edits → 显示。
        let app = AppState {
            permission_mode: Some(deepseeknova_permission::PermissionMode::AcceptEdits),
            tr: Tr::new(crate::i18n::Lang::En),
            ..Default::default()
        };
        let segments = status_segments(&app, &theme);
        let mode = segments
            .iter()
            .find(|s| s.content.contains("perm"))
            .expect("权限模式段存在");
        assert!(mode.content.contains("accept_edits"), "{}", mode.content);
        // 中文：`权限 accept_edits`。
        let app = AppState {
            permission_mode: Some(deepseeknova_permission::PermissionMode::Plan),
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        let segments = status_segments(&app, &theme);
        let mode = segments
            .iter()
            .find(|s| s.content.contains("权限"))
            .expect("中文权限段存在");
        assert!(mode.content.contains("plan"), "{}", mode.content);
    }

    #[test]
    fn status_segments_warn_when_config_incomplete() {
        let theme = Theme::default();
        // 未配置 provider → 红色 no-provider tag。
        let app = AppState::default();
        let segments = status_segments(&app, &theme);
        let no_provider = segments
            .iter()
            .find(|s| s.content.contains("no-provider"))
            .expect("未配置 provider tag 存在");
        assert_eq!(no_provider.style.fg, theme.verification_fail.fg);
        // provider 就绪但 key 缺失 → 黄色 tag。
        let app = AppState {
            provider_configured: true,
            api_key_configured: false,
            ..Default::default()
        };
        let segments = status_segments(&app, &theme);
        assert!(
            segments.iter().any(|s| s.content.contains("no-api-key")),
            "缺 key tag: {:?}",
            segments
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>()
        );
        // 全部就绪 → 无警示。
        let app = AppState {
            provider_configured: true,
            api_key_configured: true,
            ..Default::default()
        };
        let segments = status_segments(&app, &theme);
        assert!(!segments.iter().any(|s| s.content.contains("no-provider")));
        assert!(!segments.iter().any(|s| s.content.contains("no-api-key")));
    }

    #[test]
    fn status_shows_effort_suffix_and_workspace() {
        let theme = Theme::default();
        // effort 后缀：high → 模型名带 ·high。
        let app = AppState {
            model_label: "deepseek-v4-flash".into(),
            effort_label: "high".into(),
            git_branch: Some("feat/tui".into()),
            workspace_cwd: "/Users/ze/proj".into(),
            ..Default::default()
        };
        let segments = status_segments(&app, &theme);
        let model = segments
            .iter()
            .find(|s| s.content.contains("deepseek-v4-flash"))
            .expect("模型段存在");
        assert!(
            model.content.contains("·high"),
            "effort 后缀: {}",
            model.content
        );
        assert!(
            segments.iter().any(|s| s.content.contains("⎇ feat/tui")),
            "git 分支段: {:?}",
            segments
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>()
        );
        // 无分支：显示 cwd basename。
        let app = AppState {
            model_label: "m".into(),
            workspace_cwd: "/Users/ze/proj".into(),
            git_branch: None,
            ..Default::default()
        };
        let segments = status_segments(&app, &theme);
        assert!(segments.iter().any(|s| s.content.contains(" proj")));
        // effort 为空（Disabled）：无后缀。
        let app = AppState {
            model_label: "m".into(),
            effort_label: String::new(),
            ..Default::default()
        };
        let segments = status_segments(&app, &theme);
        let model = segments
            .iter()
            .find(|s| s.content.contains('m') && s.style.fg == Some(theme.accent))
            .expect("模型段");
        assert!(!model.content.contains('·'), "无后缀: {}", model.content);
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
        let segments = status_segments(&app, &theme);
        let ctx = segments.iter().find(|s| s.content.contains("46%")).unwrap();
        assert!(ctx.content.contains('█'), "预算条渲染: {}", ctx.content);
        assert!(ctx.content.contains("46%"));
    }

    #[test]
    fn fit_status_line_drops_least_important_first() {
        let theme = Theme::default();
        let app = AppState {
            model_label: "deepseek-v4-flash-0731".into(),
            context_usage: Some((4_000, 128_000)),
            total_cost_usd: Some(0.0012),
            turn: 3,
            usage: Some(deepseeknova_core::chunk::Usage {
                prompt_tokens: 4000,
                completion_tokens: 100,
                total_tokens: 4100,
                reasoning_tokens: 20,
                cache_hit_tokens: 0,
                cache_miss_tokens: 0,
            }),
            rendered_lines: 30,
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };

        // 宽裕：核心段全量可见（精简后成本/turn/usage/lines 不进状态行）。
        let wide = fit_status_line(&app, &theme, 400);
        let wide_text = wide.to_string();
        assert!(wide_text.contains("deepseek-v4-flash-0731"));
        assert!(wide_text.contains("ctx"), "宽行含 ctx: {wide_text}");
        assert!(
            !wide_text.contains("lines 30"),
            "宽行也不含 lines: {wide_text}"
        );
        assert!(
            !wide_text.contains("缓存hit"),
            "宽行也不含 usage: {wide_text}"
        );

        // 收窄：先丢 ctx，保留运行态 + 模型。
        let narrow = fit_status_line(&app, &theme, 30);
        let narrow_text = narrow.to_string();
        assert!(
            narrow_text.contains("deepseek-v4-flash-0731"),
            "模型必须保留: {narrow_text}"
        );
        assert!(
            !narrow_text.contains("ctx"),
            "窄行应已丢 ctx 预算段: {narrow_text}"
        );

        // 极窄：只留运行态 + 模型（+ 省略号），不出现截断空白。
        let tiny = fit_status_line(&app, &theme, 12);
        let tiny_text = tiny.to_string();
        assert!(
            tiny_text.contains("deepseek-v4-flash-0731") || tiny_text.contains("…"),
            "极窄退化为模型+省略号: {tiny_text}"
        );
        assert!(tiny_text.width() <= 13, "宽度受约束: {tiny_text}");
    }
}

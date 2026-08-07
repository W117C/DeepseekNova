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
/// 先丢 usage 明细 → lines → turn → 折叠 → 成本 → ctx → 模型/运行态 → 退出警示。
const PRIO_QUIT: u8 = 10;
const PRIO_MODEL: u8 = 9;
const PRIO_MODE: u8 = 9;
const PRIO_CTX: u8 = 8;
const PRIO_COST: u8 = 7;
const PRIO_FOLD: u8 = 6;
const PRIO_TURN: u8 = 5;
const PRIO_USAGE: u8 = 4;
const PRIO_LINES: u8 = 3;

/// 状态行分段（仪表盘式，3 组信息，语义色分层）：
/// 1) 运行态 + 模型（主信息，accent/bold）
/// 2) token 预算条 + 成本（资源，阈值变色）
/// 3) 计数（turn/usage/lines，dim 静默）
///
/// 仅测试使用：生产渲染走 [`fit_status_line`]（宽度感知）。保留裸段构造
/// 供测试直接断言各段内容与样式。
#[cfg(test)]
pub fn status_segments(app: &AppState, theme: &Theme, scroll_pct: usize) -> Vec<Span<'static>> {
    tagged_segments(app, theme, scroll_pct)
        .into_iter()
        .map(|(_, s)| s)
        .collect()
}

/// 带优先级的 segment 构建（fit 与直接展示共用，避免两份逻辑漂移）。
fn tagged_segments(app: &AppState, theme: &Theme, scroll_pct: usize) -> Vec<(u8, Span<'static>)> {
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
    segments.push((
        PRIO_MODEL,
        Span::styled(
            app.model_label.clone(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
    ));
    // ── 组 2：token 预算条 + 成本 ────────────────────────
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
    if let Some(cost) = app.total_cost_usd {
        segments.push((PRIO_COST, Span::styled(format!(" │ ${cost:.4}"), dim)));
    }
    // 折叠模式指示：/fold all|none|reset 后用户能一眼看到当前状态。
    segments.push((
        PRIO_FOLD,
        Span::styled(
            app.tr
                .t_args(Key::FoldIndicator, &[("state", app.tr.t(app.fold_label()))]),
            dim,
        ),
    ));
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
    // ── 组 3：计数（静默） ───────────────────────────────
    segments.push((
        PRIO_TURN,
        Span::styled(format!(" │ turn {}", app.turn), dim),
    ));
    if let Some(u) = &app.usage {
        segments.push((
            PRIO_USAGE,
            Span::styled(
                app.tr.t_args(
                    Key::UsageDetail,
                    &[
                        ("up", &u.prompt_tokens.to_string()),
                        ("down", &u.completion_tokens.to_string()),
                        ("total", &u.total_tokens.to_string()),
                        ("reasoning", &u.reasoning_tokens.to_string()),
                        ("hit", &u.cache_hit_tokens.to_string()),
                    ],
                ),
                dim,
            ),
        ));
    }
    segments.push((
        PRIO_LINES,
        Span::styled(
            app.tr.t_args(
                Key::LinesIndicator,
                &[
                    ("lines", &app.render_line_count().to_string()),
                    ("scroll", &scroll_pct.to_string()),
                ],
            ),
            dim,
        ),
    ));
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
pub fn fit_status_line(
    app: &AppState,
    theme: &Theme,
    scroll_pct: usize,
    width: usize,
) -> Line<'static> {
    let mut segs = tagged_segments(app, theme, scroll_pct);
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
        let segments = status_segments(&app, &theme, 0);
        // 组 1：运行态圆点 + 模型名（accent bold）。
        assert_eq!(segments[0].content, "○");
        assert!(segments[0].style.add_modifier.contains(Modifier::DIM));
        let model = segments
            .iter()
            .find(|s| s.style.fg == Some(theme.accent))
            .expect("模型 span（accent）存在");
        assert!(model.style.add_modifier.contains(Modifier::BOLD));
        // 计数段 dim。
        let turn = segments
            .iter()
            .find(|s| s.content.contains("turn"))
            .unwrap();
        assert!(turn.style.add_modifier.contains(Modifier::DIM));
        // 折叠模式指示：默认态也要显示。
        assert!(segments.iter().any(|s| s.content.contains("折叠 默认")));
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
        let segments = status_segments(&app, &theme, 0);
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
        let segments = status_segments(&app, &theme, 0);
        let ctx = segments.iter().find(|s| s.content.contains("85%")).unwrap();
        assert_eq!(ctx.style.fg, Some(ratatui::style::Color::Yellow));

        // 97%：红色（verification_fail 的 fg）。
        let app = AppState {
            context_usage: Some((97_000, 100_000)),
            ..Default::default()
        };
        let segments = status_segments(&app, &theme, 0);
        let ctx = segments.iter().find(|s| s.content.contains("97%")).unwrap();
        assert_eq!(ctx.style.fg, theme.verification_fail.fg);

        // 无 context_usage：不渲染 ctx 段。
        let app = AppState::default();
        let segments = status_segments(&app, &theme, 0);
        assert!(!segments.iter().any(|s| s.content.contains('█')));
    }

    #[test]
    fn status_segments_show_permission_mode_when_set() {
        let theme = Theme::default();
        // 未注入 gate（None）→ 不显示权限段。
        let app = AppState::default();
        let segments = status_segments(&app, &theme, 0);
        assert!(!segments.iter().any(|s| s.content.contains("perm")));
        // accept_edits → 显示。
        let app = AppState {
            permission_mode: Some(deepseeknova_permission::PermissionMode::AcceptEdits),
            tr: Tr::new(crate::i18n::Lang::En),
            ..Default::default()
        };
        let segments = status_segments(&app, &theme, 0);
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
        let segments = status_segments(&app, &theme, 0);
        let mode = segments
            .iter()
            .find(|s| s.content.contains("权限"))
            .expect("中文权限段存在");
        assert!(mode.content.contains("plan"), "{}", mode.content);
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
        let segments = status_segments(&app, &theme, 0);
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

        // 宽裕：全量可见。
        let wide = fit_status_line(&app, &theme, 0, 400);
        let wide_text = wide.to_string();
        assert!(wide_text.contains("lines 30"), "宽行含 lines: {wide_text}");
        assert!(wide_text.contains("缓存hit"));

        // 收窄到 ~70 列：丢 usage 明细、lines、cost、turn，保留 model/ctx。
        let narrow = fit_status_line(&app, &theme, 0, 70);
        let narrow_text = narrow.to_string();
        assert!(
            !narrow_text.contains("缓存hit"),
            "窄行应已丢 usage 明细: {narrow_text}"
        );
        assert!(
            !narrow_text.contains("lines 30"),
            "窄行应已丢 lines: {narrow_text}"
        );
        assert!(
            narrow_text.contains("deepseek-v4-flash-0731"),
            "模型必须保留: {narrow_text}"
        );
        assert!(
            narrow_text.contains("ctx"),
            "ctx 预算段优先保留: {narrow_text}"
        );

        // 极窄：只留运行态 + 模型（+ 省略号），不出现截断空白。
        let tiny = fit_status_line(&app, &theme, 0, 12);
        let tiny_text = tiny.to_string();
        assert!(
            tiny_text.contains("deepseek-v4-flash-0731") || tiny_text.contains("…"),
            "极窄退化为模型+省略号: {tiny_text}"
        );
        assert!(tiny_text.width() <= 13, "宽度受约束: {tiny_text}");
    }
}

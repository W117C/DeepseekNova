//! 状态行与提示行：model 标签用 accent，次要信息 dim，随焦点显示键位。

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::actions::ActionContext;
use crate::app::focus::Focus;
use crate::app::state::AppState;
use crate::i18n::{Key, Tr};
use crate::theme::{SemanticTone, Theme};

/// 状态行分段优先级（数字越大越先保留）。宽度不足时按此顺序丢弃：
/// 先丢 ctx → 模型/运行态/权限模式 → 退出警示。
///
/// 会话 prefix cache 命中率段（资源健康信息：低于 ctx、高于 turn——
/// 命中率异常是成本异常的前兆）。阈值与 runtime/metrics.rs 的 30% 告警对齐。
const PRIO_CACHE: u8 = 7;
/// 命中率“健康”下限（≥70% 绿色；DeepSeek 前缀缓存稳定前缀下应达 80%+）。
const CACHE_OK_PCT: u64 = 70;
/// 命中率告警阈值（<30% 黄色，对齐 runtime `CACHE_HIT_WARN_THRESHOLD`）。
const CACHE_WARN_PCT: u64 = 30;
/// Claude Code 风格精简：只保留运行态 + 模型 + ctx 占用 + 权限模式 +
/// 退出警示；成本/turn/usage/lines 等明细挪到 `/cost` 等命令查看。
const PRIO_QUIT: u8 = 10;
const PRIO_MODEL: u8 = 9;
const PRIO_MODE: u8 = 9;
const PRIO_CTX: u8 = 8;
/// turn 计数段（三栏中栏；优先级低于 ctx、高于 workspace）。
const PRIO_TURN: u8 = 6;
/// 工作区/git 分支：信息性最低优先级，窄终端最先丢弃。
const PRIO_WORKSPACE: u8 = 2;

/// grok 状态栏段间分隔符（xai-grok-pager context_bar.rs 同款）。
const SEPARATOR: &str = "│";

/// 统一徽章样式：状态行主信息标签（模型/成本等）——accent 前景 + 加粗，
/// 所有标签共用同一视觉语言（参考 [`crate::theme::Theme::style_for`] 的 Agent 观感）。
fn badge_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD)
}

/// 状态行分段（Claude Code 风格精简，语义色分层）：
/// 1) 运行态 + 模型（主信息，accent/bold）
/// 2) token 预算条（资源，阈值变色）
/// 3) 权限模式（安全相关，窄终端优先保留）
///
/// 仅测试使用：生产渲染走 [`fit_status_line`]（宽度感知）。保留裸段构造
/// 供测试直接断言各段内容与样式（每段内多个 span 拍平为独立 span）。
#[cfg(test)]
pub fn status_segments(app: &AppState, theme: &Theme) -> Vec<Span<'static>> {
    tagged_segments(app, theme)
        .into_iter()
        .flat_map(|(_, line)| line.spans)
        .collect()
}

/// 带优先级的 segment 构建（fit 与直接展示共用，避免两份逻辑漂移）。
/// 每段为一条 [Line]：可容纳多个 span（如 token 预算条的逐格渐变），
/// 宽度不足时整段一起保留或一起丢弃。
///
/// 生产渲染走三栏 [`fit_status_line`]（宽度感知 + 栏内优先级丢弃）；
/// 本函数保留给测试入口 [`status_segments`] 直接断言段内容与样式。
#[cfg(test)]
fn tagged_segments(app: &AppState, theme: &Theme) -> Vec<(u8, Line<'static>)> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut segments = Vec::new();
    // ── 组 1：运行态 + 模型 ──────────────────────────────
    let state = if app.running {
        Span::styled(
            "●",
            Style::default().fg(theme.semantic(SemanticTone::Success)),
        )
    } else {
        Span::styled("○", dim)
    };
    segments.push((PRIO_MODEL, Line::from(vec![state])));
    segments.push((PRIO_MODEL, Line::from(vec![Span::styled(" ", dim)])));
    // 模型 + effort 后缀（thinking off 时无后缀）。
    let model_text = if app.effort_label.is_empty() {
        app.model_label.clone()
    } else {
        format!("{}·{}", app.model_label, app.effort_label)
    };
    segments.push((
        PRIO_MODEL,
        Line::from(vec![Span::styled(model_text, badge_style(theme))]),
    ));
    // ── 组 0：工作区 + git 分支（信息性，最优先丢弃）──
    if let Some(branch) = &app.git_branch {
        segments.push((
            PRIO_WORKSPACE,
            Line::from(vec![Span::styled(format!(" ⎇ {branch}"), dim)]),
        ));
    } else if !app.workspace_cwd.is_empty() {
        // 非 git 工作区：显示目录 basename（兼容 / 与 \ 分隔符）。
        let normalized = app.workspace_cwd.replace('\\', "/");
        let name = normalized
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or(&normalized);
        segments.push((
            PRIO_WORKSPACE,
            Line::from(vec![Span::styled(format!(" {name}"), dim)]),
        ));
    }
    // ── 组 2：token 预算条 ──────────────────────────────
    if let Some(seg) = ctx_segment(app, theme, dim) {
        segments.push(seg);
    }
    // 权限模式预设指示：高优先级（安全相关，窄终端优先保留）。
    // gate 未注入（permission_mode=None）时不显示。
    if let Some(mode) = app.permission_mode {
        segments.push((
            PRIO_MODE,
            Line::from(vec![Span::styled(
                app.tr.t_args(
                    Key::PermModeIndicator,
                    &[(
                        "mode",
                        app.tr
                            .t(crate::app::state::permission_mode_label(Some(mode))),
                    )],
                ),
                dim,
            )]),
        ));
    }
    // 配置状态警示（高优先级，红色/黄色）：CLI 入口通常已拦截，此处为
    // 库级嵌入等绕过门禁的场景兜底，让未配置一眼可见。
    let warn_style = |c: ratatui::style::Color| Style::default().fg(c).add_modifier(Modifier::BOLD);
    if !app.provider_configured {
        segments.push((
            PRIO_MODE,
            Line::from(vec![Span::styled(
                format!(" ⚠ {}", app.tr.t(Key::StatusNoProvider)),
                warn_style(theme.semantic(SemanticTone::Danger)),
            )]),
        ));
    } else if !app.api_key_configured {
        segments.push((
            PRIO_MODE,
            Line::from(vec![Span::styled(
                format!(" ⚠ {}", app.tr.t(Key::StatusNoApiKey)),
                warn_style(theme.semantic(SemanticTone::Warning)),
            )]),
        ));
    }
    // 退出确认警示（最高优先级，红色加粗）。
    if app.quit_armed {
        segments.push((
            PRIO_QUIT,
            Line::from(vec![Span::styled(
                app.tr.t(Key::QuitWarning),
                Style::default()
                    .fg(theme.semantic(SemanticTone::Danger))
                    .add_modifier(Modifier::BOLD),
            )]),
        ));
    }
    segments
}

/// 宽度感知的状态行（grok 三栏对齐）：左栏 context（工作区/模型/ctx）、
/// 中栏 turn（回合计数 + 视图标签）、右栏 mode（运行态/权限/警示/退出）。
///
/// 总宽超过 `width` 时按栏内优先级丢弃：先丢左栏 workspace → 左栏 ctx →
/// 中栏 turn → 最后保留右栏核心（运行态 + 权限模式 + 退出警示），
/// 仍放不下则对整行截断加省略号。参考 Codex footer 的回退链思路。
pub fn fit_status_line(app: &AppState, theme: &Theme, width: usize) -> Line<'static> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    // ── 三栏：左 context / 中 turn / 右 mode ──────────────
    // 左栏：工作区 + 模型 + ctx 预算条（信息性，优先丢弃）。
    let mut left: Vec<(u8, Line<'static>)> = Vec::new();
    if let Some(branch) = &app.git_branch {
        left.push((
            PRIO_WORKSPACE,
            Line::from(vec![Span::styled(format!(" ⎇ {branch}"), dim)]),
        ));
    } else if !app.workspace_cwd.is_empty() {
        let normalized = app.workspace_cwd.replace('\\', "/");
        let name = normalized
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or(&normalized);
        left.push((
            PRIO_WORKSPACE,
            Line::from(vec![Span::styled(format!(" {name}"), dim)]),
        ));
    }
    let state = if app.running {
        Span::styled(
            "●",
            Style::default().fg(theme.semantic(SemanticTone::Success)),
        )
    } else {
        Span::styled("○", dim)
    };
    let model_text = if app.effort_label.is_empty() {
        app.model_label.clone()
    } else {
        format!("{}·{}", app.model_label, app.effort_label)
    };
    left.push((PRIO_MODEL, Line::from(vec![state.clone()])));
    left.push((
        PRIO_MODEL,
        Line::from(vec![Span::styled(model_text, badge_style(theme))]),
    ));
    if let Some(seg) = grok_ctx_segment(app, theme) {
        left.push(seg);
    }
    if let Some(seg) = session_cache_segment(app, theme) {
        left.push(seg);
    }
    // 中栏：回合计数 + 视图标签（All 显示总数，Single 显示 `选中/总数`）。
    let mut middle: Vec<(u8, Line<'static>)> = Vec::new();
    let turns = app.conversation.turn_count();
    if turns > 0 {
        let turn_text = if app.turn_view == crate::app::state::TurnView::Single {
            let sel = app.selected_turn.map(|t| t + 1).unwrap_or(1);
            format!(" ♯{sel}/{turns} {}", app.tr.t(Key::TurnViewSingle))
        } else {
            format!(" ♯{turns} {}", app.tr.t(Key::TurnViewAll))
        };
        middle.push((PRIO_TURN, Line::from(vec![Span::styled(turn_text, dim)])));
    }
    // 右栏：权限模式 + 配置警示 + 退出警示（安全相关，最后丢弃）。
    let mut right: Vec<(u8, Line<'static>)> = Vec::new();
    if let Some(mode) = app.permission_mode {
        right.push((
            PRIO_MODE,
            Line::from(vec![Span::styled(
                app.tr.t_args(
                    Key::PermModeIndicator,
                    &[(
                        "mode",
                        app.tr
                            .t(crate::app::state::permission_mode_label(Some(mode))),
                    )],
                ),
                dim,
            )]),
        ));
    }
    let warn_style = |c: ratatui::style::Color| Style::default().fg(c).add_modifier(Modifier::BOLD);
    if !app.provider_configured {
        right.push((
            PRIO_MODE,
            Line::from(vec![Span::styled(
                format!(" ⚠ {}", app.tr.t(Key::StatusNoProvider)),
                warn_style(theme.semantic(SemanticTone::Danger)),
            )]),
        ));
    } else if !app.api_key_configured {
        right.push((
            PRIO_MODE,
            Line::from(vec![Span::styled(
                format!(" ⚠ {}", app.tr.t(Key::StatusNoApiKey)),
                warn_style(theme.semantic(SemanticTone::Warning)),
            )]),
        ));
    }
    if app.quit_armed {
        right.push((
            PRIO_QUIT,
            Line::from(vec![Span::styled(
                app.tr.t(Key::QuitWarning),
                Style::default()
                    .fg(theme.semantic(SemanticTone::Danger))
                    .add_modifier(Modifier::BOLD),
            )]),
        ));
    }

    // ── 合并 + 宽度感知丢弃 ─────────────────────────────
    // grok 风格：栏间用 `│` 分隔（只在有相邻栏内容时插入，避免空栏
    // 产生双分隔符）；分隔符优先级同 turn 段，窄终端可随栏一起被丢弃。
    let mut segs: Vec<(u8, Line<'static>)> = Vec::new();
    segs.extend(left);
    if !middle.is_empty() {
        if !segs.is_empty() {
            segs.push((PRIO_TURN, Line::from(vec![Span::styled(SEPARATOR, dim)])));
        }
        segs.extend(middle);
    }
    if !right.is_empty() {
        if !segs.is_empty() {
            segs.push((PRIO_TURN, Line::from(vec![Span::styled(SEPARATOR, dim)])));
        }
        segs.extend(right);
    }
    let total: usize = segs.iter().map(|(_, s)| s.width()).sum();
    if total <= width.max(1) {
        return flatten_lines(segs);
    }
    // 丢弃非核心 segment（优先级低于 PRIO_MODEL 的都可丢），按优先级升序丢最次要的，
    // 直到放得下或只剩核心（运行态 + 模型 + 权限模式 + 退出警示）。
    loop {
        let total: usize = segs.iter().map(|(_, s)| s.width()).sum();
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
    let remaining: usize = segs.iter().map(|(_, s)| s.width()).sum();
    if remaining <= width.max(1) {
        return flatten_lines(segs);
    }
    // 核心仍超宽：对整行做显示宽度截断并加省略号。
    let mut line = String::new();
    let mut used = 0usize;
    let budget = width.saturating_sub(1);
    for (_, s) in &segs {
        let w = s.width();
        if used + w > budget && !line.is_empty() {
            break;
        }
        line.push_str(&s.to_string());
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

/// 把带优先级的 segment 列表拍平为单行（丢弃优先级，合并所有 span）。
fn flatten_lines(segs: Vec<(u8, Line<'static>)>) -> Line<'static> {
    Line::from(
        segs.into_iter()
            .flat_map(|(_, line)| line.spans)
            .collect::<Vec<_>>(),
    )
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

/// token 数的人类可读格式（grok 对齐，K/M 大写，≤4 字符）：<1000 原值、
/// <10K `1.2K`、<1M `12K`、<10M `1.2M`、否则 `12M`。
pub fn fmt_tokens(n: u64) -> String {
    if n >= 10_000_000 {
        format!("{}M", n / 1_000_000)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{}K", n / 1_000)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// 5 字符百分比（grok 对齐）：<10 用两位小数 `0.00%`、10–99 用一位小数
/// `42.0%`、≥100 显示 `MAX %`。供 hover 进度条百分比等定宽展示用。
pub fn fmt_pct5(pct: u64) -> String {
    if pct >= 100 {
        "MAX %".to_string()
    } else if pct < 10 {
        format!("{:.2}%", pct as f64)
    } else {
        format!("{:.1}%", pct as f64)
    }
}

/// 两个颜色间的线性插值（t 在 0.0..=1.0，RGB 通道级混合）。
///
/// 任一端不是 RGB（终端默认 Reset/命名色）时无法混合：按 t 就近取
/// 端点色，保证断点语义不漂移（低 t 取起点、高 t 取终点）。
fn lerp_rgb(a: Color, b: Color, t: f64) -> Color {
    let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (a, b) else {
        return if t < 0.5 { a } else { b };
    };
    let mix = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round() as u8;
    Color::Rgb(mix(ar, br), mix(ag, bg), mix(ab, bb))
}

/// ctx 使用率 → 渐变前景色（grok 断点对齐）：
/// - 0% 用 [`Theme::text_primary`]；
/// - 50–65% 保持 [`Theme::accent_user`]（0→50 lerp 渐入）；
/// - 75–85% 保持 [`Theme::warning`]（65→75 lerp 渐入）；
/// - 85→95 lerp 渐入 [`Theme::accent_error`]，95%+ 钳制 accent_error。
fn ctx_usage_color(theme: &Theme, pct: u64) -> Color {
    let pct = pct.min(100) as f64;
    let stops = [
        (0.0, theme.text_primary),
        (50.0, theme.accent_user),
        (65.0, theme.accent_user),
        (75.0, theme.warning),
        (85.0, theme.warning),
        (95.0, theme.accent_error),
    ];
    for w in stops.windows(2) {
        let (lo, a) = w[0];
        let (hi, b) = w[1];
        if pct <= hi {
            let t = if hi > lo { (pct - lo) / (hi - lo) } else { 0.0 };
            return lerp_rgb(a, b, t);
        }
    }
    stops[5].1
}

/// grok 风格 ctx 段（生产 fit 路径专用）：默认 `8.5K / 1.0M`（used/total），
/// 鼠标 hover 状态栏时切换为 `█████ 42.0%` 进度条 + 百分比（grok
/// context_bar 同款——hover 与默认同宽，不位移）；前景色按使用率
/// [`ctx_usage_color`] 渐变。
///
/// 旧格式 `ctx_segment` 保留给 status_segments 测试路径（i18n 模板）。
/// 会话级 prefix cache 命中率段：`⌁71%`，三档着色（<30% 黄 / ≥70% 绿 /
/// 其余默认前景）。数据源为 [`AppState::session_cache_hit`] /
/// [`AppState::session_cache_miss`]（跨轮次饱和累计，`/new` `/resume` 清零）；
/// 可评估 token（hit+miss）为 0 时返回 None（provider 未上报缓存统计或
/// 会话尚无 LLM 调用，不显示）。
fn session_cache_segment(app: &AppState, theme: &Theme) -> Option<(u8, Line<'static>)> {
    let hit = app.session_cache_hit;
    let miss = app.session_cache_miss;
    let evaluable = hit.checked_add(miss)?;
    if evaluable == 0 {
        return None;
    }
    let pct = hit.saturating_mul(100) / evaluable;
    // 三档着色：<30% 黄（告警，对齐 runtime 阈值）、≥70% 绿（健康）、其余 dim。
    let style = if pct < CACHE_WARN_PCT {
        Style::default().fg(theme.semantic(SemanticTone::Warning))
    } else if pct >= CACHE_OK_PCT {
        Style::default().fg(theme.semantic(SemanticTone::Success))
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    Some((
        PRIO_CACHE,
        Line::from(vec![Span::styled(format!(" ⌁{pct}%"), style)]),
    ))
}

fn grok_ctx_segment(app: &AppState, theme: &Theme) -> Option<(u8, Line<'static>)> {
    let (used, window) = app.context_usage?;
    let pct = used.saturating_mul(100).checked_div(window).unwrap_or(0);
    let hovered = app.mouse_pos.is_some();
    let text = if hovered {
        // hover：进度条（10 格）+ 5 字符百分比（grok context_bar hover 观感）。
        let filled = (pct.min(100) * 10).div_ceil(100) as usize;
        let bar = "█".repeat(filled) + &"░".repeat(10 - filled);
        format!("{bar} {}", fmt_pct5(pct))
    } else {
        format!("{} / {}", fmt_tokens(used), fmt_tokens(window))
    };
    Some((
        PRIO_CTX,
        Line::from(vec![Span::styled(
            text,
            Style::default().fg(ctx_usage_color(theme, pct)),
        )]),
    ))
}

/// ctx 预算条段（旧格式，仅 status_segments 测试路径使用；生产走
/// [`grok_ctx_segment`] 的 grok 风格 `8.5K / 1.0M`）。
///
/// 返回带优先级的整段（`PRIO_CTX`）：标签 + 逐格渐变进度条合并为
/// 单段，宽度不足时整体保留或整体丢弃，避免"进度条在、标签被丢"。
#[cfg(test)]
fn ctx_segment(app: &AppState, theme: &Theme, dim: Style) -> Option<(u8, Line<'static>)> {
    let (used, window) = app.context_usage?;
    let pct = used.saturating_mul(100).checked_div(window).unwrap_or(0);
    let style = if pct >= 95 {
        Style::default().fg(theme.semantic(SemanticTone::Danger))
    } else if pct >= 80 {
        Style::default().fg(theme.semantic(SemanticTone::Warning))
    } else {
        dim
    };
    let bar = token_bar(pct);
    let text = app.tr.t_args(
        Key::CtxUsage,
        &[
            ("bar", &bar),
            ("pct", &pct.to_string()),
            ("used", &fmt_tokens(used)),
            ("window", &fmt_tokens(window)),
        ],
    );
    // 定位 `[{bar}]` 括号：预算条替换为逐格渐变分段，`[`/`]` 与前后缀
    // 沿用阈值样式，保证模板观感（含括号）不变。
    let open = text.find('[');
    let close = text.rfind(']');
    let mut spans = Vec::new();
    match (open, close) {
        (Some(open), Some(close)) if open < close => {
            spans.push(Span::styled(text[..=open].to_string(), style));
            spans.extend(ctx_bar_cells(pct, theme, style));
            spans.push(Span::styled(text[close..].to_string(), style));
        }
        _ => {
            // 模板不含括号（防御）：退化为单 span 阈值样式。
            spans.push(Span::styled(text, style));
        }
    }
    Some((PRIO_CTX, Line::from(spans)))
}

/// 10 格 token 预算条：占用百分比 → `████░░░░░░`（Claude Code
/// token 预算条的资源可见性设计；有占用即至少 1 格）。仅测试路径使用。
#[cfg(test)]
fn token_bar(pct: u64) -> String {
    let filled = (pct.min(100) * 10).div_ceil(100) as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled))
}

/// token 预算条的逐格渐变分段：按格位把已占格拆成 success/info/warning/danger
/// 四段语义色（0-40% success、50-60% info、70-80% warning、90%+ danger），
/// 未占格沿用上下文阈值样式。颜色一律从 theme 语义色取，不硬编码。
/// 仅测试路径使用。
#[cfg(test)]
fn ctx_bar_cells(pct: u64, theme: &Theme, base: Style) -> Vec<Span<'static>> {
    let filled = (pct.min(100) * 10).div_ceil(100) as usize;
    (0..10)
        .map(|i| {
            let tone = match i {
                0..=4 => SemanticTone::Success,
                5..=6 => SemanticTone::Info,
                7..=8 => SemanticTone::Warning,
                _ => SemanticTone::Danger,
            };
            let style = if i < filled {
                Style::default().fg(theme.semantic(tone))
            } else {
                base
            };
            Span::styled(if i < filled { "█" } else { "░" }, style)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

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

        // 30%：dim 常规 + 前 3 格 success 渐变分段。
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
            ctx.content.contains(']'),
            "预算条后缀保留括号: {}",
            ctx.content
        );
        assert_eq!(ctx.style, dim);
        let bar_cells: Vec<&Span<'static>> = segments
            .iter()
            .filter(|s| s.content == "█" || s.content == "░")
            .collect();
        assert_eq!(bar_cells.len(), 10, "预算条共 10 格");
        assert_eq!(
            bar_cells.iter().filter(|s| s.content == "█").count(),
            3,
            "30% 占 3 格"
        );
        assert!(
            bar_cells[..3]
                .iter()
                .all(|s| { s.style.fg == Some(theme.semantic(SemanticTone::Success)) }),
            "低占用格用 success 语义色"
        );

        // 85%：黄色警示（theme.warning），高占用格渐入 warning 段。
        let app = AppState {
            context_usage: Some((85_000, 100_000)),
            ..Default::default()
        };
        let segments = status_segments(&app, &theme);
        let ctx = segments.iter().find(|s| s.content.contains("85%")).unwrap();
        assert_eq!(
            ctx.style.fg,
            Some(theme.semantic(SemanticTone::Warning)),
            "≥80% 用 warning 语义色"
        );
        let bar_cells: Vec<&Span<'static>> = segments.iter().filter(|s| s.content == "█").collect();
        assert!(
            bar_cells
                .iter()
                .skip(7)
                .all(|s| s.style.fg == Some(theme.semantic(SemanticTone::Warning))),
            "7/8 格用 warning 语义色"
        );

        // 97%：红色（theme.danger），第 10 格用 danger 语义色。
        let app = AppState {
            context_usage: Some((97_000, 100_000)),
            ..Default::default()
        };
        let segments = status_segments(&app, &theme);
        let ctx = segments.iter().find(|s| s.content.contains("97%")).unwrap();
        assert_eq!(
            ctx.style.fg,
            Some(theme.semantic(SemanticTone::Danger)),
            "≥95% 用 danger 语义色"
        );
        let bar_cells: Vec<&Span<'static>> = segments.iter().filter(|s| s.content == "█").collect();
        assert_eq!(bar_cells.len(), 10, "97% 满格");
        assert!(
            bar_cells
                .last()
                .is_some_and(|s| s.style.fg == Some(theme.semantic(SemanticTone::Danger))),
            "第 10 格用 danger 语义色"
        );

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
        assert_eq!(
            no_provider.style.fg,
            Some(theme.semantic(SemanticTone::Danger)),
            "未配置 provider 用 danger 语义色"
        );
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
        assert_eq!(fmt_tokens(1_500), "1.5K");
        assert_eq!(fmt_tokens(240_000), "240K");
        assert_eq!(fmt_tokens(1_200_000), "1.2M");
        assert_eq!(fmt_tokens(12_345_678), "12M");
        assert_eq!(fmt_tokens(9_600), "9.6K");
        assert_eq!(fmt_tokens(12_000), "12K");
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
        assert!(
            segments.iter().any(|s| s.content.contains('█')),
            "预算条渲染: {:?}",
            segments
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>()
        );
        assert!(ctx.content.contains("46%"));
    }

    #[test]
    fn cache_segment_absent_when_no_evaluable_tokens() {
        let theme = Theme::default();
        let app = AppState::default();
        assert!(session_cache_segment(&app, &theme).is_none());
    }

    #[test]
    fn cache_segment_shows_rate_with_threshold_tones() {
        let theme = Theme::default();
        // 健康档（≥70% 绿色）。
        let mut app = AppState::default();
        app.session_cache_hit = 8_700;
        app.session_cache_miss = 1_300;
        let (prio, line) = session_cache_segment(&app, &theme).expect("有可评估数据必有段");
        assert_eq!(prio, PRIO_CACHE);
        assert!(line.to_string().contains("87%"), "87%: {line:?}");
        assert_eq!(
            line.spans[0].style,
            Style::default().fg(theme.semantic(SemanticTone::Success))
        );
        // 告警档（<30% 黄色）。
        let mut low = AppState::default();
        low.session_cache_hit = 100;
        low.session_cache_miss = 900;
        let (_, line) = session_cache_segment(&low, &theme).expect("10% 必有段");
        assert_eq!(
            line.spans[0].style,
            Style::default().fg(theme.semantic(SemanticTone::Warning))
        );
        // 边界：恰 70% 归健康档（>= 语义）。
        let mut edge = AppState::default();
        edge.session_cache_hit = 700;
        edge.session_cache_miss = 300;
        let (_, line) = session_cache_segment(&edge, &theme).expect("70% 必有段");
        assert_eq!(
            line.spans[0].style,
            Style::default().fg(theme.semantic(SemanticTone::Success))
        );
    }

    #[test]
    fn fit_status_line_includes_cache_segment_when_evaluable() {
        let theme = Theme::default();
        let app = AppState {
            session_cache_hit: 8_700,
            session_cache_miss: 1_300,
            ..Default::default()
        };
        let wide = fit_status_line(&app, &theme, 400).to_string();
        assert!(wide.contains("87%"), "宽行含 cache 段: {wide}");
        // 无可评估数据（provider 未上报缓存统计）不显示段，避免"无数据"被误读为 0%。
        let empty = AppState::default();
        assert!(
            !fit_status_line(&empty, &theme, 400)
                .to_string()
                .contains('⌁'),
            "无数据不渲染 cache 段"
        );
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
        assert!(
            wide_text.contains("4.0K / 128K"),
            "宽行含 grok ctx: {wide_text}"
        );
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

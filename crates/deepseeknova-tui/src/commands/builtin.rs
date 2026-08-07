//! 内建命令：斜杠命令与 Ctrl+K 命令面板共用。
//!
//! 命令逻辑从旧 `TuiRunner::handle_command` / `AppState::execute_command` 迁移，
//! 注入能力统一经 [`crate::commands::TuiCaps`]（Option）读取，缺失时降级回显提示。

use async_trait::async_trait;
use deepseeknova_provider::cost::ModelRole;
use deepseeknova_provider::factory::ReasoningEffort;

use super::{ArgsSpec, Command, CommandCtx, CommandHandler, CommandOutcome, CommandRegistry};
use crate::app::state::{display_mode_label, McpStatus};
use crate::i18n::{Key, Tr};
use crate::model::conversation::LineKind;
use crate::model::scorecard::{dim_label_key, Scorecard};

// ── help ────────────────────────────────────────────────────────

struct HelpCmd;

#[async_trait]
impl CommandHandler for HelpCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, _args: &str) -> CommandOutcome {
        // /help 改为可滚动浮层：不再往对话面板灌 30+ 行（用户反馈"一直堆在
        // 对话页面里很碍事"）。Esc/q 关闭，j/k、↑/↓、PageUp/Down 滚动。
        // 命令列表由注册表生成（名称 + 词表描述），键位说明走词表。
        let tr = ctx.app.tr;
        let mut lines: Vec<String> = CommandRegistry::builtin()
            .iter()
            .map(|c| format!("  /{:<12} {}", c.name, tr.t(*c.desc)))
            .collect();
        lines.push(String::new());
        for key in [
            Key::HelpKeyCmdPalette,
            Key::HelpKeyNav,
            Key::HelpKeyEnter,
            Key::HelpKeyY,
            Key::HelpKeyPage,
            Key::HelpKeyHistory,
            Key::HelpKeyShiftEnter,
            Key::HelpKeyCursor,
            Key::HelpKeyEdit,
            Key::HelpKeyCtrlUW,
            Key::HelpKeyCtrlC,
            Key::HelpFooter,
        ] {
            lines.push(tr.t(key).to_string());
        }
        ctx.app.help_overlay = Some(crate::app::focus::HelpOverlay { lines, scroll: 0 });
        CommandOutcome::Handled
    }
}

// ── clear ───────────────────────────────────────────────────────

struct ClearCmd;

#[async_trait]
impl CommandHandler for ClearCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, _args: &str) -> CommandOutcome {
        ctx.app.clear_display();
        ctx.app.show_notice(ctx.app.tr.t(Key::NoticeCleared));
        CommandOutcome::Handled
    }
}

// ── new / sessions / resume（会话）─────────────────────────────

struct NewCmd;

#[async_trait]
impl CommandHandler for NewCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, _args: &str) -> CommandOutcome {
        let tr = ctx.app.tr;
        match &ctx.caps.session {
            Some(ctrl) => match ctrl.new_session().await {
                Ok(()) => {
                    ctx.app.clear_display();
                    ctx.app.last_prompt = None;
                    ctx.app.sessions_loaded = false;
                    ctx.app.show_notice(tr.t(Key::NoticeNewSession));
                }
                Err(e) => ctx
                    .app
                    .show_notice(tr.t_args(Key::NewSessionFailed, &[("err", &e.to_string())])),
            },
            None => ctx.app.show_notice(tr.t(Key::SessionUnavailable)),
        }
        CommandOutcome::Handled
    }
}

struct SessionsCmd;

#[async_trait]
impl CommandHandler for SessionsCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, _args: &str) -> CommandOutcome {
        let tr = ctx.app.tr;
        match &ctx.caps.session {
            Some(ctrl) => match ctrl.list_sessions().await {
                Ok(mut metas) if !metas.is_empty() => {
                    metas.sort_by_key(|m| m.id.clone());
                    metas.reverse(); // id 按时间字典序，最新优先
                    let current = ctrl.current_session().await;
                    ctx.app
                        .echo_line(LineKind::System, tr.t(Key::SavedSessionsHeader));
                    for m in &metas {
                        let marker = if current.as_deref() == Some(m.id.as_str()) {
                            tr.t(Key::SessionCurrentMarker)
                        } else {
                            ""
                        };
                        let label = if m.preview.is_empty() {
                            m.id.clone()
                        } else {
                            format!("{} — {}", m.id, m.preview)
                        };
                        ctx.app
                            .echo_line(LineKind::System, &format!("  {label}{marker}"));
                    }
                }
                Ok(_) => ctx.app.show_notice(tr.t(Key::NoSavedSessions)),
                Err(e) => ctx
                    .app
                    .show_notice(tr.t_args(Key::ListSessionsFailed, &[("err", &e.to_string())])),
            },
            None => ctx.app.show_notice(tr.t(Key::SessionUnavailable)),
        }
        CommandOutcome::Handled
    }
}

struct ResumeCmd;

#[async_trait]
impl CommandHandler for ResumeCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, args: &str) -> CommandOutcome {
        let target = args.trim();
        let tr = ctx.app.tr;
        match &ctx.caps.session {
            Some(ctrl) if !target.is_empty() => match ctrl.resume(target).await {
                Ok(lines) => {
                    let count = lines.len();
                    ctx.app.restore_conversation(lines);
                    ctx.app.last_prompt = None;
                    ctx.app.show_notice(tr.t_args(
                        Key::ResumeDone,
                        &[("target", target), ("n", &count.to_string())],
                    ));
                }
                Err(e) => ctx
                    .app
                    .show_notice(tr.t_args(Key::ResumeFailed, &[("err", &e.to_string())])),
            },
            Some(_) => ctx.app.show_notice(tr.t(Key::ResumeUsage)),
            None => ctx.app.show_notice(tr.t(Key::SessionUnavailable)),
        }
        CommandOutcome::Handled
    }
}

// ── model / cost ────────────────────────────────────────────────

/// 用工厂重建 runner（/model 系列命令）。失败只提示，不破坏当前会话。
fn rebuild_runner(
    ctx: &mut CommandCtx<'_>,
    effort: Option<ReasoningEffort>,
    model: Option<String>,
) {
    let tr = ctx.app.tr;
    let guard = ctx.caps.runtime.lock().unwrap_or_else(|e| e.into_inner());
    let Some(f) = guard.factory.clone() else {
        ctx.app.show_notice(tr.t(Key::ModelSwitchUnavailable));
        return;
    };
    let eff = effort.unwrap_or(guard.current_effort);
    let mdl = model.or_else(|| guard.current_model.clone());
    drop(guard);
    match f(Some(eff), mdl.clone()) {
        Ok(runner) => {
            let mut guard = ctx.caps.runtime.lock().unwrap_or_else(|e| e.into_inner());
            guard.runner = Some(runner);
            guard.current_effort = eff;
            guard.current_model = mdl.clone();
            guard.model_label = mdl.unwrap_or_else(|| "default".to_string());
            ctx.app.show_notice(tr.t_args(
                Key::ModelSwitched,
                &[("effort", effort_label(eff)), ("model", &guard.model_label)],
            ));
        }
        Err(e) => ctx
            .app
            .show_notice(tr.t_args(Key::ModelSwitchFailed, &[("err", &e.to_string())])),
    }
}

struct ModelCmd;

#[async_trait]
impl CommandHandler for ModelCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, args: &str) -> CommandOutcome {
        let tr = ctx.app.tr;
        let (sub, sub_args) = args.split_once(' ').unwrap_or((args, ""));
        let (current_effort, current_model, baseline_effort, has_router, has_factory) = {
            let g = ctx.caps.runtime.lock().unwrap_or_else(|e| e.into_inner());
            (
                g.current_effort,
                g.current_model.clone(),
                g.baseline_effort,
                g.router.is_some(),
                g.factory.is_some(),
            )
        };
        match sub {
            "" | "help" => {
                ctx.app
                    .echo_line(LineKind::System, tr.t(Key::ModelCommandsHeader));
                for key in [
                    Key::ModelHelpDisplay,
                    Key::ModelHelpEffort,
                    Key::ModelHelpThinking,
                    Key::ModelHelpSwitch,
                    Key::ModelHelpUse,
                ] {
                    ctx.app.echo_line(LineKind::System, tr.t(key));
                }
                ctx.app.echo_line(
                    LineKind::System,
                    &tr.t_args(
                        Key::ModelCurrent,
                        &[
                            ("effort", effort_label(current_effort)),
                            (
                                "model",
                                current_model.as_deref().unwrap_or(tr.t(Key::DefaultLabel)),
                            ),
                        ],
                    ),
                );
                if has_router {
                    let g = ctx.caps.runtime.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(r) = &g.router {
                        for role in [
                            ModelRole::Main,
                            ModelRole::Task,
                            ModelRole::Compact,
                            ModelRole::Quick,
                        ] {
                            ctx.app.echo_line(
                                LineKind::System,
                                &format!(
                                    "  {:<8} → {}",
                                    role.label(),
                                    r.pointer(role)
                                        .unwrap_or_else(|| tr.t(Key::DefaultLabel).to_string())
                                ),
                            );
                        }
                    }
                }
            }
            "effort" => {
                if sub_args.is_empty() {
                    ctx.app.show_notice(tr.t_args(
                        Key::EffortCurrent,
                        &[
                            ("effort", effort_label(current_effort)),
                            ("baseline", effort_label(baseline_effort)),
                        ],
                    ));
                } else {
                    match parse_effort_command(sub_args, tr) {
                        Ok(effort) => rebuild_runner(ctx, Some(effort), None),
                        Err(msg) => ctx.app.show_notice(msg),
                    }
                }
            }
            "thinking" => {
                let new_effort = toggle_thinking(current_effort, baseline_effort);
                if new_effort != current_effort {
                    // ThinkingToggle 技术性文案：中英一致（zh 回退英文）。
                    let from = if current_effort.thinking() {
                        "on"
                    } else {
                        "off"
                    };
                    let to = if new_effort.thinking() { "on" } else { "off" };
                    ctx.app
                        .show_notice(tr.t_args(Key::ThinkingToggle, &[("from", from), ("to", to)]));
                    rebuild_runner(ctx, Some(new_effort), None);
                } else {
                    ctx.app.show_notice(tr.t(Key::ThinkingUnchanged));
                }
            }
            "switch" => {
                if sub_args.is_empty() {
                    ctx.app.show_notice(tr.t(Key::ModelSwitchUsage));
                } else {
                    rebuild_runner(ctx, None, Some(sub_args.to_string()));
                }
            }
            "use" => {
                let mut parts = sub_args.split_whitespace();
                let (role_s, model) = (parts.next(), parts.next());
                if !has_router {
                    ctx.app.show_notice(tr.t(Key::ModelPointersUnavailable));
                    return CommandOutcome::Handled;
                }
                let (Some(role_s), Some(model)) = (role_s, model) else {
                    ctx.app.show_notice(tr.t(Key::ModelUseUsage));
                    return CommandOutcome::Handled;
                };
                let Some(role) = ModelRole::parse(role_s) else {
                    ctx.app.show_notice(tr.t(Key::UnknownRole));
                    return CommandOutcome::Handled;
                };
                {
                    let g = ctx.caps.runtime.lock().unwrap_or_else(|e| e.into_inner());
                    match g.router.as_ref().map(|r| r.set_pointer(role, model)) {
                        Some(Ok(())) => {
                            // PointerSet 技术性文案：中英一致（zh 回退英文）。
                            ctx.app.show_notice(tr.t_args(
                                Key::PointerSet,
                                &[("role", role.label()), ("model", model)],
                            ));
                        }
                        Some(Err(e)) => {
                            ctx.app.show_notice(e.to_string());
                            return CommandOutcome::Handled;
                        }
                        None => {
                            ctx.app.show_notice(tr.t(Key::RouterUnavailable));
                            return CommandOutcome::Handled;
                        }
                    }
                }
                rebuild_runner(ctx, None, None);
            }
            other => {
                ctx.app
                    .show_notice(tr.t_args(Key::UnknownModelSubcommand, &[("cmd", other)]));
            }
        }
        // 保持 model 命令不需要 factory 也能展示帮助；需要 factory 的子命令由 rebuild_runner 降级。
        let _ = has_factory;
        CommandOutcome::Handled
    }
}

struct CostCmd;

#[async_trait]
impl CommandHandler for CostCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, _args: &str) -> CommandOutcome {
        let tr = ctx.app.tr;
        let Some(r) = ctx
            .caps
            .runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .router
            .clone()
        else {
            ctx.app.show_notice(tr.t(Key::CostRouterUnavailable));
            return CommandOutcome::Handled;
        };
        let report = r.ledger().report(&r.price_table());
        if report.rows.is_empty() {
            ctx.app.show_notice(tr.t(Key::NoUsageRecords));
            return CommandOutcome::Handled;
        }
        let window = ctx.caps.context_window.map(|w| w as u64);
        ctx.app.echo_line(
            LineKind::System,
            &format!(
                "{:<22} {:<8} {:>10} {:>12} {:>10} {:>10} {:>8}",
                "model", "role", "prompt", "completion", "cache-hit", "cost($)", "ctx%"
            ),
        );
        for row in report.rows.iter().take(20) {
            let cost = row
                .cost_usd
                .map(|c| format!("{c:.6}"))
                .unwrap_or_else(|| "-".to_string());
            // 上下文占用率 = 该模型×角色累计 prompt+completion ÷ 注入的窗口。
            let ctx_pct = match window {
                Some(w) if w > 0 => {
                    let used = row.bucket.prompt_tokens + row.bucket.completion_tokens;
                    format!("{:>7}%", used * 100 / w)
                }
                _ => "      -".to_string(),
            };
            ctx.app.echo_line(
                LineKind::System,
                &format!(
                    "{:<22} {:<8} {:>10} {:>12} {:>10} {:>10} {:>8}",
                    row.model,
                    row.role.label(),
                    row.bucket.prompt_tokens,
                    row.bucket.completion_tokens,
                    row.bucket.cache_hit_tokens,
                    cost,
                    ctx_pct
                ),
            );
        }
        if let Some(total) = report.total_usd {
            ctx.app.echo_line(
                LineKind::System,
                &tr.t_args(Key::CostTotal, &[("total", &format!("{total:.6}"))]),
            );
        }
        if report.unmetered_calls > 0 {
            ctx.app.echo_line(
                LineKind::System,
                &tr.t_args(
                    Key::UnmeteredCalls,
                    &[("n", &report.unmetered_calls.to_string())],
                ),
            );
        }
        CommandOutcome::Handled
    }
}

// ── scorecard ──────────────────────────────────────────────────

struct ScorecardCmd;

#[async_trait]
impl CommandHandler for ScorecardCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, _args: &str) -> CommandOutcome {
        let tr = ctx.app.tr;
        // 优先工作区，其次用户目录；取最新 JSON 评分卡。
        let mut candidates = Vec::new();
        if let Ok(cwd) = std::env::current_dir() {
            candidates.push(cwd.join(".deepseeknova").join("metrics"));
        }
        if let Some(home) = dirs::home_dir() {
            candidates.push(home.join(".deepseeknova").join("metrics"));
        }
        let mut scorecard: Option<Scorecard> = None;
        for dir in candidates {
            if let Some(sc) = Scorecard::latest_from_dir(&dir) {
                scorecard = Some(sc);
                break;
            }
        }
        let Some(sc) = scorecard else {
            ctx.app.show_notice(tr.t(Key::NoScorecardFound));
            return CommandOutcome::Handled;
        };
        ctx.app.scorecard = Some(sc.clone());
        ctx.app
            .echo_line(LineKind::System, tr.t(Key::ScorecardHeader));
        for row in &sc.rows {
            let bar = crate::model::scorecard::photometry_bar(row.score);
            let label = dim_label_key(&row.dim)
                .map(|k| tr.t(k).to_string())
                .unwrap_or_else(|| row.dim.clone());
            ctx.app.echo_line(
                LineKind::System,
                &format!(" {label:<4} {bar} {:>5.1}", row.score),
            );
        }
        CommandOutcome::Handled
    }
}

// ── skills / mcp ────────────────────────────────────────────────

struct SkillsCmd;

#[async_trait]
impl CommandHandler for SkillsCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, _args: &str) -> CommandOutcome {
        let tr = ctx.app.tr;
        let mut found = false;
        for path in &ctx.caps.skills_paths {
            let loader = deepseeknova_skills::SkillLoader::new(path);
            match loader.load_all() {
                Ok(skills) if !skills.is_empty() => {
                    if !found {
                        ctx.app.echo_line(LineKind::System, tr.t(Key::SkillsHeader));
                        found = true;
                    }
                    for skill in &skills {
                        ctx.app.echo_line(
                            LineKind::System,
                            &format!("  • {} — {}", skill.name, skill.description),
                        );
                        if !skill.tools_allowed.is_empty() {
                            ctx.app.echo_line(
                                LineKind::System,
                                &format!("    tools: {}", skill.tools_allowed.join(", ")),
                            );
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => ctx.app.show_notice(tr.t_args(
                    Key::SkillsLoadFailed,
                    &[
                        ("path", &path.display().to_string()),
                        ("err", &e.to_string()),
                    ],
                )),
            }
        }
        if !found {
            ctx.app.show_notice(tr.t(Key::NoSkillsFound));
        }
        CommandOutcome::Handled
    }
}

struct McpCmd;

#[async_trait]
impl CommandHandler for McpCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, _args: &str) -> CommandOutcome {
        let tr = ctx.app.tr;
        if ctx.caps.mcp_servers.is_empty() {
            ctx.app.show_notice(tr.t(Key::McpNotConfigured));
            return CommandOutcome::Handled;
        }
        let statuses = match &ctx.caps.mcp_probe {
            Some(probe) => probe.probe(&ctx.caps.mcp_servers).await,
            None => Vec::new(),
        };
        ctx.app.echo_line(LineKind::System, tr.t(Key::McpHeader));
        for (i, server) in ctx.caps.mcp_servers.iter().enumerate() {
            let line = match statuses.get(i) {
                Some(McpStatus::Connected) => {
                    tr.t_args(Key::McpConnected, &[("name", &server.name)])
                }
                Some(McpStatus::Disconnected(reason)) => tr.t_args(
                    Key::McpDisconnected,
                    &[("name", &server.name), ("reason", reason)],
                ),
                None => format!("  • {}", server.name),
            };
            ctx.app.echo_line(LineKind::System, &line);
        }
        CommandOutcome::Handled
    }
}

// ── undo ────────────────────────────────────────────────────────

struct UndoCmd;

#[async_trait]
impl CommandHandler for UndoCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, args: &str) -> CommandOutcome {
        let tr = ctx.app.tr;
        let Some(ctrl) = &ctx.caps.undo else {
            ctx.app.show_notice(tr.t(Key::UndoUnavailable));
            return CommandOutcome::Handled;
        };
        match args.trim() {
            "" => match ctrl.rollback_one().await {
                Ok(Some(msg)) => ctx.app.show_notice(format!("✓ {msg}")),
                Ok(None) => ctx.app.show_notice(tr.t(Key::NoRollbackSnapshot)),
                Err(e) => ctx
                    .app
                    .show_notice(tr.t_args(Key::UndoFailed, &[("err", &e.to_string())])),
            },
            "all" => match ctrl.rollback_all().await {
                Ok(n) => ctx
                    .app
                    .show_notice(tr.t_args(Key::RolledBackAll, &[("n", &n.to_string())])),
                Err(e) => ctx
                    .app
                    .show_notice(tr.t_args(Key::UndoFailed, &[("err", &e.to_string())])),
            },
            "list" => match ctrl.list().await {
                Ok(lines) if !lines.is_empty() => {
                    ctx.app
                        .echo_line(LineKind::System, tr.t(Key::SnapshotListHeader));
                    for line in lines {
                        ctx.app.echo_line(LineKind::System, &format!("  {line}"));
                    }
                }
                Ok(_) => ctx.app.show_notice(tr.t(Key::NoSnapshots)),
                Err(e) => ctx
                    .app
                    .show_notice(tr.t_args(Key::ListSnapshotsFailed, &[("err", &e.to_string())])),
            },
            other => ctx
                .app
                .show_notice(tr.t_args(Key::UndoUnknownArg, &[("arg", other)])),
        }
        CommandOutcome::Handled
    }
}

// ── raw ─────────────────────────────────────────────────────────

struct RawCmd;

#[async_trait]
impl CommandHandler for RawCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, _args: &str) -> CommandOutcome {
        ctx.app.display_mode = match ctx.app.display_mode {
            crate::app::state::DisplayMode::Normal => crate::app::state::DisplayMode::Lite,
            crate::app::state::DisplayMode::Lite => crate::app::state::DisplayMode::Raw,
            crate::app::state::DisplayMode::Raw => crate::app::state::DisplayMode::Normal,
        };
        ctx.app.show_notice(ctx.app.tr.t_args(
            Key::DisplayModeNotice,
            &[(
                "mode",
                ctx.app.tr.t(display_mode_label(ctx.app.display_mode)),
            )],
        ));
        CommandOutcome::Handled
    }
}

// ── fold / copy ─────────────────────────────────────────────────

struct FoldCmd;

#[async_trait]
impl CommandHandler for FoldCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, args: &str) -> CommandOutcome {
        let tr = ctx.app.tr;
        match args.trim() {
            "all" => {
                ctx.app.fold_all(true);
                ctx.app.show_notice(
                    tr.t_args(Key::FoldedAll, &[("state", tr.t(ctx.app.fold_label()))]),
                );
            }
            "none" => {
                ctx.app.fold_all(false);
                ctx.app.show_notice(
                    tr.t_args(Key::ExpandedAll, &[("state", tr.t(ctx.app.fold_label()))]),
                );
            }
            "reset" => {
                ctx.app.fold_reset();
                ctx.app.show_notice(tr.t(Key::FoldReset));
            }
            "" => ctx.app.show_notice(tr.t(Key::FoldUsage)),
            other => ctx
                .app
                .show_notice(tr.t_args(Key::FoldUnknownArg, &[("arg", other)])),
        }
        CommandOutcome::Handled
    }
}

struct CopyCmd;

#[async_trait]
impl CommandHandler for CopyCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, _args: &str) -> CommandOutcome {
        ctx.app.copy_selected();
        CommandOutcome::Handled
    }
}

// ── quit ────────────────────────────────────────────────────────

struct QuitCmd;

#[async_trait]
impl CommandHandler for QuitCmd {
    async fn run(&self, _ctx: &mut CommandCtx<'_>, _args: &str) -> CommandOutcome {
        CommandOutcome::Quit
    }
}

// ── 注册表 ──────────────────────────────────────────────────────

static HELP: HelpCmd = HelpCmd;
static CLEAR: ClearCmd = ClearCmd;
static NEW: NewCmd = NewCmd;
static SESSIONS: SessionsCmd = SessionsCmd;
static RESUME: ResumeCmd = ResumeCmd;
static MODEL: ModelCmd = ModelCmd;
static COST: CostCmd = CostCmd;
static SCORECARD: ScorecardCmd = ScorecardCmd;
static SKILLS: SkillsCmd = SkillsCmd;
static MCP: McpCmd = McpCmd;
static UNDO: UndoCmd = UndoCmd;
static RAW: RawCmd = RawCmd;
static FOLD: FoldCmd = FoldCmd;
static COPY: CopyCmd = CopyCmd;
static QUIT: QuitCmd = QuitCmd;

/// 内建命令表（顺序即 /help 与命令面板展示顺序）。
pub const BUILTIN: &[Command] = &[
    Command {
        name: "help",
        desc: &Key::CmdHelpDesc,
        keywords: &["帮助", "命令", "h"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &HELP,
    },
    Command {
        name: "clear",
        desc: &Key::CmdClearDesc,
        keywords: &["清空"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &CLEAR,
    },
    Command {
        name: "new",
        desc: &Key::CmdNewDesc,
        keywords: &["新会话", "会话"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &NEW,
    },
    Command {
        name: "sessions",
        desc: &Key::CmdSessionsDesc,
        keywords: &["会话", "历史"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &SESSIONS,
    },
    Command {
        name: "resume",
        desc: &Key::CmdResumeDesc,
        keywords: &["会话", "恢复"],
        args_spec: ArgsSpec::FreeText,
        args_hint: Some(&["<session-id>"]),
        handler: &RESUME,
    },
    Command {
        name: "model",
        desc: &Key::CmdModelDesc,
        keywords: &["模型", "effort", "thinking", "switch", "use"],
        args_spec: ArgsSpec::FreeText,
        args_hint: Some(&[
            "effort <disabled|high|max>",
            "thinking",
            "switch <model-name>",
            "use <main|task|compact|quick> <model-name>",
        ]),
        handler: &MODEL,
    },
    Command {
        name: "cost",
        desc: &Key::CmdCostDesc,
        keywords: &["成本", "价格", "费用", "tokens"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &COST,
    },
    Command {
        name: "scorecard",
        desc: &Key::CmdScorecardDesc,
        keywords: &["评分卡", "测光", "质量"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &SCORECARD,
    },
    Command {
        name: "skills",
        desc: &Key::CmdSkillsDesc,
        keywords: &["技能", "skills"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &SKILLS,
    },
    Command {
        name: "mcp",
        desc: &Key::CmdMcpDesc,
        keywords: &["mcp", "服务器"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &MCP,
    },
    Command {
        name: "undo",
        desc: &Key::CmdUndoDesc,
        keywords: &["撤销", "回滚", "快照"],
        args_spec: ArgsSpec::Enum(&["all", "list"]),
        args_hint: Some(&["all", "list"]),
        handler: &UNDO,
    },
    Command {
        name: "raw",
        desc: &Key::CmdRawDesc,
        keywords: &["显示", "模式", "raw"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &RAW,
    },
    Command {
        name: "fold",
        desc: &Key::CmdFoldDesc,
        keywords: &["折叠", "展开"],
        args_spec: ArgsSpec::Enum(&["all", "none", "reset"]),
        args_hint: Some(&["all", "none", "reset"]),
        handler: &FOLD,
    },
    Command {
        name: "copy",
        desc: &Key::CmdCopyDesc,
        keywords: &["复制"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &COPY,
    },
    Command {
        name: "quit",
        desc: &Key::CmdQuitDesc,
        keywords: &["退出", "exit", "q"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &QUIT,
    },
];

// ── 辅助函数（effort 解析/切换/标签）───────────────────────────

fn parse_effort_command(args: &str, tr: Tr) -> Result<ReasoningEffort, String> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return Err(tr.t(Key::EffortMissing).to_string());
    }
    ReasoningEffort::from_config_str(trimmed)
        .ok_or_else(|| tr.t_args(Key::EffortUnknown, &[("effort", trimmed)]))
}

fn toggle_thinking(current: ReasoningEffort, baseline: ReasoningEffort) -> ReasoningEffort {
    if current.thinking() {
        ReasoningEffort::Disabled
    } else {
        baseline
    }
}

fn effort_label(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Disabled => "disabled",
        ReasoningEffort::High => "high",
        ReasoningEffort::Max => "max",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::{AppState, DisplayMode};
    use crate::commands::{CommandRegistry, TuiCaps};
    use crate::model::conversation::{done_output, LineKind as LK};
    use deepseeknova_core::runner::RunEvent;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    /// 构造全 None caps（无注入能力的降级路径测试）。
    fn empty_caps() -> TuiCaps {
        TuiCaps {
            runtime: Arc::new(Mutex::new(crate::commands::TuiRuntime::default())),
            session: None,
            skills_paths: vec![PathBuf::from(".deepseeknova/skills")],
            mcp_servers: vec![],
            mcp_probe: None,
            undo: None,
            context_window: None,
            budget_window: None,
            approval_rx: None,
        }
    }

    async fn run_cmd(name: &str, args: &str, app: &mut AppState, caps: &TuiCaps) -> CommandOutcome {
        let cmd = CommandRegistry::find(name).expect("command exists");
        let mut ctx = CommandCtx { app, caps };
        cmd.handler.run(&mut ctx, args).await
    }

    #[test]
    fn registry_contains_all_builtin() {
        for name in [
            "help",
            "clear",
            "new",
            "sessions",
            "resume",
            "model",
            "cost",
            "skills",
            "mcp",
            "undo",
            "raw",
            "fold",
            "copy",
            "scorecard",
            "quit",
        ] {
            assert!(CommandRegistry::find(name).is_some(), "missing {name}");
        }
    }

    #[tokio::test]
    async fn quit_returns_quit() {
        let caps = empty_caps();
        let mut app = AppState::default();
        assert_eq!(
            run_cmd("quit", "", &mut app, &caps).await,
            CommandOutcome::Quit
        );
    }

    #[tokio::test]
    async fn clear_wipes_display_and_shows_notice() {
        let caps = empty_caps();
        let mut app = AppState {
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        app.conversation.begin_turn("q".into());
        app.apply_run_event(RunEvent::TextDelta("x".into()));
        app.apply_run_event(RunEvent::Done(done_output("")));
        run_cmd("clear", "", &mut app, &caps).await;
        assert_eq!(app.conversation.segment_count(), 0);
        assert!(app
            .notice
            .as_ref()
            .is_some_and(|(t, _)| t.contains("已清空")));
    }

    #[tokio::test]
    async fn raw_cycles_display_modes() {
        let caps = empty_caps();
        let mut app = AppState::default();
        run_cmd("raw", "", &mut app, &caps).await;
        assert_eq!(app.display_mode, DisplayMode::Lite);
        run_cmd("raw", "", &mut app, &caps).await;
        assert_eq!(app.display_mode, DisplayMode::Raw);
        run_cmd("raw", "", &mut app, &caps).await;
        assert_eq!(app.display_mode, DisplayMode::Normal);
    }

    #[tokio::test]
    async fn cost_without_router_reports_unavailable() {
        let caps = empty_caps();
        let mut app = AppState {
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        run_cmd("cost", "", &mut app, &caps).await;
        assert!(app
            .notice
            .as_ref()
            .is_some_and(|(t, _)| t.contains("router 不可用")));
    }

    #[tokio::test]
    async fn fold_all_none_reset_control_fold_state() {
        let caps = empty_caps();
        let mut app = AppState::default();
        let id = app.conversation.begin_turn("q".into());
        app.apply_run_event(RunEvent::ReasoningDelta {
            text: "r".into(),
            signature: None,
        });
        app.apply_run_event(RunEvent::TextDelta("a".into()));
        app.apply_run_event(RunEvent::Done(done_output("")));
        assert!(app.is_folded((id, 0), LK::Reasoning), "默认推理折叠");
        assert!(!app.is_folded((id, 1), LK::Agent));

        run_cmd("fold", "none", &mut app, &caps).await;
        assert!(!app.is_folded((id, 0), LK::Reasoning));
        run_cmd("fold", "all", &mut app, &caps).await;
        assert!(app.is_folded((id, 1), LK::Agent));
        run_cmd("fold", "reset", &mut app, &caps).await;
        assert!(app.is_folded((id, 0), LK::Reasoning));
        assert!(!app.is_folded((id, 1), LK::Agent));
    }

    #[tokio::test]
    async fn help_lists_commands() {
        let caps = empty_caps();
        let mut app = AppState::default();
        run_cmd("help", "", &mut app, &caps).await;
        // /help 打开可滚动浮层，不再往对话面板 echo 30+ 行（用户反馈污染）。
        assert!(app.help_overlay.is_some(), "帮助走浮层");
        assert!(app.echo.is_empty(), "帮助不进 echo 通道");
        let overlay = app.help_overlay.as_ref().unwrap();
        assert!(overlay.lines.iter().any(|l| l.contains("/fold")));
        assert!(overlay.lines.iter().any(|l| l.contains("/copy")));
        assert!(overlay.lines.iter().any(|l| l.contains("Ctrl+K")));
    }

    #[tokio::test]
    async fn unknown_command_echoes_error() {
        // 未知命令由事件循环回显错误；注册表层面验证找不到。
        assert!(CommandRegistry::find("wat").is_none());
    }

    #[tokio::test]
    async fn model_without_factory_degrades() {
        let caps = empty_caps();
        let mut app = AppState {
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        run_cmd("model", "switch deepseek-v4-pro", &mut app, &caps).await;
        assert!(app
            .notice
            .as_ref()
            .is_some_and(|(t, _)| t.contains("模型切换不可用")));
    }
}

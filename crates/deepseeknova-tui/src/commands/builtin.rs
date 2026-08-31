//! 内建命令：斜杠命令与 `/` 命令面板共用。
//!
//! 命令逻辑从旧 `TuiRunner::handle_command` / `AppState::execute_command` 迁移，
//! 注入能力统一经 [`crate::commands::TuiCaps`]（Option）读取，缺失时降级回显提示。

use async_trait::async_trait;
use deepseeknova_provider::cost::ModelRole;
use deepseeknova_provider::factory::ReasoningEffort;

use super::{ArgsSpec, Command, CommandCtx, CommandHandler, CommandOutcome, CommandRegistry};
use crate::app::state::{display_mode_label, fold_policy_label, FoldPolicy, McpStatus, TurnView};
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
            Key::HelpKeyFocus,
            Key::HelpKeyGlobal,
            Key::HelpKeyEsc,
            Key::HelpKeyShortcuts,
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
                    // 会话切换：cache 命中率统计随新会话重置。
                    ctx.app.session_cache_hit = 0;
                    ctx.app.session_cache_miss = 0;
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
                        // title 优先；无命名回退 id（预览空时）。
                        let label = match (&m.title, m.preview.is_empty()) {
                            (Some(t), _) => format!("{t}  ({})", m.id),
                            (None, false) => format!("{} — {}", m.id, m.preview),
                            (None, true) => m.id.clone(),
                        };
                        // 非当前工作区的会话给 basename 标注（当前工作区不加噪声）。
                        let ws_tag = m
                            .workspace
                            .as_ref()
                            .filter(|w| **w != ctx.app.workspace_cwd)
                            .map(|w| {
                                tr.t_args(Key::SessionWorkspaceTag, &[("ws", &short_ws_label(w))])
                            })
                            .unwrap_or_default();
                        ctx.app
                            .echo_line(LineKind::System, &format!("  {label}{ws_tag}{marker}"));
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
                    // 恢复时若有命名 title 一并显示（查列表取，无则回退不带 title 文案）。
                    let title = match ctrl.list_sessions().await {
                        Ok(metas) => metas
                            .into_iter()
                            .find(|m| m.id == target)
                            .and_then(|m| m.title),
                        Err(_) => None,
                    };
                    let notice = match title {
                        Some(t) => tr.t_args(
                            Key::ResumeDoneTitled,
                            &[("target", target), ("title", &t), ("n", &count.to_string())],
                        ),
                        None => tr.t_args(
                            Key::ResumeDone,
                            &[("target", target), ("n", &count.to_string())],
                        ),
                    };
                    ctx.app.show_notice(notice);
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

struct RenameCmd;

#[async_trait]
impl CommandHandler for RenameCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, args: &str) -> CommandOutcome {
        let title = args.trim();
        let tr = ctx.app.tr;
        match &ctx.caps.session {
            Some(ctrl) if !title.is_empty() => match ctrl.current_session().await {
                Some(id) => match ctrl.rename(&id, title).await {
                    Ok(()) => ctx
                        .app
                        .show_notice(tr.t_args(Key::RenameDone, &[("title", title)])),
                    Err(e) => ctx
                        .app
                        .show_notice(tr.t_args(Key::RenameFailed, &[("err", &e.to_string())])),
                },
                None => ctx.app.show_notice(tr.t(Key::SessionUnavailable)),
            },
            Some(_) => ctx.app.show_notice(tr.t(Key::RenameUsage)),
            None => ctx.app.show_notice(tr.t(Key::SessionUnavailable)),
        }
        CommandOutcome::Handled
    }
}

struct CheckpointCmd;

#[async_trait]
impl CommandHandler for CheckpointCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, args: &str) -> CommandOutcome {
        let tr = ctx.app.tr;
        let Some(ctrl) = &ctx.caps.checkpoint else {
            ctx.app.show_notice(tr.t(Key::CheckpointUnavailable));
            return CommandOutcome::Handled;
        };
        let (sub, sub_args) = args.split_once(' ').unwrap_or((args, ""));
        match sub {
            "save" => {
                let label = sub_args.trim();
                let label = if label.is_empty() {
                    None
                } else {
                    Some(label.to_string())
                };
                let lines = ctx.app.export_conversation_lines();
                match ctrl.save(label, lines).await {
                    Ok(id) => ctx
                        .app
                        .show_notice(tr.t_args(Key::CheckpointSaved, &[("id", &id)])),
                    Err(e) => ctx.app.show_notice(
                        tr.t_args(Key::CheckpointSaveFailed, &[("err", &e.to_string())]),
                    ),
                }
            }
            "list" => match ctrl.list().await {
                Ok(lines) if !lines.is_empty() => {
                    ctx.app
                        .echo_line(LineKind::System, tr.t(Key::CheckpointListHeader));
                    for line in lines {
                        ctx.app.echo_line(LineKind::System, &format!("  {line}"));
                    }
                }
                Ok(_) => ctx.app.show_notice(tr.t(Key::NoCheckpoints)),
                Err(e) => ctx
                    .app
                    .show_notice(tr.t_args(Key::CheckpointListFailed, &[("err", &e.to_string())])),
            },
            "rollback" => {
                let id = sub_args.trim();
                let id = if id.is_empty() { None } else { Some(id) };
                match ctrl.rollback(id).await {
                    Ok(Some(ck)) => {
                        ctx.app.restore_conversation(
                            crate::app::state::AppState::resumed_lines_from_checkpoint(&ck),
                        );
                        ctx.app.last_prompt = None;
                        ctx.app.show_notice(tr.t_args(
                            Key::CheckpointRollbackDone,
                            &[("id", &ck.id), ("n", &ck.conversation.len().to_string())],
                        ));
                    }
                    Ok(None) => ctx.app.show_notice(tr.t(Key::NoCheckpoints)),
                    Err(e) => ctx.app.show_notice(
                        tr.t_args(Key::CheckpointRollbackFailed, &[("err", &e.to_string())]),
                    ),
                }
            }
            "" => ctx.app.show_notice(tr.t(Key::CheckpointUsage)),
            other => ctx
                .app
                .show_notice(tr.t_args(Key::CheckpointUnknownArg, &[("arg", other)])),
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
                if !ctx.app.provider_configured {
                    ctx.app
                        .echo_line(LineKind::Error, tr.t(Key::WelcomeNoProvider));
                }
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
        use deepseeknova_core::registry::SkillScope;
        use deepseeknova_skills::SkillResolver;
        // 三层来源：builtin / user / project（project 层用 TUI 配置的 skills_paths，
        // 均视为项目级；缺省含 .deepseeknova/skills 与 .agents/skills）。
        let user_skills = dirs::home_dir()
            .map(|h| h.join(".deepseeknova/skills"))
            .unwrap_or_default();
        // 激活 deprecated 过滤：从工作区 fitness.json 读取已弃用技能名（与
        // runtime 装配点同源同路径语义），这些技能不在 `/skills` 展示；工作区
        // 未知时（caps.workspace_root=None）视为无 fitness 上下文，不过滤。
        let deprecated = ctx
            .caps
            .workspace_root
            .as_ref()
            .map(|ws| {
                deepseeknova_skills::fitness::load_deprecated_set(
                    &ws.join(".deepseeknova").join("skills").join("fitness.json"),
                )
            })
            .unwrap_or_default();
        let mut resolver = SkillResolver::new()
            .with_deprecated(deprecated)
            .add_preloaded(
                SkillScope::Builtin,
                deepseeknova_skills::load_builtin_skills(),
            )
            .add_source(SkillScope::User, user_skills);
        for path in &ctx.caps.skills_paths {
            resolver = resolver.add_source(SkillScope::Project, path);
        }
        let skills = resolver.resolve();
        if skills.is_empty() {
            ctx.app.show_notice(tr.t(Key::NoSkillsFound));
        } else {
            ctx.app.echo_line(LineKind::System, tr.t(Key::SkillsHeader));
            for skill in &skills {
                ctx.app.echo_line(
                    LineKind::System,
                    &format!(
                        "  • [{}] {} — {}",
                        skill.scope.label(),
                        skill.name,
                        skill.description
                    ),
                );
                if !skill.tools_allowed.is_empty() {
                    ctx.app.echo_line(
                        LineKind::System,
                        &format!("    tools: {}", skill.tools_allowed.join(", ")),
                    );
                }
            }
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
            "diff" => match ctrl.diffs().await {
                Ok(lines) if !lines.is_empty() => {
                    for line in lines {
                        ctx.app.echo_line(LineKind::System, &line);
                    }
                }
                Ok(_) => ctx.app.show_notice(tr.t(Key::NoDiffChanges)),
                Err(e) => ctx
                    .app
                    .show_notice(tr.t_args(Key::DiffFailed, &[("err", &e.to_string())])),
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
            // 策略级设置：auto 回智能默认（清显式折叠 + 切回 Auto 策略），
            // open/compact 直接设 fold_policy（字段 pub），全部走 notice 指示。
            "auto" => {
                ctx.app.fold_reset();
                ctx.app.fold_policy = FoldPolicy::Auto;
                ctx.app
                    .show_notice(tr.t(fold_policy_label(FoldPolicy::Auto)));
            }
            "open" => {
                ctx.app.fold_policy = FoldPolicy::Open;
                ctx.app
                    .show_notice(tr.t(fold_policy_label(FoldPolicy::Open)));
            }
            "compact" => {
                ctx.app.fold_policy = FoldPolicy::Compact;
                ctx.app
                    .show_notice(tr.t(fold_policy_label(FoldPolicy::Compact)));
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

/// 工作区路径的短标签：取最后一段（basename），空/根路径回退原样。
fn short_ws_label(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    normalized
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

// ── workspace（当前工作区 + 可用 worktree）────────────────

struct WorkspaceCmd;

#[async_trait]
impl CommandHandler for WorkspaceCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, _args: &str) -> CommandOutcome {
        let tr = ctx.app.tr;
        let cwd = if ctx.app.workspace_cwd.is_empty() {
            std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        } else {
            ctx.app.workspace_cwd.clone()
        };
        match &ctx.app.git_branch {
            Some(b) => ctx.app.echo_line(
                LineKind::System,
                &tr.t_args(Key::WorkspaceHeader, &[("path", &cwd), ("branch", b)]),
            ),
            None => ctx.app.echo_line(
                LineKind::System,
                &tr.t_args(Key::WorkspaceNoBranch, &[("path", &cwd)]),
            ),
        }
        ctx.app.echo_line(
            LineKind::System,
            &tr.t_args(
                Key::WorkspaceSessions,
                &[("n", &ctx.app.saved_sessions.len().to_string())],
            ),
        );
        // 每工作区会话数明细（非当前工作区 + 全局），一眼看出会话分布。
        let mut counts: Vec<(String, usize)> = Vec::new();
        let mut global = 0usize;
        for m in &ctx.app.saved_sessions {
            match &m.workspace {
                Some(ws) => {
                    if let Some((_, c)) = counts.iter_mut().find(|(w, _)| w == ws) {
                        *c += 1;
                    } else {
                        counts.push((ws.clone(), 1));
                    }
                }
                None => global += 1,
            }
        }
        for (ws, c) in counts {
            ctx.app.echo_line(
                LineKind::System,
                &tr.t_args(
                    Key::WorkspaceCountRow,
                    &[("ws", &short_ws_label(&ws)), ("n", &c.to_string())],
                ),
            );
        }
        if global > 0 {
            ctx.app.echo_line(
                LineKind::System,
                &tr.t_args(
                    Key::WorkspaceCountRow,
                    &[
                        ("ws", tr.t(Key::SidebarGlobalSessions)),
                        ("n", &global.to_string()),
                    ],
                ),
            );
        }
        ctx.app
            .echo_line(LineKind::System, tr.t(Key::WorkspaceGlobalSessions));
        // worktree 列表（best-effort；非 git 工作区/无 worktree 时显示占位）。
        ctx.app
            .echo_line(LineKind::System, tr.t(Key::WorktreesHeader));
        let mut any = false;
        if let Ok(out) = std::process::Command::new("git")
            .args(["worktree", "list"])
            .output()
        {
            if out.status.success() {
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    any = true;
                    let cols: Vec<&str> = line.split_whitespace().collect();
                    let path = cols.first().copied().unwrap_or("").to_string();
                    let branch = cols
                        .iter()
                        .find(|p| p.starts_with('[') && p.ends_with(']'))
                        .map(|b| b.trim_matches(|c| c == '[' || c == ']').to_string())
                        .unwrap_or_else(|| tr.t(Key::WorkspaceNoBranch).to_string());
                    ctx.app.echo_line(
                        LineKind::System,
                        &tr.t_args(Key::WorktreeRow, &[("path", &path), ("branch", &branch)]),
                    );
                }
            }
        }
        if !any {
            ctx.app
                .echo_line(LineKind::System, tr.t(Key::WorktreesNone));
        }
        ctx.app.echo_line(
            LineKind::System,
            &tr.t_args(
                Key::WorkspaceSwitchHint,
                &[("cmd", "cd <path> && deepseeknova-cli chat --tui")],
            ),
        );
        ctx.app.echo_line(
            LineKind::System,
            &tr.t_args(
                Key::WorkspaceIsolationHint,
                &[("cmd", "deepseeknova-cli worktree new")],
            ),
        );
        CommandOutcome::Handled
    }
}

// ── mode（权限模式预设）─────────────────────────────────────────

/// 权限模式标签（经 i18n 词表取当前语言值）。
fn mode_label(tr: Tr, mode: Option<deepseeknova_permission::PermissionMode>) -> String {
    tr.t(crate::app::state::permission_mode_label(mode))
        .to_string()
}

struct ModeCmd;

#[async_trait]
impl CommandHandler for ModeCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, args: &str) -> CommandOutcome {
        let tr = ctx.app.tr;
        let arg = args.trim();
        let gate = ctx
            .caps
            .runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .permission
            .clone();
        let Some(gate) = gate else {
            ctx.app.show_notice(tr.t(Key::PermModeGateUnavailable));
            return CommandOutcome::Handled;
        };
        let current = gate.mode();
        match arg {
            "" | "show" => {
                ctx.app.show_notice(
                    tr.t_args(Key::PermModeNotice, &[("mode", &mode_label(tr, current))]),
                );
            }
            "cycle" => {
                let next = crate::app::state::next_permission_mode(current);
                gate.set_mode(Some(next));
                ctx.app.permission_mode = Some(next);
                ctx.app.show_notice(tr.t_args(
                    Key::PermModeNotice,
                    &[("mode", &mode_label(tr, Some(next)))],
                ));
            }
            other => {
                use deepseeknova_permission::PermissionMode;
                let next = match other {
                    "plan" => PermissionMode::Plan,
                    "accept_edits" => PermissionMode::AcceptEdits,
                    "auto" => PermissionMode::Auto,
                    _ => {
                        ctx.app.show_notice(tr.t(Key::PermModeUsage));
                        return CommandOutcome::Handled;
                    }
                };
                gate.set_mode(Some(next));
                ctx.app.permission_mode = Some(next);
                ctx.app.show_notice(tr.t_args(
                    Key::PermModeNotice,
                    &[("mode", &mode_label(tr, Some(next)))],
                ));
            }
        }
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
static RENAME: RenameCmd = RenameCmd;
static CHECKPOINT: CheckpointCmd = CheckpointCmd;
static MODEL: ModelCmd = ModelCmd;
static COST: CostCmd = CostCmd;
static SCORECARD: ScorecardCmd = ScorecardCmd;
static SKILLS: SkillsCmd = SkillsCmd;
static MCP: McpCmd = McpCmd;
static UNDO: UndoCmd = UndoCmd;
static RAW: RawCmd = RawCmd;
static FOLD: FoldCmd = FoldCmd;
static COPY: CopyCmd = CopyCmd;
static MODE: ModeCmd = ModeCmd;
static QUIT: QuitCmd = QuitCmd;
static WORKSPACE: WorkspaceCmd = WorkspaceCmd;
static JUMP: JumpCmd = JumpCmd;

/// /jump：跳转到指定回合（`/jump <n>`，n ∈ 1..=总回合数）。
///
/// 切到单回合视图并选中目标回合（grok 的 turn jump 对齐）；参数非法时
/// 回显用法提示，不破坏当前视图。
struct JumpCmd;

#[async_trait]
impl CommandHandler for JumpCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, args: &str) -> CommandOutcome {
        let tr = ctx.app.tr;
        let total = ctx.app.conversation.turn_count();
        match args.trim().parse::<usize>() {
            Ok(n) if n >= 1 && n <= total => {
                ctx.app.selected_turn = Some(n - 1);
                ctx.app.turn_view = TurnView::Single;
                ctx.app.show_notice(tr.t_args(
                    Key::JumpedTo,
                    &[("n", &n.to_string()), ("total", &total.to_string())],
                ));
            }
            _ => {
                ctx.app
                    .show_notice(tr.t_args(Key::JumpUsage, &[("total", &total.to_string())]));
            }
        }
        CommandOutcome::Handled
    }
}

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
        name: "rename",
        desc: &Key::CmdRenameDesc,
        keywords: &["命名", "重命名", "title"],
        args_spec: ArgsSpec::FreeText,
        args_hint: Some(&["<title>"]),
        handler: &RENAME,
    },
    Command {
        name: "checkpoint",
        desc: &Key::CmdCheckpointDesc,
        keywords: &["检查点", "快照", "回退", "rollback", "checkpoint"],
        args_spec: ArgsSpec::FreeText,
        args_hint: Some(&["save [label]", "list", "rollback [id]"]),
        handler: &CHECKPOINT,
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
        args_spec: ArgsSpec::Enum(&["all", "none", "reset", "auto", "open", "compact"]),
        args_hint: Some(&["all", "none", "reset", "auto", "open", "compact"]),
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
        name: "workspace",
        desc: &Key::CmdWorkspaceDesc,
        keywords: &["工作区", "workspace", "worktree"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &WORKSPACE,
    },
    Command {
        name: "jump",
        desc: &Key::CmdJumpDesc,
        keywords: &["跳转", "回合", "turn"],
        args_spec: ArgsSpec::FreeText,
        args_hint: Some(&["<n>"]),
        handler: &JUMP,
    },
    Command {
        name: "mode",
        desc: &Key::CmdModeDesc,
        keywords: &["权限", "模式", "perm", "mode"],
        args_spec: ArgsSpec::Enum(&["plan", "accept_edits", "auto", "cycle"]),
        args_hint: Some(&["plan", "accept_edits", "auto", "cycle"]),
        handler: &MODE,
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
    use crate::app::state::{AppState, DisplayMode, FoldPolicy};
    use crate::commands::{CommandRegistry, TuiCaps, TuiRuntime};
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
            checkpoint: None,
            context_window: None,
            budget_window: None,
            approval_rx: None,
            trust: None,
            workspace_root: None,
            project_rule_count: 0,
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
            "rename",
            "checkpoint",
            "model",
            "cost",
            "skills",
            "mcp",
            "undo",
            "raw",
            "fold",
            "copy",
            "scorecard",
            "mode",
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
    async fn fold_auto_open_compact_set_fold_policy() {
        let caps = empty_caps();
        let mut app = AppState::default();
        let id = app.conversation.begin_turn("q".into());
        app.apply_run_event(RunEvent::ReasoningDelta {
            text: "r".into(),
            signature: None,
        });
        app.apply_run_event(RunEvent::ToolCallStart {
            id: "t1".into(),
            name: "grep".into(),
        });
        app.apply_run_event(RunEvent::ToolResult {
            call_id: "t1".into(),
            result: "hit".into(),
        });
        app.apply_run_event(RunEvent::TextDelta("a".into()));
        app.apply_run_event(RunEvent::Done(done_output("")));

        // compact：推理 + 工具均默认折叠（含紧凑策略下工具折叠的契约）。
        run_cmd("fold", "compact", &mut app, &caps).await;
        assert_eq!(app.fold_policy, FoldPolicy::Compact);
        assert!(app.is_folded((id, 1), LK::Tool), "紧凑策略下工具默认折叠");
        assert!(app.is_folded((id, 0), LK::Reasoning));
        assert!(
            app.notice
                .as_ref()
                .is_some_and(|(t, _)| t.contains("compact")),
            "compact 分支有 notice 指示"
        );

        // open：推理与工具默认展开。
        run_cmd("fold", "open", &mut app, &caps).await;
        assert_eq!(app.fold_policy, FoldPolicy::Open);
        assert!(!app.is_folded((id, 0), LK::Reasoning));
        assert!(!app.is_folded((id, 1), LK::Tool));

        // auto：清显式折叠 + 回智能默认（推理折叠、工具展开）。
        app.fold
            .insert((id, 1), crate::app::state::FoldState::Collapsed);
        run_cmd("fold", "auto", &mut app, &caps).await;
        assert_eq!(app.fold_policy, FoldPolicy::Auto);
        assert!(app.fold.is_empty(), "auto 清空显式折叠设置");
        assert!(!app.is_folded((id, 1), LK::Tool));
        assert!(app.is_folded((id, 0), LK::Reasoning));
        assert!(
            app.notice.as_ref().is_some_and(|(t, _)| t.contains("auto")),
            "auto 分支有 notice 指示"
        );
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
        // 命令面板走 `/` 触发（无 Ctrl+K 绑定）；help 浮层应含命令面板说明。
        assert!(overlay
            .lines
            .iter()
            .any(|l| l.contains("命令面板") || l.contains("Command palette")));
    }

    #[tokio::test]
    async fn unknown_command_echoes_error() {
        // 未知命令由事件循环回显错误；注册表层面验证找不到。
        assert!(CommandRegistry::find("wat").is_none());
    }

    #[tokio::test]
    async fn workspace_command_registered_and_echoes_header() {
        assert!(
            CommandRegistry::find("workspace").is_some(),
            "/workspace 已注册"
        );
        let caps = empty_caps();
        let mut app = AppState {
            tr: Tr::new(crate::i18n::Lang::Zh),
            workspace_cwd: "/tmp/demo".into(),
            git_branch: Some("main".into()),
            ..Default::default()
        };
        run_cmd("workspace", "", &mut app, &caps).await;
        assert!(
            app.echo
                .iter()
                .any(|l| l.text.contains("工作区: /tmp/demo（main）")),
            "工作区头回显: {:?}",
            app.echo.iter().map(|l| l.text.clone()).collect::<Vec<_>>()
        );
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

    #[tokio::test]
    async fn mode_command_cycles_and_sets_permission_mode() {
        use deepseeknova_permission::{Decision, PermissionGate, PermissionMode, Policy};
        let gate = Arc::new(PermissionGate::new(Policy {
            mode: Decision::Ask,
            allow: vec![],
            ask: vec![],
            deny: vec![],
        }));
        let caps = TuiCaps {
            runtime: Arc::new(Mutex::new(TuiRuntime {
                permission: Some(gate.clone()),
                ..Default::default()
            })),
            session: None,
            skills_paths: vec![PathBuf::from(".deepseeknova/skills")],
            mcp_servers: vec![],
            mcp_probe: None,
            undo: None,
            checkpoint: None,
            context_window: None,
            budget_window: None,
            approval_rx: None,
            trust: None,
            workspace_root: None,
            project_rule_count: 0,
        };
        let mut app = AppState::default();
        // 默认 None → cycle → Plan。
        run_cmd("mode", "cycle", &mut app, &caps).await;
        assert_eq!(gate.mode(), Some(PermissionMode::Plan));
        assert_eq!(app.permission_mode, Some(PermissionMode::Plan));
        // 显式 auto。
        run_cmd("mode", "auto", &mut app, &caps).await;
        assert_eq!(gate.mode(), Some(PermissionMode::Auto));
        // 未知参数 → 用法提示。
        let mut app = AppState {
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        run_cmd("mode", "banana", &mut app, &caps).await;
        assert!(
            app.notice.as_ref().is_some_and(|(t, _)| t.contains("用法")),
            "未知参数应给用法: {:?}",
            app.notice
        );
        // 仍停在 auto（未知参数不改变状态）。
        assert_eq!(gate.mode(), Some(PermissionMode::Auto));
    }

    #[tokio::test]
    async fn mode_without_gate_degrades() {
        let caps = empty_caps();
        let mut app = AppState {
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        run_cmd("mode", "", &mut app, &caps).await;
        assert!(app
            .notice
            .as_ref()
            .is_some_and(|(t, _)| t.contains("不可用")));
    }

    // ── /rename /checkpoint /sessions title ───────────────────────────

    /// 内存版会话控制器（/rename /sessions 测试桩）。
    struct MockSessionController {
        current: std::sync::Mutex<Option<String>>,
        metas: std::sync::Mutex<Vec<crate::app::state::SessionMeta>>,
        renamed: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl MockSessionController {
        fn new(current: Option<String>, metas: Vec<crate::app::state::SessionMeta>) -> Self {
            Self {
                current: std::sync::Mutex::new(current),
                metas: std::sync::Mutex::new(metas),
                renamed: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl crate::app::state::SessionController for MockSessionController {
        async fn new_session(&self) -> Result<(), deepseeknova_core::DeepseeknovaError> {
            Ok(())
        }
        async fn list_sessions(
            &self,
        ) -> Result<Vec<crate::app::state::SessionMeta>, deepseeknova_core::DeepseeknovaError>
        {
            Ok(self.metas.lock().unwrap().clone())
        }
        async fn current_session(&self) -> Option<String> {
            self.current.lock().unwrap().clone()
        }
        async fn rename(
            &self,
            id: &str,
            title: &str,
        ) -> Result<(), deepseeknova_core::DeepseeknovaError> {
            self.renamed
                .lock()
                .unwrap()
                .push((id.to_string(), title.to_string()));
            Ok(())
        }
        async fn resume(
            &self,
            _id: &str,
        ) -> Result<Vec<crate::app::state::ResumedLine>, deepseeknova_core::DeepseeknovaError>
        {
            Ok(Vec::new())
        }
        async fn record_turn(
            &self,
            _prompt: &str,
            _output_text: &str,
            _model: Option<String>,
        ) -> Result<(), deepseeknova_core::DeepseeknovaError> {
            Ok(())
        }
    }

    /// 内存版会话检查点控制器（/checkpoint 测试桩）。
    #[derive(Default)]
    struct MockCheckpointController {
        checkpoints: std::sync::Mutex<Vec<deepseeknova_checkpoint::SessionCheckpoint>>,
    }

    #[async_trait]
    impl crate::app::state::SessionCheckpointController for MockCheckpointController {
        async fn save(
            &self,
            label: Option<String>,
            conversation: Vec<deepseeknova_checkpoint::ConversationLine>,
        ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
            let mut cks = self.checkpoints.lock().unwrap();
            let id = format!("ck-{}", cks.len());
            cks.push(deepseeknova_checkpoint::SessionCheckpoint {
                id: id.clone(),
                created_at: chrono::Utc::now(),
                label,
                conversation,
                files: Vec::new(),
            });
            Ok(id)
        }
        async fn list(&self) -> Result<Vec<String>, deepseeknova_core::DeepseeknovaError> {
            Ok(self
                .checkpoints
                .lock()
                .unwrap()
                .iter()
                .map(|c| c.id.clone())
                .collect())
        }
        async fn rollback(
            &self,
            id: Option<&str>,
        ) -> Result<
            Option<deepseeknova_checkpoint::SessionCheckpoint>,
            deepseeknova_core::DeepseeknovaError,
        > {
            let mut cks = self.checkpoints.lock().unwrap();
            let idx = match id {
                Some(id) => cks.iter().position(|c| c.id == id),
                None => cks.len().checked_sub(1),
            };
            Ok(idx.map(|i| cks.remove(i)))
        }
    }

    #[tokio::test]
    async fn rename_renames_current_session() {
        let ctrl = Arc::new(MockSessionController::new(
            Some("chat-20260807-000001".into()),
            vec![],
        ));
        let caps = TuiCaps {
            session: Some(ctrl.clone() as Arc<dyn crate::app::state::SessionController>),
            ..empty_caps()
        };
        let mut app = AppState {
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        run_cmd("rename", "  项目重构  ", &mut app, &caps).await;
        let renamed = ctrl.renamed.lock().unwrap();
        assert_eq!(renamed.len(), 1, "rename 应作用于当前会话");
        assert_eq!(renamed[0].0, "chat-20260807-000001");
        assert_eq!(renamed[0].1, "项目重构", "参数应 trim");
        assert!(app
            .notice
            .as_ref()
            .is_some_and(|(t, _)| t.contains("已将会话重命名为")));
    }

    #[tokio::test]
    async fn rename_without_title_shows_usage() {
        let ctrl = Arc::new(MockSessionController::new(Some("chat-x".into()), vec![]));
        let caps = TuiCaps {
            session: Some(ctrl.clone() as Arc<dyn crate::app::state::SessionController>),
            ..empty_caps()
        };
        let mut app = AppState {
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        run_cmd("rename", "   ", &mut app, &caps).await;
        assert!(app.notice.as_ref().is_some_and(|(t, _)| t.contains("用法")));
        assert!(ctrl.renamed.lock().unwrap().is_empty(), "空参数不改名");
    }

    #[tokio::test]
    async fn rename_without_session_degrades() {
        let caps = empty_caps();
        let mut app = AppState {
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        run_cmd("rename", "标题", &mut app, &caps).await;
        assert!(app
            .notice
            .as_ref()
            .is_some_and(|(t, _)| t.contains("会话管理不可用")));
    }

    #[tokio::test]
    async fn resume_shows_title_when_named() {
        let metas = vec![crate::app::state::SessionMeta {
            id: "chat-20260807-000001".into(),
            preview: "旧预览".into(),
            title: Some("项目重构".into()),
            workspace: None,
        }];
        let ctrl = Arc::new(MockSessionController::new(Some("chat-x".into()), metas));
        let caps = TuiCaps {
            session: Some(ctrl.clone() as Arc<dyn crate::app::state::SessionController>),
            ..empty_caps()
        };
        let mut app = AppState {
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        run_cmd("resume", "chat-20260807-000001", &mut app, &caps).await;
        assert!(
            app.notice
                .as_ref()
                .is_some_and(|(t, _)| t.contains("已恢复") && t.contains("项目重构")),
            "恢复时显示命名 title: {:?}",
            app.notice
        );
    }

    #[tokio::test]
    async fn checkpoint_save_list_rollback_roundtrip_restores_conversation() {
        let ctrl = Arc::new(MockCheckpointController::default());
        let caps = TuiCaps {
            checkpoint: Some(
                ctrl.clone() as Arc<dyn crate::app::state::SessionCheckpointController>
            ),
            ..empty_caps()
        };
        let mut app = AppState {
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        // 第一轮 → save。
        app.conversation.begin_turn("第一问".into());
        app.apply_run_event(RunEvent::TextDelta("第一答".into()));
        app.apply_run_event(RunEvent::Done(done_output("")));
        run_cmd("checkpoint", "save 阶段一", &mut app, &caps).await;
        assert!(app
            .notice
            .as_ref()
            .is_some_and(|(t, _)| t.contains("检查点已保存")));
        // 第二轮 → 记录继续对话后的状态。
        app.conversation.begin_turn("第二问".into());
        app.apply_run_event(RunEvent::TextDelta("第二答".into()));
        app.apply_run_event(RunEvent::Done(done_output("")));
        assert_eq!(app.conversation.turn_count(), 2);
        // list 展示检查点。
        run_cmd("checkpoint", "list", &mut app, &caps).await;
        assert!(
            app.echo.iter().any(|l| l.text.contains("ck-")),
            "list 应含检查点 id: {:?}",
            app.echo
        );
        // rollback（最新）→ 回到快照点的 1 个回合。
        run_cmd("checkpoint", "rollback", &mut app, &caps).await;
        assert_eq!(app.conversation.turn_count(), 1, "回退后回到快照点");
        assert_eq!(app.conversation.user_text_of(1), Some("第一问"));
        assert!(app
            .notice
            .as_ref()
            .is_some_and(|(t, _)| t.contains("已回退到检查点")));
    }

    #[tokio::test]
    async fn checkpoint_without_caps_degrades() {
        let caps = empty_caps();
        let mut app = AppState {
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        run_cmd("checkpoint", "save", &mut app, &caps).await;
        assert!(app
            .notice
            .as_ref()
            .is_some_and(|(t, _)| t.contains("不可用")));
    }

    #[tokio::test]
    async fn sessions_list_prefers_title_over_preview_and_id() {
        let metas = vec![
            crate::app::state::SessionMeta {
                id: "chat-20260807-000001".into(),
                preview: "旧预览".into(),
                title: Some("项目重构".into()),
                workspace: None,
            },
            crate::app::state::SessionMeta {
                id: "chat-20260806-000001".into(),
                preview: String::new(),
                title: None,
                workspace: None,
            },
            crate::app::state::SessionMeta {
                id: "chat-20260805-000001".into(),
                preview: "有预览无标题".into(),
                title: None,
                workspace: None,
            },
        ];
        let ctrl = Arc::new(MockSessionController::new(
            Some("chat-20260807-000001".into()),
            metas,
        ));
        let caps = TuiCaps {
            session: Some(ctrl.clone() as Arc<dyn crate::app::state::SessionController>),
            ..empty_caps()
        };
        let mut app = AppState {
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        run_cmd("sessions", "", &mut app, &caps).await;
        let echo: String = app
            .echo
            .iter()
            .map(|l| l.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(echo.contains("项目重构"), "title 显示: {echo}");
        assert!(
            echo.contains("chat-20260807-000001"),
            "title 旁带 id: {echo}"
        );
        assert!(
            echo.contains("chat-20260806-000001"),
            "无 title 回退 id: {echo}"
        );
        assert!(
            echo.contains("有预览无标题"),
            "无 title 有预览回退预览: {echo}"
        );
        assert!(echo.contains("(当前)"), "当前标记: {echo}");
    }

    #[tokio::test]
    async fn skills_cmd_excludes_deprecated_skills() {
        // 工作区 fitness.json 标记一个内置技能 deprecated：/skills 不再展示。
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(ws.join(".deepseeknova/skills")).unwrap();
        let fitness_path = ws.join(".deepseeknova/skills/fitness.json");
        let mut store = deepseeknova_skills::fitness::FitnessStore::load(&fitness_path).unwrap();
        store.mark_deprecated("coding-copilot");
        store.save().unwrap();

        let caps = TuiCaps {
            workspace_root: Some(ws),
            ..empty_caps()
        };
        let mut app = AppState {
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        run_cmd("skills", "", &mut app, &caps).await;
        let echo: String = app
            .echo
            .iter()
            .map(|l| l.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !echo.contains("coding-copilot"),
            "deprecated skill must be hidden from /skills, got: {echo}"
        );
        assert!(
            echo.contains("frontend-developer"),
            "non-deprecated builtin must still be shown, got: {echo}"
        );
    }
}

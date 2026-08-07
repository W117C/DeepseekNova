//! 内建命令：斜杠命令与 Ctrl+K 命令面板共用。
//!
//! 命令逻辑从旧 `TuiRunner::handle_command` / `AppState::execute_command` 迁移，
//! 注入能力统一经 [`crate::commands::TuiCaps`]（Option）读取，缺失时降级回显提示。

use async_trait::async_trait;
use deepseeknova_provider::cost::ModelRole;
use deepseeknova_provider::factory::ReasoningEffort;

use super::{ArgsSpec, Command, CommandCtx, CommandHandler, CommandOutcome};
use crate::app::state::McpStatus;
use crate::model::conversation::LineKind;
use crate::model::scorecard::Scorecard;

// ── help ────────────────────────────────────────────────────────

struct HelpCmd;

#[async_trait]
impl CommandHandler for HelpCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, _args: &str) -> CommandOutcome {
        ctx.app.echo_line(LineKind::System, "可用命令:");
        for line in [
            "  /help          显示帮助",
            "  /clear         清空对话面板",
            "  /new           开始新会话",
            "  /sessions      列出已保存会话",
            "  /resume <id>   恢复指定会话",
            "  /model         模型与 effort 热切换",
            "  /cost          会话成本报表",
            "  /scorecard     读取最新测光评分卡",
            "  /skills        列出可用技能",
            "  /mcp           列出已配置 MCP 服务器",
            "  /raw           切换显示模式（normal/lite/raw）",
            "  /fold          折叠控制（all/none/reset）",
            "  /copy          复制当前选中消息",
            "  /undo          回滚最近一个快照",
            "  /undo all      回滚全部快照",
            "  /undo list     列出快照与状态",
            "  /quit          退出 TUI（Esc）",
            "  Ctrl+K         命令面板",
            "  j/k            Conversation 焦点下消息导航",
            "  Enter          折叠/展开选中消息",
            "  y              复制选中消息",
            "  PageUp/Down    滚动回看",
            "  ↑/↓            输入历史（多行时移动光标）",
            "  Shift+Enter    换行（Ctrl+J 同）",
            "  ←/→/Home/End  输入内移动光标（空闲时）",
            "  Delete/Backspace 编辑输入",
            "  Ctrl+U/W       清空输入 / 删前一词",
            "  Ctrl+C         取消当前运行",
        ] {
            ctx.app.echo_line(LineKind::System, line);
        }
        CommandOutcome::Handled
    }
}

// ── clear ───────────────────────────────────────────────────────

struct ClearCmd;

#[async_trait]
impl CommandHandler for ClearCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, _args: &str) -> CommandOutcome {
        ctx.app.clear_display();
        ctx.app.show_notice("已清空对话面板");
        CommandOutcome::Handled
    }
}

// ── new / sessions / resume（会话）─────────────────────────────

struct NewCmd;

#[async_trait]
impl CommandHandler for NewCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, _args: &str) -> CommandOutcome {
        match &ctx.caps.session {
            Some(ctrl) => match ctrl.new_session().await {
                Ok(()) => {
                    ctx.app.clear_display();
                    ctx.app.last_prompt = None;
                    ctx.app.sessions_loaded = false;
                    ctx.app.show_notice("新会话已开始");
                }
                Err(e) => ctx.app.show_notice(format!("新建会话失败: {e}")),
            },
            None => ctx
                .app
                .show_notice("会话管理不可用（未提供 SessionController）"),
        }
        CommandOutcome::Handled
    }
}

struct SessionsCmd;

#[async_trait]
impl CommandHandler for SessionsCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, _args: &str) -> CommandOutcome {
        match &ctx.caps.session {
            Some(ctrl) => match ctrl.list_sessions().await {
                Ok(mut ids) if !ids.is_empty() => {
                    ids.sort();
                    ids.reverse(); // id 按时间字典序，最新优先
                    let current = ctrl.current_session().await;
                    ctx.app
                        .echo_line(LineKind::System, "已保存会话（最新优先）:");
                    for id in &ids {
                        let marker = if current.as_deref() == Some(id.as_str()) {
                            "  (当前)"
                        } else {
                            ""
                        };
                        ctx.app
                            .echo_line(LineKind::System, &format!("  {id}{marker}"));
                    }
                }
                Ok(_) => ctx.app.show_notice("（还没有已保存的会话）"),
                Err(e) => ctx.app.show_notice(format!("列出会话失败: {e}")),
            },
            None => ctx
                .app
                .show_notice("会话管理不可用（未提供 SessionController）"),
        }
        CommandOutcome::Handled
    }
}

struct ResumeCmd;

#[async_trait]
impl CommandHandler for ResumeCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, args: &str) -> CommandOutcome {
        let target = args.trim();
        match &ctx.caps.session {
            Some(ctrl) if !target.is_empty() => match ctrl.resume(target).await {
                Ok(lines) => {
                    let count = lines.len();
                    ctx.app.restore_conversation(lines);
                    ctx.app.last_prompt = None;
                    ctx.app.show_notice(format!(
                        "已恢复 '{target}' — {} 条消息（进入对话面板，可滚动/折叠）",
                        count
                    ));
                }
                Err(e) => ctx.app.show_notice(format!("恢复会话失败: {e}")),
            },
            Some(_) => ctx
                .app
                .show_notice("用法: /resume <session-id>（见 /sessions）"),
            None => ctx
                .app
                .show_notice("会话管理不可用（未提供 SessionController）"),
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
    let guard = ctx.caps.runtime.lock().unwrap();
    let Some(f) = guard.factory.clone() else {
        ctx.app.show_notice("模型切换不可用（未提供 agent 工厂）");
        return;
    };
    let eff = effort.unwrap_or(guard.current_effort);
    let mdl = model.or_else(|| guard.current_model.clone());
    drop(guard);
    match f(Some(eff), mdl.clone()) {
        Ok(runner) => {
            let mut guard = ctx.caps.runtime.lock().unwrap();
            guard.runner = Some(runner);
            guard.current_effort = eff;
            guard.current_model = mdl.clone();
            guard.model_label = mdl.unwrap_or_else(|| "default".to_string());
            ctx.app.show_notice(format!(
                "模型已切换: effort={} model={}",
                effort_label(eff),
                guard.model_label
            ));
        }
        Err(e) => ctx.app.show_notice(format!("模型切换失败: {e}")),
    }
}

struct ModelCmd;

#[async_trait]
impl CommandHandler for ModelCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, args: &str) -> CommandOutcome {
        let (sub, sub_args) = args.split_once(' ').unwrap_or((args, ""));
        let (current_effort, current_model, baseline_effort, has_router, has_factory) = {
            let g = ctx.caps.runtime.lock().unwrap();
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
                ctx.app.echo_line(LineKind::System, "Model commands:");
                for line in [
                    "  /model                  显示当前模型与帮助",
                    "  /model effort <level>   设置 reasoning effort: disabled|high|max",
                    "  /model thinking         切换 thinking 开/关",
                    "  /model switch <name>    切换到指定模型",
                    "  /model use <role> <name> 设置角色指针: main|task|compact|quick",
                ] {
                    ctx.app.echo_line(LineKind::System, line);
                }
                ctx.app.echo_line(
                    LineKind::System,
                    &format!(
                        "当前: effort={} model={}",
                        effort_label(current_effort),
                        current_model.as_deref().unwrap_or("(default)")
                    ),
                );
                if has_router {
                    let g = ctx.caps.runtime.lock().unwrap();
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
                                    r.pointer(role).unwrap_or_else(|| "(default)".to_string())
                                ),
                            );
                        }
                    }
                }
            }
            "effort" => {
                if sub_args.is_empty() {
                    ctx.app.show_notice(format!(
                        "当前 reasoning effort: {} (基线: {}); 用法: /model effort disabled|high|max",
                        effort_label(current_effort),
                        effort_label(baseline_effort)
                    ));
                } else {
                    match parse_effort_command(sub_args) {
                        Ok(effort) => rebuild_runner(ctx, Some(effort), None),
                        Err(msg) => ctx.app.show_notice(msg),
                    }
                }
            }
            "thinking" => {
                let new_effort = toggle_thinking(current_effort, baseline_effort);
                if new_effort != current_effort {
                    ctx.app.show_notice(format!(
                        "thinking {} → {}",
                        if current_effort.thinking() {
                            "on"
                        } else {
                            "off"
                        },
                        if new_effort.thinking() { "on" } else { "off" }
                    ));
                    rebuild_runner(ctx, Some(new_effort), None);
                } else {
                    ctx.app.show_notice("thinking 状态未变");
                }
            }
            "switch" => {
                if sub_args.is_empty() {
                    ctx.app
                        .show_notice("用法: /model switch <provider-model-name>");
                } else {
                    rebuild_runner(ctx, None, Some(sub_args.to_string()));
                }
            }
            "use" => {
                let mut parts = sub_args.split_whitespace();
                let (role_s, model) = (parts.next(), parts.next());
                if !has_router {
                    ctx.app
                        .show_notice("model pointers 不可用（未提供 router）");
                    return CommandOutcome::Handled;
                }
                let (Some(role_s), Some(model)) = (role_s, model) else {
                    ctx.app
                        .show_notice("用法: /model use <main|task|compact|quick> <model-name>");
                    return CommandOutcome::Handled;
                };
                let Some(role) = ModelRole::parse(role_s) else {
                    ctx.app.show_notice("未知角色（main|task|compact|quick）");
                    return CommandOutcome::Handled;
                };
                {
                    let g = ctx.caps.runtime.lock().unwrap();
                    match g.router.as_ref().map(|r| r.set_pointer(role, model)) {
                        Some(Ok(())) => {
                            ctx.app
                                .show_notice(format!("pointer {} → {model}", role.label()));
                        }
                        Some(Err(e)) => {
                            ctx.app.show_notice(e.to_string());
                            return CommandOutcome::Handled;
                        }
                        None => {
                            ctx.app.show_notice("router 不可用");
                            return CommandOutcome::Handled;
                        }
                    }
                }
                rebuild_runner(ctx, None, None);
            }
            other => {
                ctx.app
                    .show_notice(format!("未知 /model 子命令: {other}（/model help 查看）"));
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
        let Some(r) = ctx.caps.runtime.lock().unwrap().router.clone() else {
            ctx.app
                .show_notice("router 不可用（/cost 需要 ModelRouter）");
            return CommandOutcome::Handled;
        };
        let report = r.ledger().report(&r.price_table());
        if report.rows.is_empty() {
            ctx.app.show_notice("还没有用量记录");
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
            ctx.app
                .echo_line(LineKind::System, &format!("总计: ${total:.6}"));
        }
        if report.unmetered_calls > 0 {
            ctx.app.echo_line(
                LineKind::System,
                &format!("（未计量调用: {}）", report.unmetered_calls),
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
            ctx.app
                .show_notice("未找到测光数据（.deepseeknova/metrics 无评分卡 JSON）");
            return CommandOutcome::Handled;
        };
        ctx.app.scorecard = Some(sc.clone());
        ctx.app
            .echo_line(LineKind::System, "测光·评分卡（最近一次 run）");
        for row in &sc.rows {
            let bar = crate::model::scorecard::photometry_bar(row.score);
            ctx.app.echo_line(
                LineKind::System,
                &format!(" {:<4} {bar} {:>5.1}", row.dim, row.score),
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
        let mut found = false;
        for path in &ctx.caps.skills_paths {
            let loader = deepseeknova_skills::SkillLoader::new(path);
            match loader.load_all() {
                Ok(skills) if !skills.is_empty() => {
                    if !found {
                        ctx.app.echo_line(LineKind::System, "可用技能:");
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
                Err(e) => ctx
                    .app
                    .show_notice(format!("加载技能失败 {}: {e}", path.display())),
            }
        }
        if !found {
            ctx.app
                .show_notice("（未找到技能，可创建 .md 文件放到 .deepseeknova/skills/）");
        }
        CommandOutcome::Handled
    }
}

struct McpCmd;

#[async_trait]
impl CommandHandler for McpCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, _args: &str) -> CommandOutcome {
        if ctx.caps.mcp_servers.is_empty() {
            ctx.app.show_notice(
                "未配置 MCP 服务器\n在 deepseeknova.toml 顶层 mcp_servers 数组配置后重启生效",
            );
            return CommandOutcome::Handled;
        }
        let statuses = match &ctx.caps.mcp_probe {
            Some(probe) => probe.probe(&ctx.caps.mcp_servers).await,
            None => Vec::new(),
        };
        ctx.app
            .echo_line(LineKind::System, "已配置 MCP 服务器（实时状态）:");
        for (i, server) in ctx.caps.mcp_servers.iter().enumerate() {
            let line = match statuses.get(i) {
                Some(McpStatus::Connected) => format!("  • {} — ✓ 已连接", server.name),
                Some(McpStatus::Disconnected(reason)) => {
                    format!("  • {} — ✗ 未连接（{reason}）", server.name)
                }
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
        let Some(ctrl) = &ctx.caps.undo else {
            ctx.app.show_notice("撤销不可用（未提供 UndoController）");
            return CommandOutcome::Handled;
        };
        match args.trim() {
            "" => match ctrl.rollback_one().await {
                Ok(Some(msg)) => ctx.app.show_notice(format!("✓ {msg}")),
                Ok(None) => ctx.app.show_notice("没有可回滚的快照"),
                Err(e) => ctx.app.show_notice(format!("撤销失败: {e}")),
            },
            "all" => match ctrl.rollback_all().await {
                Ok(n) => ctx.app.show_notice(format!("已全部回滚 {n} 个快照")),
                Err(e) => ctx.app.show_notice(format!("撤销失败: {e}")),
            },
            "list" => match ctrl.list().await {
                Ok(lines) if !lines.is_empty() => {
                    ctx.app.echo_line(LineKind::System, "快照列表:");
                    for line in lines {
                        ctx.app.echo_line(LineKind::System, &format!("  {line}"));
                    }
                }
                Ok(_) => ctx.app.show_notice("（没有快照）"),
                Err(e) => ctx.app.show_notice(format!("列出快照失败: {e}")),
            },
            other => ctx.app.show_notice(format!(
                "未知参数: {other}（用法: /undo | /undo all | /undo list）"
            )),
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
        ctx.app.show_notice(format!(
            "显示模式: {}",
            crate::app::state::display_mode_label(ctx.app.display_mode)
        ));
        CommandOutcome::Handled
    }
}

// ── fold / copy ─────────────────────────────────────────────────

struct FoldCmd;

#[async_trait]
impl CommandHandler for FoldCmd {
    async fn run(&self, ctx: &mut CommandCtx<'_>, args: &str) -> CommandOutcome {
        match args.trim() {
            "all" => {
                ctx.app.fold_all(true);
                ctx.app
                    .show_notice(format!("已折叠全部消息（当前: {}）", ctx.app.fold_label()));
            }
            "none" => {
                ctx.app.fold_all(false);
                ctx.app
                    .show_notice(format!("已展开全部消息（当前: {}）", ctx.app.fold_label()));
            }
            "reset" => {
                ctx.app.fold_reset();
                ctx.app.show_notice("已重置折叠态（回智能默认）");
            }
            "" => ctx.app.show_notice("用法: /fold all | none | reset"),
            other => ctx
                .app
                .show_notice(format!("未知参数: {other}（all|none|reset）")),
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
        desc: "显示帮助与全部命令",
        keywords: &["帮助", "命令", "h"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &HELP,
    },
    Command {
        name: "clear",
        desc: "清空对话面板",
        keywords: &["清空"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &CLEAR,
    },
    Command {
        name: "new",
        desc: "开始新会话",
        keywords: &["新会话", "会话"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &NEW,
    },
    Command {
        name: "sessions",
        desc: "列出已保存会话",
        keywords: &["会话", "历史"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &SESSIONS,
    },
    Command {
        name: "resume",
        desc: "恢复指定会话",
        keywords: &["会话", "恢复"],
        args_spec: ArgsSpec::FreeText,
        args_hint: Some(&["<session-id>"]),
        handler: &RESUME,
    },
    Command {
        name: "model",
        desc: "模型与 effort 热切换",
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
        desc: "会话成本报表",
        keywords: &["成本", "价格", "费用", "tokens"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &COST,
    },
    Command {
        name: "scorecard",
        desc: "读取最新测光评分卡（六维光度表）",
        keywords: &["评分卡", "测光", "质量"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &SCORECARD,
    },
    Command {
        name: "skills",
        desc: "列出可用技能",
        keywords: &["技能", "skills"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &SKILLS,
    },
    Command {
        name: "mcp",
        desc: "列出已配置 MCP 服务器（实时状态）",
        keywords: &["mcp", "服务器"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &MCP,
    },
    Command {
        name: "undo",
        desc: "回滚快照（all/list）",
        keywords: &["撤销", "回滚", "快照"],
        args_spec: ArgsSpec::Enum(&["all", "list"]),
        args_hint: Some(&["all", "list"]),
        handler: &UNDO,
    },
    Command {
        name: "raw",
        desc: "切换显示模式（normal/lite/raw）",
        keywords: &["显示", "模式", "raw"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &RAW,
    },
    Command {
        name: "fold",
        desc: "折叠控制（all/none/reset）",
        keywords: &["折叠", "展开"],
        args_spec: ArgsSpec::Enum(&["all", "none", "reset"]),
        args_hint: Some(&["all", "none", "reset"]),
        handler: &FOLD,
    },
    Command {
        name: "copy",
        desc: "复制当前选中消息",
        keywords: &["复制"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &COPY,
    },
    Command {
        name: "quit",
        desc: "退出 TUI",
        keywords: &["退出", "exit", "q"],
        args_spec: ArgsSpec::None,
        args_hint: None,
        handler: &QUIT,
    },
];

// ── 辅助函数（effort 解析/切换/标签）───────────────────────────

fn parse_effort_command(args: &str) -> Result<ReasoningEffort, String> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return Err("未提供 effort 级别".into());
    }
    ReasoningEffort::from_config_str(trimmed)
        .ok_or_else(|| format!("未知 effort 级别: '{trimmed}'"))
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
        let mut app = AppState::default();
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
        let mut app = AppState::default();
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
        let texts: Vec<&str> = app.echo.iter().map(|l| l.text.as_str()).collect();
        assert!(texts.iter().any(|t| t.contains("/fold")));
        assert!(texts.iter().any(|t| t.contains("/copy")));
        assert!(texts.iter().any(|t| t.contains("Ctrl+K")));
    }

    #[tokio::test]
    async fn unknown_command_echoes_error() {
        // 未知命令由事件循环回显错误；注册表层面验证找不到。
        assert!(CommandRegistry::find("wat").is_none());
    }

    #[tokio::test]
    async fn model_without_factory_degrades() {
        let caps = empty_caps();
        let mut app = AppState::default();
        run_cmd("model", "switch deepseek-v4-pro", &mut app, &caps).await;
        assert!(app
            .notice
            .as_ref()
            .is_some_and(|(t, _)| t.contains("模型切换不可用")));
    }
}

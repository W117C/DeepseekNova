//! 事件循环：输入 reader + 事件合并 + 批量重绘。
//!
//! 按键经 [`AppState::handle_key`] 分派；命令（斜杠与 `/` 面板）经注册表执行，
//! 注入能力从 [`crate::commands::TuiCaps`] 读取；runner 事件流转发到
//! [`crate::model::apply::ConversationApply`]。

pub mod actions;
pub mod focus;
pub mod keybindings;
pub mod state;

use crossterm::event::{self, Event as CEvent, KeyEventKind, MouseEventKind};
use deepseeknova_core::runner::{RunEvent, RunInput};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_stream::StreamExt;

use crate::commands::TuiCaps;
use crate::commands::{CommandCtx, CommandOutcome, CommandRegistry};
use crate::i18n::Key;
use crate::model::conversation::LineKind;
use state::{AppState, KeyAction};

/// ctx 计量的有效窗口：`min(context_window, budget_window)`——预算才是真实
/// 压力点，窗口配置过大（如 1M）时进度条不至于永远接近 0%。预算缺省或为 0
/// 时回退窗口本身。
fn effective_ctx_window(context_window: Option<u32>, budget_window: Option<u32>) -> Option<u32> {
    context_window.map(|w| match budget_window {
        Some(b) if b > 0 => w.min(b),
        _ => w,
    })
}

/// 运行代际：每轮提交/取消递增，旧回合残留事件按 gen 丢弃。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RunSession {
    gen: u64,
}

impl RunSession {
    fn begin(&mut self) -> u64 {
        self.gen += 1;
        self.gen
    }

    fn cancel(&mut self) {
        self.gen += 1;
    }

    fn finish(&mut self, gen: u64) -> bool {
        gen == self.gen
    }

    fn accepts(&self, gen: u64) -> bool {
        gen == self.gen
    }
}

/// 事件循环内部事件。
enum AppEvent {
    Input(CEvent),
    Runner {
        gen: u64,
        ev: RunEvent,
    },
    /// 运行器错误（流错误/启动失败/未注入），渲染为 System::Error 段。
    Error {
        gen: u64,
        text: String,
    },
    Done {
        gen: u64,
    },
    /// 侧边栏会话列表刷新结果（SessionController 异步拉取）。
    Sessions {
        metas: Vec<state::SessionMeta>,
        current: Option<String>,
    },
    /// 会话列表拉取失败/完成：只复位刷新中标记，不清空旧列表。
    SessionsRefreshDone,
    /// 侧边栏 MCP 探测结果（异步探测完成，缓存进 AppState 供 Mcp 面板展示）。
    McpProbe(Vec<state::McpStatus>),
}

/// 主事件循环：阻塞直到退出。返回 `true` 表示正常退出（命令 `/quit` 或 Esc）。
pub async fn run_loop(
    terminal: &mut DefaultTerminal,
    app: &mut AppState,
    caps: &mut TuiCaps,
) -> Result<bool, deepseeknova_core::DeepseeknovaError> {
    let (tx, mut rx) = mpsc::channel::<AppEvent>(256);
    // 审批通道：从 caps 取出由本循环独占消费（agent 侧 responder 持有发送端）。
    let mut approval_rx = caps.approval_rx.take();

    // Spawn input reader（阻塞线程，crossterm 事件源）。
    let input_tx = tx.clone();
    tokio::task::spawn_blocking(move || loop {
        if input_tx.is_closed() {
            break;
        }
        match event::poll(std::time::Duration::from_millis(100)) {
            Ok(true) => {
                if let Ok(event) = event::read() {
                    if input_tx.blocking_send(AppEvent::Input(event)).is_err() {
                        break;
                    }
                }
            }
            Ok(false) => {}
            Err(_) => break,
        }
    });

    let mut current_run: Option<JoinHandle<()>> = None;
    let mut session = RunSession::default();
    let mut last_keymap_check = std::time::Instant::now();
    let mut sessions_refreshing = false;
    let mut last_sessions_refresh = std::time::Instant::now() - std::time::Duration::from_secs(10);
    // 鼠标捕获初始态：与 AppState 当前值保持一致（生产路径 lib.rs run()
    // 启动时已 EnableMouseCapture 并注入 true）。
    let mut last_mouse_capture = app.mouse_capture;

    loop {
        // 临时命令反馈超时自动清除（不进入对话面板永久 echo）。
        if app.notice_expired() {
            app.notice = None;
        }
        // Ctrl+T 切换鼠标捕获：状态变化时同步终端模式（滚轮滚动对话 vs
        // 鼠标选中复制文本）。
        if app.mouse_capture != last_mouse_capture {
            last_mouse_capture = app.mouse_capture;
            if app.mouse_capture {
                let _ =
                    crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture);
            } else {
                let _ =
                    crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
            }
        }
        // 状态栏常驻成本与上下文占用：成本仍取 router ledger 会话累计值；
        // 上下文占用取最近一次请求的实际占用（usage.prompt_tokens 即本次
        // 实际发送的输入，含历史重发与 cache hit；completion 含推理输出）。
        // 与 Claude Code 等工具口径一致：显示当前窗口放了多少，而不是会话
        // 累计消耗——累计值只增不减，聊几句就"爆"，且 compaction 后不回落。
        // 分母取 min(context_window, budget_window)：预算才是真实压力点，
        // 避免窗口配置过大（如 1M）时进度条永远接近 0% 失去参考价值。
        if let Some(r) = &caps
            .runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .router
        {
            let report = r.ledger().report(&r.price_table());
            app.total_cost_usd = report.total_usd;
            app.context_usage = app.usage.as_ref().and_then(|u| {
                effective_ctx_window(caps.context_window, caps.budget_window).map(|window| {
                    (
                        u64::from(u.prompt_tokens) + u64::from(u.completion_tokens),
                        u64::from(window),
                    )
                })
            });
        }
        // 权限模式显示态：每帧从共享 gate 刷新（Ctrl+P 后立即反映）。
        if let Some(gate) = caps
            .runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .permission
            .clone()
        {
            app.permission_mode = gate.mode();
        }
        // 当前 thinking effort：每帧从 runtime 刷新（/model 热切换后立即反映）。
        app.effort_label = caps
            .runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .current_effort
            .effort_str()
            .unwrap_or_default()
            .to_string();
        terminal.draw(|f| app.draw(f))?;

        // 消费 `/` 面板待执行命令（真实 caps）。
        if let Some((name, args)) = app.pending_command.take() {
            if let Some(cmd) = CommandRegistry::find(&name) {
                let mut ctx = CommandCtx { app, caps };
                if cmd.handler.run(&mut ctx, &args).await == CommandOutcome::Quit {
                    return Ok(true);
                }
            }
        }

        // 消费 Ctrl+L 清屏重绘请求（terminal.clear 后下一帧重画）。
        if app.redraw_requested {
            app.redraw_requested = false;
            let _ = terminal.clear();
        }

        // 消费 Ctrl+P 权限模式循环（真实 gate：set_mode + 反馈）。
        if app.perm_mode_cycle {
            app.perm_mode_cycle = false;
            let gate = caps
                .runtime
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .permission
                .clone();
            match gate {
                Some(g) => {
                    let next = crate::app::state::next_permission_mode(g.mode());
                    g.set_mode(Some(next));
                    app.permission_mode = Some(next);
                    app.show_notice(
                        app.tr.t_args(
                            crate::i18n::Key::PermModeNotice,
                            &[(
                                "mode",
                                app.tr
                                    .t(crate::app::state::permission_mode_label(Some(next))),
                            )],
                        ),
                    );
                }
                None => app.show_notice(app.tr.t(crate::i18n::Key::PermModeGateUnavailable)),
            }
        }

        // 消费信任确认结果：y → TrustController 落盘 + gate.set_trusted(true)；
        // n/Esc → 保持 untrusted（项目 allow 规则继续降级）。
        if let Some(trust) = app.trust_decision.take() {
            let (ctrl, root, gate) = {
                let g = caps.runtime.lock().unwrap_or_else(|e| e.into_inner());
                (
                    caps.trust.clone(),
                    caps.workspace_root.clone(),
                    g.permission.clone(),
                )
            };
            if trust {
                let root = root.clone().unwrap_or_default();
                if let Some(ctrl) = &ctrl {
                    let _ = ctrl.trust(&root);
                }
                if let Some(gate) = &gate {
                    gate.set_trusted(true);
                }
                app.show_notice(app.tr.t(crate::i18n::Key::TrustAccepted));
            } else {
                if let Some(gate) = &gate {
                    gate.set_trusted(false);
                }
                app.show_notice(app.tr.t(crate::i18n::Key::TrustRejected));
            }
        }

        // keybindings.json 热重载：500ms 轮询 mtime（Claude Code 同款
        // 稳定阈值），改文件即时生效并回显诊断。
        if last_keymap_check.elapsed() >= std::time::Duration::from_millis(500) {
            last_keymap_check = std::time::Instant::now();
            let mtime = std::fs::metadata(&app.keymap_path)
                .ok()
                .and_then(|m| m.modified().ok());
            if mtime != app.keymap_mtime && mtime.is_some() {
                let reloaded = crate::app::keybindings::Keymap::load(&app.keymap_path, app.tr);
                app.keymap_mtime = mtime;
                if reloaded.diagnostics.is_empty() {
                    app.keymap = reloaded;
                    app.echo_line(
                        crate::model::conversation::LineKind::System,
                        app.tr.t(Key::KeymapReloaded),
                    );
                } else {
                    for d in &reloaded.diagnostics {
                        app.echo_line(crate::model::conversation::LineKind::Error, d);
                    }
                    // 诊断失败不替换当前生效的 keymap。
                }
            }
        }

        // 侧边栏会话列表：首次必拉（含侧边栏未开时预热）；之后侧边栏可见时
        // 每 2s 增量刷新（新会话落盘/恢复后自动出现）。
        let need_sessions_refresh = caps.session.is_some()
            && !sessions_refreshing
            && (!app.sessions_loaded
                || (app.sidebar_visible
                    && last_sessions_refresh.elapsed() >= std::time::Duration::from_secs(2)));
        if need_sessions_refresh {
            sessions_refreshing = true;
            last_sessions_refresh = std::time::Instant::now();
            let ctrl = caps.session.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let Some(ctrl) = ctrl else {
                    return;
                };
                match ctrl.list_sessions().await {
                    Ok(mut metas) => {
                        metas.sort_by_key(|m| m.id.clone());
                        metas.reverse(); // id 按时间字典序，最新优先
                        let current = ctrl.current_session().await;
                        let _ = tx.send(AppEvent::Sessions { metas, current }).await;
                    }
                    Err(_) => {
                        let _ = tx.send(AppEvent::SessionsRefreshDone).await;
                    }
                }
            });
        }

        // 侧边栏 Skills 面板：首次一次性扫描技能目录（dir 读取，轻量）。
        if !app.skills_scanned && !caps.skills_paths.is_empty() {
            app.skills_scanned = true;
            for path in &caps.skills_paths {
                let loader = deepseeknova_skills::SkillLoader::new(path);
                if let Ok(skills) = loader.load_all() {
                    app.skills
                        .extend(skills.into_iter().map(|s| state::SkillEntry {
                            name: s.name,
                            description: s.description,
                        }));
                }
            }
        }
        // 侧边栏 MCP 面板：进入即探测（首次/空缓存），之后每 30s 冷却刷新。
        // 探测异步 spawn，不阻塞事件循环（进程 spawn + 短超时）。
        let mcp_due = app.sidebar_tab == crate::app::focus::SidebarTab::Mcp
            && !app.mcp_servers.is_empty()
            && (app.mcp_statuses.is_empty()
                || app
                    .mcp_last_probe
                    .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(30)));
        if mcp_due {
            if let Some(probe) = caps.mcp_probe.clone() {
                app.mcp_last_probe = Some(std::time::Instant::now());
                let servers = app.mcp_servers.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let statuses = probe.probe(&servers).await;
                    let _ = tx.send(AppEvent::McpProbe(statuses)).await;
                });
            }
        }

        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                // 轮询 tick：无输入事件也定期检查热重载。
            }
            Some(req) = async { approval_rx.as_mut()?.recv().await }, if approval_rx.is_some() => {
                // 权限审批：agent 阻塞等待裁决，浮层显示在对话区上方。
                if app.pending_approval.is_none() {
                    app.pending_approval = Some(req);
                } else {
                    // 前一条未裁决（理论不发生：agent 单请求串行）→ 拒绝新请求。
                    let _ = req.reply.send(false);
                }
            }
            Some(event) = rx.recv() => {
                // 合并积压事件，burst 输出只重绘一次。
                let mut batch = vec![event];
                while let Ok(next) = rx.try_recv() {
                    batch.push(next);
                }
                for event in batch {
                    match event {
                        AppEvent::Input(CEvent::Key(key)) if key.kind == KeyEventKind::Press => {
                            // 焦点无关热键：Ctrl+\ 侧边栏（命令面板走纯 `/` 触发）。
                            if app.handle_modal_shortcuts(&key) {
                                continue;
                            }
                            match app.handle_key(&key) {
                                KeyAction::Quit => return Ok(true),
                                KeyAction::ExternalEditor => {
                                    // ctrl+x ctrl+e：挂起终端跑 $EDITOR，读回内容写回输入框。
                                    if let Err(e) = edit_external(app, terminal).await {
                                        app.echo_line(
                                            crate::model::conversation::LineKind::Error,
                                            &app.tr.t_args(
                                                Key::ExternalEditorFailed,
                                                &[("err", &e.to_string())],
                                            ),
                                        );
                                    }
                                }
                                KeyAction::Submit(prompt) => {
                                    // 命令交给注册表处理（/model /cost 需要 caps）。
                                    if let Some(cmd) = prompt.strip_prefix('/') {
                                        if handle_command(app, caps, cmd).await {
                                            return Ok(true);
                                        }
                                        continue;
                                    }
                                    app.running = true;
                                    app.run_started_at = Some(std::time::Instant::now());
                                    app.turn += 1;
                                    app.last_prompt = Some(prompt.clone());
                                    app.conversation.begin_turn(prompt.clone());
                                    // 新回合强制贴底：用户此前滚动查看历史
                                    // 也不影响新消息可见性。
                                    app.auto_scroll = true;
                                    let tx = tx.clone();
                                    let tr = app.tr;
                                    let runner = caps.runtime.lock().unwrap_or_else(|e| e.into_inner()).runner.clone();
                                    let gen = session.begin();
                                    current_run = Some(tokio::spawn(async move {
                                        if let Some(runner) = runner {
                                            let input = RunInput {
                                                prompt,
                                                images: vec![],
                                                model_override: None,
                                            };
                                            match runner.run_stream(input).await {
                                                Ok(mut stream) => {
                                                    while let Some(event) = stream.next().await {
                                                        let ev = match event {
                                                            Ok(e) => e,
                                                            Err(e) => {
                                                                let _ = tx
                                                                    .send(AppEvent::Error {
                                                                        gen,
                                                                        text: tr.t_args(
                                                                            Key::RunnerError,
                                                                            &[("err", &e.to_string())],
                                                                        ),
                                                                    })
                                                                    .await;
                                                                continue;
                                                            }
                                                        };
                                                        if tx
                                                            .send(AppEvent::Runner { gen, ev })
                                                            .await
                                                            .is_err()
                                                        {
                                                            break;
                                                        }
                                                    }
                                                    let _ = tx.send(AppEvent::Done { gen }).await;
                                                }
                                                Err(e) => {
                                                    let _ = tx
                                                        .send(AppEvent::Error {
                                                            gen,
                                                            text: tr.t_args(
                                                                Key::RunnerError,
                                                                &[("err", &e.to_string())],
                                                            ),
                                                        })
                                                        .await;
                                                    let _ = tx.send(AppEvent::Done { gen }).await;
                                                }
                                            }
                                        } else {
                                            let _ = tx
                                                .send(AppEvent::Error {
                                                    gen,
                                                    text: tr.t(Key::RunnerUnavailable).to_string(),
                                                })
                                                .await;
                                            let _ = tx.send(AppEvent::Done { gen }).await;
                                        }
                                    }));
                                }
                                KeyAction::Cancel => {
                                    if let Some(handle) = current_run.take() {
                                        handle.abort();
                                        session.cancel();
                                        app.conversation.mark_current_cancelled();
                                        app.running = false;
                                        app.run_started_at = None;
                                        app.echo_line(LineKind::System, app.tr.t(Key::Cancelled));
                                    }
                                }
                                KeyAction::None => {}
                            }
                        }
                        AppEvent::Input(CEvent::Paste(text)) => {
                            // bracketed paste：整段文本原样插入输入框（换行是
                            // 字面换行，不触发提交）。运行中忽略，与键入一致。
                            if !app.running && !text.is_empty() {
                                let normalized = text.replace("\r\n", "\n");
                                app.input.insert_str(&normalized);
                                app.refresh_command_hint();
                            }
                        }
                        AppEvent::Input(CEvent::Mouse(m)) => {
                            // 鼠标滚轮 = 对话历史滚动（任何焦点都生效）：
                            // 滚轮向上 → 看更早的记录（offset 减小）；
                            // 滚轮向下 → 看更新内容（offset 增大），滚到底自动恢复跟随。
                            match m.kind {
                                MouseEventKind::ScrollUp => {
                                    app.scroll_offset = app.scroll_offset.saturating_sub(3);
                                    app.auto_scroll = false;
                                }
                                MouseEventKind::ScrollDown => {
                                    app.scroll_offset = app.scroll_offset.saturating_add(3);
                                    app.auto_scroll = false;
                                }
                                _ => {}
                            }
                        }
                        AppEvent::Input(_) => {}
                        AppEvent::Sessions { metas, current } => {
                            app.saved_sessions = metas;
                            app.current_session = current;
                            app.sessions_loaded = true;
                            app.saved_session_selected = app
                                .saved_session_selected
                                .min(app.saved_sessions.len().saturating_sub(1));
                            sessions_refreshing = false;
                        }
                        AppEvent::SessionsRefreshDone => {
                            sessions_refreshing = false;
                        }
                        AppEvent::McpProbe(statuses) => {
                            app.mcp_statuses = statuses;
                        }
                        AppEvent::Runner { gen, ev } => {
                            if !session.accepts(gen) {
                                continue;
                            }
                            // 回合完成时落盘（用户 prompt + 助手输出），供 /sessions /resume。
                            if let RunEvent::Done(ref output) = ev {
                                if let Some(ctrl) = &caps.session {
                                    if let Some(prompt) = app.last_prompt.clone() {
                                        let model = caps
                                            .runtime
                                            .lock()
                                            .unwrap_or_else(|e| e.into_inner())
                                            .current_model
                                            .clone();
                                        match ctrl
                                            .record_turn(&prompt, &output.text, model)
                                            .await
                                        {
                                            Ok(()) => {}
                                            Err(e) => app.echo_line(
                                                LineKind::Error,
                                                &app.tr.t_args(
                                                    Key::SessionPersistFailed,
                                                    &[("err", &e.to_string())],
                                                ),
                                            ),
                                        }
                                    }
                                }
                            }
                            app.apply_run_event(ev);
                        }
                        AppEvent::Error { gen, text } => {
                            if session.accepts(gen) {
                                app.conversation
                                    .push_system(crate::model::conversation::SystemKind::Error, text);
                            } else {
                                // 非当前 run 的错误（已取消/已结束回合的迟到错误、
                                // 启动期探测失败）不再静默丢弃——回显到命令反馈区，
                                // 让"出错了"始终可见（曾因无当前回合被 push_system 丢掉）。
                                app.echo_line(LineKind::Error, &text);
                            }
                        }
                        AppEvent::Done { gen } => {
                            if !session.finish(gen) {
                                continue;
                            }
                            // 回合结束边界行：轮次 + 耗时，画在回合尾部（System
                            // 段），让"本轮结束"与等待/失败状态有明确分界。
                            let elapsed = app
                                .run_started_at
                                .map(|t| t.elapsed().as_secs_f32())
                                .unwrap_or(0.0);
                            let round = app.turn;
                            app.running = false;
                            app.run_started_at = None;
                            current_run = None;
                            if round > 0 {
                                app.conversation.push_system(
                                    crate::model::conversation::SystemKind::Info,
                                    app.tr.t_args(
                                        Key::TurnBoundary,
                                        &[
                                            ("n", &round.to_string()),
                                            ("secs", &format!("{elapsed:.1}")),
                                        ],
                                    ),
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

/// 处理 `/` 命令。返回 `true` 表示退出。
async fn handle_command(app: &mut AppState, caps: &TuiCaps, cmd: &str) -> bool {
    let (name, args) = cmd.split_once(char::is_whitespace).unwrap_or((cmd, ""));
    match CommandRegistry::find(name) {
        Some(command) => {
            let mut ctx = CommandCtx { app, caps };
            command.handler.run(&mut ctx, args).await == CommandOutcome::Quit
        }
        None => {
            app.show_notice(app.tr.t_args(Key::UnknownCommand, &[("cmd", name)]));
            false
        }
    }
}

/// ctrl+x ctrl+e：把当前输入写入临时文件，挂起终端运行 `$EDITOR`
/// （无 $EDITOR 时回退 vim），读回内容替换输入框。
/// 挂起/恢复用手动 escape 序列（ratatui 0.30 无 suspend API）：
/// 离开 alternate screen 让编辑器使用原始终端，回来后再进入。
async fn edit_external(
    app: &mut AppState,
    terminal: &mut ratatui::DefaultTerminal,
) -> Result<(), deepseeknova_core::DeepseeknovaError> {
    use std::io::Write;

    let mut path = std::env::temp_dir();
    path.push(format!("deepseeknova-edit-{}.md", std::process::id()));
    {
        let mut f = std::fs::File::create(&path)?;
        f.write_all(app.input.text.as_bytes())?;
    }

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".to_string());
    use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
    let mut out = std::io::stdout();
    crossterm::execute!(out, LeaveAlternateScreen)?;
    crossterm::execute!(out, crossterm::event::DisableMouseCapture)?;
    crossterm::execute!(out, crossterm::cursor::Show)?;
    let result = std::process::Command::new(&editor).arg(&path).status();
    crossterm::execute!(out, EnterAlternateScreen)?;
    crossterm::execute!(out, crossterm::event::EnableMouseCapture)?;
    crossterm::execute!(out, crossterm::cursor::Hide)?;
    terminal.clear()?;
    let status = match result {
        Ok(s) => s,
        Err(e) => {
            return Err(deepseeknova_core::DeepseeknovaError::runner(format!(
                "failed to launch editor {editor}: {e}"
            )))
        }
    };
    if !status.success() {
        return Err(deepseeknova_core::DeepseeknovaError::runner(format!(
            "editor exited with code {:?}",
            status.code()
        )));
    }

    let edited = std::fs::read_to_string(&path)?;
    app.input.set_text(edited.trim_end().to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn effective_ctx_window_takes_min_with_budget() {
        // 窗口 > 预算 → 预算生效（压力点）。
        assert_eq!(
            effective_ctx_window(Some(1_000_000), Some(128_000)),
            Some(128_000)
        );
        // 窗口 < 预算 → 窗口生效。
        assert_eq!(
            effective_ctx_window(Some(64_000), Some(128_000)),
            Some(64_000)
        );
        // 无预算 → 窗口本身。
        assert_eq!(effective_ctx_window(Some(128_000), None), Some(128_000));
        // 预算为 0（禁用语义）→ 窗口本身。
        assert_eq!(effective_ctx_window(Some(128_000), Some(0)), Some(128_000));
        // 无窗口 → None（不显示占用率）。
        assert_eq!(effective_ctx_window(None, Some(128_000)), None);
    }

    #[test]
    fn run_session_generation_isolates_events() {
        let mut session = RunSession::default();
        let g1 = session.begin();
        assert_eq!(g1, 1);
        assert!(session.accepts(g1));
        assert!(!session.accepts(0), "上一回合的 gen 不接受");
        assert!(!session.finish(0), "旧 Done 不清新回合");
        assert!(session.accepts(g1));
        assert!(session.finish(g1));

        let g2 = session.begin();
        assert_eq!(g2, 2);
        assert!(session.accepts(g2));
        session.cancel();
        assert!(!session.accepts(g2), "取消后旧 gen 事件全部丢弃");
        assert!(!session.finish(g2));

        let g3 = session.begin();
        assert_eq!(g3, 4, "cancel 也递增，begin 接着计数");
        assert!(session.accepts(g3));
    }

    #[test]
    fn unknown_command_reports_error() {
        let caps = crate::commands::TuiCaps {
            runtime: Arc::new(std::sync::Mutex::new(crate::commands::TuiRuntime::default())),
            session: None,
            skills_paths: vec![],
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
        let mut app = AppState {
            tr: crate::i18n::Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let quit = handle_command(&mut app, &caps, "wat").await;
            assert!(!quit);
            assert!(app
                .notice
                .as_ref()
                .is_some_and(|(t, _)| t.contains("未知命令")));
        });
    }
}

//! 事件循环：输入 reader + 事件合并 + 批量重绘。
//!
//! 按键经 [`AppState::handle_key`] 分派；命令（斜杠与 Ctrl+K 面板）经注册表执行，
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
use crate::model::conversation::LineKind;
use state::{AppState, KeyAction};

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
        ids: Vec<String>,
        current: Option<String>,
    },
    /// 会话列表拉取失败/完成：只复位刷新中标记，不清空旧列表。
    SessionsRefreshDone,
}

/// 主事件循环：阻塞直到退出。返回 `true` 表示正常退出（命令 `/quit` 或 Esc）。
pub async fn run_loop(
    terminal: &mut DefaultTerminal,
    app: &mut AppState,
    caps: &mut TuiCaps,
) -> anyhow::Result<bool> {
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
        if let Some(r) = &caps.runtime.lock().unwrap().router {
            let report = r.ledger().report(&r.price_table());
            app.total_cost_usd = report.total_usd;
            app.context_usage = app.usage.as_ref().and_then(|u| {
                caps.context_window.map(|w| {
                    (
                        u64::from(u.prompt_tokens) + u64::from(u.completion_tokens),
                        w as u64,
                    )
                })
            });
        }
        terminal.draw(|f| app.draw(f))?;

        // 消费 Ctrl+K 面板待执行命令（真实 caps）。
        if let Some((name, args)) = app.pending_command.take() {
            if let Some(cmd) = CommandRegistry::find(&name) {
                let mut ctx = CommandCtx { app, caps };
                if cmd.handler.run(&mut ctx, &args).await == CommandOutcome::Quit {
                    return Ok(true);
                }
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
                let reloaded = crate::app::keybindings::Keymap::load(&app.keymap_path);
                app.keymap_mtime = mtime;
                if reloaded.diagnostics.is_empty() {
                    app.keymap = reloaded;
                    app.echo_line(
                        crate::model::conversation::LineKind::System,
                        "键位配置已热重载",
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
                    Ok(mut ids) => {
                        ids.sort();
                        ids.reverse();
                        let current = ctrl.current_session().await;
                        let _ = tx.send(AppEvent::Sessions { ids, current }).await;
                    }
                    Err(_) => {
                        let _ = tx.send(AppEvent::SessionsRefreshDone).await;
                    }
                }
            });
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
                            // 焦点无关热键：Ctrl+K 命令面板 / Ctrl+\ 侧边栏。
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
                                            &format!("外部编辑器失败: {e}"),
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
                                    let runner = caps.runtime.lock().unwrap().runner.clone();
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
                                                                        text: format!("❌ {e}"),
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
                                                            text: format!("❌ {e}"),
                                                        })
                                                        .await;
                                                    let _ = tx.send(AppEvent::Done { gen }).await;
                                                }
                                            }
                                        } else {
                                            let _ = tx
                                                .send(AppEvent::Error {
                                                    gen,
                                                    text: "❌ runner 不可用（未注入）".into(),
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
                                        app.echo_line(LineKind::System, "已取消（Ctrl+C / Esc）");
                                    }
                                }
                                KeyAction::None => {}
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
                        AppEvent::Sessions { ids, current } => {
                            app.saved_sessions = ids;
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
                                            .unwrap()
                                            .current_model
                                            .clone();
                                        match ctrl
                                            .record_turn(&prompt, &output.text, model)
                                            .await
                                        {
                                            Ok(()) => {}
                                            Err(e) => app.echo_line(
                                                LineKind::Error,
                                                &format!("会话落盘失败: {e}"),
                                            ),
                                        }
                                    }
                                }
                            }
                            app.apply_run_event(ev);
                        }
                        AppEvent::Error { gen, text } => {
                            if !session.accepts(gen) {
                                continue;
                            }
                            app.conversation
                                .push_system(crate::model::conversation::SystemKind::Error, text);
                        }
                        AppEvent::Done { gen } => {
                            if !session.finish(gen) {
                                continue;
                            }
                            app.running = false;
                            app.run_started_at = None;
                            current_run = None;
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
            app.show_notice(format!("未知命令: /{name}（/help 查看）"));
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
) -> anyhow::Result<()> {
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
        Err(e) => anyhow::bail!("无法启动编辑器 {editor}: {e}"),
    };
    if !status.success() {
        anyhow::bail!("编辑器退出码 {:?}", status.code());
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
            context_window: None,
            approval_rx: None,
        };
        let mut app = AppState::default();
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

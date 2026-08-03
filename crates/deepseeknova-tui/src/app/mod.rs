//! 事件循环：输入 reader + 事件合并 + 批量重绘。
//!
//! 按键经 [`AppState::handle_key`] 分派；命令（斜杠与 Ctrl+K 面板）经注册表执行，
//! 注入能力从 [`crate::commands::TuiCaps`] 读取；runner 事件流转发到
//! [`crate::model::apply::ConversationApply`]。

pub mod focus;
pub mod state;

use crossterm::event::{self, Event as CEvent, KeyEventKind};
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
}

/// 主事件循环：阻塞直到退出。返回 `true` 表示正常退出（命令 `/quit` 或 Esc）。
pub async fn run_loop(
    terminal: &mut DefaultTerminal,
    app: &mut AppState,
    caps: &TuiCaps,
) -> anyhow::Result<bool> {
    let (tx, mut rx) = mpsc::channel::<AppEvent>(256);

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

    loop {
        // 状态栏常驻成本与上下文占用：每帧从 router ledger 取会话累计值。
        if let Some(r) = &caps.runtime.lock().unwrap().router {
            let report = r.ledger().report(&r.price_table());
            app.total_cost_usd = report.total_usd;
            // 上下文占用分子 = 会话累计 prompt + completion（含 cache 输入，
            // 输出也占窗口）；分母来自 config 注入。compaction 后 ledger 不减，
            // 占用率偏保守（显示偏高），属已知口径。
            let used: u64 = report
                .rows
                .iter()
                .map(|row| row.bucket.prompt_tokens + row.bucket.completion_tokens)
                .sum();
            app.context_usage = caps.context_window.map(|w| (used, w as u64));
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

        tokio::select! {
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
                                KeyAction::Submit(prompt) => {
                                    // 命令交给注册表处理（/model /cost 需要 caps）。
                                    if let Some(cmd) = prompt.strip_prefix('/') {
                                        if handle_command(app, caps, cmd).await {
                                            return Ok(true);
                                        }
                                        continue;
                                    }
                                    app.running = true;
                                    app.turn += 1;
                                    app.last_prompt = Some(prompt.clone());
                                    app.conversation.begin_turn(prompt.clone());
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
                                        app.echo_line(LineKind::System, "已取消（Ctrl+C）");
                                    }
                                }
                                KeyAction::None => {}
                            }
                        }
                        AppEvent::Input(_) => {}
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
            app.echo_line(LineKind::Error, &format!("未知命令: /{name}（/help 查看）"));
            false
        }
    }
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
        };
        let mut app = AppState::default();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let quit = handle_command(&mut app, &caps, "wat").await;
            assert!(!quit);
            assert!(app.echo.iter().any(|l| l.text.contains("未知命令")));
        });
    }
}

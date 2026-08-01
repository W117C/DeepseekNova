//! ratatui-based interactive terminal UI for deepseeknova.
//!
//! Wraps a [`Runner`] and displays its full event stream in a scrolling
//! conversation pane:
//!
//! - streaming text / reasoning (dimmed)
//! - tool calls and truncated results
//! - deterministic verification (`✓` / `✗`)
//! - pauses, errors, approval requests
//! - status bar with model, phase, token usage and scrollback position
//! - input history, scrollback, `/help` `/clear` `/quit`, Ctrl+C cancel
//!
//! ```no_run
//! use deepseeknova_tui::TuiRunner;
//! # use std::sync::Arc;
//! # struct DummyRunner;
//! # #[async_trait::async_trait]
//! # impl deepseeknova_core::runner::Runner for DummyRunner {
//! #     async fn run_stream(&self, _input: deepseeknova_core::runner::RunInput) -> anyhow::Result<deepseeknova_core::runner::RunEventStream> {
//! #         unreachable!()
//! #     }
//! # }
//! # #[tokio::main]
//! # async fn main() -> anyhow::Result<()> {
//! # let runner = Arc::new(DummyRunner);
//! TuiRunner::new(runner).run().await?;
//! # Ok(())
//! # }
//! ```

use crossterm::event::{self, Event as CEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use deepseeknova_core::chunk::Usage;
use deepseeknova_core::runner::{RunEvent, RunInput, Runner};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_stream::StreamExt;

/// 滚动回看上限（行数），防止长会话无界增长。
const MAX_LINES: usize = 2000;
/// 工具参数单行预览上限（字符）。
const ARGS_PREVIEW: usize = 200;
/// 工具结果单行预览上限（字符）。
const RESULT_PREVIEW: usize = 400;

// ── TuiRunner ──────────────────────────────────────────────────

/// A [`Runner`] wrapper that drives an interactive split-pane terminal UI.
pub struct TuiRunner {
    runner: Arc<dyn Runner>,
    model_label: String,
}

impl TuiRunner {
    /// Wrap `runner` for display in the TUI.
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self {
            runner,
            model_label: "default".to_string(),
        }
    }

    /// 状态栏显示的模型标签（CLI 传入实际模型名）。
    pub fn with_model_label(mut self, label: impl Into<String>) -> Self {
        self.model_label = label.into();
        self
    }

    /// Enter the TUI and block until the user quits.
    pub async fn run(&self) -> anyhow::Result<()> {
        let mut terminal = ratatui::init();
        let result = self.run_inner(&mut terminal).await;
        ratatui::restore();
        result
    }

    async fn run_inner(&self, terminal: &mut DefaultTerminal) -> anyhow::Result<()> {
        let (tx, mut rx) = mpsc::channel::<AppEvent>(256);

        // Spawn input reader（阻塞线程，crossterm 事件源）。
        let input_tx = tx.clone();
        tokio::task::spawn_blocking(move || {
            while let Ok(event) = event::read() {
                if input_tx.blocking_send(AppEvent::Input(event)).is_err() {
                    break;
                }
            }
        });

        let mut app = AppState {
            model_label: self.model_label.clone(),
            ..Default::default()
        };
        let mut current_run: Option<JoinHandle<()>> = None;

        loop {
            terminal.draw(|f| app.draw(f))?;

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
                                match app.handle_key(&key) {
                                    KeyAction::Quit => return Ok(()),
                                    KeyAction::Submit(prompt) => {
                                        app.running = true;
                                        app.turn += 1;
                                        app.push_line(LineKind::User, &prompt);
                                        let tx = tx.clone();
                                        let runner = self.runner.clone();
                                        current_run = Some(tokio::spawn(async move {
                                            let input = RunInput {
                                                prompt,
                                                images: vec![],
                                                model_override: None,
                                            };
                                            match runner.run_stream(input).await {
                                                Ok(mut stream) => {
                                                    while let Some(event) = stream.next().await {
                                                        let ev = match event {
                                                            Ok(e) => AppEvent::Runner(e),
                                                            Err(e) => AppEvent::Runner(RunEvent::TextDelta(
                                                                format!("\n❌ {e}")
                                                            )),
                                                        };
                                                        if tx.send(ev).await.is_err() {
                                                            break;
                                                        }
                                                    }
                                                    let _ = tx.send(AppEvent::Done).await;
                                                }
                                                Err(e) => {
                                                    let _ = tx.send(AppEvent::Runner(
                                                        RunEvent::TextDelta(format!("\n❌ {e}"))
                                                    )).await;
                                                    let _ = tx.send(AppEvent::Done).await;
                                                }
                                            }
                                        }));
                                    }
                                    KeyAction::Cancel => {
                                        if let Some(handle) = current_run.take() {
                                            handle.abort();
                                            app.running = false;
                                            app.push_line(LineKind::System, "已取消（Ctrl+C）");
                                        }
                                    }
                                    KeyAction::None => {}
                                }
                            }
                            AppEvent::Input(_) => {}
                            AppEvent::Runner(ev) => app.apply_run_event(ev),
                            AppEvent::Done => {
                                app.flush_all();
                                app.running = false;
                                current_run = None;
                            }
                        }
                    }
                }
            }
        }
    }
}

// ── App state ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    User,
    Agent,
    Reasoning,
    Tool,
    ToolResult,
    Verification { passed: bool },
    System,
    Error,
    Paused,
}

#[derive(Debug, Clone)]
struct UiLine {
    kind: LineKind,
    text: String,
}

/// 回车后的处理结果。
enum KeyAction {
    Quit,
    Submit(String),
    Cancel,
    None,
}

#[derive(Default)]
struct AppState {
    lines: Vec<UiLine>,
    input: String,
    history: Vec<String>,
    history_idx: Option<usize>,
    running: bool,
    turn: usize,
    pending_text: String,
    pending_reasoning: String,
    usage: Option<Usage>,
    scroll_offset: usize,
    auto_scroll: bool,
    model_label: String,
}

impl AppState {
    fn push_line(&mut self, kind: LineKind, text: &str) {
        self.lines.push(UiLine {
            kind,
            text: text.to_string(),
        });
        if self.lines.len() > MAX_LINES {
            let overflow = self.lines.len() - MAX_LINES;
            self.lines.drain(0..overflow);
            if self.scroll_offset >= overflow {
                self.scroll_offset -= overflow;
            } else {
                self.scroll_offset = 0;
            }
        }
    }

    fn append_text(&mut self, delta: &str) {
        self.pending_text.push_str(delta);
    }

    fn append_reasoning(&mut self, delta: &str) {
        self.pending_reasoning.push_str(delta);
    }

    fn flush_text(&mut self) {
        if !self.pending_text.is_empty() {
            let text = std::mem::take(&mut self.pending_text);
            self.push_line(LineKind::Agent, &text);
        }
    }

    fn flush_reasoning(&mut self) {
        if !self.pending_reasoning.is_empty() {
            let text = std::mem::take(&mut self.pending_reasoning);
            self.push_line(LineKind::Reasoning, &text);
        }
    }

    fn flush_all(&mut self) {
        self.flush_reasoning();
        self.flush_text();
    }

    /// 单一入口消费 RunEvent（可测试）。
    fn apply_run_event(&mut self, ev: RunEvent) {
        match ev {
            RunEvent::TextDelta(text) => self.append_text(&text),
            RunEvent::ReasoningDelta { text, .. } => self.append_reasoning(&text),
            RunEvent::ToolCallStart { name, .. } => {
                self.flush_reasoning();
                self.push_line(LineKind::Tool, &format!("⚙ {name} …"));
            }
            RunEvent::ToolCallDelta { .. } => {}
            RunEvent::ToolCallEnd {
                name, arguments, ..
            } => {
                let args = truncate_str(&arguments, ARGS_PREVIEW);
                self.push_line(LineKind::Tool, &format!("⚙ {name}({args})"));
            }
            RunEvent::ToolResult { result, .. } => {
                let result = truncate_str(&result, RESULT_PREVIEW);
                self.push_line(LineKind::ToolResult, &format!("  → {result}"));
            }
            RunEvent::Verification {
                command,
                passed,
                summary,
            } => {
                let mark = if passed { "✓" } else { "✗" };
                let mut text = format!("{mark} 验证: {command}");
                if !passed && !summary.is_empty() {
                    text.push_str(&format!(" — {}", truncate_str(&summary, 160)));
                }
                self.push_line(LineKind::Verification { passed }, &text);
            }
            RunEvent::Usage(usage) => self.usage = Some(usage),
            RunEvent::TurnComplete => self.flush_all(),
            RunEvent::ApprovalRequest {
                title, description, ..
            } => {
                let desc = description
                    .as_deref()
                    .map(|d| truncate_str(d, 120))
                    .unwrap_or_default();
                let text = if desc.is_empty() {
                    format!("🔒 请求授权: {title}")
                } else {
                    format!("🔒 请求授权: {title} — {desc}")
                };
                self.push_line(LineKind::System, &text);
            }
            RunEvent::Paused { reason, .. } => {
                self.flush_all();
                self.push_line(LineKind::Paused, &format!("⏸ {reason}"));
                self.running = false;
            }
            RunEvent::Done(output) => {
                if !output.text.is_empty() {
                    self.append_text(&output.text);
                }
                self.flush_all();
                self.running = false;
            }
        }
    }

    /// 处理按键；返回是否需要退出/提交/取消。
    fn handle_key(&mut self, key: &KeyEvent) -> KeyAction {
        match key.code {
            KeyCode::Esc => {
                if self.running {
                    KeyAction::None
                } else {
                    KeyAction::Quit
                }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.running {
                    KeyAction::Cancel
                } else {
                    KeyAction::None
                }
            }
            KeyCode::Enter => {
                if self.running {
                    return KeyAction::None;
                }
                let prompt = std::mem::take(&mut self.input);
                let prompt = prompt.trim().to_string();
                if prompt.is_empty() {
                    return KeyAction::None;
                }
                if let Some(rest) = prompt.strip_prefix('/') {
                    return match self.execute_command(rest) {
                        CommandResult::Quit => KeyAction::Quit,
                        _ => KeyAction::None,
                    };
                }
                self.history.push(prompt.clone());
                self.history_idx = None;
                KeyAction::Submit(prompt)
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                KeyAction::None
            }
            KeyCode::Backspace => {
                self.input.pop();
                KeyAction::None
            }
            KeyCode::Up => {
                if !self.running {
                    self.history_prev();
                }
                KeyAction::None
            }
            KeyCode::Down => {
                if !self.running {
                    self.history_next();
                }
                KeyAction::None
            }
            KeyCode::PageUp => {
                self.scroll_offset = self.scroll_offset.saturating_add(20);
                self.auto_scroll = false;
                KeyAction::None
            }
            KeyCode::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(20);
                KeyAction::None
            }
            KeyCode::Home => {
                self.scroll_offset = 0;
                self.auto_scroll = false;
                KeyAction::None
            }
            KeyCode::End => {
                self.auto_scroll = true;
                KeyAction::None
            }
            _ => KeyAction::None,
        }
    }

    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let idx = match self.history_idx {
            Some(i) if i > 0 => i - 1,
            Some(_) => 0,
            None => self.history.len() - 1,
        };
        self.history_idx = Some(idx);
        self.input = self.history[idx].clone();
    }

    fn history_next(&mut self) {
        match self.history_idx {
            Some(i) if i + 1 < self.history.len() => {
                self.history_idx = Some(i + 1);
                self.input = self.history[i + 1].clone();
            }
            Some(_) => {
                self.history_idx = None;
                self.input.clear();
            }
            None => {}
        }
    }

    fn execute_command(&mut self, cmd: &str) -> CommandResult {
        let (name, _rest) = cmd.split_once(char::is_whitespace).unwrap_or((cmd, ""));
        match name {
            "quit" | "exit" | "q" => CommandResult::Quit,
            "clear" => {
                self.lines.clear();
                self.pending_text.clear();
                self.pending_reasoning.clear();
                self.scroll_offset = 0;
                self.auto_scroll = true;
                CommandResult::Handled
            }
            "help" | "h" => {
                self.push_line(LineKind::System, "可用命令:");
                for line in [
                    "  /help          显示帮助",
                    "  /clear         清空对话面板",
                    "  /quit          退出 TUI（Esc）",
                    "  PageUp/Down    滚动回看",
                    "  ↑/↓            输入历史",
                    "  Ctrl+C         取消当前运行",
                ] {
                    self.push_line(LineKind::System, line);
                }
                CommandResult::Handled
            }
            _ => {
                self.push_line(LineKind::Error, &format!("未知命令: /{name}（/help 查看）"));
                CommandResult::Handled
            }
        }
    }

    fn clamp_scroll(&mut self, viewport: usize) {
        let max = self.lines.len().saturating_sub(viewport);
        if self.auto_scroll {
            self.scroll_offset = max;
        } else {
            self.scroll_offset = self.scroll_offset.min(max);
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(1),
                Constraint::Length(3),
            ])
            .split(area);

        let conv_area = chunks[0];
        let viewport = conv_area.height.saturating_sub(2) as usize;
        self.clamp_scroll(viewport.max(1));

        let title = if self.running {
            "🧠 运行中…"
        } else {
            "💬 就绪"
        };
        let conv_block = Block::default().borders(Borders::ALL).title(title);

        let mut text_lines: Vec<Line> = Vec::new();
        for line in &self.lines {
            text_lines.push(Line::from(Span::styled(&line.text, style_for(line.kind))));
        }
        if !self.pending_reasoning.is_empty() {
            text_lines.push(Line::from(Span::styled(
                &self.pending_reasoning,
                style_for(LineKind::Reasoning),
            )));
        }
        if !self.pending_text.is_empty() {
            text_lines.push(Line::from(Span::styled(
                &self.pending_text,
                style_for(LineKind::Agent),
            )));
        }

        let paragraph = Paragraph::new(text_lines)
            .block(conv_block)
            .wrap(Wrap { trim: false })
            .scroll((self.scroll_offset as u16, 0));
        f.render_widget(paragraph, conv_area);

        // ── Status bar ───────────────────────────────────────
        let phase = if self.running { "running" } else { "ready" };
        let status_text = match &self.usage {
            Some(u) => format!(
                " model={} {} | turn {} | ↑{} ↓{} Σ{} 推理{} 缓存hit{} | lines {} | 滚动 {}%",
                self.model_label,
                phase,
                self.turn,
                u.prompt_tokens,
                u.completion_tokens,
                u.total_tokens,
                u.reasoning_tokens,
                u.cache_hit_tokens,
                self.lines.len(),
                if self.lines.is_empty() {
                    0
                } else {
                    self.scroll_offset * 100 / self.lines.len()
                },
            ),
            None => format!(
                " model={} {} | turn {} | lines {} | 滚动 {}%",
                self.model_label,
                phase,
                self.turn,
                self.lines.len(),
                if self.lines.is_empty() {
                    0
                } else {
                    self.scroll_offset * 100 / self.lines.len()
                },
            ),
        };
        let status = Paragraph::new(Span::styled(
            status_text,
            Style::default().fg(Color::DarkGray),
        ));
        f.render_widget(status, chunks[1]);

        // ── Input pane ───────────────────────────────────────
        let input_style = if self.running {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Green)
        };
        let input_text = if self.running {
            " (等待响应… Ctrl+C 取消) ".to_string()
        } else {
            self.input.clone()
        };
        let input_block = Block::default()
            .borders(Borders::ALL)
            .title("> prompt  (/help, Esc 退出)");
        let input_widget = Paragraph::new(Span::styled(input_text, input_style)).block(input_block);
        f.render_widget(input_widget, chunks[2]);
    }
}

#[derive(Debug, PartialEq)]
enum CommandResult {
    Quit,
    Handled,
}

fn style_for(kind: LineKind) -> Style {
    match kind {
        LineKind::User => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        LineKind::Agent => Style::default().fg(Color::White),
        LineKind::Reasoning => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
        LineKind::Tool => Style::default().fg(Color::Yellow),
        LineKind::ToolResult => Style::default().fg(Color::DarkGray),
        LineKind::Verification { passed } => {
            Style::default().fg(if passed { Color::Green } else { Color::Red })
        }
        LineKind::System => Style::default().fg(Color::Blue),
        LineKind::Error => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        LineKind::Paused => Style::default().fg(Color::Yellow),
    }
}

// ── Internal types ─────────────────────────────────────────────

enum AppEvent {
    Input(CEvent),
    Runner(RunEvent),
    Done,
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_utf8_boundary() {
        assert_eq!(truncate_str("你好世界", 4), "你…");
        assert_eq!(truncate_str("hello", 100), "hello");
        let s = "a".repeat(300);
        assert_eq!(truncate_str(&s, 200).len(), 203); // 200 bytes + "…"(3B)
    }

    #[test]
    fn run_events_render_to_lines() {
        let mut app = AppState::default();
        app.apply_run_event(RunEvent::TextDelta("hello ".into()));
        app.apply_run_event(RunEvent::ReasoningDelta {
            text: "think".into(),
            signature: None,
        });
        app.apply_run_event(RunEvent::ToolCallStart {
            id: "1".into(),
            name: "grep".into(),
        });
        app.apply_run_event(RunEvent::ToolCallEnd {
            id: "1".into(),
            name: "grep".into(),
            arguments: r#"{"pattern":"x"}"#.into(),
        });
        app.apply_run_event(RunEvent::ToolResult {
            call_id: "1".into(),
            result: "hit".into(),
        });
        app.apply_run_event(RunEvent::Verification {
            command: "cargo check".into(),
            passed: true,
            summary: "ok".into(),
        });
        app.apply_run_event(RunEvent::Verification {
            command: "cargo test".into(),
            passed: false,
            summary: "exit 1".into(),
        });
        app.apply_run_event(RunEvent::TurnComplete);
        app.apply_run_event(RunEvent::Done(RunOutputStub::output("done")));

        let texts: Vec<&str> = app.lines.iter().map(|l| l.text.as_str()).collect();
        assert!(texts.iter().any(|t| t.contains("think")), "reasoning line");
        assert!(texts.iter().any(|t| t.contains("⚙ grep")), "tool line");
        assert!(
            texts.iter().any(|t| t.starts_with("  → hit")),
            "result line"
        );
        assert!(texts.iter().any(|t| t.contains("✓ 验证: cargo check")));
        assert!(texts.iter().any(|t| t.contains("✗ 验证: cargo test")));
        assert!(texts.iter().any(|t| t.contains("done")), "final text");
        assert!(!app.running);
    }

    #[test]
    fn paused_event_marks_not_running() {
        let mut app = AppState {
            running: true,
            ..Default::default()
        };
        app.apply_run_event(RunEvent::Paused {
            reason: "max steps".into(),
            session_id: None,
        });
        assert!(!app.running);
        assert!(app.lines.iter().any(|l| l.text.contains("⏸ max steps")));
    }

    #[test]
    fn commands_quit_clear_and_report_unknown() {
        let mut app = AppState::default();
        assert!(matches!(app.execute_command("quit"), CommandResult::Quit));
        app.push_line(LineKind::User, "x");
        assert_eq!(app.execute_command("clear"), CommandResult::Handled);
        assert!(app.lines.is_empty());
        app.execute_command("wat");
        assert!(app.lines.iter().any(|l| l.text.contains("未知命令")));
    }

    #[test]
    fn input_history_navigates() {
        let mut app = AppState::default();
        app.history.push("first".into());
        app.history.push("second".into());
        app.history_prev();
        assert_eq!(app.input, "second");
        app.history_prev();
        assert_eq!(app.input, "first");
        app.history_next();
        assert_eq!(app.input, "second");
        app.history_next();
        assert_eq!(app.input, "");
    }

    #[test]
    fn scroll_clamps_and_autofollows() {
        let mut app = AppState::default();
        for i in 0..50 {
            app.push_line(LineKind::Agent, &format!("line {i}"));
        }
        app.auto_scroll = true;
        app.clamp_scroll(10);
        assert_eq!(app.scroll_offset, 40);

        app.scroll_offset = 1000;
        app.auto_scroll = false;
        app.clamp_scroll(10);
        assert_eq!(app.scroll_offset, 40);
    }

    #[test]
    fn scrollback_is_capped() {
        let mut app = AppState::default();
        for i in 0..(MAX_LINES + 100) {
            app.push_line(LineKind::System, &format!("x{i}"));
        }
        assert_eq!(app.lines.len(), MAX_LINES);
    }

    // RunOutput 构造辅助（避免暴露内部类型）。
    struct RunOutputStub;
    impl RunOutputStub {
        fn output(text: &str) -> deepseeknova_core::runner::RunOutput {
            deepseeknova_core::runner::RunOutput {
                text: text.to_string(),
                tool_calls: vec![],
                usage: None,
            }
        }
    }
}

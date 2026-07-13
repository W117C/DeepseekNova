//! ratatui-based interactive terminal UI for dpronix.
//!
//! Wraps a [`Runner`] and displays streaming output in a split-pane TUI:
//! - **Conversation pane** (top) — scrollable, shows agent text + tool calls.
//! - **Status bar** — current model, token usage.
//! - **Input pane** (bottom) — user prompt entry.
//!
//! ```no_run
//! use dpronix_tui::TuiRunner;
//! # use std::sync::Arc;
//! # struct DummyRunner;
//! # #[async_trait::async_trait]
//! # impl dpronix_core::runner::Runner for DummyRunner {
//! #     async fn run_stream(&self, _input: dpronix_core::runner::RunInput) -> anyhow::Result<dpronix_core::runner::RunEventStream> {
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

use crossterm::event::{self, Event as CEvent, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use dpronix_core::runner::{RunEvent, RunInput, Runner};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;

// ── TuiRunner ──────────────────────────────────────────────────

pub struct TuiRunner {
    runner: Arc<dyn Runner>,
}

impl TuiRunner {
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self { runner }
    }

    /// Enter the TUI and block until the user quits.
    pub async fn run(&self) -> anyhow::Result<()> {
        let mut terminal = ratatui::init();
        let result = self.run_inner(&mut terminal).await;
        ratatui::restore();
        result
    }

    async fn run_inner(&self, terminal: &mut DefaultTerminal) -> anyhow::Result<()> {
        let (tx, mut rx) = mpsc::channel::<AppEvent>(64);

        // Spawn input reader
        let input_tx = tx.clone();
        tokio::task::spawn_blocking(move || {
            while let Ok(event) = event::read() {
                if input_tx.blocking_send(AppEvent::Input(event)).is_err() {
                    break;
                }
            }
        });

        let mut app = AppState::default();

        loop {
            terminal.draw(|f| app.draw(f))?;

            tokio::select! {
                Some(event) = rx.recv() => {
                    match event {
                        AppEvent::Input(CEvent::Key(key)) if key.kind == KeyEventKind::Press => {
                            match key.code {
                                KeyCode::Esc | KeyCode::Char('q') if !app.running => {
                                    // Quit on Esc/q when idle (ignore during runner)
                                    return Ok(());
                                }
                                KeyCode::Enter => {
                                    if app.running {
                                        continue; // ignore — already processing
                                    }
                                    let prompt = std::mem::take(&mut app.input);
                                    if !prompt.trim().is_empty() {
                                        app.running = true;
                                        app.add_line(LineType::User, &prompt);
                                        let tx = tx.clone();
                                        let runner = self.runner.clone();
                                        tokio::spawn(async move {
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
                                        });
                                    }
                                }
                                KeyCode::Char(c) => {
                                    app.input.push(c);
                                }
                                KeyCode::Backspace => {
                                    app.input.pop();
                                }
                                _ => {}
                            }
                        }
                        AppEvent::Input(_) => {} // ignore non-key events
                        AppEvent::Runner(RunEvent::TextDelta(text)) => {
                            // Accumulate text deltas and append as a line when TurnComplete
                            app.append_text(&text);
                        }
                        AppEvent::Runner(RunEvent::ToolCallStart { name, .. }) => {
                            app.add_line(LineType::Tool, &format!("⚙ {name} ..."));
                        }
                        AppEvent::Runner(RunEvent::ToolCallEnd { name, arguments, .. }) => {
                            app.add_line(LineType::Tool, &format!("⚙ {name}({arguments})"));
                        }
                        AppEvent::Runner(RunEvent::ToolResult { result, .. }) => {
                            // Truncate long results
                            let truncated = truncate_str(&result, 300);
                            app.add_line(LineType::ToolResult, &format!("  → {truncated}"));
                        }
                        AppEvent::Runner(RunEvent::Usage(u)) => {
                            app.last_usage = Some(u);
                        }
                        AppEvent::Runner(RunEvent::Done(output)) => {
                            if !output.text.is_empty() {
                                app.append_text(&output.text);
                            }
                            app.flush_text();
                            app.running = false;
                        }
                        AppEvent::Runner(RunEvent::TurnComplete) => {
                            app.flush_text();
                        }
                        AppEvent::Done => {
                            app.flush_text();
                            app.running = false;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

// ── App State ─────────────────────────────────────────────────

#[derive(Default)]
struct AppState {
    lines: Vec<UiLine>,
    input: String,
    running: bool,
    pending_text: String,
    last_usage: Option<dpronix_core::chunk::Usage>,
}

struct UiLine {
    kind: LineType,
    text: String,
}

enum LineType {
    User,
    Agent,
    Tool,
    ToolResult,
}

impl AppState {
    fn add_line(&mut self, kind: LineType, text: &str) {
        self.lines.push(UiLine {
            kind,
            text: text.to_string(),
        });
    }

    fn append_text(&mut self, delta: &str) {
        self.pending_text.push_str(delta);
    }

    fn flush_text(&mut self) {
        if !self.pending_text.is_empty() {
            let text = std::mem::take(&mut self.pending_text);
            self.add_line(LineType::Agent, &text);
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        let area = f.area();

        // Layout: main area | status bar | input
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(1),
                Constraint::Length(3),
            ])
            .split(area);

        // ── Conversation pane ────────────────────────────────
        let title = if self.running { "🧠 thinking…" } else { "💬 ready" };
        let conv_block = Block::default()
            .borders(Borders::ALL)
            .title(title);

        let mut text_lines: Vec<Line> = Vec::new();
        for line in &self.lines {
            let style = match line.kind {
                LineType::User => Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                LineType::Agent => Style::default().fg(Color::White),
                LineType::Tool => Style::default().fg(Color::Yellow),
                LineType::ToolResult => Style::default().fg(Color::DarkGray),
            };
            text_lines.push(Line::from(Span::styled(&line.text, style)));
        }

        // Show pending text while streaming
        if !self.pending_text.is_empty() {
            text_lines.push(Line::from(Span::styled(
                &self.pending_text,
                Style::default().fg(Color::White),
            )));
        }

        let paragraph = Paragraph::new(Text::from(text_lines))
            .block(conv_block)
            .wrap(Wrap { trim: false });
        f.render_widget(paragraph, chunks[0]);

        // ── Status bar ───────────────────────────────────────
        let status_text = if let Some(ref u) = self.last_usage {
            format!(
                " ↑{} ↓{} total:{} | {} lines ",
                u.prompt_tokens,
                u.completion_tokens,
                u.total_tokens,
                self.lines.len(),
            )
        } else {
            format!(" {} lines ", self.lines.len())
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
            " (waiting for response…) "
        } else {
            &self.input
        };
        let input_block = Block::default()
            .borders(Borders::ALL)
            .title("> prompt  (Esc to quit)");
        let input_widget = Paragraph::new(Span::styled(input_text, input_style))
            .block(input_block);
        f.render_widget(input_widget, chunks[2]);
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
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

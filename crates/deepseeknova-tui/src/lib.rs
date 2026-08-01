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

use async_trait::async_trait;
use crossterm::event::{self, Event as CEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use deepseeknova_core::chunk::Usage;
use deepseeknova_core::runner::{RunEvent, RunInput, Runner};
use deepseeknova_provider::cost::ModelRole;
use deepseeknova_provider::factory::ReasoningEffort;
use deepseeknova_provider::router::ModelRouter;
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
    /// agent 重建工厂：`(effort, model)` → 新 runner（用于 `/model` 热切换）。
    factory: Option<AgentFactory>,
    /// 可选 ModelRouter：启用 `/model use` 角色指针与 `/cost`。
    router: Option<Arc<ModelRouter>>,
    baseline_effort: ReasoningEffort,
    current_effort: ReasoningEffort,
    current_model: Option<String>,
    /// 可选会话控制器：启用 `/new` `/sessions` `/resume` 与回合落盘。
    session: Option<Arc<dyn SessionController>>,
}

/// agent 重建工厂类型。
type AgentFactory = Arc<
    dyn Fn(Option<ReasoningEffort>, Option<String>) -> anyhow::Result<Arc<dyn Runner>>
        + Send
        + Sync,
>;

/// 会话管理控制器（由 CLI 用 ChatPersistence 实现，TUI 不依赖 CLI 类型）。
#[async_trait]
pub trait SessionController: Send + Sync {
    /// 开始新会话：清空共享历史并更换 session id。
    async fn new_session(&self) -> anyhow::Result<()>;
    /// 列出已保存会话 id。
    async fn list_sessions(&self) -> anyhow::Result<Vec<String>>;
    /// 当前会话 id。
    async fn current_session(&self) -> Option<String>;
    /// 恢复指定会话到共享历史，返回恢复的消息数。
    async fn resume(&self, id: &str) -> anyhow::Result<usize>;
    /// 落盘一个已完成回合（用户 prompt + 助手输出）。
    async fn record_turn(
        &self,
        prompt: &str,
        output_text: &str,
        model: Option<String>,
    ) -> anyhow::Result<()>;
}

impl TuiRunner {
    /// Wrap `runner` for display in the TUI.
    pub fn new(runner: Arc<dyn Runner>) -> Self {
        Self {
            runner,
            model_label: "default".to_string(),
            factory: None,
            router: None,
            baseline_effort: ReasoningEffort::High,
            current_effort: ReasoningEffort::High,
            current_model: None,
            session: None,
        }
    }

    /// 状态栏显示的模型标签（CLI 传入实际模型名）。
    pub fn with_model_label(mut self, label: impl Into<String>) -> Self {
        self.model_label = label.into();
        self
    }

    /// 提供 agent 重建工厂（与 chat REPL 相同的签名），启用 `/model` 热切换。
    pub fn with_agent_factory<F>(mut self, factory: F) -> Self
    where
        F: Fn(Option<ReasoningEffort>, Option<String>) -> anyhow::Result<Arc<dyn Runner>>
            + Send
            + Sync
            + 'static,
    {
        self.factory = Some(Arc::new(factory));
        self
    }

    /// 提供 ModelRouter，启用 `/model use` 与 `/cost`。
    pub fn with_model_router(mut self, router: Arc<ModelRouter>) -> Self {
        self.router = Some(router);
        self
    }

    /// 配置基线 reasoning effort（`/model thinking` 恢复目标）。
    pub fn with_baseline_effort(mut self, effort: ReasoningEffort) -> Self {
        self.baseline_effort = effort;
        self.current_effort = effort;
        self
    }

    /// 当前模型名（`/model switch` 后自动更新）。
    pub fn with_current_model(mut self, model: Option<String>) -> Self {
        self.current_model = model;
        self
    }

    /// 提供会话控制器，启用 `/new` `/sessions` `/resume` 与回合落盘。
    pub fn with_session_controller(mut self, controller: Arc<dyn SessionController>) -> Self {
        self.session = Some(controller);
        self
    }

    /// Enter the TUI and block until the user quits.
    pub async fn run(&mut self) -> anyhow::Result<()> {
        let mut terminal = ratatui::init();
        let result = self.run_inner(&mut terminal).await;
        ratatui::restore();
        result
    }

    async fn run_inner(&mut self, terminal: &mut DefaultTerminal) -> anyhow::Result<()> {
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
                                        // 命令交给 TUI 层处理（/model /cost 需要工厂与 router）。
                                        if let Some(cmd) = prompt.strip_prefix('/') {
                                            if self.handle_command(&mut app, cmd).await {
                                                return Ok(());
                                            }
                                            continue;
                                        }
                                        app.running = true;
                                        app.turn += 1;
                                        app.last_prompt = Some(prompt.clone());
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
                            AppEvent::Runner(ev) => {
                                // 回合完成时落盘（用户 prompt + 助手输出），供 /sessions /resume。
                                if let RunEvent::Done(ref output) = ev {
                                    if let Some(ctrl) = &self.session {
                                        if let Some(prompt) = app.last_prompt.clone() {
                                            match ctrl
                                                .record_turn(
                                                    &prompt,
                                                    &output.text,
                                                    self.current_model.clone(),
                                                )
                                                .await
                                            {
                                                Ok(()) => {}
                                                Err(e) => app.push_line(
                                                    LineKind::Error,
                                                    &format!("会话落盘失败: {e}"),
                                                ),
                                            }
                                        }
                                    }
                                }
                                app.apply_run_event(ev);
                            }
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

impl TuiRunner {
    /// 处理以 `/` 开头的命令。返回 `true` 表示退出。
    /// 处理以 `/` 开头的命令。返回 `true` 表示退出。
    async fn handle_command(&mut self, app: &mut AppState, cmd: &str) -> bool {
        let (name, rest) = cmd.split_once(char::is_whitespace).unwrap_or((cmd, ""));
        match name {
            "model" => {
                self.handle_model(app, rest);
                false
            }
            "cost" => {
                self.handle_cost(app);
                false
            }
            "new" => {
                match &self.session {
                    Some(ctrl) => match ctrl.new_session().await {
                        Ok(()) => {
                            app.clear_display();
                            app.last_prompt = None;
                            app.push_line(LineKind::System, "新会话已开始");
                        }
                        Err(e) => app.push_line(LineKind::Error, &format!("新建会话失败: {e}")),
                    },
                    None => app.push_line(
                        LineKind::System,
                        "会话管理不可用（未提供 SessionController）",
                    ),
                }
                false
            }
            "sessions" => {
                match &self.session {
                    Some(ctrl) => match ctrl.list_sessions().await {
                        Ok(mut ids) if !ids.is_empty() => {
                            ids.sort();
                            ids.reverse(); // id 按时间字典序，最新优先
                            let current = ctrl.current_session().await;
                            app.push_line(LineKind::System, "已保存会话（最新优先）:");
                            for id in &ids {
                                let marker = if current.as_deref() == Some(id.as_str()) {
                                    "  (当前)"
                                } else {
                                    ""
                                };
                                app.push_line(LineKind::System, &format!("  {id}{marker}"));
                            }
                        }
                        Ok(_) => app.push_line(LineKind::System, "（还没有已保存的会话）"),
                        Err(e) => app.push_line(LineKind::Error, &format!("列出会话失败: {e}")),
                    },
                    None => app.push_line(
                        LineKind::System,
                        "会话管理不可用（未提供 SessionController）",
                    ),
                }
                false
            }
            "resume" => {
                let target = rest.trim();
                match &self.session {
                    Some(ctrl) if !target.is_empty() => match ctrl.resume(target).await {
                        Ok(n) => {
                            app.clear_display();
                            app.last_prompt = None;
                            app.push_line(
                                LineKind::System,
                                &format!("已恢复 '{target}' — {n} 条消息"),
                            );
                        }
                        Err(e) => app.push_line(LineKind::Error, &format!("恢复会话失败: {e}")),
                    },
                    Some(_) => app.push_line(
                        LineKind::Error,
                        "用法: /resume <session-id>（见 /sessions）",
                    ),
                    None => app.push_line(
                        LineKind::System,
                        "会话管理不可用（未提供 SessionController）",
                    ),
                }
                false
            }
            _ => matches!(app.execute_command(cmd), CommandResult::Quit),
        }
    }

    fn handle_model(&mut self, app: &mut AppState, args: &str) {
        let (sub, sub_args) = args.split_once(' ').unwrap_or((args, ""));
        match sub {
            "" | "help" => {
                app.push_line(LineKind::System, "Model commands:");
                for line in [
                    "  /model                  显示当前模型与帮助",
                    "  /model effort <level>   设置 reasoning effort: disabled|high|max",
                    "  /model thinking         切换 thinking 开/关",
                    "  /model switch <name>    切换到指定模型",
                    "  /model use <role> <name> 设置角色指针: main|task|compact|quick",
                ] {
                    app.push_line(LineKind::System, line);
                }
                app.push_line(
                    LineKind::System,
                    &format!(
                        "当前: effort={} model={}",
                        effort_label(self.current_effort),
                        self.current_model.as_deref().unwrap_or("(default)")
                    ),
                );
                if let Some(r) = &self.router {
                    for role in [
                        ModelRole::Main,
                        ModelRole::Task,
                        ModelRole::Compact,
                        ModelRole::Quick,
                    ] {
                        app.push_line(
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
            "effort" => {
                if sub_args.is_empty() {
                    app.push_line(
                        LineKind::System,
                        &format!(
                            "当前 reasoning effort: {} (基线: {})",
                            effort_label(self.current_effort),
                            effort_label(self.baseline_effort)
                        ),
                    );
                    app.push_line(LineKind::System, "用法: /model effort disabled|high|max");
                } else {
                    match parse_effort_command(sub_args) {
                        Ok(effort) => self.rebuild_runner(app, Some(effort), None),
                        Err(msg) => app.push_line(LineKind::Error, &msg),
                    }
                }
            }
            "thinking" => {
                let new_effort = toggle_thinking(self.current_effort, self.baseline_effort);
                if new_effort != self.current_effort {
                    app.push_line(
                        LineKind::System,
                        &format!(
                            "thinking {} → {}",
                            if self.current_effort.thinking() {
                                "on"
                            } else {
                                "off"
                            },
                            if new_effort.thinking() { "on" } else { "off" }
                        ),
                    );
                    self.rebuild_runner(app, Some(new_effort), None);
                } else {
                    app.push_line(LineKind::System, "thinking 状态未变");
                }
            }
            "switch" => {
                if sub_args.is_empty() {
                    app.push_line(LineKind::Error, "用法: /model switch <provider-model-name>");
                } else {
                    self.rebuild_runner(app, None, Some(sub_args.to_string()));
                }
            }
            "use" => {
                let mut parts = sub_args.split_whitespace();
                match (parts.next(), parts.next(), &self.router) {
                    (Some(role_s), Some(model), Some(r)) => match ModelRole::parse(role_s) {
                        Some(role) => match r.set_pointer(role, model) {
                            Ok(()) => {
                                app.push_line(
                                    LineKind::System,
                                    &format!("pointer {} → {model}", role.label()),
                                );
                                self.rebuild_runner(app, None, None);
                            }
                            Err(e) => app.push_line(LineKind::Error, &e.to_string()),
                        },
                        None => {
                            app.push_line(LineKind::Error, "未知角色（main|task|compact|quick）")
                        }
                    },
                    (_, _, None) => {
                        app.push_line(LineKind::Error, "model pointers 不可用（未提供 router）")
                    }
                    _ => app.push_line(
                        LineKind::Error,
                        "用法: /model use <main|task|compact|quick> <model-name>",
                    ),
                }
            }
            other => {
                app.push_line(
                    LineKind::Error,
                    &format!("未知 /model 子命令: {other}（/model help 查看）"),
                );
            }
        }
    }

    fn handle_cost(&mut self, app: &mut AppState) {
        let Some(r) = &self.router else {
            app.push_line(LineKind::System, "router 不可用（/cost 需要 ModelRouter）");
            return;
        };
        let report = r.ledger().report(&r.price_table());
        if report.rows.is_empty() {
            app.push_line(LineKind::System, "还没有用量记录");
            return;
        }
        app.push_line(
            LineKind::System,
            &format!(
                "{:<22} {:<8} {:>10} {:>12} {:>10} {:>10}",
                "model", "role", "prompt", "completion", "cache-hit", "cost($)"
            ),
        );
        for row in report.rows.iter().take(20) {
            let cost = row
                .cost_usd
                .map(|c| format!("{c:.6}"))
                .unwrap_or_else(|| "-".to_string());
            app.push_line(
                LineKind::System,
                &format!(
                    "{:<22} {:<8} {:>10} {:>12} {:>10} {:>10}",
                    row.model,
                    row.role.label(),
                    row.bucket.prompt_tokens,
                    row.bucket.completion_tokens,
                    row.bucket.cache_hit_tokens,
                    cost
                ),
            );
        }
        if let Some(total) = report.total_usd {
            app.push_line(LineKind::System, &format!("总计: ${total:.6}"));
        }
        if report.unmetered_calls > 0 {
            app.push_line(
                LineKind::System,
                &format!("（未计量调用: {}）", report.unmetered_calls),
            );
        }
    }

    /// 用工厂重建 runner（/model 系列命令）。失败只提示，不破坏当前会话。
    fn rebuild_runner(
        &mut self,
        app: &mut AppState,
        effort: Option<ReasoningEffort>,
        model: Option<String>,
    ) {
        let Some(f) = &self.factory else {
            app.push_line(LineKind::Error, "模型切换不可用（未提供 agent 工厂）");
            return;
        };
        let eff = effort.unwrap_or(self.current_effort);
        let mdl = model.or_else(|| self.current_model.clone());
        match f(Some(eff), mdl.clone()) {
            Ok(runner) => {
                self.runner = runner;
                self.current_effort = eff;
                self.current_model = mdl.clone();
                self.model_label = mdl.unwrap_or_else(|| "default".to_string());
                app.push_line(
                    LineKind::System,
                    &format!(
                        "模型已切换: effort={} model={}",
                        effort_label(eff),
                        self.model_label
                    ),
                );
            }
            Err(e) => app.push_line(LineKind::Error, &format!("模型切换失败: {e}")),
        }
    }
}

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
    /// 最近一次提交的 prompt（回合落盘用）。
    last_prompt: Option<String>,
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

    /// 清空对话面板（/clear、/new、/resume 共用）。
    fn clear_display(&mut self) {
        self.lines.clear();
        self.pending_text.clear();
        self.pending_reasoning.clear();
        self.scroll_offset = 0;
        self.auto_scroll = true;
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
                if prompt.starts_with('/') {
                    // 命令不入输入历史，由 run loop 分派。
                    return KeyAction::Submit(prompt);
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
                self.clear_display();
                CommandResult::Handled
            }
            "help" | "h" => {
                self.push_line(LineKind::System, "可用命令:");
                for line in [
                    "  /help          显示帮助",
                    "  /clear         清空对话面板",
                    "  /new           开始新会话",
                    "  /sessions      列出已保存会话",
                    "  /resume <id>   恢复指定会话",
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
    use deepseeknova_core::runner::{RunEventStream, RunInput};

    struct StubRunner;

    #[async_trait::async_trait]
    impl Runner for StubRunner {
        async fn run_stream(&self, _input: RunInput) -> anyhow::Result<RunEventStream> {
            Ok(Box::pin(tokio_stream::empty()))
        }
    }

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

    #[test]
    fn effort_helpers_parse_and_toggle() {
        assert!(matches!(
            parse_effort_command("high"),
            Ok(ReasoningEffort::High)
        ));
        assert!(matches!(
            parse_effort_command("disabled"),
            Ok(ReasoningEffort::Disabled)
        ));
        assert!(parse_effort_command("bogus").is_err());
        assert!(matches!(
            toggle_thinking(ReasoningEffort::High, ReasoningEffort::Max),
            ReasoningEffort::Disabled
        ));
        assert!(matches!(
            toggle_thinking(ReasoningEffort::Disabled, ReasoningEffort::Max),
            ReasoningEffort::Max
        ));
        assert_eq!(effort_label(ReasoningEffort::Max), "max");
    }

    #[tokio::test]
    async fn model_effort_rebuilds_via_factory() {
        let calls = Arc::new(std::sync::Mutex::new(0usize));
        let c2 = calls.clone();
        let factory = move |effort: Option<ReasoningEffort>,
                            _model: Option<String>|
              -> anyhow::Result<Arc<dyn Runner>> {
            *c2.lock().unwrap() += 1;
            assert!(matches!(effort, Some(ReasoningEffort::Max)));
            Ok(Arc::new(StubRunner))
        };
        let mut tui = TuiRunner::new(Arc::new(StubRunner)).with_agent_factory(factory);
        let mut app = AppState::default();
        assert!(!tui.handle_command(&mut app, "model effort max").await);
        assert_eq!(*calls.lock().unwrap(), 1);
        assert!(matches!(tui.current_effort, ReasoningEffort::Max));
        assert!(app.lines.iter().any(|l| l.text.contains("模型已切换")));
    }

    #[tokio::test]
    async fn model_switch_updates_label_and_model() {
        let factory = |_effort: Option<ReasoningEffort>,
                       model: Option<String>|
         -> anyhow::Result<Arc<dyn Runner>> {
            assert_eq!(model.as_deref(), Some("deepseek-v4-pro"));
            Ok(Arc::new(StubRunner))
        };
        let mut tui = TuiRunner::new(Arc::new(StubRunner)).with_agent_factory(factory);
        let mut app = AppState::default();
        tui.handle_command(&mut app, "model switch deepseek-v4-pro")
            .await;
        assert_eq!(tui.current_model.as_deref(), Some("deepseek-v4-pro"));
        assert_eq!(tui.model_label, "deepseek-v4-pro");
    }

    #[tokio::test]
    async fn cost_without_router_reports_unavailable() {
        let mut tui = TuiRunner::new(Arc::new(StubRunner));
        let mut app = AppState::default();
        tui.handle_command(&mut app, "cost").await;
        assert!(app.lines.iter().any(|l| l.text.contains("router 不可用")));
    }

    #[tokio::test]
    async fn quit_command_returns_true() {
        let mut tui = TuiRunner::new(Arc::new(StubRunner));
        let mut app = AppState::default();
        assert!(tui.handle_command(&mut app, "quit").await);
    }

    #[derive(Default)]
    struct MockSessionController {
        sessions: std::sync::Mutex<Vec<String>>,
        current: std::sync::Mutex<Option<String>>,
        resumed: std::sync::Mutex<Option<String>>,
        new_count: std::sync::Mutex<usize>,
    }

    #[async_trait]
    impl SessionController for MockSessionController {
        async fn new_session(&self) -> anyhow::Result<()> {
            *self.new_count.lock().unwrap() += 1;
            *self.current.lock().unwrap() = Some("new-id".into());
            Ok(())
        }
        async fn list_sessions(&self) -> anyhow::Result<Vec<String>> {
            Ok(self.sessions.lock().unwrap().clone())
        }
        async fn current_session(&self) -> Option<String> {
            self.current.lock().unwrap().clone()
        }
        async fn resume(&self, id: &str) -> anyhow::Result<usize> {
            *self.resumed.lock().unwrap() = Some(id.to_string());
            Ok(3)
        }
        async fn record_turn(
            &self,
            _prompt: &str,
            _output: &str,
            _model: Option<String>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn session_new_clears_display_and_calls_controller() {
        let ctrl = Arc::new(MockSessionController::default());
        let mut tui = TuiRunner::new(Arc::new(StubRunner)).with_session_controller(ctrl.clone());
        let mut app = AppState::default();
        app.push_line(LineKind::User, "old");
        assert!(!tui.handle_command(&mut app, "new").await);
        assert_eq!(*ctrl.new_count.lock().unwrap(), 1);
        assert_eq!(app.lines.len(), 1, "旧内容已清空，只剩提示行");
        assert!(app.lines[0].text.contains("新会话已开始"));
    }

    #[tokio::test]
    async fn session_list_marks_current() {
        let ctrl = Arc::new(MockSessionController::default());
        *ctrl.sessions.lock().unwrap() = vec!["b".into(), "a".into()];
        *ctrl.current.lock().unwrap() = Some("a".into());
        let mut tui = TuiRunner::new(Arc::new(StubRunner)).with_session_controller(ctrl);
        let mut app = AppState::default();
        tui.handle_command(&mut app, "sessions").await;
        let texts: Vec<&str> = app.lines.iter().map(|l| l.text.as_str()).collect();
        assert!(texts.iter().any(|t| t.contains("  b")));
        assert!(texts.iter().any(|t| t.contains("  a  (当前)")));
    }

    #[tokio::test]
    async fn session_resume_restores_and_clears() {
        let ctrl = Arc::new(MockSessionController::default());
        let mut tui = TuiRunner::new(Arc::new(StubRunner)).with_session_controller(ctrl.clone());
        let mut app = AppState::default();
        app.push_line(LineKind::User, "old");
        tui.handle_command(&mut app, "resume abc").await;
        assert_eq!(ctrl.resumed.lock().unwrap().as_deref(), Some("abc"));
        assert_eq!(app.lines.len(), 1);
        assert!(app.lines[0].text.contains("已恢复 'abc' — 3 条消息"));
    }

    #[tokio::test]
    async fn session_commands_without_controller_report_unavailable() {
        let mut tui = TuiRunner::new(Arc::new(StubRunner));
        let mut app = AppState::default();
        tui.handle_command(&mut app, "new").await;
        tui.handle_command(&mut app, "resume x").await;
        let texts: Vec<&str> = app.lines.iter().map(|l| l.text.as_str()).collect();
        assert!(texts.iter().any(|t| t.contains("会话管理不可用")));
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

//! ratatui-based interactive terminal UI for deepseeknova.
//!
//! Wraps a [`Runner`] and displays its full event stream as a structured
//! message tree in a split-pane terminal UI:
//!
//! - streaming text / reasoning（推理整段提交，默认折叠）
//! - tool calls with truncated results and live status
//! - deterministic verification（`✓` / `✗`）
//! - pauses, errors, approval requests
//! - status bar with model, phase, token usage, cost and scrollback position
//! - multi-line input editing（←/→/Home/End、Shift+Enter、Ctrl+U/W）、
//!   input history、markdown 高亮、`@` 文件补全、粘贴路径转引用
//! - slash commands 与 Ctrl+K 命令面板（同一注册表）：`/help` `/clear`
//!   `/new` `/sessions` `/resume` `/model` `/cost` `/skills` `/mcp` `/raw`
//!   `/fold` `/copy` `/undo` `/quit`
//! - 可切换侧边栏（会话/工具活动/MCP/成本/技能），窄终端自动隐藏
//! - 主题：`DEEPSEEKNOVA_THEME`（`codex` 默认 | `dark` | `light`）
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

mod app;
pub mod approval;
mod commands;
pub mod input;
mod model;
mod render;
mod theme;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use deepseeknova_core::runner::Runner;
use deepseeknova_provider::factory::ReasoningEffort;
use deepseeknova_provider::router::ModelRouter;

pub use app::state::{
    McpProbe, McpServerInfo, McpStatus, ResumedLine, ResumedRole, SessionController, UndoController,
};
pub use theme::Theme;

use app::state::AppState;
use commands::TuiCaps;

/// agent 重建工厂类型。
type AgentFactory = Arc<
    dyn Fn(Option<ReasoningEffort>, Option<String>) -> anyhow::Result<Arc<dyn Runner>>
        + Send
        + Sync,
>;

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
    /// `/skills` 扫描的技能目录（默认 `.deepseeknova/skills`、`.agents/skills`）。
    skills_paths: Vec<PathBuf>,
    /// `/mcp` 展示的已启用 MCP server（由 CLI 从配置传入，含探测用启动命令）。
    mcp_servers: Vec<McpServerInfo>,
    /// `/mcp` 实时连接探测（CLI 实现；缺失时仅列名）。
    mcp_probe: Option<Arc<dyn McpProbe>>,
    /// 可选撤销控制器：启用 `/undo` `/undo all` `/undo list`。
    undo: Option<Arc<dyn UndoController>>,
    /// 编程式注入主题（优先级高于 `DEEPSEEKNOVA_THEME`）。
    theme: Option<Theme>,
    /// `@` 补全候选文件清单（CLI 注入；为空不触发补全）。
    at_files: Vec<String>,
    /// 主模型上下文窗口上限（tokens），CLI 从 config 注入；None 不显示占用率。
    context_window: Option<u32>,
    /// 权限审批请求接收端（CLI 注入 agent 的 responder 通道）。
    approval_rx: Option<tokio::sync::mpsc::Receiver<crate::approval::ApprovalRequest>>,
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
            skills_paths: vec![
                PathBuf::from(".deepseeknova/skills"),
                PathBuf::from(".agents/skills"),
            ],
            mcp_servers: Vec::new(),
            mcp_probe: None,
            undo: None,
            theme: None,
            at_files: Vec::new(),
            context_window: None,
            approval_rx: None,
        }
    }

    /// 注入权限审批请求接收端（与注入 agent 的 `TuiApprovalResponder`
    /// 同一通道），启用确认浮层（y 允许 / n 拒绝）。
    pub fn with_approval_rx(
        mut self,
        rx: tokio::sync::mpsc::Receiver<crate::approval::ApprovalRequest>,
    ) -> Self {
        self.approval_rx = Some(rx);
        self
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

    /// 指定 `/skills` 扫描的技能目录（默认为 `.deepseeknova/skills`、`.agents/skills`）。
    pub fn with_skills_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.skills_paths = paths;
        self
    }

    /// 指定 `/mcp` 展示的已启用 MCP server（含探测用启动命令）。
    pub fn with_mcp_servers(mut self, servers: Vec<McpServerInfo>) -> Self {
        self.mcp_servers = servers;
        self
    }

    /// 指定 `/mcp` 实时连接探测器。
    pub fn with_mcp_probe(mut self, probe: Arc<dyn McpProbe>) -> Self {
        self.mcp_probe = Some(probe);
        self
    }

    /// 提供撤销控制器，启用 `/undo` `/undo all` `/undo list`。
    pub fn with_undo_controller(mut self, controller: Arc<dyn UndoController>) -> Self {
        self.undo = Some(controller);
        self
    }

    /// 编程式注入主题（优先级高于 `DEEPSEEKNOVA_THEME` 环境变量）。
    pub fn with_theme(mut self, theme: Theme) -> Self {
        self.theme = Some(theme);
        self
    }

    /// 注入 `@` 补全候选文件清单（CLI 从工作区扫描后传入；缺省不触发补全）。
    pub fn with_at_files(mut self, files: Vec<String>) -> Self {
        self.at_files = files;
        self
    }

    /// 注入主模型上下文窗口上限（tokens），CLI 从 config 模型定义读取；
    /// 缺省/None 时状态行与 `/cost` 不显示占用率百分比。
    pub fn with_context_window(mut self, window: Option<u32>) -> Self {
        self.context_window = window;
        self
    }

    /// Enter the TUI and block until the user quits.
    pub async fn run(&mut self) -> anyhow::Result<()> {
        let mut terminal = ratatui::init();
        // 启用鼠标上报：滚轮事件由应用消费并滚动对话历史，
        // 否则滚轮只会滚动终端自身滚动区（表现为“滚动到了输入框”）。
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture);
        let result = self.run_inner(&mut terminal).await;
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
        ratatui::restore();
        result
    }

    async fn run_inner(&mut self, terminal: &mut ratatui::DefaultTerminal) -> anyhow::Result<()> {
        // 主题：编程式注入优先，否则读环境变量（未知值回退 codex + 提示）。
        let (theme, theme_warning) = match &self.theme {
            Some(t) => (t.clone(), None),
            None => theme::theme_from_env(),
        };

        let runtime = commands::TuiRuntime {
            runner: Some(self.runner.clone()),
            model_label: self.model_label.clone(),
            current_effort: self.current_effort,
            current_model: self.current_model.clone(),
            baseline_effort: self.baseline_effort,
            factory: self.factory.clone(),
            router: self.router.clone(),
        };
        let mut caps = TuiCaps {
            runtime: Arc::new(Mutex::new(runtime)),
            session: self.session.clone(),
            skills_paths: self.skills_paths.clone(),
            mcp_servers: self.mcp_servers.clone(),
            mcp_probe: self.mcp_probe.clone(),
            undo: self.undo.clone(),
            context_window: self.context_window,
            approval_rx: self.approval_rx.take(),
        };

        let mut app = AppState {
            model_label: self.model_label.clone(),
            theme,
            at_files: self.at_files.clone(),
            ..Default::default()
        };
        // 与上方 EnableMouseCapture 保持一致：鼠标捕获默认开启。
        app.mouse_capture = true;
        // 用户键位定制（keybindings.json）：启动时加载，事件循环轮询热重载。
        app.keymap_path = crate::app::keybindings::Keymap::default_path();
        app.keymap = crate::app::keybindings::Keymap::load(&app.keymap_path);
        app.keymap_mtime = std::fs::metadata(&app.keymap_path)
            .ok()
            .and_then(|m| m.modified().ok());
        if !app.keymap.diagnostics.is_empty() {
            let diags: Vec<String> = app.keymap.diagnostics.clone();
            for d in &diags {
                app.echo_line(model::conversation::LineKind::Error, d);
            }
        } else if app.keymap_path.exists() {
            app.echo_line(
                model::conversation::LineKind::System,
                &format!(
                    "已加载键位定制 {}（{} 条覆盖）",
                    app.keymap_path.display(),
                    app.keymap.override_count()
                ),
            );
        }
        if let Some(warning) = theme_warning {
            app.echo_line(model::conversation::LineKind::System, &warning);
        }

        app::run_loop(terminal, &mut app, &mut caps).await?;
        Ok(())
    }
}

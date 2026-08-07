//! 命令注册表：斜杠命令与 Ctrl+K 命令面板共用同一 handler。
//!
//! 消除旧版 `TuiRunner::handle_command`（需外部依赖）与 `AppState::execute_command`
//! （纯本地）的双路径分叉：命令统一注册，注入能力经 [`TuiCaps`] 读取，命令反馈
//! 写入 `AppState.echo`（`CommandCtx.app.echo_line`）。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::app::state::AppState;
use deepseeknova_core::runner::Runner;
use deepseeknova_provider::factory::ReasoningEffort;
use deepseeknova_provider::router::ModelRouter;

/// 命令参数规格（命令面板补全/提示用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgsSpec {
    /// 无参数。
    None,
    /// 自由文本参数。
    FreeText,
    /// 枚举参数（命令面板展示候选）。
    Enum(&'static [&'static str]),
}

/// 斜杠命令行内候选状态（输入 `/` 开头时展示，纯 `/` 触发）。
#[derive(Clone, Default, PartialEq)]
pub struct CommandHintState {
    /// 模糊匹配候选（复用 CommandRegistry::search）。
    pub candidates: Vec<&'static Command>,
    /// 当前选中项。
    pub selected: usize,
    /// 参数模式：已输入 `/<cmd> <前缀>` 时，展示该命令的枚举参数候选
    /// （如 `/fold ` → all|none|reset），Enter 选中执行。
    pub arg_options: Option<Vec<&'static str>>,
}

impl CommandHintState {
    /// 浮层实际渲染的候选行数（参数模式按 arg_options，否则按命令数；上限 8）。
    /// 渲染端与高度估算共用，保证与 message.rs 的 hint_area 高度一致。
    pub fn visible_rows(&self) -> usize {
        match &self.arg_options {
            Some(opts) => opts.len().min(8),
            None => self.candidates.len().min(8),
        }
    }
}

impl std::fmt::Debug for CommandHintState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.candidates.iter().map(|c| c.name).collect();
        f.debug_struct("CommandHintState")
            .field("candidates", &names)
            .field("selected", &self.selected)
            .field("arg_options", &self.arg_options)
            .finish()
    }
}
/// 命令执行结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOutcome {
    /// 已处理。
    Handled,
    /// 退出 TUI。
    Quit,
}

/// agent 重建工厂类型：`(effort, model)` → 新 runner。
pub type RunnerFactory = Arc<
    dyn Fn(Option<ReasoningEffort>, Option<String>) -> anyhow::Result<Arc<dyn Runner>>
        + Send
        + Sync,
>;

/// 运行时可变状态（builder 注入 + 热切换产生的模型状态），
/// 命令内经 [`TuiCaps`] 的 Mutex 修改。
pub struct TuiRuntime {
    /// 当前 runner（模型切换后替换）。
    pub runner: Option<Arc<dyn Runner>>,
    pub model_label: String,
    pub current_effort: ReasoningEffort,
    pub current_model: Option<String>,
    pub baseline_effort: ReasoningEffort,
    /// agent 重建工厂。
    pub factory: Option<RunnerFactory>,
    /// ModelRouter（启用 `/model use` 与 `/cost`）。
    pub router: Option<Arc<ModelRouter>>,
}

impl Default for TuiRuntime {
    fn default() -> Self {
        Self {
            runner: None,
            model_label: String::new(),
            current_effort: ReasoningEffort::Disabled,
            current_model: None,
            baseline_effort: ReasoningEffort::Disabled,
            factory: None,
            router: None,
        }
    }
}

/// 命令可用的注入能力（全部 Option，缺失时命令降级提示）。
pub struct TuiCaps {
    pub runtime: Arc<Mutex<TuiRuntime>>,
    /// 会话控制器（`/new` `/sessions` `/resume` + 回合落盘）。
    pub session: Option<Arc<dyn crate::app::state::SessionController>>,
    /// `/skills` 扫描目录。
    pub skills_paths: Vec<std::path::PathBuf>,
    /// `/mcp` 展示的已启用 server。
    pub mcp_servers: Vec<crate::app::state::McpServerInfo>,
    /// `/mcp` 实时连接探测器。
    pub mcp_probe: Option<Arc<dyn crate::app::state::McpProbe>>,
    /// 撤销控制器（`/undo`）。
    pub undo: Option<Arc<dyn crate::app::state::UndoController>>,
    /// 主模型上下文窗口上限（tokens），由 CLI 从 config 注入；
    /// `None` 时状态行与 `/cost` 不显示占用率百分比。
    pub context_window: Option<u32>,
    /// 会话总预算上限（tokens），CLI 从 `[budget] max_total_tokens` 注入；
    /// 与 `context_window` 取较小值作为 ctx 计量分母。
    pub budget_window: Option<u32>,
    /// 权限审批请求接收端（CLI 注入 agent 的 responder 通道）。
    pub approval_rx: Option<tokio::sync::mpsc::Receiver<crate::approval::ApprovalRequest>>,
}

/// 命令执行上下文：AppState（渲染/回显/折叠）+ 注入能力。
pub struct CommandCtx<'a> {
    pub app: &'a mut AppState,
    pub caps: &'a TuiCaps,
}

/// 命令 handler 抽象（静态分发，async-friendly）。
#[async_trait]
pub trait CommandHandler: Send + Sync {
    async fn run(&self, ctx: &mut CommandCtx<'_>, args: &str) -> CommandOutcome;
}

/// 一个注册命令。
pub struct Command {
    pub name: &'static str,
    pub desc: &'static str,
    /// Ctrl+K 模糊搜索的附加关键词。
    pub keywords: &'static [&'static str],
    pub args_spec: ArgsSpec,
    /// 参数模式候选提示（`/cmd ` 已输入时展示）；None 表示无参数提示。
    /// 与 `ArgsSpec` 搭配：Enum 给出各枚举项，FreeText 给出用法串。
    pub args_hint: Option<&'static [&'static str]>,
    pub handler: &'static dyn CommandHandler,
}

impl PartialEq for Command {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

/// 命令注册表：内建命令的静态集合。
pub struct CommandRegistry;

impl CommandRegistry {
    /// 全部内建命令（由 `builtin.rs` 提供）。
    pub fn builtin() -> &'static [Command] {
        crate::commands::builtin::BUILTIN
    }

    /// 按名查找。
    pub fn find(name: &str) -> Option<&'static Command> {
        Self::builtin().iter().find(|c| c.name == name)
    }

    /// 模糊搜索（name/desc/keywords 子串匹配，用于 Ctrl+K 面板）。
    pub fn search(query: &str) -> Vec<&'static Command> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Self::builtin().iter().collect();
        }
        Self::builtin()
            .iter()
            .filter(|c| {
                c.name.to_lowercase().contains(&q)
                    || c.desc.to_lowercase().contains(&q)
                    || c.keywords.iter().any(|k| k.to_lowercase().contains(&q))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_find_returns_builtin() {
        assert!(CommandRegistry::find("help").is_some());
        assert!(CommandRegistry::find("nope").is_none());
    }

    #[test]
    fn registry_search_matches_name_desc_keywords() {
        let by_name = CommandRegistry::search("cost");
        assert!(by_name.iter().any(|c| c.name == "cost"));
        let by_keyword = CommandRegistry::search("折叠");
        assert!(by_keyword.iter().any(|c| c.name == "fold"));
        assert!(CommandRegistry::search("zzz").is_empty());
        assert_eq!(
            CommandRegistry::search("").len(),
            CommandRegistry::builtin().len()
        );
    }
}

pub mod builtin;

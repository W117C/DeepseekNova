use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "deepseeknova")]
#[command(version = "0.1.0")]
#[command(about = "A DeepSeek-native AI coding agent for your terminal", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// Shared flags for commands that dispatch to a provider model.
#[derive(Args, Debug, Clone)]
pub struct ModelArgs {
    /// Model name (default: config default_model).
    #[arg(long)]
    pub model: Option<String>,

    /// Max tool-call rounds (0 = use config/default).
    #[arg(long, default_value_t = 0)]
    pub max_steps: usize,
}

/// Coordinator-specific flags — when set, a two-model planner + executor
/// pipeline is used instead of the single-agent loop.
#[derive(Args, Debug, Clone)]
pub struct CoordinatorArgs {
    /// Enable coordinator mode with this model as the planner.
    #[arg(long, help_heading = "Coordinator")]
    pub planner_model: Option<String>,

    /// Model for the executor phase (defaults to --model or config default).
    #[arg(long, help_heading = "Coordinator")]
    pub executor_model: Option<String>,

    /// Max graph nodes allowed from the planner (default: 20).
    #[arg(long, default_value_t = 20, help_heading = "Coordinator")]
    pub max_graph_nodes: usize,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run agent on a prompt (single-model or coordinator).
    Run {
        #[command(flatten)]
        model: ModelArgs,

        #[command(flatten)]
        coordinator: CoordinatorArgs,

        prompt: Vec<String>,
    },
    /// Produce a structured plan without executing tools.
    Plan {
        /// Model name for planning (default: config default_model).
        #[arg(long)]
        model: Option<String>,

        #[command(flatten)]
        coordinator: CoordinatorArgs,

        prompt: Vec<String>,
    },
    /// Interactive chat session
    Chat {
        #[arg(long)]
        model: Option<String>,
        /// Resume the most recent saved session's history.
        #[arg(long)]
        resume: bool,
        /// Launch the full-screen terminal UI instead of the line REPL.
        #[arg(long, conflicts_with = "resume")]
        tui: bool,
    },
    /// Start the HTTP/SSE server
    Serve {
        #[arg(long, default_value = "127.0.0.1:8787")]
        addr: String,
    },
    /// Run configuration wizard
    Setup {
        #[arg(long)]
        local: bool,
    },
    /// Print configuration details
    Config,
    /// 记忆库管理（查看/检索/删除/统计）。
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
    /// Init a new DeepseekNova project
    Init,
}

#[derive(Subcommand)]
pub enum MemoryAction {
    /// 列出某类记忆（task/skill/user_profile）。
    List {
        #[arg(long, default_value = "task")]
        category: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// 按相关度检索记忆。
    Search { query: Vec<String> },
    /// 按 id/key 删除一条记忆。
    Forget { id: String },
    /// 打印统计（召回命中率、reinforce 比例）——P2 决策依据。
    Stats,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        // Catches structural errors (e.g. bad conflicts_with references).
        Cli::command().debug_assert();
    }

    #[test]
    fn chat_tui_conflicts_with_resume() {
        let err = Cli::try_parse_from(["deepseeknova", "chat", "--tui", "--resume"]);
        assert!(
            err.is_err(),
            "--tui and --resume must be mutually exclusive"
        );
    }

    #[test]
    fn chat_tui_alone_parses() {
        let parsed = Cli::try_parse_from(["deepseeknova", "chat", "--tui"]);
        assert!(parsed.is_ok(), "--tui alone should parse");
    }
}

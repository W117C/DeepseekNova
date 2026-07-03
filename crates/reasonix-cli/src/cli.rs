use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "reasonix")]
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
    /// Init a new Reasonix project
    Init,
}

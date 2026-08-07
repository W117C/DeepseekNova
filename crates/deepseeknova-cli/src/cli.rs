use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "deepseeknova")]
#[command(version = "0.4.0")]
#[command(about = "A DeepSeek-native AI coding agent for your terminal", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// 一键开启安全默认：权限门控保持开启 + 沙箱启用（若 OS 支持；Windows
    /// 无 OS 沙箱后端时回落 NoOpSandbox 并在启动时警告）。未启用项在启动
    /// 日志横幅明示（runtime 构建 agent 时检查）。
    #[arg(long, global = true)]
    pub secure_defaults: bool,
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
    /// Scan the codebase for security issues (regex matchers + optional AI investigation).
    Scan {
        /// Root path to scan (default: current directory).
        #[arg(long)]
        path: Option<String>,
        /// Output format: "md" or "json".
        #[arg(long, default_value = "md")]
        format: String,
        /// Skip the AI investigation stage (matcher-only output).
        #[arg(long)]
        no_ai: bool,
        /// Minimum severity to report: high|medium|low.
        #[arg(long, default_value = "low")]
        severity_min: String,
    },
    /// Run eval cases from a JSONL file and print a pass/fail report.
    Eval {
        /// Path to JSONL eval file (default: evals.jsonl). Each line:
        /// {"prompt":"...","must_contain":["..."]}
        #[arg(long, default_value = "evals.jsonl")]
        path: String,
        /// Output format: "md" or "json".
        #[arg(long, default_value = "md")]
        format: String,
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
        /// Run as an Agent Client Protocol (ACP) stdio server instead of HTTP.
        #[arg(long)]
        acp: bool,
        /// Require this bearer token on every /v1/* route. When unset the
        /// server is open — only safe on trusted loopback.
        #[arg(long)]
        token: Option<String>,
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
    /// 检查点快照管理（写前快照 + 回滚）。
    Checkpoint {
        #[command(subcommand)]
        action: CheckpointAction,
    },
    /// 生成项目后置产出（Wiki / 知识卡片，A2）。
    Artifacts {
        #[command(subcommand)]
        action: ArtifactsAction,
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
    /// 打印统计（召回命中率、reinforce 比例、stage 分布）——P2 决策依据。
    Stats,
    /// 为尚无向量的旧记忆生成嵌入（embedder=none/缺 key 时无操作）。
    EmbedBackfill,
    /// 衰减 + 归档超期清理（decay_rate/archive_ttl_days 取自 \[memory\] 配置）。
    Cleanup,
}

#[derive(Subcommand)]
pub enum CheckpointAction {
    /// 列出当前快照与文件状态（unchanged/modified）。
    List,
    /// 回滚最近一个快照；--all 回滚全部。
    Rollback {
        #[arg(long)]
        all: bool,
    },
    /// 丢弃全部快照（不恢复文件）。
    Clear,
}

#[derive(Subcommand)]
pub enum ArtifactsAction {
    /// 生成 Repo Wiki（首页/ADR/API/依赖/变更日志）。
    Wiki {
        #[arg(long, default_value = "wiki")]
        out: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        summary: Option<String>,
    },
    /// 生成一张知识卡片。
    Cards {
        #[arg(long, default_value = "cards")]
        out: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        insight: String,
        #[arg(long)]
        tags: Vec<String>,
        #[arg(long)]
        source: Option<String>,
    },
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

    #[test]
    fn secure_defaults_flag_parses_before_and_after_subcommand() {
        // global flag：子命令前、后均可解析，且所有子命令共享。
        let before =
            Cli::try_parse_from(["deepseeknova", "--secure-defaults", "run", "x"]).unwrap();
        assert!(before.secure_defaults);
        let after = Cli::try_parse_from(["deepseeknova", "chat", "--secure-defaults"]).unwrap();
        assert!(
            after.secure_defaults,
            "global flag must parse after subcommand"
        );
        let off = Cli::try_parse_from(["deepseeknova", "chat"]).unwrap();
        assert!(!off.secure_defaults, "absent flag must stay false");
    }
}

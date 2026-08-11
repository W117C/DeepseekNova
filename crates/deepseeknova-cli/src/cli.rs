use clap::{Args, Parser, Subcommand};

/// 解析 `--require-dimension name>=N`：校验维度名（含中文别名）与 0..1 阈值。
fn parse_require_dimension(s: &str) -> Result<(String, f32), String> {
    let (name, rest) = s
        .split_once(">=")
        .or_else(|| s.split_once('>'))
        .ok_or_else(|| format!("expected `name>=threshold`, got `{s}`"))?;
    let threshold: f32 = rest
        .trim()
        .parse()
        .map_err(|_| format!("invalid threshold `{rest}` in `{s}`"))?;
    let name = name.trim().to_string();
    if !deepseeknova_metrics::ScoreDimensions::is_valid_name(&name) {
        return Err(format!(
            "unknown dimension `{name}` in `{s}` (governance/verification/reflection/review/protocol/composite)"
        ));
    }
    Ok((name, threshold))
}

#[derive(Parser)]
#[command(name = "deepseeknova")]
// 版本号跟随 workspace（Cargo.toml workspace.package.version），避免
// release tag（v0.5.0 等）与 `--version` 输出漂移——硬编码曾导致用户
// 安装新版后 `--version` 仍显示旧号。
#[command(version = env!("CARGO_PKG_VERSION"))]
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
    /// Run eval cases from a JSONL file and print a graded pass/fail report.
    Eval {
        /// Path to JSONL eval file (default: evals.jsonl). Each line:
        /// {"prompt":"...","must_contain":["..."],"min_score":0.8,
        ///  "dimension_min":{"governance":0.9},"cost_max":0.05,"rounds":3}
        #[arg(long, default_value = "evals.jsonl")]
        path: String,
        /// Output format: "md" or "json".
        #[arg(long, default_value = "md")]
        format: String,
        /// CI 门槛：全部用例综合分均值（0..5；<=1.0 按 0..1 折算 ×5）下限。
        /// 任一 run 均值低于门槛 → 进程退出非零（供 CI 门禁）。
        #[arg(long)]
        require_min_score: Option<f32>,
        /// CI 门槛：单维均值下限，`name>=N`（可重复）。name 支持英文名
        /// (governance/verification/reflection/review/protocol/composite) 与
        /// 中文别名 (治理/验证/反思/审查/协议/综合)，N 为 0..1 阈值。
        #[arg(long = "require-dimension", value_parser = parse_require_dimension)]
        require_dimension: Vec<(String, f32)>,
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
    /// 导入外部 Agent 工具配置（Claude Code / Codex）为分层配置。
    /// 默认仅预览映射计划；`--apply` 才写入目标配置层。
    Import {
        #[command(subcommand)]
        source: ImportSource,
        /// 应用导入（默认仅预览）。
        #[arg(long, global = true)]
        apply: bool,
        /// 写入层级：user（~/.deepseeknova/config.toml，默认）/ project（deepseeknova.toml）。
        #[arg(long, global = true, default_value = "user")]
        scope: String,
    },
    /// 生成项目后置产出（Wiki / 知识卡片，A2）。
    Artifacts {
        #[command(subcommand)]
        action: ArtifactsAction,
    },
    /// Init a new DeepseekNova project
    Init {
        /// 生成私有 DEEPSEEKNOVA.md 而非行业标准 AGENTS.md（向后兼容）。
        #[arg(long)]
        legacy: bool,
    },
    /// 预执行安全审计：预览一条命令/工具调用会被如何分类与裁决。
    /// 用法：`audit <shell-command>` 或 `audit <tool> <json-args>`；
    /// `--rules` 显示当前全部权限规则。
    ///
    /// 审计参数（--format/--rules/--workspace）须置于命令之前；命令内以
    /// `-` 开头的 flag 一律并入被审计命令（`audit ls -la` 审计的就是
    /// `ls -la` 整条命令）。
    Audit {
        /// shell 命令（如 `rm -rf /tmp/x`），或 shell 工具名 + JSON 参数
        /// （如 `bash '{"command":"git status"}'`）。与 `--rules` 连用时可为空。
        /// 允许 `-` 开头的值（shell 命令常带 flag，如 `ls -la`）。
        #[arg(allow_hyphen_values = true)]
        args: Vec<String>,
        /// 输出格式：md | json。
        #[arg(long, default_value = "md")]
        format: String,
        /// 显示当前全部权限规则（deny/ask/allow）后退出。
        #[arg(long)]
        rules: bool,
        /// 工作区根路径（默认当前目录）。
        #[arg(long)]
        workspace: Option<String>,
    },
    /// git worktree 隔离的并行会话（P2-7）。每个 worktree 是主仓库的一个
    /// 独立工作副本，会话状态（.deepseeknova/）按工作区根落盘，天然隔离。
    Worktree {
        #[command(subcommand)]
        action: WorktreeAction,
    },
}

#[derive(Subcommand, Debug)]
pub enum MemoryAction {
    /// 列出记忆（分页；--stage/--tag/--search 过滤，--category 限定类目）。
    List {
        /// 类目：task|skill|user_profile|all（默认 task）。
        #[arg(long, default_value = "task")]
        category: String,
        /// 每页条数（默认 20）。
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// 跳过前 N 条（分页）。
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// 按生命周期阶段过滤：candidate|verified|permanent|archived。
        #[arg(long)]
        stage: Option<String>,
        /// 按标签过滤（任一标签精确匹配即保留）。
        #[arg(long)]
        tag: Option<String>,
        /// 按关键字过滤（内容子串匹配，大小写敏感）。
        #[arg(long)]
        search: Option<String>,
    },
    /// 按相关度检索记忆。
    Search { query: Vec<String> },
    /// 编辑一条记忆的内容（保留 lifecycle；启用嵌入时强制重算向量）。
    Edit {
        /// 记忆 id。
        id: String,
        /// 新内容（多个词以空格连接）。
        content: Vec<String>,
    },
    /// 删除一条记忆（二次确认，不可逆；--yes 跳过确认）。
    Delete {
        /// 记忆 id。
        id: String,
        /// 跳过二次确认直接删除。
        #[arg(long)]
        yes: bool,
    },
    /// 按 id/key 删除一条记忆（无二次确认）。
    Forget { id: String },
    /// 召回回放：执行一次与 recall 同源的混合检索，展示每条命中的
    /// id/内容与分数分解（bm25 / 余弦 / 生命周期惩罚）。
    Replay {
        query: Vec<String>,
        /// 最大命中条数（默认 10）。
        #[arg(long)]
        top_k: Option<usize>,
    },
    /// 打印统计（召回命中率、reinforce 比例、stage 分布）——P2 决策依据。
    Stats,
    /// 为尚无向量的旧记忆生成嵌入（embedder=none/缺 key 时无操作）。
    EmbedBackfill,
    /// 衰减 + 归档超期清理（decay_rate/archive_ttl_days 取自 \[memory\] 配置）。
    Cleanup,
}

#[derive(Subcommand)]
pub enum CheckpointAction {
    /// 列出当前快照与文件状态（unchanged/modified），modified 行带变更行数。
    List,
    /// 显示快照与当前文件的内容级 diff（+ 新增 / - 删除）。
    Diff {
        /// 只显示该文件的 diff；缺省显示全部 modified 文件的 diff。
        #[arg(long)]
        path: Option<String>,
    },
    /// 回滚最近一个快照；--all 回滚全部。
    Rollback {
        #[arg(long)]
        all: bool,
    },
    /// 丢弃全部快照（不恢复文件）。
    Clear,
}

/// 配置导入来源（`import claude` / `import codex`）。
#[derive(Subcommand, Clone, Copy, Debug)]
pub enum ImportSource {
    /// Claude Code（扫描 ~/.claude/settings.json、.claude/settings.json、.mcp.json）。
    Claude,
    /// Codex CLI（扫描 ~/.codex/config.toml、.codex/config.toml）。
    Codex,
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

#[derive(Subcommand, Debug)]
pub enum WorktreeAction {
    /// 创建 git worktree（默认 .deepseeknova/worktrees/`<name>`）用于隔离会话。
    New {
        /// Worktree 名（缺省 wt-<时间戳>-<序号>；同时作为新分支名）。
        #[arg(long)]
        name: Option<String>,
        /// 基础 ref（分支/tag/commit，缺省 HEAD）。
        #[arg(long)]
        base: Option<String>,
    },
    /// 列出全部 git worktree（路径 / 分支 / 当前标记 / \[cli\] 标记）。
    List,
    /// 打印指定 worktree 的目录供 cd 进入（CLI 不能改变父进程 cwd）。
    Switch {
        /// Worktree 名。
        name: String,
    },
    /// 删除 worktree（有未提交变更时拒绝；--force 强制丢弃变更删除）。
    Delete {
        /// Worktree 名。
        name: String,
        /// 有未提交/未跟踪变更时仍强制删除（丢弃变更）。
        #[arg(long)]
        force: bool,
    },
    /// 清理所有由本 CLI 创建的 worktree（有未提交变更的跳过）。
    Clean,
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

    #[test]
    fn init_without_legacy_flag_defaults_to_agents_md() {
        let parsed = Cli::try_parse_from(["deepseeknova", "init"]).unwrap();
        match parsed.command {
            Some(Commands::Init { legacy }) => {
                assert!(!legacy, "default init must target AGENTS.md")
            }
            _ => panic!("expected Init command"),
        }
    }

    #[test]
    fn init_legacy_flag_parses() {
        let parsed = Cli::try_parse_from(["deepseeknova", "init", "--legacy"]).unwrap();
        match parsed.command {
            Some(Commands::Init { legacy }) => assert!(legacy, "--legacy must set legacy=true"),
            _ => panic!("expected Init command"),
        }
    }

    #[test]
    fn eval_require_dimension_parses_and_validates() {
        // 英文名 + 中文别名均可解析。
        let parsed = Cli::try_parse_from([
            "deepseeknova",
            "eval",
            "--require-dimension",
            "governance>=0.9",
            "--require-dimension",
            "协议>=0.8",
            "--require-min-score",
            "3.5",
        ])
        .unwrap();
        match parsed.command {
            Some(Commands::Eval {
                require_min_score,
                require_dimension,
                ..
            }) => {
                assert_eq!(require_min_score, Some(3.5));
                assert_eq!(
                    require_dimension,
                    vec![("governance".to_string(), 0.9), ("协议".to_string(), 0.8)]
                );
            }
            _ => panic!("expected Eval command"),
        }
        // 非法格式 / 未知维度 / 非法阈值 → 解析失败。
        assert!(
            Cli::try_parse_from(["deepseeknova", "eval", "--require-dimension", "governance"])
                .is_err()
        );
        assert!(
            Cli::try_parse_from(["deepseeknova", "eval", "--require-dimension", "nope>=0.5"])
                .is_err()
        );
        assert!(Cli::try_parse_from([
            "deepseeknova",
            "eval",
            "--require-dimension",
            "governance>=abc"
        ])
        .is_err());
    }

    // ── memory 用户面（P1-11）解析 ─────────────────────────────────────

    #[test]
    fn memory_list_parses_filters() {
        let parsed = Cli::try_parse_from([
            "deepseeknova",
            "memory",
            "list",
            "--category",
            "all",
            "--stage",
            "verified",
            "--tag",
            "rust",
            "--search",
            "borrow",
            "--limit",
            "5",
            "--offset",
            "10",
        ])
        .unwrap();
        match parsed.command {
            Some(Commands::Memory { action }) => match action {
                MemoryAction::List {
                    category,
                    limit,
                    offset,
                    stage,
                    tag,
                    search,
                } => {
                    assert_eq!(category, "all");
                    assert_eq!(limit, 5);
                    assert_eq!(offset, 10);
                    assert_eq!(stage.as_deref(), Some("verified"));
                    assert_eq!(tag.as_deref(), Some("rust"));
                    assert_eq!(search.as_deref(), Some("borrow"));
                }
                other => panic!("expected List, got {other:?}"),
            },
            _ => panic!("expected Memory command"),
        }
        // 无过滤参数时全部为默认。
        let bare = Cli::try_parse_from(["deepseeknova", "memory", "list"]).unwrap();
        match bare.command {
            Some(Commands::Memory {
                action:
                    MemoryAction::List {
                        category,
                        limit,
                        offset,
                        stage,
                        tag,
                        search,
                    },
            }) => {
                assert_eq!(category, "task");
                assert_eq!(limit, 20);
                assert_eq!(offset, 0);
                assert!(stage.is_none() && tag.is_none() && search.is_none());
            }
            _ => panic!("expected List defaults"),
        }
    }

    #[test]
    fn memory_edit_parses_id_and_content() {
        let parsed =
            Cli::try_parse_from(["deepseeknova", "memory", "edit", "k", "new", "content"]).unwrap();
        match parsed.command {
            Some(Commands::Memory {
                action: MemoryAction::Edit { id, content },
            }) => {
                assert_eq!(id, "k");
                assert_eq!(content, vec!["new", "content"]);
            }
            _ => panic!("expected Edit"),
        }
        // 缺 id 解析失败。
        assert!(Cli::try_parse_from(["deepseeknova", "memory", "edit"]).is_err());
    }

    #[test]
    fn memory_delete_parses_id_and_yes() {
        let parsed =
            Cli::try_parse_from(["deepseeknova", "memory", "delete", "k", "--yes"]).unwrap();
        match parsed.command {
            Some(Commands::Memory {
                action: MemoryAction::Delete { id, yes },
            }) => {
                assert_eq!(id, "k");
                assert!(yes, "--yes 必须置位");
            }
            _ => panic!("expected Delete"),
        }
        let no_flag = Cli::try_parse_from(["deepseeknova", "memory", "delete", "k"]).unwrap();
        match no_flag.command {
            Some(Commands::Memory {
                action: MemoryAction::Delete { yes, .. },
            }) => {
                assert!(!yes, "缺 --yes 默认需二次确认");
            }
            _ => panic!("expected Delete"),
        }
    }

    #[test]
    fn memory_replay_parses_query_and_top_k() {
        let parsed =
            Cli::try_parse_from(["deepseeknova", "memory", "replay", "rust", "--top-k", "5"])
                .unwrap();
        match parsed.command {
            Some(Commands::Memory {
                action: MemoryAction::Replay { query, top_k },
            }) => {
                assert_eq!(query, vec!["rust"]);
                assert_eq!(top_k, Some(5));
            }
            _ => panic!("expected Replay"),
        }
        let bare = Cli::try_parse_from(["deepseeknova", "memory", "replay", "rust"]).unwrap();
        match bare.command {
            Some(Commands::Memory {
                action: MemoryAction::Replay { top_k, .. },
            }) => {
                assert!(top_k.is_none(), "缺 --top-k 时由调用方取默认 10");
            }
            _ => panic!("expected Replay"),
        }
    }

    // ── audit 子命令（exec 审计，P1-7）解析 ───────────────────────────

    #[test]
    fn audit_parses_shell_args_and_format() {
        // 审计参数须置于命令之前；`--format` 先解析，之后全部为命令。
        let parsed =
            Cli::try_parse_from(["deepseeknova", "audit", "--format", "json", "git", "status"])
                .unwrap();
        match parsed.command {
            Some(Commands::Audit {
                args,
                format,
                rules,
                workspace,
            }) => {
                assert_eq!(args, vec!["git", "status"]);
                assert_eq!(format, "json");
                assert!(!rules);
                assert!(workspace.is_none());
            }
            _ => panic!("expected Audit command"),
        }
    }

    #[test]
    fn audit_hyphen_command_args_stay_in_command() {
        // 命令内以 `-` 开头的 flag 并入被审计命令（`ls -la` 整条审计）。
        let parsed = Cli::try_parse_from(["deepseeknova", "audit", "ls", "-la"]).unwrap();
        match parsed.command {
            Some(Commands::Audit { args, .. }) => {
                assert_eq!(args, vec!["ls", "-la"]);
            }
            _ => panic!("expected Audit command"),
        }
    }

    #[test]
    fn audit_rules_flag_parses_without_args() {
        let parsed = Cli::try_parse_from(["deepseeknova", "audit", "--rules"]).unwrap();
        match parsed.command {
            Some(Commands::Audit { args, rules, .. }) => {
                assert!(args.is_empty());
                assert!(rules, "--rules 必须置位");
            }
            _ => panic!("expected Audit command"),
        }
    }

    #[test]
    fn audit_workspace_flag_parses() {
        let parsed = Cli::try_parse_from([
            "deepseeknova",
            "audit",
            "--workspace",
            "/tmp/ws",
            "ls",
            "-la",
        ])
        .unwrap();
        match parsed.command {
            Some(Commands::Audit {
                workspace, args, ..
            }) => {
                assert_eq!(workspace.as_deref(), Some("/tmp/ws"));
                assert_eq!(args, vec!["ls", "-la"]);
            }
            _ => panic!("expected Audit command"),
        }
    }

    // ── worktree 子命令（并行会话隔离，P2-7）解析 ──────────────────────

    #[test]
    fn worktree_new_parses_name_and_base() {
        let parsed = Cli::try_parse_from([
            "deepseeknova",
            "worktree",
            "new",
            "--name",
            "feat",
            "--base",
            "main",
        ])
        .unwrap();
        match parsed.command {
            Some(Commands::Worktree { action }) => match action {
                WorktreeAction::New { name, base } => {
                    assert_eq!(name.as_deref(), Some("feat"));
                    assert_eq!(base.as_deref(), Some("main"));
                }
                other => panic!("expected New, got {other:?}"),
            },
            _ => panic!("expected Worktree command"),
        }
        // 无参数时 name/base 均为 None（由实现取缺省）。
        let bare = Cli::try_parse_from(["deepseeknova", "worktree", "new"]).unwrap();
        match bare.command {
            Some(Commands::Worktree {
                action: WorktreeAction::New { name, base },
            }) => {
                assert!(name.is_none() && base.is_none());
            }
            _ => panic!("expected New defaults"),
        }
    }

    #[test]
    fn worktree_delete_parses_name_and_force() {
        let parsed =
            Cli::try_parse_from(["deepseeknova", "worktree", "delete", "feat", "--force"]).unwrap();
        match parsed.command {
            Some(Commands::Worktree {
                action: WorktreeAction::Delete { name, force },
            }) => {
                assert_eq!(name, "feat");
                assert!(force, "--force 必须置位");
            }
            _ => panic!("expected Delete"),
        }
        let no_flag = Cli::try_parse_from(["deepseeknova", "worktree", "delete", "feat"]).unwrap();
        match no_flag.command {
            Some(Commands::Worktree {
                action: WorktreeAction::Delete { force, .. },
            }) => {
                assert!(!force, "缺 --force 默认拒绝有变更的 worktree");
            }
            _ => panic!("expected Delete"),
        }
    }

    #[test]
    fn worktree_list_switch_clean_parse() {
        for argv in [
            vec!["deepseeknova", "worktree", "list"],
            vec!["deepseeknova", "worktree", "clean"],
        ] {
            let parsed = Cli::try_parse_from(argv).unwrap();
            match parsed.command {
                Some(Commands::Worktree { action }) => {
                    assert!(matches!(
                        action,
                        WorktreeAction::List | WorktreeAction::Clean
                    ));
                }
                _ => panic!("expected Worktree command"),
            }
        }
        let parsed = Cli::try_parse_from(["deepseeknova", "worktree", "switch", "feat"]).unwrap();
        match parsed.command {
            Some(Commands::Worktree {
                action: WorktreeAction::Switch { name },
            }) => {
                assert_eq!(name, "feat");
            }
            _ => panic!("expected Switch"),
        }
    }
}

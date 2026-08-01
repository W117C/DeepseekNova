mod chat;
mod cli;
mod init;
mod setup;

use clap::Parser;
use cli::{Cli, Commands};
use deepseeknova_agent::{CoordinatorRunner, PlanModeRunner};
use deepseeknova_core::planner::SimplePlanner;
use deepseeknova_core::runner::{RunInput, Runner};
use std::sync::Arc;
use tokio_stream::StreamExt;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Config is loaded before any subscriber exists, so load-time diagnostics
    // go to stderr directly.
    let config = deepseeknova_config::Config::load().unwrap_or_else(|e| {
        eprintln!("warning: failed to load config, using defaults: {e}");
        deepseeknova_config::Config::default()
    });

    // Role-pointer routing + cost accounting. The router owns its ledger
    // (retrievable via `router.ledger()`), so no separate binding is needed.
    let model_router = Arc::new(
        deepseeknova_provider::router::ModelRouter::from_config(
            &config,
            Arc::new(deepseeknova_provider::cost::CostLedger::new()),
        )
        .unwrap_or_else(|e| {
            eprintln!("config error: {e}");
            std::process::exit(2);
        }),
    );

    // Tracing backend — exactly one of the two is installed. The telemetry
    // guard's registry carries no fmt layer, so terminal log output is
    // suppressed while OTLP export is active (known trade-off); it must be
    // held in a named binding so spans flush on exit.
    let _telemetry_guard = if config.telemetry.enabled {
        Some(deepseeknova_telemetry::TelemetryGuard::init(
            "deepseeknova",
            config.telemetry.otlp_endpoint.as_deref(),
        )?)
    } else {
        let subscriber = FmtSubscriber::builder()
            .with_max_level(Level::INFO)
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .expect("setting default subscriber failed");
        None
    };

    match &cli.command {
        // ── Run (single-model or coordinator) ─────────────────────────────
        Some(Commands::Run {
            model: model_args,
            coordinator,
            prompt,
        }) => {
            let prompt_str = prompt.join(" ");
            info!(
                "run: model={:?}, max_steps={}, planner_model={:?}, prompt={prompt_str}",
                model_args.model, model_args.max_steps, coordinator.planner_model,
            );

            if let Some(ref planner_model) = coordinator.planner_model {
                // ── Coordinator mode ──────────────────────────────────────
                use deepseeknova_provider::cost::ModelRole;
                let planner_provider =
                    model_router.provider_for_model(planner_model, ModelRole::Main, None)?;
                let executor_model = coordinator
                    .executor_model
                    .clone()
                    .or_else(|| model_args.model.clone());
                let executor_provider = model_router.provider_for_maybe_model(
                    ModelRole::Task,
                    executor_model.as_deref(),
                    Some(deepseeknova_provider::factory::ReasoningEffort::High),
                )?;
                let max_nodes = coordinator.max_graph_nodes;

                let workspace_root = std::env::current_dir().unwrap_or_default();
                let security =
                    deepseeknova_runtime::build_security_context(&config, &workspace_root)?;
                let mut runner = CoordinatorRunner::new(planner_provider, executor_provider)
                    .with_max_graph_nodes(max_nodes)
                    .with_workspace_root(workspace_root.clone())
                    .with_security(security);
                if let Some(gate) =
                    deepseeknova_runtime::permission_gate_for(&config, &workspace_root)
                {
                    runner = runner.with_permission_gate(gate);
                }

                // Delegate 动作路由：子代理走 task 指针，压缩走 compact 指针。
                // 压缩是机械摘要，按 Disabled 分类省掉 reasoning tokens。
                if config.delegate.enabled {
                    use deepseeknova_provider::factory::ReasoningEffort;
                    let task_provider =
                        model_router.provider_for(ModelRole::Task, Some(ReasoningEffort::High))?;
                    let compact_provider = model_router
                        .provider_for(ModelRole::Compact, Some(ReasoningEffort::Disabled))?;
                    runner =
                        runner.with_sub_agent_runner(deepseeknova_runtime::build_sub_agent_runner(
                            &config,
                            task_provider,
                            Some(compact_provider),
                        ));
                }

                // P4.1 Coordinator 图索引接入：注入 GraphHandle，图工具不再排除；
                // 只读工具对规划器开放（安全边界：规划器只能调用只读工具）。
                if config.graph.enabled {
                    match deepseeknova_graph::GraphIndex::open(
                        &workspace_root,
                        config.graph.max_file_size,
                    ) {
                        Ok(index) => {
                            let handle: deepseeknova_tools::GraphHandle =
                                Arc::new(std::sync::Mutex::new(index));
                            let bg = handle.clone();
                            tokio::task::spawn_blocking(move || {
                                if let Ok(mut idx) = bg.lock() {
                                    if let Err(e) = idx.refresh() {
                                        tracing::warn!("graph index refresh failed: {e}");
                                    }
                                } else {
                                    tracing::warn!("graph index lock poisoned during refresh");
                                }
                            });
                            runner = runner.with_extension(handle);
                        }
                        Err(e) => {
                            tracing::warn!("graph index unavailable, tools will degrade: {e}")
                        }
                    }
                }
                let graph_tools = ["search_code", "traverse_graph", "retrieve_entity"];
                for tool in deepseeknova_tools::all_builtin_tools() {
                    if !config.graph.enabled && graph_tools.contains(&tool.schema().name.as_str()) {
                        continue;
                    }
                    if tool.read_only() {
                        runner.register_read_only_tool(tool);
                    } else {
                        runner.register_tool(tool);
                    }
                }

                // MCP 工具：与单 Agent 路径一致，从 config.mcp_servers 发现并注册到
                // 执行器 Runner（子代理按设计不接 MCP）。graph 工具仍受上面的排除
                // 限制，待 coordinator graph wiring 落地后再补。
                for tool in deepseeknova_runtime::discover_mcp_tools(&config).await {
                    runner.register_tool(tool);
                }

                let input = RunInput {
                    prompt: prompt_str,
                    images: Vec::new(),
                    model_override: model_args.model.clone(),
                };
                stream_coordinator(&runner, input).await?;
            } else {
                // ── Single-agent mode ─────────────────────────────────────
                use deepseeknova_provider::cost::ModelRole;
                let provider = model_router.provider_for_maybe_model(
                    ModelRole::Main,
                    model_args.model.as_deref(),
                    None,
                )?;
                let task_provider = model_router.provider_for(ModelRole::Task, None)?;
                let mcp_tools = deepseeknova_runtime::discover_mcp_tools(&config).await;
                let (step_quick, step_high) =
                    step_effort_providers(&model_router, &config, model_args.model.as_deref())?;
                let mut roles = deepseeknova_runtime::AgentRoleProviders::default();
                roles.task = Some(task_provider);
                roles.compact = Some(compact_provider_for(&model_router, &config)?);
                roles.review = review_provider_for(&model_router, &config)?;
                roles.step_quick = step_quick;
                roles.step_high = step_high;
                let agent = build_agent(
                    Arc::clone(&provider),
                    roles,
                    model_args.model.as_deref(),
                    &config,
                    model_args.max_steps,
                    mcp_tools,
                )?;

                let input = RunInput {
                    prompt: prompt_str,
                    images: Vec::new(),
                    model_override: model_args.model.clone(),
                };
                stream_events(&agent, input).await?;
            }
        }

        // ── Plan ─────────────────────────────────────────────────────────
        Some(Commands::Plan {
            model,
            coordinator,
            prompt,
        }) => {
            let prompt_str = prompt.join(" ");
            info!("plan: model={model:?}, prompt={prompt_str}");

            use deepseeknova_provider::cost::ModelRole;
            let provider =
                model_router.provider_for_maybe_model(ModelRole::Main, model.as_deref(), None)?;
            let mut plan_runner = PlanModeRunner::new(provider);

            // When coordinator flags are present, attach a Planner so the
            // output includes a structured ExecutionGraph.
            if coordinator.planner_model.is_some() {
                plan_runner = plan_runner.with_planner(Arc::new(SimplePlanner));
            }

            let input = RunInput {
                prompt: prompt_str,
                images: Vec::new(),
                model_override: model.clone(),
            };
            stream_events(&plan_runner, input).await?;
        }

        // ── Scan (matcher + optional AI investigation) ───────────────────
        Some(Commands::Scan {
            path,
            format,
            no_ai,
            severity_min,
        }) => {
            use deepseeknova_provider::cost::ModelRole;
            let workspace_root = std::env::current_dir().unwrap_or_default();
            let raw_root = std::path::PathBuf::from(path.as_deref().unwrap_or("."));
            // 逃逸企图（`..`/绝对路径/symlink）直接中止（fail-closed）；
            // 仅路径不存在等非安全失败回落归一化路径（扫描结果为空）。
            let root = resolve_scan_root(&workspace_root, &raw_root)?;
            let min = deepseeknova_scanner::rule::Severity::parse(severity_min)
                .unwrap_or(deepseeknova_scanner::rule::Severity::Low);

            info!(
                "scan: path={}, format={format}, no_ai={no_ai}",
                root.display()
            );
            let rules = deepseeknova_scanner::rule::builtin_rules();
            let mut findings = deepseeknova_scanner::scan::scan_files(&root, &rules)?;
            // severity 过滤：min 为下限，保留 severity <= min（High 序最小）。
            findings.retain(|f| f.severity <= min);

            if !*no_ai && !findings.is_empty() {
                let mcp_tools = deepseeknova_runtime::discover_mcp_tools(&config).await;
                let provider = model_router
                    .provider_for(ModelRole::Task, None)
                    .map_err(|e| anyhow::anyhow!("{e}（可用 --no-ai 跳过 AI 调查）"))?;
                // agent 一次性构建，跨 findings 复用（Agent::run_stream 每次调用克隆状态）。
                let agent = build_agent(
                    Arc::clone(&provider),
                    deepseeknova_runtime::AgentRoleProviders::default(),
                    None,
                    &config,
                    5,
                    mcp_tools,
                )?;
                for f in &mut findings {
                    f.verdict = deepseeknova_scanner::investigate::investigate(f, &agent).await;
                }
            }

            let report = deepseeknova_scanner::report::ScanReport::new(findings);
            match format.as_str() {
                "json" => println!("{}", report.to_json()?),
                _ => println!("{}", report.to_markdown()),
            }
        }

        // ── Chat (with /new loop) ────────────────────────────────────────
        Some(Commands::Chat { model, resume, tui }) => {
            info!("chat: model={model:?}, resume={resume}, tui={tui}");
            // Compute the baseline reasoning effort from config so the
            // REPL knows what to restore when toggling thinking back on.
            let provider_cfg = resolve_provider_cfg(&config, model.as_deref());
            let baseline_effort =
                deepseeknova_provider::factory::resolve_effort(provider_cfg, None);

            // Discover MCP tools once — reused across model/effort rebuilds so
            // `/model` switching never re-spawns MCP server processes.
            let mcp_tools = deepseeknova_runtime::discover_mcp_tools(&config).await;

            // Full-screen TUI: build one agent (with a fresh shared history so
            // multi-turn context works) and hand it to the TUI runner.
            // `/model` 热切换通过 agent 工厂重建，`/cost` 走 model router。
            if *tui {
                use deepseeknova_provider::cost::ModelRole;
                use deepseeknova_provider::factory::ReasoningEffort;
                let history: Arc<tokio::sync::Mutex<Vec<deepseeknova_core::Message>>> =
                    Arc::new(tokio::sync::Mutex::new(Vec::new()));
                let factory_router = Arc::clone(&model_router);
                let cfg = config.clone();
                let hist = history.clone();
                let mcp = mcp_tools;
                let factory = move |effort: Option<ReasoningEffort>,
                                    model: Option<String>|
                      -> anyhow::Result<
                    Arc<dyn deepseeknova_core::runner::Runner>,
                > {
                    let provider = factory_router.provider_for_maybe_model(
                        ModelRole::Main,
                        model.as_deref(),
                        effort,
                    )?;
                    let task_provider = factory_router.provider_for(ModelRole::Task, effort)?;
                    let mut roles = deepseeknova_runtime::AgentRoleProviders::default();
                    roles.task = Some(task_provider);
                    roles.compact = Some(compact_provider_for(&factory_router, &cfg)?);
                    roles.review = review_provider_for(&factory_router, &cfg)?;
                    let agent =
                        build_agent(provider, roles, model.as_deref(), &cfg, 0, mcp.clone())?
                            .with_conversation_history(hist.clone());
                    Ok(Arc::new(agent))
                };
                let initial = factory(Some(baseline_effort), model.clone())?;
                let mut tui = deepseeknova_tui::TuiRunner::new(initial)
                    .with_model_label(model.as_deref().unwrap_or("default"))
                    .with_agent_factory(factory)
                    .with_model_router(Arc::clone(&model_router))
                    .with_baseline_effort(baseline_effort)
                    .with_current_model(model.clone());
                tui.run().await?;
                return Ok(());
            }

            let sessions_root = sessions_root(&config);
            // Resume applies only to the first `/new` cycle.
            let mut resume_next = *resume;

            loop {
                // Persistent session memory — shared across model/effort
                // rebuilds within the same `/new` session.
                let history: Arc<tokio::sync::Mutex<Vec<deepseeknova_core::Message>>> =
                    Arc::new(tokio::sync::Mutex::new(Vec::new()));

                let persist = build_chat_persistence(
                    sessions_root.clone(),
                    Arc::clone(&history),
                    resume_next,
                )
                .await;
                resume_next = false;

                let history_clone = Arc::clone(&history);
                let cfg = &config;
                let mcp_tools = mcp_tools.clone();
                let router = Arc::clone(&model_router);
                let agent_factory =
                    move |effort: Option<deepseeknova_provider::factory::ReasoningEffort>,
                          model_name: Option<String>|
                          -> anyhow::Result<Box<dyn Runner + Send>> {
                        use deepseeknova_provider::cost::ModelRole;
                        // `/model switch <name>` 显式覆盖，仍按 Main 角色计量。
                        let provider = router.provider_for_maybe_model(
                            ModelRole::Main,
                            model_name.as_deref(),
                            effort,
                        )?;
                        let task_provider = router.provider_for(ModelRole::Task, effort)?;
                        let (step_quick, step_high) =
                            step_effort_providers(&router, cfg, model_name.as_deref())?;
                        let mut roles = deepseeknova_runtime::AgentRoleProviders::default();
                        roles.task = Some(task_provider);
                        roles.compact = Some(compact_provider_for(&router, cfg)?);
                        roles.review = review_provider_for(&router, cfg)?;
                        roles.step_quick = step_quick;
                        roles.step_high = step_high;
                        let agent = build_agent(
                            provider,
                            roles,
                            model_name.as_deref(),
                            cfg,
                            0, // no max_steps limit in chat mode
                            mcp_tools.clone(),
                        )?
                        .with_conversation_history(Arc::clone(&history_clone));
                        Ok(Box::new(agent))
                    };

                let restart = chat::run_chat_repl(
                    agent_factory,
                    baseline_effort,
                    model.clone(),
                    persist,
                    Some(Arc::clone(&model_router)),
                )
                .await?;
                if !restart {
                    break;
                }
                // Drop & recreate history for the new session.
                drop(history);
                info!("restarting chat session...");
            }
        }

        Some(Commands::Serve { addr }) => {
            info!("serve command: addr={addr}");

            use deepseeknova_provider::cost::ModelRole;
            let provider = model_router.provider_for(ModelRole::Main, None)?;
            let task_provider = model_router.provider_for(ModelRole::Task, None)?;
            let mcp_tools = deepseeknova_runtime::discover_mcp_tools(&config).await;
            // Share a pending-approvals map between the agent's approval
            // responder and the server's POST /v1/approval route so the gate's
            // `Ask` decisions can be answered over HTTP.
            let pending = deepseeknova_serve::new_pending_approvals();
            let responder: Arc<dyn deepseeknova_core::runner::ApprovalResponder> = Arc::new(
                deepseeknova_serve::ServerApprovalResponder::new(pending.clone()),
            );
            let mut roles = deepseeknova_runtime::AgentRoleProviders::default();
            roles.task = Some(task_provider);
            roles.compact = Some(compact_provider_for(&model_router, &config)?);
            roles.review = review_provider_for(&model_router, &config)?;
            let agent = build_agent(Arc::clone(&provider), roles, None, &config, 0, mcp_tools)?
                .with_approval_responder(responder);
            let runner: Arc<dyn Runner> = Arc::new(agent);

            let server = deepseeknova_serve::Server::with_pending(runner, pending);
            server.serve(addr).await?;
        }

        Some(Commands::Setup { local }) => {
            info!("setup: local={local}");
            setup::run_setup_wizard(*local).await?;
        }

        Some(Commands::Config) => {
            println!("{:#?}", config);
        }

        // ── Memory (审查/检索/删除/统计) ─────────────────────────────────
        Some(Commands::Memory { action }) => {
            use deepseeknova_core::memory::store::MemoryCategory;
            let db = std::env::current_dir()
                .unwrap_or_default()
                .join(&config.memory.db_path);
            let engine = deepseeknova_core::memory::engine::MemoryEngine::open(
                &db,
                config.memory.redact_secrets,
            )?;
            match action {
                cli::MemoryAction::List { category, limit } => {
                    let cat = match category.as_str() {
                        "skill" => MemoryCategory::Skill,
                        "user_profile" => MemoryCategory::UserProfile,
                        _ => MemoryCategory::Task,
                    };
                    for e in engine.list(cat)?.into_iter().take(*limit) {
                        let preview: String = e.content.chars().take(100).collect();
                        println!("[{}] ({}) {}", e.id, e.source, preview);
                    }
                }
                cli::MemoryAction::Search { query } => {
                    let q = query.join(" ");
                    for (i, r) in engine.recall(&q, 10)?.iter().enumerate() {
                        let preview: String = r.entry.content.chars().take(120).collect();
                        println!("{}. [{}] {}", i + 1, r.entry.id, preview);
                    }
                }
                cli::MemoryAction::Forget { id } => {
                    println!(
                        "{}",
                        if engine.forget(id)? {
                            "removed"
                        } else {
                            "not found"
                        }
                    );
                }
                cli::MemoryAction::Stats => {
                    let s = engine.stats()?;
                    println!(
                        "total={} recall_hit_rate={:.2} reinforce_ratio={:.2}",
                        s.total, s.recall_hit_rate, s.reinforce_ratio
                    );
                }
            }
        }

        // ── Checkpoint（写前快照 + 回滚，A1）────────────────────────────
        Some(Commands::Checkpoint { action }) => {
            use deepseeknova_checkpoint::CheckpointManager;
            let workspace_root = std::env::current_dir().unwrap_or_default();
            let path = workspace_root.join(&config.checkpoint.path);
            match action {
                cli::CheckpointAction::List => {
                    let ck = CheckpointManager::load_from(&path)?;
                    if ck.is_empty() {
                        println!("no checkpoints (path: {})", path.display());
                    }
                    for (snap, clean) in ck.verify().await? {
                        let status = if clean { "unchanged" } else { "modified" };
                        println!(
                            "{} [{}] {} ({})",
                            if clean { "✓" } else { "✗" },
                            status,
                            snap.path.display(),
                            &snap.hash[..8.min(snap.hash.len())]
                        );
                    }
                }
                cli::CheckpointAction::Rollback { all } => {
                    let mut ck = CheckpointManager::load_from(&path)?;
                    if *all {
                        let n = ck.rollback_all().await?;
                        println!("rolled back {n} snapshot(s)");
                    } else if let Some((p, h)) = ck.rollback().await? {
                        println!(
                            "rolled back {} (hash {})",
                            p.display(),
                            &h[..8.min(h.len())]
                        );
                    } else {
                        println!("no checkpoints to roll back");
                    }
                }
                cli::CheckpointAction::Clear => {
                    let mut ck = CheckpointManager::load_from(&path)?;
                    let n = ck.len();
                    ck.clear();
                    println!("cleared {n} snapshot(s)");
                }
            }
        }

        // ── Artifacts（项目后置产出，A2）────────────────────────────────
        Some(Commands::Artifacts { action }) => {
            use deepseeknova_core::artifacts::cards::{CardGenerator, KnowledgeCard};
            use deepseeknova_core::artifacts::wiki::{ProjectSummary, WikiConfig, WikiGenerator};
            match action {
                cli::ArtifactsAction::Wiki {
                    out,
                    project,
                    summary,
                } => {
                    let name = project.clone().unwrap_or_else(|| {
                        std::env::current_dir()
                            .ok()
                            .and_then(|d| d.file_name().map(|s| s.to_string_lossy().into_owned()))
                            .unwrap_or_else(|| "project".to_string())
                    });
                    let mut gen = WikiGenerator::new(WikiConfig {
                        output_dir: std::path::PathBuf::from(out),
                        ..Default::default()
                    });
                    gen.add_home_page(&ProjectSummary {
                        name,
                        description: summary.clone().unwrap_or_default(),
                        modules: vec![],
                        key_decisions: vec![],
                        metrics: vec![],
                    });
                    for p in gen.generate()? {
                        println!("{}", p.display());
                    }
                }
                cli::ArtifactsAction::Cards {
                    out,
                    title,
                    insight,
                    tags,
                    source,
                } => {
                    let mut gen = CardGenerator::new(out);
                    gen.add_card(KnowledgeCard {
                        id: format!("card-{}", chrono::Utc::now().timestamp()),
                        title: title.clone(),
                        tags: tags.clone(),
                        created: chrono::Utc::now().format("%Y-%m-%d").to_string(),
                        source: source.clone().unwrap_or_else(|| "cli".to_string()),
                        context: String::new(),
                        key_insight: insight.clone(),
                        code_example: None,
                        related: vec![],
                    });
                    for p in gen.generate()? {
                        println!("{}", p.display());
                    }
                }
            }
        }

        Some(Commands::Init) => {
            info!("init command");
            init::run_init().await?;
        }

        None => {
            info!("no command provided — starting interactive chat");
            // Resolve baseline effort from the default provider config.
            let provider_cfg = resolve_provider_cfg(&config, None);
            let baseline_effort =
                deepseeknova_provider::factory::resolve_effort(provider_cfg, None);

            // Discover MCP tools once (see Chat branch for rationale).
            let mcp_tools = deepseeknova_runtime::discover_mcp_tools(&config).await;
            let sessions_root = sessions_root(&config);

            loop {
                let history: Arc<tokio::sync::Mutex<Vec<deepseeknova_core::Message>>> =
                    Arc::new(tokio::sync::Mutex::new(Vec::new()));

                let persist =
                    build_chat_persistence(sessions_root.clone(), Arc::clone(&history), false)
                        .await;

                let history_clone = Arc::clone(&history);
                let cfg = &config;
                let mcp_tools = mcp_tools.clone();
                let router = Arc::clone(&model_router);
                let agent_factory =
                    move |effort: Option<deepseeknova_provider::factory::ReasoningEffort>,
                          model_name: Option<String>|
                          -> anyhow::Result<Box<dyn Runner + Send>> {
                        use deepseeknova_provider::cost::ModelRole;
                        // `/model switch <name>` 显式覆盖，仍按 Main 角色计量。
                        let provider = router.provider_for_maybe_model(
                            ModelRole::Main,
                            model_name.as_deref(),
                            effort,
                        )?;
                        let task_provider = router.provider_for(ModelRole::Task, effort)?;
                        let mut roles = deepseeknova_runtime::AgentRoleProviders::default();
                        roles.task = Some(task_provider);
                        roles.compact = Some(compact_provider_for(&router, cfg)?);
                        roles.review = review_provider_for(&router, cfg)?;
                        let agent = build_agent(
                            provider,
                            roles,
                            model_name.as_deref(),
                            cfg,
                            0,
                            mcp_tools.clone(),
                        )?
                        .with_conversation_history(Arc::clone(&history_clone));
                        Ok(Box::new(agent))
                    };

                let restart = chat::run_chat_repl(
                    agent_factory,
                    baseline_effort,
                    None,
                    persist,
                    Some(Arc::clone(&model_router)),
                )
                .await?;
                if !restart {
                    break;
                }
                drop(history);
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve a ProviderConfig for a given model name (or the default).
fn resolve_provider_cfg<'a>(
    config: &'a deepseeknova_config::Config,
    model: Option<&str>,
) -> &'a deepseeknova_config::ProviderConfig {
    if let Some(model_name) = model {
        config
            .resolve_provider_for_model(model_name)
            .unwrap_or_else(|| &config.providers[0])
    } else {
        &config.providers[0]
    }
}

/// Compact 覆盖模型判定：指针优先；指针未设而 B2 的 agent.compact_model
/// 非空时，以该模型为显式覆盖（经 router 构建，照样计量）。
fn compact_override_model(config: &deepseeknova_config::Config) -> Option<&str> {
    if config.model_pointers.compact.is_none() && !config.agent.compact_model.is_empty() {
        Some(config.agent.compact_model.as_str())
    } else {
        None
    }
}

/// Compact 角色 provider（Agent L3 压缩用）。L3 摘要是机械任务，按 Disabled
/// 分类省 reasoning tokens（与 coordinator compact 决策一致）。
fn compact_provider_for(
    router: &deepseeknova_provider::router::ModelRouter,
    config: &deepseeknova_config::Config,
) -> anyhow::Result<Arc<dyn deepseeknova_provider::Provider>> {
    router.provider_for_maybe_model(
        deepseeknova_provider::cost::ModelRole::Compact,
        compact_override_model(config),
        Some(deepseeknova_provider::factory::ReasoningEffort::Disabled),
    )
}

/// P2.1 每步 effort 路由的 quick/high provider（未启用时返回 (None, None)）。
type EffortProviderPair = (
    Option<Arc<dyn deepseeknova_provider::Provider>>,
    Option<Arc<dyn deepseeknova_provider::Provider>>,
);

fn step_effort_providers(
    router: &deepseeknova_provider::router::ModelRouter,
    config: &deepseeknova_config::Config,
    model: Option<&str>,
) -> anyhow::Result<EffortProviderPair> {
    use deepseeknova_provider::cost::ModelRole;
    use deepseeknova_provider::factory::ReasoningEffort;
    if !config.agent.step_effort_routing {
        return Ok((None, None));
    }
    let quick =
        router.provider_for_maybe_model(ModelRole::Main, model, Some(ReasoningEffort::Disabled))?;
    let high =
        router.provider_for_maybe_model(ModelRole::Main, model, Some(ReasoningEffort::High))?;
    Ok((Some(quick), Some(high)))
}

/// Review 角色 provider（B3 完成前自审门禁用）。review 关闭时不构建（避免
/// 无关路径因 quick 指针的 API key 缺失阻断构建）；review_model 非空时按名
/// 经 router 构建（照样计量）；空则走 quick 指针（门禁属快速操作类，未设时
/// 回落 main 指针）。评审判定受益于 reasoning，不强制降档 effort。
fn review_provider_for(
    router: &deepseeknova_provider::router::ModelRouter,
    config: &deepseeknova_config::Config,
) -> anyhow::Result<Option<Arc<dyn deepseeknova_provider::Provider>>> {
    if !config.review.enabled {
        return Ok(None);
    }
    let override_model = if config.review.review_model.is_empty() {
        None
    } else {
        Some(config.review.review_model.as_str())
    };
    Ok(Some(router.provider_for_maybe_model(
        deepseeknova_provider::cost::ModelRole::Quick,
        override_model,
        None,
    )?))
}

/// Build an agent with built-in tools registered, plus any `extra_tools`
/// (e.g. MCP tools discovered via [`deepseeknova_runtime::discover_mcp_tools`]).
/// `roles` routes the delegation engine (sub-agents) to the Task-role model
/// and Agent L3 compaction to the Compact-role model when supplied; unset
/// roles fall back to the main provider.
fn build_agent(
    provider: Arc<dyn deepseeknova_provider::Provider>,
    roles: deepseeknova_runtime::AgentRoleProviders,
    _model: Option<&str>,
    config: &deepseeknova_config::Config,
    max_steps: usize,
    extra_tools: Vec<Arc<dyn deepseeknova_core::Tool>>,
) -> anyhow::Result<deepseeknova_agent::Agent> {
    let workspace_root = std::env::current_dir().unwrap_or_default();
    // Delegate to the shared runtime builder (security + sandbox + permission
    // gate wiring lives in one place). CLI is non-interactive, so no approval
    // responder is attached — the gate falls back to Allow on `Ask`.
    deepseeknova_runtime::build_agent_with_role_providers(
        config,
        workspace_root,
        provider,
        roles,
        max_steps,
        None,
        extra_tools,
    )
}

/// Directory where chat sessions are persisted, driven by `[session]` config:
/// `enabled = false` disables persistence entirely; empty `root` keeps the
/// pre-B2 default `~/.deepseeknova/sessions`; non-empty `root` is used as-is.
fn sessions_root(config: &deepseeknova_config::Config) -> Option<std::path::PathBuf> {
    if !config.session.enabled {
        return None;
    }
    if config.session.root.is_empty() {
        dirs::home_dir().map(|h| h.join(".deepseeknova").join("sessions"))
    } else {
        Some(std::path::PathBuf::from(&config.session.root))
    }
}

/// Build the chat persistence context for one session.
///
/// Returns `None` (persistence disabled) when no root is available or the
/// store can't be opened — chat still works, it just isn't recorded. When
/// `resume` is set, the newest saved session is loaded into `history` and
/// becomes the active session; any failure falls back to a fresh session.
async fn build_chat_persistence(
    root: Option<std::path::PathBuf>,
    history: Arc<tokio::sync::Mutex<Vec<deepseeknova_core::Message>>>,
    resume: bool,
) -> Option<chat::ChatPersistence> {
    let store = match deepseeknova_store::SessionStore::new(root?) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("session store unavailable, persistence disabled: {e}");
            return None;
        }
    };

    let mut session_id = deepseeknova_store::new_session_id();
    let mut turn = 0u64;

    if resume {
        // Session ids sort lexicographically by time, so max() is the newest.
        let latest = store
            .list_sessions()
            .ok()
            .and_then(|ids| ids.into_iter().max());
        match latest {
            Some(id) => match store.load(&id) {
                Ok(turns) if !turns.is_empty() => {
                    let mut hist = history.lock().await;
                    for t in &turns {
                        for m in &t.messages {
                            hist.push(m.into());
                        }
                    }
                    let restored = hist.len();
                    drop(hist);
                    session_id = id.clone();
                    turn = turns.len() as u64;
                    println!("resumed session '{id}' — {restored} messages restored");
                }
                Ok(_) => tracing::warn!("latest session '{id}' is empty; starting fresh"),
                Err(e) => {
                    tracing::warn!("failed to load session '{id}' for resume: {e}; starting fresh")
                }
            },
            None => tracing::warn!("no saved session to resume; starting fresh"),
        }
    }

    Some(chat::ChatPersistence {
        store,
        session_id,
        turn,
        history,
    })
}

/// Stream events from any [`Runner`] to stdout in a consistent format.
async fn stream_events(runner: &dyn Runner, input: RunInput) -> anyhow::Result<()> {
    let mut stream = runner.run_stream(input).await?;
    while let Some(event) = stream.next().await {
        match event? {
            deepseeknova_core::RunEvent::TextDelta(text) => {
                print!("{text}");
            }
            deepseeknova_core::RunEvent::ToolCallStart { id, name } => {
                println!("\n🔧 {name} (call {id})...");
            }
            deepseeknova_core::RunEvent::ToolCallEnd {
                name: _, arguments, ..
            } => {
                println!("   args: {arguments}");
            }
            deepseeknova_core::RunEvent::Usage(u) => {
                info!("tokens: {}/{}", u.prompt_tokens, u.completion_tokens);
            }
            deepseeknova_core::RunEvent::TurnComplete => {
                println!();
            }
            deepseeknova_core::RunEvent::Done(output) => {
                println!("\n--- done ---");
                if !output.text.is_empty() {
                    println!("{}", output.text);
                }
            }
            deepseeknova_core::RunEvent::Paused { reason, session_id } => {
                eprintln!("\n⏸ paused: {reason}");
                match session_id {
                    Some(id) => {
                        eprintln!("resume with: deepseeknova chat --resume   (session {id})")
                    }
                    None => eprintln!("resume with: deepseeknova chat --resume"),
                }
                // 非交互（CI/脚本）可判定的专用退出码：3 = paused。
                std::process::exit(3);
            }
            _ => {}
        }
    }
    Ok(())
}

/// Stream from a [`CoordinatorRunner`] — uses plan-aware display labels.
async fn stream_coordinator(runner: &dyn Runner, input: RunInput) -> anyhow::Result<()> {
    let mut stream = runner.run_stream(input).await?;
    while let Some(event) = stream.next().await {
        match event? {
            deepseeknova_core::RunEvent::TextDelta(text) => {
                print!("{text}");
            }
            deepseeknova_core::RunEvent::ToolCallStart { id, name } => {
                println!("\n⚡ {name} (call {id})...");
            }
            deepseeknova_core::RunEvent::ToolCallEnd {
                name: _, arguments, ..
            } => {
                println!("   args: {arguments}");
            }
            deepseeknova_core::RunEvent::ToolResult { call_id, result } => {
                let truncated = truncate_str(&result, 300);
                println!("   → {truncated}");
                let _ = call_id;
            }
            deepseeknova_core::RunEvent::Usage(u) => {
                info!("tokens: {}/{}", u.prompt_tokens, u.completion_tokens);
            }
            deepseeknova_core::RunEvent::Done(output) => {
                println!("\n--- coordinator done ---");
                if !output.text.is_empty() {
                    println!("{}", output.text);
                }
            }
            deepseeknova_core::RunEvent::ReasoningDelta { text, .. } => {
                // Show reasoning in dim text for coordinator planning.
                print!("\x1b[2m{text}\x1b[0m");
            }
            deepseeknova_core::RunEvent::Paused { reason, session_id } => {
                eprintln!("\n⏸ paused: {reason}");
                match session_id {
                    Some(id) => {
                        eprintln!("resume with: deepseeknova chat --resume   (session {id})")
                    }
                    None => eprintln!("resume with: deepseeknova chat --resume"),
                }
                // 非交互（CI/脚本）可判定的专用退出码：3 = paused。
                std::process::exit(3);
            }
            _ => {}
        }
    }
    Ok(())
}

/// 解析并校验扫描根：词法归一 + 规范化双重包含性检查。
/// `..`/绝对路径逃逸与 symlink 逃逸均 fail-closed 中止；仅"路径不存在"
/// 这类非安全失败回落归一化路径（扫描结果为空，不泄露）。
fn resolve_scan_root(
    workspace: &std::path::Path,
    raw: &std::path::Path,
) -> anyhow::Result<std::path::PathBuf> {
    let abs = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        workspace.join(raw)
    };
    let norm = normalize_path(&abs);
    if !norm.starts_with(workspace) {
        anyhow::bail!("scan path escapes the workspace root: {}", raw.display());
    }
    // symlink 逃逸：canonicalize 解析符号链接后复核包含性。
    if let Ok(canon) = std::fs::canonicalize(&norm) {
        let ws_canon = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
        if !canon.starts_with(&ws_canon) {
            anyhow::bail!(
                "scan path escapes the workspace root via symlink: {}",
                raw.display()
            );
        }
    }
    Ok(norm)
}

/// 词法归一化路径：折叠 `..`、丢弃 `.`（保留根前缀）。
/// 与 security crate 的 normalize_path 语义一致，用于 resolve_scan_root 的
/// starts_with 预检查（词法前缀匹配不折叠 `..`，必须先归一）。
fn normalize_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut normalized = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            _ => {
                normalized.push(component);
            }
        }
    }
    normalized
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        // P0 Fix: Find the largest UTF-8 character boundary at or before max.
        // `&s[..max]` panics if max splits a multi-byte character.
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_root_honors_session_config() {
        let mut c = deepseeknova_config::Config::default();
        assert!(sessions_root(&c).is_some(), "default = enabled, home path");
        c.session.root = "/tmp/custom-sessions".into();
        assert_eq!(
            sessions_root(&c).unwrap(),
            std::path::PathBuf::from("/tmp/custom-sessions")
        );
        c.session.enabled = false;
        assert!(sessions_root(&c).is_none(), "disabled kills persistence");
    }

    #[test]
    fn review_provider_none_when_disabled() {
        // review.enabled = false → 不构建 review provider（避免无关路径因
        // quick 指针的 API key 缺失而阻断 agent 构建）。
        let config = deepseeknova_config::Config::default();
        let router = deepseeknova_provider::router::ModelRouter::from_config(
            &config,
            std::sync::Arc::new(deepseeknova_provider::cost::CostLedger::new()),
        )
        .unwrap();
        assert!(review_provider_for(&router, &config).unwrap().is_none());
    }

    #[test]
    fn compact_override_prefers_pointer_over_compact_model() {
        // 指针未设 + compact_model 非空 → override 为 compact_model
        let mut c = deepseeknova_config::Config::default();
        c.agent.compact_model = "cheap".into();
        assert_eq!(compact_override_model(&c), Some("cheap"));
        // 指针已设 → 指针胜，无 override
        c.model_pointers.compact = Some("ptr-model".into());
        assert_eq!(compact_override_model(&c), None);
        // 双无 → 无 override
        c.model_pointers.compact = None;
        c.agent.compact_model.clear();
        assert_eq!(compact_override_model(&c), None);
    }

    // ── resolve_scan_root（fail-closed 逃逸检查） ───────────────────────

    fn temp_scan_root(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("dpr-cli-scan-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn scan_root_aborts_on_parent_traversal() {
        let root = temp_scan_root("traversal");
        for bad in ["..", "../..", "../../etc/passwd"] {
            let err = resolve_scan_root(&root, std::path::Path::new(bad)).unwrap_err();
            assert!(
                err.to_string().contains("escapes the workspace root"),
                "`{bad}` must fail-closed, got: {err}"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_root_aborts_on_absolute_escape() {
        let root = temp_scan_root("abs-escape");
        let err = resolve_scan_root(&root, std::path::Path::new("/etc")).unwrap_err();
        assert!(err.to_string().contains("escapes the workspace root"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_root_allows_inner_path() {
        let root = temp_scan_root("inner");
        std::fs::create_dir_all(root.join("a/b/c")).unwrap();
        let res = resolve_scan_root(&root, std::path::Path::new("a/b/c")).unwrap();
        assert_eq!(res, root.join("a/b/c"));
        let _ = std::fs::remove_dir_all(&root);
    }

    // symlink 场景依赖 unix 的 symlink()；非 unix 下链接不存在时
    // canonicalize 失败走回落分支（返回 Ok），因此整个测试 cfg 门控。
    #[cfg(unix)]
    #[test]
    fn scan_root_aborts_on_symlink_escape() {
        let ws = std::env::temp_dir().join(format!("dnv-symlink-{}", std::process::id()));
        let outside = ws.with_extension("outside"); // 同级外部目录
        std::fs::create_dir_all(outside.join("sub")).unwrap();
        std::fs::write(outside.join("sub/secret.rs"), "let api_key = \"sk-x\";\n").unwrap();
        std::fs::create_dir_all(&ws).unwrap();
        let _ = std::fs::remove_file(ws.join("link"));
        std::os::unix::fs::symlink(&outside, ws.join("link")).unwrap();
        let ws_root = std::path::PathBuf::from(&ws);
        let err = resolve_scan_root(&ws_root, std::path::Path::new("link/sub")).unwrap_err();
        assert!(err.to_string().contains("symlink"));
        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn severity_min_filter_direction() {
        use deepseeknova_scanner::rule::Severity;
        // "medium" 下限应保留 High 与 Medium，排除 Low。
        let min = Severity::Medium;
        assert!(Severity::High <= min, "High kept under medium floor");
        assert!(Severity::Medium <= min);
        assert!(!(Severity::Low <= min), "Low excluded under medium floor");
    }
}

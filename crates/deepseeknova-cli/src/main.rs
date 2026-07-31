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

                // Wire built-in tools for the executor. Graph tools require a GraphHandle
                // injected via ToolContext (only wired in the single-agent build_agent path),
                // so they are excluded here until coordinator graph wiring lands.
                let graph_tools = ["search_code", "traverse_graph", "retrieve_entity"];
                for tool in deepseeknova_tools::all_builtin_tools() {
                    if graph_tools.contains(&tool.schema().name.as_str()) {
                        continue;
                    }
                    runner.register_tool(tool);
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
                let agent = build_agent(
                    Arc::clone(&provider),
                    Some(task_provider),
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
            if *tui {
                let history: Arc<tokio::sync::Mutex<Vec<deepseeknova_core::Message>>> =
                    Arc::new(tokio::sync::Mutex::new(Vec::new()));
                use deepseeknova_provider::cost::ModelRole;
                let provider = model_router.provider_for_maybe_model(
                    ModelRole::Main,
                    model.as_deref(),
                    Some(baseline_effort),
                )?;
                let task_provider =
                    model_router.provider_for(ModelRole::Task, Some(baseline_effort))?;
                let agent = build_agent(
                    provider,
                    Some(task_provider),
                    model.as_deref(),
                    &config,
                    0,
                    mcp_tools,
                )?
                .with_conversation_history(history);
                deepseeknova_tui::TuiRunner::new(Arc::new(agent))
                    .run()
                    .await?;
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
                        let agent = build_agent(
                            provider,
                            Some(task_provider),
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
            let agent = build_agent(
                Arc::clone(&provider),
                Some(task_provider),
                None,
                &config,
                0,
                mcp_tools,
            )?
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
                        let agent = build_agent(
                            provider,
                            Some(task_provider),
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

/// Build an agent with built-in tools registered, plus any `extra_tools`
/// (e.g. MCP tools discovered via [`deepseeknova_runtime::discover_mcp_tools`]).
/// `task_provider` routes the delegation engine (sub-agents) to the Task-role
/// model when supplied; `None` falls back to the main provider.
fn build_agent(
    provider: Arc<dyn deepseeknova_provider::Provider>,
    task_provider: Option<Arc<dyn deepseeknova_provider::Provider>>,
    _model: Option<&str>,
    config: &deepseeknova_config::Config,
    max_steps: usize,
    extra_tools: Vec<Arc<dyn deepseeknova_core::Tool>>,
) -> anyhow::Result<deepseeknova_agent::Agent> {
    let workspace_root = std::env::current_dir().unwrap_or_default();
    // Delegate to the shared runtime builder (security + sandbox + permission
    // gate wiring lives in one place). CLI is non-interactive, so no approval
    // responder is attached — the gate falls back to Allow on `Ask`.
    deepseeknova_runtime::build_agent_with_task_provider(
        config,
        workspace_root,
        provider,
        task_provider,
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
}

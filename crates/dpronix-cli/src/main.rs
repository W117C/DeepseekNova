mod chat;
mod cli;
mod init;
mod setup;

use clap::Parser;
use cli::{Cli, Commands};
use dpronix_agent::{CoordinatorRunner, PlanModeRunner};
use dpronix_core::planner::SimplePlanner;
use dpronix_core::runner::{RunInput, Runner};
use std::sync::Arc;
use tokio_stream::StreamExt;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let cli = Cli::parse();

    let config = dpronix_config::Config::load().unwrap_or_else(|e| {
        tracing::warn!("failed to load config, using defaults: {e}");
        dpronix_config::Config::default()
    });

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
                let planner_provider = resolve_provider(&config, &Some(planner_model.clone()))?;
                let executor_model = coordinator
                    .executor_model
                    .clone()
                    .or_else(|| model_args.model.clone());
                let executor_provider = resolve_provider_for_task(
                    &config,
                    &executor_model,
                    Some(dpronix_provider::factory::ReasoningEffort::High),
                )?;
                let max_nodes = coordinator.max_graph_nodes;

                let workspace_root = std::env::current_dir().unwrap_or_default();
                let security = dpronix_runtime::build_security_context(&config, &workspace_root)?;
                let mut runner = CoordinatorRunner::new(planner_provider, executor_provider)
                    .with_max_graph_nodes(max_nodes)
                    .with_workspace_root(workspace_root)
                    .with_security(security);

                // Wire all built-in tools for the executor.
                for tool in dpronix_tools::all_builtin_tools() {
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
                let provider = resolve_provider(&config, &model_args.model)?;
                let agent = build_agent(
                    Arc::clone(&provider),
                    model_args.model.as_deref(),
                    &config,
                    model_args.max_steps,
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

            let provider = resolve_provider(&config, model)?;
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
        Some(Commands::Chat { model }) => {
            info!("chat: model={model:?}");
            loop {
                let provider = resolve_provider(&config, model)?;
                let agent = build_agent(
                    Arc::clone(&provider),
                    model.as_deref(),
                    &config,
                    0, // no max_steps limit in chat mode
                )?;
                let restart = chat::run_chat_repl(&agent, model.clone()).await?;
                if !restart {
                    break;
                }
                info!("restarting chat session...");
            }
        }

        Some(Commands::Serve { addr }) => {
            info!("serve command: addr={addr}");

            let provider = resolve_provider(&config, &None)?;
            let agent = build_agent(Arc::clone(&provider), None, &config, 0)?;
            let runner: Arc<dyn Runner> = Arc::new(agent);

            let server = dpronix_serve::Server::new(runner);
            server.serve(addr).await?;
        }

        Some(Commands::Setup { local }) => {
            info!("setup: local={local}");
            setup::run_setup_wizard(*local).await?;
        }

        Some(Commands::Config) => {
            println!("{:#?}", config);
        }

        Some(Commands::Init) => {
            info!("init command");
            init::run_init().await?;
        }

        None => {
            info!("no command provided — starting interactive chat");
            loop {
                let provider = resolve_provider(&config, &None)?;
                let agent = build_agent(Arc::clone(&provider), None, &config, 0)?;
                let restart = chat::run_chat_repl(&agent, None).await?;
                if !restart {
                    break;
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the provider from config for a given model name.
fn resolve_provider(
    config: &dpronix_config::Config,
    model: &Option<String>,
) -> anyhow::Result<Arc<dyn dpronix_provider::Provider>> {
    resolve_provider_for_task(config, model, None)
}

/// Resolve a provider, applying a reasoning-effort task classification.
///
/// Used to cap per-node executor reasoning below the planner's depth: in the
/// two-model coordinator the planner already performs the deep reasoning, so
/// paying DeepSeek Max-effort reasoning tokens on every mechanical execution
/// node is wasteful. A `High` ceiling keeps executor reasoning useful while
/// preventing a `Max` config default from applying to each node.
fn resolve_provider_for_task(
    config: &dpronix_config::Config,
    model: &Option<String>,
    task: Option<dpronix_provider::factory::ReasoningEffort>,
) -> anyhow::Result<Arc<dyn dpronix_provider::Provider>> {
    let provider_cfg = if let Some(ref model_name) = model {
        config
            .resolve_provider_for_model(model_name)
            .or_else(|| config.providers.first())
            .ok_or_else(|| anyhow::anyhow!("no provider found for model '{model_name}'"))?
    } else {
        config
            .providers
            .first()
            .ok_or_else(|| anyhow::anyhow!("no providers configured"))?
    };

    Ok(dpronix_provider::factory::create_provider_for_task(provider_cfg, task)?.into())
}

/// Build an agent with built-in tools registered.
fn build_agent(
    provider: Arc<dyn dpronix_provider::Provider>,
    _model: Option<&str>,
    config: &dpronix_config::Config,
    max_steps: usize,
) -> anyhow::Result<dpronix_agent::Agent> {
    let workspace_root = std::env::current_dir().unwrap_or_default();
    let security = dpronix_runtime::build_security_context(config, &workspace_root)?;

    let mut agent = dpronix_agent::Agent::new(
        provider,
        if max_steps > 0 {
            max_steps
        } else {
            config.agent.max_steps
        },
    )
    .with_workspace_root(workspace_root)
    .with_security(security);

    if let Some(ref sp) = config.agent.system_prompt {
        agent = agent.with_system_prompt(sp.clone());
    }

    // Register all built-in tools
    for tool in dpronix_tools::all_builtin_tools() {
        agent.register_tool(tool);
    }

    Ok(agent)
}

/// Stream events from any [`Runner`] to stdout in a consistent format.
async fn stream_events(runner: &dyn Runner, input: RunInput) -> anyhow::Result<()> {
    let mut stream = runner.run_stream(input).await?;
    while let Some(event) = stream.next().await {
        match event? {
            dpronix_core::RunEvent::TextDelta(text) => {
                print!("{text}");
            }
            dpronix_core::RunEvent::ToolCallStart { id, name } => {
                println!("\n🔧 {name} (call {id})...");
            }
            dpronix_core::RunEvent::ToolCallEnd {
                name: _, arguments, ..
            } => {
                println!("   args: {arguments}");
            }
            dpronix_core::RunEvent::Usage(u) => {
                info!("tokens: {}/{}", u.prompt_tokens, u.completion_tokens);
            }
            dpronix_core::RunEvent::TurnComplete => {
                println!();
            }
            dpronix_core::RunEvent::Done(output) => {
                println!("\n--- done ---");
                if !output.text.is_empty() {
                    println!("{}", output.text);
                }
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
            dpronix_core::RunEvent::TextDelta(text) => {
                print!("{text}");
            }
            dpronix_core::RunEvent::ToolCallStart { id, name } => {
                println!("\n⚡ {name} (call {id})...");
            }
            dpronix_core::RunEvent::ToolCallEnd {
                name: _, arguments, ..
            } => {
                println!("   args: {arguments}");
            }
            dpronix_core::RunEvent::ToolResult { call_id, result } => {
                let truncated = truncate_str(&result, 300);
                println!("   → {truncated}");
                let _ = call_id;
            }
            dpronix_core::RunEvent::Usage(u) => {
                info!("tokens: {}/{}", u.prompt_tokens, u.completion_tokens);
            }
            dpronix_core::RunEvent::Done(output) => {
                println!("\n--- coordinator done ---");
                if !output.text.is_empty() {
                    println!("{}", output.text);
                }
            }
            dpronix_core::RunEvent::ReasoningDelta { text, .. } => {
                // Show reasoning in dim text for coordinator planning.
                print!("\x1b[2m{text}\x1b[0m");
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
        format!("{}…", &s[..max])
    }
}

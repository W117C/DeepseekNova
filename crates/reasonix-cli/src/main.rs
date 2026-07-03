mod chat;
mod cli;
mod init;
mod setup;

use clap::Parser;
use cli::{Cli, Commands};
use reasonix_agent::{CoordinatorRunner, PlanModeRunner};
use reasonix_core::planner::SimplePlanner;
use reasonix_core::runner::{RunInput, Runner};
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

    let config = reasonix_config::Config::load().unwrap_or_else(|e| {
        tracing::warn!("failed to load config, using defaults: {e}");
        reasonix_config::Config::default()
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
                let executor_provider = resolve_provider(&config, &executor_model)?;
                let max_nodes = coordinator.max_graph_nodes;

                let mut runner = CoordinatorRunner::new(planner_provider, executor_provider)
                    .with_max_graph_nodes(max_nodes);

                // Wire all built-in tools for the executor.
                for tool in reasonix_tools::all_builtin_tools() {
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
                );

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

        // ── Chat ─────────────────────────────────────────────────────────
        Some(Commands::Chat { model }) => {
            info!("chat: model={model:?}");

            let provider = resolve_provider(&config, model)?;
            let agent = build_agent(
                Arc::clone(&provider),
                model.as_deref(),
                &config,
                0, // no max_steps limit in chat mode
            );

            chat::run_chat_repl(&agent, model.clone()).await?;
        }

        Some(Commands::Serve { addr }) => {
            info!("serve command: addr={addr}");
            println!("HTTP serve is coming in Phase 4.");
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
            let provider = resolve_provider(&config, &None)?;
            let agent = build_agent(Arc::clone(&provider), None, &config, 0);
            chat::run_chat_repl(&agent, None).await?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the provider from config for a given model name.
fn resolve_provider(
    config: &reasonix_config::Config,
    model: &Option<String>,
) -> anyhow::Result<Arc<dyn reasonix_provider::Provider>> {
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

    Ok(reasonix_provider::factory::create_provider(provider_cfg)?.into())
}

/// Build an agent with built-in tools registered.
fn build_agent(
    provider: Arc<dyn reasonix_provider::Provider>,
    _model: Option<&str>,
    config: &reasonix_config::Config,
    max_steps: usize,
) -> reasonix_agent::Agent {
    let mut agent = reasonix_agent::Agent::new(
        provider,
        if max_steps > 0 {
            max_steps
        } else {
            config.agent.max_steps
        },
    );

    if let Some(ref sp) = config.agent.system_prompt {
        agent = agent.with_system_prompt(sp.clone());
    }

    // Register all built-in tools
    for tool in reasonix_tools::all_builtin_tools() {
        agent.register_tool(tool);
    }

    agent
}

/// Stream events from any [`Runner`] to stdout in a consistent format.
async fn stream_events(runner: &dyn Runner, input: RunInput) -> anyhow::Result<()> {
    let mut stream = runner.run_stream(input).await?;
    while let Some(event) = stream.next().await {
        match event? {
            reasonix_core::RunEvent::TextDelta(text) => {
                print!("{text}");
            }
            reasonix_core::RunEvent::ToolCallStart { id, name } => {
                println!("\n🔧 {name} (call {id})...");
            }
            reasonix_core::RunEvent::ToolCallEnd {
                name: _, arguments, ..
            } => {
                println!("   args: {arguments}");
            }
            reasonix_core::RunEvent::Usage(u) => {
                info!("tokens: {}/{}", u.prompt_tokens, u.completion_tokens);
            }
            reasonix_core::RunEvent::TurnComplete => {
                println!();
            }
            reasonix_core::RunEvent::Done(output) => {
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
            reasonix_core::RunEvent::TextDelta(text) => {
                print!("{text}");
            }
            reasonix_core::RunEvent::ToolCallStart { id, name } => {
                println!("\n⚡ {name} (call {id})...");
            }
            reasonix_core::RunEvent::ToolCallEnd {
                name: _, arguments, ..
            } => {
                println!("   args: {arguments}");
            }
            reasonix_core::RunEvent::ToolResult { call_id, result } => {
                let truncated = truncate_str(&result, 300);
                println!("   → {truncated}");
                let _ = call_id;
            }
            reasonix_core::RunEvent::Usage(u) => {
                info!("tokens: {}/{}", u.prompt_tokens, u.completion_tokens);
            }
            reasonix_core::RunEvent::Done(output) => {
                println!("\n--- coordinator done ---");
                if !output.text.is_empty() {
                    println!("{}", output.text);
                }
            }
            reasonix_core::RunEvent::ReasoningDelta { text, .. } => {
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

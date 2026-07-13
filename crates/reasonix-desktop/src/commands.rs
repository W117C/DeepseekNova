//! Tauri command handlers — the bridge between the React/TS frontend
//! and the reasonix agent kernel.
//!
//! Each command is an async function that the frontend calls via `invoke()`.
//! Streaming events are delivered through Tauri Channels (`tauri::ipc::Channel`),
//! the desktop equivalent of the HTTP SSE stream in `reasonix-serve`.

use reasonix_core::runner::{RunEvent, RunInput, Runner};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{ipc::Channel, State};
use tokio_stream::StreamExt;
use tracing::info;

use crate::AppState;

// ---------------------------------------------------------------------------
// Wire types — the JSON contract between frontend and backend.
// Mirrors the SSE wire format from reasonix-serve but uses
// Tauri Channel events instead of HTTP data: frames.
// ---------------------------------------------------------------------------

/// A single event pushed to the frontend Channel.
/// The `kind` field discriminates the event type (analogous to SSE event names).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireEvent {
    TextDelta { text: String },
    ReasoningDelta { text: String, signature: Option<String> },
    ToolCallStart { id: String, name: String },
    ToolCallDelta { id: String, args_delta: String },
    ToolCallEnd { id: String, name: String, arguments: String },
    ToolResult { call_id: String, result: String },
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        cache_hit_tokens: u32,
        cache_miss_tokens: u32,
        session_cache_hit_tokens: u32,
        session_cache_miss_tokens: u32,
    },
    TurnComplete,
    Done {
        text: String,
        usage: Option<UsageInfo>,
    },
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageInfo {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub cache_hit_tokens: u32,
    pub cache_miss_tokens: u32,
    pub session_cache_hit_tokens: u32,
    pub session_cache_miss_tokens: u32,
}

/// Frontend request to submit a prompt to the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitRequest {
    pub prompt: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub thinking_enabled: Option<bool>,
}

/// A single skill summary for the frontend skills panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    pub tools_allowed: Vec<String>,
}

/// Provider summary for settings panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSummary {
    pub name: String,
    pub kind: String,
    pub model: Option<String>,
    pub base_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Submit a prompt to the agent and stream results back via Channel.
///
/// The frontend calls this with a prompt string and receives streaming
/// `WireEvent` values through the `on_event` callback.
#[tauri::command]
pub async fn submit_prompt(
    _app: tauri::AppHandle,
    state: State<'_, AppState>,
    request: SubmitRequest,
    on_event: Channel<WireEvent>,
) -> Result<(), String> {
    info!("submit_prompt: prompt={}", request.prompt);

    // Load config
    let config = reasonix_config::Config::load().map_err(|e| format!("config error: {e}"))?;

    // Resolve provider
    let provider_cfg = config.providers.first().ok_or("no providers configured")?;
    let provider = reasonix_provider::factory::create_provider(provider_cfg)
        .map_err(|e| format!("provider error: {e}"))?;

    // Build agent with all tools
    let mut agent = reasonix_agent::Agent::new(provider.into(), config.agent.max_steps);
    if let Some(ref sp) = config.agent.system_prompt {
        agent = agent.with_system_prompt(sp.clone());
    }
    for tool in reasonix_tools::all_builtin_tools() {
        agent.register_tool(tool);
    }

    // Create cancellation token and wire Ctrl-C
    let cancel = tokio_util::sync::CancellationToken::new();
    {
        let mut state_cancel = state.cancel.lock().await;
        *state_cancel = Some(cancel.clone());
    }

    let agent: Arc<dyn Runner> = Arc::new(agent);

    let input = RunInput {
        prompt: request.prompt,
        images: vec![],
        model_override: request.model,
    };

    // Run the agent in a spawned task, streaming events through the Channel
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        match agent.run_stream(input).await {
            Ok(mut stream) => {
                let mut final_text = String::new();
                let mut final_usage: Option<UsageInfo> = None;

                while let Some(event) = stream.next().await {
                    if cancel_clone.is_cancelled() {
                        let _ = on_event.send(WireEvent::Done {
                            text: final_text,
                            usage: final_usage,
                        });
                        return;
                    }

                    match event {
                        Ok(RunEvent::TextDelta(text)) => {
                            final_text.push_str(&text);
                            let _ = on_event.send(WireEvent::TextDelta { text });
                        }
                        Ok(RunEvent::ReasoningDelta { text, signature }) => {
                            let _ = on_event.send(WireEvent::ReasoningDelta { text, signature });
                        }
                        Ok(RunEvent::ToolCallStart { id, name }) => {
                            let _ = on_event.send(WireEvent::ToolCallStart { id, name });
                        }
                        Ok(RunEvent::ToolCallDelta { id, args_delta }) => {
                            let _ = on_event.send(WireEvent::ToolCallDelta { id, args_delta });
                        }
                        Ok(RunEvent::ToolCallEnd { id, name, arguments }) => {
                            let _ = on_event.send(WireEvent::ToolCallEnd { id, name, arguments });
                        }
                        Ok(RunEvent::ToolResult { call_id, result }) => {
                            let _ = on_event.send(WireEvent::ToolResult { call_id, result });
                        }
                        Ok(RunEvent::Usage(u)) => {
                            let usage_info = UsageInfo {
                                prompt_tokens: u.prompt_tokens,
                                completion_tokens: u.completion_tokens,
                                total_tokens: u.total_tokens,
                                cache_hit_tokens: u.cache_hit_tokens,
                                cache_miss_tokens: u.cache_miss_tokens,
                                session_cache_hit_tokens: u.cache_hit_tokens,
                                session_cache_miss_tokens: u.cache_miss_tokens,
                            };
                            final_usage = Some(usage_info.clone());
                            let _ = on_event.send(WireEvent::Usage {
                                prompt_tokens: u.prompt_tokens,
                                completion_tokens: u.completion_tokens,
                                total_tokens: u.total_tokens,
                                cache_hit_tokens: u.cache_hit_tokens,
                                cache_miss_tokens: u.cache_miss_tokens,
                                session_cache_hit_tokens: u.cache_hit_tokens,
                                session_cache_miss_tokens: u.cache_miss_tokens,
                            });
                        }
                        Ok(RunEvent::TurnComplete) => {
                            let _ = on_event.send(WireEvent::TurnComplete);
                        }
                        Ok(RunEvent::Done(output)) => {
                            let usage_info = UsageInfo {
                                prompt_tokens: output.usage.as_ref().map_or(0, |u| u.prompt_tokens),
                                completion_tokens: output.usage.as_ref().map_or(0, |u| u.completion_tokens),
                                total_tokens: output.usage.as_ref().map_or(0, |u| u.total_tokens),
                                cache_hit_tokens: output.usage.as_ref().map_or(0, |u| u.cache_hit_tokens),
                                cache_miss_tokens: output.usage.as_ref().map_or(0, |u| u.cache_miss_tokens),
                                session_cache_hit_tokens: output.usage.as_ref().map_or(0, |u| u.cache_hit_tokens),
                                session_cache_miss_tokens: output.usage.as_ref().map_or(0, |u| u.cache_miss_tokens),
                            };
                            let _ = on_event.send(WireEvent::Done {
                                text: output.text,
                                usage: Some(usage_info),
                            });
                            return;
                        }
                        Err(e) => {
                            let _ = on_event.send(WireEvent::Error {
                                message: format!("{e}"),
                            });
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                let _ = on_event.send(WireEvent::Error {
                    message: format!("{e}"),
                });
            }
        }
    });

    Ok(())
}

/// Cancel the current agent run.
#[tauri::command]
pub async fn cancel_run(state: State<'_, AppState>) -> Result<(), String> {
    let mut cancel = state.cancel.lock().await;
    if let Some(token) = cancel.take() {
        token.cancel();
        info!("agent run cancelled");
    }
    Ok(())
}

/// List available skills from .reasonix/skills/ and .agents/skills/.
#[tauri::command]
pub async fn list_skills() -> Result<Vec<SkillSummary>, String> {
    let mut skills = Vec::new();
    let paths = [".reasonix/skills", ".agents/skills"];
    for path_str in &paths {
        let loader = reasonix_skills::SkillLoader::new(path_str);
        if let Ok(loaded) = loader.load_all() {
            for skill in loaded {
                skills.push(SkillSummary {
                    name: skill.name,
                    description: skill.description,
                    tools_allowed: skill.tools_allowed,
                });
            }
        }
    }
    Ok(skills)
}

/// List configured providers.
#[tauri::command]
pub async fn list_providers() -> Result<Vec<ProviderSummary>, String> {
    let config = reasonix_config::Config::load().map_err(|e| format!("config error: {e}"))?;
    Ok(config
        .providers
        .iter()
        .map(|p| ProviderSummary {
            name: p.name.clone(),
            kind: p.kind.clone(),
            model: p.model.clone(),
            base_url: p.base_url.clone(),
        })
        .collect())
}

/// Get the current configuration as a JSON string.
#[tauri::command]
pub async fn get_config() -> Result<String, String> {
    let config = reasonix_config::Config::load().map_err(|e| format!("config error: {e}"))?;
    serde_json::to_string_pretty(&config).map_err(|e| format!("serialize error: {e}"))
}

/// Get agent capabilities information (for frontend feature detection).
#[tauri::command]
pub async fn get_capabilities() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "supports_thinking": true,
        "supports_reasoning_effort": true,
        "supports_tools": true,
        "supports_mcp": true,
        "supports_images": false,
        "max_steps_default": 25,
        "reasoning_effort_levels": ["low", "medium", "high", "max"],
    }))
}

/// Health check command.
#[tauri::command]
pub async fn health_check() -> Result<String, String> {
    Ok("ok".to_string())
}

//! Tauri command handlers — the bridge between the React/TS frontend
//! and the deepnova agent kernel.
//!
//! Each command is an async function that the frontend calls via `invoke()`.
//! Streaming events are delivered through Tauri Channels (`tauri::ipc::Channel`),
//! the desktop equivalent of the HTTP SSE stream in `deepnova-serve`.

use deepnova_core::runner::{RunInput, Runner, WireEvent};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{ipc::Channel, State};
use tokio_stream::StreamExt;
use tracing::info;

use crate::AppState;

// ---------------------------------------------------------------------------
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
    let config = deepnova_config::Config::load().map_err(|e| format!("config error: {e}"))?;

    // Build runtime — single shared composition root
    let workspace_root = std::env::current_dir().unwrap_or_default();
    let security = deepnova_runtime::build_security_context(&config, &workspace_root)
        .map_err(|e| format!("security context error: {e}"))?;

    // Resolve the provider config. If the frontend specified a model name,
    // look it up by name; otherwise use the first configured provider.
    let provider_cfg = if let Some(ref model_name) = request.model {
        config
            .resolve_provider_for_model(model_name)
            .ok_or_else(|| format!("provider '{model_name}' not found in config"))?
    } else {
        config.providers.first().ok_or("no providers configured")?
    };

    // Map the frontend's reasoning_effort string to an enum, then let
    // thinking_enabled=false override it to Disabled.
    let effort = {
        let from_string = request
            .reasoning_effort
            .as_deref()
            .and_then(deepnova_provider::factory::ReasoningEffort::from_config_str);
        // If the user explicitly disabled thinking, force Disabled
        // regardless of what the reasoning_effort string says.
        if request.thinking_enabled == Some(false) {
            Some(deepnova_provider::factory::ReasoningEffort::Disabled)
        } else {
            from_string
        }
    };

    let provider = deepnova_provider::factory::create_provider_for_task(provider_cfg, effort)
        .map_err(|e| format!("provider error: {e}"))?;

    // Build agent wired through the composition root
    let mut agent = deepnova_agent::Agent::new(provider.into(), config.agent.max_steps)
        .with_workspace_root(workspace_root)
        .with_security(security)
        // Share the session's persistent conversation store so the agent
        // carries memory across prompts (multi-turn). This is also what lets
        // DeepSeek-V4's reasoning_content replay contract span user turns.
        .with_conversation_history(state.history.clone());
    if let Some(ref sp) = config.agent.system_prompt {
        agent = agent.with_system_prompt(sp.clone());
    }
    for tool in deepnova_tools::all_builtin_tools() {
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
                let mut final_usage: Option<deepnova_core::chunk::Usage> = None;

                while let Some(event) = stream.next().await {
                    if cancel_clone.is_cancelled() {
                        let _ = on_event.send(WireEvent::Done {
                            text: final_text,
                            usage: final_usage.map(Into::into),
                        });
                        return;
                    }

                    match event {
                        Ok(ev) => {
                            // Accumulate text deltas for the final Done event
                            if let deepnova_core::runner::RunEvent::TextDelta(ref text) = ev {
                                final_text.push_str(text);
                            }
                            if let deepnova_core::runner::RunEvent::Usage(ref usage) = ev {
                                final_usage = Some(usage.clone());
                            }
                            // Also capture text from the terminal Done event
                            if let deepnova_core::runner::RunEvent::Done(ref output) = ev {
                                if !output.text.is_empty() {
                                    final_text = output.text.clone();
                                }
                                if output.usage.is_some() {
                                    final_usage = output.usage.clone();
                                }
                            }
                            let wire: WireEvent = ev.into();
                            let _ = on_event.send(wire);
                        }
                        Err(e) => {
                            let _ = on_event.send(WireEvent::Error {
                                message: format!("{e}"),
                            });
                            return;
                        }
                    }
                }

                // Stream ended normally — send final Done event
                let _ = on_event.send(WireEvent::Done {
                    text: final_text,
                    usage: final_usage.map(Into::into),
                });
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

/// Start a fresh conversation: clear the persistent history store so the next
/// prompt begins with no prior context (system prompt re-injected).
#[tauri::command]
pub async fn new_session(state: State<'_, AppState>) -> Result<(), String> {
    let mut history = state.history.lock().await;
    history.clear();
    info!("new session started (conversation history cleared)");
    Ok(())
}

/// List available skills from .deepnova/skills/ and .agents/skills/.
#[tauri::command]
pub async fn list_skills() -> Result<Vec<SkillSummary>, String> {
    let mut skills = Vec::new();
    let paths = [".deepnova/skills", ".agents/skills"];
    for path_str in &paths {
        let loader = deepnova_skills::SkillLoader::new(path_str);
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
    let config = deepnova_config::Config::load().map_err(|e| format!("config error: {e}"))?;
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
    let config = deepnova_config::Config::load().map_err(|e| format!("config error: {e}"))?;
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

/// Respond to an approval request (approve or reject a pending action).
#[tauri::command]
pub async fn respond_approval(
    state: State<'_, AppState>,
    request_id: String,
    approved: bool,
) -> Result<(), String> {
    info!("respond_approval: id={request_id} approved={approved}");
    // Forward the response to the agent's approval channel if one exists.
    let mut approval_tx = state.approval_tx.lock().await;
    if let Some(tx) = approval_tx.take() {
        let _ = tx.send((request_id, approved));
    }
    Ok(())
}

/// Health check command.
#[tauri::command]
pub async fn health_check() -> Result<String, String> {
    Ok("ok".to_string())
}

// ── Session persistence ─────────────────────────────────────────

/// A summary of one conversation session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub message_count: usize,
    pub created_at: String,
}

/// Helper to load/ save session metadata JSON.
fn sessions_path() -> std::path::PathBuf {
    let mut p = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    p.push(".deepnova");
    p.push("sessions.json");
    p
}

fn load_sessions() -> Vec<SessionInfo> {
    let path = sessions_path();
    if !path.exists() {
        return vec![SessionInfo {
            id: "default".into(),
            title: "Current Session".into(),
            message_count: 0,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs().to_string())
                .unwrap_or_default(),
        }];
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_sessions(sessions: &[SessionInfo]) {
    let path = sessions_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(
        &path,
        serde_json::to_string_pretty(sessions).unwrap_or_default(),
    );
}

fn generate_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("s{:x}{:04x}", d.as_secs(), d.subsec_nanos() % 0x10000)
}

/// List all sessions.
#[tauri::command]
pub async fn list_sessions() -> Result<Vec<SessionInfo>, String> {
    Ok(load_sessions())
}

/// Create a new session and return its id.
#[tauri::command]
pub async fn create_session(title: Option<String>) -> Result<SessionInfo, String> {
    let mut sessions = load_sessions();
    let id = generate_id();
    let session = SessionInfo {
        id: id.clone(),
        title: title.unwrap_or_else(|| "Untitled".into()),
        message_count: 0,
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs().to_string())
            .unwrap_or_default(),
    };
    sessions.push(session.clone());
    save_sessions(&sessions);
    info!("created session {id}");
    Ok(session)
}

/// Delete a session by id.
#[tauri::command]
pub async fn delete_session(id: String) -> Result<(), String> {
    let mut sessions = load_sessions();
    sessions.retain(|s| s.id != id);
    save_sessions(&sessions);
    info!("deleted session {id}");
    Ok(())
}

/// List top-level workspace files (for the Files tab in sidebar).
#[tauri::command]
pub async fn get_workspace_files() -> Result<Vec<String>, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cwd error: {e}"))?;
    let mut entries = Vec::new();
    let mut dir = tokio::fs::read_dir(&cwd)
        .await
        .map_err(|e| format!("read_dir error: {e}"))?;
    while let Some(entry) = dir
        .next_entry()
        .await
        .map_err(|e| format!("entry error: {e}"))?
    {
        let path = entry.path();
        let display = if path.is_dir() {
            format!(
                "{}/",
                path.file_name()
                    .map(|s| s.to_string_lossy())
                    .unwrap_or_default()
            )
        } else {
            path.file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        };
        entries.push(display);
    }
    entries.sort();
    Ok(entries)
}

use super::*;

#[tauri::command]
pub async fn submit_prompt(
    _app: tauri::AppHandle,
    state: State<'_, AppState>,
    request: SubmitRequest,
    on_event: Channel<WireEvent>,
) -> Result<(), String> {
    info!("submit_prompt: prompt={}", request.prompt);

    let config = deepseeknova_config::Config::load().map_err(|e| format!("config error: {e}"))?;

    let workspace_root = std::env::current_dir().unwrap_or_default();
    let security = deepseeknova_runtime::build_security_context(&config, &workspace_root)
        .map_err(|e| format!("security context error: {e}"))?;

    let provider_cfg = if let Some(ref model_name) = request.model {
        config
            .resolve_provider_for_model(model_name)
            .ok_or_else(|| format!("provider '{model_name}' not found in config"))?
    } else {
        config.providers.first().ok_or("no providers configured")?
    };

    let effort = {
        let from_string = request
            .reasoning_effort
            .as_deref()
            .and_then(deepseeknova_provider::factory::ReasoningEffort::from_config_str);
        if request.thinking_enabled == Some(false) {
            Some(deepseeknova_provider::factory::ReasoningEffort::Disabled)
        } else {
            from_string
        }
    };

    let provider = deepseeknova_provider::factory::create_provider_for_task(provider_cfg, effort)
        .map_err(|e| format!("provider error: {e}"))?;

    let mut agent = deepseeknova_agent::Agent::new(provider.into(), config.agent.max_steps)
        .with_workspace_root(workspace_root)
        .with_security(security)
        .with_conversation_history(state.history.clone());
    if let Some(ref sp) = config.agent.system_prompt {
        agent = agent.with_system_prompt(sp.clone());
    }
    for tool in deepseeknova_tools::all_builtin_tools() {
        agent.register_tool(tool);
    }

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

    let cancel_clone = cancel.clone();
    let usage_arc = state.usage.clone();
    tokio::spawn(async move {
        match agent.run_stream(input).await {
            Ok(mut stream) => {
                let mut final_text = String::new();
                let mut final_usage: Option<deepseeknova_core::chunk::Usage> = None;

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
                            if let deepseeknova_core::runner::RunEvent::TextDelta(ref text) = ev {
                                final_text.push_str(text);
                            }
                            if let deepseeknova_core::runner::RunEvent::Usage(ref usage) = ev {
                                final_usage = Some(usage.clone());
                            }
                            if let deepseeknova_core::runner::RunEvent::Done(ref output) = ev {
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

                // Accumulate usage into AppState for billing stats
                if let Some(ref u) = final_usage {
                    let mut usage_state = usage_arc.lock().await;
                    usage_state.prompt_tokens += u.prompt_tokens as u64;
                    usage_state.completion_tokens += u.completion_tokens as u64;
                    usage_state.total_tokens += u.total_tokens as u64;
                    usage_state.cache_hit_tokens += u.cache_hit_tokens as u64;
                    usage_state.cache_miss_tokens += u.cache_miss_tokens as u64;
                    usage_state.reasoning_tokens += u.reasoning_tokens as u64;
                    usage_state.run_count += 1;
                }

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

#[tauri::command]
pub async fn cancel_run(state: State<'_, AppState>) -> Result<(), String> {
    let mut cancel = state.cancel.lock().await;
    if let Some(token) = cancel.take() {
        token.cancel();
        info!("agent run cancelled");
    }
    Ok(())
}

#[tauri::command]
pub async fn new_session(state: State<'_, AppState>) -> Result<(), String> {
    let mut history = state.history.lock().await;
    history.clear();
    info!("new session started (conversation history cleared)");
    Ok(())
}

#[tauri::command]
pub async fn respond_approval(
    state: State<'_, AppState>,
    request_id: String,
    approved: bool,
) -> Result<(), String> {
    info!("respond_approval: id={request_id} approved={approved}");
    let mut approval_tx = state.approval_tx.lock().await;
    if let Some(tx) = approval_tx.take() {
        let _ = tx.send((request_id, approved));
    }
    Ok(())
}

#[tauri::command]
pub async fn health_check() -> Result<String, String> {
    Ok("ok".to_string())
}

#[tauri::command]
pub async fn get_config() -> Result<String, String> {
    let config = deepseeknova_config::Config::load().map_err(|e| format!("config error: {e}"))?;
    serde_json::to_string_pretty(&config).map_err(|e| format!("serialize error: {e}"))
}

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

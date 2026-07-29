use super::*;

/// Desktop approval responder: registers a oneshot per request id in the shared
/// AppState map and awaits the user's answer (delivered by `respond_approval`).
struct DesktopApprovalResponder {
    approvals: crate::ApprovalChannel,
}

#[async_trait::async_trait]
impl deepseeknova_agent::ApprovalResponder for DesktopApprovalResponder {
    async fn request(&self, id: &str, _title: &str, _description: Option<&str>) -> bool {
        let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
        {
            let mut map = self.approvals.lock().await;
            map.insert(id.to_string(), tx);
        }
        rx.await.unwrap_or(false)
    }
}

/// 纯函数核心：将字符串截断到不超过 `max_bytes` 字节，且落在 UTF-8 字符边界上
/// （否则 `String::truncate` 会 panic）。超长时追加截断标记。
fn truncate_attachment(content: &mut String, max_bytes: usize) {
    if content.len() <= max_bytes {
        return;
    }
    let mut cut = max_bytes;
    while cut > 0 && !content.is_char_boundary(cut) {
        cut -= 1;
    }
    content.truncate(cut);
    content.push_str("\n…(已截断)");
}

#[tauri::command]
pub async fn submit_prompt(
    _app: tauri::AppHandle,
    state: State<'_, AppState>,
    request: SubmitRequest,
    on_event: Channel<WireEvent>,
) -> Result<(), String> {
    info!("submit_prompt: prompt={}", request.prompt);

    // Session-cached config: load once per session so the permission gate's
    // approval cache (below) survives across prompts (C7).
    let config = {
        let mut cached = state.session_config.lock().await;
        if cached.is_none() {
            *cached = Some(
                deepseeknova_config::Config::load().map_err(|e| format!("config error: {e}"))?,
            );
        }
        cached
            .clone()
            .ok_or_else(|| "config unavailable".to_string())?
    };

    // 桌面端设置接线：系统提示词 / 推理参数 / 工具开关叠加到本次运行的 Config。
    let config = {
        let mut config = config;
        super::settings::apply_desktop_overrides(&mut config);
        config
    };

    let workspace_root = std::env::current_dir().unwrap_or_default();

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

    let provider =
        match deepseeknova_provider::factory::create_provider_for_task(provider_cfg, effort) {
            Ok(p) => p,
            Err(primary_err) => {
                // 推理参数里配置了降级模型时，主 provider 创建失败自动切换
                let fallback = super::settings::desktop_fallback_model()
                    .and_then(|m| config.resolve_provider_for_model(&m).map(|c| (m, c)));
                match fallback {
                    Some((name, fb_cfg)) => {
                        tracing::warn!(
                            "primary provider failed ({primary_err}); falling back to {name}"
                        );
                        deepseeknova_provider::factory::create_provider_for_task(fb_cfg, effort)
                            .map_err(|e| {
                                format!("provider error: {primary_err}; fallback error: {e}")
                            })?
                    }
                    None => return Err(format!("provider error: {primary_err}")),
                }
            }
        };

    // Desktop can prompt the user, so attach an approval responder to resolve
    // any permission-gate `Ask` decisions (the gate itself is only active when
    // config.permissions.enabled). Shared security + sandbox + gate wiring is
    // built once by the runtime composition helper.
    let responder: Arc<dyn deepseeknova_agent::ApprovalResponder> =
        Arc::new(DesktopApprovalResponder {
            approvals: state.approval_tx.clone(),
        });

    // Session-cached permission gate so its approval decisions persist across
    // prompts (the user isn't re-prompted for the same tool within a session).
    let gate = {
        let mut cached = state.session_gate.lock().await;
        if cached.is_none() {
            *cached = deepseeknova_runtime::permission_gate_for(&config, &workspace_root);
        }
        cached.clone()
    };

    // Session-cached MCP tools. Discovered once (spawning stdio server
    // processes), then reused so we don't spawn/kill servers per prompt.
    // Discovery runs outside the lock so a slow MCP handshake never blocks
    // other session commands (e.g. new_session) that touch this cache.
    let mcp_tools = {
        let cached = state.session_mcp_tools.lock().await.clone();
        match cached {
            Some(tools) => tools,
            None => {
                let discovered = deepseeknova_runtime::discover_mcp_tools(&config).await;
                let mut slot = state.session_mcp_tools.lock().await;
                // If another prompt populated the cache while we were
                // discovering, keep that set and drop ours; the surplus
                // connections (and their child processes) are released on drop.
                slot.get_or_insert(discovered).clone()
            }
        }
    };

    let agent = deepseeknova_runtime::build_agent(
        &config,
        workspace_root,
        provider.into(),
        config.agent.max_steps,
        gate,
        mcp_tools,
    )
    .map_err(|e| format!("agent error: {e}"))?
    .with_conversation_history(state.history.clone())
    .with_approval_responder(responder);

    let cancel = tokio_util::sync::CancellationToken::new();
    {
        let mut state_cancel = state.cancel.lock().await;
        *state_cancel = Some(cancel.clone());
    }

    let agent: Arc<dyn Runner> = Arc::new(agent);

    // 附件：一期将文本内容拼入 prompt 前缀（单文件上限 64KB，超出截断）。
    // 图片通道 RunInput.images 已预留，二期接入。
    let mut prompt = request.prompt;
    if let Some(paths) = request.attachments.as_ref().filter(|p| !p.is_empty()) {
        const MAX_ATTACHMENT_BYTES: usize = 64 * 1024;
        let mut prefix = String::new();
        for path in paths {
            match std::fs::read_to_string(path) {
                Ok(mut content) => {
                    truncate_attachment(&mut content, MAX_ATTACHMENT_BYTES);
                    prefix.push_str(&format!("【附件 {path}】\n```\n{content}\n```\n\n"));
                }
                Err(e) => {
                    prefix.push_str(&format!("【附件 {path}】读取失败：{e}\n\n"));
                }
            }
        }
        prompt = format!("{prefix}{prompt}");
    }
    if let Some(mode) = request.agent_mode.as_deref() {
        info!("agent_mode={mode}");
    }

    let input = RunInput {
        prompt,
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
    state.history.lock().await.clear();
    *state.session_config.lock().await = None;
    *state.session_gate.lock().await = None;
    *state.session_mcp_tools.lock().await = None;
    state.progress.reset();
    info!("new session started (history + session caches cleared)");
    Ok(())
}

#[tauri::command]
pub async fn respond_approval(
    state: State<'_, AppState>,
    request_id: String,
    approved: bool,
) -> Result<(), String> {
    info!("respond_approval: id={request_id} approved={approved}");
    let mut map = state.approval_tx.lock().await;
    if let Some(tx) = map.remove(&request_id) {
        let _ = tx.send(approved);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_attachment_keeps_short_content_untouched() {
        let mut s = "hello".to_string();
        truncate_attachment(&mut s, 64);
        assert_eq!(s, "hello");
    }

    #[test]
    fn truncate_attachment_cuts_ascii_at_exact_limit() {
        let mut s = "abcdef".to_string();
        truncate_attachment(&mut s, 4);
        assert_eq!(s, "abcd\n…(已截断)");
    }

    #[test]
    fn truncate_attachment_respects_utf8_char_boundary() {
        // 每个汉字 3 字节；限制 4 字节落在第二字中间，应回退到 3 字节而非 panic
        let mut s = "你好世界".to_string();
        truncate_attachment(&mut s, 4);
        assert_eq!(s, "你\n…(已截断)");
    }
}

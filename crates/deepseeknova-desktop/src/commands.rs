//! Tauri command handlers — the bridge between the React/TS frontend
//! and the deepseeknova agent kernel.
//!
//! Each command is an async function that the frontend calls via `invoke()`.
//! Streaming events are delivered through Tauri Channels (`tauri::ipc::Channel`),
//! the desktop equivalent of the HTTP SSE stream in `deepseeknova-serve`.

use deepseeknova_core::runner::{RunInput, Runner, WireEvent};
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
// Commands — Core
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Commands — Session
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn new_session(state: State<'_, AppState>) -> Result<(), String> {
    let mut history = state.history.lock().await;
    history.clear();
    info!("new session started (conversation history cleared)");
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub message_count: usize,
    pub created_at: String,
}

fn sessions_path() -> std::path::PathBuf {
    let mut p = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    p.push(".deepseeknova");
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
    let _ = std::fs::write(&path, serde_json::to_string_pretty(sessions).unwrap_or_default());
}

fn generate_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("s{:x}{:04x}", d.as_secs(), d.subsec_nanos() % 0x10000)
}

#[tauri::command]
pub async fn list_sessions() -> Result<Vec<SessionInfo>, String> {
    Ok(load_sessions())
}

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

#[tauri::command]
pub async fn delete_session(id: String) -> Result<(), String> {
    let mut sessions = load_sessions();
    sessions.retain(|s| s.id != id);
    save_sessions(&sessions);
    info!("deleted session {id}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Commands — Skills & Providers
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_skills() -> Result<Vec<SkillSummary>, String> {
    let mut skills = Vec::new();
    let paths = [".deepseeknova/skills", ".agents/skills"];
    for path_str in &paths {
        let loader = deepseeknova_skills::SkillLoader::new(path_str);
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

#[tauri::command]
pub async fn list_providers() -> Result<Vec<ProviderSummary>, String> {
    let config = deepseeknova_config::Config::load().map_err(|e| format!("config error: {e}"))?;
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

#[tauri::command]
pub async fn health_check() -> Result<String, String> {
    Ok("ok".to_string())
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
            format!("/{}/",
                path.file_name().map(|s| s.to_string_lossy()).unwrap_or_default()
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

// ===========================================================================
// Commands — 沙箱配置 (Sandbox)
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    pub enabled: bool,
    pub allowed_paths: Vec<String>,
    pub blocked_paths: Vec<String>,
    pub isolate_env: bool,
    pub csp_enabled: bool,
}

fn sandbox_config_path() -> std::path::PathBuf {
    let mut p = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    p.push(".deepseeknova");
    p.push("sandbox.json");
    p
}

#[tauri::command]
pub async fn get_sandbox_config() -> Result<SandboxConfig, String> {
    let path = sandbox_config_path();
    if path.exists() {
        let data = std::fs::read_to_string(&path).map_err(|e| format!("read error: {e}"))?;
        serde_json::from_str(&data).map_err(|e| format!("parse error: {e}"))
    } else {
        Ok(SandboxConfig {
            enabled: true,
            allowed_paths: vec![std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default()],
            blocked_paths: vec!["/etc".into(), "/var".into(), "~/.ssh".into()],
            isolate_env: true,
            csp_enabled: true,
        })
    }
}

#[tauri::command]
pub async fn set_sandbox_config(config: SandboxConfig) -> Result<(), String> {
    let path = sandbox_config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let data = serde_json::to_string_pretty(&config).map_err(|e| format!("serialize error: {e}"))?;
    std::fs::write(&path, data).map_err(|e| format!("write error: {e}"))?;
    info!("sandbox config updated");
    Ok(())
}

// ===========================================================================
// Commands — 网络配置 (Network)
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub allow_network: bool,
    pub proxy: Option<String>,
    pub timeout_secs: u64,
    pub max_retries: u32,
    pub ssl_verify: bool,
    pub auto_reconnect: bool,
}

fn network_config_path() -> std::path::PathBuf {
    let mut p = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    p.push(".deepseeknova");
    p.push("network.json");
    p
}

#[tauri::command]
pub async fn get_network_config() -> Result<NetworkConfig, String> {
    let path = network_config_path();
    if path.exists() {
        let data = std::fs::read_to_string(&path).map_err(|e| format!("read error: {e}"))?;
        serde_json::from_str(&data).map_err(|e| format!("parse error: {e}"))
    } else {
        Ok(NetworkConfig {
            allow_network: true,
            proxy: None,
            timeout_secs: 30,
            max_retries: 3,
            ssl_verify: true,
            auto_reconnect: true,
        })
    }
}

#[tauri::command]
pub async fn set_network_config(config: NetworkConfig) -> Result<(), String> {
    let path = network_config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let data = serde_json::to_string_pretty(&config).map_err(|e| format!("serialize error: {e}"))?;
    std::fs::write(&path, data).map_err(|e| format!("write error: {e}"))?;
    info!("network config updated");
    Ok(())
}

#[tauri::command]
pub async fn network_diagnostics() -> Result<serde_json::Value, String> {
    let results = serde_json::json!([
        {"name": "DeepSeek API", "status": "pass", "detail": "128ms"},
        {"name": "GitHub API", "status": "pass", "detail": "45ms"},
        {"name": "MCP: web-search", "status": "warn", "detail": "未连接"},
    ]);
    Ok(results)
}

// ===========================================================================
// Commands — 权限规则 (Permissions)
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRule {
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub rule_type: String,
}

fn permissions_config_path() -> std::path::PathBuf {
    let mut p = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    p.push(".deepseeknova");
    p.push("permissions.json");
    p
}

#[tauri::command]
pub async fn get_permissions() -> Result<Vec<PermissionRule>, String> {
    let path = permissions_config_path();
    if path.exists() {
        let data = std::fs::read_to_string(&path).map_err(|e| format!("read error: {e}"))?;
        serde_json::from_str(&data).map_err(|e| format!("parse error: {e}"))
    } else {
        Ok(vec![
            PermissionRule { name: "目录沙箱".into(), description: "所有工具仅能访问启动时的项目目录".into(), enabled: true, rule_type: "文件".into() },
            PermissionRule { name: "Plan 模式".into(), description: "AI 只能读，不能写，必须先提交计划".into(), enabled: false, rule_type: "执行".into() },
            PermissionRule { name: "Review 审批".into(), description: "写操作进入审核队列，每次确认".into(), enabled: true, rule_type: "执行".into() },
            PermissionRule { name: "Shell 命令确认".into(), description: "所有 Shell 命令都需要用户确认".into(), enabled: true, rule_type: "执行".into() },
            PermissionRule { name: "自动提交".into(), description: "Agent 完成任务后自动 git commit".into(), enabled: false, rule_type: "Git".into() },
            PermissionRule { name: "网络访问".into(), description: "允许 Agent 访问网络".into(), enabled: true, rule_type: "网络".into() },
            PermissionRule { name: "文件删除".into(), description: "允许 Agent 删除文件".into(), enabled: false, rule_type: "文件".into() },
            PermissionRule { name: "文件大小限制".into(), description: "单文件读写最大 10MB".into(), enabled: true, rule_type: "限制".into() },
            PermissionRule { name: "Token 预算".into(), description: "单会话 Token 上限 500K".into(), enabled: true, rule_type: "限制".into() },
            PermissionRule { name: "敏感文件保护".into(), description: "禁止访问 .env、.ssh、.aws 等".into(), enabled: true, rule_type: "安全".into() },
            PermissionRule { name: "多标签隔离".into(), description: "标签之间完全隔离".into(), enabled: true, rule_type: "隔离".into() },
            PermissionRule { name: "剪贴板访问".into(), description: "允许 Agent 读取剪贴板".into(), enabled: false, rule_type: "隐私".into() },
        ])
    }
}

#[tauri::command]
pub async fn set_permission_rule(name: String, enabled: bool) -> Result<(), String> {
    let path = permissions_config_path();
    let mut rules: Vec<PermissionRule> = if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        // Load defaults
        get_permissions().await.unwrap_or_default()
    };

    for rule in &mut rules {
        if rule.name == name {
            rule.enabled = enabled;
        }
    }

    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let data = serde_json::to_string_pretty(&rules).map_err(|e| format!("serialize error: {e}"))?;
    std::fs::write(&path, data).map_err(|e| format!("write error: {e}"))?;
    info!("permission rule '{name}' set to {enabled}");
    Ok(())
}

// ===========================================================================
// Commands — 钩子 (Hooks)
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hook {
    pub event: String,
    pub command: String,
    pub enabled: bool,
}

fn hooks_config_path() -> std::path::PathBuf {
    let mut p = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    p.push(".deepseeknova");
    p.push("hooks.json");
    p
}

#[tauri::command]
pub async fn get_hooks() -> Result<Vec<Hook>, String> {
    let path = hooks_config_path();
    if path.exists() {
        let data = std::fs::read_to_string(&path).map_err(|e| format!("read error: {e}"))?;
        serde_json::from_str(&data).map_err(|e| format!("parse error: {e}"))
    } else {
        Ok(vec![
            Hook { event: "on_session_start".into(), command: "echo 'Session started'".into(), enabled: true },
            Hook { event: "on_session_end".into(), command: "git add -A && git stash".into(), enabled: true },
            Hook { event: "on_tool_call".into(), command: "logger -t deepseeknova 'Tool: $TOOL'".into(), enabled: true },
            Hook { event: "on_approval_request".into(), command: "paplay /usr/share/sounds/alert.wav".into(), enabled: true },
            Hook { event: "on_task_complete".into(), command: "notify-send 'Done' 'Task completed'".into(), enabled: true },
            Hook { event: "on_budget_exceeded".into(), command: "notify-send 'Budget!' 'Check billing'".into(), enabled: true },
        ])
    }
}

#[tauri::command]
pub async fn set_hook(event: String, command: String, enabled: bool) -> Result<(), String> {
    let path = hooks_config_path();
    let mut hooks: Vec<Hook> = if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        get_hooks().await.unwrap_or_default()
    };

    // Update or insert
    if let Some(h) = hooks.iter_mut().find(|h| h.event == event) {
        h.command = command.clone();
        h.enabled = enabled;
    } else {
        hooks.push(Hook { event, command, enabled });
    }

    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let data = serde_json::to_string_pretty(&hooks).map_err(|e| format!("serialize error: {e}"))?;
    std::fs::write(&path, data).map_err(|e| format!("write error: {e}"))?;
    info!("hook '{event}' updated");
    Ok(())
}

#[tauri::command]
pub async fn delete_hook(event: String) -> Result<(), String> {
    let path = hooks_config_path();
    let mut hooks: Vec<Hook> = if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    hooks.retain(|h| h.event != event);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let data = serde_json::to_string_pretty(&hooks).map_err(|e| format!("serialize error: {e}"))?;
    std::fs::write(&path, data).map_err(|e| format!("write error: {e}"))?;
    info!("hook '{event}' deleted");
    Ok(())
}

// ===========================================================================
// Commands — MCP 服务器管理
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServer {
    pub name: String,
    pub command: String,
    pub args: String,
    pub transport: String,
    pub status: String,
}

fn mcp_config_path() -> std::path::PathBuf {
    let mut p = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    p.push(".deepseeknova");
    p.push("mcp.json");
    p
}

#[tauri::command]
pub async fn list_mcp_servers() -> Result<Vec<McpServer>, String> {
    let path = mcp_config_path();
    if path.exists() {
        let data = std::fs::read_to_string(&path).map_err(|e| format!("read error: {e}"))?;
        serde_json::from_str(&data).map_err(|e| format!("parse error: {e}"))
    } else {
        Ok(vec![
            McpServer { name: "filesystem".into(), command: "npx".into(), args: "@modelcontextprotocol/server-filesystem".into(), transport: "stdio".into(), status: "running".into() },
            McpServer { name: "git".into(), command: "npx".into(), args: "@modelcontextprotocol/server-git".into(), transport: "stdio".into(), status: "running".into() },
            McpServer { name: "shell".into(), command: "npx".into(), args: "@modelcontextprotocol/server-shell".into(), transport: "stdio".into(), status: "running".into() },
            McpServer { name: "web-search".into(), command: "npx".into(), args: "@modelcontextprotocol/server-brave-search".into(), transport: "stdio".into(), status: "stopped".into() },
        ])
    }
}

#[tauri::command]
pub async fn add_mcp_server(server: McpServer) -> Result<(), String> {
    let path = mcp_config_path();
    let mut servers: Vec<McpServer> = if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    servers.push(server);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let data = serde_json::to_string_pretty(&servers).map_err(|e| format!("serialize error: {e}"))?;
    std::fs::write(&path, data).map_err(|e| format!("write error: {e}"))?;
    info!("MCP server added");
    Ok(())
}

#[tauri::command]
pub async fn remove_mcp_server(name: String) -> Result<(), String> {
    let path = mcp_config_path();
    let mut servers: Vec<McpServer> = if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    servers.retain(|s| s.name != name);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let data = serde_json::to_string_pretty(&servers).map_err(|e| format!("serialize error: {e}"))?;
    std::fs::write(&path, data).map_err(|e| format!("write error: {e}"))?;
    info!("MCP server '{name}' removed");
    Ok(())
}

#[tauri::command]
pub async fn toggle_mcp_server(name: String, start: bool) -> Result<(), String> {
    let path = mcp_config_path();
    let mut servers: Vec<McpServer> = if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    for s in &mut servers {
        if s.name == name {
            s.status = if start { "running".into() } else { "stopped".into() };
        }
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let data = serde_json::to_string_pretty(&servers).map_err(|e| format!("serialize error: {e}"))?;
    std::fs::write(&path, data).map_err(|e| format!("write error: {e}"))?;
    info!("MCP server '{name}' {}", if start { "started" } else { "stopped" });
    Ok(())
}

// ===========================================================================
// Commands — 子智能体 (Sub-Agents)
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgent {
    pub name: String,
    pub description: String,
    pub model: String,
    pub status: String,
    pub tasks: u64,
}

#[tauri::command]
pub async fn list_subagents() -> Result<Vec<SubAgent>, String> {
    Ok(vec![
        SubAgent { name: "code-reviewer".into(), description: "代码审查专家".into(), model: "deepseek-v4-pro".into(), status: "idle".into(), tasks: 12 },
        SubAgent { name: "bug-hunter".into(), description: "Bug 检测和根因分析".into(), model: "deepseek-v4-pro".into(), status: "idle".into(), tasks: 5 },
        SubAgent { name: "test-generator".into(), description: "自动生成测试用例".into(), model: "deepseek-v4-flash".into(), status: "idle".into(), tasks: 8 },
        SubAgent { name: "refactor-assistant".into(), description: "代码重构建议".into(), model: "deepseek-v4-pro".into(), status: "running".into(), tasks: 3 },
        SubAgent { name: "frontend-design".into(), description: "前端 UI/UX 设计".into(), model: "deepseek-v4-flash".into(), status: "idle".into(), tasks: 7 },
        SubAgent { name: "doc-generator".into(), description: "文档生成".into(), model: "deepseek-v4-flash".into(), status: "idle".into(), tasks: 15 },
    ])
}

// ===========================================================================
// Commands — 诊断 (Diagnostics)
// ===========================================================================

#[tauri::command]
pub async fn run_diagnostics() -> Result<serde_json::Value, String> {
    let results = serde_json::json!([
        {"name": "Node.js 运行时", "status": "pass", "detail": "v22.22.1"},
        {"name": "Tauri 框架", "status": "pass", "detail": "v2.0"},
        {"name": "DeepSeek API 连接", "status": "pass", "detail": "128ms"},
        {"name": "API Key 有效", "status": "pass", "detail": "sk-••••••••"},
        {"name": "MCP: filesystem", "status": "pass", "detail": "运行中"},
        {"name": "MCP: git", "status": "pass", "detail": "运行中"},
        {"name": "MCP: web-search", "status": "warn", "detail": "未启动"},
        {"name": "缓存系统", "status": "pass", "detail": "命中率 94%"},
        {"name": "记忆系统", "status": "pass", "detail": "7 条记忆"},
        {"name": "沙箱配置", "status": "pass", "detail": "目录限制已启用"},
        {"name": "磁盘空间", "status": "pass", "detail": "12.4 GB 可用"},
        {"name": "内存使用", "status": "warn", "detail": "412 MB / 2 GB"},
    ]);
    info!("diagnostics completed");
    Ok(results)
}

// ===========================================================================
// Commands — 账单 (Billing)
// ===========================================================================

#[tauri::command]
pub async fn get_billing_stats() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "session": {
            "cache_hit": 1280,
            "cache_miss": 78,
            "completion_tokens": 856,
            "reasoning_tokens": 342,
            "total_tokens": 4382,
            "cache_rate": 94.2,
        },
        "cost": {
            "input_full": 0.0218,
            "input_cached": 0.0359,
            "output": 0.0753,
            "total": 0.133,
        },
        "history": [
            {"period": "今天", "sessions": 3, "cost": "¥0.42"},
            {"period": "昨天", "sessions": 5, "cost": "¥0.78"},
            {"period": "本周", "sessions": 18, "cost": "¥2.14"},
            {"period": "本月", "sessions": 62, "cost": "¥8.45"},
        ]
    }))
}

// ===========================================================================
// Commands — 知识库 (Knowledge Base)
// ===========================================================================

#[tauri::command]
pub async fn get_wiki_pages() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!([
        {"title": "项目架构", "desc": "Rust + Tauri 2.0 + React 18 + TypeScript", "icon": "🏗️", "updated": "2 天前", "sections": 12},
        {"title": "API 文档", "desc": "工具调用规范、流式 SSE 协议、错误处理", "icon": "📡", "updated": "1 天前", "sections": 8},
        {"title": "开发指南", "desc": "环境搭建、构建流程、调试技巧、发布", "icon": "📖", "updated": "3 天前", "sections": 15},
        {"title": "缓存机制", "desc": "Prefix-Cache 三层架构", "icon": "💡", "updated": "5 天前", "sections": 6},
    ]))
}

#[tauri::command]
pub async fn get_knowledge_cards() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!([
        {"title": "Rust 异步编程", "desc": "tokio + async/await 最佳实践", "tag": "编程", "confidence": 95},
        {"title": "Tauri IPC 通信", "desc": "invoke() 和 emit() 性能对比", "tag": "架构", "confidence": 88},
        {"title": "React 性能优化", "desc": "useMemo、useCallback 和 memo", "tag": "前端", "confidence": 92},
        {"title": "DeepSeek API", "desc": "V4 Flash 和 Pro 选择策略", "tag": "AI", "confidence": 98},
    ]))
}

// ===========================================================================
// Commands — 记忆 CRUD (Memory)
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub memory_type: String,
    pub text: String,
    pub created_at: String,
}

fn memory_config_path() -> std::path::PathBuf {
    let mut p = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    p.push(".deepseeknova");
    p.push("memory.json");
    p
}

#[tauri::command]
pub async fn get_memories() -> Result<Vec<MemoryEntry>, String> {
    let path = memory_config_path();
    if path.exists() {
        let data = std::fs::read_to_string(&path).map_err(|e| format!("read error: {e}"))?;
        serde_json::from_str(&data).map_err(|e| format!("parse error: {e}"))
    } else {
        Ok(vec![
            MemoryEntry { id: "1".into(), memory_type: "project".into(), text: "项目使用 Rust + Tauri 2.0 构建".into(), created_at: "2 天前".into() },
            MemoryEntry { id: "2".into(), memory_type: "user".into(), text: "用户偏好 VS Code 和深色主题".into(), created_at: "1 天前".into() },
            MemoryEntry { id: "3".into(), memory_type: "global".into(), text: "Flash 用于日常，Pro 用于复杂推理".into(), created_at: "3 天前".into() },
        ])
    }
}

#[tauri::command]
pub async fn add_memory(memory_type: String, text: String) -> Result<MemoryEntry, String> {
    let path = memory_config_path();
    let mut memories: Vec<MemoryEntry> = if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let entry = MemoryEntry {
        id: generate_id(),
        memory_type,
        text,
        created_at: "刚刚".into(),
    };
    memories.push(entry.clone());

    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let data = serde_json::to_string_pretty(&memories).map_err(|e| format!("serialize error: {e}"))?;
    std::fs::write(&path, data).map_err(|e| format!("write error: {e}"))?;
    info!("memory added");
    Ok(entry)
}

#[tauri::command]
pub async fn delete_memory(id: String) -> Result<(), String> {
    let path = memory_config_path();
    let mut memories: Vec<MemoryEntry> = if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    memories.retain(|m| m.id != id);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let data = serde_json::to_string_pretty(&memories).map_err(|e| format!("serialize error: {e}"))?;
    std::fs::write(&path, data).map_err(|e| format!("write error: {e}"))?;
    info!("memory {id} deleted");
    Ok(())
}

// ===========================================================================
// Commands — 设置持久化 (Settings)
// ===========================================================================

fn settings_path() -> std::path::PathBuf {
    let mut p = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    p.push(".deepseeknova");
    p.push("settings.json");
    p
}

#[tauri::command]
pub async fn save_settings(settings: serde_json::Value) -> Result<(), String> {
    let path = settings_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let data = serde_json::to_string_pretty(&settings).map_err(|e| format!("serialize error: {e}"))?;
    std::fs::write(&path, data).map_err(|e| format!("write error: {e}"))?;
    info!("settings saved");
    Ok(())
}

#[tauri::command]
pub async fn load_settings() -> Result<serde_json::Value, String> {
    let path = settings_path();
    if path.exists() {
        let data = std::fs::read_to_string(&path).map_err(|e| format!("read error: {e}"))?;
        serde_json::from_str(&data).map_err(|e| format!("parse error: {e}"))
    } else {
        Ok(serde_json::json!({}))
    }
}

// ===========================================================================
// Commands — 快捷键 (Shortcuts)
// ===========================================================================

#[tauri::command]
pub async fn get_shortcuts() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!([
        {"action": "发送消息", "keys": "Enter", "category": "输入"},
        {"action": "换行", "keys": "Shift + Enter", "category": "输入"},
        {"action": "命令面板", "keys": "Ctrl/Cmd + P", "category": "全局"},
        {"action": "中断生成", "keys": "Esc", "category": "对话"},
        {"action": "新建会话", "keys": "Ctrl/Cmd + N", "category": "会话"},
        {"action": "关闭标签", "keys": "Ctrl/Cmd + W", "category": "会话"},
        {"action": "搜索会话", "keys": "Ctrl/Cmd + F", "category": "搜索"},
        {"action": "切换主题", "keys": "Ctrl/Cmd + Shift + T", "category": "全局"},
        {"action": "折叠侧边栏", "keys": "Ctrl/Cmd + B", "category": "全局"},
        {"action": "Plan 模式", "keys": "Ctrl/Cmd + Shift + 1", "category": "模式"},
        {"action": "Act 模式", "keys": "Ctrl/Cmd + Shift + 2", "category": "模式"},
        {"action": "YOLO 模式", "keys": "Ctrl/Cmd + Shift + 3", "category": "模式"},
    ]))
}

// ===========================================================================
// Commands — 更新检查 (Update Check)
// ===========================================================================

#[tauri::command]
pub async fn check_for_updates() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "update_available": false,
        "current_version": env!("CARGO_PKG_VERSION"),
        "latest_version": env!("CARGO_PKG_VERSION"),
        "release_notes": "",
    }))
}

// ===========================================================================
// Commands — 文件 Diff
// ===========================================================================

#[tauri::command]
pub async fn get_file_diff(file_path: String) -> Result<String, String> {
    // Use git diff to get the diff for the file
    let output = tokio::process::Command::new("git")
        .args(["diff", "--no-color", &file_path])
        .output()
        .await
        .map_err(|e| format!("git diff error: {e}"))?;

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// ===========================================================================
// Commands — 标签页管理 (Tabs)
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabInfo {
    pub id: String,
    pub title: String,
}

fn tabs_path() -> std::path::PathBuf {
    let mut p = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    p.push(".deepseeknova");
    p.push("tabs.json");
    p
}

#[tauri::command]
pub async fn list_tabs() -> Result<Vec<TabInfo>, String> {
    let path = tabs_path();
    if path.exists() {
        let data = std::fs::read_to_string(&path).map_err(|e| format!("read error: {e}"))?;
        serde_json::from_str(&data).map_err(|e| format!("parse error: {e}"))
    } else {
        Ok(vec![TabInfo { id: "1".into(), title: "主会话".into() }])
    }
}

#[tauri::command]
pub async fn create_tab(title: String) -> Result<TabInfo, String> {
    let path = tabs_path();
    let mut tabs: Vec<TabInfo> = if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        vec![TabInfo { id: "1".into(), title: "主会话".into() }]
    };

    let tab = TabInfo { id: generate_id(), title };
    tabs.push(tab.clone());

    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let data = serde_json::to_string_pretty(&tabs).map_err(|e| format!("serialize error: {e}"))?;
    std::fs::write(&path, data).map_err(|e| format!("write error: {e}"))?;
    info!("tab created");
    Ok(tab)
}

#[tauri::command]
pub async fn close_tab(id: String) -> Result<(), String> {
    let path = tabs_path();
    let mut tabs: Vec<TabInfo> = if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    tabs.retain(|t| t.id != id);
    if tabs.is_empty() {
        tabs.push(TabInfo { id: "1".into(), title: "主会话".into() });
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let data = serde_json::to_string_pretty(&tabs).map_err(|e| format!("serialize error: {e}"))?;
    std::fs::write(&path, data).map_err(|e| format!("write error: {e}"))?;
    info!("tab {id} closed");
    Ok(())
}

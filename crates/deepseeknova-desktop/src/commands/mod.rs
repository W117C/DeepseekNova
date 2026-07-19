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

// ---------------------------------------------------------------------------
// Commands — Session
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Commands — Skills & Providers
// ---------------------------------------------------------------------------

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

// ===========================================================================
// Commands — 子智能体 (Sub-Agents)
// ===========================================================================

// ===========================================================================
// Commands — 诊断 (Diagnostics)
// ===========================================================================

// ===========================================================================
// Commands — 账单 (Billing)
// ===========================================================================

// ===========================================================================
// Commands — 知识库 (Knowledge Base)
// ===========================================================================

// ===========================================================================
// Commands — 记忆 (Memory — uses core's SQLite FTS5 MemoryStore)
// ===========================================================================

// ===========================================================================
// Commands — 设置持久化 (Settings)
// ===========================================================================

fn settings_path() -> std::path::PathBuf {
    let mut p = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    p.push(".deepseeknova");
    p.push("settings.json");
    p
}

// ===========================================================================
// Commands — 快捷键 (Shortcuts)
// ===========================================================================

// ===========================================================================
// Commands — 更新检查 (Update Check)
// ===========================================================================

// ===========================================================================
// Commands — 文件 Diff
// ===========================================================================

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

// ---------------------------------------------------------------------------
// Sub-modules
// ---------------------------------------------------------------------------

pub mod billing;
pub mod core;
pub mod diagnostics;
pub mod hooks;
pub mod knowledge;
pub mod mcp;
pub mod memory;
pub mod misc;
pub mod network;
pub mod permissions;
pub mod sandbox;
pub mod sessions;
pub mod settings;
pub mod skills;
pub mod subagents;
pub mod tabs;
pub mod workspace;

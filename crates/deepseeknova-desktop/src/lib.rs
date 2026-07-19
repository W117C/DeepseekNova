//! # deepseeknova-desktop
//!
//! Native desktop application for the deepseeknova AI agent framework.
//! Built with Tauri 2.x — Rust backend with a React/TypeScript frontend.

mod commands;

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    Manager,
};

/// Type alias for the approval channel sender.
type ApprovalSender = tokio::sync::oneshot::Sender<(String, bool)>;
/// Type alias for the shared approval channel.
type ApprovalChannel = std::sync::Arc<tokio::sync::Mutex<Option<ApprovalSender>>>;

/// Cumulative usage statistics gathered from all agent runs.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct CumulativeUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cache_hit_tokens: u64,
    pub cache_miss_tokens: u64,
    pub reasoning_tokens: u64,
    pub run_count: u64,
}

pub struct AppState {
    pub runner: tokio::sync::Mutex<Option<Box<dyn deepseeknova_core::Runner + Send>>>,
    pub cancel: tokio::sync::Mutex<Option<tokio_util::sync::CancellationToken>>,
    pub approval_tx: ApprovalChannel,
    pub history: std::sync::Arc<tokio::sync::Mutex<Vec<deepseeknova_core::Message>>>,
    pub usage: std::sync::Arc<tokio::sync::Mutex<CumulativeUsage>>,
}

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            let _ = app.get_webview_window("main").map(|w| w.set_focus());
        }))
        .manage(AppState {
            runner: tokio::sync::Mutex::new(None),
            cancel: tokio::sync::Mutex::new(None),
            approval_tx: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
            history: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
            usage: std::sync::Arc::new(tokio::sync::Mutex::new(CumulativeUsage::default())),
        })
        .invoke_handler(tauri::generate_handler![
            // Core
            commands::core::submit_prompt,
            commands::core::cancel_run,
            commands::core::new_session,
            commands::core::respond_approval,
            commands::core::health_check,
            commands::core::get_config,
            commands::core::get_capabilities,
            // Sessions
            commands::sessions::list_sessions,
            commands::sessions::create_session,
            commands::sessions::delete_session,
            // Skills & Providers
            commands::skills::list_skills,
            commands::skills::list_providers,
            // Workspace
            commands::workspace::get_workspace_files,
            commands::workspace::get_file_diff,
            // Sandbox
            commands::sandbox::get_sandbox_config,
            commands::sandbox::set_sandbox_config,
            // Network
            commands::network::get_network_config,
            commands::network::set_network_config,
            commands::network::network_diagnostics,
            // Permissions
            commands::permissions::get_permissions,
            commands::permissions::set_permission_rule,
            // Hooks
            commands::hooks::get_hooks,
            commands::hooks::set_hook,
            commands::hooks::delete_hook,
            // MCP
            commands::mcp::list_mcp_servers,
            commands::mcp::add_mcp_server,
            commands::mcp::remove_mcp_server,
            commands::mcp::toggle_mcp_server,
            // Sub-Agents
            commands::subagents::list_subagents,
            // Diagnostics
            commands::diagnostics::run_diagnostics,
            // Billing
            commands::billing::get_billing_stats,
            // Knowledge Base
            commands::knowledge::get_wiki_pages,
            commands::knowledge::get_knowledge_cards,
            // Memory
            commands::memory::get_memories,
            commands::memory::add_memory,
            commands::memory::delete_memory,
            // Settings
            commands::settings::save_settings,
            commands::settings::load_settings,
            // Shortcuts
            commands::misc::get_shortcuts,
            // Update
            commands::misc::check_for_updates,
            // Tabs
            commands::tabs::list_tabs,
            commands::tabs::create_tab,
            commands::tabs::close_tab,
        ])
        .setup(|app| {
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let show = MenuItem::with_id(app, "show", "Show Window", true, None::<&str>)?;
            let menu =
                Menu::with_items(app, &[&show, &PredefinedMenuItem::separator(app)?, &quit])?;

            let _tray = TrayIconBuilder::new()
                .icon(
                    app.default_window_icon()
                        .cloned()
                        .unwrap_or_else(|| tauri::image::Image::new(&[0, 0, 0, 0], 1, 1)),
                )
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => app.exit(0),
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    _ => {}
                })
                .build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to build Tauri app");

    app.run(|_app_handle, _event| {});
}

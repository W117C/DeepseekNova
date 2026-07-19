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

pub struct AppState {
    pub runner: tokio::sync::Mutex<Option<Box<dyn deepseeknova_core::Runner + Send>>>,
    pub cancel: tokio::sync::Mutex<Option<tokio_util::sync::CancellationToken>>,
    pub approval_tx: ApprovalChannel,
    pub history: std::sync::Arc<tokio::sync::Mutex<Vec<deepseeknova_core::Message>>>,
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
        })
        .invoke_handler(tauri::generate_handler![
            // Core
            commands::submit_prompt,
            commands::cancel_run,
            commands::new_session,
            commands::respond_approval,
            commands::health_check,
            commands::get_config,
            commands::get_capabilities,
            // Sessions
            commands::list_sessions,
            commands::create_session,
            commands::delete_session,
            // Skills & Providers
            commands::list_skills,
            commands::list_providers,
            // Workspace
            commands::get_workspace_files,
            commands::get_file_diff,
            // Sandbox
            commands::get_sandbox_config,
            commands::set_sandbox_config,
            // Network
            commands::get_network_config,
            commands::set_network_config,
            commands::network_diagnostics,
            // Permissions
            commands::get_permissions,
            commands::set_permission_rule,
            // Hooks
            commands::get_hooks,
            commands::set_hook,
            commands::delete_hook,
            // MCP
            commands::list_mcp_servers,
            commands::add_mcp_server,
            commands::remove_mcp_server,
            commands::toggle_mcp_server,
            // Sub-Agents
            commands::list_subagents,
            // Diagnostics
            commands::run_diagnostics,
            // Billing
            commands::get_billing_stats,
            // Knowledge Base
            commands::get_wiki_pages,
            commands::get_knowledge_cards,
            // Memory
            commands::get_memories,
            commands::add_memory,
            commands::delete_memory,
            // Settings
            commands::save_settings,
            commands::load_settings,
            // Shortcuts
            commands::get_shortcuts,
            // Update
            commands::check_for_updates,
            // Tabs
            commands::list_tabs,
            commands::create_tab,
            commands::close_tab,
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

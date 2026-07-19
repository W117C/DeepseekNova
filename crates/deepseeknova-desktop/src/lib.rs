//! # deepseeknova-desktop
//!
//! Native desktop application for the deepseeknova AI agent framework.
//! Built with Tauri 2.x — Rust backend with a React/TypeScript frontend.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────┐
//! │  Webview (React + TS, Vite)                  │
//! │    bridge.ts ──invoke──▶ Tauri Commands      │
//! │    bridge.ts ◀─Channel── agent:event stream   │
//! └───────────────▲──────────────────────────────┘
//!         commands │                  events
//! ┌───────────────┴──────────────────────────────┐
//! │  commands.rs  (Tauri command handlers)        │
//! │    └── runner::run_stream() → Channel         │
//! └───────────────▲──────────────────────────────┘
//!                 │
//! ┌───────────────┴──────────────────────────────┐
//! │  deepseeknova-runtime / deepseeknova-agent (Rust)     │
//! │  (same kernel as CLI, TUI, HTTP server)       │
//! └──────────────────────────────────────────────┘
//! ```

mod commands;

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    Manager,
};

/// Application state shared across commands.
pub struct AppState {
    pub runner: tokio::sync::Mutex<Option<Box<dyn deepseeknova_core::Runner + Send>>>,
    pub cancel: tokio::sync::Mutex<Option<tokio_util::sync::CancellationToken>>,
    /// Channel for delivering approval responses to a waiting agent.
    pub approval_tx:
        std::sync::Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<(String, bool)>>>>,
    /// Persistent conversation store for the current session. Shared across
    /// successive `submit_prompt` calls so the agent remembers prior turns
    /// (and DeepSeek-V4 reasoning replay spans user turns). Cleared by
    /// `new_session`.
    pub history: std::sync::Arc<tokio::sync::Mutex<Vec<deepseeknova_core::Message>>>,
}

/// Run the Tauri desktop application.
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
            commands::submit_prompt,
            commands::cancel_run,
            commands::new_session,
            commands::list_skills,
            commands::list_providers,
            commands::get_config,
            commands::get_capabilities,
            commands::health_check,
            commands::respond_approval,
            commands::get_workspace_files,
            commands::list_sessions,
            commands::create_session,
            commands::delete_session,
        ])
        .setup(|app| {
            // Build system tray
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
            // Minimize to tray instead of closing
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to build Tauri app");

    app.run(|_app_handle, _event| {});
}

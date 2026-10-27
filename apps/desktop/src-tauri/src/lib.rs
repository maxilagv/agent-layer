//! LayerAgent desktop shell.
//!
//! The shell is a thin client of the local LayerAgent daemon. It owns no
//! privilege of its own: the webview only reaches the daemon through the
//! bounded commands of this crate, and the daemon enforces every rule.

pub mod commands;
pub mod daemon;
pub mod error;
pub mod handlers;
pub mod state;
pub mod tray;

#[cfg(not(unix))]
compile_error!(
    "the LayerAgent desktop app targets unix: the daemon speaks HTTP over a UNIX socket"
);

use tauri::{Manager, WindowEvent};

/// Start the desktop shell.
pub fn run() -> anyhow::Result<()> {
    let endpoint = daemon::Endpoint::resolve()?;
    let state = state::AppState::new(endpoint)?;
    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            handlers::bootstrap,
            handlers::list_projects,
            handlers::list_sessions,
            handlers::session_events,
            handlers::list_tasks,
            handlers::create_task,
            handlers::task,
            handlers::get_task,
            handlers::task_events,
            handlers::cancel_task,
            handlers::correct_task,
            handlers::approve,
            handlers::artifact,
            handlers::list_connections,
            handlers::connection_action,
            handlers::usage,
            handlers::config,
            handlers::models,
            handlers::list_models,
            handlers::mcp_capabilities,
            handlers::daemon_status,
            handlers::stop_daemon,
            handlers::subscribe,
            handlers::unsubscribe,
        ])
        .setup(|app| {
            tray::install(app.handle())?;
            Ok(())
        })
        .on_window_event(|window, event| match event {
            // Closing the window hides it: the daemon keeps working and the tray
            // keeps the state visible.
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
                window.app_handle().state::<state::AppState>().cancel_all();
            }
            WindowEvent::Destroyed => {
                window.app_handle().state::<state::AppState>().cancel_all();
            }
            _ => {}
        })
        .run(tauri::generate_context!())?;
    Ok(())
}

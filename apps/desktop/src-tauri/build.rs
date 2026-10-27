// Build script for the LayerAgent desktop app.
//
// The frontend bundle lives outside this crate (`../../frontend/dist`) and is
// produced by a separate build. The desktop crate must stay compilable while
// that bundle is missing, so the missing directory is only reported as a
// warning here: the assets are embedded by `tauri` when the `custom-protocol`
// feature is enabled, which is exactly when a production bundle is built.

use std::path::Path;

/// Commands of the shell. They are declared in the application manifest so
/// that the capability file can grant them to a window explicitly.
const COMMANDS: &[&str] = &[
    "bootstrap",
    "list_projects",
    "list_sessions",
    "session_events",
    "list_tasks",
    "create_task",
    "task",
    "task_events",
    "cancel_task",
    "correct_task",
    "approve",
    "artifact",
    "list_connections",
    "connection_action",
    "usage",
    "config",
    "models",
    "mcp_capabilities",
    "daemon_status",
    "stop_daemon",
    "subscribe",
];

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let frontend = Path::new(&manifest_dir).join("../../frontend/dist");
    if !frontend.exists() {
        println!(
            "cargo:warning=frontend bundle not found; the desktop crate still compiles, \
             but a production bundle requires the frontend to be built first"
        );
    }
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to prepare the Tauri application");
}

//! System tray entry of the desktop shell.
//!
//! The tray shows the state of the daemon and the number of active tasks. It
//! offers three actions: open the window, ask the daemon to stop and leave the
//! application. Closing the window only hides it, so the daemon keeps running.

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::commands;
use crate::daemon;

/// Identifier of the tray icon.
pub const TRAY_ID: &str = "layeragent-tray";

/// Identifier of the window opened from the tray.
pub const WINDOW_LABEL: &str = "main";

/// Build the tray menu.
fn build_menu<R: Runtime>(app: &AppHandle<R>, status: &str) -> tauri::Result<Menu<R>> {
    let status = MenuItem::with_id(
        app,
        "status",
        format!("Estado: {status}"),
        false,
        None::<&str>,
    )?;
    let open = MenuItem::with_id(app, "open", "Abrir ventana", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "Detener daemon", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Salir de la aplicación", true, None::<&str>)?;
    Menu::with_items(app, &[&status, &open, &stop, &quit])
}

/// Install the tray icon.
pub fn install<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let menu = build_menu(app, "desconocido")?;
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png"))?;
    TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .icon(icon)
        .tooltip("LayerAgent")
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.0.as_str() {
            "open" => open_window(app),
            "stop" => stop_daemon(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}

/// Show the main window again.
pub fn open_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Ask the daemon to stop and report the cancellations that stayed uncertain.
///
/// The daemon is never killed from the shell: the request goes through the
/// documented shutdown route and the result is published to the webview.
pub fn stop_daemon<R: Runtime>(app: &AppHandle<R>) {
    let handle = (*app).clone();
    tauri::async_runtime::spawn(async move {
        let result = commands::stop_daemon(&handle, None).await;
        let payload = match &result {
            Ok(value) => value.clone(),
            Err(error) => serde_json::json!({
              "stopped": false,
              "uncertain_tasks": [],
              "error": error.to_string(),
            }),
        };
        let _ = handle.emit(daemon::STATUS_CHANNEL, payload);
        refresh(&handle, false, 0);
    });
}

/// Refresh the tray with the current state of the daemon.
pub fn refresh<R: Runtime>(app: &AppHandle<R>, reachable: bool, active_tasks: usize) {
    let status = if reachable {
        format!("activo · {active_tasks} tareas activas")
    } else {
        String::from("no disponible")
    };
    let Ok(menu) = build_menu(app, &status) else {
        return;
    };
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_menu(Some(menu));
        let _ = tray.set_tooltip(Some(format!("LayerAgent · {status}")));
    }
}

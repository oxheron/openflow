#[doc(hidden)]
pub mod credentials;
#[doc(hidden)]
pub mod hotkey;
#[doc(hidden)]
pub mod local_server;
#[doc(hidden)]
pub mod platform;
#[doc(hidden)]
pub mod target;

use local_server::LocalServerState;
use target::{PatchRequest, StableInsertRequest, TargetLease, TargetState};
use tauri::{
    Emitter, Manager, WindowEvent,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
};

#[tauri::command]
fn get_platform_capabilities() -> platform::PlatformCapabilities {
    target::capabilities()
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
fn is_openflow_window_focused(app: tauri::AppHandle) -> bool {
    app.webview_windows()
        .values()
        // If the window backend cannot answer, fail closed: treating OpenFlow as
        // focused prevents dictation from being captured into its own UI.
        .any(|window| window.is_focused().unwrap_or(true))
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
async fn capture_target(state: tauri::State<'_, TargetState>) -> Result<TargetLease, String> {
    state.capture().await.map_err(|error| error.to_string())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
async fn insert_stable_text(
    state: tauri::State<'_, TargetState>,
    request: StableInsertRequest,
) -> Result<u64, String> {
    state
        .insert(&request)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
async fn apply_target_patch(
    state: tauri::State<'_, TargetState>,
    request: PatchRequest,
) -> Result<u64, String> {
    state
        .apply_patch(&request)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
async fn release_target(state: tauri::State<'_, TargetState>, lease_id: u64) -> Result<(), String> {
    state
        .release(lease_id)
        .await
        .map_err(|error| error.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// Starts the native desktop application.
///
/// # Panics
///
/// Panics when Tauri cannot initialize or run its desktop event loop.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_http::init())
        .manage(LocalServerState::default())
        .manage(TargetState::default())
        .manage(hotkey::WaylandHotkeyState::default())
        .invoke_handler(tauri::generate_handler![
            credentials::load_server_credential,
            credentials::store_server_credential,
            credentials::delete_server_credential,
            local_server::ensure_local_server,
            hotkey::register_wayland_hotkey,
            hotkey::unregister_wayland_hotkey,
            get_platform_capabilities,
            is_openflow_window_focused,
            capture_target,
            insert_stable_text,
            apply_target_patch,
            release_target,
        ])
        .on_window_event(|window, event| {
            if window.label() == "main"
                && let WindowEvent::CloseRequested { api, .. } = event
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(|app| {
            let toggle =
                MenuItem::with_id(app, "toggle", "Start / stop dictation", true, None::<&str>)?;
            let show = MenuItem::with_id(app, "show", "Open OpenFlow", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&toggle, &show, &quit])?;
            let mut tray = TrayIconBuilder::new()
                .tooltip("OpenFlow")
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "toggle" => {
                        let _ = app.emit("openflow://toggle-requested", ());
                    }
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.unminimize();
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to run OpenFlow desktop client");
}

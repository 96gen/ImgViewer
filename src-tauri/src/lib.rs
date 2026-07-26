#![deny(unsafe_code)]

mod catalog;
mod decode;
mod error;
mod model;
mod navigation;
mod policy;
mod protocol;
mod viewer;
mod window;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub use error::ViewerError;
pub use protocol::{
    NavigationDirection, PROTOCOL_VERSION, RenderDescriptor, ViewerSnapshot, ViewerStatus,
};
use tauri::{Emitter, Manager};
use tauri_plugin_window_state::StateFlags;
pub use viewer::ViewerController;

const SNAPSHOT_EVENT: &str = "viewer://snapshot";

#[tauri::command]
fn open_path(path: String, state: tauri::State<'_, ViewerController>) -> ViewerSnapshot {
    state.open_path(path)
}

#[tauri::command]
fn navigate(
    direction: NavigationDirection,
    state: tauri::State<'_, ViewerController>,
) -> ViewerSnapshot {
    state.navigate(direction)
}

#[tauri::command]
fn current_snapshot(state: tauri::State<'_, ViewerController>) -> ViewerSnapshot {
    state.current_snapshot()
}

#[tauri::command]
fn read_render(render_id: u64, state: tauri::State<'_, ViewerController>) -> tauri::ipc::Response {
    // An absent/expired token deliberately returns an empty raw ArrayBuffer.
    // The frontend treats that as a recoverable render error. This response
    // never returns a filesystem path and avoids JSON/base64 image payloads.
    tauri::ipc::Response::new(state.take_render(render_id).unwrap_or_default())
}

pub fn run() {
    let controller = ViewerController::new();
    let shutdown_controller = controller.clone();
    let builder = tauri::Builder::default()
        // The single-instance plugin must be registered first so another
        // process cannot race plugin initialization.
        .plugin(tauri_plugin_single_instance::init(
            |app, arguments, working_directory| {
                let controller = app.state::<ViewerController>();
                if let Some(path) = cli_path_from_strings(&arguments, Path::new(&working_directory))
                {
                    let snapshot = controller.open_path(path);
                    let _ = app.emit_to("main", SNAPSHOT_EVENT, snapshot);
                }
                focus_main_window(app);
            },
        ))
        // CSP governs subresources, while this hook is the cancellation
        // boundary for top-level WebView navigation. Keep it before any
        // plugin that may expose an IPC command.
        .plugin(navigation::navigation_policy_plugin())
        // tauri-plugin-window-state parses its persisted JSON during plugin
        // setup even when initial restore is skipped. Bound and validate that
        // untrusted file before the upstream setup hook runs.
        .plugin(window::state_preflight_plugin())
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(StateFlags::POSITION | StateFlags::SIZE | StateFlags::MAXIMIZED)
                // The built-in restore uses synchronous window getters during
                // startup and maximizing can reveal the window before its
                // bounds are repaired. We retain the plugin's event tracking
                // and persistence, then restore its pinned JSON state through
                // the Win32 bridge in `window::prepare_main_window`.
                .skip_initial_state("main")
                .build(),
        )
        .plugin(tauri_plugin_dialog::init());

    let result = builder
        .manage(controller)
        .invoke_handler(tauri::generate_handler![
            open_path,
            navigate,
            current_snapshot,
            read_render
        ])
        .setup(|app| {
            let app_handle = app.handle().clone();
            app.state::<ViewerController>()
                .set_event_sink(move |snapshot| {
                    let _ = app_handle.emit_to("main", SNAPSHOT_EVENT, snapshot);
                });

            window::prepare_main_window(app.handle());

            if let Some(path) = initial_cli_path(std::env::args_os()) {
                let snapshot = app.state::<ViewerController>().open_path(path);
                // This event may precede Vue mounting, so the frontend also
                // calls current_snapshot during startup.
                let _ = app.emit_to("main", SNAPSHOT_EVENT, snapshot);
            }
            Ok(())
        })
        .run(tauri::generate_context!());
    // The worker uses a soft in-process deadline, so explicitly join it after
    // Tauri's event loop ends instead of relying on process teardown.
    shutdown_controller.shutdown();
    result.expect("ImgViewer encountered a fatal Tauri runtime error");
}

fn focus_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn initial_cli_path(arguments: impl IntoIterator<Item = OsString>) -> Option<PathBuf> {
    arguments
        .into_iter()
        .skip(1)
        .map(PathBuf::from)
        .find(|argument| !is_option(argument))
}

fn cli_path_from_strings(arguments: &[String], working_directory: &Path) -> Option<PathBuf> {
    arguments
        .iter()
        .skip(1)
        .map(PathBuf::from)
        .find(|argument| !is_option(argument))
        .map(|argument| {
            if argument.is_absolute() {
                argument
            } else {
                working_directory.join(argument)
            }
        })
}

fn is_option(path: &Path) -> bool {
    path.as_os_str().to_string_lossy().starts_with('-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_instance_relative_path_uses_second_process_working_directory() {
        let args = vec!["ImgViewer.exe".to_owned(), "images\\1.jpg".to_owned()];
        assert_eq!(
            cli_path_from_strings(&args, Path::new("C:\\incoming")),
            Some(PathBuf::from("C:\\incoming\\images\\1.jpg"))
        );
    }

    #[test]
    fn command_line_options_are_not_treated_as_image_paths() {
        let args = vec![
            OsString::from("ImgViewer.exe"),
            OsString::from("--inspect"),
            OsString::from("photo.png"),
        ];
        assert_eq!(initial_cli_path(args), Some(PathBuf::from("photo.png")));
    }
}

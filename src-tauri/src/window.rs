#![allow(unsafe_code)]

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::mem::size_of;
use std::path::Path;
use std::ptr::null_mut;

use serde::Deserialize;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::{AppHandle, Manager, Runtime};
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromRect,
};
use windows_sys::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GWL_EXSTYLE, GWL_STYLE, GetAncestor, GetWindowLongPtrW, GetWindowRect, SWP_NOACTIVATE,
    SWP_NOOWNERZORDER, SWP_NOZORDER, SetWindowPos,
};

const MAX_WINDOW_STATE_BYTES: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WorkArea {
    x: i64,
    y: i64,
    width: u64,
    height: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[allow(dead_code)] // Mirrors every required upstream window-state 2.4.1 field.
struct SavedWindowState {
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    prev_x: i32,
    prev_y: i32,
    maximized: bool,
    visible: bool,
    decorated: bool,
    fullscreen: bool,
}

pub(crate) fn state_preflight_plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("window-state-preflight")
        .setup(|app, _api| {
            sanitize_window_state(app)?;
            Ok(())
        })
        .build()
}

fn sanitize_window_state<R: Runtime>(app: &AppHandle<R>) -> io::Result<()> {
    let state_path = app
        .path()
        .app_config_dir()
        .map_err(|error| io::Error::other(error.to_string()))?
        .join(tauri_plugin_window_state::DEFAULT_FILENAME);
    sanitize_window_state_path(&state_path)
}

fn sanitize_window_state_path(state_path: &Path) -> io::Result<()> {
    match read_bounded_state(state_path) {
        Ok(bytes)
            if serde_json::from_slice::<HashMap<String, SavedWindowState>>(&bytes).is_ok() =>
        {
            Ok(())
        }
        Ok(_) => {
            // The upstream plugin parses this file before honoring
            // skip_initial_state. Remove an invalid or oversized file before
            // its setup hook can allocate from untrusted state.
            std::fs::remove_file(state_path)
        }
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            std::fs::remove_file(state_path)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) fn prepare_main_window<R: Runtime>(app: &AppHandle<R>) {
    let saved = load_main_window_state(app);
    let Some(window) = app.get_webview_window("main") else {
        eprintln!("ImgViewer could not find its main window.");
        return;
    };
    let callback_window = window.clone();
    let result = window.with_webview(move |webview| {
        let native_result = (|| -> Result<(), String> {
            // SAFETY: Tauri invokes this closure while the WebView controller
            // and its parent window are alive. Every returned pointer is
            // checked before it is passed to the isolated Win32 adapter.
            unsafe {
                let controller = webview.controller();
                let mut parent = Default::default();
                controller
                    .ParentWindow(&mut parent)
                    .map_err(|error| format!("WebView2 parent HWND is unavailable: {error}"))?;
                let parent_hwnd = parent.0 as HWND;
                let root = GetAncestor(parent_hwnd, GA_ROOT);
                let hwnd = if root.is_null() { parent_hwnd } else { root };
                restore_and_clamp_native(hwnd, saved.as_ref())
            }
        })();

        if let Err(error) = native_result {
            eprintln!("ImgViewer could not restore native window geometry: {error}");
        }
        if saved.is_some_and(|state| state.maximized)
            && let Err(error) = callback_window.maximize()
        {
            eprintln!("ImgViewer could not restore maximized state: {error}");
        }
        if let Err(error) = callback_window.show() {
            eprintln!("ImgViewer could not show the main window: {error}");
        }
    });

    if let Err(error) = result {
        eprintln!("ImgViewer could not schedule native window restoration: {error}");
        if let Err(show_error) = window.show() {
            eprintln!("ImgViewer could not show the fallback window: {show_error}");
        }
    }
}

fn load_main_window_state<R: Runtime>(app: &AppHandle<R>) -> Option<SavedWindowState> {
    let state_path = app
        .path()
        .app_config_dir()
        .ok()?
        .join(tauri_plugin_window_state::DEFAULT_FILENAME);
    let bytes = match read_bounded_state(&state_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            eprintln!("ImgViewer could not read saved window state: {error}");
            return None;
        }
    };
    match serde_json::from_slice::<HashMap<String, SavedWindowState>>(&bytes) {
        Ok(states) => states.get("main").copied(),
        Err(error) => {
            eprintln!("ImgViewer ignored invalid saved window state: {error}");
            None
        }
    }
}

fn read_bounded_state(path: &Path) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let length = file.metadata()?.len();
    if length > MAX_WINDOW_STATE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "window state exceeds the 64 KiB safety limit",
        ));
    }

    let mut bytes = Vec::with_capacity(length as usize);
    file.take(MAX_WINDOW_STATE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_WINDOW_STATE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "window state grew beyond the 64 KiB safety limit",
        ));
    }
    Ok(bytes)
}

/// Restores a hidden Tauri window using its native Win32 handle.
///
/// # Safety
///
/// `hwnd` must identify the live top-level window that owns the WebView for
/// the entire duration of this synchronous call.
unsafe fn restore_and_clamp_native(
    hwnd: HWND,
    saved: Option<&SavedWindowState>,
) -> Result<(), String> {
    if hwnd.is_null() {
        return Err("WebView2 returned a null parent HWND.".to_owned());
    }

    let mut current = RECT::default();
    // SAFETY: `hwnd` was validated as non-null and `current` is writable for
    // the duration of this synchronous Win32 call.
    if unsafe { GetWindowRect(hwnd, &mut current) } == 0 {
        return Err("GetWindowRect failed.".to_owned());
    }

    let current_width = i64::from(current.right) - i64::from(current.left);
    let current_height = i64::from(current.bottom) - i64::from(current.top);
    let (x, y, mut outer_width, mut outer_height) = if let Some(state) =
        saved.filter(|state| state.width > 0 && state.height > 0)
    {
        let (x, y) = if state.maximized {
            (state.prev_x, state.prev_y)
        } else {
            (state.x, state.y)
        };
        let probe = RECT {
            left: x,
            top: y,
            right: x.saturating_add(state.width.min(i32::MAX as u32) as i32),
            bottom: y.saturating_add(state.height.min(i32::MAX as u32) as i32),
        };
        // SAFETY: `probe` is a fully initialized RECT and the function only
        // reads it during this call.
        let monitor = unsafe { MonitorFromRect(&probe, MONITOR_DEFAULTTOPRIMARY) };
        let dpi = monitor_dpi(monitor);
        // SAFETY: `hwnd` remains a live top-level window for this callback;
        // these getters do not retain the handle.
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
        // SAFETY: Same live HWND invariant as the style lookup above.
        let extended_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        let mut adjusted = RECT {
            left: 0,
            top: 0,
            right: state.width.min(i32::MAX as u32) as i32,
            bottom: state.height.min(i32::MAX as u32) as i32,
        };
        // SAFETY: `adjusted` is valid writable storage and all scalar style
        // values came from the same live HWND.
        if unsafe { AdjustWindowRectExForDpi(&mut adjusted, style, 0, extended_style, dpi) } != 0 {
            (
                i64::from(x),
                i64::from(y),
                i64::from(adjusted.right) - i64::from(adjusted.left),
                i64::from(adjusted.bottom) - i64::from(adjusted.top),
            )
        } else {
            (
                i64::from(x),
                i64::from(y),
                i64::from(state.width),
                i64::from(state.height),
            )
        }
    } else {
        (
            i64::from(current.left),
            i64::from(current.top),
            current_width,
            current_height,
        )
    };

    outer_width = outer_width.max(1);
    outer_height = outer_height.max(1);
    let probe = RECT {
        left: clamp_i64_to_i32(x),
        top: clamp_i64_to_i32(y),
        right: clamp_i64_to_i32(x.saturating_add(outer_width)),
        bottom: clamp_i64_to_i32(y.saturating_add(outer_height)),
    };
    // SAFETY: `probe` is initialized and borrowed only for this synchronous
    // monitor lookup.
    let monitor = unsafe { MonitorFromRect(&probe, MONITOR_DEFAULTTOPRIMARY) };
    let work_area = monitor_work_area(monitor)?;
    let ((x, y), (outer_width, outer_height)) = clamp_geometry(
        (x, y),
        (outer_width as u64, outer_height as u64),
        &[work_area],
    )
    .ok_or_else(|| "Windows did not report a usable monitor work area.".to_owned())?;
    set_native_bounds(hwnd, (x, y), (outer_width, outer_height))?;

    // A move between monitors can synchronously adjust the DPI and non-client
    // frame. Clamp the actual post-move rectangle once more while still hidden.
    let mut actual = RECT::default();
    // SAFETY: `hwnd` is still live and `actual` is writable for this call.
    if unsafe { GetWindowRect(hwnd, &mut actual) } != 0 {
        let actual_position = (i64::from(actual.left), i64::from(actual.top));
        let actual_size = (
            (i64::from(actual.right) - i64::from(actual.left)).max(1) as u64,
            (i64::from(actual.bottom) - i64::from(actual.top)).max(1) as u64,
        );
        // SAFETY: `actual` was initialized by GetWindowRect and is borrowed
        // only for this synchronous monitor lookup.
        let monitor = unsafe { MonitorFromRect(&actual, MONITOR_DEFAULTTOPRIMARY) };
        let actual_work_area = monitor_work_area(monitor)?;
        if let Some((position, size)) =
            clamp_geometry(actual_position, actual_size, &[actual_work_area])
            && (position != actual_position || size != actual_size)
        {
            set_native_bounds(hwnd, position, size)?;
        }
    }
    Ok(())
}

fn monitor_dpi(monitor: windows_sys::Win32::Graphics::Gdi::HMONITOR) -> u32 {
    let mut x = 96;
    let mut y = 96;
    // SAFETY: `x` and `y` are valid writable u32 values and the monitor handle
    // came from MonitorFromRect.
    let result = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) };
    if result >= 0 && x > 0 { x } else { 96 }
}

fn monitor_work_area(
    monitor: windows_sys::Win32::Graphics::Gdi::HMONITOR,
) -> Result<WorkArea, String> {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        rcMonitor: RECT::default(),
        rcWork: RECT::default(),
        dwFlags: 0,
    };
    // SAFETY: `info.cbSize` identifies the initialized MONITORINFO allocation
    // and the monitor handle came from MonitorFromRect.
    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return Err("GetMonitorInfoW failed.".to_owned());
    }
    let width = i64::from(info.rcWork.right) - i64::from(info.rcWork.left);
    let height = i64::from(info.rcWork.bottom) - i64::from(info.rcWork.top);
    if width <= 0 || height <= 0 {
        return Err("Windows reported an empty monitor work area.".to_owned());
    }
    Ok(WorkArea {
        x: i64::from(info.rcWork.left),
        y: i64::from(info.rcWork.top),
        width: width as u64,
        height: height as u64,
    })
}

fn set_native_bounds(hwnd: HWND, position: (i64, i64), size: (u64, u64)) -> Result<(), String> {
    // SAFETY: `hwnd` is the checked live top-level window, no insertion HWND is
    // used with SWP_NOZORDER, and all dimensions are clamped to Win32 ranges.
    let success = unsafe {
        SetWindowPos(
            hwnd,
            null_mut(),
            clamp_i64_to_i32(position.0),
            clamp_i64_to_i32(position.1),
            size.0.min(i32::MAX as u64) as i32,
            size.1.min(i32::MAX as u64) as i32,
            SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_NOZORDER,
        )
    };
    if success == 0 {
        Err("SetWindowPos failed.".to_owned())
    } else {
        Ok(())
    }
}

fn clamp_i64_to_i32(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn clamp_geometry(
    position: (i64, i64),
    size: (u64, u64),
    work_areas: &[WorkArea],
) -> Option<((i64, i64), (u64, u64))> {
    if work_areas.is_empty() {
        return None;
    }
    let (x, y) = position;
    let (width, height) = size;
    let best = work_areas
        .iter()
        .max_by_key(|area| overlap_area(x, y, width, height, **area))
        .copied()
        .expect("non-empty work areas");
    let target = if overlap_area(x, y, width, height, best) == 0 {
        work_areas[0]
    } else {
        best
    };
    let width = width.min(target.width).max(1);
    let height = height.min(target.height).max(1);
    let max_x = target.x + target.width.saturating_sub(width) as i64;
    let max_y = target.y + target.height.saturating_sub(height) as i64;
    let x = x.clamp(target.x, max_x);
    let y = y.clamp(target.y, max_y);
    Some(((x, y), (width, height)))
}

fn overlap_area(x: i64, y: i64, width: u64, height: u64, area: WorkArea) -> u64 {
    let left = x.max(area.x);
    let top = y.max(area.y);
    let right = x
        .saturating_add(width as i64)
        .min(area.x.saturating_add(area.width as i64));
    let bottom = y
        .saturating_add(height as i64)
        .min(area.y.saturating_add(area.height as i64));
    if right <= left || bottom <= top {
        0
    } else {
        (right - left) as u64 * (bottom - top) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn removed_monitor_moves_window_into_primary_work_area() {
        let areas = [WorkArea {
            x: 0,
            y: 0,
            width: 1920,
            height: 1040,
        }];
        assert_eq!(
            clamp_geometry((2500, 100), (1100, 750), &areas),
            Some(((820, 100), (1100, 750)))
        );
    }

    #[test]
    fn completely_offscreen_window_prefers_primary_in_multi_monitor_layout() {
        let areas = [
            WorkArea {
                x: 0,
                y: 0,
                width: 1920,
                height: 1040,
            },
            WorkArea {
                x: 1920,
                y: 0,
                width: 1920,
                height: 1040,
            },
        ];
        assert_eq!(
            clamp_geometry((8000, 4000), (1100, 750), &areas),
            Some(((820, 290), (1100, 750)))
        );
    }

    #[test]
    fn oversized_saved_window_is_shrunk_to_work_area() {
        let areas = [WorkArea {
            x: -1280,
            y: 0,
            width: 1280,
            height: 720,
        }];
        assert_eq!(
            clamp_geometry((-1300, -50), (2000, 1200), &areas),
            Some(((-1280, 0), (1280, 720)))
        );
    }

    #[test]
    fn existing_visible_geometry_is_unchanged() {
        let areas = [WorkArea {
            x: 0,
            y: 0,
            width: 1920,
            height: 1040,
        }];
        assert_eq!(
            clamp_geometry((100, 100), (1100, 750), &areas),
            Some(((100, 100), (1100, 750)))
        );
    }

    #[test]
    fn i64_values_are_safely_clamped_for_win32_calls() {
        assert_eq!(clamp_i64_to_i32(i64::MAX), i32::MAX);
        assert_eq!(clamp_i64_to_i32(i64::MIN), i32::MIN);
        assert_eq!(clamp_i64_to_i32(42), 42);
    }

    #[test]
    fn untrusted_window_state_has_a_hard_read_limit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("window-state.json");
        let mut file = File::create(&path).unwrap();
        file.write_all(&vec![b'x'; MAX_WINDOW_STATE_BYTES as usize + 1])
            .unwrap();
        drop(file);

        let error = read_bounded_state(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn invalid_or_oversized_state_is_removed_before_plugin_setup() {
        let directory = tempfile::tempdir().unwrap();
        let incomplete = directory.path().join("incomplete.json");
        std::fs::write(&incomplete, br#"{"main":{"width":1100,"height":750}}"#).unwrap();
        sanitize_window_state_path(&incomplete).unwrap();
        assert!(!incomplete.exists());

        let invalid = directory.path().join("invalid.json");
        std::fs::write(&invalid, b"not-json").unwrap();
        sanitize_window_state_path(&invalid).unwrap();
        assert!(!invalid.exists());

        let oversized = directory.path().join("oversized.json");
        std::fs::write(&oversized, vec![b'x'; MAX_WINDOW_STATE_BYTES as usize + 1]).unwrap();
        sanitize_window_state_path(&oversized).unwrap();
        assert!(!oversized.exists());
    }

    #[test]
    fn bounded_window_state_is_read_normally() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("window-state.json");
        std::fs::write(&path, br#"{"main":{"width":1100,"height":750}}"#).unwrap();

        let bytes = read_bounded_state(&path).unwrap();
        assert!(bytes.len() < MAX_WINDOW_STATE_BYTES as usize);
    }
}

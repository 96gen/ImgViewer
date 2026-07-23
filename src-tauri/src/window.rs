use std::collections::HashMap;
use std::mem::size_of;
use std::ptr::null_mut;

use serde::Deserialize;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WorkArea {
    x: i64,
    y: i64,
    width: u64,
    height: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default)]
struct SavedWindowState {
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    prev_x: i32,
    prev_y: i32,
    maximized: bool,
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
    let bytes = match std::fs::read(&state_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            eprintln!(
                "ImgViewer could not read saved window state at {}: {error}",
                state_path.display()
            );
            return None;
        }
    };
    match serde_json::from_slice::<HashMap<String, SavedWindowState>>(&bytes) {
        Ok(states) => states.get("main").copied(),
        Err(error) => {
            eprintln!(
                "ImgViewer ignored invalid saved window state at {}: {error}",
                state_path.display()
            );
            None
        }
    }
}

unsafe fn restore_and_clamp_native(
    hwnd: HWND,
    saved: Option<&SavedWindowState>,
) -> Result<(), String> {
    if hwnd.is_null() {
        return Err("WebView2 returned a null parent HWND.".to_owned());
    }

    let mut current = RECT::default();
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
        let monitor = unsafe { MonitorFromRect(&probe, MONITOR_DEFAULTTOPRIMARY) };
        let dpi = monitor_dpi(monitor);
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
        let extended_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        let mut adjusted = RECT {
            left: 0,
            top: 0,
            right: state.width.min(i32::MAX as u32) as i32,
            bottom: state.height.min(i32::MAX as u32) as i32,
        };
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
    let work_area =
        monitor_work_area(unsafe { MonitorFromRect(&probe, MONITOR_DEFAULTTOPRIMARY) })?;
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
    if unsafe { GetWindowRect(hwnd, &mut actual) } != 0 {
        let actual_position = (i64::from(actual.left), i64::from(actual.top));
        let actual_size = (
            (i64::from(actual.right) - i64::from(actual.left)).max(1) as u64,
            (i64::from(actual.bottom) - i64::from(actual.top)).max(1) as u64,
        );
        let actual_work_area =
            monitor_work_area(unsafe { MonitorFromRect(&actual, MONITOR_DEFAULTTOPRIMARY) })?;
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
}

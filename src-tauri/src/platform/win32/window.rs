//! Window behaviour Tauri does not expose the way the overlay needs it.

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::*;

fn hwnd_of(window: &tauri::WebviewWindow) -> Option<HWND> {
    window.hwnd().ok().map(|h| HWND(h.0))
}

/// Where the mouse pointer is, in physical screen pixels. The click-through
/// lock uses it to keep its own button clickable.
pub fn cursor_position() -> Option<(i32, i32)> {
    let mut point = POINT::default();
    unsafe { GetCursorPos(&mut point) }.ok()?;
    Some((point.x, point.y))
}

/// Begin dragging the window, as if its title bar had been grabbed.
pub fn start_drag(window: &tauri::WebviewWindow) {
    use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
    let Some(hwnd) = hwnd_of(window) else { return };
    unsafe {
        let _ = ReleaseCapture();
        const HTCAPTION: usize = 2;
        let _ = PostMessageW(Some(hwnd), WM_NCLBUTTONDOWN, WPARAM(HTCAPTION), LPARAM(0));
    }
}

/// Bring the overlay back on top without taking focus from the game.
pub fn show_on_top_without_focus(window: &tauri::WebviewWindow) {
    let Some(hwnd) = hwnd_of(window) else { return };
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(
            hwnd, Some(HWND_TOPMOST),
            0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

/// Drop the overlay from the top and minimise it, without activating anything.
pub fn minimize_off_top(window: &tauri::WebviewWindow) {
    let Some(hwnd) = hwnd_of(window) else { return };
    unsafe {
        let _ = SetWindowPos(
            hwnd, Some(HWND_NOTOPMOST),
            0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
        let _ = ShowWindow(hwnd, SW_MINIMIZE);
    }
}

pub fn set_size(window: &tauri::WebviewWindow, size: tauri::Size) {
    let _ = window.set_size(size);
}

pub fn release_size(_window: &tauri::WebviewWindow, _min: tauri::LogicalSize<f64>) {}

pub fn primary_button_down() -> Option<bool> {
    None
}

pub fn compositor_resize_supported(_window: &tauri::WebviewWindow) -> bool {
    false
}

pub fn resize_pointer_down(_window: &tauri::WebviewWindow) -> Option<bool> {
    None
}

pub async fn prepare_resize(_window: &tauri::WebviewWindow, _min: tauri::LogicalSize<f64>) -> Result<(), String> {
    Err("Compositor resize is unavailable".into())
}

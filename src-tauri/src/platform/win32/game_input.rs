//! Esc and left clicks in AION 2 (`platform::game_input`) on Windows.
//!
//! While a plugin asks and the game's window is in front, the keyboard and
//! mouse state is read about 60 times a second (GetAsyncKeyState, as the
//! click-through lock reads the mouse buttons): no hook, nothing grabbed, the
//! game gets every key and click as before. Only Esc and the primary button
//! are read. With another window in front it only looks, four times a
//! second, at which one it is. (Raw input would need no polling, but a
//! process has one raw input target per device and the windowing library
//! keeps its own.)

use std::time::Duration;

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE, VK_LBUTTON, VK_RBUTTON};
use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, GetCursorPos, GetForegroundWindow, GetSystemMetrics, SM_SWAPBUTTON};

use crate::platform::game_input::{Area, GameInput};

/// A window's client area on screen (physical pixels: the meter is DPI aware).
fn area_of(hwnd: HWND) -> Option<Area> {
    let mut rect = RECT::default();
    let mut origin = POINT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rect);
        let _ = ClientToScreen(hwnd, &mut origin);
    }
    let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
    (width > 0 && height > 0).then_some(Area { x: origin.x, y: origin.y, width, height })
}

pub fn game_area() -> Option<Area> {
    area_of(super::window_detector::game_hwnd()?)
}

/// Whether a key or mouse button is down now.
fn down(vk: u16) -> bool {
    (unsafe { GetAsyncKeyState(i32::from(vk)) }) as u16 & 0x8000 != 0
}

/// Hand `on_input` Esc and clicks in the game while `on()`.
pub fn watch_while(on_input: &dyn Fn(GameInput), on: &dyn Fn() -> bool) -> Result<(), String> {
    let mut front = std::ptr::null_mut();
    let mut game = false;
    let (mut escape, mut click) = (false, false);
    while on() {
        // Which window is in front, asked again only when it changes.
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0 != front {
            front = hwnd.0;
            game = super::window_detector::is_aion2_foreground();
        }
        // The primary button: the right one on a mouse set for the left hand.
        let primary = if unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0 { VK_RBUTTON } else { VK_LBUTTON };
        let (escape_now, click_now) = if game { (down(VK_ESCAPE.0), down(primary.0)) } else { (false, false) };
        if escape_now && !escape {
            on_input(GameInput::Escape);
        }
        if click_now && !click {
            let mut at = POINT::default();
            if let (true, Some(area)) = (unsafe { GetCursorPos(&mut at) }.is_ok(), area_of(hwnd)) {
                on_input(GameInput::Click { x: at.x - area.x, y: at.y - area.y, area });
            }
        }
        (escape, click) = (escape_now, click_now);
        std::thread::sleep(Duration::from_millis(if game { 16 } else { 250 }));
    }
    Ok(())
}

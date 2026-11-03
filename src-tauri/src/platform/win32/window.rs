//! Window behaviour Tauri does not expose the way the overlay needs it.

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::*;

fn hwnd_of(window: &tauri::WebviewWindow) -> Option<HWND> {
    window.hwnd().ok().map(|h| HWND(h.0))
}

/// Whether the window is on screen, as Windows has it. tao's own idea can be
/// stale: `hide_without_focus` hides a window behind its back, and tao shows
/// it again on its next style change (`set_ignore_cursor_events`), so a
/// hidden window's cursor setting is left alone (see `widget_click_through`).
fn shown(window: &tauri::WebviewWindow) -> bool {
    hwnd_of(window).is_some_and(|hwnd| unsafe { IsWindowVisible(hwnd) }.as_bool())
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

/// A tool window already open but behind others (or minimised), brought in
/// front of them all when its button is clicked again. Windows refuses the
/// front to a program that isn't in front (the overlay never is: it takes no
/// focus), so it is raised, not activated: on top, the game keeping the
/// keyboard. `set_focus` alone left it where it was.
pub fn bring_forward(window: &tauri::WebviewWindow) {
    let Some(hwnd) = hwnd_of(window) else { return };
    unsafe {
        let shown = if IsIconic(hwnd).as_bool() { SW_SHOWNOACTIVATE } else { SW_SHOWNA };
        let _ = ShowWindow(hwnd, shown);
        let _ = SetWindowPos(
            hwnd, Some(HWND_TOPMOST),
            0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

/// Bring the overlay back on top without taking focus from the game.
pub fn show_on_top_without_focus(window: &tauri::WebviewWindow) {
    let Some(hwnd) = hwnd_of(window) else { return };
    unsafe {
        // A focus-hidden tool may be maximized or snapped. Keep that size;
        // only a minimized window needs its normal placement restored.
        let shown = if IsIconic(hwnd).as_bool() { SW_SHOWNOACTIVATE } else { SW_SHOWNA };
        let _ = ShowWindow(hwnd, shown);
        let _ = SetWindowPos(
            hwnd, Some(HWND_TOPMOST),
            0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

/// Hide a window that `show_on_top_without_focus` shows, the same way: Tauri's
/// own show and hide go through tao, which keeps a visible flag of its own.
/// A raw show leaves that flag "hidden", so a tao hide after it changed
/// nothing (the window stayed up), and a tao show takes focus. Shown and
/// hidden raw both ways, tao's flag stays "visible" from the window's
/// creation (`HIDE_AFTER_BUILD`).
pub fn hide_without_focus(window: &tauri::WebviewWindow) {
    let Some(hwnd) = hwnd_of(window) else { return };
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

/// The window in front now (the game, while playing), for `never_activate`
/// to give the front back to.
pub fn front_window() -> Option<isize> {
    let hwnd = unsafe { GetForegroundWindow() };
    (!hwnd.0.is_null()).then_some(hwnd.0 as isize)
}

/// Whether the window in front is the meter's own (any of its windows) or
/// the game's: what a tool window stays on top for (`keep_on_top_while_ours`).
pub fn front_is_ours_or_game() -> bool {
    use windows::Win32::System::Threading::GetCurrentProcessId;
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return true;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid == unsafe { GetCurrentProcessId() } || crate::platform::window_detector::is_aion2_foreground()
}

/// If `window` has the front, give it back to `before` (the window in front
/// when it was made): a widget's page has just loaded, which is when WebView2
/// can take it.
pub fn give_front_back(window: &tauri::WebviewWindow, before: Option<isize>) {
    let (Some(hwnd), Some(before)) = (hwnd_of(window), before) else { return };
    unsafe {
        if GetForegroundWindow() == hwnd && before != hwnd.0 as isize {
            let back = SetForegroundWindow(HWND(before as *mut _)).as_bool();
            tracing::info!("A widget's page took the front as it loaded; given back: {back}");
        }
    }
}

/// A small window that must never take the front from the game (a plugin's
/// alert icon): it still gets clicks and drags, but is never activated
/// (`WS_EX_NOACTIVATE`). Built with focus off, it still took the front as
/// its page was created (WebView2), and an alert icon appearing tabbed a
/// player out of the game (2026-10-18). For ten seconds after it is built,
/// if it has the front anyway, the front goes back to `before`: three were
/// too few on a busy PC, where WebView2 takes longer to start (Hirai, still
/// tabbed out on 2.0.71, borderless). Its page loading checks once more
/// (`give_front_back`).
pub fn never_activate(window: &tauri::WebviewWindow, before: Option<isize>) {
    let Some(hwnd) = hwnd_of(window) else { return };
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE.0 as isize);
    }
    let Some(before) = before else { return };
    let own = hwnd.0 as isize;
    if before == own {
        return;
    }
    std::thread::spawn(move || {
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            unsafe {
                if GetForegroundWindow().0 as isize == own {
                    let back = SetForegroundWindow(HWND(before as *mut _)).as_bool();
                    tracing::info!("A widget took the front; given back to the window before it: {back}");
                }
            }
        }
    });
}

/// A window to be shown and hidden with the functions above is built visible
/// (without focus), so tao applies its frameless style and marks it visible,
/// then hidden with `hide_without_focus` if it should start hidden. Built
/// hidden, tao never styled it, and a raw show brought it up with a title bar.
pub const HIDE_AFTER_BUILD: bool = true;

/// Drop the overlay from the top and minimise it, without activating anything.
pub fn minimize_off_top(window: &tauri::WebviewWindow) {
    let Some(hwnd) = hwnd_of(window) else { return };
    unsafe {
        let _ = SetWindowPos(
            hwnd, Some(HWND_NOTOPMOST),
            0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
        // SW_MINIMIZE activates the next window in the Z order: the game, even
        // with an overlay (NVIDIA's) in front, and the game then locks the
        // mouse to the middle of the screen.
        let _ = ShowWindow(hwnd, SW_SHOWMINNOACTIVE);
    }
}

pub fn set_size(window: &tauri::WebviewWindow, size: tauri::Size) {
    let _ = window.set_size(size);
}

/// Size and move a window in one `SetWindowPos` (`meter_anchor::resize`), so
/// no frame is drawn grown the wrong way.
pub fn set_size_and_position(window: &tauri::WebviewWindow, size: tauri::Size, pos: tauri::PhysicalPosition<i32>) {
    let Some(hwnd) = hwnd_of(window) else {
        let _ = window.set_size(size);
        let _ = window.set_position(tauri::Position::Physical(pos));
        return;
    };
    let Some(physical) = crate::platform::meter_anchor::physical_size(window, &size) else {
        let _ = window.set_size(size);
        let _ = window.set_position(tauri::Position::Physical(pos));
        return;
    };
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            pos.x,
            pos.y,
            physical.width as i32,
            physical.height as i32,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// The edge the overlay keeps: the meter moves it itself here
/// (`meter_anchor::resize`), KWin only on KDE Wayland.
pub fn set_main_anchor(_anchor: crate::platform::meter_anchor::Anchor) {}

/// Whether the overlay's next resize keeps its edge: for KWin on KDE Wayland
/// only; here `resize_window` moves it itself.
pub async fn anchor_next_resize(_anchored: bool) {}

/// Whether Settings offers "Meter stays fixed at": always here, where the
/// meter places its own windows.
pub fn meter_anchor_available() -> bool {
    true
}

/// Linux only: there tao's edge band and the pinned size need it.
pub fn resizable_edges(_window: &tauri::WebviewWindow, _min: tauri::LogicalSize<f64>) {}

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

/// The click-through lock as an input region, on Linux only (see
/// `linux/window.rs`). Here the pointer watch in `app.rs` does it instead.
pub fn click_through_except(_window: &tauri::WebviewWindow, _ignore: bool, _keep: Option<(f64, f64, f64, f64, f64)>) -> bool {
    false
}

/// A widget's click-through: locked, clicks pass everywhere; unlocked, only
/// its drawn parts (`keep`: CSS-px rects and the page's scale) take them, so
/// an Event Timers grid's empty cells let the game have its clicks.
///
/// By a pointer watch, as the overlay's lock: while any unlocked widget has
/// parts, a thread looks at the pointer every 40 ms and lets the clicks
/// through except over a part. A window region did it first, but Windows
/// then drew the borderless widget's title bar and buttons back over it.
pub fn widget_click_through(window: &tauri::WebviewWindow, ignore: bool, keep: Option<(&[(f64, f64, f64, f64)], f64)>) -> bool {
    if hwnd_of(window).is_none() {
        return false;
    }
    let label = window.label().to_string();
    match keep.filter(|_| ignore) {
        Some((rects, scale)) => {
            let parts = rects
                .iter()
                .map(|&(x, y, w, h)| ((x * scale).floor() as i32, (y * scale).floor() as i32, ((x + w) * scale).ceil() as i32, ((y + h) * scale).ceil() as i32))
                .filter(|&(l, t, r, b)| r > l && b > t)
                .collect();
            settled().lock().unwrap_or_else(|p| p.into_inner()).remove(&label);
            watched().lock().unwrap_or_else(|p| p.into_inner()).insert(label, (window.clone(), parts, None));
            watch_widgets();
        }
        None => {
            {
                let mut watched = watched().lock().unwrap_or_else(|p| p.into_inner());
                watched.remove(&label);
                settled().lock().unwrap_or_else(|p| p.into_inner()).insert(label, ignore);
            }
            // Hidden: set when it is shown again (`relock`).
            if shown(window) {
                let _ = window.set_ignore_cursor_events(ignore);
            }
        }
    }
    true
}

/// The unlocked widgets with parts: each window, its parts (client pixels:
/// left, top, right, bottom) and whether it lets clicks through now.
type Watched = std::collections::HashMap<String, (tauri::WebviewWindow, Vec<(i32, i32, i32, i32)>, Option<bool>)>;
fn watched() -> &'static std::sync::Mutex<Watched> {
    static WATCHED: std::sync::OnceLock<std::sync::Mutex<Watched>> = std::sync::OnceLock::new();
    WATCHED.get_or_init(Default::default)
}

/// The cursor setting of widgets that left the watch (locked, or no parts):
/// put back if the watch changed one in the same instant.
fn settled() -> &'static std::sync::Mutex<std::collections::HashMap<String, bool>> {
    static SETTLED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, bool>>> = std::sync::OnceLock::new();
    SETTLED.get_or_init(Default::default)
}

/// Whether a mouse button (the primary, or the secondary for a swapped
/// mouse) is held down now.
fn mouse_button_held() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
    unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) | GetAsyncKeyState(VK_RBUTTON.0 as i32)) as u16 & 0x8000 != 0 }
}

/// Start the pointer watch if it isn't running; it ends once no widget needs it.
fn watch_widgets() {
    static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| loop {
        // A button held: a widget being dragged (the pointer is on its part
        // anyway) or a click under way. Nothing changes then, and asking the
        // main thread for positions mid-drag cost the game frames.
        if mouse_button_held() {
            std::thread::sleep(std::time::Duration::from_millis(40));
            continue;
        }
        let pointer = cursor_position();
        // What to look at, copied out: a window's position and its cursor
        // setting go through the main thread, and the main thread takes this
        // lock to change a widget (closing the dock hid and relocked them),
        // so asking it while holding the lock froze the meter.
        let snapshot: Vec<(String, tauri::WebviewWindow, Vec<(i32, i32, i32, i32)>, Option<bool>)> = {
            let watched = watched().lock().unwrap_or_else(|p| p.into_inner());
            if watched.is_empty() {
                RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
                return;
            }
            watched.iter().map(|(label, (window, parts, through))| (label.clone(), window.clone(), parts.clone(), *through)).collect()
        };
        for (label, window, parts, through) in snapshot {
            // A widget its plugin closed (hidden): the pointer passing over
            // where it was brought it back (Death Recap's popup, 2026-10-28).
            if !shown(&window) {
                continue;
            }
            let Ok(origin) = window.inner_position() else {
                watched().lock().unwrap_or_else(|p| p.into_inner()).remove(&label);
                continue;
            };
            let over = pointer.is_some_and(|(px, py)| {
                let (x, y) = (px - origin.x, py - origin.y);
                parts.iter().any(|&(l, t, r, b)| x >= l && x < r && y >= t && y < b)
            });
            if through == Some(!over) || !watched().lock().unwrap_or_else(|p| p.into_inner()).contains_key(&label) {
                continue;
            }
            let _ = window.set_ignore_cursor_events(!over);
            // Noted if the widget is still watched with the same parts; if it
            // left the watch meanwhile, its own setting goes back.
            let left = {
                let mut watched = watched().lock().unwrap_or_else(|p| p.into_inner());
                match watched.get_mut(&label) {
                    Some(entry) => {
                        if entry.1 == parts {
                            entry.2 = Some(!over);
                        }
                        None
                    }
                    None => settled().lock().unwrap_or_else(|p| p.into_inner()).get(&label).copied(),
                }
            };
            if let Some(ignore) = left {
                let _ = window.set_ignore_cursor_events(ignore);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    });
}

/// Hide a plugin window so it comes back where it was. Here a hidden window
/// keeps its place, so it is simply hidden (Linux keeps it mapped instead).
pub fn hide_in_place(window: &tauri::WebviewWindow) {
    hide_without_focus(window);
}

/// The meter places its own windows here.
pub fn places_own_windows() -> bool {
    true
}

/// Windows are placed by the meter itself here (Linux on Wayland asks KWin).
pub fn keep_places(
    _save: impl Fn(&str, (i32, i32, i32, i32)) + Send + Sync + 'static,
    _load: impl Fn(&str) -> Option<(i32, i32, i32, i32)> + Send + Sync + 'static,
) {
}

/// Nothing is hidden in place here.
pub fn is_hidden_in_place(_window: &tauri::WebviewWindow) -> bool {
    false
}

/// Nothing is hidden in place here: windows are shown as usual.
pub fn reveal_in_place(_window: &tauri::WebviewWindow) -> bool {
    false
}

/// Whether a tray icon can be shown: always, in the notification area.
pub fn tray_supported() -> bool {
    true
}

/// Whether the click-through lock works by an input region, needing no
/// pointer: not here, where a pointer watch keeps the lock button.
pub fn click_through_by_region() -> bool {
    false
}

/// Linux only (see the Linux version). WebView2 has no related views: its
/// browser and GPU processes are shared, and Chromium decides the renderers.
pub fn share_process<'a, R: tauri::Runtime, M: tauri::Manager<R>>(
    builder: tauri::WebviewWindowBuilder<'a, R, M>,
    _group: crate::platform::ProcessGroup,
) -> tauri::WebviewWindowBuilder<'a, R, M> {
    builder
}

/// Linux only (see the Linux version).
pub fn joined(_window: &tauri::WebviewWindow, _group: crate::platform::ProcessGroup) {}

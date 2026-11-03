//! Tauri's own cross-platform equivalents, which is the best available here.

/// Not available: on Wayland an app cannot read the pointer outside its own
/// windows, so the click-through lock (which needs it) is not offered.
pub fn cursor_position() -> Option<(i32, i32)> {
    None
}

/// No foreground tracking here; focus requests remain the OS's decision.
pub fn user_focus_unchanged(_before: Option<isize>) -> bool {
    true
}

/// A recent explicit configuration Open asks the compositor for focus.
/// There is no portable foreground handle here; its focus policy decides.
pub fn activate_from_user(window: &tauri::WebviewWindow, _before: Option<isize>) {
    let _ = window.set_focus();
}

pub fn start_drag(window: &tauri::WebviewWindow) {
    let _ = window.start_dragging();
}

pub fn show_on_top_without_focus(window: &tauri::WebviewWindow) {
    let _ = window.show();
    let _ = window.set_always_on_top(true);
}

/// A tool window behind others, brought in front (see the Windows one).
pub fn bring_forward(window: &tauri::WebviewWindow) {
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_always_on_top(true);
    let _ = window.set_focus();
}

/// Hide a window `show_on_top_without_focus` shows (see the Windows one).
pub fn hide_without_focus(window: &tauri::WebviewWindow) {
    let _ = window.hide();
}

/// Here a window can be built hidden and shown later as it is.
pub const HIDE_AFTER_BUILD: bool = false;

pub fn minimize_off_top(window: &tauri::WebviewWindow) {
    let _ = window.set_always_on_top(false);
    let _ = window.minimize();
}

pub fn set_size(window: &tauri::WebviewWindow, size: tauri::Size) {
    let _ = window.set_size(size);
}

/// Size and move a window (`meter_anchor::resize`).
pub fn set_size_and_position(window: &tauri::WebviewWindow, size: tauri::Size, pos: tauri::PhysicalPosition<i32>) {
    let _ = window.set_size(size);
    let _ = window.set_position(tauri::Position::Physical(pos));
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

/// Hide a plugin window so it comes back where it was. Here a hidden window
/// keeps its place, so it is simply hidden (Linux keeps it mapped instead).
pub fn hide_in_place(window: &tauri::WebviewWindow) {
    hide_without_focus(window);
}

/// The meter places its own windows here.
pub fn places_own_windows() -> bool {
    true
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

/// Windows only (see the Windows version): here the window manager decides
/// what takes focus.
pub fn front_window() -> Option<isize> {
    None
}

/// Nothing to tell here: tool windows stay on top.
pub fn front_is_ours_or_game() -> bool {
    true
}

/// Windows only (see the Windows version).
pub fn never_activate(_window: &tauri::WebviewWindow, _before: Option<isize>) {}

/// Windows only (see the Windows version).
pub fn give_front_back(_window: &tauri::WebviewWindow, _before: Option<isize>) {}

/// Whether a tray icon can be shown: not here.
pub fn tray_supported() -> bool {
    false
}

/// Whether the click-through lock works by an input region: not here.
pub fn click_through_by_region() -> bool {
    false
}

/// A plugin widget's click-through by parts: not here (the window's own
/// cursor setting does it whole).
pub fn widget_click_through(_window: &tauri::WebviewWindow, _ignore: bool, _keep: Option<(&[(f64, f64, f64, f64)], f64)>) -> bool {
    false
}

/// Linux only (see the Linux version): here pages already share processes.
pub fn share_process<'a, R: tauri::Runtime, M: tauri::Manager<R>>(
    builder: tauri::WebviewWindowBuilder<'a, R, M>,
    _group: crate::platform::ProcessGroup,
) -> tauri::WebviewWindowBuilder<'a, R, M> {
    builder
}

/// Linux only (see the Linux version).
pub fn joined(_window: &tauri::WebviewWindow, _group: crate::platform::ProcessGroup) {}

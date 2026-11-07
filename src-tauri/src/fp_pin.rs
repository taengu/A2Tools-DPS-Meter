//! The flight power pin: the footer's FP timer as a small always-on-top pin
//! the player drags anywhere over the game, as an Event Timers icon (the
//! footer menu's "Flight power as a pin"). Its page (`fp-pin.html`) reads the
//! same `flight-power` updates the footer does. It never takes focus, keeps
//! where it was left, and lets clicks through while the meter is locked.

use tauri::Manager;

use crate::app::AppState;

pub const LABEL: &str = "fp-pin";
const PLACE_KEY: &str = "dpsMeter.fpPinPlace";
/// The meter's own lock (its header's padlock): the pin follows it.
pub const LOCKED_KEY: &str = "dpsMeter.overlayLocked";
const WIDTH: f64 = 100.0;
const HEIGHT: f64 = 34.0;

fn setting(app: &tauri::AppHandle, key: &str) -> Option<String> {
    app.try_state::<AppState>()?.settings.get(key)
}

fn locked(app: &tauri::AppHandle) -> bool {
    setting(app, LOCKED_KEY).as_deref() == Some("true")
}

fn saved_place(app: &tauri::AppHandle) -> Option<(i32, i32)> {
    let value = setting(app, PLACE_KEY)?;
    let (x, y) = value.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// Clicks through to the game while the meter is locked.
pub fn apply_lock(app: &tauri::AppHandle) {
    if let Some(pin) = app.get_webview_window(LABEL) {
        let _ = pin.set_ignore_cursor_events(locked(app));
    }
}

/// Where it was left, kept as it moves.
pub fn note_moved(app: &tauri::AppHandle, at: tauri::PhysicalPosition<i32>) {
    if let Some(state) = app.try_state::<AppState>() {
        state.settings.set(PLACE_KEY, &format!("{},{}", at.x, at.y));
    }
}

/// Open the pin, or close it.
#[tauri::command]
pub async fn fp_pin_show(app: tauri::AppHandle, on: bool) -> Result<(), String> {
    if !on {
        if let Some(pin) = app.get_webview_window(LABEL) {
            let _ = pin.close();
        }
        return Ok(());
    }
    if app.get_webview_window(LABEL).is_some() {
        apply_lock(&app);
        return Ok(());
    }
    let front = crate::platform::window::front_window();
    let pin = crate::platform::window::share_process(
        tauri::WebviewWindowBuilder::new(&app, LABEL, tauri::WebviewUrl::App("fp-pin.html".into())),
        crate::platform::ProcessGroup::Meter,
    )
    .title("A2Tools FP")
    .inner_size(WIDTH, HEIGHT)
    .decorations(false)
    .transparent(true)
    .shadow(false)
    .always_on_top(true)
    .skip_taskbar(true)
    // As the plugin widgets: resizable only where GTK would otherwise size
    // it to the page (see plugins::widgets).
    .resizable(cfg!(target_os = "linux"))
    .focused(false)
    .focusable(false)
    .build()
    .map_err(|e| e.to_string())?;
    crate::platform::window::joined(&pin, crate::platform::ProcessGroup::Meter);
    // Never in front of the game.
    crate::platform::window::never_activate(&pin, front);
    crate::platform::window::set_size(&pin, tauri::LogicalSize::new(WIDTH, HEIGHT).into());
    if let Some((x, y)) = saved_place(&app) {
        let _ = pin.set_position(tauri::PhysicalPosition::new(x, y));
    }
    apply_lock(&app);
    Ok(())
}

/// The pin's page asks to be dragged (unless the meter is locked).
#[tauri::command]
pub fn fp_pin_drag(app: tauri::AppHandle, window: tauri::WebviewWindow) -> Result<(), String> {
    if window.label() == LABEL && !locked(&app) {
        crate::platform::window::start_drag(&window);
    }
    Ok(())
}

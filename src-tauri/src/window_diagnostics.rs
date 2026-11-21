//! A snapshot of the meter's windows in the debug log, for window problems a
//! player reports (a meter that takes no clicks, windows in the wrong place,
//! one gone behind the game): the display the meter runs on, the monitors,
//! and each window as it is at that moment.
//!
//! Written at debug level, so only into `debug.log` and only while Settings →
//! Diagnostics → Enable debug logging is on: five seconds after start, when
//! debug logging is turned on, and as Send Logs to Dev is pressed, so a log
//! sent while the meter is wrong shows it wrong. Window events between
//! snapshots (shown, hidden, concealed, input region changed) are logged by
//! the platform code, under the same "windows" target.

use tauri::Manager;

/// Debug logging asked for at start (`A2TOOLS_DEBUG_LOG=1`), without
/// Settings: for a meter that takes no clicks. It is not saved.
pub fn debug_log_requested() -> bool {
    std::env::var("A2TOOLS_DEBUG_LOG").is_ok_and(|v| v.trim() == "1")
}

/// Log the snapshot, saying what asked for it.
pub fn log_snapshot(app: &tauri::AppHandle, why: &str) {
    if !crate::logging::logger::is_debug_enabled() {
        return;
    }
    tracing::debug!(target: "windows", "== window snapshot ({why})");
    tracing::debug!(target: "windows", "environment: {}", crate::platform::window::environment());
    if let Ok(monitors) = app.available_monitors() {
        for (i, m) in monitors.iter().enumerate() {
            let (p, s) = (m.position(), m.size());
            tracing::debug!(
                target: "windows",
                "monitor {i}: {} at {},{} {}x{} scale {}",
                m.name().map(String::as_str).unwrap_or("?"),
                p.x,
                p.y,
                s.width,
                s.height,
                m.scale_factor()
            );
        }
    }
    let mut windows: Vec<_> = app.webview_windows().into_iter().collect();
    windows.sort_by(|a, b| a.0.cmp(&b.0));
    for (label, w) in windows {
        let at = w.outer_position().map(|p| format!("{},{}", p.x, p.y)).unwrap_or_else(|_| "?".into());
        let size = w.outer_size().map(|s| format!("{}x{}", s.width, s.height)).unwrap_or_else(|_| "?".into());
        let known = crate::platform::window::known_position(&w).map(|(x, y)| format!(" (placed at {x},{y})")).unwrap_or_default();
        tracing::debug!(
            target: "windows",
            "window {label} \"{}\": visible {}, minimized {}, focused {}, at {at}{known}, {size}, scale {}; {}",
            w.title().unwrap_or_default(),
            w.is_visible().unwrap_or(false),
            w.is_minimized().unwrap_or(false),
            w.is_focused().unwrap_or(false),
            w.scale_factor().unwrap_or(0.0),
            crate::platform::window::diagnostics(&w)
        );
    }
}

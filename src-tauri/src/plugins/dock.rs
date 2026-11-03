//! The plugin dock: one window holding the plugins as tabs (or stacked), so
//! each plugin no longer needs a window of its own (Hirai, #101). A plugin
//! can still be popped out to its own window (Settings → Plugins, or the
//! tab's button) and docked back.
//!
//! Each docked plugin keeps its own sandboxed frame in the dock's page
//! (pluginDock.js), with its own origin, as in a window of its own. The page
//! is the meter's: it names the plugin each frame's call is for, and the
//! commands accept that name only for a plugin docked right now
//! (`plugin_of_call`), then check its permissions as before.
//!
//! Which plugins have their own window is a setting (`OWN_WINDOW_KEY`); the
//! rest are docked. A docked plugin's tab is shown or hidden as its window
//! was (`WINDOWS_KEY`): the dock is up while any tab is shown. The page keeps
//! the tabs' order, the open tab, the layout, the fold and the opacity.

use serde_json::Value;
use tauri::{Emitter, Manager};

use super::{disabled, host, listed, valid_id, WINDOWS_KEY};
use crate::app::AppState;

/// The dock window's label. Not `plugin-…`, which names a plugin's own window.
pub const LABEL: &str = "plugins-dock";
/// Its title: ending " (plugin)", so KWin's rule for plugin windows covers it
/// (kwin_rules::plugin_windows_rule_entries), and KWin keeps its place.
pub const TITLE: &str = "A2Tools Plugins (plugin)";
/// The plugins that open in a window of their own, not in the dock.
pub const OWN_WINDOW_KEY: &str = "dpsMeter.pluginsOwnWindow";
/// The dock's key where plugin windows keep their place and size: `_` is no
/// plugin id's, so it never meets one.
pub const PLACE_ID: &str = "_dock";
/// The tabs that were open when the dock's × closed it: they come back with
/// it, whichever plugin brings it back (a widget double-clicked brought only
/// its own tab, 2026-10-23).
const CLOSED_WITH_KEY: &str = "dpsMeter.pluginDockClosedWith";

/// The plugin a call from window `label` is for. A plugin's own window is its
/// plugin's, whatever the call says; the dock's calls name their plugin,
/// which must be one docked now (`docked`). Anything else is refused.
pub fn plugin_of_call(label: &str, plugin: Option<&str>, docked: &[String]) -> Result<String, String> {
    if label == LABEL {
        let id = plugin.filter(|id| valid_id(id)).ok_or("the dock names no plugin")?;
        return if docked.iter().any(|d| d == id) { Ok(id.to_string()) } else { Err("that plugin is not in the dock".into()) };
    }
    label
        .strip_prefix("plugin-")
        .filter(|id| valid_id(id))
        .map(str::to_string)
        .ok_or_else(|| "not a plugin window".to_string())
}

// An explicitly opened configuration dock stays usable after its alerts hide.
// This is only this run's window state, never a saved plugin preference.
static CONFIGURATION_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static HELD_FOR_FOCUS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub(super) fn set_configuration_open(open: bool) {
    CONFIGURATION_OPEN.store(open, std::sync::atomic::Ordering::Relaxed);
    if !open {
        HELD_FOR_FOCUS.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}
pub(super) fn configuration_open() -> bool {
    CONFIGURATION_OPEN.load(std::sync::atomic::Ordering::Relaxed)
}
pub fn wants_visible(app: &tauri::AppHandle) -> bool {
    configuration_open() || !shown_members(app).is_empty()
}

/// A plugin callback must not bring the dock back over another program.
/// Remember the withheld show even if the focus ticker never saw it up.
pub(super) fn hold_unfocused(app: &tauri::AppHandle) -> bool {
    if wants_visible(app) && super::hides_unfocused(app, LABEL)
        && !crate::platform::window::front_is_ours_or_game()
    {
        HELD_FOR_FOCUS.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(window) = app.get_webview_window(LABEL) {
            crate::platform::window::hide_in_place(&window);
        }
        return true;
    }
    false
}

/// Retry a withheld show when focus returns, or its hide option changes.
pub fn sync_focus_hiding(app: &tauri::AppHandle) {
    if !HELD_FOR_FOCUS.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    if !wants_visible(app) {
        HELD_FOR_FOCUS.store(false, std::sync::atomic::Ordering::Relaxed);
    } else if !hold_unfocused(app) {
        HELD_FOR_FOCUS.store(false, std::sync::atomic::Ordering::Relaxed);
        sync(app, None);
    }
}

/// Whether plugin `id` goes in the dock (rather than a window of its own).
pub fn docks(app: &tauri::AppHandle, id: &str) -> bool {
    !listed(app, OWN_WINDOW_KEY).iter().any(|o| o == id)
}

/// The plugins in the dock now: loaded, turned on, not closed this run, and
/// not set to a window of their own.
pub fn members(app: &tauri::AppHandle) -> Vec<String> {
    let Ok(host) = host(app) else { return Vec::new() };
    let off = disabled(app);
    let own = listed(app, OWN_WINDOW_KEY);
    let closed = host.closed.lock().unwrap_or_else(|e| e.into_inner()).clone();
    host.list()
        .into_iter()
        .filter(|p| p.error.is_none() && !off.contains(&p.id) && !own.contains(&p.id) && !closed.contains(&p.id))
        .map(|p| p.id)
        .collect()
}

/// Whether a docked plugin's tab is shown: as its window was left.
fn tab_shown(app: &tauri::AppHandle, id: &str) -> bool {
    let windows: serde_json::Map<String, Value> = app
        .try_state::<AppState>()
        .and_then(|s| s.settings.get(WINDOWS_KEY))
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    windows.get(id).and_then(Value::as_str) != Some("hidden")
}

/// The docked plugins whose tabs are shown.
pub fn shown_members(app: &tauri::AppHandle) -> Vec<String> {
    members(app).into_iter().filter(|id| tab_shown(app, id)).collect()
}

/// What the dock's page shows: each docked plugin's tab.
pub fn layout(app: &tauri::AppHandle) -> Value {
    let Ok(host) = host(app) else { return serde_json::json!({ "tabs": [] }) };
    let tabs: Vec<Value> = members(app)
        .into_iter()
        .filter_map(|id| {
            let (name, version) = host.with(&id, |l| (l.manifest.name.clone(), l.manifest.version.clone()))?;
            Some(serde_json::json!({ "id": id, "name": name, "version": version, "shown": tab_shown(app, &id) }))
        })
        .collect();
    serde_json::json!({ "tabs": tabs })
}

/// Bring the dock in line with its plugins: built when it has any, closed
/// when it has none, shown while any tab is shown, and its page told the
/// tabs. `focus` also opens that plugin's tab.
pub fn sync(app: &tauri::AppHandle, focus: Option<&str>) {
    let members = members(app);
    let window = app.get_webview_window(LABEL);
    if members.is_empty() {
        if let Some(window) = window {
            let _ = window.close();
        }
        set_configuration_open(false);
        return;
    }
    let shown = shown_members(app);
    // Back after its ×: the tabs it closed with come back too.
    let closed_with = listed(app, CLOSED_WITH_KEY);
    if !shown.is_empty() && !closed_with.is_empty() {
        if let Some(state) = app.try_state::<AppState>() {
            state.settings.set(CLOSED_WITH_KEY, "[]");
        }
        for id in closed_with.iter().filter(|id| members.contains(id) && !shown.contains(id)) {
            super::remember_window(app, id, Some("shown"));
        }
    }
    let visible = wants_visible(app);
    let Some(window) = window else {
        // Built off the calling thread: on Windows, building a window from a
        // synchronous command deadlocks.
        let app = app.clone();
        // The tab to open: the page asks for it when it starts (`dock_hello`),
        // as an event sent now would arrive before the page listens.
        *first_focus().lock().unwrap_or_else(|p| p.into_inner()) = focus.map(str::to_string);
        tauri::async_runtime::spawn(async move {
            if let Err(e) = build(&app, visible) {
                tracing::info!("Plugins: the dock could not open: {e}");
            }
        });
        return;
    };
    let _ = app.emit_to(LABEL, "dock-layout", layout(app));
    if let Some(id) = focus {
        let _ = app.emit_to(LABEL, "dock-focus", id);
    }
    let up = window.is_visible().unwrap_or(false) && !crate::platform::window::is_hidden_in_place(&window);
    if visible && !up {
        if hold_unfocused(app) {
            // The focus ticker restores it only while still wanted.
        } else if super::window_game_hides(app, LABEL) {
            super::hold_for_game(&window);
        } else if !crate::platform::window::reveal_in_place(&window) {
            super::show_in_place(app, &window, PLACE_ID, || crate::platform::window::show_on_top_without_focus(&window));
        }
    } else if !visible && up {
        crate::platform::window::hide_in_place(&window);
        crate::app::keep_something_on_screen(app, LABEL);
    }
}

/// A plugin that showed itself in the dock (an alert, Hidden Cubes' found
/// cube): how the dock was before, put back when it hides itself again.
#[derive(Clone, Copy)]
pub struct Raised {
    pub dock_was_up: bool,
    pub tab_was_shown: bool,
}

fn raised() -> &'static std::sync::Mutex<std::collections::HashMap<String, Raised>> {
    static RAISED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Raised>>> = std::sync::OnceLock::new();
    RAISED.get_or_init(Default::default)
}

/// `id` shows itself: note how the dock was, unless it already showed
/// itself since (the first time is what to go back to).
pub fn note_raised(app: &tauri::AppHandle, id: &str) {
    let dock_was_up = app
        .get_webview_window(LABEL)
        .is_some_and(|w| w.is_visible().unwrap_or(false) && !crate::platform::window::is_hidden_in_place(&w));
    let tab_was_shown = shown_members(app).iter().any(|m| m == id);
    raised()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry(id.to_string())
        .or_insert(Raised { dock_was_up, tab_was_shown });
}

/// How the dock was before `id` showed itself, if it did.
pub fn take_raised(id: &str) -> Option<Raised> {
    raised().lock().unwrap_or_else(|p| p.into_inner()).remove(id)
}

/// The dock out of sight, its tabs as they are (shown again as they were).
pub fn hide_now(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window(LABEL) {
        crate::platform::window::hide_in_place(&window);
        crate::app::keep_something_on_screen(app, LABEL);
    }
}

/// Build the dock window, shown or hidden, where it was left.
fn build(app: &tauri::AppHandle, visible: bool) -> Result<(), String> {
    if app.get_webview_window(LABEL).is_some() {
        return Ok(());
    }
    let visible = visible && wants_visible(app);
    let held_for_focus = visible && hold_unfocused(app);
    let held_for_game = visible && super::window_game_hides(app, LABEL);
    let visible = visible && !held_for_focus && !held_for_game;
    let (w, h) = super::saved_size(app, PLACE_ID).unwrap_or((380.0, 520.0));
    crate::app::note_window_title(TITLE, LABEL);
    let window = crate::platform::window::share_process(tauri::WebviewWindowBuilder::new(app, LABEL, tauri::WebviewUrl::App("plugin-dock.html".into())), crate::platform::ProcessGroup::Plugins)
        .title(TITLE)
        .inner_size(w, h)
        .min_inner_size(240.0, 120.0)
        .decorations(false)
        .always_on_top(true)
        .resizable(true)
        .visible(visible || crate::platform::window::HIDE_AFTER_BUILD)
        .focused(false)
        // Transparent, so the panel's opacity slider shows the game through it.
        .transparent(true)
        .shadow(false)
        .build()
        .map_err(|e| e.to_string())?;
    crate::platform::window::joined(&window, crate::platform::ProcessGroup::Plugins);
    if !visible && crate::platform::window::HIDE_AFTER_BUILD {
        crate::platform::window::hide_without_focus(&window);
    }
    if held_for_game {
        super::hold_for_game(&window);
    }
    // Building can take long enough for foreground ownership to change.
    hold_unfocused(app);
    if let Some((x, y)) = super::saved_position(app, &window, PLACE_ID) {
        let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
    }
    Ok(())
}

/// What the dock's page needs to start: its tabs, the meter's language for
/// its own labels, and the Settings colours.
#[tauri::command]
pub fn dock_hello(app: tauri::AppHandle) -> Value {
    let mut hello = layout(&app);
    hello["language"] = Value::String(
        app.try_state::<AppState>().and_then(|s| s.settings.get("dpsMeter.language")).unwrap_or_else(|| "en".into()),
    );
    hello["theme"] = super::plugin_theme(&app);
    if let Some(id) = first_focus().lock().unwrap_or_else(|p| p.into_inner()).take() {
        hello["focus"] = Value::String(id);
    }
    hello
}

/// The tab a dock being built is to open on (`sync`, read by `dock_hello`).
fn first_focus() -> &'static std::sync::Mutex<Option<String>> {
    static FOCUS: std::sync::OnceLock<std::sync::Mutex<Option<String>>> = std::sync::OnceLock::new();
    FOCUS.get_or_init(Default::default)
}

/// Fold the dock to its tab strip, `height` CSS px tall, or unfold it to the
/// size it had (`None`). The size before folding is kept until then, so a
/// fold never becomes the size the dock is remembered at (note_resized skips
/// sizes under 120 px).
#[tauri::command]
pub fn dock_fold(window: tauri::WebviewWindow, height: Option<f64>) -> Result<(), String> {
    static BEFORE: std::sync::OnceLock<std::sync::Mutex<Option<tauri::PhysicalSize<u32>>>> = std::sync::OnceLock::new();
    if window.label() != LABEL {
        return Err("not the dock".into());
    }
    let before = BEFORE.get_or_init(Default::default);
    let mut kept = before.lock().unwrap_or_else(|p| p.into_inner());
    match height {
        Some(h) => {
            let h = h.clamp(24.0, 120.0);
            let scale = window.scale_factor().map_err(|e| e.to_string())?;
            let now = window.inner_size().map_err(|e| e.to_string())?;
            kept.get_or_insert(now);
            let width = now.to_logical::<f64>(scale).width;
            let _ = window.set_min_size(Some(tauri::LogicalSize::new(240.0, h)));
            window.set_size(tauri::LogicalSize::new(width, h)).map_err(|e| e.to_string())?;
        }
        None => {
            if let Some(size) = kept.take() {
                let _ = window.set_min_size(Some(tauri::LogicalSize::new(240.0, 120.0)));
                window.set_size(size).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

/// The dock's × button: every tab hidden, the dock with them. The plugins
/// keep running, as with a plugin window's ×.
#[tauri::command]
pub fn dock_close(app: tauri::AppHandle) {
    let open = shown_members(&app);
    if let Some(state) = app.try_state::<AppState>() {
        state.settings.set(CLOSED_WITH_KEY, &serde_json::to_string(&open).unwrap_or_default());
    }
    for id in members(&app) {
        super::remember_window(&app, &id, Some("hidden"));
    }
    set_configuration_open(false);
    sync(&app, None);
}

/// A click inside an existing dock keeps its configuration open. Selection
/// belongs to the page: never echo dock-focus, build, show or activate here.
/// Synchronous with dock_close, so a delayed acknowledgement cannot reopen it.
#[tauri::command]
pub fn dock_select(app: tauri::AppHandle, window: tauri::WebviewWindow, id: String) -> Result<(), String> {
    if window.label() != LABEL || !wants_visible(&app) {
        return Err("the dock is not open".into());
    }
    let id = plugin_of_call(window.label(), Some(&id), &members(&app))?;
    set_configuration_open(true);
    super::remember_window(&app, &id, Some("shown"));
    app.emit_to(LABEL, "dock-layout", layout(&app)).map_err(|e| e.to_string())
}

/// Dock a plugin, or give it a window of its own (Settings → Plugins, and
/// the dock tab's pop-out button / the window's dock button).
/// Async: it may build a window.
#[tauri::command]
pub async fn plugin_set_docked(app: tauri::AppHandle, id: String, docked: bool) -> Result<(), String> {
    if !valid_id(&id) {
        return Err("not a plugin id".into());
    }
    let state = app.try_state::<AppState>().ok_or("no app state")?;
    let mut own = listed(&app, OWN_WINDOW_KEY);
    own.retain(|o| o != &id);
    if !docked {
        own.push(id.clone());
    }
    state.settings.set(OWN_WINDOW_KEY, &serde_json::to_string(&own).unwrap_or_default());
    if docked { set_configuration_open(true); }
    super::remember_window(&app, &id, Some("shown"));
    if docked {
        if let Some(window) = app.get_webview_window(&format!("plugin-{id}")) {
            let _ = window.close();
        }
        sync(&app, Some(&id));
    } else {
        // Out of the dock first (its frame stops), then in its own window.
        sync(&app, None);
        if !disabled(&app).contains(&id) {
            super::open_window(&app, &id, true)?;
        }
    }
    tracing::info!("Plugins: {id} {}", if docked { "docked" } else { "in its own window" });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn docked() -> Vec<String> {
        vec!["event-timers".to_string(), "ping".to_string()]
    }

    #[test]
    fn the_dock_speaks_only_for_its_docked_plugins() {
        assert_eq!(plugin_of_call(LABEL, Some("ping"), &docked()), Ok("ping".to_string()));
        // Undocked (its own window, turned off, or never loaded): refused.
        assert!(plugin_of_call(LABEL, Some("pet-souls"), &docked()).is_err());
        // No plugin named, or not an id at all.
        assert!(plugin_of_call(LABEL, None, &docked()).is_err());
        assert!(plugin_of_call(LABEL, Some("../ping"), &docked()).is_err());
        assert!(plugin_of_call(LABEL, Some(""), &[String::new()]).is_err());
    }

    #[test]
    fn a_plugins_own_window_is_its_plugins_whatever_the_call_says() {
        assert_eq!(plugin_of_call("plugin-ping", None, &[]), Ok("ping".to_string()));
        // A window can't speak for another plugin, docked or not.
        assert_eq!(plugin_of_call("plugin-ping", Some("event-timers"), &docked()), Ok("ping".to_string()));
        assert!(plugin_of_call("main", Some("ping"), &docked()).is_err());
        assert!(plugin_of_call("settings", None, &docked()).is_err());
    }

    #[test]
    fn the_dock_label_and_place_never_meet_a_plugin() {
        assert!(!LABEL.starts_with("plugin-"));
        assert!(!valid_id(PLACE_ID));
        assert!(TITLE.ends_with(" (plugin)"));
    }
}

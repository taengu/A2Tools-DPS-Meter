//! Plugin widgets (#77): small windows a plugin opens beside its own, such as
//! a countdown icon, which the player can drag anywhere and lock in place.
//!
//! A widget shows the plugin's `widget.html`, sandboxed like its main page,
//! inside `plugin-widget.html`. The plugin's main window opens, closes and
//! feeds its widgets (`a2.widgets`, permission `widgets`); a widget can only
//! ask to be dragged and post a message back to its plugin (`a2.widget`).
//! Each widget remembers where it was left; locked, a plugin's widgets let
//! clicks through to the game and cannot be moved until unlocked. They close
//! with the plugin's main window.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;
use tauri::{Emitter, Manager};

use super::{host, require, valid_id, window_plugin};
use crate::app::AppState;

/// Most widgets one plugin may have open.
const MAX_WIDGETS: usize = 16;
/// Most bytes of data a widget message may carry, either way.
const MAX_MESSAGE: usize = 64 * 1024;

/// A widget's window label: `pw_<plugin>_<name>`. Neither a plugin id nor a
/// widget name has a `_`, so the label reads back unambiguously, and it never
/// starts with `plugin-`, so no main-window command accepts it.
fn label(id: &str, name: &str) -> String {
    format!("pw_{id}_{name}")
}

/// The plugin and widget a window label names, if it is a widget's.
pub fn parse_label(label: &str) -> Option<(String, String)> {
    let (id, name) = label.strip_prefix("pw_")?.split_once('_')?;
    (valid_id(id) && valid_name(name)).then(|| (id.to_string(), name.to_string()))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 32 && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn locked_key(id: &str) -> String {
    format!("dpsMeter.pluginWidgetsLocked.{id}")
}

fn place_key(id: &str, name: &str) -> String {
    format!("dpsMeter.pluginWidget.{id}.{name}")
}

fn is_locked(app: &tauri::AppHandle, id: &str) -> bool {
    app.try_state::<AppState>().and_then(|s| s.settings.get(&locked_key(id))).as_deref() == Some("true")
}

/// The calling window's widget: its plugin and name.
fn widget_of(window: &tauri::WebviewWindow) -> Result<(String, String), String> {
    parse_label(window.label()).ok_or_else(|| "not a widget window".to_string())
}

/// The widgets of plugin `id` now open, by label.
/// Plugin `id`'s widget windows, the ones its plugin has closed included.
fn all_widgets(app: &tauri::AppHandle, id: &str) -> Vec<tauri::WebviewWindow> {
    app.webview_windows()
        .into_iter()
        .filter(|(label, _)| parse_label(label).is_some_and(|(owner, _)| owner == id))
        .map(|(_, window)| window)
        .collect()
}

/// Plugin `id`'s open widgets: not the ones it closed.
fn open_widgets(app: &tauri::AppHandle, id: &str) -> Vec<tauri::WebviewWindow> {
    all_widgets(app, id).into_iter().filter(|w| !is_closed(w.label())).collect()
}

/// The widgets their plugin closed, by label. A closed widget's window is
/// hidden and kept, so opening it again shows it at once: a new window and
/// page took half a second or more (Ping's display, toggled, 2026-10-23).
/// Nothing that brings hidden windows back shows one of these.
fn closed() -> &'static Mutex<std::collections::HashSet<String>> {
    static CLOSED: std::sync::OnceLock<Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new();
    CLOSED.get_or_init(Mutex::default)
}

/// Whether the widget window `label` is one its plugin closed.
pub fn is_closed(label: &str) -> bool {
    closed().lock().unwrap_or_else(|p| p.into_inner()).contains(label)
}

/// Widgets opened while another app is in front, including ones the focus
/// poll has never seen. Take a snapshot before touching windows: native calls
/// can wait for the UI thread, and a concurrent open must keep its own entry.
fn held_for_focus() -> &'static Mutex<std::collections::HashSet<String>> {
    static HELD: std::sync::OnceLock<Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new();
    HELD.get_or_init(Mutex::default)
}

/// Keep a widget out of sight before showing it or loading its plugin frame.
/// Also used when game-closed hiding releases it; that caller holds its own
/// game-hiding lock, so this function must not call `hold_for_game`.
pub fn hold_unfocused(app: &tauri::AppHandle, window: &tauri::WebviewWindow) -> bool {
    let Some((id, _)) = parse_label(window.label()) else { return false };
    let inactive = is_closed(window.label()) || super::disabled(app).contains(&id);
    if !inactive && !(super::hides_unfocused(app, window.label()) && !crate::platform::window::front_is_ours_or_game()) {
        return false;
    }
    if !crate::platform::window::is_hidden_in_place(window) {
        crate::platform::window::hide_in_place(window);
    }
    if !inactive {
        held_for_focus().lock().unwrap_or_else(|p| p.into_inner()).insert(window.label().to_string());
    }
    true
}

/// Either automatic hide rule may still own a widget the other is releasing.
pub fn hold_hidden(app: &tauri::AppHandle, window: &tauri::WebviewWindow) -> bool {
    if hold_unfocused(app, window) {
        return true;
    }
    if let Some((id, _)) = parse_label(window.label()) && super::game_hides(app, &id) {
        super::hold_for_game(window);
        return true;
    }
    false
}

/// Open again when allowed, even if this plugin opted out while others still
/// hide. Closed, disabled or destroyed widgets are not brought back.
pub fn sync_focus_holds(app: &tauri::AppHandle) {
    let pending = std::mem::take(&mut *held_for_focus().lock().unwrap_or_else(|p| p.into_inner()));
    for label in pending {
        let Some(window) = app.get_webview_window(&label) else { continue };
        if hold_hidden(app, &window) {
            continue;
        }
        if !crate::platform::window::reveal_in_place(&window) {
            crate::platform::window::show_on_top_without_focus(&window);
        }
        relock(app, &window);
    }
}

/// The parts of each widget that take the mouse, as its page reported them
/// (`a2.widget.area`, `a2.widget.areas`): rectangles in CSS px, and the
/// page's scale. Unlocked, clicks elsewhere in its window go through (by an
/// input region on Linux, a window region on Windows), so icons set close
/// together don't catch each other's drags, and the room a widget keeps for
/// more icons doesn't catch the game's clicks.
type Area = (Vec<(f64, f64, f64, f64)>, f64);

/// Most rectangles one widget may report.
const MAX_AREA_RECTS: usize = 64;

/// One rectangle of a widget's area, as its page sends it.
#[derive(serde::Deserialize)]
pub struct AreaRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// A widget's reported area, checked: every rectangle within the window's
/// possible size, at most `MAX_AREA_RECTS`, and a sane scale.
fn checked_area(rects: &[AreaRect], scale: f64) -> Result<Area, String> {
    let ok = |v: f64| v.is_finite() && (0.0..=4000.0).contains(&v);
    if rects.len() > MAX_AREA_RECTS || !(scale.is_finite() && scale > 0.0 && scale <= 8.0) {
        return Err("not an area".into());
    }
    let mut out = Vec::with_capacity(rects.len());
    for r in rects {
        if !(ok(r.x) && ok(r.y) && ok(r.width) && ok(r.height)) {
            return Err("not an area".into());
        }
        out.push((r.x, r.y, r.width, r.height));
    }
    Ok((out, scale))
}
fn areas() -> &'static Mutex<HashMap<String, Area>> {
    static AREAS: std::sync::OnceLock<Mutex<HashMap<String, Area>>> = std::sync::OnceLock::new();
    AREAS.get_or_init(Mutex::default)
}

/// Locked: clicks go through to the game. Unlocked: only its area (when it
/// reported one) takes them. On Linux by an input region, which needs the
/// window on screen (tao's own call would abort without one; see
/// `platform::window::hide_in_place`), so a widget is locked again once its
/// page has loaded (`plugin_widget_hello`) and each time it is shown again.
fn apply_lock(window: &tauri::WebviewWindow, locked: bool) {
    // A screenshot tool in front: clicks go through, whatever the lock.
    let locked = locked || crate::app::screenshot_tool_in_front();
    let area = areas().lock().unwrap_or_else(|p| p.into_inner()).get(window.label()).cloned();
    let applied = match (locked, &area) {
        (true, _) => crate::platform::window::widget_click_through(window, true, None),
        (false, Some((rects, scale))) => crate::platform::window::widget_click_through(window, true, Some((rects, *scale))),
        (false, None) => crate::platform::window::widget_click_through(window, false, None),
    };
    if !applied && !cfg!(target_os = "linux") {
        let _ = window.set_ignore_cursor_events(locked);
    }
}

/// Every widget's click-through as its lock and area say (after a
/// screenshot tool, see `app::screenshot_tool_in_front`).
pub fn reapply_locks(app: &tauri::AppHandle) {
    for (_, window) in app.webview_windows() {
        relock(app, &window);
    }
}

/// A widget's click-through as its lock and area say, if `window` is one:
/// after `platform::window::reveal_in_place`, which gives a window the whole
/// mouse, so a locked widget shown again took the clicks over it.
pub fn relock(app: &tauri::AppHandle, window: &tauri::WebviewWindow) {
    if let Some((id, _)) = parse_label(window.label()) {
        apply_lock(window, is_locked(app, &id));
    }
}

/// The window in front when each widget was made, until its page loads.
fn fronts() -> &'static Mutex<HashMap<String, Option<isize>>> {
    static FRONTS: std::sync::OnceLock<Mutex<HashMap<String, Option<isize>>>> = std::sync::OnceLock::new();
    FRONTS.get_or_init(Mutex::default)
}

pub fn saved_place(app: &tauri::AppHandle, id: &str, name: &str) -> Option<(i32, i32)> {
    let value = app.try_state::<AppState>()?.settings.get(&place_key(id, name))?;
    let (x, y) = value.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// Open widget `name` of the calling plugin, `width` x `height` logical pixels,
/// or show it again if it is open.
#[tauri::command]
pub async fn plugin_widget_open(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    plugin: Option<String>,
    name: String,
    width: f64,
    height: f64,
) -> Result<(), String> {
    let id = window_plugin(&window, plugin.as_deref())?;
    require(&app, &id, "widgets")?;
    // A plugin turned off opens none: its page can still be finishing an
    // update when it goes.
    if super::disabled(&app).contains(&id) {
        return Err("that plugin is turned off".into());
    }
    tracing::debug!("Widgets: {id} opens {name:?} (from {})", window.label());
    if !valid_name(&name) {
        return Err("a widget name is 1 to 32 lowercase letters, digits and '-'".into());
    }
    let size = |v: f64| if v.is_finite() { v.clamp(24.0, 800.0) } else { 64.0 };
    let (width, height) = (size(width), size(height));
    if let Some(widget) = app.get_webview_window(&label(&id, &name)) {
        closed().lock().unwrap_or_else(|p| p.into_inner()).remove(widget.label());
        crate::platform::window::set_size(&widget, tauri::LogicalSize::new(width, height).into());
        // Grown past the screen's edge, or the screen it was on gone.
        if let Ok(at) = widget.outer_position() {
            let (x, y) = keep_on_screen(&app, &widget, (at.x, at.y));
            if (x, y) != (at.x, at.y) {
                let _ = widget.set_position(tauri::PhysicalPosition::new(x, y));
            }
        }
        if !hold_hidden(&app, &widget) && !crate::platform::window::reveal_in_place(&widget) {
            crate::platform::window::show_on_top_without_focus(&widget);
        }
        apply_lock(&widget, is_locked(&app, &id));
        return Ok(());
    }
    if all_widgets(&app, &id).len() >= MAX_WIDGETS {
        // Room made by a closed one going for good, if there is one.
        let Some(spare) = all_widgets(&app, &id).into_iter().find(|w| is_closed(w.label())) else {
            return Err(format!("at most {MAX_WIDGETS} widgets"));
        };
        closed().lock().unwrap_or_else(|p| p.into_inner()).remove(spare.label());
        areas().lock().unwrap_or_else(|p| p.into_inner()).remove(spare.label());
        let _ = spare.destroy();
    }
    let plugin_name = host(&app)?.with(&id, |l| l.manifest.name.clone()).ok_or("no such plugin")?;
    let script = format!(
        "window.__A2_PLUGIN_ID__ = {}; window.__A2_WIDGET__ = {};",
        serde_json::to_string(&id).unwrap_or_default(),
        serde_json::to_string(&name).unwrap_or_default(),
    );
    let title = format!("{plugin_name} · {name}");
    crate::app::note_window_title(&title, &label(&id, &name));
    let front = crate::platform::window::front_window();
    let widget = crate::platform::window::share_process(
        tauri::WebviewWindowBuilder::new(&app, label(&id, &name), tauri::WebviewUrl::App("plugin-widget.html".into())),
        crate::platform::ProcessGroup::Plugins,
    )
        .initialization_script(script)
        // Its own title, so KWin can keep it where it was left (#76).
        .title(title)
        .inner_size(width, height)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        // Resizable, its size then pinned (set_size below), as the meter's
        // other windows are: GTK sizes a window that isn't resizable to what
        // the page asks for, at least 200 x 200, so a 118 x 38 widget had a
        // 200 x 200 window. Dropped near a screen's bottom edge, that window
        // reached past it, and KWin moved it back up each time it opened
        // (Ping, 2026-10-20: left at y 1316, back at 1032). Windows sizes it
        // as asked either way.
        .resizable(cfg!(target_os = "linux"))
        .focused(false)
        // Never takes focus, from the start: clicks and drags still reach it.
        // Focusable, tao turns a GTK window's focus back on after its first
        // draw, and KWin gave a new icon the front at its alert (Hirai,
        // CachyOS KDE, 2.0.71); on Windows it is WS_EX_NOACTIVATE from
        // creation, before WebView2 can take the front.
        .focusable(false)
        .build()
        .map_err(|e| e.to_string())?;
    crate::platform::window::joined(&widget, crate::platform::ProcessGroup::Plugins);
    // Never in front of the game: an alert icon appearing must not tab the
    // player out of it.
    crate::platform::window::never_activate(&widget, front);
    crate::platform::window::set_size(&widget, tauri::LogicalSize::new(width, height).into());
    fronts().lock().unwrap_or_else(|p| p.into_inner()).insert(widget.label().to_string(), front);
    // Where it was left; a widget never moved goes beside the plugin's others
    // rather than wherever the window manager puts a new window (the middle
    // of the screen).
    let place = saved_place(&app, &id, &name).or_else(|| {
        let others: Vec<_> = open_widgets(&app, &id).into_iter().filter(|w| w.label() != widget.label()).collect();
        beside(others.iter().filter_map(|w| {
            let at = w.outer_position().ok()?;
            let size = w.outer_size().ok()?;
            Some((at.x, at.y, size.width as i32, size.height as i32))
        }))
    }).or_else(|| beside_left(&app, &id));
    if let Some(at) = place {
        let (x, y) = keep_on_screen(&app, &widget, at);
        let _ = widget.set_position(tauri::PhysicalPosition::new(x, y));
    }
    hold_hidden(&app, &widget);
    Ok(())
}

/// With none of the plugin's widgets open, a new one goes beside where its
/// widgets were last left, not mid-screen (Hirai, 2026-10-19: an alert's
/// icon in the middle of the game).
fn beside_left(app: &tauri::AppHandle, id: &str) -> Option<(i32, i32)> {
    let state = app.try_state::<AppState>()?;
    let prefix = format!("dpsMeter.pluginWidget.{id}.");
    let places: Vec<(i32, i32)> = state.settings.get_all().into_iter().filter_map(|(key, value)| {
        key.strip_prefix(&prefix)?;
        let (x, y) = value.split_once(',')?;
        Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
    }).collect();
    beside_cluster(&places, &screens(app))
}

/// The width of an icon left somewhere is not kept; an Event Timers icon is
/// about this wide.
const ICON_WIDTH: i32 = 96;

/// Beside the busiest spot among `places`: the place with the most others
/// near it, so one icon left far off (a test icon on another screen) does
/// not draw the next one there (2.0.72: the anchor opened on the second
/// monitor). On that place's screen: to its right, or its left when the
/// right runs off the screen.
fn beside_cluster(places: &[(i32, i32)], screens: &[(i32, i32, u32, u32)]) -> Option<(i32, i32)> {
    const NEAR: i64 = 400;
    let near = |a: &(i32, i32), b: &(i32, i32)| {
        let (dx, dy) = ((a.0 - b.0) as i64, (a.1 - b.1) as i64);
        dx * dx + dy * dy <= NEAR * NEAR
    };
    let &(x, y) = places.iter().max_by_key(|p| (places.iter().filter(|q| near(p, q)).count(), std::cmp::Reverse(p.0)))?;
    let screen = screens.iter().find(|&&(sx, sy, w, h)| {
        x >= sx && x < sx + w as i32 && y >= sy && y < sy + h as i32
    });
    let right = x + ICON_WIDTH + 6;
    match screen {
        Some(&(sx, _, w, _)) if right + ICON_WIDTH > sx + w as i32 => Some(((x - ICON_WIDTH - 6).max(sx), y)),
        _ => Some((right, y)),
    }
}

/// How much of a widget must be on a screen for it to count as shown there:
/// enough to see it and grab it.
const ON_SCREEN: i64 = 48;

/// `rect` (x, y, width, height, physical px) as it is when enough of it is on
/// one of `screens` (their work areas), else moved onto the nearest one, all
/// of it when it fits. A widget left where no screen is any more (a monitor
/// unplugged or rearranged, its scale changed) opened there, out of sight,
/// and its plugin could not tell (#108: Event Timers' anchor, left on a
/// second monitor, never showed again).
fn on_screen(rect: (i32, i32, i32, i32), screens: &[(i32, i32, u32, u32)]) -> (i32, i32) {
    let (x, y, w, h) = (rect.0 as i64, rect.1 as i64, rect.2.max(1) as i64, rect.3.max(1) as i64);
    let need = |side: i64| side.min(ON_SCREEN);
    let shown = screens.iter().any(|&(sx, sy, sw, sh)| {
        let (sx, sy, sw, sh) = (sx as i64, sy as i64, sw as i64, sh as i64);
        let across = (x + w).min(sx + sw) - x.max(sx);
        let down = (y + h).min(sy + sh) - y.max(sy);
        across >= need(w) && down >= need(h)
    });
    if shown || screens.is_empty() {
        return (rect.0, rect.1);
    }
    // The nearest: the least distance from the widget's middle to the screen.
    let (cx, cy) = (x + w / 2, y + h / 2);
    let gap = |&&(sx, sy, sw, sh): &&(i32, i32, u32, u32)| {
        let (sx, sy, sw, sh) = (sx as i64, sy as i64, sw as i64, sh as i64);
        let dx = (sx - cx).max(cx - (sx + sw)).max(0);
        let dy = (sy - cy).max(cy - (sy + sh)).max(0);
        dx * dx + dy * dy
    };
    let &(sx, sy, sw, sh) = screens.iter().min_by_key(gap).expect("screens is not empty");
    let (sx, sy, sw, sh) = (sx as i64, sy as i64, sw as i64, sh as i64);
    let nx = x.clamp(sx, (sx + sw - w).max(sx));
    let ny = y.clamp(sy, (sy + sh - h).max(sy));
    (nx as i32, ny as i32)
}

/// The work areas of the screens connected now.
fn screens(app: &tauri::AppHandle) -> Vec<(i32, i32, u32, u32)> {
    app.available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let a = m.work_area();
            (a.position.x, a.position.y, a.size.width, a.size.height)
        })
        .collect()
}

/// Puts `widget` back on a screen when it is off all of them (see on_screen),
/// remembering the new place. `at` is where it is or is about to be.
fn keep_on_screen(app: &tauri::AppHandle, widget: &tauri::WebviewWindow, at: (i32, i32)) -> (i32, i32) {
    let size = widget.outer_size().map(|s| (s.width as i32, s.height as i32)).unwrap_or((64, 64));
    let place = on_screen((at.0, at.1, size.0, size.1), &screens(app));
    if place != at {
        tracing::info!("Widgets: {} was off every screen at {},{}: moved to {},{}", widget.label(), at.0, at.1, place.0, place.1);
        if let (Some((id, name)), Some(state)) = (parse_label(widget.label()), app.try_state::<AppState>()) {
            state.settings.set(&place_key(&id, &name), &format!("{},{}", place.0, place.1));
        }
    }
    place
}

/// Just right of the rightmost of these places, on its row, with a gap.
pub fn beside(places: impl Iterator<Item = (i32, i32, i32, i32)>) -> Option<(i32, i32)> {
    places.max_by_key(|&(x, _, w, _)| x + w).map(|(x, y, w, _)| (x + w + 6, y))
}

/// The widget a window label names, beside its plugin's other widgets, for
/// the window manager to place it (Linux on Wayland, see app.rs): from where
/// the others were left.
pub fn place_beside_others(app: &tauri::AppHandle, label: &str, place_of: impl Fn(&str) -> Option<(i32, i32, i32, i32)>) -> Option<(i32, i32)> {
    let (id, _) = parse_label(label)?;
    beside(open_widgets(app, &id).iter().filter(|w| w.label() != label).filter_map(|w| place_of(w.label())))
        .or_else(|| beside_left(app, &id))
}

/// The parts of the calling widget that take the mouse while unlocked (CSS
/// px from its top left, and the page's scale): `rects`, from
/// `a2.widget.areas` (meter 2.0.77 on), or one rectangle as `x`, `y`,
/// `width`, `height`, from `a2.widget.area`.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub fn plugin_widget_area(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    rects: Option<Vec<AreaRect>>,
    x: Option<f64>,
    y: Option<f64>,
    width: Option<f64>,
    height: Option<f64>,
    scale: f64,
) -> Result<(), String> {
    let (id, _) = widget_of(&window)?;
    let rects = match (rects, x, y, width, height) {
        (Some(rects), ..) => rects,
        (None, Some(x), Some(y), Some(width), Some(height)) => vec![AreaRect { x, y, width, height }],
        _ => return Err("not an area".into()),
    };
    let area = checked_area(&rects, scale)?;
    areas().lock().unwrap_or_else(|p| p.into_inner()).insert(window.label().to_string(), area);
    apply_lock(&window, is_locked(&app, &id));
    Ok(())
}

/// Where widget `name` of the calling plugin is: `{ x, y, width, height }`
/// in physical screen pixels, and the work area of the screen it is on
/// (`screen`, the same), or null when it is not open.
#[tauri::command]
pub fn plugin_widget_where(app: tauri::AppHandle, window: tauri::WebviewWindow, plugin: Option<String>, name: String) -> Result<Value, String> {
    let id = window_plugin(&window, plugin.as_deref())?;
    require(&app, &id, "widgets")?;
    let Some(widget) = app.get_webview_window(&label(&id, &name)) else { return Ok(Value::Null) };
    let at = widget.outer_position().map_err(|e| e.to_string())?;
    let size = widget.outer_size().map_err(|e| e.to_string())?;
    let screen = widget.current_monitor().ok().flatten().map(|m| {
        let area = m.work_area();
        serde_json::json!({ "x": area.position.x, "y": area.position.y,
                            "width": area.size.width, "height": area.size.height })
    });
    Ok(serde_json::json!({ "x": at.x, "y": at.y, "width": size.width, "height": size.height, "screen": screen }))
}

/// Move widget `name` of the calling plugin to `x, y` (physical screen
/// pixels, its top left), as a plugin that lays its widgets out does.
/// Locked or not: the lock is the player's hand off them, not the plugin's.
/// A widget not open yet opens there: the place is kept for it, where the
/// window manager reads it too (KDE on Wayland places a new window itself,
/// and lets no app move one after: an icon placed only once open stayed
/// mid-screen there, Hirai 2026-10-19).
#[tauri::command]
pub fn plugin_widget_place(app: tauri::AppHandle, window: tauri::WebviewWindow, plugin: Option<String>, name: String, x: i32, y: i32) -> Result<(), String> {
    let id = window_plugin(&window, plugin.as_deref())?;
    require(&app, &id, "widgets")?;
    if !valid_name(&name) {
        return Err("a widget name is 1 to 32 lowercase letters, digits and '-'".into());
    }
    let far = 1_000_000;
    if !(-far..far).contains(&x) || !(-far..far).contains(&y) {
        return Err("not a place on a screen".into());
    }
    let widget_label = label(&id, &name);
    // A plugin laying its widgets out keeps them on a screen too (one open
    // has its size; one not open yet, an icon's).
    let open = app.get_webview_window(&widget_label);
    let size = open.as_ref().and_then(|w| w.outer_size().ok()).map(|s| (s.width as i32, s.height as i32)).unwrap_or((64, 64));
    let (x, y) = on_screen((x, y, size.0, size.1), &screens(&app));
    if let Some(state) = app.try_state::<AppState>() {
        state.settings.set(&place_key(&id, &name), &format!("{x},{y}"));
        state.settings.set(&format!("window.{widget_label}.placed"), &format!("{x},{y},0,0"));
    }
    match open {
        Some(widget) => widget.set_position(tauri::PhysicalPosition::new(x, y)).map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

/// Close widget `name` of the calling plugin: hidden and kept (`closed`),
/// its page and place as they are, for opening it again at once.
#[tauri::command]
pub fn plugin_widget_close(app: tauri::AppHandle, window: tauri::WebviewWindow, plugin: Option<String>, name: String) -> Result<(), String> {
    let id = window_plugin(&window, plugin.as_deref())?;
    require(&app, &id, "widgets")?;
    if let Some(widget) = app.get_webview_window(&label(&id, &name)) {
        tracing::debug!("Widgets: {id} closes {name:?} (from {})", window.label());
        closed().lock().unwrap_or_else(|p| p.into_inner()).insert(widget.label().to_string());
        crate::platform::window::hide_in_place(&widget);
    }
    Ok(())
}

/// Hand widget `name` data from its plugin: it hears it as a "widget" event.
#[tauri::command]
pub fn plugin_widget_send(app: tauri::AppHandle, window: tauri::WebviewWindow, plugin: Option<String>, name: String, data: Value) -> Result<(), String> {
    let id = window_plugin(&window, plugin.as_deref())?;
    require(&app, &id, "widgets")?;
    if data.to_string().len() > MAX_MESSAGE {
        return Err("message too large".into());
    }
    let event = serde_json::json!({ "plugin": id, "event": "widget", "data": data });
    app.emit_to(label(&id, &name), "plugin-event", event).map_err(|e| e.to_string())
}

/// Lock (clicks go through, no moving) or unlock all the calling plugin's
/// widgets. Kept across restarts.
#[tauri::command]
pub fn plugin_widgets_lock(app: tauri::AppHandle, window: tauri::WebviewWindow, plugin: Option<String>, locked: bool) -> Result<(), String> {
    let id = window_plugin(&window, plugin.as_deref())?;
    require(&app, &id, "widgets")?;
    if let Some(state) = app.try_state::<AppState>() {
        state.settings.set(&locked_key(&id), if locked { "true" } else { "false" });
    }
    for widget in open_widgets(&app, &id) {
        apply_lock(&widget, locked);
        let event = serde_json::json!({ "plugin": id, "event": "locked", "data": locked });
        let _ = widget.emit("plugin-event", event);
    }
    Ok(())
}

/// Whether the calling plugin's widgets are locked.
#[tauri::command]
pub fn plugin_widgets_locked(app: tauri::AppHandle, window: tauri::WebviewWindow, plugin: Option<String>) -> Result<bool, String> {
    let id = window_plugin(&window, plugin.as_deref())?;
    require(&app, &id, "widgets")?;
    Ok(is_locked(&app, &id))
}

/// What a widget's host page needs to start. Its window is on screen by now,
/// so this is where a locked widget is made click-through.
#[tauri::command]
pub fn plugin_widget_hello(app: tauri::AppHandle, window: tauri::WebviewWindow) -> Result<Value, String> {
    let (id, name) = widget_of(&window)?;
    // Its page has loaded: if that took the front from the game, give it back.
    let before = fronts().lock().unwrap_or_else(|p| p.into_inner()).remove(window.label()).flatten();
    crate::platform::window::give_front_back(&window, before);
    let locked = is_locked(&app, &id);
    apply_lock(&window, locked);
    // The transparent host waits for this reply before loading widget.html.
    // Check after giving focus back so an alert cannot paint over another app.
    hold_unfocused(&app, &window);
    let language = app.try_state::<AppState>().and_then(|s| s.settings.get("dpsMeter.language")).unwrap_or_else(|| "en".into());
    Ok(serde_json::json!({ "plugin": id, "name": name, "locked": locked, "language": language }))
}

/// A widget's message to its plugin's main window, heard there as a
/// "widget-message" event: `{ name, data }`.
#[tauri::command]
pub fn plugin_widget_post(app: tauri::AppHandle, window: tauri::WebviewWindow, data: Value) -> Result<(), String> {
    let (id, name) = widget_of(&window)?;
    if data.to_string().len() > MAX_MESSAGE {
        return Err("message too large".into());
    }
    let event = serde_json::json!({ "plugin": id, "event": "widget-message", "data": { "name": name, "data": data } });
    super::emit_plugin(&app, &id, event);
    Ok(())
}

/// Move the widget with the pointer, unless its plugin's widgets are locked.
#[tauri::command]
pub fn plugin_widget_drag(app: tauri::AppHandle, window: tauri::WebviewWindow) -> Result<(), String> {
    let (id, _) = widget_of(&window)?;
    if !is_locked(&app, &id) {
        crate::platform::window::start_drag(&window);
    }
    Ok(())
}

/// Whether a widget is on screen and unlocked (so a double-click on it can
/// open its plugin's window): a way back to the plugins with the meter hidden.
pub fn any_unlocked_widget_shown(app: &tauri::AppHandle) -> bool {
    app.webview_windows().into_iter().any(|(label, w)| {
        parse_label(&label).is_some_and(|(id, _)| !is_locked(app, &id))
            && w.is_visible().unwrap_or(false)
            && !crate::platform::window::is_hidden_in_place(&w)
    })
}

/// Close every widget of plugin `id`: its main window has gone.
pub fn close_all(app: &tauri::AppHandle, id: &str) {
    tracing::debug!("Widgets: {id}'s all closed (its window went)");
    for widget in all_widgets(app, id) {
        closed().lock().unwrap_or_else(|p| p.into_inner()).remove(widget.label());
        areas().lock().unwrap_or_else(|p| p.into_inner()).remove(widget.label());
        let _ = widget.close();
    }
}

/// A widget moved: remember where, once it has stopped moving, and tell its
/// plugin ("widget-moved", `{ name, x, y }`), which may lay its widgets out
/// around it.
pub fn note_moved(app: &tauri::AppHandle, label: &str, at: tauri::PhysicalPosition<i32>) {
    static LATEST: std::sync::OnceLock<Mutex<HashMap<String, (i32, i32)>>> = std::sync::OnceLock::new();
    let Some((id, name)) = parse_label(label) else { return };
    let key = place_key(&id, &name);
    let latest = LATEST.get_or_init(Mutex::default);
    let first = latest.lock().unwrap_or_else(|p| p.into_inner()).insert(key.clone(), (at.x, at.y)).is_none();
    if !first {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(600)).await;
        let Some((x, y)) = latest.lock().unwrap_or_else(|p| p.into_inner()).remove(&key) else { return };
        if let Some(state) = app.try_state::<AppState>() {
            state.settings.set(&key, &format!("{x},{y}"));
        }
        let event = serde_json::json!({ "plugin": id, "event": "widget-moved", "data": { "name": name, "x": x, "y": y } });
        super::emit_plugin(&app, &id, event);
    });
}

#[cfg(test)]
mod tests {
    use super::{checked_area, AreaRect, MAX_AREA_RECTS};

    fn rect(x: f64, y: f64, width: f64, height: f64) -> AreaRect {
        AreaRect { x, y, width, height }
    }

    #[test]
    fn a_widget_area_is_its_rectangles_each_checked() {
        let (rects, scale) = checked_area(&[rect(0.0, 0.0, 40.0, 40.0), rect(48.0, 0.0, 40.0, 40.0)], 1.5).unwrap();
        assert_eq!(rects, vec![(0.0, 0.0, 40.0, 40.0), (48.0, 0.0, 40.0, 40.0)]);
        assert_eq!(scale, 1.5);
        assert!(checked_area(&[], 1.0).unwrap().0.is_empty(), "nothing drawn: no rectangle");
        assert!(checked_area(&[rect(0.0, 0.0, f64::NAN, 4.0)], 1.0).is_err());
        assert!(checked_area(&[rect(-1.0, 0.0, 4.0, 4.0)], 1.0).is_err());
        assert!(checked_area(&[rect(0.0, 0.0, 5000.0, 4.0)], 1.0).is_err());
        assert!(checked_area(&[rect(0.0, 0.0, 4.0, 4.0)], 0.0).is_err());
        let many: Vec<_> = (0..=MAX_AREA_RECTS).map(|i| rect(i as f64, 0.0, 1.0, 1.0)).collect();
        assert!(checked_area(&many, 1.0).is_err());
    }

    use super::*;

    #[test]
    fn a_widget_label_reads_back_and_is_never_a_plugin_window() {
        let l = label("event-timers", "shugo-2");
        assert_eq!(parse_label(&l), Some(("event-timers".into(), "shugo-2".into())));
        assert!(!l.starts_with("plugin-"));
        assert_eq!(parse_label("plugin-event-timers"), None);
        assert_eq!(parse_label("pw_Bad_name"), None);
        assert!(!valid_name("has_underscore") && !valid_name("") && valid_name("boss-101006"));
    }

    #[test]
    fn a_new_widget_goes_beside_the_busiest_spot_on_its_screen() {
        // This PC on 2.0.72: icons around (2000, 250) on the main screen, a
        // test icon at (2680, 122) just over on the second one.
        let screens = [(2560, 0, 1920, 1080), (0, 0, 2560, 1440)];
        let places = [(2090, 250), (1956, 187), (1994, 225), (2232, 250), (2088, 370), (1971, 291),
            (2112, 122), (2001, 315), (2075, 228), (2680, 122), (17, 394), (43, 491)];
        let (x, y) = beside_cluster(&places, &screens).unwrap();
        assert!(x < 2560 && (1900..2400).contains(&x) && (100..400).contains(&y), "{x},{y}");
        // At a screen's right edge it goes to the left instead.
        assert_eq!(beside_cluster(&[(2500, 100)], &screens), Some((2398, 100)));
        assert_eq!(beside_cluster(&[], &screens), None);
    }

    #[test]
    fn a_widget_off_every_screen_comes_back_onto_the_nearest() {
        // A main screen, and a second one to its left (taskbar 48 px).
        let screens = [(0, 0, 2560, 1392), (-1920, 0, 1920, 1032)];
        // On a screen, or enough of it: left where it is.
        assert_eq!(on_screen((100, 100, 400, 370), &screens), (100, 100));
        assert_eq!(on_screen((-1900, 900, 400, 370), &screens), (-1900, 900), "partly past the bottom, still to see");
        // Where the second screen was before it moved, or went: onto the nearest.
        assert_eq!(on_screen((-3000, 800, 400, 370), &screens), (-1920, 662));
        assert_eq!(on_screen((-1900, 1100, 400, 370), &screens), (-1900, 662), "below the second screen's work area");
        assert_eq!(on_screen((5000, -900, 120, 40), &screens), (2440, 0));
        // Wider than the screen: its left edge on it.
        assert_eq!(on_screen((9000, 10, 3000, 100), &screens), (0, 10));
        // No screens known (a headless test, a Wayland quirk): left alone.
        assert_eq!(on_screen((-5000, -5000, 64, 64), &[]), (-5000, -5000));
    }

    #[test]
    fn a_new_widget_goes_right_of_the_rightmost_one() {
        assert_eq!(beside([].into_iter()), None);
        let places = [(100, 50, 80, 90), (300, 400, 80, 90), (200, 60, 80, 90)];
        assert_eq!(beside(places.into_iter()), Some((386, 400)));
    }
}

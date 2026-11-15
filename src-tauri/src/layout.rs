//! The layout editor (2.1.0): every piece the meter shows over the game (the
//! meter, the flight power pin, the plugin dock and windows, every plugin
//! widget) unlocked at once to arrange, then locked back as each was.
//!
//! Editing, each piece can be dragged whatever its own lock, and shows an
//! outline with its name (its page hears "layout-editing"). A dropped piece
//! snaps to the screen's edges and middle and to the other pieces' edges and
//! middles, or else to a grid when one is on. Done keeps the new places
//! (each window saves its own as it moves, as always); Cancel puts every
//! piece back where it was. Plugins hear a "layout" event, so a widget that
//! shows only on an alert can show a stand-in to place.
//!
//! The bar with Done and Cancel is a window of its own (`layout-bar.html`),
//! at the top of the meter's screen. Only where the meter places its own
//! windows (`places_own_windows`): elsewhere (KDE Wayland) the window manager
//! keeps them, and Cancel could not put them back.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{Emitter, Manager};

use crate::app::AppState;

/// The bar's window label.
pub const BAR_LABEL: &str = "layout-bar";
/// Snap to edges and middles: on unless turned off ("false").
const SNAP_KEY: &str = "dpsMeter.layoutSnap";
/// The grid's step in physical px, or 0 for none.
const GRID_KEY: &str = "dpsMeter.layoutGrid";
/// How near (logical px) an edge must be dropped to snap to it.
const REACH: f64 = 12.0;
const BAR_WIDTH: f64 = 760.0;
const BAR_HEIGHT: f64 = 56.0;

static EDITING: AtomicBool = AtomicBool::new(false);

/// Whether the layout is being edited: every piece unlocked.
pub fn editing() -> bool {
    EDITING.load(Ordering::Relaxed)
}

/// Where each piece was when editing began, for Cancel.
fn before() -> &'static Mutex<Vec<(String, i32, i32)>> {
    static BEFORE: std::sync::OnceLock<Mutex<Vec<(String, i32, i32)>>> = std::sync::OnceLock::new();
    BEFORE.get_or_init(Mutex::default)
}

/// Whether a window is one of the pieces laid out over the game.
fn is_piece(label: &str) -> bool {
    label == "main"
        || label == crate::fp_pin::LABEL
        || crate::plugins::is_plugin_window(label)
        || crate::plugins::widgets::parse_label(label).is_some()
}

/// The pieces on screen now.
fn pieces(app: &tauri::AppHandle) -> Vec<tauri::WebviewWindow> {
    app.webview_windows()
        .into_iter()
        .filter(|(label, w)| {
            is_piece(label)
                && !crate::plugins::widgets::is_closed(label)
                && w.is_visible().unwrap_or(false)
                && !w.is_minimized().unwrap_or(false)
                && !crate::platform::window::is_hidden_in_place(w)
        })
        .map(|(_, w)| w)
        .collect()
}

fn rect_of(window: &tauri::WebviewWindow) -> Option<(i32, i32, i32, i32)> {
    let at = window.outer_position().ok()?;
    let size = window.outer_size().ok()?;
    Some((at.x, at.y, size.width as i32, size.height as i32))
}

/// Every piece's lock as editing says: all unlocked while editing, each as
/// its own lock says after.
fn apply_locks(app: &tauri::AppHandle) {
    crate::plugins::widgets::reapply_locks(app);
    crate::fp_pin::apply_lock(app);
}

/// Tell every page, and every plugin, that editing began or ended.
fn announce(app: &tauri::AppHandle, on: bool) {
    let _ = app.emit("layout-editing", on);
    crate::plugins::broadcast(app, "layout", serde_json::json!({ "editing": on }));
    // A widget hears its own plugin's events only.
    for (label, window) in app.webview_windows() {
        if let Some((id, _)) = crate::plugins::widgets::parse_label(&label) {
            let event = serde_json::json!({ "plugin": id, "event": "layout", "data": { "editing": on } });
            let _ = window.emit("plugin-event", event);
        }
    }
}

/// Only the meter's own pages start, stop or set up editing: not a plugin's
/// window, nor a widget.
fn from_meter(window: &tauri::WebviewWindow) -> Result<(), String> {
    match window.label() {
        "main" | "settings" | BAR_LABEL | crate::plugins::dock::LABEL => Ok(()),
        _ => Err("not from this window".into()),
    }
}

/// Start editing the layout.
#[tauri::command]
pub async fn layout_edit_start(app: tauri::AppHandle, window: tauri::WebviewWindow) -> Result<(), String> {
    from_meter(&window)?;
    start(app).await
}

/// Its keybind: editing on, or Done when it is.
pub fn toggle(app: &tauri::AppHandle) {
    if editing() {
        stop(app, true);
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = start(app).await {
            tracing::info!("Layout: not from the keybind: {e}");
        }
    });
}

async fn start(app: tauri::AppHandle) -> Result<(), String> {
    if !crate::platform::window::places_own_windows() {
        return Err("this desktop places the meter's windows itself".into());
    }
    if EDITING.swap(true, Ordering::Relaxed) {
        if let Some(bar) = app.get_webview_window(BAR_LABEL) {
            crate::platform::window::show_on_top_without_focus(&bar);
        }
        return Ok(());
    }
    // Widgets that show only on an alert, opened to place.
    crate::plugins::widgets::show_stand_ins(&app);
    let places: Vec<_> = pieces(&app)
        .iter()
        .filter_map(|w| rect_of(w).map(|(x, y, _, _)| (w.label().to_string(), x, y)))
        .collect();
    tracing::info!("Layout: editing {} pieces", places.len());
    *before().lock().unwrap_or_else(|p| p.into_inner()) = places;
    apply_locks(&app);
    announce(&app, true);
    if let Err(e) = open_bar(&app) {
        tracing::warn!("Layout: no bar: {e}");
        stop(&app, true);
        return Err(e);
    }
    Ok(())
}

/// Stop editing: keep the new places (`keep`), or put every piece back.
#[tauri::command]
pub fn layout_edit_stop(app: tauri::AppHandle, window: tauri::WebviewWindow, keep: bool) -> Result<(), String> {
    from_meter(&window)?;
    tracing::debug!("Layout: {} from {}", if keep { "Done" } else { "Cancel" }, window.label());
    stop(&app, keep);
    Ok(())
}

pub fn stop(app: &tauri::AppHandle, keep: bool) {
    if !EDITING.swap(false, Ordering::Relaxed) {
        return;
    }
    let places = std::mem::take(&mut *before().lock().unwrap_or_else(|p| p.into_inner()));
    if !keep {
        for (label, x, y) in places {
            if let Some(window) = app.get_webview_window(&label) {
                let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
            }
        }
    }
    tracing::info!("Layout: editing done ({})", if keep { "kept" } else { "put back" });
    crate::plugins::widgets::hide_stand_ins(app);
    apply_locks(app);
    announce(app, false);
    if let Some(bar) = app.get_webview_window(BAR_LABEL) {
        let _ = bar.close();
    }
}

/// The snap and grid settings, for the bar.
#[tauri::command]
pub fn layout_options(app: tauri::AppHandle) -> serde_json::Value {
    let (snap, grid) = options(&app);
    let language = setting(&app, "dpsMeter.language").unwrap_or_else(|| "en".into());
    serde_json::json!({ "snap": snap, "grid": grid, "language": language })
}

/// Set the snap and grid settings from the bar.
#[tauri::command]
pub fn layout_set_options(app: tauri::AppHandle, window: tauri::WebviewWindow, snap: bool, grid: u32) -> Result<(), String> {
    from_meter(&window)?;
    let state = app.try_state::<AppState>().ok_or("not ready")?;
    state.settings.set(SNAP_KEY, if snap { "true" } else { "false" });
    state.settings.set(GRID_KEY, &grid.min(200).to_string());
    Ok(())
}

/// Drag the bar itself.
#[tauri::command]
pub fn layout_bar_drag(window: tauri::WebviewWindow) {
    if window.label() == BAR_LABEL {
        crate::platform::window::start_drag(&window);
    }
}

/// Drag a piece while editing (one whose own drag a lock would refuse).
pub fn may_drag() -> bool {
    editing()
}

fn setting(app: &tauri::AppHandle, key: &str) -> Option<String> {
    app.try_state::<AppState>()?.settings.get(key)
}

fn options(app: &tauri::AppHandle) -> (bool, u32) {
    let snap = setting(app, SNAP_KEY).as_deref() != Some("false");
    let grid = setting(app, GRID_KEY).and_then(|v| v.parse().ok()).unwrap_or(0u32).min(200);
    (snap, grid)
}

fn open_bar(app: &tauri::AppHandle) -> Result<(), String> {
    if let Some(bar) = app.get_webview_window(BAR_LABEL) {
        crate::platform::window::show_on_top_without_focus(&bar);
        return Ok(());
    }
    let bar = crate::platform::window::share_process(
        tauri::WebviewWindowBuilder::new(app, BAR_LABEL, tauri::WebviewUrl::App("layout-bar.html".into())),
        crate::platform::ProcessGroup::Meter,
    )
    .title("A2Tools Layout")
    .inner_size(BAR_WIDTH, BAR_HEIGHT)
    .decorations(false)
    .transparent(true)
    .shadow(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(cfg!(target_os = "linux"))
    // Not the keyboard: it opened in front and took it, and an Enter typed
    // elsewhere (or in the game's chat) pressed Done. Clicked, it has it.
    .focused(false)
    .visible(false)
    .build()
    .map_err(|e| e.to_string())?;
    crate::platform::window::joined(&bar, crate::platform::ProcessGroup::Meter);
    crate::platform::window::set_size(&bar, tauri::LogicalSize::new(BAR_WIDTH, BAR_HEIGHT).into());
    // Across the middle of the meter's screen, near its top, clear of every
    // piece (Ping's widget sits at the top middle).
    let screen = app
        .get_webview_window("main")
        .and_then(|m| m.current_monitor().ok().flatten())
        .or_else(|| bar.primary_monitor().ok().flatten());
    if let (Some(screen), Ok(size)) = (screen, bar.outer_size()) {
        let area = screen.work_area();
        let pieces: Vec<_> = pieces(app).iter().filter_map(rect_of).collect();
        let (x, y) = bar_place(
            (area.position.x, area.position.y, area.size.width as i32, area.size.height as i32),
            (size.width as i32, size.height as i32),
            (24.0 * screen.scale_factor()) as i32,
            &pieces,
        );
        let _ = bar.set_position(tauri::PhysicalPosition::new(x, y));
    }
    crate::platform::window::show_on_top_without_focus(&bar);
    Ok(())
}

/// Where the bar goes on a screen's work `area`: centred across it, at the
/// first height from `gap` below its top (down to half the screen) where it
/// covers no piece; at the top when none is clear.
fn bar_place(area: (i32, i32, i32, i32), size: (i32, i32), gap: i32, pieces: &[(i32, i32, i32, i32)]) -> (i32, i32) {
    let (ax, ay, aw, ah) = area;
    let (w, h) = size;
    let x = ax + (aw - w) / 2;
    let covers = |y: i32| pieces.iter().any(|&(px, py, pw, ph)| x < px + pw && px < x + w && y < py + ph && py < y + h);
    let step = (h / 2).max(1);
    let mut y = ay + gap;
    while y + h <= ay + ah / 2 {
        if !covers(y) {
            return (x, y);
        }
        y += step;
    }
    (x, ay + gap)
}

/// A piece moved while editing: once it is let go, it snaps.
pub fn note_moved(app: &tauri::AppHandle, label: &str) {
    if !editing() || !is_piece(label) {
        return;
    }
    // One watch per piece: a drag sends many moves.
    static WATCHED: std::sync::OnceLock<Mutex<HashSet<String>>> = std::sync::OnceLock::new();
    let watched = WATCHED.get_or_init(Mutex::default);
    if !watched.lock().unwrap_or_else(|p| p.into_inner()).insert(label.to_string()) {
        return;
    }
    let app = app.clone();
    let label = label.to_string();
    std::thread::spawn(move || {
        let Some(window) = app.get_webview_window(&label) else {
            watched.lock().unwrap_or_else(|p| p.into_inner()).remove(&label);
            return;
        };
        let started = Instant::now();
        let mut last = rect_of(&window);
        let mut still = 0;
        // Until the button is up (or, where that can't be known, the piece
        // has kept still a moment), for at most two minutes.
        while started.elapsed() < Duration::from_secs(120) {
            std::thread::sleep(Duration::from_millis(50));
            match crate::platform::window::primary_button_down() {
                Some(true) => continue,
                Some(false) => break,
                None => {
                    let now = rect_of(&window);
                    still = if now == last { still + 1 } else { 0 };
                    last = now;
                    if still >= 6 {
                        break;
                    }
                }
            }
        }
        watched.lock().unwrap_or_else(|p| p.into_inner()).remove(&label);
        if editing() {
            snap_window(&app, &window);
        }
    });
}

fn snap_window(app: &tauri::AppHandle, window: &tauri::WebviewWindow) {
    let (snap_on, grid) = options(app);
    if !snap_on && grid == 0 {
        return;
    }
    let Some(rect) = rect_of(window) else { return };
    let others: Vec<_> = pieces(app)
        .iter()
        .filter(|w| w.label() != window.label() && w.label() != BAR_LABEL)
        .filter_map(rect_of)
        .collect();
    let screens: Vec<_> = app
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let a = m.work_area();
            (a.position.x, a.position.y, a.size.width, a.size.height)
        })
        .collect();
    let scale = window.scale_factor().unwrap_or(1.0);
    let reach = if snap_on { (REACH * scale).round() as i32 } else { 0 };
    let (x, y) = snap(rect, &others, &screens, reach, grid as i32);
    if (x, y) != (rect.0, rect.1) {
        tracing::debug!("Layout: {} snapped {},{} -> {x},{y}", window.label(), rect.0, rect.1);
        let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
    }
}

/// Where a piece dropped at `rect` (x, y, width, height) settles: each axis
/// to the nearest edge or middle within `reach` (the screen's work area, or
/// another piece's: lined up with it or beside it), or else to a `grid` step
/// from the screen's corner (0: no grid).
pub fn snap(rect: (i32, i32, i32, i32), others: &[(i32, i32, i32, i32)], screens: &[(i32, i32, u32, u32)], reach: i32, grid: i32) -> (i32, i32) {
    let (x, y, w, h) = rect;
    let (cx, cy) = (x + w / 2, y + h / 2);
    // The screen its middle is on, or the nearest.
    let screen = screens
        .iter()
        .min_by_key(|&&(sx, sy, sw, sh)| {
            let dx = (sx - cx).max(cx - (sx + sw as i32)).max(0) as i64;
            let dy = (sy - cy).max(cy - (sy + sh as i32)).max(0) as i64;
            dx * dx + dy * dy
        })
        .map(|&(sx, sy, sw, sh)| (sx, sy, sw as i32, sh as i32));
    // Each axis: the places this piece's start could go, nearest wins.
    let axis = |at: i32, size: i32, screen: Option<(i32, i32)>, others: Vec<(i32, i32)>| -> i32 {
        let mut targets = Vec::new();
        if let Some((start, len)) = screen {
            targets.extend([start, start + len - size, start + (len - size) / 2]);
        }
        for (o, olen) in others {
            // Lined up: starts, ends, middles. Beside: touching either side.
            targets.extend([o, o + olen - size, o + (olen - size) / 2, o + olen, o - size]);
        }
        let best = targets.into_iter().min_by_key(|t| (t - at).abs()).filter(|t| (t - at).abs() <= reach);
        match (best, screen) {
            (Some(t), _) => t,
            (None, Some((start, _))) if grid > 0 => start + ((at - start) as f64 / grid as f64).round() as i32 * grid,
            _ => at,
        }
    };
    let nx = axis(x, w, screen.map(|s| (s.0, s.2)), others.iter().map(|o| (o.0, o.2)).collect());
    let ny = axis(y, h, screen.map(|s| (s.1, s.3)), others.iter().map(|o| (o.1, o.3)).collect());
    (nx, ny)
}

#[cfg(test)]
mod tests {
    use super::snap;

    const SCREEN: [(i32, i32, u32, u32); 1] = [(0, 0, 2560, 1392)];

    #[test]
    fn a_piece_dropped_near_an_edge_or_middle_snaps_to_it() {
        // Near the top left corner.
        assert_eq!(snap((8, 10, 100, 40), &[], &SCREEN, 12, 0), (0, 0));
        // Near the right edge and the bottom of the work area.
        assert_eq!(snap((2455, 1345, 100, 40), &[], &SCREEN, 12, 0), (2460, 1352));
        // Near the middle across.
        assert_eq!(snap((1225, 500, 100, 40), &[], &SCREEN, 12, 0), (1230, 500));
        // Too far from anything: where it was dropped.
        assert_eq!(snap((300, 500, 100, 40), &[], &SCREEN, 12, 0), (300, 500));
    }

    #[test]
    fn a_piece_snaps_beside_or_in_line_with_another() {
        let other = [(1000, 600, 200, 50)];
        // Dropped just right of it: touching it, and its top lined up.
        assert_eq!(snap((1206, 604, 80, 30), &other, &SCREEN, 12, 0), (1200, 600));
        // Dropped just below it, left edges nearly lined up.
        assert_eq!(snap((995, 655, 80, 30), &other, &SCREEN, 12, 0), (1000, 650));
    }

    #[test]
    fn the_bar_goes_where_it_covers_no_piece() {
        use super::bar_place;
        let area = (0, 0, 2560, 1392);
        assert_eq!(bar_place(area, (760, 56), 24, &[]), (900, 24));
        // Ping's widget at the top middle: the bar just below it.
        let ping = [(1150, 0, 200, 60)];
        let (x, y) = bar_place(area, (760, 56), 24, &ping);
        assert_eq!(x, 900);
        assert!((60..=120).contains(&y), "{y}");
        // Nowhere clear in the top half: the top.
        assert_eq!(bar_place(area, (760, 56), 24, &[(0, 0, 2560, 800)]), (900, 24));
    }

    #[test]
    fn with_no_edge_near_a_grid_takes_it() {
        assert_eq!(snap((303, 497, 100, 40), &[], &SCREEN, 12, 16), (304, 496));
        // Snapping off (reach 0): the grid alone.
        assert_eq!(snap((8, 10, 100, 40), &[], &SCREEN, 0, 16), (16, 16));
        assert_eq!(snap((8, 10, 100, 40), &[], &SCREEN, 0, 0), (8, 10));
        // A screen to the left: the grid counts from its corner.
        let screens = [(-1920, 0, 1920, 1032)];
        assert_eq!(snap((-1500, 300, 100, 40), &[], &screens, 0, 25), (-1495, 300));
    }
}

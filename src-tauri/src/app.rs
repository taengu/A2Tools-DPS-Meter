//! The desktop application: Tauri commands, capture wiring, window management.
//!
//! Split out of `lib.rs` so the crate can be built *without* it. The parser has
//! to compile to `wasm32-unknown-unknown` — that is how the server re-derives an
//! uploaded encounter with the same code the client ran — and nothing in this
//! file can: `tauri`, `libloading`, `reqwest` and the Windows API all fail on
//! that target. Everything here is behind the `desktop` feature; the parser core
//! is not, and `cargo check --no-default-features --target wasm32-unknown-unknown`
//! is what keeps it that way.

// Brought into scope as modules because this file was split out of lib.rs,
// where these were siblings at the crate root.
use crate::{entity, i18n, logging, platform, share};

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tauri::{Emitter, Manager};
use tokio::sync::mpsc;

use crate::capture::captured_payload::CapturedPayload;
use crate::capture::combat_port_detector::CombatPortDetector;
use crate::capture::pcap_capturer::PcapCapturer;
use crate::combat::capture_dispatcher::CaptureDispatcher;
use crate::combat::data_storage::DataStorage;
use crate::combat::dps_calculator::DpsCalculator;
use crate::combat::ping_tracker::PingTracker;
use crate::config::settings::Settings;
use crate::entity::dps_data::DpsData;
use crate::entity::fight_record::{FightRecord, FightSummary};
use crate::entity::details_context::{DetailsContext, TargetDetailsResponse};
use crate::history::fight_history::FightHistoryManager;
use crate::i18n::lookup::{NpcLookup, SkillLookup};

/// Monitor the Details window was last placed on. Recorded here rather than
/// passed back from JS so the confirmation cannot disagree with the placement.
static DETAILS_MONITOR: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(usize::MAX);

/// Requests waiting for the window that will serve them, keyed by window label.
/// A webview that has just been built has no event listener attached yet, so a
/// push would be dropped; each window pulls its own entry on startup instead
/// (see `take_pending_details_request`). Keyed rather than a single slot
/// because several fight windows can be opening at once, and one clobbering
/// another's request would leave a blank window.
static PENDING_DETAILS_REQUEST: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, serde_json::Value>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Stamped onto every request so the Details window can ignore one it has
/// already applied — the pull and the push can both carry the same request.
static DETAILS_REQUEST_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Shared application state.
pub struct AppState {
    pub data_storage: Arc<DataStorage>,
    pub dps_calculator: Mutex<DpsCalculator>,
    pub ping_tracker: Arc<PingTracker>,
    pub port_detector: Arc<CombatPortDetector>,
    pub fight_history: FightHistoryManager,
    /// Order fight snapshots and their writes together. Settings never take
    /// this lock; the calculator guard is released before disk work.
    pub fight_save: Mutex<()>,
    /// Fights taken before a reset, with their names, until HISTORY saves
    /// them (see `save_fights_before_reset`). Whoever saves next under
    /// `fight_save` takes them: quitting does not leave them behind.
    pub queued_fight_saves: Mutex<Vec<(Vec<FightRecord>, share::SliceNames)>>,
    pub settings: Settings,
    pub skill_lookup: Arc<SkillLookup>,
    pub npc_lookup: Arc<NpcLookup>,
    pub app_data_dir: std::path::PathBuf,
    pub i18n_data_dir: Option<std::path::PathBuf>,
    /// One client, reused. The update check and the MSI download each used a
    /// one-shot `reqwest::get`, which builds a fresh client and TLS stack per
    /// call; anything periodic wants a pool rather than a handshake every time.
    #[cfg(feature = "online")]
    pub http: reqwest::Client,
    /// The header's suspend button: while set, the capture dispatcher drops
    /// every packet. Shared with it (`CaptureDispatcher::use_suspend_flag`).
    pub capture_suspended: Arc<std::sync::atomic::AtomicBool>,
    /// The overlay's click-through lock. See `apply_overlay_lock`.
    pub overlay_lock: Arc<OverlayLock>,
    /// What the last account check found: `None` until one has run, then
    /// `Some(None)` signed out or `Some(Some(_))` signed in. Settings shows it
    /// at once instead of "checking" for as long as the server takes.
    #[cfg(feature = "online")]
    pub account_seen: Mutex<Option<Option<crate::account::AccountSummary>>>,
    /// The LAN stream overlay for OBS on another PC; off unless enabled.
    #[cfg(feature = "online")]
    pub stream_overlay: crate::stream_overlay::Manager,
    /// The overlay's height when it was last anchored (Settings → Meter stays
    /// fixed at), which the next anchored resize goes from: not its height
    /// now, which a tooltip, the details panel or the promo can have grown
    /// for a moment. Starts as the saved `window.h`.
    pub anchored_height: std::sync::atomic::AtomicI32,
}

/// The overlay's click-through lock: while locked, clicks go through the meter
/// to the game, and it cannot be dragged. Only its own lock button stays
/// clickable, so the same button unlocks it; a hotkey does too.
///
/// On Linux the window takes the mouse over the button alone (an input
/// region, `platform::window::click_through_except`). Elsewhere a window
/// either takes the mouse or ignores it, all of it, so the button is kept
/// clickable by watching the pointer: while it is over the button the window
/// takes the mouse again. That needs the pointer's position outside the
/// window, which Wayland does not give (`platform::window::cursor_position`),
/// so the lock is offered only where it is available.
#[derive(Default)]
pub struct OverlayLock {
    locked: std::sync::atomic::AtomicBool,
    /// The lock button in the main window's page: CSS-pixel x, y, width,
    /// height, and the page's scale. Sent by the page when it lays out.
    button: Mutex<Option<(f64, f64, f64, f64, f64)>>,
    /// Whether the pointer watch is running.
    watching: std::sync::atomic::AtomicBool,
    /// A card is open in the page (a one-time popup, the update prompt): the
    /// window takes the mouse while it is, locked or not, or nothing on the
    /// card could be clicked. It covers the lock button too.
    card_open: std::sync::atomic::AtomicBool,
    /// What the window was last told (`true` = ignore the mouse). Every change
    /// goes through `sync_click_through` under this lock, so a late pointer
    /// check can never leave an unlocked window click-through.
    applied: Mutex<Option<bool>>,
}

/// The setting that hides the DPS meter, for players who use only plugins:
/// capture, uploads and plugins go on; the overlay stays hidden until shown
/// again (Settings → Plugins, the toggle hotkey, or a plugin window's bar).
const SHOW_METER_KEY: &str = "dpsMeter.showMeter";

fn meter_wanted(app: &tauri::AppHandle) -> bool {
    app.try_state::<AppState>()
        .and_then(|s| s.settings.get(SHOW_METER_KEY))
        .as_deref()
        != Some("false")
}

/// Show or hide the DPS meter and remember it. Hidden in place (Linux keeps
/// it mapped, so it comes back where it was). Every window hears
/// "meter-shown", so Settings and plugin bars follow.
fn set_meter_wanted(app: &tauri::AppHandle, shown: bool) {
    if let Some(state) = app.try_state::<AppState>() {
        state.settings.set(SHOW_METER_KEY, if shown { "true" } else { "false" });
    }
    if let Some(window) = app.get_webview_window("main") {
        if shown {
            show_meter_again(app, &window);
        } else {
            platform::window::hide_in_place(&window);
        }
    }
    let _ = app.emit("meter-shown", shown);
}

/// Whether something would be left to open the meter or a plugin with, with
/// `except` (a window about to go) gone: the meter, another plugin window,
/// or an unlocked widget (double-clicked, it opens its plugin's window).
pub(crate) fn way_back_without(app: &tauri::AppHandle, except: &str) -> bool {
    meter_wanted(app)
        || app.webview_windows().into_iter().any(|(label, w)| {
            label != except && crate::plugins::is_plugin_window(&label)
                && w.is_visible().unwrap_or(false) && !platform::window::is_hidden_in_place(&w)
        })
        || crate::plugins::widgets::any_unlocked_widget_shown(app)
}

/// The meter hidden and nothing left on screen to open anything with (a
/// tester hid both, 2026-10-17): the meter comes back. `except` is a window
/// about to go.
pub(crate) fn keep_something_on_screen(app: &tauri::AppHandle, except: &str) {
    if !way_back_without(app, except) {
        set_meter_wanted(app, true);
    }
}

#[tauri::command]
fn set_meter_shown(app: tauri::AppHandle, shown: bool) {
    set_meter_wanted(&app, shown);
}

#[tauri::command]
fn meter_shown(app: tauri::AppHandle) -> bool {
    meter_wanted(&app)
}

/// Show the meter that auto-hide hid: in place where it was hidden so
/// (Linux), else as usual. The page re-measures, as its content may have
/// changed meanwhile.
fn show_meter_again(app: &tauri::AppHandle, window: &tauri::WebviewWindow) {
    if !platform::window::reveal_in_place(window) {
        platform::window::show_on_top_without_focus(window);
    }
    // Shown again, the window takes the mouse (revealed, or a new surface
    // where it was unmapped, KDE): the lock decides that anew.
    if let Some(state) = app.try_state::<AppState>() {
        let lock = state.overlay_lock.clone();
        *lock.applied.lock() = None;
        sync_click_through(app, &lock, false);
    }
    let _ = window.emit("force-resize", ());
}

/// The windows that hide when AION2 isn't in front: with the meter's "Hide
/// when AION2 isn't focused" (`meter`), Settings, Battle History and Details
/// (only the meter hid, and the rest stayed over the browser: Hirai,
/// 2026-10-19); and, whether or not that is on, the windows and icons of
/// plugins set to (Settings → Plugins). A plugin's own option did nothing
/// while the meter's was off (2026-10-27).
fn hides_with_meter(app: &tauri::AppHandle, label: &str, meter: bool) -> bool {
    (meter && (TOOL_WINDOWS.contains(&label) || label.starts_with("details-"))) || crate::plugins::hides_unfocused(app, label)
}

/// Hide those of `hides_with_meter` that are up, noting each.
fn hide_with_meter(app: &tauri::AppHandle, meter: bool, hidden: &mut std::collections::HashSet<String>) {
    for (label, window) in app.webview_windows() {
        if !hides_with_meter(app, &label, meter) || hidden.contains(&label) {
            continue;
        }
        if window.is_visible().unwrap_or(false) && !platform::window::is_hidden_in_place(&window) {
            platform::window::hide_in_place(&window);
            hidden.insert(label);
        }
    }
}

/// Show again those `hide_with_meter` hid.
fn show_with_meter(app: &tauri::AppHandle, hidden: &mut std::collections::HashSet<String>) {
    for label in hidden.drain() {
        let Some(window) = app.get_webview_window(&label) else { continue };
        // A widget its plugin closed meanwhile stays hidden.
        if crate::plugins::widgets::is_closed(&label) {
            continue;
        }
        if !platform::window::reveal_in_place(&window) {
            platform::window::show_on_top_without_focus(&window);
        }
        crate::plugins::widgets::relock(app, &window);
    }
}

/// The label of the meter window titled `title`.
fn window_titled(app: &tauri::AppHandle, title: &str) -> Option<String> {
    app.webview_windows()
        .into_iter()
        .find(|(_, window)| window.title().ok().as_deref() == Some(title))
        .map(|(label, _)| label)
        .or_else(|| titles().lock().unwrap_or_else(|p| p.into_inner()).get(title).cloned())
}

/// Each window's title and label, noted before the window is built: KWin
/// asks where a window goes the moment it appears, which can be before the
/// app has the new window in its list, and a window it could not name
/// opened in the middle of the screen (widgets moved before, a tester on
/// KDE Wayland, 2026-10-17).
fn titles() -> &'static std::sync::Mutex<HashMap<String, String>> {
    static TITLES: std::sync::OnceLock<std::sync::Mutex<HashMap<String, String>>> = std::sync::OnceLock::new();
    TITLES.get_or_init(Default::default)
}

pub(crate) fn note_window_title(title: &str, label: &str) {
    titles().lock().unwrap_or_else(|p| p.into_inner()).insert(title.to_string(), label.to_string());
}

/// Where the window manager says a window was left, by its label (#76).
fn save_window_place(app: &tauri::AppHandle, title: &str, (x, y, w, h): (i32, i32, i32, i32)) {
    let (Some(label), Some(state)) = (window_titled(app, title), app.try_state::<AppState>()) else { return };
    state.settings.set(&format!("window.{label}.placed"), &format!("{x},{y},{w},{h}"));
}

/// Where a window belongs, for the window manager to put it (#76). The
/// overlay sizes itself to its content, so it gets its place, and its height
/// there only for the edge it keeps (Settings → Meter stays fixed at): with
/// Bottom, its bottom goes back where it was.
fn load_window_place(app: &tauri::AppHandle, title: &str) -> Option<(i32, i32, i32, i32)> {
    let label = window_titled(app, title)?;
    // On X11 the meter places its windows itself; KWin puts back only a
    // widget, where the meter saved it, as KWin may not let it go there
    // (platform::linux::focus, X11_IN_WAYLAND).
    if platform::window::places_own_windows() {
        let (id, name) = crate::plugins::widgets::parse_label(&label)?;
        return crate::plugins::widgets::saved_place(app, &id, &name).map(|(x, y)| (x, y, 0, 0));
    }
    let state = app.try_state::<AppState>()?;
    let placed = |label: &str| {
        let value = state.settings.get(&format!("window.{label}.placed"))?;
        let n: Vec<i32> = value.split(',').filter_map(|v| v.trim().parse().ok()).collect();
        let [x, y, w, h] = n[..] else { return None };
        Some((x, y, w, h))
    };
    match placed(&label) {
        Some((x, y, w, h)) => Some(if label == "main" { (x, y, 0, h) } else { (x, y, w, h) }),
        // A plugin's widget never moved: beside its others, not mid-screen.
        None => crate::plugins::widgets::place_beside_others(app, &label, placed).map(|(x, y)| (x, y, 0, 0)),
    }
}

/// Auto-hide at once, when the desktop says the active window changed (KWin,
/// see platform::linux::focus): the 500 ms check and its wait before hiding
/// left the meter up for a moment after an alt-tab (2026-10-17). The check
/// still runs, for every other desktop and as a fallback.
fn auto_hide_now(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<AppState>() else { return };
    if state.settings.get("dpsMeter.autoHideMeter").unwrap_or_default() != "true" || !meter_wanted(app) {
        return;
    }
    // Hidden by a rule of its own (AION2 closed, solo, out of combat): the
    // game coming to the front doesn't bring it back.
    if HIDDEN_BY_RULE.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let Some(window) = app.get_webview_window("main") else { return };
    let front = platform::window_detector::is_aion2_foreground() || window.is_focused().unwrap_or(false);
    let hidden = !window.is_visible().unwrap_or(true)
        || window.is_minimized().unwrap_or(false)
        || platform::window::is_hidden_in_place(&window);
    if front && hidden {
        show_meter_again(app, &window);
    } else if !front && !hidden {
        platform::window::minimize_off_top(&window);
    }
}

/// The meter hidden by the 500 ms check's own rules: AION2 closed, solo, or
/// out of combat. Not the player hiding it (`meter_wanted`): it comes back
/// by itself when the rule no longer holds.
static HIDDEN_BY_RULE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// "Hide out of combat" (#106): seconds without combat before the meter
/// hides; 0 is off.
const HIDE_OUT_OF_COMBAT_KEY: &str = "dpsMeter.hideOutOfCombat";

fn hide_out_of_combat_secs(settings: &Settings) -> u64 {
    settings.get(HIDE_OUT_OF_COMBAT_KEY).and_then(|v| v.trim().parse().ok()).unwrap_or(0)
}

/// Whether `secs` seconds without combat have passed by `now_ms`, timed from
/// `last_combat_ms` (clock ms): the later of the last damage the meter
/// counted and when the wait began. 0 seconds is off.
fn out_of_combat(last_combat_ms: i64, now_ms: i64, secs: u64) -> bool {
    secs > 0 && now_ms.saturating_sub(last_combat_ms) >= i64::try_from(secs).unwrap_or(i64::MAX).saturating_mul(1000)
}

/// What keeps the meter up out of combat: Settings, History or a fight's
/// Details open, or (`pointer` true) the mouse over the meter. The pointer
/// isn't asked while the meter is hidden: on Linux it stays mapped where it
/// was, and passing over the empty spot would bring it back.
fn keeps_meter_up(app: &tauri::AppHandle, main: &tauri::WebviewWindow, pointer: bool) -> bool {
    let tool_open = app.webview_windows().into_iter().any(|(label, w)| {
        (TOOL_WINDOWS.contains(&label.as_str()) || label.starts_with("details-"))
            && w.is_visible().unwrap_or(false)
            && !platform::window::is_hidden_in_place(&w)
    });
    tool_open || (pointer && pointer_over_window(main))
}

fn pointer_over_window(window: &tauri::WebviewWindow) -> bool {
    let Some((px, py)) = platform::window::cursor_position() else { return false };
    let (Ok(at), Ok(size)) = (window.inner_position(), window.inner_size()) else { return false };
    px >= at.x && px < at.x + size.width as i32 && py >= at.y && py < at.y + size.height as i32
}

/// Tell the main window whether to ignore the mouse: when locked, unless the
/// pointer is over the lock button or a card is open.
/// A screenshot tool (KDE's Spectacle) is the active window: every meter
/// window lets clicks through, so its area selection can be dragged across
/// them, which still show in the shot (KDE draws the selection under them).
static SCREENSHOT_TOOL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn screenshot_tool_in_front() -> bool {
    SCREENSHOT_TOOL.load(std::sync::atomic::Ordering::Relaxed)
}

/// A screenshot tool came to the front (`true`) or left it.
fn screenshot_tool(app: &tauri::AppHandle, in_front: bool) {
    SCREENSHOT_TOOL.store(in_front, std::sync::atomic::Ordering::Relaxed);
    // The overlay, by its lock (which reads the flag).
    if let Some(state) = app.try_state::<AppState>() {
        let lock = state.overlay_lock.clone();
        *lock.applied.lock() = None;
        sync_click_through(app, &lock, false);
    }
    // Plugins' icons, by their lock and area.
    crate::plugins::widgets::reapply_locks(app);
    // Settings, History, Details, and plugins' windows: all or nothing.
    for (label, window) in app.webview_windows() {
        let tool = TOOL_WINDOWS.contains(&label.as_str()) || label.starts_with("details-") || crate::plugins::is_plugin_window(&label);
        if tool && !platform::window::click_through_except(&window, in_front, None) && !cfg!(target_os = "linux") {
            let _ = window.set_ignore_cursor_events(in_front);
        }
    }
}

fn sync_click_through(app: &tauri::AppHandle, lock: &OverlayLock, over_button: bool) {
    use std::sync::atomic::Ordering;
    let mut applied = lock.applied.lock();
    let ignore = screenshot_tool_in_front()
        || click_through(lock.locked.load(Ordering::SeqCst), over_button, lock.card_open.load(Ordering::SeqCst));
    // The region follows the button, so it is set again even unchanged.
    if let Some(main) = app.get_webview_window("main") {
        if platform::window::click_through_except(&main, ignore, *lock.button.lock()) {
            *applied = Some(ignore);
            return;
        }
    }
    // Not applied on Linux means the window has no GDK window yet, and tao's
    // own call would unwrap it and abort. Left for the next sync.
    if cfg!(target_os = "linux") {
        return;
    }
    if *applied == Some(ignore) {
        return;
    }
    if let Some(main) = app.get_webview_window("main") {
        if main.set_ignore_cursor_events(ignore).is_ok() {
            *applied = Some(ignore);
        }
    }
}

fn click_through(locked: bool, over_button: bool, card_open: bool) -> bool {
    locked && !over_button && !card_open
}

fn pointer_over_lock_button(app: &tauri::AppHandle, lock: &OverlayLock) -> bool {
    let Some((px, py)) = platform::window::cursor_position() else { return false };
    let Some((x, y, w, h, scale)) = *lock.button.lock() else { return false };
    let Some(origin) = app.get_webview_window("main").and_then(|m| m.inner_position().ok()) else {
        return false;
    };
    let (left, top) = (origin.x as f64 + x * scale, origin.y as f64 + y * scale);
    let (px, py) = (px as f64, py as f64);
    px >= left && px < left + w * scale && py >= top && py < top + h * scale
}

/// Lock or unlock the overlay. Locking starts the pointer watch, which ends
/// by itself once unlocked.
fn apply_overlay_lock(app: &tauri::AppHandle, locked: bool) {
    use std::sync::atomic::Ordering;
    let Some(state) = app.try_state::<AppState>() else { return };
    let lock = state.overlay_lock.clone();
    lock.locked.store(locked, Ordering::SeqCst);
    sync_click_through(app, &lock, false);
    tracing::info!("Overlay {}", if locked { "locked (click-through)" } else { "unlocked" });
    // No pointer watch where the window keeps the button by its input region.
    if cfg!(target_os = "linux") {
        return;
    }
    if locked && !lock.watching.swap(true, Ordering::SeqCst) {
        let app = app.clone();
        std::thread::spawn(move || {
            while lock.locked.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(50));
                let over = pointer_over_lock_button(&app, &lock);
                sync_click_through(&app, &lock, over);
            }
            sync_click_through(&app, &lock, false);
            lock.watching.store(false, Ordering::SeqCst);
        });
    }
}

// ===== TAURI COMMANDS =====

#[tauri::command]
fn get_app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

// Details read shared immutable lookups and storage snapshots, without taking
// the mutable DPS calculator's mutex or copying its caches/target-selection state.
fn details_reader(state: &AppState) -> DpsCalculator {
    DpsCalculator::new(state.data_storage.clone(), state.skill_lookup.clone(),
        state.npc_lookup.clone(), state.ping_tracker.clone())
}

#[tauri::command]
async fn get_dps_snapshot(app: tauri::AppHandle) -> Result<DpsData, String> {
    crate::blocking::CALCULATIONS.run(move || {
        app.state::<AppState>().dps_calculator.lock().get_dps()
    }).await
}

#[tauri::command]
async fn get_skill_details(app: tauri::AppHandle, target_id: i32, actor_ids: Option<Vec<i32>>, summary_only: Option<bool>) -> Result<TargetDetailsResponse, String> {
    crate::blocking::CALCULATIONS.run(move || {
        let calculator = details_reader(&app.state::<AppState>());
        if summary_only.unwrap_or(false) {
            calculator.get_hover_details(target_id, actor_ids.as_deref())
        } else {
            calculator.get_target_details(target_id, actor_ids.as_deref())
        }
    }).await
}

/// The buffs and debuffs of the live fight on a target (Details' Buffs
/// section), and its stagger windows over `target_ids`, all the targets
/// Details shows together (see `DpsCalculator::live_fight_buffs`).
#[tauri::command]
async fn get_fight_buffs(app: tauri::AppHandle, target_id: i32, target_ids: Option<Vec<i32>>) -> Result<Option<crate::combat::fight_buffs::LiveFightBuffs>, String> {
    crate::blocking::CALCULATIONS.run(move || {
        details_reader(&app.state::<AppState>()).live_fight_buffs(target_id, target_ids.as_deref().unwrap_or_default())
    }).await
}

#[tauri::command]
async fn get_details_context(app: tauri::AppHandle) -> Result<DetailsContext, String> {
    crate::blocking::CALCULATIONS.run(move || {
        details_reader(&app.state::<AppState>()).get_details_context()
    }).await
}

#[tauri::command]
/// `async` keeps this off the main thread. Sync commands run there, and even
/// with the summary cache a cold call reads every fight file — measured at
/// ~350ms, during which no other IPC and no window painting can proceed. Each
/// window calls this at startup and again every 10s, which is what made opening
/// History feel like it hung.
async fn get_fight_history(app: tauri::AppHandle) -> Result<Vec<FightSummary>, String> {
    crate::blocking::HISTORY.run(move || {
        app.state::<AppState>().fight_history.list_fights()
    }).await
}

/// Every personal best (`history::personal_bests`), off the main thread as
/// `get_fight_history` is.
#[tauri::command]
async fn get_personal_bests(app: tauri::AppHandle) -> Result<Vec<crate::history::personal_bests::Encounter>, String> {
    crate::blocking::HISTORY.run(move || {
        app.state::<AppState>().fight_history.personal_bests()
    }).await
}

#[tauri::command]
async fn save_fight(app: tauri::AppHandle, record: FightRecord) -> Result<(), String> {
    crate::blocking::HISTORY.run(move || {
        let state = app.state::<AppState>();
        let _saving = state.fight_save.lock();
        state.fight_history.save_fight(&record)
    }).await?
}

#[tauri::command]
async fn load_fight(app: tauri::AppHandle, id: String) -> Result<FightRecord, String> {
    crate::blocking::HISTORY.run(move || {
        let state = app.state::<AppState>();
        let mut record = state.fight_history.load_fight(&id)?;

        // Re-resolve supporter status against the roster as it is *now*, rather
        // than trusting the flag written when the fight was saved. Supporter status
        // changes; a fight from last month opened today should show who is a
        // supporter today, and every record saved before this feature existed has
        // no flag at all.
        //
        // Party members are the honest limitation here. `obscure_nickname` masks
        // their names before the record is written, so a name-keyed roster can only
        // ever match the local player, whose name is stored intact. `dbid` is kept
        // on each actor precisely so a dbid-keyed roster resolves everyone — see
        // `crate::supporters::KeyKind`.
        crate::supporters::apply_to_record(&mut record, &state.data_storage.supporters());
        Ok(record)
    }).await?
}

#[tauri::command]
async fn delete_fight(app: tauri::AppHandle, id: String) -> Result<(), String> {
    if !crate::history::fight_history::is_plain_name(&id) {
        return Err(format!("Invalid fight id: {id:?}"));
    }
    crate::blocking::HISTORY.run(move || {
        let state = app.state::<AppState>();
        let _saving = state.fight_save.lock();
        share::forget_slice(&state.app_data_dir, &id);
        state.fight_history.delete_fight(&id)
    }).await?
}

#[cfg(feature = "online")]
/// Upload a saved fight to a2tools.app as a log, and return its link.
#[tauri::command]
async fn upload_fight(
    state: tauri::State<'_, AppState>,
    fight_id: String,
) -> Result<share::UploadResult, String> {
    let record = state.fight_history.load_fight(&fight_id)?;
    share::upload(&state.http, &state.app_data_dir, &record, &state.settings).await
}

#[cfg(feature = "online")]
/// Upload a finished fight in the background, and tell every window.
///
/// Failure is quiet on purpose: not being signed in, or being offline, is not
/// something to interrupt a fight about. The fight keeps its slice, and the
/// upload button in History still works. A failure that waiting could fix
/// (offline, a server error, a rate limit) is tried again later, on the
/// schedule in `share::note_auto_upload_failure`.
fn auto_upload(app: tauri::AppHandle, record: FightRecord) {
    let Some(in_flight) = InFlight::start(&record.id) else { return };
    tauri::async_runtime::spawn(async move {
        let _in_flight = in_flight;
        let Some(state) = app.try_state::<AppState>() else { return };
        match share::upload_detailed(&state.http, &state.app_data_dir, &record, &state.settings).await {
            Ok(result) => {
                tracing::info!("Auto-uploaded {} -> {}", record.id, result.url);
                let _ = app.emit("fight-uploaded", serde_json::json!({
                    "fightId": record.id, "url": result.url, "visibility": result.visibility,
                }));
            }
            Err(failure) => {
                share::note_auto_upload_failure(
                    &state.app_data_dir, &record.id, &failure, crate::clock::now_ms());
                tracing::info!(
                    "Auto-upload of {} failed{}: {}",
                    record.id,
                    if failure.retryable { ", will retry" } else { "" },
                    failure.message
                );
            }
        }
    });
}

/// Save fights to History, with the packets behind each so it can be uploaded
/// and verified later (training dummies are not logs), and auto-upload the
/// finished ones when that is on.
#[cfg_attr(not(feature = "online"), allow(unused_variables))]
fn save_fight_records(app: &tauri::AppHandle, state: &AppState, records: Vec<FightRecord>, names: &share::SliceNames) {
    for record in &records {
        let _ = state.fight_history.save_fight(record);
        if !record.is_train {
            if let Err(e) = share::save_slice(&state.app_data_dir, record, names) {
                // A fight that began before the meter did is normal; anything
                // else is why an upload will later say no packets were saved.
                if e.starts_with("no packets in memory") {
                    tracing::debug!("No slice for {}: {e}", record.id);
                } else {
                    tracing::warn!("No slice for {}: {e}", record.id);
                }
            }
        }
    }
    if !records.is_empty() {
        share::prune_slices(&state.app_data_dir);
    }
    #[cfg(feature = "online")]
    if share::auto_uploads(&state.settings) {
        let now = crate::clock::now_ms();
        for record in records.into_iter()
            .filter(|r| share::wants_auto_upload(&state.app_data_dir, r, now))
        {
            auto_upload(app.clone(), record);
        }
    }
}

/// Save every fight on the meter, before its combat data is cleared: a zone
/// change, the end of a party, the reset button or the reload hotkey. Each
/// cleared it unsaved, and leaving an instance right after a kill lost
/// everything since the last 30-second save (issue #19).
///
/// The records are taken now; writing them, and cutting each fight's slice,
/// is queued behind any save already running. A zone change and the end of a
/// party call this from the parser's thread, and waiting there for the disk
/// stopped the parser: packets were lost and the meter froze for minutes.
fn save_fights_before_reset(app: &tauri::AppHandle) {
    let Some(fights) = fights_before_reset(app) else { return };
    let Some(state) = app.try_state::<AppState>() else { return };
    state.queued_fight_saves.lock().push(fights);
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let job = handle.clone();
        if let Err(error) = crate::blocking::HISTORY.run(move || save_queued_fights(&job)).await {
            tracing::warn!("Saving fights before a reset failed: {error}");
        }
    });
}

/// Save the fights `save_fights_before_reset` queued, if nobody has yet.
fn save_queued_fights(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<AppState>() else { return };
    let _saving = state.fight_save.lock();
    let queued = std::mem::take(&mut *state.queued_fight_saves.lock());
    for (records, names) in queued {
        save_fight_records(app, &state, records, &names);
    }
}

/// Save every fight on the meter before quitting, and wait for it: the
/// fights still queued from a reset first, which a save running on HISTORY
/// could hold up past the exit.
fn save_fights_before_quit(app: &tauri::AppHandle) {
    save_queued_fights(app);
    let Some((records, names)) = fights_before_reset(app) else { return };
    let Some(state) = app.try_state::<AppState>() else { return };
    let _saving = state.fight_save.lock();
    save_fight_records(app, &state, records, &names);
}

/// The fights on the meter, as records, with the names their slices need.
fn fights_before_reset(app: &tauri::AppHandle) -> Option<(Vec<FightRecord>, share::SliceNames)> {
    let state = app.try_state::<AppState>()?;
    if state.data_storage.damage_generation() <= 0 {
        return None;
    }
    let records = state.dps_calculator.lock().snapshot_boss_fights_force();
    if records.is_empty() {
        return None;
    }
    tracing::info!("Saving {} fight(s) before combat data is cleared", records.len());
    Some((records, share::SliceNames::of(&state.data_storage)))
}

#[cfg(feature = "online")]
/// Fights with an automatic upload running. One upload of a fight at a time:
/// a retry must not start while the first try is still waiting on the network.
static IN_FLIGHT: std::sync::LazyLock<Mutex<HashSet<String>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashSet::new()));

#[cfg(feature = "online")]
/// A fight's place in `IN_FLIGHT`, given up on drop: every way out of the
/// upload task clears it, a panic included.
struct InFlight(String);

#[cfg(feature = "online")]
impl InFlight {
    fn start(id: &str) -> Option<Self> {
        IN_FLIGHT.lock().insert(id.to_string()).then(|| Self(id.to_string()))
    }
}

#[cfg(feature = "online")]
impl Drop for InFlight {
    fn drop(&mut self) {
        IN_FLIGHT.lock().remove(&self.0);
    }
}

/// Which fights have a slice to upload, and which already have a link.
#[tauri::command]
async fn share_status(
    state: tauri::State<'_, AppState>,
) -> Result<std::collections::HashMap<String, share::ShareStatus>, String> {
    let dir = state.app_data_dir.clone();
    tokio::task::spawn_blocking(move || share::share_status(&dir))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn export_fight_json(state: tauri::State<'_, AppState>, record: FightRecord) -> Result<String, String> {
    state.fight_history.export_fight_json(&record)
}

/// Write what sharing this fight *would* upload, without uploading anything.
///
/// The point is auditability: it produces the `.a2es` and `.upload.json` an
/// upload would send, cut the same way (only the name tokens differ: each cut
/// draws its own key), so a user can open them — `a2t-inspect` reads the
/// former — instead of taking `docs/PRIVACY.md` on faith. There is no network
/// call anywhere in this path.
///
/// Async because it re-parses a packet capture and replays it to resolve the
/// names the blinder has to remove; on a long capture that is seconds of CPU,
/// and a sync command would hold the main thread (see `get_fight_history`).
#[tauri::command]
async fn preview_share(
    state: tauri::State<'_, AppState>,
    fight_id: String,
) -> Result<share::PreviewResult, String> {
    let record = state.fight_history.load_fight(&fight_id)?;
    let app_data_dir = state.app_data_dir.clone();
    tokio::task::spawn_blocking(move || {
        let captures = share::find_captures(&app_data_dir);
        let out_dir = app_data_dir.join("share-preview");
        share::preview(&record, &captures, &out_dir)
    })
    .await
    .map_err(|e| format!("preview task failed: {e}"))?
}

#[cfg(feature = "online")]
/// Who, if anyone, this meter is signed in as.
///
/// Returns `None` when there is no token, or when the server says the one we
/// have is no longer valid — a token revoked from the website should stop
/// looking connected here at the next check, not at the next reinstall.
#[tauri::command]
async fn account_status(
    state: tauri::State<'_, AppState>,
) -> Result<Option<crate::account::AccountSummary>, String> {
    let who = match crate::account::whoami(&state.http, &state.app_data_dir).await {
        crate::account::AccountState::SignedIn(summary) => Some(summary),
        crate::account::AccountState::SignedOut => None,
        crate::account::AccountState::Unavailable(why) => return Err(why),
    };
    // Signed in with fights still uploaded as a guest on this PC (the claim
    // at sign-in didn't get through): claimed now.
    if who.is_some() && crate::account::guest::has_guest(&state.app_data_dir) {
        crate::account::guest::merge_into_account(&state.http, &state.app_data_dir).await;
    }
    *state.account_seen.lock() = Some(who.clone());
    Ok(who)
}

#[cfg(feature = "online")]
/// "Stop sharing" (the sharing notice, 2.0.80 on): no more uploads without an
/// account, and the fights this PC already uploaded that way deleted from
/// a2tools.app, in the background.
#[tauri::command]
fn guest_stop_sharing(app: tauri::AppHandle, state: tauri::State<'_, AppState>) {
    state.settings.set(crate::account::guest::GUEST_UPLOAD_KEY, "false");
    state.settings.set(crate::account::guest::DELETE_PENDING_KEY, "true");
    spawn_guest_delete(app);
}

#[cfg(feature = "online")]
/// Delete this PC's guest uploads while that is pending: at once, then less
/// and less often (up to every 30 minutes) until the site answers; a meter
/// closed meanwhile tries again at its next start.
fn spawn_guest_delete(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut wait = Duration::from_secs(30);
        loop {
            let Some(state) = app.try_state::<AppState>() else { return };
            if state.settings.get(crate::account::guest::DELETE_PENDING_KEY).as_deref() != Some("true") {
                return;
            }
            if crate::account::guest::delete_uploads(&state.http, &state.app_data_dir).await.is_some() {
                state.settings.set(crate::account::guest::DELETE_PENDING_KEY, "false");
                return;
            }
            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(Duration::from_secs(30 * 60));
        }
    });
}

#[cfg(feature = "online")]
/// Whether this PC has fights uploaded without an account, not claimed yet.
#[tauri::command]
fn guest_status(state: tauri::State<'_, AppState>) -> serde_json::Value {
    serde_json::json!({ "hasGuest": crate::account::guest::has_guest(&state.app_data_dir) })
}

#[cfg(feature = "online")]
/// A code that claims this PC's guest fights at a2tools.app/claim.
#[tauri::command]
async fn guest_claim_code(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
    let (code, expires_at) = crate::account::guest::claim_code(&state.http, &state.app_data_dir).await?;
    Ok(serde_json::json!({ "code": code, "expiresAt": expires_at, "url": format!("{}/claim", crate::account::base_url()) }))
}

#[cfg(feature = "online")]
/// Whether this build can show a Discord activity (it has a Discord
/// application configured). The Settings toggle is hidden when it cannot.
#[tauri::command]
fn discord_activity_available() -> bool {
    crate::presence::available()
}

#[cfg(feature = "online")]
/// What the last `account_status` found, without asking the server again.
/// `None` when nothing has been checked yet this session.
#[tauri::command]
fn account_status_cached(
    state: tauri::State<'_, AppState>,
) -> Option<Option<crate::account::AccountSummary>> {
    state.account_seen.lock().clone()
}

#[cfg(feature = "online")]
/// Begin signing in, and return the code to show the player.
///
/// The meter has no browser and cannot hold a client secret, so this is the
/// Device Authorization Grant: the server hands back a short code, the player
/// approves it on a2tools.app, and a background task polls until they do. The
/// polling credential never reaches the webview — see `DeviceGrant`.
#[tauri::command]
async fn account_begin_link(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<crate::account::LinkPrompt, String> {
    let grant = crate::account::start(&state.http, &crate::account::device_label()).await?;
    let prompt = crate::account::LinkPrompt::from(&grant);

    // Open the browser straight onto the filled-in code. If it fails the player
    // still has the code and the URL in front of them.
    if crate::account::is_site_url(&grant.verification_uri_complete) {
        open_url(grant.verification_uri_complete.clone());
    } else {
        tracing::warn!("Not opening the sign-in page: the server sent a link outside a2tools.app");
    }

    let http = state.http.clone();
    let app_data_dir = state.app_data_dir.clone();
    tauri::async_runtime::spawn(async move {
        let outcome = crate::account::poll_until_decided(&http, &grant, &app_data_dir).await;
        // Fights uploaded without an account on this PC become the account's.
        let claimed = if outcome.is_ok() && crate::account::guest::has_guest(&app_data_dir) {
            crate::account::guest::merge_into_account(&http, &app_data_dir).await.unwrap_or(0)
        } else {
            0
        };
        let payload = match &outcome {
            Ok(()) => serde_json::json!({ "connected": true, "claimed": claimed }),
            Err(why) => serde_json::json!({ "connected": false, "error": why }),
        };
        if let Err(e) = app.emit("account-changed", payload) {
            tracing::warn!("could not announce the account change: {e}");
        }
    });

    Ok(prompt)
}

#[cfg(feature = "online")]
/// Forget the token on this machine.
///
/// Local only, deliberately. Revoking it everywhere is a decision to make on the
/// website, where every device that holds a token is listed — a meter that could
/// silently revoke its own credential server-side would make "sign out on this
/// PC" and "this PC was stolen" the same button.
#[tauri::command]
fn account_sign_out(state: tauri::State<'_, AppState>) {
    crate::account::secret::clear(&state.app_data_dir);
    *state.account_seen.lock() = Some(None);
    tracing::info!("Account signed out on this machine");
}

#[cfg(feature = "online")]
/// The stream overlay as Settings shows it: on or off, the port, the URLs.
#[tauri::command]
fn stream_overlay_status(state: tauri::State<'_, AppState>) -> crate::stream_overlay::Status {
    state.stream_overlay.status(&state.settings)
}

#[cfg(feature = "online")]
/// Turn the stream overlay on or off, or move it to another port. Async so a
/// restart, which waits for the old port to close, never holds the UI thread.
#[tauri::command]
async fn stream_overlay_configure(
    app: tauri::AppHandle,
    enabled: bool,
    port: u16,
) -> Result<crate::stream_overlay::Status, String> {
    if port < 1024 {
        return Err("port must be between 1024 and 65535".into());
    }
    {
        let state = app.state::<AppState>();
        state.settings.set(crate::stream_overlay::PORT_KEY, &port.to_string());
        state.settings.set(crate::stream_overlay::ENABLED_KEY, if enabled { "true" } else { "false" });
    }
    Ok(crate::stream_overlay::sync(&app))
}

#[cfg(feature = "online")]
/// A fresh overlay key: every URL handed out before stops working.
#[tauri::command]
async fn stream_overlay_new_key(app: tauri::AppHandle) -> Result<crate::stream_overlay::Status, String> {
    crate::stream_overlay::regenerate_token(&app.state::<AppState>().settings);
    Ok(crate::stream_overlay::sync(&app))
}

#[tauri::command]
fn get_settings(state: tauri::State<'_, AppState>) -> std::collections::HashMap<String, String> {
    state.settings.get_all()
}

/// How often the DPS update is checked for a change: Settings > Advanced,
/// "Meter update rate", one of `DPS_RATES_MS`.
const DPS_RATE_KEY: &str = "dpsMeter.updateRateMs";
const DPS_RATE_DEFAULT_MS: u64 = 200;
const DPS_RATES_MS: [u64; 4] = [100, 200, 250, 500];
/// An unchanged DPS update is sent again after this long.
const DPS_RESEND: Duration = Duration::from_secs(2);

fn dps_rate_ms(settings: &Settings) -> u64 {
    settings.get(DPS_RATE_KEY)
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|ms| DPS_RATES_MS.contains(ms))
        .unwrap_or(DPS_RATE_DEFAULT_MS)
}

/// Store a setting and tell every window about it.
///
/// Settings are edited in their own window, so without this broadcast the meter
/// keeps rendering with whatever it read at startup — toggling something like
/// "Round DPS" would appear to do nothing until the app restarted. Only real
/// changes are emitted (see `Settings::set`), so the originating window's echo
/// stops here rather than bouncing between windows.

/// The All Targets time range (Settings), in milliseconds.
const ALL_TARGETS_WINDOW_KEY: &str = "dpsMeter.allTargetsWindowMs";
const METER_ANCHOR_KEY: &str = "dpsMeter.meterAnchor";
const WINDOW_HEIGHT_KEY: &str = "window.h";

fn apply_all_targets_window(state: &AppState, value: &str) {
    if let Ok(ms) = value.trim().parse::<i64>() {
        state.dps_calculator.lock().set_all_targets_window_ms(ms);
    }
}

/// Whether global hotkeys are on (Settings → Keybinds). Unset, each OS's
/// default: on for Windows, off for Linux, where registering makes the meter
/// briefly inspectable (`platform::hotkeys::DEFAULT_ON`).
const GLOBAL_HOTKEYS_KEY: &str = "dpsMeter.globalHotkeys";

/// The hotkey listener, kept so the setting can start and stop it, and what
/// each key's registration came to: "ok", or "taken" when another program
/// (or, on Linux, the desktop) holds that combination. Settings frames a
/// taken key in red.
struct Hotkeys {
    manager: platform::hotkeys::HotkeyManager,
    status: Arc<Mutex<HashMap<String, String>>>,
}

/// The keybind settings: changing one registers the keys again.
const HOTKEY_KEYS: [&str; 4] =
    ["dpsMeter.hotkey", "dpsMeter.toggleWindowHotkey", "dpsMeter.lockHotkey", "dpsMeter.pluginWindowsHotkey"];

/// Open the desktop's own shortcut settings, where the keys are set on
/// Linux (KDE keeps its own; see platform::hotkeys).
#[tauri::command]
fn open_desktop_shortcuts(hotkeys: tauri::State<'_, Hotkeys>) -> Result<(), String> {
    hotkeys.manager.open_desktop_settings()
}

/// What each key's registration came to, for Settings to show.
#[tauri::command]
fn get_hotkey_status(hotkeys: tauri::State<'_, Hotkeys>) -> HashMap<String, String> {
    hotkeys.status.lock().clone()
}

fn set_hotkey_status(app: &tauri::AppHandle, update: impl FnOnce(&mut HashMap<String, String>)) {
    let Some(hotkeys) = app.try_state::<Hotkeys>() else { return };
    let snapshot = {
        let mut status = hotkeys.status.lock();
        update(&mut status);
        status.clone()
    };
    let _ = app.emit("hotkey-status", snapshot);
}

/// Start or stop the hotkeys to match the settings.
fn apply_global_hotkeys(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<AppState>() else { return };
    let wanted = match state.settings.get(GLOBAL_HOTKEYS_KEY).as_deref() {
        Some("true") => true,
        Some("false") => false,
        _ => platform::hotkeys::DEFAULT_ON,
    };
    if wanted {
        start_hotkeys(app, &state.settings);
    } else if let Some(hotkeys) = app.try_state::<Hotkeys>() {
        hotkeys.manager.stop();
        set_hotkey_status(app, |s| s.clear());
    }
}

/// A key changed: register them again with it, if they are on.
fn restart_global_hotkeys(app: &tauri::AppHandle) {
    if let Some(hotkeys) = app.try_state::<Hotkeys>() {
        hotkeys.manager.stop();
    }
    set_hotkey_status(app, |s| s.clear());
    apply_global_hotkeys(app);
}

/// The keybind settings and their defaults, as Settings shows them: the
/// plugin windows key has none (`""`), so it is off until one is set.
pub const HOTKEY_DEFAULTS: [(&str, &str); 4] = [
    ("dpsMeter.hotkey", "Ctrl+Alt+R"),
    ("dpsMeter.toggleWindowHotkey", "Ctrl+Alt+Up"),
    ("dpsMeter.lockHotkey", "Ctrl+Alt+L"),
    ("dpsMeter.pluginWindowsHotkey", ""),
];

/// A keybind setting's key: what is saved, else (unset or unreadable) its
/// default. A key of 0 is off.
fn hotkey_of(settings: &Settings, key: &str) -> (u32, u32) {
    let default = HOTKEY_DEFAULTS.iter().find(|(k, _)| *k == key).map(|(_, d)| *d).unwrap_or("");
    let label = settings.get(key).unwrap_or_else(|| default.to_string());
    platform::hotkeys::parse_hotkey_label(&label)
        .or_else(|| platform::hotkeys::parse_hotkey_label(default))
        .unwrap_or((0, 0))
}

/// Register the hotkeys from saved settings (or defaults).
/// Show or hide the meter, and remember it (as Settings → Plugins → Show
/// the DPS meter does): its keybind and the tray's. Hidden by auto-hide
/// only, it is shown, as before, and nothing changes.
pub(crate) fn toggle_meter(app: &tauri::AppHandle) {
    if meter_wanted(app)
        && let Some(window) = app.get_webview_window("main")
        && (!window.is_visible().unwrap_or(true) || platform::window::is_hidden_in_place(&window))
    {
        show_meter_again(app, &window);
        return;
    }
    set_meter_wanted(app, !meter_wanted(app));
}

/// Toggle the click-through lock, and tell the page so its button and saved
/// setting follow: its keybind and the tray's.
pub(crate) fn toggle_overlay_lock(app: &tauri::AppHandle) {
    let locked = app
        .try_state::<AppState>()
        .is_some_and(|s| s.overlay_lock.locked.load(std::sync::atomic::Ordering::SeqCst));
    if !lock_offered() && !locked {
        return; // the lock is not offered here
    }
    apply_overlay_lock(app, !locked);
    let _ = app.emit("overlay-lock-changed", !locked);
}

fn start_hotkeys(app: &tauri::AppHandle, settings: &Settings) {
    use platform::hotkeys::Binding;
    let Some(hotkeys) = app.try_state::<Hotkeys>() else { return };
    let status_handle = app.clone();
    let binding = |name: &'static str, key: &str, on_fire: Box<dyn Fn() + Send>| {
        let (mods, vk) = hotkey_of(settings, key);
        Binding { name, mods, vk, on_fire }
    };

    let h = app.clone();
    let reload = binding("reload", "dpsMeter.hotkey", Box::new(move || {
        tracing::info!("Hotkey: reload triggered");
        save_fights_before_reset(&h);
        if let Some(state) = h.try_state::<AppState>() {
            state.dps_calculator.lock().restart_target_selection(true);
            state.data_storage.forget_guessed_nicknames();
        }
        // Notify frontend to clear UI
        let _ = h.emit("combat-reset", ());
        let _ = h.emit("dps-update", &entity::dps_data::DpsData::new());
    }));
    let h = app.clone();
    let toggle = binding("toggle", "dpsMeter.toggleWindowHotkey", Box::new(move || toggle_meter(&h)));
    let h = app.clone();
    let lock = binding("lock", "dpsMeter.lockHotkey", Box::new(move || toggle_overlay_lock(&h)));
    let h = app.clone();
    let plugins = binding("plugins", "dpsMeter.pluginWindowsHotkey", Box::new(move || {
        tracing::info!("Hotkey: bringing back the plugin windows");
        let app = h.clone();
        // Window calls belong on the main thread.
        let _ = h.run_on_main_thread(move || crate::plugins::bring_back_windows(&app));
    }));

    hotkeys.manager.start(
        vec![reload, toggle, lock, plugins],
        move |key, state, assigned| {
            set_hotkey_status(&status_handle, |s| {
                s.insert(key.to_string(), state.to_string());
                // The key the desktop gave it, where the desktop decides.
                match assigned {
                    Some(a) => s.insert(format!("{key}.assigned"), a),
                    None => s.remove(&format!("{key}.assigned")),
                };
            });
        },
    );
}

#[tauri::command]
fn update_settings(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    key: String,
    value: String,
) {
    // Kept by the meter from the folder dialog, not by the page.
    if key == PICKED_SCREENSHOT_FOLDER_KEY {
        return;
    }
    if state.settings.set(&key, &value) {
        if key == ALL_TARGETS_WINDOW_KEY {
            apply_all_targets_window(&state, &value);
        }
        if key == METER_ANCHOR_KEY {
            platform::window::set_main_anchor(platform::meter_anchor::Anchor::from_setting(Some(&value)));
        }
        if key == GLOBAL_HOTKEYS_KEY {
            apply_global_hotkeys(&app);
        }
        if HOTKEY_KEYS.contains(&key.as_str()) {
            restart_global_hotkeys(&app);
        }
        let _ = app.emit("setting-changed", serde_json::json!({ "key": key, "value": value }));
        #[cfg(feature = "online")]
        if key.starts_with(crate::stream_overlay::ENABLED_KEY) {
            crate::stream_overlay::sync(&app);
        }
    }
}

#[tauri::command]
#[cfg_attr(not(feature = "online"), allow(unused_variables))]
fn clear_settings(app: tauri::AppHandle, state: tauri::State<'_, AppState>) {
    state.settings.clear();
    #[cfg(feature = "online")]
    crate::stream_overlay::sync(&app);
}

#[tauri::command]
fn get_ping(state: tauri::State<'_, AppState>) -> Option<i32> {
    state.ping_tracker.current_ping_ms()
}

#[tauri::command]
fn get_capture_status(state: tauri::State<'_, AppState>) -> serde_json::Value {
    let port = state.port_detector.current_port();
    let device = state.port_detector.current_device();
    let local_id = state.data_storage.local_player_id();
    let char_name = state.data_storage.local_character_name();
    serde_json::json!({
        "locked": port.is_some(),
        "port": port,
        "device": device.clone().unwrap_or_default(),
        "ip": device.unwrap_or_else(|| "127.0.0.1".to_string()),
        "localPlayerId": local_id,
        "characterName": char_name,
        // When true, characterName is the game's (null = an unnamed tutorial
        // character) and the UI should adopt it rather than push its own.
        "characterNameFromGame": state.data_storage.local_identity_from_self_record(),
    })
}

#[tauri::command]
fn set_target_mode(state: tauri::State<'_, AppState>, mode: String) {
    state.dps_calculator.lock().set_target_selection_mode(&mode);
}

#[tauri::command]
fn set_character_name(state: tauri::State<'_, AppState>, name: String, manual: Option<bool>) {
    // The game has said who is playing; a name from the window title or the
    // last session is at best the same and at worst another character. A name
    // the player typed (`manual`) is taken anyway: it is their call, and the
    // game's next self record replaces it if it was wrong.
    if state.data_storage.local_identity_from_self_record() && !manual.unwrap_or(false) {
        return;
    }
    let trimmed = name.trim().to_string();
    state.data_storage.set_local_character_name(Some(name));
    // If an actor ID was already bound, propagate the new character name
    // into nickname_storage immediately so the main meter window updates.
    // Not onto an id only read from party-scope records unless typed: nothing
    // ties a remembered or window-title name to it, and after a character
    // switch it can be the last character's.
    let typed = manual.unwrap_or(false);
    if !trimmed.is_empty() && (typed || !state.data_storage.local_id_from_scope()) {
        if let Some(id) = state.data_storage.local_player_id() {
            state.data_storage.set_permanent_nickname(id as i32, &trimmed);
        }
    }
}

#[tauri::command]
fn bind_local_actor_id(state: tauri::State<'_, AppState>, actor_id: i64, manual: Option<bool>) {
    // Once the game's self record has named the player, an id the UI sends
    // back is at best the same one and at worst one from before a zone load:
    // every window echoes the id it last saw, and binding an old one put the
    // name on it, which took it off the entity the self record had named
    // (the name moves to whichever id it is bound to). Only an id the player
    // typed in Settings (`manual`) overrides the game.
    if !state.data_storage.ui_may_bind_local_id(actor_id, manual.unwrap_or(false)) {
        tracing::info!(
            "bind_local_actor_id: ignored {} (the game named {:?})",
            actor_id,
            state.data_storage.local_player_id()
        );
        return;
    }
    if actor_id <= 0 {
        // Clear manual binding — auto-detection will take over
        tracing::info!("bind_local_actor_id: cleared");
        state.data_storage.set_local_player_id(None);
        return;
    }
    let already_bound = state.data_storage.local_player_id() == Some(actor_id);
    if !already_bound {
        tracing::info!("bind_local_actor_id: {}", actor_id);
        state.data_storage.set_local_player_id(Some(actor_id));
    }
    // Always (re)apply the permanent nickname if we have a character name,
    // even when the actor_id was already bound — this handles the case where
    // the character name was set AFTER the actor_id binding. Except on an id
    // the backend only read from party-scope records (see set_character_name).
    if state.data_storage.local_id_from_scope() {
        return;
    }
    if let Some(name) = state.data_storage.local_character_name() {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            let current = state.data_storage.get_nickname(actor_id as i32);
            if current.as_deref() != Some(trimmed) {
                state.data_storage.set_permanent_nickname(actor_id as i32, trimmed);
            }
        }
    }
}

#[tauri::command]
fn bind_local_nickname(state: tauri::State<'_, AppState>, actor_id: i64, nickname: String) {
    // Always update if the stored nickname differs from the requested one.
    // Previously we skipped if the actor had ANY nickname, which left stale
    // false-positive scan results stuck in place.
    let current = state.data_storage.get_nickname(actor_id as i32);
    if state.data_storage.local_player_id() == Some(actor_id)
        && current.as_deref() == Some(nickname.as_str())
    {
        return;
    }
    // A party placeholder row is no entity.
    if actor_id >= 90_000_000 {
        return;
    }
    // The id the party-scope records point at, still unnamed: the UI's name
    // may be another character's (see set_character_name).
    if state.data_storage.local_id_from_scope() && state.data_storage.local_player_id() == Some(actor_id) {
        return;
    }
    // Once the game's self record has named the player, it alone says who
    // they are: a window binding by name kept an id from before a zone change
    // and sent it back, and uploads then named a stale uploader (issue #19).
    if state.data_storage.local_identity_from_self_record() {
        return;
    }
    tracing::info!("bind_local_nickname: {} -> '{}' (was {:?})", actor_id, nickname, current);
    state.data_storage.set_local_player_id(Some(actor_id));
    // Use set_permanent_nickname so it survives reset_nicknames() calls
    state.data_storage.set_permanent_nickname(actor_id as i32, &nickname);
}

#[tauri::command]
fn reset_combat(app: tauri::AppHandle, state: tauri::State<'_, AppState>) {
    save_fights_before_reset(&app);
    state.dps_calculator.lock().restart_target_selection(true);
    // Don't reset port detector or ping — keep the network connection alive.
    // Clear combat data, and only the names a loose scan guessed: the game
    // does not send the others again until everyone respawns.
    state.data_storage.forget_guessed_nicknames();
    state.data_storage.hide_party_placeholders();
}

#[tauri::command]
fn is_admin() -> bool {
    platform::admin::is_admin()
}

#[tauri::command]
fn set_language(app: tauri::AppHandle, state: tauri::State<'_, AppState>, language: String) {
    tracing::info!("Language change requested: {}", language);
    if let Some(ref data_dir) = state.i18n_data_dir {
        i18n::lookup::load_language(&state.skill_lookup, &state.npc_lookup, data_dir, &language);
    } else {
        tracing::warn!("No i18n data dir available for language reload");
    }
    state.settings.set("dpsMeter.language", &language);
    crate::tray::relabel(&app);
    crate::plugins::broadcast(&app, "language", serde_json::Value::String(language));
}

#[tauri::command]
fn set_debug_logging(state: tauri::State<'_, AppState>, enabled: bool) {
    logging::logger::set_debug_enabled(enabled, &state.app_data_dir);
    state.settings.set("dpsMeter.debugLoggingEnabled", if enabled { "true" } else { "false" });
}

/// Settings' "Run as administrator": on restarts the meter elevated (false
/// when Windows' prompt was refused); off takes effect at the next start.
#[tauri::command]
async fn set_run_as_admin(app: tauri::AppHandle, enabled: bool) -> Result<bool, String> {
    if !enabled {
        if let Some(state) = app.try_state::<AppState>() {
            state.settings.set("dpsMeter.runAsAdmin", "false");
        }
        return Ok(true);
    }
    tokio::task::spawn_blocking(move || restart_as_admin(&app)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
fn set_packet_logging(state: tauri::State<'_, AppState>, enabled: bool) {
    logging::logger::set_packet_log_enabled(enabled, &state.app_data_dir);
    state.settings.set("dpsMeter.saveRawPackets", if enabled { "true" } else { "false" });
}

#[cfg(feature = "online")]
/// What Send Logs to Dev would send, for the page to show before it does
/// (security audit M10): each file's name and size.
#[tauri::command]
fn dev_logs_preview(state: tauri::State<'_, AppState>) -> Vec<serde_json::Value> {
    crate::share::dev_logs::files_to_send(&state.app_data_dir)
        .iter()
        .map(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            let bytes = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            serde_json::json!({ "name": name, "bytes": bytes })
        })
        .collect()
}

#[cfg(feature = "online")]
/// Send the newest packet captures to the developer (Settings, beside packet
/// logging). Returns the report code the player passes on.
#[tauri::command]
async fn send_logs_to_dev(
    state: tauri::State<'_, AppState>,
) -> Result<share::dev_logs::SendResult, String> {
    share::dev_logs::send(&state.http, &state.app_data_dir).await
}

#[tauri::command]
fn reset_auto_detection(state: tauri::State<'_, AppState>) {
    state.port_detector.reset();
    state.ping_tracker.reset();
}

#[tauri::command]
async fn get_available_devices() -> Vec<String> {
    // Device discovery can block in the OS/pcap library. Keep it off the UI loop.
    tauri::async_runtime::spawn_blocking(|| {
        crate::capture::pcap_capturer::list_device_labels().unwrap_or_default()
    }).await.unwrap_or_default()
}

#[tauri::command]
fn set_manual_device(state: tauri::State<'_, AppState>, device: String) {
    let dev = if device.trim().is_empty() { None } else { Some(device) };
    state.port_detector.set_preferred_device(dev);
}

#[tauri::command]
pub(crate) fn quit_app(app: tauri::AppHandle) {
    save_fights_before_quit(&app);
    flush_settings_before_exit(&app);
    app.exit(0);
}

fn flush_settings_before_exit(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        if let Err(error) = state.settings.flush() {
            tracing::warn!("Could not save final settings: {error}");
        }
    }
}

/// A skill icon from the disk cache. `async`, like `write_cached_icon`: as
/// sync commands they ran on the main thread, a hundred or so of each when a
/// fight opened, and every window stopped moving and painting for seconds
/// (2026-10-18).
#[tauri::command]
async fn read_cached_icon(app: tauri::AppHandle, key: String) -> Result<Option<String>, String> {
    if !crate::history::fight_history::is_plain_name(&key) {
        return Ok(None);
    }
    crate::blocking::ICONS.run(move || {
        let path = app.state::<AppState>().app_data_dir.join("icon_cache").join(&key);
        std::fs::read_to_string(&path).ok()
    }).await
}

#[tauri::command]
fn suspend_capture(state: tauri::State<'_, AppState>, suspended: bool) {
    // The header's suspend button. It was wired to empty stubs since the move
    // to Tauri, so it changed its icon and the status line but counting went
    // on (a player found it in 2.0.37, issue #6).
    state.capture_suspended.store(suspended, std::sync::atomic::Ordering::SeqCst);
    tracing::info!("Capture {}", if suspended { "suspended" } else { "resumed" });
}

/// Whether the click-through lock can work here (it needs the pointer's
/// position outside the window; see `OverlayLock`).
#[tauri::command]
fn overlay_lock_supported() -> bool {
    lock_offered()
}

/// Whether the click-through lock is offered: where the lock button keeps
/// the mouse by an input region (Linux, native Wayland too: it was offered
/// only where the pointer could be read, X11, and so not on KDE), or by a
/// pointer watch.
fn lock_offered() -> bool {
    platform::window::click_through_by_region() || platform::window::cursor_position().is_some()
}

/// Where the mouse pointer is from the calling window's top-left, in screen
/// pixels, or `None` where it can't be read (a native Wayland window). The
/// hover tooltip checks it: a row can miss its mouse-leave (the game takes
/// the pointer while it is over the meter), and the tooltip then stayed up.
#[tauri::command]
fn pointer_in_window(window: tauri::WebviewWindow) -> Option<(i32, i32)> {
    let (px, py) = platform::window::cursor_position()?;
    let origin = window.inner_position().ok()?;
    Some((px - origin.x, py - origin.y))
}

#[tauri::command]
fn set_overlay_locked(app: tauri::AppHandle, locked: bool) {
    apply_overlay_lock(&app, locked);
}

/// A card opened or closed in the main window's page (see
/// `OverlayLock::card_open`). The lock itself stays as it was.
#[tauri::command]
fn set_overlay_card_open(app: tauri::AppHandle, open: bool) {
    let Some(state) = app.try_state::<AppState>() else { return };
    let lock = state.overlay_lock.clone();
    lock.card_open.store(open, std::sync::atomic::Ordering::SeqCst);
    sync_click_through(&app, &lock, false);
}

#[tauri::command]
fn is_overlay_locked(state: tauri::State<'_, AppState>) -> bool {
    state.overlay_lock.locked.load(std::sync::atomic::Ordering::SeqCst)
}

/// Where the lock button is in the main window's page, so it stays clickable
/// while the rest of the window lets clicks through.
#[tauri::command]
fn set_lock_button_rect(app: tauri::AppHandle, state: tauri::State<'_, AppState>, x: f64, y: f64, width: f64, height: f64, scale: f64) {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    *state.overlay_lock.button.lock() = Some((x, y, width, height, scale));
    // On Linux the button's input region moves with it.
    if cfg!(target_os = "linux") {
        let lock = state.overlay_lock.clone();
        sync_click_through(&app, &lock, false);
    }
}

#[tauri::command]
fn is_capture_suspended(state: tauri::State<'_, AppState>) -> bool {
    state.capture_suspended.load(std::sync::atomic::Ordering::SeqCst)
}

#[tauri::command]
fn log_from_ui(message: String) {
    // A problem only the webview can see (an icon the CDN would not serve, say),
    // for debug.log. The UI keeps these few; this keeps each one short.
    // One line: a newline let the page write lines that read as the
    // meter's own (security audit I1).
    let message: String = message.chars().take(300).map(|c| if c.is_control() { ' ' } else { c }).collect();
    tracing::warn!("UI: {message}");
}

/// Keep a downloaded skill icon (base64) for next time. Off the main thread;
/// see `read_cached_icon`. An icon already kept is not written again.
#[tauri::command]
async fn write_cached_icon(app: tauri::AppHandle, key: String, data: String) -> Result<(), String> {
    if !crate::history::fight_history::is_plain_name(&key) {
        return Ok(());
    }
    crate::blocking::ICONS.run(move || {
        let cache_dir = app.state::<AppState>().app_data_dir.join("icon_cache");
        let path = cache_dir.join(&key);
        if !path.exists() {
            let _ = std::fs::create_dir_all(&cache_dir);
            let _ = std::fs::write(&path, &data);
        }
    }).await
}


#[cfg(feature = "online")]
/// Look for an update (2.0.80 on): the meter fetches and checks the manifest
/// itself (`update_check`: signature, newer than this version) and offers
/// what it names. `ask` false: only whether there is one to offer (the green
/// button in the title, which asks when clicked). Answers { state, latest }:
/// "current", "offer" (ask false), "installing" (accepted) or "update" (one
/// is out: declined, or this install's package manager updates it).
#[tauri::command]
async fn check_for_update(app: tauri::AppHandle, state: tauri::State<'_, AppState>, ask: Option<bool>) -> Result<serde_json::Value, String> {
    let manifest = crate::update_check::fetch(&state.http).await?;
    let current = env!("CARGO_PKG_VERSION");
    let latest = manifest.version.trim_start_matches(['v', 'V']).to_string();
    if !crate::update_check::newer(&latest, current) {
        return Ok(serde_json::json!({ "state": "current", "latest": latest }));
    }
    let some = |s: String| Some(s).filter(|s| !s.is_empty());
    let yes = show_update_window(
        app,
        current.to_string(),
        latest.clone(),
        manifest.msi_url,
        some(manifest.arch_url),
        some(manifest.deb_url),
        some(manifest.rpm_url),
        some(manifest.msi_sha256),
        some(manifest.arch_sha256),
        some(manifest.deb_sha256),
        some(manifest.rpm_sha256),
        ask,
    )
    .await?;
    let state = match (ask, yes) {
        (Some(false), true) => "offer",
        (Some(false), false) => "update",
        (_, true) => "installing",
        (_, false) => "update",
    };
    Ok(serde_json::json!({ "state": state, "latest": latest }))
}

#[cfg(feature = "online")]
/// The last releases' notes, for Settings → What's new (#127): fetched and
/// signature-checked like the update manifest (`update_check`).
#[tauri::command]
async fn release_notes(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
    crate::update_check::fetch_changelog(&state.http).await
}

#[cfg(feature = "online")]
/// Offer the update `check_for_update` found. Not a command any more: the
/// page used to call it with a URL and hash of its own (security audit H1).
#[allow(clippy::too_many_arguments)]
async fn show_update_window(
    app: tauri::AppHandle,
    current: String,
    latest: String,
    msi_url: String,
    arch_url: Option<String>,
    deb_url: Option<String>,
    rpm_url: Option<String>,
    msi_sha256: Option<String>,
    arch_sha256: Option<String>,
    deb_sha256: Option<String>,
    rpm_sha256: Option<String>,
    ask: Option<bool>,
) -> Result<bool, String> {
    // The manifest names a package per platform (the MSI; the Arch, Debian
    // and RPM packages). Where this install cannot update itself, or the
    // manifest has nothing for it, there is nothing to offer.
    let packages = platform::UpdatePackages {
        msi: &msi_url,
        arch: arch_url.as_deref().unwrap_or(""),
        deb: deb_url.as_deref().unwrap_or(""),
        rpm: rpm_url.as_deref().unwrap_or(""),
    };
    let package_url = platform::updater::package_url(&packages).to_string();
    // The manifest's SHA-256 for that same package, picked the same way.
    let hashes = platform::UpdatePackages {
        msi: msi_sha256.as_deref().unwrap_or(""),
        arch: arch_sha256.as_deref().unwrap_or(""),
        deb: deb_sha256.as_deref().unwrap_or(""),
        rpm: rpm_sha256.as_deref().unwrap_or(""),
    };
    let package_sha256 = platform::updater::package_url(&hashes).to_string();
    // The page hands these over from the manifest it read: only a package on
    // our CDN, and only version-shaped text in the dialog (security audit H1,
    // in part; a signed manifest checked here is the rest).
    if !package_url.is_empty() && !update_url_allowed(&package_url) {
        tracing::warn!("Update: refused a package that isn't on cdn.a2tools.app");
        return Ok(false);
    }
    // The running version is the app's own, not what the page says.
    drop(current);
    let current = env!("CARGO_PKG_VERSION").to_string();
    if !version_shaped(&latest) {
        tracing::warn!("Update: refused a latest version that isn't one");
        return Ok(false);
    }
    if !platform::updater::supported() || package_url.is_empty() {
        tracing::info!("Update {} available (running {}); this install updates through its package manager", latest, current);
        return Ok(false);
    }
    // `ask` false: only whether there is something to offer, for the green
    // button in the meter's title, which asks when clicked.
    if ask == Some(false) {
        return Ok(true);
    }
    let msg = format!("A new update is available!\n\nCurrent: {}\nLatest: {}\n\nDownload and install now?", current, latest);

    let accepted = tokio::task::spawn_blocking(move || {
        platform::dialog::ask_yes_no("A2Tools - Update Available", &msg)
    }).await.unwrap_or(false);

    if accepted {
        // Download and install in background
        let app2 = app.clone();
        let url = package_url;
        tauri::async_runtime::spawn(async move {
            if let Err(e) = download_and_install_update(&app2, &url, &package_sha256).await {
                tracing::error!("Update download failed: {}", e);
                // Show error dialog
                let _ = tokio::task::spawn_blocking(move || {
                    platform::dialog::show_error(
                        "A2Tools - Update Error",
                        &format!("Download failed: {}\n\nPlease download manually.", e),
                    );
                }).await;
            }
        });
    }

    Ok(accepted)
}

#[cfg(feature = "online")]
/// Whether a downloaded package is the one the manifest names: its SHA-256,
/// in hex, equals the manifest's. A manifest without a hash matches nothing.
/// An update package on our CDN, over https, with a plain path.
#[cfg_attr(not(feature = "online"), allow(dead_code))]
fn update_url_allowed(url: &str) -> bool {
    url.strip_prefix("https://cdn.a2tools.app/").is_some_and(|path| {
        !path.is_empty() && !path.contains("..") && path.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c))
    })
}

/// "2.0.80": digits and dots, short.
#[cfg_attr(not(feature = "online"), allow(dead_code))]
fn version_shaped(v: &str) -> bool {
    (1..=20).contains(&v.len()) && v.chars().all(|c| c.is_ascii_digit() || c == '.') && v.contains('.')
}

/// Where the downloaded package goes: a name of our own (the URL's last part
/// could carry a backslash path on Windows, audit L9), random so it can't be
/// guessed beforehand, with the package's own extension, which the installer
/// and package managers go by.
#[cfg(feature = "online")]
fn update_download_path(url: &str) -> Result<std::path::PathBuf, String> {
    let ext = [".msi", ".pkg.tar.zst", ".deb", ".rpm"]
        .into_iter()
        .find(|e| url.ends_with(e))
        .ok_or("not a package the meter installs")?;
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).map_err(|e| format!("no random name: {e}"))?;
    let tag: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(std::env::temp_dir().join(format!("a2tools-update-{tag}{ext}")))
}

#[cfg_attr(not(feature = "online"), allow(dead_code))]
fn package_hash_matches(expected: &str, actual_hex: &str) -> bool {
    let expected = expected.trim();
    expected.len() == 64
        && expected.bytes().all(|b| b.is_ascii_hexdigit())
        && expected.eq_ignore_ascii_case(actual_hex)
}

#[cfg(feature = "online")]
async fn save_update_package(
    mut response: reqwest::Response,
    path: &std::path::Path,
    cap: u64,
    mut progress: impl FnMut(u64),
) -> Result<String, String> {
    use tokio::io::AsyncWriteExt;
    use sha2::{Digest, Sha256};

    let total_size = response.content_length().unwrap_or(0);
    if total_size > cap {
        return Err(format!("update package is larger than {cap} bytes"));
    }
    let mut file = tokio::fs::File::create(path).await.map_err(|e| e.to_string())?;
    let result = async {
        let mut downloaded: u64 = 0;
        let mut last_pct: u64 = 0;
        let mut hasher = Sha256::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
            // Count bytes even when the server omits Content-Length. Check
            // before writing, so the partial file cannot grow beyond the cap.
            if chunk.len() as u64 > cap - downloaded {
                return Err(format!("update package is larger than {cap} bytes"));
            }
            hasher.update(&chunk);
            file.write_all(&chunk).await.map_err(|e| e.to_string())?;
            downloaded += chunk.len() as u64;
            if total_size > 0 {
                let pct = (downloaded.saturating_mul(100) / total_size).min(100);
                if pct != last_pct {
                    last_pct = pct;
                    progress(pct);
                }
            }
        }
        file.flush().await.map_err(|e| e.to_string())?;
        Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
    }.await;
    drop(file);
    if result.is_err() {
        let _ = tokio::fs::remove_file(path).await;
    }
    result
}

#[cfg(feature = "online")]
async fn download_and_install_update(app: &tauri::AppHandle, url: &str, expected_sha256: &str) -> Result<(), String> {
    // Only a package the manifest vouches for is installed.
    if expected_sha256.trim().is_empty() {
        tracing::warn!("Update manifest has no SHA-256 for {url}; not installing it");
        return Err("the update manifest has no checksum for this package".into());
    }

    // Show progress dialog on a blocking thread
    let app_clone = app.clone();
    let url_owned = url.to_string();

    let response = app
        .state::<AppState>()
        .http
        .get(&url_owned)
        .timeout(Duration::from_secs(600))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let msi_path = update_download_path(&url_owned)?;
    // Current Windows and Linux packages are below 24 MB; allow ample room
    // for growth while bounding a bad CDN response before checksum validation.
    let actual = save_update_package(response, &msi_path, 256 * 1024 * 1024, |pct| {
        let _ = app_clone.emit("download-progress", pct);
        tracing::info!("Download: {}%", pct);
    }).await?;
    if !package_hash_matches(expected_sha256, &actual) {
        let _ = tokio::fs::remove_file(&msi_path).await;
        tracing::warn!("Update package {} has SHA-256 {actual}, the manifest says {}; deleted, not installed", msi_path.display(), expected_sha256.trim());
        return Err("the downloaded package does not match the checksum in the update manifest".into());
    }

    tracing::info!("Download complete, launching installer: {}", msi_path.display());

    // Detect current install directory from the running executable's location
    let current_exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let install_dir = current_exe.parent()
        .ok_or("Could not determine install directory")?
        .to_string_lossy()
        .into_owned();
    // Strip a trailing backslash so msiexec doesn't interpret \" as an escape
    let install_dir = install_dir.trim_end_matches('\\').to_string();

    // Launch the installer (msiexec on Windows; see platform::updater).
    flush_settings_before_exit(app);
    platform::updater::run_installer(&msi_path, expected_sha256.trim(), &install_dir)?;

    // Give installer time to start, then exit
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    flush_settings_before_exit(&app_clone);
    app_clone.exit(0);
    Ok(())
}

#[cfg(feature = "online")]
/// Where the supporter roster lives. The same bucket the installer is served
/// from, so it costs no new infrastructure and is already cached at the edge.
///
/// v2 is keyed on name and server (`KeyKind::NameServer`), which meters before
/// 2.0.56 reject; they keep reading the name-keyed `patrons-v1.bin`.
const SUPPORTER_ROSTER_URL: &str = "https://cdn.a2tools.app/patrons-v2.bin";

/// A roster placed here overrides the downloaded one.
///
/// Build it with `a2t-roster names.txt -o <this path>`. It exists so the gold
/// name can be exercised before anything is published — otherwise the only way
/// to see the feature is to ship a real roster to every user and hope it looks
/// right.
///
/// Deliberately not restricted to debug builds. It changes nothing but the
/// colour of a name **on this machine**: the roster is only ever read locally,
/// is never uploaded, and no number, ordering or leaderboard depends on it. So
/// there is nothing here to cheat, and it doubles as the way to reproduce a
/// "why is this person gold" report.
const SUPPORTER_ROSTER_OVERRIDE: &str = "patrons-local.bin";

/// How often to look again. The override is a local file, read quickly so
/// dropping it in shows up while you are still looking at the meter. The
/// published roster is asked for every half hour, so a new supporter goes gold
/// soon; the ETag makes an unchanged roster a bodiless 304. A failed fetch (a
/// 404 before a roster is published, the CDN down) waits the same, never the
/// override's 15 seconds: every meter retrying that fast would be a storm.
const ROSTER_POLL_OVERRIDE: Duration = Duration::from_secs(15);
#[cfg(feature = "online")]
const ROSTER_POLL_PUBLISHED: Duration = Duration::from_secs(30 * 60);

/// Read the local override, if one is there.
fn load_supporter_override(app_data_dir: &std::path::Path) -> Option<crate::supporters::Roster> {
    let path = app_data_dir.join(SUPPORTER_ROSTER_OVERRIDE);
    let bytes = std::fs::read(&path).ok()?;
    match crate::supporters::Roster::parse(&bytes) {
        Some(roster) => {
            tracing::info!(
                "Supporter roster OVERRIDE in use: {} entries from {} — delete the file to \
                 go back to the published roster",
                roster.len(),
                path.display()
            );
            Some(roster)
        }
        None => {
            // Say so loudly: a malformed override looks exactly like "the
            // feature is broken" from the outside.
            tracing::warn!(
                "{} is not a supporter roster — rebuild it with `a2t-roster names.txt -o {}`",
                path.display(),
                path.display()
            );
            None
        }
    }
}

#[cfg(feature = "online")]
/// What a roster fetch came back with.
#[cfg(feature = "online")]
enum RosterFetch {
    /// A roster, and the ETag to ask with next time.
    Fresh(crate::supporters::Roster, Option<String>),
    /// 304: the one held is current.
    Unchanged,
    /// Anything else.
    Failed,
}

#[cfg(feature = "online")]
/// Download and parse the supporter roster, unless `etag` says it is unchanged.
///
/// Every failure is `Failed` and changes nothing on screen. That is
/// deliberate: this is a cosmetic, and there is no version of "the CDN is
/// down" that should produce a visible error, a retry storm, or a wrong answer.
async fn fetch_supporter_roster(client: &reqwest::Client, etag: Option<&str>) -> RosterFetch {
    let mut request = client.get(SUPPORTER_ROSTER_URL).timeout(Duration::from_secs(30));
    if let Some(tag) = etag {
        request = request.header(reqwest::header::IF_NONE_MATCH, tag);
    }
    let Ok(response) = request.send().await else {
        return RosterFetch::Failed;
    };
    if response.status() == reqwest::StatusCode::NOT_MODIFIED {
        return RosterFetch::Unchanged;
    }
    if !response.status().is_success() {
        return RosterFetch::Failed;
    }
    let tag = response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    // A roster for ten thousand supporters is about 80 KB; anything far past
    // that is not one of ours.
    let Ok(bytes) = crate::update_check::read_capped(response, 8 * 1024 * 1024).await else {
        return RosterFetch::Failed;
    };
    match crate::supporters::Roster::parse(&bytes) {
        Some(roster) => RosterFetch::Fresh(roster, tag),
        None => RosterFetch::Failed,
    }
}

// ── the private build ──────────────────────────────────────────────────────
// Built without the `online` feature (`npm run build:offline`), the meter links
// no network client and none of the code above that reaches a service: the
// A2 Tools account and uploads, Discord activity, the stream overlay, Send Logs
// to Dev, update checks and the updater. The pages still invoke these commands
// by name, so each keeps its name here and says it is not in this build; the
// pages ask `build_features` first and hide what is missing.

#[cfg(not(feature = "online"))]
const NOT_IN_THIS_BUILD: &str = "not in this build: it was built without online features";

#[cfg(not(feature = "online"))]
#[tauri::command]
async fn upload_fight(fight_id: String) -> Result<serde_json::Value, String> {
    let _ = fight_id;
    Err(NOT_IN_THIS_BUILD.into())
}

#[cfg(not(feature = "online"))]
#[tauri::command]
async fn account_status() -> Result<Option<serde_json::Value>, String> {
    Err(NOT_IN_THIS_BUILD.into())
}

#[cfg(not(feature = "online"))]
#[tauri::command]
fn account_status_cached() -> Option<Option<serde_json::Value>> {
    None
}

#[cfg(not(feature = "online"))]
#[tauri::command]
fn discord_activity_available() -> bool {
    false
}

#[cfg(not(feature = "online"))]
#[tauri::command]
async fn account_begin_link() -> Result<serde_json::Value, String> {
    Err(NOT_IN_THIS_BUILD.into())
}

#[cfg(not(feature = "online"))]
#[tauri::command]
fn account_sign_out() {}

#[cfg(not(feature = "online"))]
#[tauri::command]
fn guest_status() -> serde_json::Value {
    serde_json::json!({ "hasGuest": false })
}

#[cfg(not(feature = "online"))]
#[tauri::command]
async fn guest_claim_code() -> Result<serde_json::Value, String> {
    Err(NOT_IN_THIS_BUILD.into())
}

#[cfg(not(feature = "online"))]
#[tauri::command]
fn stream_overlay_status() -> Result<serde_json::Value, String> {
    Err(NOT_IN_THIS_BUILD.into())
}

#[cfg(not(feature = "online"))]
#[tauri::command]
async fn stream_overlay_configure(enabled: bool, port: u16) -> Result<serde_json::Value, String> {
    let _ = (enabled, port);
    Err(NOT_IN_THIS_BUILD.into())
}

#[cfg(not(feature = "online"))]
#[tauri::command]
async fn stream_overlay_new_key() -> Result<serde_json::Value, String> {
    Err(NOT_IN_THIS_BUILD.into())
}

#[cfg(not(feature = "online"))]
#[tauri::command]
fn dev_logs_preview() -> Vec<serde_json::Value> {
    Vec::new()
}

#[cfg(not(feature = "online"))]
#[tauri::command]
async fn send_logs_to_dev() -> Result<serde_json::Value, String> {
    Err(NOT_IN_THIS_BUILD.into())
}

#[cfg(not(feature = "online"))]
#[tauri::command]
async fn check_for_update(ask: Option<bool>) -> Result<serde_json::Value, String> {
    let _ = ask;
    Err(NOT_IN_THIS_BUILD.into())
}

#[cfg(not(feature = "online"))]
#[tauri::command]
async fn release_notes() -> Result<serde_json::Value, String> {
    Err(NOT_IN_THIS_BUILD.into())
}

#[cfg(not(feature = "online"))]
#[tauri::command]
fn guest_stop_sharing() {}

/// Which optional parts this build has. `online` is false in the private
/// build: the pages hide the account, uploads, Streaming, Discord activity,
/// Send Logs to Dev and the update prompt.
#[tauri::command]
fn build_features() -> serde_json::Value {
    // A development build started with A2_DEV_SUPPORTER set acts as a
    // supporter's account (bar textures), to test those without one. A
    // release build ignores it.
    let dev_supporter = cfg!(debug_assertions) && std::env::var_os("A2_DEV_SUPPORTER").is_some();
    serde_json::json!({
        "online": cfg!(feature = "online"),
        "devSupporter": dev_supporter,
        "meterAnchor": platform::window::meter_anchor_available(),
        "meterAnchorDefault": platform::meter_anchor::Anchor::platform_default().name(),
    })
}

/// Open a link in the browser. Web and mail links only: the shell hands
/// whatever it is given to its handler, so a path, a network share or another
/// program's link would have run as the meter (security audit M1, 2026-10-26).
#[tauri::command]
fn open_url(url: String) {
    if openable_url(&url) {
        platform::shell::open_url(&url);
    } else {
        tracing::warn!("open_url: refused a link that isn't http(s) or mailto");
    }
}

/// An http(s) link with a host and no credentials, or a mailto link: no
/// spaces, control characters or backslashes, at most 2048 characters.
fn openable_url(url: &str) -> bool {
    if url.len() > 2048 || url.chars().any(|c| c.is_control() || c.is_whitespace() || c == '\\') {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://")) {
        let host = rest.split(['/', '?', '#']).next().unwrap_or("");
        return !host.is_empty() && !host.contains('@');
    }
    lower.len() > "mailto:".len() && lower.starts_with("mailto:")
}

fn meter_anchor(state: &tauri::State<'_, AppState>) -> platform::meter_anchor::Anchor {
    platform::meter_anchor::Anchor::from_setting(state.settings.get(METER_ANCHOR_KEY).as_deref())
}

/// `async`: on KDE Wayland it waits for the KWin script to hear whether the
/// resize keeps the edge (`anchor_next_resize`, a round trip), which must not
/// hold the main thread. The resize itself is still made there, and the
/// command answers once it is made, so the page's sizes keep their order.
#[tauri::command]
async fn resize_window(
    app: tauri::AppHandle,
    width: f64,
    height: f64,
    scale: Option<f64>,
    preserve_anchor: Option<bool>,
    player_resize: Option<bool>,
) {
    // A resize keeps the edge only when it says so (the page fitting the
    // window to the meter): a tooltip, the details panel, the promo or the
    // resize handle grows it for a moment without moving it.
    let anchored = preserve_anchor.unwrap_or(false);
    // Where the meter does not place its own windows, the KWin script keeps
    // the edge (KDE Wayland), told first whether this resize keeps it.
    if !platform::window::places_own_windows() {
        platform::window::anchor_next_resize(anchored).await;
    }
    let (done, resized) = tokio::sync::oneshot::channel();
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        resize_overlay(&handle, width, height, scale, anchored, player_resize == Some(true));
        let _ = done.send(());
    });
    let _ = resized.await;
}

fn resize_overlay(app: &tauri::AppHandle, width: f64, height: f64, scale: Option<f64>, anchored: bool, player_resize: bool) {
    // Only the overlay auto-sizes itself. The details window is sized to a
    // whole monitor by open_details_window and must never be resized from JS.
    let Some(window) = app.get_webview_window("main") else { return };
    let state = app.state::<AppState>();
    // `width`/`height` are the page's CSS pixels and `scale` its
    // devicePixelRatio. WebView2 draws a CSS pixel at the display scale TIMES
    // Windows' Accessibility "Text size", while a logical size here covers the
    // display scale only, so with Text size above 100% the meter outgrew its
    // window and was cut off. The page's own ratio covers both.
    let size = match scale.filter(|s| s.is_finite() && *s > 0.0) {
        Some(scale) => tauri::Size::Physical(tauri::PhysicalSize {
            width: (width * scale).ceil() as u32,
            height: (height * scale).ceil() as u32,
        }),
        None => tauri::Size::Logical(tauri::LogicalSize { width, height }),
    };
    // The player grabbed the resize handle: the edge is kept from wherever
    // they leave the meter. The next anchored resize, fitting the window to
    // it, keeps its top and goes from its new height (`meter_anchor::resize`).
    if player_resize {
        state.anchored_height.store(0, std::sync::atomic::Ordering::Relaxed);
    }
    if !anchored || !platform::window::places_own_windows() {
        platform::window::set_size(&window, size);
        return;
    }
    // From the height the meter last had when anchored, not its current one:
    // a tooltip closing would otherwise move it by the tooltip's height. At
    // start, the saved height: the window opens at tauri.conf's height.
    let last = state.anchored_height.load(std::sync::atomic::Ordering::Relaxed);
    if let Some(height) = platform::meter_anchor::resize(&window, size, meter_anchor(&state), (last > 0).then_some(last)) {
        state.anchored_height.store(height, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Linux can load a hidden WebKit window. Map it only after restoring its
/// position and sizing the meter, so its initial 400x300 rectangle never flashes.
#[tauri::command]
fn main_window_ready(window: tauri::WebviewWindow) {
    // Hidden for plugins only: it stays hidden.
    if window.label() == "main" && !meter_wanted(window.app_handle()) {
        platform::window::hide_in_place(&window);
        return;
    }
    platform::window_startup::main_ready(&window);
}

/// Displays as reported by the OS, for the "Show Details on Monitor" picker.
/// Positions and sizes are physical pixels, which is what set_position and
/// set_size want for exact monitor placement.
#[tauri::command]
fn list_monitors(app: tauri::AppHandle) -> Vec<serde_json::Value> {
    let primary = app.primary_monitor().ok().flatten();
    let primary_name = primary.as_ref().and_then(|m| m.name().cloned());
    let primary_rect = primary.as_ref().map(|m| {
        let p = *m.position();
        let s = *m.size();
        (p.x, p.y, s.width as i32, s.height as i32)
    });

    let monitors = match app.available_monitors() {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };

    let mut entries: Vec<(usize, serde_json::Value)> = monitors
        .into_iter()
        .enumerate()
        .map(|(index, m)| {
            let pos = m.position();
            let size = m.size();
            let name = m.name().cloned().unwrap_or_else(|| format!("Display {}", index + 1));
            let is_primary = primary_name.as_ref() == Some(&name);
            // Where this screen sits relative to the primary, so the picker can
            // say "right" / "above" instead of only a resolution — a resolution
            // alone does not tell you which physical monitor you just chose.
            let side = match primary_rect {
                _ if is_primary => "",
                Some((px, py, pw, ph)) => {
                    let (x, y, w, h) = (pos.x, pos.y, size.width as i32, size.height as i32);
                    if x >= px + pw { "right" }
                    else if x + w <= px { "left" }
                    else if y >= py + ph { "below" }
                    else if y + h <= py { "above" }
                    else { "" }
                }
                None => "",
            };
            (
                index,
                serde_json::json!({
                    // Index into available_monitors — this is what gets saved and
                    // passed back to open_details_window, so it must stay stable
                    // regardless of the display order below.
                    "index": index,
                    "name": name,
                    "x": pos.x,
                    "y": pos.y,
                    "width": size.width,
                    "height": size.height,
                    "scaleFactor": m.scale_factor(),
                    "isPrimary": is_primary,
                    "side": side,
                }),
            )
        })
        .collect();

    // Present the primary first so the picker's "1" is the screen the game is
    // on and "2" is the other one. The OS order is not dependable: on a
    // two-screen setup here it reported the secondary display first, which made
    // "Monitor 2" select the primary.
    entries.sort_by_key(|(index, v)| {
        let primary = v.get("isPrimary").and_then(|p| p.as_bool()).unwrap_or(false);
        (!primary, *index)
    });
    entries.into_iter().map(|(_, v)| v).collect()
}

/// Open (or move) the always-on Details window, filling the chosen monitor.
/// Frameless to match the overlay; the in-page header carries the close button.
///
/// `async` is load-bearing — see the note on `open_settings_window`.
#[tauri::command]
async fn open_details_window(app: tauri::AppHandle, monitor_index: usize) -> Result<(), String> {
    open_details_on_monitor_inner(&app, monitor_index, true)
}

fn open_details_on_monitor(app: &tauri::AppHandle, monitor_index: usize) -> Result<(), String> {
    open_details_on_monitor_inner(app, monitor_index, false)
}

/// `force_place` = the user just picked this monitor, so ignore any remembered
/// position and fill that screen.
fn open_details_on_monitor_inner(
    app: &tauri::AppHandle,
    monitor_index: usize,
    force_place: bool,
) -> Result<(), String> {
    let monitors = app.available_monitors().map_err(|e| e.to_string())?;
    if monitors.is_empty() {
        return Err("no monitors reported".into());
    }
    let monitor = monitors
        .get(monitor_index)
        .ok_or_else(|| format!("monitor {} is not connected", monitor_index))?;
    DETAILS_MONITOR.store(monitor_index, std::sync::atomic::Ordering::Relaxed);

    // available_monitors reports PHYSICAL pixels, but WebviewWindowBuilder's
    // position()/inner_size() take LOGICAL pixels. Convert, or on a scaled
    // display the window lands in the wrong place at the wrong size.
    let scale = monitor.scale_factor();
    let pos = *monitor.position();
    let size = *monitor.size();
    let lx = pos.x as f64 / scale;
    let ly = pos.y as f64 / scale;
    let lw = size.width as f64 / scale;
    let lh = size.height as f64 / scale;

    tracing::info!(
        "details window -> monitor {} '{}' physical {}x{} at {},{} (scale {}) => logical {}x{} at {},{}",
        monitor_index,
        monitor.name().cloned().unwrap_or_default(),
        size.width, size.height, pos.x, pos.y, scale, lw, lh, lx, ly
    );

    if let Some(existing) = app.get_webview_window("details") {
        // Already open — move it only when the user explicitly picked a monitor.
        if force_place {
            let _ = existing.unmaximize();
            let _ = existing.set_position(tauri::Position::Physical(pos));
            platform::window::set_size(&existing, tauri::Size::Physical(size));
        }
        let _ = existing.show();
        let _ = existing.unminimize();
        let _ = existing.set_focus();
        announce_details_placement(app, monitor_index);
        return Ok(());
    }

    // Born at the target coordinates rather than created-then-moved. Moving a
    // hidden window and calling maximize() put it on whichever monitor Windows
    // still considered current, which is how Details kept opening on the same
    // screen as the overlay.
    let window = platform::window::share_process(tauri::WebviewWindowBuilder::new(
        app,
        "details",
        tauri::WebviewUrl::App("index.html".into()),
    ), platform::ProcessGroup::Meter)
    .initialization_script("window.__A2_VIEW__ = 'details';")
    .title("A2Tools DPS Meter — Details")
    .decorations(false)
    .transparent(false)
    // Intentional: the point of this window is to stay readable on a second
    // monitor without being buried by whatever else is on that screen.
    .always_on_top(true)
    .resizable(true)
    .skip_taskbar(false)
    .position(lx, ly)
    .inner_size(lw, lh)
    // Visible from the start. Creating it hidden and having the page reveal
    // itself deadlocked: a hidden WebView2 window may never load its content,
    // so the reveal never ran. The background colour below covers the load so
    // there is no white flash.
    .background_color(tauri::window::Color(10, 14, 22, 255))
    .build()
    .map_err(|e| e.to_string())?;
    platform::window::joined(&window, platform::ProcessGroup::Meter);
    platform::window::set_size(&window, tauri::Size::Logical(tauri::LogicalSize { width: lw, height: lh }));

    // An explicit monitor pick always wins; otherwise fall back to wherever the
    // user last dragged the window.
    if force_place || !restore_window_geometry(app, &window, "details") {
        // Re-assert in physical units: the builder's logical values round on
        // fractional-scale displays.
        let _ = window.set_position(tauri::Position::Physical(pos));
        platform::window::set_size(&window, tauri::Size::Physical(size));
    }

    // Announced once the window reports ready (see details_window_ready); a
    // freshly built webview has no listener attached yet.
    // Safety net. Unconditional: is_visible() does not reliably reflect whether
    // the window was actually mapped, so guarding on it left the window created
    // but never revealed. show() on an already-visible window is a no-op, so the
    // worst case here is a redundant call.
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        if let Some(w) = handle.get_webview_window("details") {
            let _ = w.show();
            let _ = w.set_focus();
            tracing::info!("details window revealed (safety net)");
        }
    });
    Ok(())
}



/// The monitor the user has pinned Details to, or `None` when the setting is
/// off. Read from settings on every use rather than from `DETAILS_MONITOR`:
/// that atomic is session-sticky and never cleared, so once a monitor had been
/// picked, Details kept landing on that screen for the rest of the run even
/// after the user set the dropdown back to Off.
fn details_monitor_setting(app: &tauri::AppHandle) -> Option<usize> {
    let state = app.try_state::<AppState>()?;
    let raw = state.settings.get("dpsMeter.detailsMonitor")?;
    let raw = raw.trim();
    if raw.is_empty() || raw == "off" {
        return None;
    }
    raw.parse::<usize>().ok()
}

/// Whether the Details window's saved rect is somewhere the *user* put it.
///
/// Geometry recorded while Details was pinned to a monitor is the setting's
/// placement, not a choice — and it used to be saved anyway, so turning the
/// setting off left Details reopening on that same screen. The saver now stamps
/// this marker only when it records an unpinned window, so geometry written
/// under the old behaviour has no marker and is ignored exactly once. After
/// that the window remembers wherever the user drags it, including onto a
/// second screen deliberately.
fn details_geometry_is_user_placed(app: &tauri::AppHandle) -> bool {
    app.try_state::<AppState>()
        .and_then(|state| state.settings.get(DETAILS_USER_PLACED_KEY))
        .map(|v| v.trim() == "true")
        .unwrap_or(false)
}

const DETAILS_USER_PLACED_KEY: &str = "window.details.userPlaced";

/// The windows that float apart from the overlay and reopen where they were
/// left. Per-fight windows (details-*) are deliberately absent: they are
/// opened to be read and closed, and keying geometry by fight id would grow
/// settings without bound.
const TOOL_WINDOWS: [&str; 3] = ["details", "settings", "history"];

/// Settings, History and Details are on top of the overlay (always on top
/// itself), or they could not be reached over it while the game is in front.
/// But only while the meter or the game is: on top of a browser the player
/// switched to, Settings stayed in the way (2026-10-18). Focused, a tool
/// window is on top; when it loses the front to another program, it goes
/// back to an ordinary window until it is clicked again. Asked a moment
/// after the focus moves, when the new front window is known.
fn keep_on_top_while_ours(app: &tauri::AppHandle, label: &str, focused: bool) {
    let Some(window) = app.get_webview_window(label) else { return };
    if focused {
        let _ = window.set_always_on_top(true);
        return;
    }
    let app = app.clone();
    let label = label.to_string();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(150));
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            let Some(window) = handle.get_webview_window(&label) else { return };
            if !window.is_focused().unwrap_or(false) && !platform::window::front_is_ours_or_game() {
                let _ = window.set_always_on_top(false);
            }
        });
    });
}

/// A tool window `bring_forward` raised, without focus, stays on top only
/// while the meter or the game is in front, as `keep_on_top_while_ours` keeps
/// a focused one: that one is unpinned when it loses focus, which a window
/// raised without it never does, so it floated over the app clicked next.
/// Watched until another app takes the front (unpinned then), the window
/// takes focus (its own handler from then), it closes, or 30 minutes pass.
fn unpin_when_left(app: &tauri::AppHandle, label: &str) {
    let app = app.clone();
    let label = label.to_string();
    std::thread::spawn(move || {
        let mut ours = platform::window::front_is_ours_or_game();
        for _ in 0..(30 * 60 * 4) {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let Some(window) = app.get_webview_window(&label) else { return };
            if window.is_focused().unwrap_or(false) {
                return;
            }
            let now = platform::window::front_is_ours_or_game();
            if ours && !now {
                let handle = app.clone();
                let label = label.clone();
                let _ = app.run_on_main_thread(move || {
                    if let Some(window) = handle.get_webview_window(&label) {
                        let _ = window.set_always_on_top(false);
                    }
                });
                return;
            }
            ours = now;
        }
    });
}

/// Remember where tool window `label` is and its size. Called on every move
/// and resize, and every 5 s as before: with only the timer, a window moved
/// and closed within 5 s reopened where it had been (Battle History,
/// 2026-10-18).
fn save_tool_window_geometry(handle: &tauri::AppHandle, label: &str) {
    save_window_geometry(handle, label, label);
}

/// The settings key the fight windows (`details-<fight id>`) share: one
/// place and size for all of them, where the last one was left, rather than
/// one per fight, which never reopened anywhere but the middle (2026-10-18).
const FIGHT_WINDOW_KEY: &str = "fight";

/// Remember window `label` as `key` (see `save_tool_window_geometry`).
fn save_window_geometry(handle: &tauri::AppHandle, label: &str, key: &str) {
    let Some(state) = handle.try_state::<AppState>() else { return };
    // While Details is pinned to a monitor its rect comes from the setting,
    // not from the user. Saving it poisons the windowed geometry: turn the
    // setting off and Details would reopen full-size on that same screen.
    if label == "details" && details_monitor_setting(handle).is_some() {
        return;
    }
    let Some(w) = handle.get_webview_window(label) else { return };
    if !w.is_visible().unwrap_or(false) || w.is_minimized().unwrap_or(false) {
        return;
    }
    if let Ok(pos) = w.outer_position() {
        if pos.x > -10000 && pos.y > -10000 {
            state.settings.set(&format!("window.{}.x", key), &pos.x.to_string());
            state.settings.set(&format!("window.{}.y", key), &pos.y.to_string());
        }
    }
    // Inner, not outer: restore applies it with set_size, which sets the
    // inner size. Saving the outer size grew every tool window by its border
    // (16x9 px here) on each reopen.
    if let Ok(size) = w.inner_size() {
        if size.width > 100 && size.height > 100 {
            state.settings.set(&format!("window.{}.w", key), &size.width.to_string());
            state.settings.set(&format!("window.{}.h", key), &size.height.to_string());
        }
    }
    // Details is unpinned here, but the window may still be sitting on the
    // fill rect from before the setting was switched off. A rect that
    // swallows a whole screen is not a placement anyone chose by dragging, so
    // it never earns the marker.
    if label == "details" {
        let filling = match (w.outer_position(), w.outer_size()) {
            (Ok(pos), Ok(size)) => rect_covers_a_monitor(handle, pos, size),
            _ => true,
        };
        if !filling {
            state.settings.set(DETAILS_USER_PLACED_KEY, "true");
        }
    }
}

/// Whether a window rect swallows a whole monitor — the shape of a
/// monitor-filling placement rather than somewhere the user dragged a window.
fn rect_covers_a_monitor(
    app: &tauri::AppHandle,
    pos: tauri::PhysicalPosition<i32>,
    size: tauri::PhysicalSize<u32>,
) -> bool {
    app.available_monitors()
        .map(|monitors| {
            monitors.iter().any(|m| {
                let p = *m.position();
                let s = *m.size();
                pos.x <= p.x
                    && pos.y <= p.y
                    && pos.x + size.width as i32 >= p.x + s.width as i32
                    && pos.y + size.height as i32 >= p.y + s.height as i32
            })
        })
        .unwrap_or(false)
}

/// Centre a tool window on whichever screen the overlay is on — where the user
/// is actually playing — rather than on whatever Windows considers current.
fn center_on_overlay_monitor(app: &tauri::AppHandle, window: &tauri::WebviewWindow) {
    if let Some(main) = app.get_webview_window("main") {
        if let (Ok(Some(monitor)), Ok(size)) = (main.current_monitor(), window.outer_size()) {
            let p = *monitor.position();
            let s = *monitor.size();
            let x = p.x + ((s.width as i32 - size.width as i32) / 2).max(0);
            let y = p.y + ((s.height as i32 - size.height as i32) / 2).max(0);
            let _ = window.set_position(tauri::Position::Physical(tauri::PhysicalPosition { x, y }));
            return;
        }
    }
    let _ = window.center();
}

/// Where a tool window should be born, in logical units for its builder: the
/// spot `label`'s window was left at, and its size unless `keep_size` is
/// false; else (or with no `label`) centred on the overlay's screen at `size`.
/// Built at the default spot and moved after `build()` returned, it showed
/// there first and then jumped: on Windows `build()` waits for WebView2, so
/// the jump was plain to see (Settings, 2026-10-18). `restore_window_geometry` still runs after, to set
/// the physical pixels exactly; the window is already there.
fn birth_place(app: &tauri::AppHandle, label: Option<&str>, size: (f64, f64), keep_size: bool) -> (f64, f64, f64, f64) {
    let saved = |k: &str| {
        let label = label?;
        app.try_state::<AppState>()
            .and_then(|s| s.settings.get(&format!("window.{}.{}", label, k)))
            .and_then(|v| v.trim().parse::<i32>().ok())
    };
    let monitors = app.available_monitors().unwrap_or_default();
    if let (Some(x), Some(y)) = (saved("x"), saved("y")) {
        let screen = monitors.iter().find(|m| {
            let p = *m.position();
            let s = *m.size();
            x >= p.x - 64 && x < p.x + s.width as i32 && y >= p.y - 64 && y < p.y + s.height as i32
        });
        if let Some(m) = screen {
            let scale = m.scale_factor();
            let (mut w, mut h) = size;
            if keep_size {
                if let (Some(sw), Some(sh)) = (saved("w"), saved("h")) {
                    if sw > 200 && sh > 150 {
                        (w, h) = (sw as f64 / scale, sh as f64 / scale);
                    }
                }
            }
            return (x as f64 / scale, y as f64 / scale, w, h);
        }
    }
    let monitor = app
        .get_webview_window("main")
        .and_then(|main| main.current_monitor().ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());
    match monitor {
        Some(m) => {
            let scale = m.scale_factor();
            let (p, s) = (m.position().to_logical::<f64>(scale), m.size().to_logical::<f64>(scale));
            (p.x + ((s.width - size.0) / 2.0).max(0.0), p.y + ((s.height - size.1) / 2.0).max(0.0), size.0, size.1)
        }
        None => (100.0, 100.0, size.0, size.1),
    }
}

/// Restore a tool window's remembered geometry. Returns true if anything was
/// applied, so callers know whether they still need to place it themselves.
fn restore_window_geometry(app: &tauri::AppHandle, window: &tauri::WebviewWindow, label: &str) -> bool {
    let Some(state) = app.try_state::<AppState>() else { return false };
    let get = |k: &str| state.settings.get(&format!("window.{}.{}", label, k))
        .and_then(|v| v.trim().parse::<i32>().ok());
    let (Some(x), Some(y)) = (get("x"), get("y")) else { return false };
    if x <= -10000 || y <= -10000 {
        return false;
    }
    // Only restore onto a screen that still exists — an unplugged monitor would
    // otherwise strand the window off-desktop.
    let on_screen = app.available_monitors().map(|ms| {
        ms.iter().any(|m| {
            let p = *m.position();
            let s = *m.size();
            x >= p.x - 64 && x < p.x + s.width as i32 && y >= p.y - 64 && y < p.y + s.height as i32
        })
    }).unwrap_or(false);
    if !on_screen {
        tracing::info!("{} window: saved position {},{} is off-desktop; ignoring", label, x, y);
        return false;
    }
    let _ = window.set_position(tauri::Position::Physical(tauri::PhysicalPosition { x, y }));
    // Settings is a fixed list of options: it reopens where it was left, at its
    // own size. A remembered size only ever made it sprawl (see the inner-size
    // note where geometry is saved), and it can still be resized while open.
    if label == "settings" {
        return true;
    }
    if let (Some(w), Some(h)) = (get("w"), get("h")) {
        if w > 200 && h > 150 {
            platform::window::set_size(window, tauri::Size::Physical(tauri::PhysicalSize {
                width: w as u32,
                height: h as u32,
            }));
        }
    }
    true
}

/// The Settings window. Free-floating like Details — it used to be a panel that
/// forced the overlay to resize itself to ~820px tall.
///
/// **This command must stay `async`.** Tauri runs synchronous commands on the
/// main thread, and on Windows `WebviewWindowBuilder::build()` deadlocks there:
/// WebView2 needs the main thread's message loop to deliver the
/// `CreateCoreWebView2Controller` callback, which cannot run while `build()` is
/// blocking that same loop. The window still appeared — sized, and painting its
/// background colour — but its WebView2 host stayed 0x0 and hidden, and the page
/// never left `about:blank`. That is the "blank tool window". Marshalling
/// through `run_on_main_thread` makes it worse, not better. An `async` command
/// runs off the main thread, so the loop stays free to complete the callback.
/// See <https://docs.rs/tauri/latest/tauri/webview/struct.WebviewWindowBuilder.html>.
/// The Settings section a window opened on its behalf is to show first
/// (`open_settings_at`), until its page asks (`take_settings_section`).
fn pending_settings_section() -> &'static Mutex<Option<String>> {
    static PENDING: std::sync::OnceLock<Mutex<Option<String>>> = std::sync::OnceLock::new();
    PENDING.get_or_init(Mutex::default)
}

/// Open Settings on one section (the plugin dock's button: Plugins). An
/// open Settings window switches to it; a new one shows it first.
#[tauri::command]
async fn open_settings_at(app: tauri::AppHandle, section: String) -> Result<(), String> {
    if section.is_empty() || section.len() > 24 || !section.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
        return Err("not a section".into());
    }
    if app.get_webview_window("settings").is_some() {
        let _ = app.emit_to("settings", "settings-section", &section);
    } else {
        *pending_settings_section().lock() = Some(section);
    }
    open_settings_window(app).await
}

/// The section a new Settings window opens on, once (`open_settings_at`).
#[tauri::command]
fn take_settings_section() -> Option<String> {
    pending_settings_section().lock().take()
}

#[tauri::command]
pub(crate) async fn open_settings_window(app: tauri::AppHandle) -> Result<(), String> {
    let ready = platform::window_startup::request_settings_open();
    if let Some(existing) = app.get_webview_window("settings") {
        if !ready {
            return Ok(());
        }
        // In front of everything, even where focus is refused (Windows, from
        // the overlay), then asked for the keyboard for its fields.
        platform::window::bring_forward(&existing);
        unpin_when_left(&app, "settings");
        let _ = existing.set_focus();
        // Already open (perhaps behind the game): check the account again.
        // A hidden one also resumes its form, without repeating page startup.
        let _ = app.emit_to("settings", "settings-shown", ());
        return Ok(());
    }
    build_settings_window(&app)
}

/// Runs in the Settings window before the page. Besides naming the view, it
/// answers Quit, Close and Escape the moment they are on screen. They used to
/// be wired when the page's scripts had run (about 800 KB, lucide first), so
/// on every open both buttons sat dead for a while: the window is rebuilt each
/// time it opens (see `close_settings_window`). A listener on `document` is
/// there before the buttons are, and catches clicks on them as they appear.
const SETTINGS_WINDOW_SCRIPT: &str = r#"
window.__A2_VIEW__ = 'settings';
(function () {
  const call = (cmd) => {
    if (cmd === 'close_settings_window') window.dispatchEvent(new Event('settings-hidden'));
    window.__TAURI_INTERNALS__.invoke(cmd).catch(() => {});
  };
  document.addEventListener('click', (event) => {
    const el = event.target instanceof Element ? event.target : null;
    if (el?.closest('.quitButton')) {
      event.stopPropagation();
      call('quit_app');
    } else if (el?.closest('.settingsClose')) {
      event.stopPropagation();
      call('close_settings_window');
    }
  }, true);
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') call('close_settings_window');
  }, true);
})();
"#;

fn build_settings_window(app: &tauri::AppHandle) -> Result<(), String> {
    platform::window_startup::begin_settings_load();
    let (x, y, _, _) = birth_place(app, Some("settings"), (760.0, 820.0), false);
    let window = platform::window::share_process(tauri::WebviewWindowBuilder::new(
        app,
        "settings",
        tauri::WebviewUrl::App("index.html".into()),
    ), platform::ProcessGroup::Meter)
    // Injected before any page script. WebviewUrl::App is a path, so a ?query
    // gets percent-encoded — this is the one channel that is reliable.
    .initialization_script(SETTINGS_WINDOW_SCRIPT)
    .title("A2Tools DPS Meter — Settings")
    .decorations(false)
    .transparent(false)
    // Matches Details: the overlay itself is always-on-top, so a settings window
    // that could fall behind it would be unreachable while the game is focused.
    .always_on_top(true)
    .resizable(true)
    .skip_taskbar(false)
    .position(x, y)
    .inner_size(760.0, 820.0)
    .min_inner_size(520.0, 420.0)
    // Built visible on Windows, with the app's background colour to cover the
    // load rather than flashing white. Building it hidden is not an option
    // there: a hidden WebView2 window may never load its content, so a
    // page-driven reveal deadlocks. Linux WebKit loads hidden, and the page
    // reveals it once the form is translated and wired.
    .background_color(tauri::window::Color(10, 14, 22, 255))
    .visible(!platform::window_startup::loads_hidden())
    .build()
    .map_err(|e| e.to_string())?;
    platform::window::joined(&window, platform::ProcessGroup::Meter);
    platform::window_startup::prepare_settings(&window);
    platform::window::set_size(&window, tauri::Size::Logical(tauri::LogicalSize { width: 760.0, height: 820.0 }));

    if !restore_window_geometry(app, &window, "settings") {
        platform::window_startup::center_before_show(app, &window,
            tauri::LogicalSize { width: 760.0, height: 820.0 });
    }
    Ok(())
}

#[tauri::command]
fn close_settings_window(app: tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("settings") {
        if platform::window_startup::reuses_settings() {
            // The page already announced `settings-hidden`.
            platform::window_startup::hide_settings(&window);
        } else {
            // Closed, not hidden: a hidden WebView2 window came back blank when
            // shown again. Rebuilding it costs little now that Quit and Close
            // are answered before the page loads (SETTINGS_WINDOW_SCRIPT) and
            // the account line starts from the last check.
            let _ = window.close();
        }
    }
}

/// Shown when the frontend of a tool window has painted. Answers whether it
/// was: a Settings window closed before it was ready stays hidden.
#[tauri::command]
fn tool_window_ready(app: tauri::AppHandle, label: String) -> bool {
    if label == "settings" && !platform::window_startup::settings_ready() {
        return false;
    }
    let Some(window) = app.get_webview_window(&label) else { return false };
    let _ = window.show();
    let _ = window.set_focus();
    // Resizable from the outer edge too (Linux), from the first time it shows.
    static EDGES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let mut hooked = EDGES.lock().unwrap_or_else(|p| p.into_inner());
    if !hooked.contains(&label) {
        hooked.push(label.clone());
        let min = if label == "settings" { (520.0, 420.0) } else if label == "history" { (480.0, 360.0) } else { (520.0, 360.0) };
        platform::window::resizable_edges(&window, tauri::LogicalSize::new(min.0, min.1));
    }
    true
}

/// Tell the Details window which screen it just landed on, so it can confirm
/// visually. A dropdown label alone does not prove the right monitor was picked.
fn announce_details_placement(app: &tauri::AppHandle, monitor_index: usize) {
    let monitors = match app.available_monitors() {
        Ok(m) => m,
        Err(_) => return,
    };
    let Some(monitor) = monitors.get(monitor_index) else { return };
    let primary = app.primary_monitor().ok().flatten();
    let primary_name = primary.as_ref().and_then(|m| m.name().cloned());
    let name = monitor.name().cloned().unwrap_or_default();
    let is_primary = primary_name.as_ref() == Some(&name);

    // Position in the primary-first ordering the picker shows.
    let mut ordered: Vec<(bool, usize)> = monitors
        .iter()
        .enumerate()
        .map(|(i, m)| (m.name().cloned() == primary_name, i))
        .collect();
    ordered.sort_by_key(|(is_p, i)| (!*is_p, *i));
    let position = ordered
        .iter()
        .position(|(_, i)| *i == monitor_index)
        .unwrap_or(monitor_index);

    let size = *monitor.size();
    let _ = app.emit_to(
        "details",
        "details-placed",
        serde_json::json!({
            "number": position + 1,
            "width": size.width,
            "height": size.height,
            "isPrimary": is_primary,
        }),
    );
}

/// Called by a Details-family window once its panel has painted. Reveals the
/// calling window rather than a fixed label, since there can now be several.
#[tauri::command]
fn details_window_ready(app: tauri::AppHandle, window: tauri::Window) {
    let _ = window.show();
    tracing::info!("{} window revealed (frontend ready)", window.label());
    // Only the monitor-pinned singleton has a placement to confirm; a fight
    // window is placed by cascade and the History window by its own geometry.
    if window.label() == "details" {
        let index = DETAILS_MONITOR.load(std::sync::atomic::Ordering::Relaxed);
        if index != usize::MAX {
            announce_details_placement(&app, index);
        }
    }
}

#[tauri::command]
fn close_details_window(app: tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("details") {
        let _ = window.close();
    }
}

/// Window label for a saved fight. Each fight gets its own window so several can
/// be compared side by side, so the id has to survive as a label — sanitised,
/// because labels are also used to build the webview's internal identifiers.
fn fight_window_label(fight_id: &str) -> String {
    let safe: String = fight_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("details-{}", safe)
}

/// Ask a Details surface to show something. Which window serves the request
/// depends on what is being asked for:
///
/// - `history` → the one persistent History window. It is a browser you leave
///   open, so it never closes just because you opened something from it.
/// - `fight`   → a window of its own, `details-<id>`, so fights can be compared
///   side by side. Asking for a fight that is already open raises that window
///   rather than opening a second copy of it.
/// - anything else (a meter row) → the single live `details` window, re-targeted
///   in place. Live rows are clicked constantly during combat; spawning a window
///   per click would bury the game.
///
/// If the target window is up the request is pushed straight to it. If not, the
/// request is parked under that window's label and the window pulls it on
/// startup — a webview that was created to serve a request has no listener
/// attached at the moment the request is emitted.
///
/// `async` is load-bearing — it creates windows. See `open_settings_window`.
#[tauri::command]
async fn request_details_view(
    app: tauri::AppHandle,
    payload: serde_json::Value,
) -> Result<(), String> {
    let seq = DETAILS_REQUEST_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let mut payload = payload;
    if !payload.is_object() {
        payload = serde_json::json!({});
    }
    let kind = payload
        .get("kind")
        .and_then(|k| k.as_str())
        .unwrap_or("row")
        .to_string();
    let fight_id = payload
        .get("fightId")
        .and_then(|f| f.as_str())
        .unwrap_or("")
        .to_string();
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("seq".into(), serde_json::json!(seq));
    }

    let label = match kind.as_str() {
        "history" => "history".to_string(),
        "fight" if !fight_id.is_empty() => fight_window_label(&fight_id),
        _ => "details".to_string(),
    };

    if let Some(window) = app.get_webview_window(&label) {
        // Live window: nothing to park, the listener is already attached.
        if let Ok(mut pending) = PENDING_DETAILS_REQUEST.lock() {
            pending.remove(&label);
        }
        // Raise it if it was put away. One already on screen but behind
        // other windows is brought in front without taking focus: the click
        // came from an overlay on top of a full-screen game, and pulling focus
        // would tab out of it (and Windows refuses it anyway: the overlay is
        // never the program in front).
        let hidden = !window.is_visible().unwrap_or(true)
            || window.is_minimized().unwrap_or(false);
        if hidden {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        } else {
            platform::window::bring_forward(&window);
            unpin_when_left(&app, &label);
        }
        app.emit_to(&label, "details-request", payload)
            .map_err(|e| e.to_string())?;
        return Ok(());
    }

    if let Ok(mut pending) = PENDING_DETAILS_REQUEST.lock() {
        pending.insert(label.clone(), payload);
    }

    let result = match kind.as_str() {
        "history" => open_history_window_inner(&app),
        "fight" if !fight_id.is_empty() => open_fight_window(&app, &label),
        // The setting decides, every time. Reading DETAILS_MONITOR here is what
        // made "Show Details on monitor: Off" do nothing once a monitor had
        // been picked earlier in the session.
        _ => match details_monitor_setting(&app) {
            Some(index) => open_details_on_monitor(&app, index),
            None => {
                // Forget the earlier pick too, so the placement badge does not
                // announce a monitor this window is no longer tied to.
                DETAILS_MONITOR.store(usize::MAX, std::sync::atomic::Ordering::Relaxed);
                open_details_windowed(&app)
            }
        },
    };
    if result.is_err() {
        // Nothing will ever pull it, and a stale request must not resurface
        // against some later window that happens to take the same label.
        if let Ok(mut pending) = PENDING_DETAILS_REQUEST.lock() {
            pending.remove(&label);
        }
    }
    result
}

/// One window per saved fight, cascaded so a second fight does not land exactly
/// on top of the first. These are deliberately not remembered: they are opened
/// to be read and closed, and persisting geometry per fight id would accumulate
/// settings without bound.
fn open_fight_window(app: &tauri::AppHandle, label: &str) -> Result<(), String> {
    // Where the last fight window was left, at its size; cascaded only past
    // fight windows already open. Born there, not moved after `build()`: it
    // showed at the default spot first (see birth_place).
    let step = app.webview_windows().keys().filter(|l| l.starts_with("details-")).count() as f64;
    let offset = (step % 6.0) * 34.0;
    let (x, y, w, h) = birth_place(app, Some(FIGHT_WINDOW_KEY), (1180.0, 760.0), true);

    let window = platform::window::share_process(tauri::WebviewWindowBuilder::new(
        app,
        label,
        tauri::WebviewUrl::App("index.html".into()),
    ), platform::ProcessGroup::Meter)
    .initialization_script("window.__A2_VIEW__ = 'details';")
    .title("A2Tools DPS Meter — Fight")
    .decorations(false)
    .transparent(false)
    .always_on_top(true)
    .resizable(true)
    .skip_taskbar(false)
    .position(x + offset, y + offset)
    .inner_size(w, h)
    .min_inner_size(520.0, 360.0)
    .background_color(tauri::window::Color(10, 14, 22, 255))
    .build()
    .map_err(|e| e.to_string())?;
    platform::window::joined(&window, platform::ProcessGroup::Meter);
    platform::window::set_size(&window, tauri::Size::Logical(tauri::LogicalSize { width: w, height: h }));
    Ok(())
}

/// The History window. Persistent by design — it is the browser you pick fights
/// from, and it stays put while those fights open in windows of their own.
fn open_history_window_inner(app: &tauri::AppHandle) -> Result<(), String> {
    let (x, y, w, h) = birth_place(app, Some("history"), (1100.0, 720.0), true);
    let window = platform::window::share_process(tauri::WebviewWindowBuilder::new(
        app,
        "history",
        tauri::WebviewUrl::App("index.html".into()),
    ), platform::ProcessGroup::Meter)
    .initialization_script("window.__A2_VIEW__ = 'history';")
    .title("A2Tools DPS Meter — Battle History")
    .decorations(false)
    .transparent(false)
    .always_on_top(true)
    .resizable(true)
    .skip_taskbar(false)
    .position(x, y)
    .inner_size(w, h)
    .min_inner_size(480.0, 360.0)
    .background_color(tauri::window::Color(10, 14, 22, 255))
    .build()
    .map_err(|e| e.to_string())?;
    platform::window::joined(&window, platform::ProcessGroup::Meter);
    platform::window::set_size(&window, tauri::Size::Logical(tauri::LogicalSize { width: w, height: h }));

    if !restore_window_geometry(app, &window, "history") {
        center_on_overlay_monitor(app, &window);
    }
    Ok(())
}

/// Close whichever tool window asked. Fight windows are frameless and there can
/// be several, so each closes itself rather than the overlay guessing which.
#[tauri::command]
fn close_tool_window(window: tauri::Window) {
    let _ = window.close();
}

/// Create the Details window without claiming a whole screen. Used when the
/// user has never picked a monitor in Settings: clicking a meter row should
/// give them a window they can move, not black out a display over the game.
/// A remembered position still wins — this is only the first-run geometry.
fn open_details_windowed(app: &tauri::AppHandle) -> Result<(), String> {
    let placed = details_geometry_is_user_placed(app).then_some("details");
    let (x, y, w, h) = birth_place(app, placed, (1180.0, 760.0), true);
    let window = platform::window::share_process(tauri::WebviewWindowBuilder::new(
        app,
        "details",
        tauri::WebviewUrl::App("index.html".into()),
    ), platform::ProcessGroup::Meter)
    .initialization_script("window.__A2_VIEW__ = 'details';")
    .title("A2Tools DPS Meter — Details")
    .decorations(false)
    .transparent(false)
    .always_on_top(true)
    .resizable(true)
    .skip_taskbar(false)
    .position(x, y)
    .inner_size(w, h)
    .min_inner_size(520.0, 360.0)
    // Visible, with the app background painted behind the load — same reason as
    // the monitor-filling path: a hidden WebView2 window may never load at all.
    .background_color(tauri::window::Color(10, 14, 22, 255))
    .build()
    .map_err(|e| e.to_string())?;
    platform::window::joined(&window, platform::ProcessGroup::Meter);
    platform::window::set_size(&window, tauri::Size::Logical(tauri::LogicalSize { width: w, height: h }));

    // A remembered position still wins, but only one the user actually chose.
    if !details_geometry_is_user_placed(app)
        || !restore_window_geometry(app, &window, "details")
    {
        center_on_overlay_monitor(app, &window);
    }
    Ok(())
}

/// Pulled by a tool window once its listener is attached. The label comes from
/// the calling window rather than an argument, so a window can only ever claim
/// its own request. Clearing on read keeps a stale one from resurfacing.
#[tauri::command]
fn take_pending_details_request(window: tauri::Window) -> Option<serde_json::Value> {
    PENDING_DETAILS_REQUEST
        .lock()
        .ok()
        .and_then(|mut pending| pending.remove(window.label()))
}

/// What a screenshot achieved: on the clipboard, and the file it was saved to.
#[derive(serde::Serialize)]
struct ScreenshotResult {
    clipboard: bool,
    file: Option<String>,
}

/// Capture part of the calling window: `x`/`y`/`width`/`height` are the page's
/// CSS pixels and `scale` its `devicePixelRatio`. Measured against the window
/// that asked, so the Details window captures itself rather than whatever sits
/// at the same offset from the meter. `include_meter` adds the whole meter
/// window, for a tool window that cannot measure the meter itself. With
/// `save_file`, also writes a PNG to `folder` (default: Pictures\A2Tools DPS
/// Meter) named `filename`.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn capture_screenshot(
    app: tauri::AppHandle,
    webview_window: tauri::WebviewWindow,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    scale: Option<f64>,
    include_meter: Option<bool>,
    save_file: Option<bool>,
    folder: Option<String>,
    filename: Option<String>,
) -> ScreenshotResult {
    let scale = scale.unwrap_or_else(|| webview_window.scale_factor().unwrap_or(1.0));
    let meter = include_meter
        .unwrap_or(false)
        .then(|| app.get_webview_window("main"))
        .flatten()
        .filter(|main| main.label() != webview_window.label());
    let path = save_file.unwrap_or(false).then(|| {
        // Only a folder the player picked in the folder dialog, else the
        // default: the page's word alone could have written anywhere
        // (security audit M7).
        let picked = picked_screenshot_folder(&app);
        let dir = folder
            .filter(|f| !f.trim().is_empty() && picked.as_deref() == Some(f.as_str()))
            .map(std::path::PathBuf::from)
            .or_else(platform::screenshot::default_folder)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let name = filename
            .filter(|n| !n.trim().is_empty() && !n.contains(['/', '\\', ':']) && crate::history::fight_history::is_plain_name(n))
            .map(|n| if n.to_ascii_lowercase().ends_with(".png") { n } else { format!("{n}.png") })
            .unwrap_or_else(|| format!("AION2_DPS_{}.png", chrono::Local::now().format("%Y%m%d_%H%M%S")));
        dir.join(name)
    });
    tauri::async_runtime::spawn_blocking(move || {
        let (clipboard, file_ok) = platform::screenshot::capture(
            &webview_window, x, y, width, height, scale, meter.as_ref(), path.as_deref(),
        );
        if path.is_some() && !file_ok {
            tracing::warn!("Screenshot not saved to {:?}", path);
        }
        ScreenshotResult {
            clipboard,
            file: path.filter(|_| file_ok).map(|p| p.display().to_string()),
        }
    })
    .await
    .unwrap_or(ScreenshotResult { clipboard: false, file: None })
}

/// Where screenshots go: the folder the player picked, else the default.
pub(crate) fn screenshot_dir(app: &tauri::AppHandle) -> std::path::PathBuf {
    picked_screenshot_folder(app)
        .map(std::path::PathBuf::from)
        .or_else(platform::screenshot::default_folder)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// Where screenshots go when no folder has been chosen.
#[tauri::command]
fn default_screenshot_folder() -> String {
    platform::screenshot::default_folder()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

/// The screenshot folder the player picked in the folder dialog, kept by the
/// meter itself (`update_settings` won't write it). A folder chosen before
/// 2.0.80 was kept only by the page: taken over once, the first time.
const PICKED_SCREENSHOT_FOLDER_KEY: &str = "dpsMeter.screenshotFolderPicked";
const PAGE_SCREENSHOT_FOLDER_KEY: &str = "dpsMeter.detailsScreenshotFolder";

fn picked_screenshot_folder(app: &tauri::AppHandle) -> Option<String> {
    let state = app.try_state::<AppState>()?;
    if let Some(picked) = state.settings.get(PICKED_SCREENSHOT_FOLDER_KEY) {
        return Some(picked).filter(|p| !p.is_empty());
    }
    let earlier = state.settings.get(PAGE_SCREENSHOT_FOLDER_KEY).unwrap_or_default();
    state.settings.set(PICKED_SCREENSHOT_FOLDER_KEY, &earlier);
    Some(earlier).filter(|p| !p.is_empty())
}

/// Let the player pick the screenshot folder. `None` if they cancel.
#[tauri::command]
async fn choose_screenshot_folder(
    app: tauri::AppHandle,
    webview_window: tauri::WebviewWindow,
    current: Option<String>,
) -> Option<String> {
    let picked = platform::screenshot::pick_folder(&webview_window, current.as_deref())?;
    if let Some(state) = app.try_state::<AppState>() {
        state.settings.set(PICKED_SCREENSHOT_FOLDER_KEY, &picked);
    }
    Some(picked)
}

#[tauri::command]
fn start_drag(app: tauri::AppHandle, state: tauri::State<'_, AppState>) {
    // A locked overlay stays where it is.
    if state.overlay_lock.locked.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    if let Some(window) = app.get_webview_window("main") {
        platform::window::start_drag(&window);
    }
}

/// Drag a tool window (Details, History, Settings) by its header. Their CSS
/// marks the header `-webkit-app-region: drag`, which WebView2 honours and
/// WebKitGTK does not, so on Linux the page asks for the drag instead.
#[tauri::command]
fn start_tool_drag(window: tauri::WebviewWindow) {
    if window.label() == "main" {
        return;
    }
    platform::window::start_drag(&window);
}

/// Whether this backend supports compositor-driven window resizing.
#[tauri::command]
fn compositor_resize_supported(window: tauri::WebviewWindow) -> bool {
    platform::window::compositor_resize_supported(&window)
}

/// Unpin the window before a compositor resize gesture.
#[tauri::command]
async fn begin_window_resize(
    window: tauri::WebviewWindow,
    min_width: f64,
    min_height: f64,
    scale: f64,
) -> Result<bool, String> {
    if !platform::window::compositor_resize_supported(&window) {
        return Err("Compositor resize is unavailable".into());
    }
    if ![min_width, min_height, scale].iter().all(|v| v.is_finite() && *v > 0.0) {
        return Err("Invalid resize dimensions".into());
    }
    let display_scale = window.scale_factor().map_err(|e| e.to_string())?;
    platform::window::prepare_resize(&window, tauri::LogicalSize::new(
        min_width * scale / display_scale,
        min_height * scale / display_scale,
    )).await?;
    Ok(platform::window::resize_pointer_down(&window) == Some(true))
}

#[tauri::command]
fn finish_window_resize(window: tauri::WebviewWindow, state: tauri::State<'_, AppState>, cancel: bool) -> Result<bool, String> {
    if platform::window::compositor_resize_supported(&window) {
        if !cancel && platform::window::resize_pointer_down(&window) != Some(false) {
            return Ok(false);
        }
        let size = window.inner_size().map_err(|e| e.to_string())?;
        platform::window::set_size(&window, tauri::Size::Physical(size));
        // The player sized the overlay: its edge is kept from this height, not
        // the one it had before.
        if window.label() == "main" {
            state.anchored_height.store(size.height as i32, std::sync::atomic::Ordering::Relaxed);
        }
    }
    Ok(true)
}

/// Compatibility path for backends without compositor resize support.
#[tauri::command]
fn begin_tool_resize(window: tauri::WebviewWindow, min_width: f64, min_height: f64) {
    if window.label() == "main" {
        return;
    }
    platform::window::release_size(&window, tauri::LogicalSize::new(min_width, min_height));
    std::thread::spawn(move || {
        // Wait for release; use stable size only if the X11 pointer query fails.
        std::thread::sleep(Duration::from_millis(150));
        let mut last = window.inner_size().ok();
        let mut still = 0;
        for _ in 0..1200 {
            std::thread::sleep(Duration::from_millis(50));
            match platform::window::primary_button_down() {
                Some(true) => continue,
                Some(false) => break,
                None => {
                    let now = window.inner_size().ok();
                    still = if now == last { still + 1 } else { 0 };
                    last = now;
                    if still >= 10 {
                        break;
                    }
                }
            }
        }
        if let Ok(size) = window.inner_size() {
            platform::window::set_size(&window, tauri::Size::Physical(size));
        }
    });
}

#[tauri::command]
fn get_aion2_window_title() -> Option<String> {
    platform::window_detector::find_aion2_window_title()
}

#[tauri::command]
fn test_auto_hide() -> serde_json::Value {
    let aion_fg = platform::window_detector::is_aion2_foreground();
    let aion_title = platform::window_detector::find_aion2_window_title();
    serde_json::json!({
        "aion2_foreground": aion_fg,
        "aion2_title": aion_title,
    })
}

#[tauri::command]
fn debug_status(state: tauri::State<'_, AppState>) -> serde_json::Value {
    let port = state.port_detector.current_port();
    let device = state.port_detector.current_device();
    let ping = state.ping_tracker.current_ping_ms();
    let dmg_gen = state.data_storage.damage_generation();
    let window = platform::window_detector::find_aion2_window_title();
    let admin = platform::admin::is_admin();
    serde_json::json!({
        "port": port,
        "device": device,
        "ping": ping,
        "damageGeneration": dmg_gen,
        "aion2Window": window,
        "isAdmin": admin,
    })
}

#[tauri::command]
async fn replay_file(state: tauri::State<'_, AppState>, file_path: String) -> Result<String, String> {
    // A developer's tool that reads any file it is named: nothing in the app
    // calls it, and release builds refuse it (security audit M7).
    if !cfg!(debug_assertions) {
        return Err("replay is for development builds".into());
    }
    // Reset existing data before replay
    state.dps_calculator.lock().restart_target_selection(true);
    state.data_storage.reset_nicknames();
    state.data_storage.forget_summon_links();

    // Feed packets directly to StreamProcessor, bypassing CaptureDispatcher
    // (no AION2 window check, no port detection needed for replay)
    let data_storage = state.data_storage.clone();
    let skill_lookup = state.skill_lookup.clone();
    let npc_lookup = state.npc_lookup.clone();
    let i18n_dir = state.i18n_data_dir.clone();

    let count = tokio::task::spawn_blocking(move || {
        use crate::capture::stream_processor::StreamProcessor;

        let mut processor = StreamProcessor::new(data_storage.clone(), skill_lookup, npc_lookup);
        // Load DOT IDs
        if let Some(ref data_dir) = i18n_dir {
            let mut dot_ids = std::collections::HashSet::new();
            if let Ok(text) = std::fs::read_to_string(data_dir.join("dot_skill_ids.json")) {
                if let Ok(ids) = serde_json::from_str::<Vec<i32>>(&text) {
                    for id in ids { dot_ids.insert(id); }
                }
            }
            processor.set_dot_skill_ids(dot_ids);
        }

        // Each line in the replay file is a complete game payload — process directly
        // without TCP reassembly (the assembler would incorrectly concatenate payloads)
        let text = match std::fs::read_to_string(&file_path) {
            Ok(t) => t.trim_start_matches('\u{feff}').to_string(), // Strip BOM
            Err(e) => return Err(format!("Failed to read file: {}", e)),
        };

        let mut packet_count = 0;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') { continue; }
            let parts: Vec<&str> = line.splitn(3, '|').collect();
            if parts.len() != 3 { continue; }
            // Use capture-time timestamp from the row, not wall clock
            if let Some(ts) = parse_replay_timestamp(parts[0].trim()) {
                processor.set_override_timestamp(Some(ts));
            }
            let hex = parts[2];
            let data = match decode_replay_hex(hex) {
                Some(d) => d,
                None => continue,
            };
            packet_count += 1;
            processor.consume_stream(&data);
        }

        let dmg = data_storage.damage_generation();
        Ok(format!("Replay complete. {} packets, {} damage events.", packet_count, dmg))
    }).await.map_err(|e| format!("Replay task failed: {}", e))?;

    // Force snapshot boss fights from the replay
    {
        let _saving = state.fight_save.lock();
        let records = state.dps_calculator.lock().snapshot_boss_fights_force();
        let mut sorted = records;
        sorted.sort_by(|a, b| b.total_damage.cmp(&a.total_damage));
        for record in sorted.iter().take(10) {
            if let Err(e) = state.fight_history.save_fight(record) {
                tracing::warn!("Failed to save replay fight: {}", e);
            } else {
                tracing::info!("Saved replay fight: {} ({})", record.boss_name, record.id);
            }
        }
        // Mark all targets as saved so the periodic auto-save loop doesn't re-process them
        state.dps_calculator.lock().mark_all_targets_saved();
    }

    count
}

/// Parse an ISO 8601 timestamp (or plain epoch millis) into epoch milliseconds.
fn parse_replay_timestamp(s: &str) -> Option<i64> {
    // Try plain integer first (epoch millis)
    if let Ok(ms) = s.parse::<i64>() {
        return Some(ms);
    }
    // Parse ISO 8601: "2026-04-01T14:08:18.447814200-03:00"
    // Manual parse to avoid adding a chrono dependency
    // Format: YYYY-MM-DDTHH:MM:SS.fractional[+-]HH:MM
    let t_pos = s.find('T')?;
    let date_part = &s[..t_pos];
    let time_and_tz = &s[t_pos + 1..];

    let date_parts: Vec<&str> = date_part.split('-').collect();
    if date_parts.len() != 3 { return None; }
    let year: i64 = date_parts[0].parse().ok()?;
    let month: i64 = date_parts[1].parse().ok()?;
    let day: i64 = date_parts[2].parse().ok()?;

    // Split time from timezone offset (look for + or - after the seconds)
    let (time_part, tz_offset_mins) = if let Some(plus_pos) = time_and_tz.rfind('+') {
        if plus_pos > 6 { // Must be after HH:MM:SS
            let tz = &time_and_tz[plus_pos + 1..];
            let tz_parts: Vec<&str> = tz.split(':').collect();
            let h: i64 = tz_parts.first()?.parse().ok()?;
            let m: i64 = tz_parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            (&time_and_tz[..plus_pos], h * 60 + m)
        } else {
            (time_and_tz, 0i64)
        }
    } else if let Some(minus_pos) = time_and_tz.rfind('-') {
        if minus_pos > 6 {
            let tz = &time_and_tz[minus_pos + 1..];
            let tz_parts: Vec<&str> = tz.split(':').collect();
            let h: i64 = tz_parts.first()?.parse().ok()?;
            let m: i64 = tz_parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            (&time_and_tz[..minus_pos], -(h * 60 + m))
        } else {
            (time_and_tz, 0i64)
        }
    } else {
        // No timezone, treat as UTC
        let tp = time_and_tz.trim_end_matches('Z');
        (tp, 0i64)
    };

    // Parse time: HH:MM:SS.fractional
    let colon_parts: Vec<&str> = time_part.split(':').collect();
    if colon_parts.len() < 3 { return None; }
    let hour: i64 = colon_parts[0].parse().ok()?;
    let minute: i64 = colon_parts[1].parse().ok()?;
    let sec_parts: Vec<&str> = colon_parts[2].split('.').collect();
    let second: i64 = sec_parts[0].parse().ok()?;
    let millis: i64 = if sec_parts.len() > 1 {
        let frac = sec_parts[1];
        // Take first 3 digits for milliseconds
        let padded = if frac.len() >= 3 { &frac[..3] } else { frac };
        let mut ms: i64 = padded.parse().ok()?;
        if frac.len() < 3 {
            for _ in 0..(3 - frac.len()) { ms *= 10; }
        }
        ms
    } else {
        0
    };

    // Convert to Unix epoch using a simplified algorithm
    // Days from epoch (1970-01-01)
    let days = days_from_civil(year, month, day);
    let total_secs = days * 86400 + hour * 3600 + minute * 60 + second - tz_offset_mins * 60;
    Some(total_secs * 1000 + millis)
}

/// Days from 1970-01-01 for a given civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64;
    let m_adj = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * m_adj + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn decode_replay_hex(hex: &str) -> Option<Vec<u8>> {
    let clean: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
    if clean.len() % 2 != 0 { return None; }
    let mut bytes = Vec::with_capacity(clean.len() / 2);
    for chunk in clean.as_bytes().chunks(2) {
        let h = match chunk[0] {
            b'0'..=b'9' => chunk[0] - b'0',
            b'a'..=b'f' => chunk[0] - b'a' + 10,
            b'A'..=b'F' => chunk[0] - b'A' + 10,
            _ => return None,
        };
        let l = match chunk[1] {
            b'0'..=b'9' => chunk[1] - b'0',
            b'a'..=b'f' => chunk[1] - b'a' + 10,
            b'A'..=b'F' => chunk[1] - b'A' + 10,
            _ => return None,
        };
        bytes.push((h << 4) | l);
    }
    Some(bytes)
}

// ===== APP SETUP =====

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// The identifier the settings live under (tauri.conf.json), for reading
/// `dpsMeter.runAsAdmin` before Tauri starts.
const IDENTIFIER: &str = "com.a2tools.dps-meter";
/// The argument a meter restarting itself as administrator passes: its own
/// process id, to wait for (it holds the one-meter lock until it has closed).
const AFTER_ARG: &str = "--after";

/// Before anything starts: a meter started by one restarting itself as
/// administrator waits for that one to close; one set to run as
/// administrator (for the game's frame rate) and not yet starts itself again
/// elevated and ends. Refused, it runs on as it is.
fn elevate_at_start() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(pid) = args.iter().position(|a| a == AFTER_ARG).and_then(|i| args.get(i + 1)?.parse::<u32>().ok()) {
        tracing::info!("Started as administrator: waiting for the meter before to close");
        platform::admin::wait_for_exit(pid, std::time::Duration::from_secs(20));
        return;
    }
    if !platform::admin::wants_admin(IDENTIFIER) || platform::admin::is_admin() {
        return;
    }
    match platform::admin::relaunch_elevated(&[]) {
        Ok(true) => {
            tracing::info!("Set to run as administrator: started again elevated");
            std::process::exit(0);
        }
        Ok(false) => tracing::info!("Set to run as administrator, but the prompt was refused: running as is"),
        Err(e) => tracing::info!("Set to run as administrator, but could not start elevated: {e}"),
    }
}

/// Run as administrator from now on (`dpsMeter.runAsAdmin`), and restart so
/// now: the new meter is started elevated, then this one closes. Ok(false)
/// when Windows' prompt was refused (nothing changes then).
pub(crate) fn restart_as_admin(app: &tauri::AppHandle) -> Result<bool, String> {
    if platform::admin::is_admin() {
        if let Some(state) = app.try_state::<AppState>() {
            state.settings.set("dpsMeter.runAsAdmin", "true");
        }
        return Ok(true);
    }
    let pid = std::process::id().to_string();
    if !platform::admin::relaunch_elevated(&[AFTER_ARG.to_string(), pid])? {
        return Ok(false);
    }
    if let Some(state) = app.try_state::<AppState>() {
        state.settings.set("dpsMeter.runAsAdmin", "true");
    }
    tracing::info!("Restarting as administrator");
    app.exit(0);
    Ok(true)
}

pub fn run() {
    // Before anything starts a thread: it may set environment variables.
    let process_note = platform::process::prepare();
    logging::logger::init_logging();
    if let Some(note) = process_note {
        tracing::info!("{note}");
    }
    elevate_at_start();

    let mut context = tauri::generate_context!();
    if platform::window_startup::loads_hidden()
        && let Some(main) = context.config_mut().app.windows.iter_mut().find(|w| w.label == "main")
    {
        main.visible = false;
    }

    tauri::Builder::default()
        .append_invoke_initialization_script(format!(
            "window.__A2_WINDOW_STARTUP__ = {{loadsHidden:{},reusesSettings:{}}};",
            platform::window_startup::loads_hidden(),
            platform::window_startup::reuses_settings(),
        ))
        // One meter at a time: a second start shows this one instead. Two
        // ran side by side (a tester), each with its plugins, and every alert
        // came twice. First, as the plugin asks.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tracing::info!("A second start: showing this meter instead");
            set_meter_wanted(app, true);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri::plugin::Builder::<_, ()>::new("settings-persistence")
            .on_event(|app, event| {
                // Flush before normal exit or restart, with Exit as a final safeguard.
                if matches!(event, tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit) {
                    save_queued_fights(app);
                    flush_settings_before_exit(app);
                }
            })
            .build())
        .plugin(tauri_plugin_process::init())
        // Plugin files, each plugin's own origin (crate::plugins::serve).
        .register_uri_scheme_protocol(crate::plugins::SCHEME, |ctx, request| {
            crate::plugins::serve(ctx.app_handle(), request.uri().path())
        })
        // Closing the meter quits, even with Details, History or a kept
        // Settings window still open.
        .on_window_event(|window, event| {
            // Plugin windows open where they were left, at the size they
            // were left (#76).
            if let tauri::WindowEvent::Moved(at) = event
                && crate::plugins::is_plugin_window(window.label())
            {
                crate::plugins::note_moved(window.app_handle(), window.label(), *at);
            }
            if let tauri::WindowEvent::Resized(size) = event
                && crate::plugins::is_plugin_window(window.label())
            {
                crate::plugins::note_resized(window.app_handle(), window, *size);
            }
            if matches!(event, tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_))
                && TOOL_WINDOWS.contains(&window.label())
            {
                save_tool_window_geometry(window.app_handle(), window.label());
            }
            if let tauri::WindowEvent::Focused(focused) = event
                && (TOOL_WINDOWS.contains(&window.label()) || window.label().starts_with("details-"))
            {
                keep_on_top_while_ours(window.app_handle(), window.label(), *focused);
            }
            if matches!(event, tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_))
                && window.label().starts_with("details-")
            {
                save_window_geometry(window.app_handle(), window.label(), FIGHT_WINDOW_KEY);
            }
            // Plugin widgets (#77) open where they were left, and close with
            // their plugin's window.
            if let tauri::WindowEvent::Moved(at) = event
                && window.label().starts_with("pw_")
            {
                crate::plugins::widgets::note_moved(window.app_handle(), window.label(), *at);
            }
            // The dock gone: its plugins' frames with it, so their widgets
            // and follows too, as a plugin window's below.
            if let tauri::WindowEvent::Destroyed = event
                && window.label() == crate::plugins::dock::LABEL
            {
                for id in crate::plugins::dock::members(window.app_handle()) {
                    crate::plugins::widgets::close_all(window.app_handle(), &id);
                    crate::plugins::boss_live::forget_plugin(window.app_handle(), &id);
                    crate::plugins::fps::forget_plugin(&id);
                }
            }
            if let tauri::WindowEvent::Destroyed = event
                && let Some(id) = window.label().strip_prefix("plugin-")
            {
                // Moved into the dock: it runs there now, and has already
                // opened its widgets and follows again (the window goes after
                // the dock's frame starts), so they stay. Closed, they went
                // with the window (Ping's display, 2026-10-23).
                if !crate::plugins::dock::members(window.app_handle()).iter().any(|m| m == id) {
                    crate::plugins::widgets::close_all(window.app_handle(), id);
                    crate::plugins::boss_live::forget_plugin(window.app_handle(), id);
                    crate::plugins::fps::forget_plugin(id);
                }
                // The meter hidden and its last plugin window closed: nothing
                // would be left on screen, so the meter comes back.
                let app = window.app_handle();
                let label = window.label().to_string();
                let others = app.webview_windows().into_keys().any(|l| l != label && crate::plugins::is_plugin_window(&l));
                if !others && !meter_wanted(app) {
                    set_meter_wanted(app, true);
                }
            }
            if window.label() == "main"
                && let tauri::WindowEvent::CloseRequested { api, .. } = event
            {
                api.prevent_close();
                quit_app(window.app_handle().clone());
            }
        })
        .setup(|app| {
            if let Some(main) = app.get_webview_window("main") {
                platform::window::joined(&main, platform::ProcessGroup::Meter);
            }
            // Resolve data directory
            let app_data_dir = app.path().app_data_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."));
            let _ = std::fs::create_dir_all(&app_data_dir);

            // Dev builds quit when a `dev-quit` file appears in the data
            // folder (`npm run dev:stop`): run as administrator, the meter
            // can't be stopped from an ordinary shell.
            #[cfg(debug_assertions)]
            {
                let flag = app_data_dir.join("dev-quit");
                let _ = std::fs::remove_file(&flag);
                let handle = app.handle().clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(Duration::from_millis(500));
                    if flag.exists() {
                        let _ = std::fs::remove_file(&flag);
                        tracing::info!("dev-quit: quitting");
                        quit_app(handle.clone());
                        // Still here a few seconds on: stop outright.
                        std::thread::sleep(Duration::from_secs(5));
                        std::process::exit(0);
                    }
                });
            }

            // Load resources — try multiple paths (dev vs production)
            let skill_lookup = SkillLookup::new();
            let npc_lookup = NpcLookup::new();
            let mut dot_ids: HashSet<i32> = HashSet::new();

            let resource_dir = app.path().resource_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."));
            let candidate_dirs = [
                resource_dir.join("data"),                        // production: resources/data
                resource_dir.join("_up_").join("src").join("data"), // production: resources/_up_/src/data (from ../src/data)
                resource_dir.join("..").join("src").join("data"), // dev: src-tauri/../src/data
                std::path::PathBuf::from("src/data"),             // dev: cwd fallback
                std::path::PathBuf::from("../src/data"),          // dev: from src-tauri/
            ];

            // Find the data directory
            let mut found_data_dir: Option<std::path::PathBuf> = None;
            for data_dir in &candidate_dirs {
                if data_dir.exists() && data_dir.join("i18n").join("skills").exists() {
                    found_data_dir = Some(data_dir.clone());
                    break;
                }
            }

            if let Some(ref data_dir) = found_data_dir {
                // Load DOT skill IDs (language-independent)
                if let Ok(text) = std::fs::read_to_string(data_dir.join("dot_skill_ids.json")) {
                    if let Ok(ids) = serde_json::from_str::<Vec<i32>>(&text) {
                        for id in ids { dot_ids.insert(id); }
                        tracing::info!("Loaded {} DOT skill IDs", dot_ids.len());
                    }
                }

                // Load skill/NPC data in the user's language
                let language = Settings::new(app_data_dir.clone())
                    .get("dpsMeter.language")
                    .unwrap_or_else(|| "en".to_string());
                i18n::lookup::load_language(&skill_lookup, &npc_lookup, data_dir, &language);
            } else {
                tracing::warn!("Failed to find data directory!");
            }

            let skill_lookup = Arc::new(skill_lookup);
            let npc_lookup = Arc::new(npc_lookup);

            let data_storage = Arc::new(DataStorage::new());
            if let Some(ref data_dir) = found_data_dir
                && let Ok(text) = std::fs::read_to_string(data_dir.join("abnormals.json"))
            {
                data_storage.set_abnormal_stack_limits(crate::capture::abnormal::stack_limits(&text));
            }
            let ping_tracker = Arc::new(PingTracker::with_perf_clock(platform::clock::perf_clock()));
            let port_detector = Arc::new(CombatPortDetector::new());

            let mut dps_calculator = DpsCalculator::new(
                data_storage.clone(),
                skill_lookup.clone(),
                npc_lookup.clone(),
                ping_tracker.clone(),
            );

            // A new install (no settings yet): the page asks about sharing
            // before anything uploads (2.0.80 on; it used to switch guest
            // uploads on by itself, audit H3).
            let new_install = !app_data_dir.join("settings.json").exists();
            let settings = Settings::new(app_data_dir.clone());
            platform::window::set_main_anchor(platform::meter_anchor::Anchor::from_setting(
                settings.get(METER_ANCHOR_KEY).as_deref(),
            ));
            let startup_height = settings
                .get(WINDOW_HEIGHT_KEY)
                .and_then(|v| v.trim().parse::<i32>().ok())
                .unwrap_or(0);
            #[cfg(feature = "online")]
            if new_install {
                settings.set("dpsMeter.newInstall", "true");
            }
            #[cfg(not(feature = "online"))]
            let _ = new_install;
            if let Some(ms) = settings.get(ALL_TARGETS_WINDOW_KEY).and_then(|v| v.trim().parse::<i64>().ok()) {
                dps_calculator.set_all_targets_window_ms(ms);
            }

            // Load logging settings from saved state
            if settings.get("dpsMeter.debugLoggingEnabled").as_deref() == Some("true") {
                logging::logger::set_debug_enabled(true, &app_data_dir);
            }
            if settings.get("dpsMeter.saveRawPackets").as_deref() == Some("true") {
                logging::logger::set_packet_log_enabled(true, &app_data_dir);
            }
            // A capture device the player picked by hand (Settings, Auto-detect
            // off) is kept across restarts: it was held in memory only, so a
            // VPN adapter chosen over auto-detection was lost (#39).
            if settings.get("dpsMeter.autoDetectDevice").as_deref() == Some("false") {
                if let Some(device) = settings.get("dpsMeter.manualDevice").filter(|d| !d.trim().is_empty()) {
                    port_detector.set_preferred_device(Some(device));
                }
            }

            let state = AppState {
                data_storage: data_storage.clone(),
                dps_calculator: Mutex::new(dps_calculator),
                ping_tracker: ping_tracker.clone(),
                port_detector: port_detector.clone(),
                fight_history: FightHistoryManager::new(app_data_dir.clone()),
                fight_save: Mutex::new(()),
                queued_fight_saves: Mutex::new(Vec::new()),
                settings,
                skill_lookup: skill_lookup.clone(),
                npc_lookup: npc_lookup.clone(),
                app_data_dir: app_data_dir.clone(),
                i18n_data_dir: found_data_dir.clone(),
                #[cfg(feature = "online")]
                http: reqwest::Client::builder()
                    .user_agent(concat!("A2Tools-DPS-Meter/", env!("CARGO_PKG_VERSION")))
                    .connect_timeout(Duration::from_secs(10))
                    .timeout(Duration::from_secs(30))
                    .build()
                    .unwrap_or_default(),
                capture_suspended: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                overlay_lock: Arc::new(OverlayLock::default()),
                #[cfg(feature = "online")]
                account_seen: Mutex::new(None),
                #[cfg(feature = "online")]
                stream_overlay: crate::stream_overlay::Manager::default(),
                anchored_height: std::sync::atomic::AtomicI32::new(startup_height),
            };
            let capture_suspended = state.capture_suspended.clone();

            app.manage(state);

            // On KDE, a KWin rule keeps the meter above a borderless game
            // (see platform::kwin_rules). Checked at every start and put back
            // if it is missing: without it the meter sits behind the game, and
            // a player lost it to KDE's Window Rules page rewriting the file
            // (2026-10-16). Off the setup thread, as it runs KDE's tools.
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    // For "Hide when AION2 isn't focused": KWin says which
                    // window is active (platform::linux::focus).
                    // On Wayland, KWin keeps the windows where they were
                    // left (#76). Before the watch, which starts the script.
                    let (save_app, load_app) = (handle.clone(), handle.clone());
                    platform::window::keep_places(
                        move |title, rect| save_window_place(&save_app, title, rect),
                        move |title| load_window_place(&load_app, title),
                    );
                    let shot_app = handle.clone();
                    platform::window_detector::watch_screenshot_tool(move |in_front| screenshot_tool(&shot_app, in_front));
                    let focus_app = handle.clone();
                    if let Err(e) = platform::window_detector::watch_focus(move || auto_hide_now(&focus_app)) {
                        tracing::warn!("Cannot follow the active window: {e}");
                    }
                    // Each rule checked at every start and put back when
                    // missing: KWin can drop rules it did not write itself,
                    // and a rule added once and never again left the overlay
                    // taking the focus (a CachyOS player, 2026-10-19). The
                    // overlay takes no focus, so a click on it leaves the game
                    // focused (#71); nor do plugins' icons. Plugin windows
                    // stay above the game, so their alerts are seen.
                    let rules: [(&str, fn(bool) -> Result<bool, String>); 4] = [
                        ("above", platform::window_rules::keep_above_fullscreen),
                        ("no-focus", platform::window_rules::click_without_focus),
                        ("icons", platform::window_rules::icons_without_focus),
                        ("plugin windows", platform::window_rules::plugin_windows_above),
                    ];
                    for (name, ensure) in rules {
                        if let Err(e) = ensure(false) {
                            tracing::warn!("Could not add the KWin {name} rule: {e}");
                        }
                    }
                    // And put back while the meter runs, if KWin drops them.
                    platform::window_rules::keep_rules();
                });
            }
            {
                let handle = app.handle().clone();
                app.state::<AppState>().data_storage
                    .set_before_reset(move || save_fights_before_reset(&handle));
            }
            #[cfg(feature = "online")]
            crate::presence::spawn(app.handle().clone());
            #[cfg(feature = "online")]
            crate::boss_report::spawn(app.handle().clone());
            #[cfg(feature = "online")]
            crate::supply_report::spawn(app.handle().clone());
            #[cfg(feature = "online")]
            crate::party_presence::spawn(app.handle().clone());
            // A deletion of guest uploads the player asked for that hasn't
            // gone through yet: tried again.
            #[cfg(feature = "online")]
            spawn_guest_delete(app.handle().clone());
            #[cfg(feature = "online")]
            crate::stream_overlay::sync(app.handle());

            // Reopen the Details window if it was left enabled. Done here rather
            // than from JS because the backend already has settings loaded — the
            // frontend reads them asynchronously and would race the first paint.
            {
                let saved = app.state::<AppState>().settings.get("dpsMeter.detailsMonitor");
                if let Some(value) = saved {
                    let value = value.trim().to_string();
                    if !value.is_empty() && value != "off" {
                        if let Ok(index) = value.parse::<usize>() {
                            let handle = app.handle().clone();
                            // Deferred: available_monitors is unreliable until the
                            // main window exists and the event loop has run once.
                            tauri::async_runtime::spawn(async move {
                                tokio::time::sleep(Duration::from_millis(600)).await;
                                if let Err(e) = open_details_on_monitor(&handle, index) {
                                    tracing::warn!("details window reopen failed: {}", e);
                                }
                            });
                        }
                    }
                }
            }

            // Restore saved window position and ensure always-on-top
            if let Some(window) = app.get_webview_window("main") {
                let state_ref = app.state::<AppState>();
                if let (Some(x), Some(y)) = (state_ref.settings.get("window.x"), state_ref.settings.get("window.y")) {
                    if let (Ok(x), Ok(y)) = (x.parse::<i32>(), y.parse::<i32>()) {
                        // Don't restore minimized positions (Windows uses -32000,-32000)
                        if x > -10000 && y > -10000 {
                            let _ = window.set_position(tauri::Position::Physical(tauri::PhysicalPosition { x, y }));
                        }
                    }
                }
                let _ = window.set_always_on_top(true);
                if let Ok(size) = window.inner_size() {
                    platform::window::set_size(&window, tauri::Size::Physical(size));
                }
                if meter_wanted(app.handle()) {
                    platform::window_startup::arm_main_fallback(window);
                } else {
                    // Plugins only: the meter stays hidden, unless no plugin
                    // window is on screen by then.
                    platform::window::hide_in_place(&window);
                    let handle = app.handle().clone();
                    tauri::async_runtime::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_secs(8)).await;
                        // No plugin window on screen by then (none started,
                        // or all hidden): the meter comes back.
                        let h = handle.clone();
                        let _ = handle.run_on_main_thread(move || keep_something_on_screen(&h, ""));
                    });
                }
            }

            // Check if Npcap is available before starting capture
            let npcap_available = platform::pcap::library_available();
            if !npcap_available {
                tracing::error!("Npcap is not installed — packet capture disabled");
            }

            // Start capture pipeline
            let (tx, rx) = mpsc::channel::<CapturedPayload>(4096);

            let capturer = PcapCapturer::new(tx, port_detector.clone());
            if npcap_available {
                capturer.start();
            } else {
                // Offer to install it, and start capturing once it is in.
                crate::npcap_setup::offer(app.handle().clone(), capturer);
            }

            let mut dispatcher = CaptureDispatcher::new(
                data_storage.clone(),
                skill_lookup.clone(),
                npc_lookup.clone(),
                port_detector.clone(),
                ping_tracker.clone(),
            );
            dispatcher.set_dot_skill_ids(dot_ids);
            dispatcher.use_suspend_flag(capture_suspended);

            // Run dispatcher in background
            tauri::async_runtime::spawn(async move {
                dispatcher.run(rx).await;
            });

            // Global hotkeys, unless turned off (or, on Linux, not yet on).
            // Plugins: the installed ones, and developer-mode folders (crate::plugins).
            let plugin_host = std::sync::Arc::new(crate::plugins::PluginHost::default());
            app.manage(std::sync::Arc::clone(&plugin_host));
            plugin_host.reload(app.handle());
            crate::plugins::restore_windows(app.handle());
            #[cfg(feature = "online")]
            crate::plugins::find_updates_at_start(app.handle());

            app.manage(Hotkeys {
                manager: platform::hotkeys::HotkeyManager::new(),
                status: Arc::new(Mutex::new(HashMap::new())),
            });
            apply_global_hotkeys(app.handle());
            crate::tray::spawn(app.handle());

            // DPS to the windows, checked at the meter update rate (200 ms
            // unless changed in Settings > Advanced) and sent when it changed
            // (and every DPS_RESEND when it didn't). Without new damage the
            // calculator returns its cached result; with it, replaying a
            // 36-minute fight took at most 0.86 ms per update. Most of the
            // cost is the windows redrawing, which is why the rate is a choice.
            let handle_dps = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let handle = handle_dps;
                let mut rate_ms = DPS_RATE_DEFAULT_MS;
                let mut interval = tokio::time::interval(Duration::from_millis(rate_ms));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                // The last update sent, and when: an unchanged one is sent
                // again only every DPS_RESEND.
                let mut last_dps: Option<serde_json::Value> = None;
                let mut last_dps_at = std::time::Instant::now();
                loop {
                    interval.tick().await;
                    if let Some(state) = handle.try_state::<AppState>() {
                        let wanted = dps_rate_ms(&state.settings);
                        if wanted != rate_ms {
                            rate_ms = wanted;
                            interval = tokio::time::interval(Duration::from_millis(rate_ms));
                            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                        }
                        let t0 = std::time::Instant::now();
                        let calculation_handle = handle.clone();
                        let result = crate::blocking::DPS_TICK.run(move || {
                            let state = calculation_handle.state::<AppState>();
                            let started = std::time::Instant::now();
                            let mut calc = state.dps_calculator.lock();
                            let lock_ms = started.elapsed().as_millis();
                            let calculating = std::time::Instant::now();
                            let dps = calc.get_dps();
                            (dps, lock_ms, calculating.elapsed().as_millis())
                        }).await;
                        match result {
                            Ok((dps, lock_ms, calc_ms)) => {
                                let emitting = std::time::Instant::now();
                                // Every window copies the update and turns it into
                                // text; idle, it was the same each time. Sent when
                                // it changed, else every few seconds, and a window
                                // asks for the current one as it opens.
                                let dps = serde_json::to_value(&dps).unwrap_or_default();
                                if last_dps.as_ref() != Some(&dps) || last_dps_at.elapsed() >= DPS_RESEND {
                                    let _ = handle.emit("dps-update", &dps);
                                    last_dps = Some(dps);
                                    last_dps_at = std::time::Instant::now();
                                }
                                let emit_ms = emitting.elapsed().as_millis();
                                let total_ms = t0.elapsed().as_millis();
                                if total_ms > 200 {
                                    tracing::warn!("Slow: lock={}ms calc={}ms emit={}ms total={}ms gen={}",
                                        lock_ms, calc_ms, emit_ms, total_ms,
                                        state.data_storage.damage_generation());
                                }
                            }
                            Err(error) => tracing::warn!("DPS update failed: {error}"),
                        }

                    }
                }
            });

            // Housekeeping every 500 ms: ping, auto-hide, window positions.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_millis(500));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut tick_count: u64 = 0;
                let mut hide_delay: u64 = 0; // ticks to wait before hiding
                let mut last_ping: Option<u64> = None;
                // The meter hidden because AION2 is closed (Settings).
                let mut hidden_closed = false;
                // Hide while AION2 is closed or while solo, as last checked.
                let mut closed_or_solo = false;
                // Out of combat (#106): the setting last seen, and when the
                // wait began (the meter started, the setting changed, the
                // player showed the meter, or a Settings/History/Details
                // window or the mouse kept it up).
                let mut combat_secs: u64 = 0;
                let mut combat_wait_from = crate::clock::now_ms();
                let mut was_wanted = true;
                // The other windows hidden with the meter (auto-hide), and how
                // many ticks nothing of ours or the game has been in front.
                let mut hidden_with_meter: std::collections::HashSet<String> = Default::default();
                let mut others_delay: u32 = 0;
                let mut last_odyle: Option<((u32, u32), Option<String>)> = None;
                loop {
                    interval.tick().await;
                    tick_count += 1;

                    if let Some(state) = handle.try_state::<AppState>() {
                        // The ping changes about every 10 s; sent when it does,
                        // and every 2 s for a window opened since.
                        if let Some(ping) = state.ping_tracker.current_ping_ms() {
                            if last_ping != Some(ping as u64) || tick_count % 4 == 0 {
                                last_ping = Some(ping as u64);
                                let _ = handle.emit("ping-update", ping);
                            }
                        }

                        // Odyle energy: sent when it changes, and every 2 s
                        // for a window opened since.
                        // With the character it belongs to, once the game
                        // has named it, for the window to remember.
                        if let Some(energy) = state.data_storage.odyle_energy() {
                            let owner = state.data_storage.odyle_owner();
                            let now = (energy, owner);
                            if last_odyle.as_ref() != Some(&now) || tick_count % 4 == 0 {
                                let _ = handle.emit(
                                    "odyle-energy",
                                    serde_json::json!({ "basic": energy.0, "charged": energy.1, "name": now.1 }),
                                );
                                last_odyle = Some(now);
                            }
                        }

                        // --- Hide while AION2 is closed, or while solo (every 2 s) ---
                        // --- or out of combat (every tick) ---
                        // Hidden as the game closes or the party breaks up (or
                        // the option is turned on then), shown as it opens or a
                        // party forms: in between, the keybind can still bring
                        // the meter back. Solo is fewer than two in the party,
                        // as the game last told (Hirai, 2026-10-22).
                        // Out of combat: hidden that many seconds after the
                        // last damage the meter counted, back with the next
                        // hit; no keybind needed (#106, a headless stream).
                        let on = |key: &str| state.settings.get(key).as_deref() == Some("true");
                        if tick_count % 4 == 0 {
                            let closed = on("dpsMeter.hideWhenGameClosed") && !platform::window_detector::find_aion2_window();
                            let solo = on("dpsMeter.hideWhenSolo") && state.data_storage.get_party_members().len() < 2;
                            closed_or_solo = closed || solo;
                        }
                        let now_ms = crate::clock::now_ms();
                        let secs = hide_out_of_combat_secs(&state.settings);
                        if secs != combat_secs {
                            combat_secs = secs;
                            combat_wait_from = now_ms;
                        }
                        let wanted = meter_wanted(&handle);
                        if wanted && !was_wanted {
                            // Shown by the player out of combat: not hidden
                            // again at once.
                            combat_wait_from = now_ms;
                        }
                        was_wanted = wanted;
                        let main = handle.get_webview_window("main");
                        if combat_secs > 0
                            && let Some(window) = &main
                            && keeps_meter_up(&handle, window, !hidden_closed)
                        {
                            combat_wait_from = now_ms;
                        }
                        let last_combat = state.data_storage.last_damage_ms().unwrap_or(i64::MIN).max(combat_wait_from);
                        let idle = tick_count > 20 && out_of_combat(last_combat, now_ms, combat_secs);
                        let want = wanted && (closed_or_solo || idle);
                        if want != hidden_closed {
                            if let Some(window) = &main {
                                if want {
                                    platform::window::minimize_off_top(window);
                                } else if wanted
                                    && !(on("dpsMeter.autoHideMeter")
                                        && !platform::window_detector::is_aion2_foreground()
                                        && !window.is_focused().unwrap_or(false))
                                {
                                    // Hidden by the player meanwhile, it stays
                                    // so; with another program in front and
                                    // "Hide when AION2 isn't focused" on, the
                                    // check below brings it back with the game.
                                    show_meter_again(&handle, window);
                                }
                            }
                            hidden_closed = want;
                            HIDDEN_BY_RULE.store(want, std::sync::atomic::Ordering::Relaxed);
                        }

                        // --- Auto-hide when AION2 loses focus (every tick) ---
                        // The meter's own hiding waits while it is hidden
                        // anyway (by the player, AION2 closed, solo, or out
                        // of combat); the rest below doesn't: Ping's icon
                        // stayed over the browser with the meter hidden while
                        // solo (Hirai).
                        let meter_auto_hide = tick_count > 20
                            && state.settings.get("dpsMeter.autoHideMeter")
                                .unwrap_or_default() == "true";
                        // Plugins set to hide go too, with or without the meter.
                        let others_auto_hide = meter_auto_hide
                            || (tick_count > 20 && crate::plugins::any_hide_unfocused(&handle));
                        let auto_hide = meter_auto_hide
                            && !hidden_closed
                            && meter_wanted(&handle);
                        if auto_hide {
                            if let Some(window) = handle.get_webview_window("main") {
                                let aion_fg = platform::window_detector::is_aion2_foreground();
                                let is_self_fg = window.is_focused().unwrap_or(false);
                                let is_visible = window.is_visible().unwrap_or(true);
                                let is_minimized = window.is_minimized().unwrap_or(false);
                                if tick_count % 4 == 0 {
                                    tracing::trace!("auto-hide: aion_fg={} self_fg={} visible={} minimized={} hide_delay={}",
                                        aion_fg, is_self_fg, is_visible, is_minimized, hide_delay);
                                }
                                // Linux hides it in place (transparent, mapped).
                                let hidden = !is_visible || is_minimized
                                    || platform::window::is_hidden_in_place(&window);
                                if aion_fg || is_self_fg {
                                    hide_delay = 0;
                                    if hidden {
                                        show_meter_again(&handle, &window);
                                    }
                                } else if !hidden {
                                    // Wait 3 ticks (1.5s) before hiding to avoid
                                    // flickering during alt-tab transitions
                                    hide_delay += 1;
                                    if hide_delay >= 3 {
                                        platform::window::minimize_off_top(&window);
                                    }
                                }
                            }
                        }
                        if others_auto_hide {
                            // The rest go and come back with it: gone while
                            // another program is in front, back with the game
                            // or any of the meter's windows (Settings in use
                            // stays).
                            if platform::window::front_is_ours_or_game() {
                                others_delay = 0;
                                if !hidden_with_meter.is_empty() {
                                    show_with_meter(&handle, &mut hidden_with_meter);
                                }
                            } else {
                                // Plugins' windows and icons at the first look
                                // (0.5-1 s); Settings, History and Details
                                // after three (1.5 s), as the meter does, so a
                                // quick alt-tab doesn't take a form in use away.
                                others_delay += 1;
                                hide_with_meter(&handle, meter_auto_hide && others_delay >= 3, &mut hidden_with_meter);
                            }
                        } else if !hidden_with_meter.is_empty() {
                            // Auto-hide turned off meanwhile: back.
                            show_with_meter(&handle, &mut hidden_with_meter);
                        }

                        // --- Save window position every ~5 seconds (every 10 ticks) ---
                        if tick_count % 10 == 0 {
                            if let Some(window) = handle.get_webview_window("main") {
                                if let Ok(pos) = window.outer_position() {
                                    // Don't save minimized/hidden positions
                                    if pos.x > -10000 && pos.y > -10000 {
                                        let moved_x = state.settings.set("window.x", &pos.x.to_string());
                                        let moved_y = state.settings.set("window.y", &pos.y.to_string());
                                        // The anchored height, not the window's: a tooltip
                                        // open at the save would be taken for the meter's.
                                        // Only with a moved position, which it goes with
                                        // (the edge Settings → "Meter stays fixed at" keeps):
                                        // the height follows the rows, and saving each
                                        // change rewrote settings.json all fight long.
                                        let height = state.anchored_height.load(std::sync::atomic::Ordering::Relaxed);
                                        if height > 0 && (moved_x || moved_y) {
                                            state.settings.set(WINDOW_HEIGHT_KEY, &height.to_string());
                                        }
                                    }
                                }
                            }
                            // These float independently of the overlay, so each
                            // remembers where it was left. Per-fight windows
                            // (details-*) are deliberately absent: they are
                            // opened to be read and closed, and keying geometry
                            // by fight id would grow settings without bound.
                            for label in TOOL_WINDOWS {
                                save_tool_window_geometry(&handle, label);
                            }
                        }

                    }
                }
            });

            // One blocking history job at a time; disk/JSON work never runs
            // on Tokio's cooperative workers. The next tick waits for completion.
            let handle_save = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    let handle = handle_save.clone();
                    if let Err(error) = crate::blocking::HISTORY.run(move || {
                        let Some(state) = handle.try_state::<AppState>() else { return };
                        let _saving = state.fight_save.lock();
                        if state.data_storage.damage_generation() > 0 {
                            let records = state.dps_calculator.try_lock()
                                .map(|mut calc| calc.snapshot_boss_fights());
                            if let Some(records) = records {
                                // The calculator guard is gone before any disk I/O.
                                let names = share::SliceNames::of(&state.data_storage);
                                save_fight_records(&handle, &state, records, &names);
                            }
                        }
                        // Automatic uploads that failed and are due again,
                        // fighting or not: a meter left open after the
                        // connection came back catches up on its own.
                        #[cfg(feature = "online")]
                        if share::auto_uploads(&state.settings) {
                            let now = crate::clock::now_ms();
                            for id in share::auto_upload_retries_due(&state.app_data_dir, now) {
                                if let Ok(record) = state.fight_history.load_fight(&id) {
                                    auto_upload(handle.clone(), record);
                                }
                            }
                        }
                    }).await {
                        tracing::warn!("History maintenance failed: {error}");
                    }
                }
            });

            // Supporter roster: fetched, never queried. See `crate::supporters`
            // — asking the server "is this player a supporter?" would hand it a
            // list of who you play with, every fight, for a cosmetic. The
            // private build fetches nothing: only a local override counts.
            let handle_roster = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut had_override = false;
                // The published roster: asked for when due, with the ETag of
                // the one held. The loop itself turns every 15 s, for the
                // override; the CDN is asked only on its own schedule.
                #[cfg(feature = "online")]
                let mut roster_etag: Option<String> = None;
                #[cfg(feature = "online")]
                let mut next_fetch = std::time::Instant::now();
                loop {
                    let wait = ROSTER_POLL_OVERRIDE;
                    if let Some(state) = handle_roster.try_state::<AppState>() {
                        // The override wins when present, and is re-read every
                        // pass so editing it takes effect without a restart.
                        match load_supporter_override(&state.app_data_dir) {
                            Some(roster) => {
                                had_override = true;
                                state.data_storage.set_supporters(roster);
                            }
                            None => {
                                let just_lost_override = had_override;
                                if had_override {
                                    tracing::info!("Supporter roster override removed");
                                    had_override = false;
                                    // The override replaced the published one:
                                    // fetch it again in full, now.
                                    #[cfg(feature = "online")]
                                    {
                                        roster_etag = None;
                                        next_fetch = std::time::Instant::now();
                                    }
                                }
                                #[cfg(not(feature = "online"))]
                                if just_lost_override {
                                    state.data_storage.set_supporters(Default::default());
                                }
                                #[cfg(feature = "online")]
                                if std::time::Instant::now() >= next_fetch {
                                    next_fetch = std::time::Instant::now() + ROSTER_POLL_PUBLISHED;
                                    match fetch_supporter_roster(&state.http, roster_etag.as_deref()).await {
                                        RosterFetch::Fresh(roster, tag) => {
                                            tracing::info!(
                                                "Supporter roster: {} entries ({:?}-keyed)",
                                                roster.len(),
                                                roster.kind()
                                            );
                                            roster_etag = tag;
                                            state.data_storage.set_supporters(roster);
                                        }
                                        RosterFetch::Unchanged => {}
                                        RosterFetch::Failed => {
                                            tracing::debug!("Supporter roster unavailable");
                                            // Only wipe the roster if the override
                                            // we were using has just gone away.
                                            // Clearing on any failed fetch would
                                            // mean one CDN hiccup removes every
                                            // supporter's gold until the next
                                            // successful poll.
                                            if just_lost_override {
                                                state
                                                    .data_storage
                                                    .set_supporters(Default::default());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    tokio::time::sleep(wait).await;
                }
            });

            Ok(())
        })
        .invoke_handler({
            let handler: fn(tauri::ipc::Invoke<tauri::Wry>) -> bool = tauri::generate_handler![
            get_app_version,
            get_hotkey_status,
            open_desktop_shortcuts,
            crate::plugins::plugin_list,
            crate::plugins::plugin_set_dev_mode,
            crate::plugins::plugin_add_folder,
            crate::plugins::plugin_remove_folder,
            crate::plugins::plugin_check_updates,
            crate::plugins::plugin_pending_updates,
            crate::plugins::plugin_offer_updates,
            crate::plugins::plugin_catalog,
            crate::plugins::plugin_install,
            crate::plugins::plugin_uninstall,
            crate::plugins::plugin_debug_frame,
            crate::plugins::plugin_window,
            crate::plugins::plugin_window_fit,
            crate::plugins::plugin_close,
            crate::plugins::plugin_open,
            crate::plugins::plugin_set_enabled,
            crate::plugins::plugin_set_hide,
            crate::plugins::plugin_hello,
            open_settings_at,
            take_settings_section,
            crate::plugins::dock::dock_hello,
            crate::plugins::dock::dock_close,
            crate::plugins::dock::dock_fold,
            crate::plugins::dock::plugin_set_docked,
            crate::plugins::set_plugin_theme,
            crate::plugins::widgets::plugin_widget_open,
            crate::plugins::widgets::plugin_widget_close,
            crate::plugins::widgets::plugin_widget_send,
            crate::plugins::widgets::plugin_widgets_lock,
            crate::plugins::widgets::plugin_widgets_locked,
            crate::plugins::widgets::plugin_widget_hello,
            crate::plugins::widgets::plugin_widget_post,
            crate::plugins::widgets::plugin_widget_drag,
            crate::plugins::boss_live::plugin_boss_follow,
            crate::plugins::fps::plugin_fps_watch,
            set_run_as_admin,
            crate::plugins::fps::plugin_fps_admin,
            crate::plugins::plugin_screenshot,
            crate::plugins::widgets::plugin_widget_area,
            crate::plugins::widgets::plugin_widget_where,
            crate::plugins::widgets::plugin_widget_place,
            set_meter_shown,
            meter_shown,
            crate::plugins::plugin_storage_get,
            crate::plugins::plugin_boss_timers,
            crate::plugins::plugin_abyss_siege,
            crate::plugins::plugin_storage_set,
            get_dps_snapshot,
            get_skill_details,
            get_details_context,
            get_fight_buffs,
            get_fight_history,
            get_personal_bests,
            save_fight,
            load_fight,
            delete_fight,
            export_fight_json,
            preview_share,
            upload_fight,
            share_status,
            account_status,
            account_status_cached,
            discord_activity_available,
            stream_overlay_status,
            stream_overlay_configure,
            stream_overlay_new_key,
            account_begin_link,
            account_sign_out,
            guest_status,
            guest_claim_code,
            get_settings,
            update_settings,
            get_ping,
            get_capture_status,
            set_target_mode,
            set_character_name,
            bind_local_actor_id,
            bind_local_nickname,
            clear_settings,
            reset_combat,
            is_admin,
            set_language,
            set_debug_logging,
            set_packet_logging,
            send_logs_to_dev,
            get_aion2_window_title,
            debug_status,
            quit_app,
            open_url,
            read_cached_icon,
            write_cached_icon,
            log_from_ui,
            suspend_capture,
            overlay_lock_supported,
            pointer_in_window,
            set_overlay_locked,
            set_overlay_card_open,
            is_overlay_locked,
            set_lock_button_rect,
            is_capture_suspended,
            resize_window,
            main_window_ready,
            list_monitors,
            open_details_window,
            close_details_window,
            request_details_view,
            take_pending_details_request,
            close_tool_window,
            open_settings_window,
            close_settings_window,
            tool_window_ready,
            details_window_ready,
            capture_screenshot,
            default_screenshot_folder,
            choose_screenshot_folder,
            start_drag,
            start_tool_drag,
            begin_tool_resize,
            compositor_resize_supported,
            begin_window_resize,
            finish_window_resize,
            reset_auto_detection,
            get_available_devices,
            set_manual_device,
            replay_file,
            test_auto_hide,
            check_for_update,
            release_notes,
            build_features,
            dev_logs_preview,
            guest_stop_sharing,
        ];
            // A plugin's window, the dock and widgets reach only the commands
            // their pages use (`plugins::allowed_from`, security audit H2).
            move |invoke: tauri::ipc::Invoke<tauri::Wry>| {
                let label = invoke.message.webview_ref().label().to_string();
                let command = invoke.message.command().to_string();
                if !crate::plugins::allowed_from(&label, &command) {
                    tracing::warn!("Refused {command} from plugin window {label}");
                    invoke.resolver.reject(format!("{command} isn't available to plugin windows"));
                    return true;
                }
                handler(invoke)
            }
        })
        .run(context)
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    // Updates are an online build's: the offline one has no updater.
    #[cfg(feature = "online")]
    #[test]
    fn updates_come_only_from_the_cdn() {
        use super::{update_url_allowed as ok, version_shaped};
        assert!(ok("https://cdn.a2tools.app/A2Tools-DPS-Meter_2.0.80_x64_en-US.msi"));
        assert!(ok("https://cdn.a2tools.app/linux/a2tools-dps-meter-2.0.80-1-x86_64.pkg.tar.zst"));
        for bad in [
            "http://cdn.a2tools.app/x.msi",
            "https://cdn.a2tools.app.evil.example/x.msi",
            "https://evil.example/x.msi",
            "https://cdn.a2tools.app/../x.msi",
            "https://cdn.a2tools.app/",
            r"https://cdn.a2tools.app/a\..\x.msi",
        ] {
            assert!(!ok(bad), "{bad}");
        }
        assert!(version_shaped("2.0.80"));
        assert!(!version_shaped("2.0.80\n\nClick Yes"));
        assert!(!version_shaped("latest"));
    }

    #[test]
    fn only_web_and_mail_links_open() {
        use super::openable_url as ok;
        assert!(ok("https://a2tools.app/logs/1"));
        assert!(ok("http://example.com"));
        assert!(ok("HTTPS://Discord.gg/Aion2Global"));
        assert!(ok("mailto:help@a2tools.app"));
        for bad in [
            "file:///C:/Windows/System32/calc.exe",
            r"C:\Windows\System32\calc.exe",
            r"\\server\share\x.exe",
            "ms-settings:privacy",
            "https://",
            "https://user:pass@evil.example/",
            "https://a2tools.app/ x",
            r"http:\\evil.example",
            "mailto:",
            "javascript:alert(1)",
        ] {
            assert!(!ok(bad), "{bad}");
        }
        assert!(!ok(&format!("https://a2tools.app/{}", "a".repeat(2100))));
    }

    #[cfg_attr(not(feature = "online"), allow(unused_imports))]
    use super::*;

    /// "Hide out of combat" (#106): hidden once the seconds have passed since
    /// the last hit (or the wait began), never while off.
    #[test]
    fn the_meter_hides_after_the_seconds_out_of_combat() {
        assert!(!out_of_combat(10_000, 19_999, 10));
        assert!(out_of_combat(10_000, 20_000, 10));
        assert!(out_of_combat(10_000, 200_000, 120));
        assert!(!out_of_combat(10_000, 129_999, 120));
        // Off.
        assert!(!out_of_combat(0, i64::MAX, 0));
        // No damage yet and no wait begun: no overflow.
        assert!(out_of_combat(i64::MIN, 0, 30));
        assert!(!out_of_combat(0, 0, u64::MAX));
    }

    /// Locked, the window ignores the mouse except over the lock button, and
    /// while a card is open: a one-time popup at start under a lock left on
    /// from last time could not be clicked, and it covered the lock button.
    #[test]
    fn a_locked_window_takes_the_mouse_while_a_card_is_open() {
        assert!(click_through(true, false, false));
        assert!(!click_through(true, true, false));
        assert!(!click_through(true, false, true));
        assert!(!click_through(false, false, false));
    }

    #[cfg(feature = "online")]
    #[test]
    fn a_panicking_upload_still_frees_its_fight() {
        let held = InFlight::start("in-flight-test").unwrap();
        assert!(InFlight::start("in-flight-test").is_none(), "one upload of a fight at a time");
        drop(held);
        let outcome = std::panic::catch_unwind(|| {
            let _held = InFlight::start("in-flight-test").unwrap();
            panic!("upload task panicked");
        });
        assert!(outcome.is_err());
        assert!(InFlight::start("in-flight-test").is_some());
    }

    #[cfg(feature = "online")]
    #[test]
    fn only_a_package_with_the_manifest_hash_is_installed() {
        let actual = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
        assert!(super::package_hash_matches(actual, actual));
        assert!(super::package_hash_matches(&format!(" {} ", actual.to_uppercase()), actual));
        assert!(!super::package_hash_matches(&actual.replace('9', "8"), actual));
        assert!(!super::package_hash_matches("", actual));
        assert!(!super::package_hash_matches("9f86d081", "9f86d081"));
    }

    #[cfg(feature = "online")]
    #[tokio::test]
    async fn update_downloads_are_capped_and_failed_partial_files_are_removed() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let cases: &[(&[u8], bool)] = &[
            (b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ntest", true),
            (b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nte\r\n2\r\nst\r\n0\r\n\r\n", true),
            (b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\ntest!", false),
            (b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nte\r\n3\r\nst!\r\n0\r\n\r\n", false),
            (b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\ntest!", false),
            (b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nte", false),
        ];
        let http = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap();
        for &(reply, succeeds) in cases {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 2048];
                let _ = socket.read(&mut request).await;
                let _ = socket.write_all(reply).await;
            });
            let response = http.get(url).send().await.unwrap();
            let path = super::update_download_path("https://cdn.a2tools.app/test.msi").unwrap();
            let result = super::save_update_package(response, &path, 4, |_| {}).await;
            server.await.unwrap();
            assert_eq!(result.is_ok(), succeeds, "{result:?}");
            if succeeds {
                assert_eq!(std::fs::read(&path).unwrap(), b"test");
                assert_eq!(result.unwrap(), "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08");
                std::fs::remove_file(path).unwrap();
            } else {
                assert!(!path.exists(), "a failed download left a partial file");
            }
        }
    }

    #[test]
    fn the_csp_allows_every_inline_handler() {
        use sha2::{Digest, Sha256};
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let read = |p: std::path::PathBuf| std::fs::read_to_string(p).unwrap();
        let conf: serde_json::Value =
            serde_json::from_str(&read(root.join("src-tauri/tauri.conf.json"))).unwrap();
        let mut pages = vec![read(root.join("index.html"))];
        for entry in std::fs::read_dir(root.join("public/src/js")).unwrap() {
            pages.push(read(entry.unwrap().path()));
        }
        // `onload="..."` and the like, in the page and in HTML the scripts build.
        let mut handlers = Vec::new();
        for page in &pages {
            let mut rest = page.as_str();
            while let Some(at) = rest.find(" on") {
                rest = &rest[at + 3..];
                let name = rest.bytes().take_while(|b| b.is_ascii_lowercase()).count();
                if name > 0 && rest[name..].starts_with("=\"") {
                    let body = &rest[name + 2..];
                    handlers.push(body[..body.find('"').unwrap()].to_string());
                }
            }
        }
        assert!(handlers.len() >= 5);
        for key in ["csp", "devCsp"] {
            let script_src = conf["app"]["security"][key]["script-src"].as_str().unwrap();
            for handler in &handlers {
                let hash = format!("'sha256-{}'", crate::share::base64(&Sha256::digest(handler.as_bytes())));
                assert!(script_src.contains(&hash), "{key} script-src has no {hash} for {handler}");
            }
        }
    }
}

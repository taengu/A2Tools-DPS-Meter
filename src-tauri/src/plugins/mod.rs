//! Plugins: features beside the meter, kept out of its core (docs/plugins-design.md).
//!
//! A plugin is a folder: `manifest.json`, an `index.html` and its scripts and
//! data. It runs in a window of its own (`plugin-<id>`), inside a sandboxed
//! frame with an origin of its own (the `a2plugin` scheme served here), no
//! network and no access to the meter: it talks only to the host page, which
//! passes on what its manifest's permissions allow. Game data reaches it
//! through its own decoders (`decoder`), as numbers.
//!
//! Two kinds load: reviewed plugins, installed from the signed catalog
//! (`catalog`) into `app_data/plugins/<id>/`, always; and in developer mode,
//! plugins from local folders, for authors to build and test them.

#[cfg(feature = "online")]
pub mod catalog;
pub mod decoder;
pub mod dock;
pub mod fps;
pub mod widgets;
#[cfg(feature = "online")]
pub mod boss_live;
/// Without the network there is nothing to follow.
#[cfg(not(feature = "online"))]
pub mod boss_live {
    #[tauri::command]
    pub fn plugin_boss_follow(_server: u32, _bosses: Vec<String>) -> Result<(), String> {
        Err("live boss updates need the online build".into())
    }
    pub fn forget_plugin(_app: &tauri::AppHandle, _id: &str) {}
}

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::Digest;
use serde_json::Value;
use tauri::{Emitter, Manager};

use crate::app::AppState;
use decoder::{Decoder, DecoderSpec};

/// Developer mode on or off.
pub const DEV_MODE_KEY: &str = "dpsMeter.pluginDevMode";
/// The folders loaded in developer mode, a JSON array of paths.
pub const DEV_FOLDERS_KEY: &str = "dpsMeter.devPlugins";
/// How often plugins' "game" event (is AION2 open) is looked at.
const GAME_CHECK: Duration = Duration::from_secs(5);
/// The scheme plugin files are served from (`http://a2plugin.localhost/` on
/// Windows, `a2plugin://localhost/` elsewhere).
pub const SCHEME: &str = "a2plugin";
/// The newest plugin API this meter speaks; it runs older ones too.
/// 2 added decoders' `self` field and `a2.window` (the `window` permission).
const API_VERSION: u32 = 2;
/// Shortest a plugin may make its window's content (`ui.panel.height`), so
/// one that pops up a line or two, such as an alert, takes no more room.
const MIN_PANEL_HEIGHT: f64 = 60.0;
/// Which plugin windows were open, and whether hidden, when the meter last
/// ran: a JSON object of id → "shown" | "hidden". They open again at start, so
/// a plugin that watches in the background (hidden) keeps doing so.
pub const WINDOWS_KEY: &str = "dpsMeter.pluginWindows";
/// Plugins turned off in Settings, a JSON array of ids: they stay installed
/// but don't run. Every other plugin starts with the meter.
pub const DISABLED_KEY: &str = "dpsMeter.pluginsDisabled";
/// Plugins whose windows and widgets hide while AION2 is closed, a JSON
/// array of ids (Settings → Plugins).
pub const HIDE_CLOSED_KEY: &str = "dpsMeter.pluginsHideClosed";
/// Plugins whose windows and widgets hide when AION2 isn't in front, a JSON
/// array of ids, whether or not the meter itself hides then. Off by default:
/// an alert's icon is most use while the game isn't in front.
pub const HIDE_UNFOCUSED_KEY: &str = "dpsMeter.pluginsHideUnfocused";
/// Whether AION2 is open, as the plugin event loop last looked.
static GAME_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// The plugin and widget windows hidden because AION2 closed, to show again
/// when it opens.
fn hidden_for_game() -> &'static Mutex<std::collections::HashSet<String>> {
    static HIDDEN: std::sync::OnceLock<Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new();
    HIDDEN.get_or_init(Default::default)
}

/// The plugins listed under a setting (`HIDE_CLOSED_KEY`, `HIDE_UNFOCUSED_KEY`).
fn listed(app: &tauri::AppHandle, key: &str) -> Vec<String> {
    app.try_state::<AppState>()
        .and_then(|s| s.settings.get(key))
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Whether any plugin hides when AION2 isn't in front.
pub fn any_hide_unfocused(app: &tauri::AppHandle) -> bool {
    !listed(app, HIDE_UNFOCUSED_KEY).is_empty()
}

/// The plugins that hide while AION2 is closed.
fn hide_closed(app: &tauri::AppHandle) -> Vec<String> {
    listed(app, HIDE_CLOSED_KEY)
}

/// Whether the window `label` (a plugin's, or one of its widgets) hides with
/// the meter when AION2 isn't in front. The dock does when every plugin
/// shown in it does.
pub fn hides_unfocused(app: &tauri::AppHandle, label: &str) -> bool {
    all_listed(&window_plugins(app, label), &listed(app, HIDE_UNFOCUSED_KEY))
}

/// The plugin a window belongs to: its main window or one of its widgets.
fn plugin_of_window(label: &str) -> Option<String> {
    label.strip_prefix("plugin-").map(str::to_string).or_else(|| widgets::parse_label(label).map(|(id, _)| id))
}

/// The plugins a window shows: its plugin, or the dock's shown tabs.
fn window_plugins(app: &tauri::AppHandle, label: &str) -> Vec<String> {
    if label == dock::LABEL {
        return dock::shown_members(app);
    }
    plugin_of_window(label).into_iter().collect()
}

/// Whether there are plugins and every one is in `list`.
fn all_listed(ids: &[String], list: &[String]) -> bool {
    !ids.is_empty() && ids.iter().all(|id| list.contains(id))
}

/// The app commands a plugin's page may call: those its host page, the
/// dock, the frame's SDK and its widgets use. Every other command is refused
/// to a plugin window, the dock and widgets (`allowed_from`), so a plugin
/// that got hold of the IPC still couldn't open links, read files, change
/// settings or start an update (security audit H2, 2026-10-26). Add a command
/// here when a plugin page starts using it.
const PLUGIN_SURFACE_COMMANDS: &[&str] = &[
    "dock_close", "dock_fold", "dock_hello", "meter_shown", "open_settings_at", "open_settings_window",
    "open_url", "plugin_abyss_siege", "plugin_boss_follow", "plugin_boss_timers", "plugin_close",
    "plugin_fps_admin", "plugin_fps_watch", "plugin_hello", "plugin_screenshot", "plugin_set_docked", "plugin_storage_get",
    "plugin_storage_set", "plugin_widget_area", "plugin_widget_close", "plugin_widget_drag",
    "plugin_widget_hello", "plugin_widget_open", "plugin_widget_place", "plugin_widget_post",
    "plugin_widget_send", "plugin_widget_where", "plugin_widgets_lock", "plugin_widgets_locked",
    "plugin_window", "plugin_window_fit", "set_meter_shown", "start_tool_drag",
];

/// Whether the window `label` may call the app command `command`: any of the
/// meter's own windows anything, a plugin's window, the dock or a widget only
/// `PLUGIN_SURFACE_COMMANDS`.
pub fn allowed_from(label: &str, command: &str) -> bool {
    let plugin_surface = is_plugin_window(label) || widgets::parse_label(label).is_some();
    !plugin_surface || PLUGIN_SURFACE_COMMANDS.contains(&command)
}

/// Whether a window (`label`) is one of the meter's plugin windows: a
/// plugin's own, or the dock.
pub fn is_plugin_window(label: &str) -> bool {
    label.starts_with("plugin-") || label == dock::LABEL
}

/// Whether window `label` is to stay hidden now: AION2 closed, and its
/// plugins (all the dock's shown ones) set to hide then.
fn window_game_hides(app: &tauri::AppHandle, label: &str) -> bool {
    !GAME_OPEN.load(std::sync::atomic::Ordering::Relaxed) && all_listed(&window_plugins(app, label), &hide_closed(app))
}

/// Whether `id`'s windows are to stay hidden now: AION2 closed, and the
/// plugin set to hide then.
pub fn game_hides(app: &tauri::AppHandle, id: &str) -> bool {
    !GAME_OPEN.load(std::sync::atomic::Ordering::Relaxed) && hide_closed(app).iter().any(|x| x == id)
}

/// A window `game_hides` keeps out of sight, to show when the game opens.
pub fn hold_for_game(window: &tauri::WebviewWindow) {
    crate::platform::window::hide_in_place(window);
    hidden_for_game().lock().unwrap_or_else(|p| p.into_inner()).insert(window.label().to_string());
}

/// Hide the windows of the plugins set to hide while AION2 is closed, or
/// show again the ones hidden so: on the game opening or closing, and on
/// the setting changing. Windows the player brings back meanwhile stay.
pub fn sync_game_hiding(app: &tauri::AppHandle) {
    let open = GAME_OPEN.load(std::sync::atomic::Ordering::Relaxed);
    let opted = hide_closed(app);
    let mut hidden = hidden_for_game().lock().unwrap_or_else(|p| p.into_inner());
    for (label, window) in app.webview_windows() {
        let ids = window_plugins(app, &label);
        if ids.is_empty() && !hidden.contains(&label) {
            continue;
        }
        let hide = !open && all_listed(&ids, &opted);
        if hide {
            let shown = window.is_visible().unwrap_or(false) && !crate::platform::window::is_hidden_in_place(&window);
            if shown && hidden.insert(label) {
                crate::platform::window::hide_in_place(&window);
            }
        } else if hidden.remove(&label) {
            // Closed by its plugin meanwhile: it stays hidden.
            if widgets::is_closed(&label) || widgets::hold_unfocused(app, &window) {
                continue;
            }
            if !crate::platform::window::reveal_in_place(&window) {
                crate::platform::window::show_on_top_without_focus(&window);
            }
            widgets::relock(app, &window);
        }
    }
}
/// Largest value one storage key may hold.
const MAX_STORED: usize = 1024 * 1024;
/// Most storage keys one plugin may keep (#131); the busiest keeps seven.
const MAX_STORED_KEYS: usize = 256;
/// How long a decoded event waits before it is stamped with the character and
/// sent. At login the game sends some data (the pet list) a few records
/// before the one that says which character is playing; the wait lets that
/// record land first, so the data is filed under the right character. Only
/// plugins that are told the character (`events:character`) wait, and not
/// for decoders marked `immediate`; the others get each event as it is read.
const SETTLE: Duration = Duration::from_millis(500);
/// How long the flusher gathers a burst of events into one message per window.
const BATCH_WAIT: Duration = Duration::from_millis(40);
/// Events kept per plugin for the run, replayed to its window when it opens.
const MAX_LOGGED: usize = 20_000;
/// And at most this much of them. Kept as full JSON trees, 20,000 events of
/// a plugin reading every hit or spawn held tens of MB each, all run long,
/// part of what made a Linux meter climb past 1 GB (2026-10-29).
const MAX_LOGGED_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api_version: u32,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default, skip_serializing)]
    pub decoders: Vec<DecoderSpec>,
    #[serde(default)]
    pub ui: Option<Value>,
}

struct Loaded {
    manifest: Manifest,
    dir: PathBuf,
    decoders: Vec<Decoder>,
    /// Installed from the signed catalog, not a developer folder.
    reviewed: bool,
}

/// What Settings lists.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub path: String,
    pub permissions: Vec<String>,
    /// Installed from the signed catalog (otherwise a developer folder).
    pub reviewed: bool,
    /// Set when the folder could not be loaded.
    pub error: Option<String>,
}

/// What Settings shows: developer mode, and each folder's plugin (or error).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginsView {
    pub dev_mode: bool,
    /// Whether this build can reach the catalog (not the offline build).
    pub online: bool,
    pub plugins: Vec<PluginInfo>,
    /// Ids of the plugins turned off.
    pub disabled: Vec<String>,
    /// Ids of the plugins that hide while AION2 is closed.
    pub hide_closed: Vec<String>,
    /// Ids of the plugins that hide with the meter when AION2 isn't in front.
    pub hide_unfocused: Vec<String>,
    /// Ids of the plugins in a window of their own, not the dock.
    pub own_window: Vec<String>,
}

fn view(app: &tauri::AppHandle, host: &PluginHost) -> PluginsView {
    let dev_mode = app
        .try_state::<AppState>()
        .is_some_and(|s| s.settings.get(DEV_MODE_KEY).as_deref() == Some("true"));
    PluginsView {
        dev_mode,
        online: cfg!(feature = "online"),
        plugins: host.list(),
        disabled: disabled(app),
        hide_closed: hide_closed(app),
        hide_unfocused: listed(app, HIDE_UNFOCUSED_KEY),
        own_window: listed(app, dock::OWN_WINDOW_KEY),
    }
}

/// The plugins turned off.
pub(crate) fn disabled(app: &tauri::AppHandle) -> Vec<String> {
    app.try_state::<AppState>()
        .and_then(|s| s.settings.get(DISABLED_KEY))
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn set_disabled(app: &tauri::AppHandle, id: &str, off: bool) {
    let Some(state) = app.try_state::<AppState>() else { return };
    let mut ids = disabled(app);
    ids.retain(|x| x != id);
    if off {
        ids.push(id.to_string());
    }
    state.settings.set(DISABLED_KEY, &serde_json::to_string(&ids).unwrap_or_default());
    if let Ok(host) = host(app) {
        host.set_off(app, id, off);
    }
}

/// Turn a plugin on (it starts now and with every start of the meter) or off
/// (its window closes; it stays installed, with its data).
/// Async: on Windows, building a window from a synchronous command deadlocks.
#[tauri::command]
pub async fn plugin_set_enabled(app: tauri::AppHandle, id: String, enabled: bool) -> Result<PluginsView, String> {
    if !valid_id(&id) {
        return Err("not a plugin id".into());
    }
    set_disabled(&app, &id, !enabled);
    reopened(&app, &id);
    if enabled {
        remember_window(&app, &id, Some("shown"));
        if dock::docks(&app, &id) {
            dock::sync(&app, Some(&id));
        } else {
            open_window(&app, &id, true)?;
        }
    } else {
        if let Some(window) = app.get_webview_window(&format!("plugin-{id}")) {
            let _ = window.close();
        }
        dock::sync(&app, None);
        // Its icons go with it. Closing its own window took them (on the
        // window's going), but one in the plugin dock has none to close, and
        // Event Timers' icons stayed on screen after it was turned off.
        widgets::close_all(&app, &id);
    }
    tracing::info!("Plugins: {id} {}", if enabled { "enabled" } else { "disabled" });
    let host = host(&app)?;
    Ok(view(&app, &host))
}

/// Hide a plugin's windows and widgets while AION2 is closed (`when`
/// "closed"), or with the meter while AION2 isn't in front ("unfocused"), or
/// not.
#[tauri::command]
pub fn plugin_set_hide(app: tauri::AppHandle, id: String, when: String, hide: bool) -> Result<PluginsView, String> {
    if !valid_id(&id) {
        return Err("not a plugin id".into());
    }
    let key = match when.as_str() {
        "closed" => HIDE_CLOSED_KEY,
        "unfocused" => HIDE_UNFOCUSED_KEY,
        _ => return Err("closed or unfocused".into()),
    };
    let state = app.try_state::<AppState>().ok_or("no app state")?;
    let mut ids = listed(&app, key);
    ids.retain(|x| x != &id);
    if hide {
        ids.push(id.clone());
    }
    state.settings.set(key, &serde_json::to_string(&ids).unwrap_or_default());
    sync_game_hiding(&app);
    let host = host(&app)?;
    Ok(view(&app, &host))
}

/// Where reviewed plugins are installed.
fn plugins_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let state = app.try_state::<AppState>().ok_or("no app state")?;
    Ok(state.app_data_dir.join("plugins"))
}

/// A decoded event waiting out `SETTLE`.
struct Pending {
    at: Instant,
    /// Waits out `SETTLE` (the plugin is told the character, and the
    /// decoder isn't `immediate`).
    settle: bool,
    /// When the record was read, unix ms: plugins get it as `at`, so one
    /// can tell how old an event is however long it waited.
    read_ms: i64,
    plugin: String,
    event: String,
    data: Value,
}

/// Decoded events reach a plugin exactly once and in order, whether or not
/// its window is open: each is numbered and kept for the run (`log`), the
/// window's host page replays the ones it has not delivered yet, and a run id
/// tells it when the numbering started over (the meter restarted).
pub struct PluginHost {
    loaded: RwLock<Vec<Loaded>>,
    errors: RwLock<Vec<PluginInfo>>,
    pending: Mutex<Vec<Pending>>,
    /// Wakes the flusher when an event is queued.
    queued: Condvar,
    /// Each plugin's numbered events, as their JSON text (a `Value` tree took
    /// several times the memory), and the bytes held.
    log: Mutex<HashMap<String, (VecDeque<String>, usize)>>,
    seq: Mutex<u64>,
    run: String,
    flusher: AtomicBool,
    /// Plugins closed with their window's ×, this run: a reload (an install,
    /// Check for updates, a folder added) does not start them again.
    closed: Mutex<HashSet<String>>,
    /// Plugins turned off (Settings): they stay loaded, listed with their
    /// data, but nothing is decoded, queued or kept for them (talyaseen,
    /// #137), as they ran on unseen and their replay log filled.
    off: RwLock<HashSet<String>>,
}

impl Default for PluginHost {
    fn default() -> Self {
        let run = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis().to_string())
            .unwrap_or_default();
        PluginHost {
            loaded: RwLock::default(),
            errors: RwLock::default(),
            pending: Mutex::default(),
            queued: Condvar::new(),
            log: Mutex::default(),
            off: RwLock::default(),
            seq: Mutex::new(0),
            run,
            flusher: AtomicBool::new(false),
            closed: Mutex::default(),
        }
    }
}

/// The Abyss artifacts' holders as the game told this player, as
/// a2.on("abyss") hands them on: the shape of the site's file (times in unix
/// seconds, owners by name).
fn abyss_event(state: &crate::capture::abyss_artifacts::ArtifactState, seen_ms: i64) -> Value {
    let secs = |ms: i64| (ms > 0).then_some(ms / 1000);
    let wars: Vec<_> = state.wars.iter()
        .map(|w| serde_json::json!({ "event": w.event_id, "since": secs(w.since_ms), "until": secs(w.until_ms) }))
        .collect();
    let artifacts: Vec<_> = state.artifacts.iter()
        .map(|a| serde_json::json!({ "id": a.id, "event": a.event_id(), "owner": a.owner.name() }))
        .collect();
    serde_json::json!({ "observedAt": seen_ms / 1000, "wars": wars, "artifacts": artifacts })
}

/// A map the player opened, as a2.on("bossMap") hands it on: the feed's
/// shape (times in unix seconds), each boss stamped with when it was read.
fn boss_map_event(map: &crate::capture::boss_map::BossMap) -> Value {
    let secs = |ms: i64| (ms > 0).then_some(ms / 1000);
    let bosses: serde_json::Map<String, Value> = map
        .bosses
        .iter()
        .map(|b| {
            let entry = serde_json::json!({
                "state": if b.up { "up" } else { "dead" },
                "upSince": if b.up { secs(b.time_ms) } else { None },
                "nextSpawn": if b.up { None } else { secs(b.time_ms) },
                "reportedAt": map.seen_ms / 1000,
                "confirmed": true,
            });
            (b.id.to_string(), entry)
        })
        .collect();
    serde_json::json!({ "map": map.map_id, "bosses": bosses })
}

/// Who is playing, as plugins with `events:character` see it.
///
/// With it, what the game says of them: combat power and gear score, and the
/// Odyle energy once the game has said it is theirs (`odyle_owner`: a login's
/// list comes before the record naming who logged in). Each is null until
/// known, and a change sends the event again.
fn character(app: &tauri::AppHandle) -> Option<Value> {
    app.try_state::<AppState>().map(|s| {
        let p = s.data_storage.local_profile();
        // Owner, energy, counts, times, shop limits, crafts, Kinah and Abyss
        // Points from one moment: a login's list replaces them all at once.
        let list = s.data_storage.entry_list();
        let theirs = matches!((list.owner.as_deref(), p.name.as_deref()), (Some(o), Some(n)) if o.trim() == n.trim());
        let odyle = list
            .odyle_energy
            .filter(|_| theirs)
            .map(|(basic, charged)| serde_json::json!({ "basic": basic, "charged": charged }));
        // The game's entry counts by id, as `[first, second]`, theirs the same
        // way (they come in Odyle energy's list). Known ids: 1 Shugo
        // Festival keys, 4 Nightmare, 8 Dimensional Invasion, 101 Daily
        // Dungeon, 60000001 Odyle energy, 80000001 Ascension Trial; 0 the
        // character's Daily Duty today, [completed, taken] (data_storage).
        // The game's other ids are passed on too, unnamed: a2.js documents only
        // the known ones. Started mid-session, the meter has only the entries
        // that changed since, until the next login or transfer.
        let content = (theirs && !list.counts.is_empty()).then(|| serde_json::json!(list.counts));
        // The shop's weekly purchase limits by item id, `[bought, next reset
        // ms]` (100134 Buy Odyle Energy, 100135 Buy Rift Stones), and the
        // crafts made by recipe (32070001 Odyle Energy, server-wide; 32070002
        // the character's): as the shop or crafting window last opened, or a
        // purchase or craft since. Theirs the same way. Past its reset a
        // limit is `[0, null]`: nothing bought, the next reset not known until
        // the shop opens again. From the same snapshot as the owner.
        let shop = (theirs && !list.shop_limits.is_empty()).then(|| serde_json::json!(list.shop_limits));
        let crafts = (theirs && !list.crafts.is_empty()).then(|| serde_json::json!(list.crafts));
        serde_json::json!({
            "name": p.name,
            "serverId": p.server_id,
            "level": p.level,
            "class": p.class.map(|c| c.class_name().to_string()),
            "combatPower": p.combat_power,
            "gearScore": p.gear_score,
            "odyleEnergy": odyle,
            "contentCounts": content,
            "shopLimits": shop,
            "crafts": crafts,
            // Kinah gained today and the Abyss Points this week with the
            // week's limit, from login, 0 once their reset has passed.
            // Theirs the same way, from the same snapshot.
            "kinahToday": list.kinah_today.filter(|_| theirs),
            "abyssPoints": list.abyss_points.filter(|_| theirs).map(|(p, l)| [p, l]),
            // The entry list's time entries by id, `[time, seen at ms]`: the
            // Abyss passages 10000001-10000003 each with its usage time left
            // (ms) while it can still be run (a used one has none). The time
            // is as the game last sent it: it counts down only while in use,
            // and the game sends the change. Theirs the same way.
            "contentTimes": Some(list.times).filter(|t| theirs && !t.is_empty()),
        })
    })
}

/// Take the events that may go now, in order: one that doesn't settle, or
/// has waited out `SETTLE`, unless an earlier event of its plugin is still
/// waiting (an `immediate` soul pickup never overtakes the pet list before it).
fn take_ready(pending: &mut Vec<Pending>, now: Instant) -> Vec<Pending> {
    let mut held: Vec<String> = Vec::new();
    let mut ready = Vec::new();
    for p in std::mem::take(pending) {
        let due = !p.settle || now.duration_since(p.at) >= SETTLE;
        if due && !held.contains(&p.plugin) {
            ready.push(p);
        } else {
            if !held.contains(&p.plugin) {
                held.push(p.plugin.clone());
            }
            pending.push(p);
        }
    }
    ready
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 40 && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Read and check a plugin folder's manifest.
fn load_folder(dir: &Path) -> Result<Loaded, String> {
    let text = std::fs::read_to_string(dir.join("manifest.json")).map_err(|e| format!("no manifest.json: {e}"))?;
    let manifest: Manifest = serde_json::from_str(&text).map_err(|e| format!("manifest.json: {e}"))?;
    if !valid_id(&manifest.id) {
        return Err(format!("id {:?}: lowercase letters, digits and '-' only", manifest.id));
    }
    if manifest.api_version == 0 || manifest.api_version > API_VERSION {
        return Err(format!("apiVersion {} (this meter speaks up to {API_VERSION}): update the meter", manifest.api_version));
    }
    if !dir.join("index.html").is_file() {
        return Err("no index.html".into());
    }
    let mut decoders = Vec::new();
    for spec in &manifest.decoders {
        let decoder = Decoder::compile(spec)?;
        if !manifest.permissions.iter().any(|p| p == &format!("decoder:{}", decoder.name)) {
            return Err(format!("decoder {} needs the permission \"decoder:{}\"", decoder.name, decoder.name));
        }
        decoders.push(decoder);
    }
    Ok(Loaded { manifest, dir: dir.to_path_buf(), decoders, reviewed: false })
}

impl PluginHost {
    /// Load the installed plugins, and in developer mode the folders the
    /// settings list. A developer folder wins over an installed copy of the
    /// same plugin, so its author tests their own files.
    pub fn reload(self: &Arc<Self>, app: &tauri::AppHandle) {
        let Some(state) = app.try_state::<AppState>() else { return };
        let dev = state.settings.get(DEV_MODE_KEY).as_deref() == Some("true");
        let folders: Vec<String> = if dev {
            state.settings.get(DEV_FOLDERS_KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut loaded = Vec::new();
        let mut errors = Vec::new();
        for folder in folders {
            match load_folder(Path::new(&folder)) {
                Ok(l) if loaded.iter().any(|x: &Loaded| x.manifest.id == l.manifest.id) => errors.push(PluginInfo {
                    error: Some(format!("another folder already loads {:?}", l.manifest.id)),
                    ..info(&l)
                }),
                Ok(l) => loaded.push(l),
                Err(e) => errors.push(PluginInfo {
                    id: String::new(),
                    name: folder.clone(),
                    version: String::new(),
                    description: String::new(),
                    path: folder,
                    permissions: Vec::new(),
                    reviewed: false,
                    error: Some(e),
                }),
            }
        }
        #[cfg(feature = "online")]
        if let Ok(dir) = plugins_dir(app) {
            catalog::keep_serials_in(&dir);
        }
        let installed = plugins_dir(app)
            .ok()
            .and_then(|dir| std::fs::read_dir(dir).ok())
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir() && path.file_name().is_some_and(|n| !n.to_string_lossy().starts_with('.')));
        for dir in installed {
            match load_folder(&dir) {
                Ok(l) if loaded.iter().any(|x: &Loaded| x.manifest.id == l.manifest.id) => {
                    tracing::info!("Plugins: {} runs from its developer folder, not the installed copy", l.manifest.id);
                }
                Ok(l) => match check_installed(app, &l) {
                    Ok(()) => loaded.push(Loaded { reviewed: true, ..l }),
                    Err(e) => {
                        tracing::warn!("Plugins: installed {} {} held: {e}", l.manifest.id, l.manifest.version);
                        errors.push(PluginInfo { reviewed: true, error: Some(e), ..info(&l) });
                    }
                },
                Err(e) => tracing::warn!("Plugins: installed {} does not load: {e}", dir.display()),
            }
        }
        let any_decoders = loaded.iter().any(|l| !l.decoders.is_empty());
        tracing::info!("Plugins: {} loaded (developer mode {})", loaded.len(), if dev { "on" } else { "off" });
        *self.loaded.write().unwrap_or_else(|p| p.into_inner()) = loaded;
        *self.errors.write().unwrap_or_else(|p| p.into_inner()) = errors;
        *self.off.write().unwrap_or_else(|p| p.into_inner()) = disabled(app).into_iter().collect();

        if !self.flusher.swap(true, Ordering::SeqCst) {
            let host = Arc::clone(self);
            let app = app.clone();
            std::thread::Builder::new()
                .name("plugin-events".into())
                .spawn(move || {
                let mut told = character(&app);
                let mut game = crate::platform::window_detector::find_aion2_window();
                GAME_OPEN.store(game, std::sync::atomic::Ordering::Relaxed);
                sync_game_hiding(&app);
                let mut game_checked = Instant::now();
                let mut ping: Option<i32> = None;
                let mut fps_told: Option<Value> = None;
                let mut map_seq = 0u64;
                let mut abyss_seq = 0u64;
                let mut recap_seq = 0u64;
                loop {
                    // Woken by each queued event; the timeout lets settling
                    // events go once their wait is over.
                    let ready = {
                        let mut pending = host.pending.lock().unwrap_or_else(|p| p.into_inner());
                        let mut ready = take_ready(&mut pending, Instant::now());
                        if ready.is_empty() {
                            let wait = if pending.is_empty() { Duration::from_secs(1) } else { Duration::from_millis(50) };
                            pending = host.queued.wait_timeout(pending, wait).unwrap_or_else(|p| p.into_inner()).0;
                            ready = take_ready(&mut pending, Instant::now());
                        }
                        ready
                    };
                    // Woken by the first of a burst (a zone loading sends
                    // thousands): what follows within BATCH_WAIT goes with it.
                    let ready = if ready.is_empty() {
                        ready
                    } else {
                        std::thread::sleep(BATCH_WAIT);
                        let mut more = take_ready(&mut host.pending.lock().unwrap_or_else(|p| p.into_inner()), Instant::now());
                        let mut ready = ready;
                        ready.append(&mut more);
                        ready
                    };
                    host.flush(&app, ready);
                    // The character as the meter learns it (a login, a switch,
                    // or the meter starting mid-game): open plugins hear it then,
                    // not only when their window opens.
                    let now = character(&app);
                    if now != told {
                        host.tell_character(&app, now.clone());
                        told = now;
                    }
                    // A map the player opened: its field bosses, to plugins
                    // with feed:boss-timers at once (the shared feed lags).
                    if let Some(state) = app.try_state::<AppState>() {
                        if let Some((next, maps)) = state.data_storage.boss_maps_since(map_seq) {
                            map_seq = next;
                            if let Some(map) = maps.iter().filter(|m| m.header == 0).max_by_key(|m| m.seen_ms) {
                                host.tell_permitted(&app, "feed:boss-timers", "bossMap", boss_map_event(map));
                            }
                        }
                        // Who holds the Abyss artifacts, as the game just
                        // told this player: at once, as the feed's "wars"
                        // and "artifacts" have it.
                        if let Some((next, artifacts, seen_ms)) = state.data_storage.abyss_artifacts_since(abyss_seq) {
                            abyss_seq = next;
                            if !artifacts.artifacts.is_empty() {
                                host.tell_permitted(&app, "feed:boss-timers", "abyss", abyss_event(&artifacts, seen_ms));
                            }
                        }
                        // Who killed the player, once they died. Numbered
                        // and kept, as decoded events: a window opened after
                        // the death still hears of it.
                        if let Some((next, recaps)) = state.data_storage.death_recaps_since(recap_seq) {
                            recap_seq = next;
                            for recap in recaps {
                                let data = serde_json::to_value(&recap).unwrap_or_default();
                                host.queue_permitted("events:death-recap", "deathRecap", recap.at, data);
                            }
                        }
                    }
                    // The ping to the game server, when it changes (about every
                    // 10 s), to every plugin: a ping widget needs no permission.
                    let now_ping = app.try_state::<AppState>().and_then(|s| s.ping_tracker.current_ping_ms());
                    if now_ping != ping {
                        ping = now_ping;
                        host.tell_all(&app, "ping", serde_json::json!({ "ms": now_ping }));
                    }
                    // The game's frame rate, while a plugin asks (fps.rs), to
                    // plugins with "fps" when it changes.
                    match fps::poll().map(fps::reading_json) {
                        Some(now) if fps_told.as_ref() != Some(&now) => {
                            host.tell_permitted(&app, "fps", "fps", now.clone());
                            fps_told = Some(now);
                        }
                        Some(_) => {}
                        None => fps_told = None,
                    }
                    // Whether AION2 is open, looked at every few seconds.
                    if game_checked.elapsed() >= GAME_CHECK {
                        game_checked = Instant::now();
                        let running = crate::platform::window_detector::find_aion2_window();
                        if running != game {
                            game = running;
                            tracing::debug!("Plugins: told the game is {}", if running { "open" } else { "closed" });
                            host.tell_all(&app, "game", serde_json::json!({ "running": running }));
                            GAME_OPEN.store(running, std::sync::atomic::Ordering::Relaxed);
                            sync_game_hiding(&app);
                        }
                    }
                }
                })
                .ok();
        }

        // The tap is set only while some plugin has a decoder.
        if any_decoders {
            let host = Arc::clone(self);
            let app = app.clone();
            crate::capture::frame_tap::set(Some(Arc::new(move |packet: &[u8]| host.on_frame(&app, packet))));
        } else {
            crate::capture::frame_tap::set(None);
        }
    }

    /// Turn a plugin's events off or on: off, what waits for it and its
    /// replay log go, and so do its live boss and frame-rate follows (its
    /// window's going drops those, but one in the dock has none).
    fn set_off(&self, app: &tauri::AppHandle, id: &str, off: bool) {
        let mut set = self.off.write().unwrap_or_else(|p| p.into_inner());
        if !off {
            set.remove(id);
            return;
        }
        set.insert(id.to_string());
        drop(set);
        self.pending.lock().unwrap_or_else(|p| p.into_inner()).retain(|p| p.plugin != id);
        self.log.lock().unwrap_or_else(|p| p.into_inner()).remove(id);
        boss_live::forget_plugin(app, id);
        fps::forget_plugin(id);
    }

    /// The loaded plugins that are on, by id, with whatever `keep` picks.
    fn on_ids(&self, keep: impl Fn(&Loaded) -> bool) -> Vec<String> {
        let off = self.off.read().unwrap_or_else(|p| p.into_inner());
        self.loaded
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|l| !off.contains(&l.manifest.id) && keep(l))
            .map(|l| l.manifest.id.clone())
            .collect()
    }

    fn on_frame(&self, app: &tauri::AppHandle, packet: &[u8]) {
        let len = crate::capture::stream_processor::read_varint(packet, 0).length;
        if len <= 0 {
            return;
        }
        let at = len as usize;
        let Some(opcode) = packet.get(at..at + 2) else { return };
        let body = &packet[at + 2..];
        let off = self.off.read().unwrap_or_else(|p| p.into_inner()).clone();
        let loaded = self.loaded.read().unwrap_or_else(|p| p.into_inner());
        for plugin in loaded.iter().filter(|l| !off.contains(&l.manifest.id)) {
            for d in plugin.decoders.iter().filter(|d| d.opcode == opcode) {
                let Some(data) = d.decode(body) else { continue };
                // Only the player's own doings, for a decoder that asks so.
                if let Some(field) = &d.self_field {
                    let me = app.try_state::<AppState>().and_then(|s| s.data_storage.local_player_id());
                    let theirs = data.get(field).and_then(Value::as_i64);
                    if me.is_none() || me != theirs {
                        continue;
                    }
                }
                let settle = !d.immediate && plugin.manifest.permissions.iter().any(|p| p == "events:character");
                self.pending.lock().unwrap_or_else(|p| p.into_inner()).push(Pending {
                    at: Instant::now(),
                    settle,
                    read_ms: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or_default(),
                    plugin: plugin.manifest.id.clone(),
                    event: format!("decoded:{}", d.name),
                    data,
                });
                self.queued.notify_one();
            }
        }
    }

    /// Send events that are ready (`take_ready`): number them, stamp them
    /// with the character (for plugins allowed to know it), keep them in the
    /// run's log, and pass them to the plugin's window if it is open.
    fn flush(&self, app: &tauri::AppHandle, ready: Vec<Pending>) {
        if ready.is_empty() {
            return;
        }
        let who = character(app);
        // One message to each window per flush (`emit_plugins`): each emit is
        // a script the page runs, with the event in its source, and a zone's
        // load sent thousands, one by one, to every listening page: the
        // plugin process went past 1.3 GB for those minutes (2026-10-29).
        let mut batches: HashMap<String, Vec<Value>> = HashMap::new();
        for item in ready {
            let allowed = self
                .with(&item.plugin, |l| l.manifest.permissions.iter().any(|p| p == "events:character"))
                .unwrap_or(false);
            let seq = {
                let mut seq = self.seq.lock().unwrap_or_else(|p| p.into_inner());
                *seq += 1;
                *seq
            };
            // Named for its plugin: a window's listener hears events sent to
            // any window (see pluginHost.js).
            let event = serde_json::json!({
                "plugin": item.plugin,
                "seq": seq,
                "event": item.event,
                "data": item.data,
                "at": item.read_ms,
                "character": if allowed { who.clone() } else { None },
            });
            {
                let text = event.to_string();
                let mut log = self.log.lock().unwrap_or_else(|p| p.into_inner());
                let (list, bytes) = log.entry(item.plugin.clone()).or_default();
                *bytes += text.len();
                list.push_back(text);
                while list.len() > MAX_LOGGED || *bytes > MAX_LOGGED_BYTES {
                    let Some(gone) = list.pop_front() else { break };
                    *bytes -= gone.len();
                }
            }
            batches.entry(item.plugin.clone()).or_default().push(event);
        }
        for (plugin, events) in batches {
            emit_plugins(app, &plugin, events);
        }
    }

    /// An unnumbered event to every loaded plugin (pluginHost.js hands it
    /// straight on).
    fn tell_all(&self, app: &tauri::AppHandle, event: &str, data: Value) {
        let ids = self.on_ids(|_| true);
        for id in ids {
            let msg = serde_json::json!({ "plugin": id, "event": event, "data": data });
            emit_plugin(app, &id, msg);
        }
    }

    /// A numbered event (see `flush`) to the plugins with a permission.
    /// `read_ms` is when it happened (unix ms).
    fn queue_permitted(&self, permission: &str, event: &str, read_ms: i64, data: Value) {
        let ids = self.on_ids(|l| l.manifest.permissions.iter().any(|p| p == permission));
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        for plugin in ids {
            pending.push(Pending {
                at: Instant::now(),
                settle: false,
                read_ms,
                plugin,
                event: event.to_string(),
                data: data.clone(),
            });
        }
        self.queued.notify_one();
    }

    /// An unnumbered event to the plugins with a permission.
    fn tell_permitted(&self, app: &tauri::AppHandle, permission: &str, event: &str, data: Value) {
        let ids = self.on_ids(|l| l.manifest.permissions.iter().any(|p| p == permission));
        for id in ids {
            let msg = serde_json::json!({ "plugin": id, "event": event, "data": data });
            emit_plugin(app, &id, msg);
        }
    }

    /// The character, to every plugin allowed to know it (pluginHost.js hands
    /// an unnumbered event straight on).
    fn tell_character(&self, app: &tauri::AppHandle, who: Option<Value>) {
        let Some(who) = who else { return };
        let ids = self.on_ids(|l| l.manifest.permissions.iter().any(|p| p == "events:character"));
        for id in ids {
            let event = serde_json::json!({ "plugin": id, "event": "character", "data": who });
            emit_plugin(app, &id, event);
        }
    }

    fn list(&self) -> Vec<PluginInfo> {
        let mut out: Vec<PluginInfo> = self.loaded.read().unwrap_or_else(|p| p.into_inner()).iter().map(info).collect();
        out.extend(self.errors.read().unwrap_or_else(|p| p.into_inner()).iter().cloned());
        out
    }

    fn with<T>(&self, id: &str, f: impl FnOnce(&Loaded) -> T) -> Option<T> {
        self.loaded.read().unwrap_or_else(|p| p.into_inner()).iter().find(|l| l.manifest.id == id).map(f)
    }
}

/// An installed plugin checked against the signed catalog its install kept
/// (`catalog::verify_installed`): until it passes, it is held, listed with
/// why, and doesn't run. The offline build installs nothing and takes the
/// folder as it is.
fn check_installed(app: &tauri::AppHandle, l: &Loaded) -> Result<(), String> {
    #[cfg(feature = "online")]
    {
        let root = plugins_dir(app)?;
        catalog::verify_installed(&root, &l.dir, &l.manifest.id, &l.manifest.version).map_err(|e| match e {
            catalog::Unverified::NoProof => {
                "Not checked against the plugin catalog yet: it runs once it is, at start or on Check for updates.".to_string()
            }
            catalog::Unverified::Changed(why) => format!(
                "Its files are not the reviewed ones ({why}), so it doesn't run: it is installed again from the catalog at start or on Check for updates."
            ),
        })
    }
    #[cfg(not(feature = "online"))]
    {
        let _ = (app, l);
        Ok(())
    }
}

/// Whether an installed plugin is held (`check_installed`).
#[cfg(feature = "online")]
fn held(p: &PluginInfo) -> bool {
    p.reviewed && p.error.is_some()
}

/// The held plugins put right from `catalog`: one whose files are its
/// version's there keeps that catalog and runs (every install before 2.0.87,
/// once); one changed, or of a version the catalog has moved on from, is
/// installed again from it, unless that asks for a permission it didn't
/// have (then it's offered as an update). Whether any runs now.
#[cfg(feature = "online")]
async fn repair_held(app: &tauri::AppHandle, host: &Arc<PluginHost>, catalog: &catalog::Catalog) -> bool {
    let (Ok(root), Some(state)) = (plugins_dir(app), app.try_state::<AppState>()) else { return false };
    let mut changed = false;
    for p in host.list().into_iter().filter(held) {
        let Some(entry) = catalog.plugins.iter().find(|e| e.id == p.id) else { continue };
        if entry.version == p.version && catalog::adopt(entry, &root).is_ok() {
            tracing::info!("Plugins: {} {} checked against the catalog", p.id, p.version);
            changed = true;
            continue;
        }
        let asks_more = entry.permissions.iter().any(|x| !p.permissions.contains(x));
        if catalog::newer(&p.version, &entry.version) || (entry.version != p.version && asks_more) {
            continue;
        }
        match catalog::install(&state.http, entry, &root).await {
            Ok(_) => {
                tracing::info!("Plugins: {} {} installed again from the catalog", entry.id, entry.version);
                changed = true;
            }
            Err(e) => tracing::info!("Plugins: {} could not be installed again: {e}", entry.id),
        }
    }
    if changed {
        host.reload(app);
        refresh_windows(app, host);
        restore_windows(app);
        let _ = app.emit_to("settings", "plugins-changed", ());
    }
    changed
}

fn info(l: &Loaded) -> PluginInfo {
    PluginInfo {
        id: l.manifest.id.clone(),
        name: l.manifest.name.clone(),
        version: l.manifest.version.clone(),
        description: l.manifest.description.clone(),
        path: l.dir.to_string_lossy().into_owned(),
        permissions: l.manifest.permissions.clone(),
        reviewed: l.reviewed,
        error: None,
    }
}

/// The plugin a call is for: a `plugin-<id>` window's own, or the one the
/// dock names, if docked now (`dock::plugin_of_call`).
fn window_plugin(window: &tauri::WebviewWindow, plugin: Option<&str>) -> Result<String, String> {
    let docked = if window.label() == dock::LABEL { dock::members(window.app_handle()) } else { Vec::new() };
    dock::plugin_of_call(window.label(), plugin, &docked)
}

fn host(app: &tauri::AppHandle) -> Result<Arc<PluginHost>, String> {
    app.try_state::<Arc<PluginHost>>().map(|h| Arc::clone(&h)).ok_or_else(|| "plugins are not available".into())
}

fn data_file(app: &tauri::AppHandle, id: &str, key: &str) -> Result<PathBuf, String> {
    if key.is_empty() || key.len() > 64 || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("storage keys are letters, digits, '-' and '_'".into());
    }
    let state = app.try_state::<AppState>().ok_or("no app state")?;
    Ok(state.app_data_dir.join("plugin-data").join(id).join(format!("{key}.json")))
}

// ── Settings ───────────────────────────────────────────────────────────────

#[tauri::command]
pub fn plugin_list(app: tauri::AppHandle) -> Result<PluginsView, String> {
    Ok(view(&app, &*host(&app)?))
}

/// Turn developer mode on or off, and load or unload its plugins.
/// Async: it may start plugins, and building a window from a synchronous
/// command deadlocks on Windows.
#[tauri::command]
pub async fn plugin_set_dev_mode(app: tauri::AppHandle, enabled: bool) -> Result<PluginsView, String> {
    let state = app.try_state::<AppState>().ok_or("no app state")?;
    state.settings.set(DEV_MODE_KEY, if enabled { "true" } else { "false" });
    let host = host(&app)?;
    host.reload(&app);
    restore_windows(&app);
    Ok(view(&app, &host))
}

/// Ask for a plugin folder and load it (developer mode).
/// Async, as the screenshot folder picker is: the dialog must not block the main thread.
#[tauri::command]
pub async fn plugin_add_folder(app: tauri::AppHandle, window: tauri::WebviewWindow) -> Result<PluginsView, String> {
    let Some(folder) = crate::platform::screenshot::pick_folder(&window, None) else {
        return Ok(view(&app, &*host(&app)?));
    };
    load_folder(Path::new(&folder))?;
    let state = app.try_state::<AppState>().ok_or("no app state")?;
    let mut folders: Vec<String> =
        state.settings.get(DEV_FOLDERS_KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    if !folders.contains(&folder) {
        folders.push(folder);
    }
    state.settings.set(DEV_FOLDERS_KEY, &serde_json::to_string(&folders).unwrap_or_default());
    let host = host(&app)?;
    host.reload(&app);
    restore_windows(&app);
    Ok(view(&app, &host))
}

/// A plugin whose version changed when its folder was read again.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUpdate {
    pub id: String,
    pub name: String,
    /// None for a plugin that did not load before.
    pub from: Option<String>,
    pub to: String,
}

/// A newer reviewed version in the catalog, for an installed plugin.
/// One version's notes ("what's new"): its version, and the text by
/// language ("en" always), as a plugin's changes.json gives them through the
/// signed catalog.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Change {
    pub version: String,
    #[serde(flatten)]
    pub text: std::collections::BTreeMap<String, String>,
}

/// A note in `lang`, else in English.
pub fn change_text<'a>(change: &'a Change, lang: &str) -> &'a str {
    change.text.get(lang).or_else(|| change.text.get("en")).map(String::as_str).unwrap_or("")
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginAvailable {
    pub id: String,
    pub name: String,
    pub from: String,
    pub to: String,
    /// Permissions the new version asks for that the installed one did not.
    pub new_permissions: Vec<String>,
    /// What the versions since the installed one added ("what's new").
    #[serde(default)]
    pub changes: Vec<Change>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheck {
    pub view: PluginsView,
    /// Versions that changed on disk (developer folders), now loaded.
    pub updated: Vec<PluginUpdate>,
    /// Newer reviewed versions waiting to be installed.
    pub available: Vec<PluginAvailable>,
    /// Why the catalog could not be checked, if it could not.
    pub catalog_error: Option<String>,
}

/// Reload open plugin windows, closing those whose plugin is gone.
fn refresh_windows(app: &tauri::AppHandle, host: &PluginHost) {
    for (label, window) in app.webview_windows() {
        // The dock's page starts its plugins again (or closes, with none).
        if label == dock::LABEL {
            let _ = window.eval("location.reload()");
            continue;
        }
        let Some(id) = label.strip_prefix("plugin-") else { continue };
        if host.with(id, |_| ()).is_some() {
            let _ = window.eval("location.reload()");
        } else {
            let _ = window.close();
        }
    }
}

/// The catalog's newer versions of installed plugins, each with the entry
/// to install.
#[cfg(feature = "online")]
fn newer_entries(catalog: catalog::Catalog, host: &PluginHost) -> Vec<(PluginAvailable, catalog::Entry)> {
    // Held ones too: an update is how one asking for more comes back.
    let installed: Vec<PluginInfo> = host.list().into_iter().filter(|p| p.reviewed).collect();
    installed
        .into_iter()
        .filter_map(|p| {
            let entry = catalog.plugins.iter().find(|e| e.id == p.id)?;
            catalog::newer(&entry.version, &p.version).then(|| {
                let available = PluginAvailable {
                    id: p.id.clone(),
                    name: entry.name.clone(),
                    from: p.version.clone(),
                    to: entry.version.clone(),
                    new_permissions: entry.permissions.iter().filter(|x| !p.permissions.contains(x)).cloned().collect(),
                    changes: catalog::changes_between(entry, &p.version, &entry.version),
                };
                (available, entry.clone())
            })
        })
        .collect()
}

/// The catalog's newer versions of installed plugins.
#[cfg(feature = "online")]
async fn available_updates(app: &tauri::AppHandle, host: &Arc<PluginHost>) -> Result<Vec<PluginAvailable>, String> {
    let http = app.try_state::<AppState>().ok_or("no app state")?.http.clone();
    let catalog = catalog::fetch(&http).await?;
    repair_held(app, host, &catalog).await;
    Ok(newer_entries(catalog, host).into_iter().map(|(a, _)| a).collect())
}

/// Check for plugin updates: read every plugin folder again (a developer
/// plugin's source is its folder) and reload open plugin windows so they run
/// the new files, then ask the signed catalog for newer reviewed versions.
#[tauri::command]
pub async fn plugin_check_updates(app: tauri::AppHandle) -> Result<UpdateCheck, String> {
    let host = host(&app)?;
    let before: HashMap<String, String> =
        host.list().into_iter().filter(|p| p.error.is_none()).map(|p| (p.id, p.version)).collect();
    host.reload(&app);
    let view = view(&app, &host);
    let updated = view
        .plugins
        .iter()
        .filter(|p| p.error.is_none() && before.get(&p.id) != Some(&p.version))
        .map(|p| PluginUpdate { id: p.id.clone(), name: p.name.clone(), from: before.get(&p.id).cloned(), to: p.version.clone() })
        .collect();
    refresh_windows(&app, &host);
    restore_windows(&app);
    #[cfg(feature = "online")]
    let (available, catalog_error) = match available_updates(&app, &host).await {
        Ok(available) => (available, None),
        Err(e) => (Vec::new(), Some(e)),
    };
    #[cfg(not(feature = "online"))]
    let (available, catalog_error) = (Vec::new(), None);
    Ok(UpdateCheck { view, updated, available, catalog_error })
}

/// Plugin updates the person said No to at start, a JSON object of id →
/// version: that version is not offered again (a newer one is).
pub const DECLINED_UPDATES_KEY: &str = "dpsMeter.pluginUpdatesDeclined";
/// Settings → Plugins, "Update plugins automatically": off unless turned on.
/// On, the updates found when the meter starts are installed without asking,
/// but for any that asks for something new, which waits for the player's
/// yes as before.
pub const AUTO_UPDATE_KEY: &str = "dpsMeter.pluginAutoUpdate";
/// How long after start the catalog is asked, so the check stays out of the
/// way of startup.
#[cfg(feature = "online")]
const UPDATE_ALERT_DELAY: Duration = Duration::from_secs(20);

/// The languages the meter's pages come in.
#[cfg_attr(not(feature = "online"), allow(dead_code))]
const LANGUAGES: [&str; 10] = ["de", "en", "es", "fr", "ja", "ko", "pt", "ru", "zh-Hans", "zh-Hant"];

/// The update prompt's words, in the meter's language
/// (`pluginUpdatePrompt` in the UI translations; the permissions as Settings
/// words them, `settings.plugins.perm*`).
#[cfg_attr(not(feature = "online"), allow(dead_code))]
struct PromptTexts {
    /// The language the texts are in (a version's notes are shown in it).
    lang: String,
    title: String,
    one: String,
    many: String,
    also_asks: String,
    update_now: String,
    failed: String,
    perm_storage: String,
    perm_notify: String,
    perm_window: String,
    perm_widgets: String,
    perm_character: String,
    perm_decoder: String,
    perm_fps: String,
    perm_death_recap: String,
    perm_screenshot: String,
}

/// One text of the meter's interface in its language (English failing that),
/// for a prompt the meter shows itself.
pub(crate) fn ui_text(app: &tauri::AppHandle, section: &[&str], key: &str, fallback: &str) -> String {
    let Some(state) = app.try_state::<AppState>() else { return fallback.to_string() };
    let lang = state.settings.get("dpsMeter.language").unwrap_or_else(|| "en".into());
    let lang = LANGUAGES.iter().find(|known| known.eq_ignore_ascii_case(&lang)).copied().unwrap_or("en");
    let load = |lang: &str| -> Option<Value> {
        let path = state.i18n_data_dir.as_ref()?.join("i18n").join("ui").join(format!("{lang}.json"));
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    };
    [load(lang), load("en")]
        .iter()
        .flatten()
        .find_map(|v| section.iter().fold(v, |v, s| &v[s])[key].as_str().map(str::to_string))
        .unwrap_or_else(|| fallback.to_string())
}

#[cfg_attr(not(feature = "online"), allow(dead_code))]
fn prompt_texts(data_dir: Option<&Path>, lang: &str) -> PromptTexts {
    let load = |lang: &str| -> Option<Value> {
        let path = data_dir?.join("i18n").join("ui").join(format!("{lang}.json"));
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    };
    let lang = LANGUAGES.iter().find(|known| known.eq_ignore_ascii_case(lang)).copied().unwrap_or("en");
    let (ui, en) = (load(lang), load("en"));
    let text = |section: &[&str], key: &str, fallback: &str| -> String {
        [ui.as_ref(), en.as_ref()]
            .into_iter()
            .flatten()
            .find_map(|v| section.iter().fold(v, |v, s| &v[s])[key].as_str().map(str::to_string))
            .unwrap_or_else(|| fallback.to_string())
    };
    let prompt = |key: &str, fallback: &str| text(&["pluginUpdatePrompt"], key, fallback);
    let perm = |key: &str, fallback: &str| text(&["settings", "plugins"], key, fallback);
    PromptTexts {
        lang: lang.to_string(),
        title: prompt("title", "A2Tools - Plugin Update"),
        one: prompt("one", "A plugin update is available:"),
        many: prompt("many", "Plugin updates are available:"),
        also_asks: prompt("alsoAsks", "The new version also asks to:"),
        update_now: prompt("updateNow", "Update now?"),
        failed: prompt("failed", "Couldn't update:"),
        perm_storage: perm("permStorage", "Save its own data on this computer"),
        perm_notify: perm("permNotify", "Show short messages in its window"),
        perm_window: perm("permWindow", "Show and hide its own window, without taking focus from the game"),
        perm_widgets: perm("permWidgets", "Show small windows of its own, which you can move anywhere and lock in place"),
        perm_character: perm("permCharacter", "See your character's name, server, class and level"),
        perm_decoder: perm("permDecoder", "Read selected game fields (may include other players): {name}"),
        perm_fps: perm("permFps", "Show the game's frame rate (on Windows, the meter asks to run as administrator for it)"),
        perm_death_recap: perm("permDeathRecap", "After a player kills you: who they were and the damage they did to you"),
        perm_screenshot: perm("permScreenshot", "Take screenshots of its own window, to the clipboard and your screenshot folder"),
    }
}

/// A permission, in plain words, as Settings words it.
#[cfg_attr(not(feature = "online"), allow(dead_code))]
fn describe_permission(t: &PromptTexts, permission: &str) -> String {
    match permission {
        "storage" => t.perm_storage.clone(),
        "notify" => t.perm_notify.clone(),
        "window" => t.perm_window.clone(),
        "widgets" => t.perm_widgets.clone(),
        "events:character" => t.perm_character.clone(),
        "fps" => t.perm_fps.clone(),
        "events:death-recap" => t.perm_death_recap.clone(),
        "screenshot" => t.perm_screenshot.clone(),
        p => match p.strip_prefix("decoder:") {
            Some(name) => t.perm_decoder.replace("{name}", name),
            None => p.to_string(),
        },
    }
}

/// The updates to offer at start: those not already declined at that version.
#[cfg_attr(not(feature = "online"), allow(dead_code))]
fn updates_to_offer<T>(available: Vec<(PluginAvailable, T)>, declined: &HashMap<String, String>) -> Vec<(PluginAvailable, T)> {
    available.into_iter().filter(|(a, _)| declined.get(&a.id) != Some(&a.to)).collect()
}

/// The question asked at start: each update, with what it newly asks to do,
/// since Yes installs it.
#[cfg_attr(not(feature = "online"), allow(dead_code))]
fn update_prompt(t: &PromptTexts, updates: &[&PluginAvailable]) -> String {
    let mut text = format!("{}\n", if updates.len() == 1 { &t.one } else { &t.many });
    for u in updates {
        text.push_str(&format!("\n{} {} → {}", u.name, u.from, u.to));
        // What's new: each version's notes, newest first.
        for c in &u.changes {
            let note = change_text(c, &t.lang);
            if !note.is_empty() {
                text.push_str(&format!("\n    – {note}"));
            }
        }
        if !u.new_permissions.is_empty() {
            text.push_str(&format!("\n    {}", t.also_asks));
            for p in &u.new_permissions {
                text.push_str(&format!("\n    • {}", describe_permission(t, p)));
            }
        }
    }
    text.push_str(&format!("\n\n{}", t.update_now));
    text
}

/// At start, ask the signed catalog for newer reviewed versions of the
/// installed plugins. Those not declined before are kept for the green button
/// in the meter's title (`plugin-updates` tells the overlay), which offers
/// them (`plugin_offer_updates`): no window pops up over the game, and no
/// need to open Settings and press Check for updates.
#[cfg(feature = "online")]
pub fn find_updates_at_start(app: &tauri::AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // A plugin held at load (installed before installs kept their
        // catalog, mostly) is checked at once, not after the delay, and
        // again every HELD_RETRY until the catalog answers: started
        // offline, it runs soon after the connection is back, not at the
        // next update check hours later.
        let held_app = app.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                let Ok(host) = host(&held_app) else { return };
                let Some(state) = held_app.try_state::<AppState>() else { return };
                if !host.list().iter().any(held) {
                    return;
                }
                match catalog::fetch(&state.http).await {
                    Ok(catalog) => {
                        repair_held(&held_app, &host, &catalog).await;
                        return;
                    }
                    Err(e) => tracing::info!("Plugins: held plugins not checked yet: {e}"),
                }
                tokio::time::sleep(HELD_RETRY).await;
            }
        });
        tokio::time::sleep(UPDATE_ALERT_DELAY).await;
        // At start, then every 6 hours for a meter left running for days.
        // Each check replaces what the title offers: updates installed from
        // Settings meanwhile drop out of it.
        let mut at_start = true;
        loop {
            find_updates(&app, at_start).await;
            at_start = false;
            tokio::time::sleep(UPDATE_RECHECK).await;
        }
    });
}

/// How often a held plugin is checked again while the catalog can't be read.
#[cfg(feature = "online")]
const HELD_RETRY: std::time::Duration = std::time::Duration::from_secs(30);

/// How often the plugin updates are looked for again after the first time.
#[cfg(feature = "online")]
const UPDATE_RECHECK: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

/// Look for plugin updates once, and have the meter's title offer them; at
/// start, with automatic updates on, install the ones that need no new
/// permission first.
#[cfg(feature = "online")]
async fn find_updates(app: &tauri::AppHandle, at_start: bool) {
    let Ok(host) = host(app) else { return };
    let Some(state) = app.try_state::<AppState>() else { return };
    let catalog = match catalog::fetch(&state.http).await {
        Ok(catalog) => catalog,
        Err(e) => {
            tracing::info!("Plugins: update check failed: {e}");
            return;
        }
    };
    repair_held(app, &host, &catalog).await;
    let declined: HashMap<String, String> =
        state.settings.get(DECLINED_UPDATES_KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let updates = updates_to_offer(newer_entries(catalog, &host), &declined);
    if !updates.is_empty() {
        let names = updates.iter().map(|(u, _)| format!("{} {} → {}", u.id, u.from, u.to)).collect::<Vec<_>>().join(", ");
        tracing::info!("Plugins: updates available: {names}");
    }
    let auto = at_start && state.settings.get(AUTO_UPDATE_KEY).as_deref() == Some("true");
    let updates = if auto { install_quietly(app, &host, updates).await } else { updates };
    let had = !pending_updates().lock().unwrap_or_else(|p| p.into_inner()).is_empty();
    if updates.is_empty() && !had {
        return;
    }
    *pending_updates().lock().unwrap_or_else(|p| p.into_inner()) = updates;
    tell_pending(app);
}

/// Install `updates` without asking, one after another, in the background
/// task that found them: the meter's own threads (capture, the DPS window,
/// plugin events) never wait on it. An update that asks for a permission its
/// plugin doesn't have, or one that fails, is left to offer as before.
#[cfg(feature = "online")]
async fn install_quietly(
    app: &tauri::AppHandle,
    host: &Arc<PluginHost>,
    updates: Vec<(PluginAvailable, catalog::Entry)>,
) -> Vec<(PluginAvailable, catalog::Entry)> {
    let mut left = Vec::new();
    let mut done = Vec::new();
    for (u, entry) in updates {
        if !u.new_permissions.is_empty() {
            left.push((u, entry));
            continue;
        }
        match install_entry(app, host, &entry).await {
            Ok(()) => done.push(format!("{} {} → {}", u.id, u.from, u.to)),
            Err(e) => {
                tracing::info!("Plugins: automatic update of {} failed: {e}", u.id);
                left.push((u, entry));
            }
        }
    }
    if !done.is_empty() {
        tracing::info!("Plugins: updated automatically: {}", done.join(", "));
        // Settings, if open, shows the new versions.
        let _ = app.emit_to("settings", "plugins-changed", ());
    }
    left
}

/// The updates found at start and not yet offered.
#[cfg(feature = "online")]
fn pending_updates() -> &'static Mutex<Vec<(PluginAvailable, catalog::Entry)>> {
    static PENDING: std::sync::OnceLock<Mutex<Vec<(PluginAvailable, catalog::Entry)>>> = std::sync::OnceLock::new();
    PENDING.get_or_init(Mutex::default)
}

#[cfg(feature = "online")]
fn tell_pending(app: &tauri::AppHandle) {
    let _ = app.emit_to("main", "plugin-updates", plugin_pending_updates());
}

/// The plugin updates the meter's title offers (none in the offline build).
#[tauri::command]
pub fn plugin_pending_updates() -> Vec<PluginAvailable> {
    #[cfg(feature = "online")]
    return pending_updates().lock().unwrap_or_else(|p| p.into_inner()).iter().map(|(u, _)| u.clone()).collect();
    #[cfg(not(feature = "online"))]
    Vec::new()
}

/// The green button in the meter's title: ask whether to install the updates
/// found at start, each with what it newly asks to do, since Yes installs it.
/// Yes installs the very entries shown, not whatever the catalog says by
/// then; No leaves those versions alone until a newer one. Either way the
/// button goes. Ones installed meanwhile from Settings are not asked about.
#[tauri::command]
pub async fn plugin_offer_updates(app: tauri::AppHandle) -> Result<bool, String> {
    #[cfg(feature = "online")]
    {
        let host = host(&app)?;
        let state = app.try_state::<AppState>().ok_or("no app state")?;
        let installed: HashMap<String, String> = host.list().into_iter().map(|p| (p.id, p.version)).collect();
        let updates: Vec<_> = std::mem::take(&mut *pending_updates().lock().unwrap_or_else(|p| p.into_inner()))
            .into_iter()
            .filter(|(u, _)| installed.get(&u.id).is_some_and(|v| catalog::newer(&u.to, v)))
            .collect();
        tell_pending(&app);
        if updates.is_empty() {
            return Ok(false);
        }
        let lang = state.settings.get("dpsMeter.language").unwrap_or_else(|| "en".into());
        let t = prompt_texts(state.i18n_data_dir.as_deref(), &lang);
        let prompt = update_prompt(&t, &updates.iter().map(|(u, _)| u).collect::<Vec<_>>());
        let title = t.title.clone();
        let accepted = tokio::task::spawn_blocking(move || crate::platform::dialog::ask_yes_no(&title, &prompt))
            .await
            .unwrap_or(false);
        if !accepted {
            let mut declined: HashMap<String, String> =
                state.settings.get(DECLINED_UPDATES_KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
            for (u, _) in &updates {
                declined.insert(u.id.clone(), u.to.clone());
            }
            state.settings.set(DECLINED_UPDATES_KEY, &serde_json::to_string(&declined).unwrap_or_default());
            return Ok(false);
        }
        let mut failed = Vec::new();
        for (u, entry) in &updates {
            if let Err(e) = install_entry(&app, &host, entry).await {
                failed.push(format!("{}: {e}", u.name));
            }
        }
        // Settings, if open, shows the new versions.
        let _ = app.emit_to("settings", "plugins-changed", ());
        if !failed.is_empty() {
            let message = format!("{}\n\n{}", t.failed, failed.join("\n"));
            let _ = tokio::task::spawn_blocking(move || crate::platform::dialog::show_error(&t.title, &message)).await;
        }
        Ok(true)
    }
    #[cfg(not(feature = "online"))]
    {
        let _ = app;
        Ok(false)
    }
}

/// A catalog entry as Settings lists it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogItem {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub permissions: Vec<String>,
    pub source: String,
    /// The installed version, if any.
    pub installed: Option<String>,
    /// A test plugin: Settings lists it only once installed, and installs it
    /// by id ("Install a test plugin").
    pub test: bool,
    /// What its recent versions added, newest first.
    pub changes: Vec<Change>,
}

/// The reviewed plugins on offer, from the signed catalog.
#[tauri::command]
pub async fn plugin_catalog(app: tauri::AppHandle) -> Result<Vec<CatalogItem>, String> {
    #[cfg(feature = "online")]
    {
        let host = host(&app)?;
        let http = app.try_state::<AppState>().ok_or("no app state")?.http.clone();
        // Settings asks on every open; a list from the last few minutes does.
        let catalog = catalog::recent(&http, std::time::Duration::from_secs(300)).await?;
        let installed: HashMap<String, String> =
            host.list().into_iter().filter(|p| p.reviewed && p.error.is_none()).map(|p| (p.id, p.version)).collect();
        Ok(catalog
            .plugins
            .into_iter()
            .map(|e| CatalogItem {
                test: e.test,
                installed: installed.get(&e.id).cloned(),
                id: e.id,
                name: e.name,
                version: e.version,
                description: e.description,
                author: e.author,
                permissions: e.permissions,
                source: e.source,
                changes: e.changes,
            })
            .collect())
    }
    #[cfg(not(feature = "online"))]
    {
        let _ = app;
        Err("The offline build does not download plugins.".into())
    }
}

/// Install one catalog entry (its files checked against the signed
/// catalog's hashes) and run it.
#[cfg(feature = "online")]
async fn install_entry(app: &tauri::AppHandle, host: &Arc<PluginHost>, entry: &catalog::Entry) -> Result<(), String> {
    let http = app.try_state::<AppState>().ok_or("no app state")?.http.clone();
    let dir = plugins_dir(app)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    catalog::install(&http, entry, &dir).await?;
    tracing::info!("Plugins: installed {} {}", entry.id, entry.version);
    host.reload(app);
    refresh_windows(app, host);
    restore_windows(app);
    Ok(())
}

/// Install a reviewed plugin from the catalog, or update it to the catalog's
/// version. Its saved data is kept.
#[tauri::command]
pub async fn plugin_install(app: tauri::AppHandle, id: String) -> Result<PluginsView, String> {
    #[cfg(feature = "online")]
    {
        let host = host(&app)?;
        let http = app.try_state::<AppState>().ok_or("no app state")?.http.clone();
        let catalog = catalog::fetch(&http).await?;
        let id = id.trim().to_lowercase();
        let entry = catalog.plugins.iter().find(|e| e.id == id).ok_or("No plugin by that id in the catalog or the test catalog.")?;
        install_entry(&app, &host, entry).await?;
        Ok(view(&app, &host))
    }
    #[cfg(not(feature = "online"))]
    {
        let _ = (app, id);
        Err("The offline build does not download plugins.".into())
    }
}

/// Remove an installed plugin. Its saved data stays, for a later reinstall.
#[tauri::command]
pub fn plugin_uninstall(app: tauri::AppHandle, id: String) -> Result<PluginsView, String> {
    if !valid_id(&id) {
        return Err("not a plugin id".into());
    }
    if let Some(window) = app.get_webview_window(&format!("plugin-{id}")) {
        let _ = window.close();
    }
    remember_window(&app, &id, None);
    set_disabled(&app, &id, false);
    let root = plugins_dir(&app)?;
    let dir = root.join(&id);
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    #[cfg(feature = "online")]
    catalog::remove_proof(&root, &id);
    let host = host(&app)?;
    host.reload(&app);
    dock::sync(&app, None);
    Ok(view(&app, &host))
}

/// Stop loading a folder (its saved data stays).
#[tauri::command]
pub fn plugin_remove_folder(app: tauri::AppHandle, path: String) -> Result<PluginsView, String> {
    let state = app.try_state::<AppState>().ok_or("no app state")?;
    let mut folders: Vec<String> =
        state.settings.get(DEV_FOLDERS_KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    folders.retain(|f| f != &path);
    state.settings.set(DEV_FOLDERS_KEY, &serde_json::to_string(&folders).unwrap_or_default());
    let host = host(&app)?;
    if let Some(window) = app.get_webview_window(&format!("plugin-{}", host.list().iter().find(|p| p.path == path).map(|p| p.id.clone()).unwrap_or_default())) {
        let _ = window.close();
    }
    host.reload(&app);
    dock::sync(&app, None);
    Ok(view(&app, &host))
}

/// Remember a plugin window's state for the next start (`None`: closed).
fn remember_window(app: &tauri::AppHandle, id: &str, state: Option<&str>) {
    // The plugin hears it ("visible"): its page stays loaded while hidden,
    // and the browser's own visibility doesn't change for a hidden window or
    // dock tab. Event Timers' anchor frame stayed up with the window hidden
    // on its Settings tab (2026-10-24).
    if let Some(shown) = state.and_then(|s| match s { "shown" => Some(true), "hidden" => Some(false), _ => None }) {
        emit_plugin(app, id, serde_json::json!({ "plugin": id, "event": "visible", "data": shown }));
    }
    let Some(settings) = app.try_state::<AppState>() else { return };
    let mut windows: serde_json::Map<String, Value> =
        settings.settings.get(WINDOWS_KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    match state {
        Some(state) => windows.insert(id.to_string(), Value::String(state.into())),
        None => windows.remove(id),
    };
    settings.settings.set(WINDOWS_KEY, &Value::Object(windows).to_string());
}

/// Where a plugin window was last left, `"x,y"` (physical pixels), per id.
const POSITION_PREFIX: &str = "dpsMeter.pluginWindow.";

/// Where to put a plugin window: where it was left, if that is still on a
/// screen (a monitor unplugged or rearranged since leaves it out of reach).
fn saved_position(app: &tauri::AppHandle, window: &tauri::WebviewWindow, id: &str) -> Option<(i32, i32)> {
    let value = app.try_state::<AppState>()?.settings.get(&format!("{POSITION_PREFIX}{id}"))?;
    let (x, y) = value.split_once(',')?;
    let (x, y) = (x.trim().parse().ok()?, y.trim().parse().ok()?);
    let width = window.outer_size().map(|size| size.width).unwrap_or(240);
    let screens: Vec<_> = window
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|monitor| {
            let area = monitor.work_area();
            (area.position.x, area.position.y, area.size.width, area.size.height)
        })
        .collect();
    within_reach(x, y, width, &screens).then_some((x, y))
}

/// Whether a window at `x, y`, `width` wide, has enough of its top bar on one
/// of `screens` (x, y, width, height) to be seen and dragged back. With no
/// screens known (some Linux sessions report none), any place will do.
fn within_reach(x: i32, y: i32, width: u32, screens: &[(i32, i32, u32, u32)]) -> bool {
    const SHOWN_WIDTH: i64 = 48;
    const BAR_HEIGHT: i64 = 32;
    if screens.is_empty() {
        return true;
    }
    let (x, y, width) = (i64::from(x), i64::from(y), i64::from(width));
    screens.iter().any(|&(sx, sy, sw, sh)| {
        let (sx, sy, sw, sh) = (i64::from(sx), i64::from(sy), i64::from(sw), i64::from(sh));
        let shown = (x + width).min(sx + sw) - x.max(sx);
        shown >= SHOWN_WIDTH.min(width) && y >= sy && y + BAR_HEIGHT <= sy + sh
    })
}

/// When each plugin window was last shown: moves right after that are the
/// window manager placing it, not the user.
fn shown_at() -> &'static Mutex<HashMap<String, Instant>> {
    static SHOWN: std::sync::OnceLock<Mutex<HashMap<String, Instant>>> = std::sync::OnceLock::new();
    SHOWN.get_or_init(Mutex::default)
}

/// Show a plugin window where it was last left. KWin places a window anew
/// each time it is mapped (centred, 2026-10-12), so the position is set
/// before showing and again once it is up.
fn show_in_place(app: &tauri::AppHandle, window: &tauri::WebviewWindow, id: &str, show: impl FnOnce()) {
    shown_at().lock().unwrap_or_else(|p| p.into_inner()).insert(id.to_string(), Instant::now());
    let saved = saved_position(app, window, id);
    if let Some((x, y)) = saved {
        let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
    }
    show();
    if let Some((x, y)) = saved {
        let window = window.clone();
        tauri::async_runtime::spawn(async move {
            for wait in [40, 120, 250] {
                tokio::time::sleep(Duration::from_millis(wait)).await;
                let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
            }
        });
    }
}

/// A plugin window moved: remember where, once it has stopped moving.
/// Ignored while the window is being shown (see `show_in_place`).
pub fn note_moved(app: &tauri::AppHandle, label: &str, at: tauri::PhysicalPosition<i32>) {
    static LATEST: std::sync::OnceLock<Mutex<HashMap<String, (i32, i32)>>> = std::sync::OnceLock::new();
    let Some(id) = place_id(label) else { return };
    let placing = shown_at()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&id)
        .is_some_and(|t| t.elapsed() < Duration::from_millis(700));
    if placing {
        return;
    }
    let latest = LATEST.get_or_init(Mutex::default);
    let first = latest.lock().unwrap_or_else(|p| p.into_inner()).insert(id.clone(), (at.x, at.y)).is_none();
    if !first {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(600)).await;
        let Some((x, y)) = latest.lock().unwrap_or_else(|p| p.into_inner()).remove(&id) else { return };
        if let Some(state) = app.try_state::<AppState>() {
            state.settings.set(&format!("{POSITION_PREFIX}{id}"), &format!("{x},{y}"));
        }
    });
}

/// Under which key a plugin window keeps its place and size: its plugin's
/// id, or the dock's own (`dock::PLACE_ID`).
fn place_id(label: &str) -> Option<String> {
    if label == dock::LABEL {
        return Some(dock::PLACE_ID.to_string());
    }
    label.strip_prefix("plugin-").filter(|id| valid_id(id)).map(str::to_string)
}

/// How big a plugin window was last left, `"w,h"` (logical pixels), per id.
const SIZE_PREFIX: &str = "dpsMeter.pluginWindowSize.";

/// The size a plugin window was left at, if it is a usable one.
fn saved_size(app: &tauri::AppHandle, id: &str) -> Option<(f64, f64)> {
    let value = app.try_state::<AppState>()?.settings.get(&format!("{SIZE_PREFIX}{id}"))?;
    let (w, h) = value.split_once(',')?;
    let (w, h): (f64, f64) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    (w.is_finite() && h.is_finite() && w >= 120.0 && h >= 120.0).then_some((w.min(2000.0), h.min(1600.0)))
}

/// A plugin window was resized: remember its size, once it has settled, as
/// `note_moved` does its place. Not while it is being shown, nor a minimized
/// or hidden window's.
pub fn note_resized(app: &tauri::AppHandle, window: &tauri::Window, size: tauri::PhysicalSize<u32>) {
    static LATEST: std::sync::OnceLock<Mutex<HashMap<String, (f64, f64)>>> = std::sync::OnceLock::new();
    let Some(id) = place_id(window.label()) else { return };
    if window.is_minimized().unwrap_or(false) || !window.is_visible().unwrap_or(true) {
        return;
    }
    let placing = shown_at()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&id)
        .is_some_and(|t| t.elapsed() < Duration::from_millis(700));
    let scale = window.scale_factor().unwrap_or(1.0).max(0.1);
    let logical = (f64::from(size.width) / scale, f64::from(size.height) / scale);
    if placing || logical.0 < 120.0 || logical.1 < 120.0 {
        return;
    }
    let latest = LATEST.get_or_init(Mutex::default);
    let first = latest.lock().unwrap_or_else(|p| p.into_inner()).insert(id.clone(), logical).is_none();
    if !first {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(600)).await;
        let Some((w, h)) = latest.lock().unwrap_or_else(|p| p.into_inner()).remove(&id) else { return };
        if let Some(state) = app.try_state::<AppState>() {
            state.settings.set(&format!("{SIZE_PREFIX}{id}"), &format!("{},{}", w.round(), h.round()));
        }
    });
}

/// Build (or show) a plugin's window; `visible: false` builds it hidden, for a
/// plugin restored at start that was watching in the background.
fn open_window(app: &tauri::AppHandle, id: &str, visible: bool) -> Result<(), String> {
    let host = host(app)?;
    let (name, width, height) = host
        .with(id, |l| {
            let panel = l.manifest.ui.as_ref().and_then(|u| u.get("panel"));
            let dim = |key: &str, default: f64, min: f64| {
                panel.and_then(|p| p.get(key)).and_then(Value::as_f64).unwrap_or(default).clamp(min, 1200.0)
            };
            (l.manifest.name.clone(), dim("width", 360.0, 240.0), dim("height", 480.0, MIN_PANEL_HEIGHT))
        })
        .ok_or("no such plugin")?;
    let label = format!("plugin-{id}");
    if let Some(window) = app.get_webview_window(&label) {
        if visible {
            if !crate::platform::window::reveal_in_place(&window) {
                show_in_place(app, &window, id, || crate::platform::window::show_on_top_without_focus(&window));
            }
            let _ = window.set_focus();
        }
        return Ok(());
    }
    // The size it was left at, else the manifest's panel and its title bar.
    let (inner_w, inner_h) = saved_size(app, id).unwrap_or((width, height + 30.0));
    crate::app::note_window_title(&format!("{name} (plugin)"), &label);
    crate::platform::window::share_process(tauri::WebviewWindowBuilder::new(app, &label, tauri::WebviewUrl::App("plugin-host.html".into())), crate::platform::ProcessGroup::Plugins)
        .initialization_script(format!("window.__A2_PLUGIN_ID__ = {};", serde_json::to_string(id).unwrap_or_default()))
        .title(format!("{name} (plugin)"))
        .inner_size(inner_w, inner_h)
        .min_inner_size(240.0, (height + 30.0).min(230.0))
        .decorations(false)
        .always_on_top(true)
        .resizable(true)
        .visible(visible || crate::platform::window::HIDE_AFTER_BUILD)
        .focused(visible)
        // Transparent, so the panel's opacity slider shows the game through it.
        .transparent(true)
        // No system border or shadow (Windows 11 draws both on a frameless
        // window): the page draws its own edge, which fades with the panel.
        .shadow(false)
        .build()
        .map(|window| {
            crate::platform::window::joined(&window, crate::platform::ProcessGroup::Plugins);
            if !visible && crate::platform::window::HIDE_AFTER_BUILD {
                crate::platform::window::hide_without_focus(&window);
            }
            if let Some((x, y)) = saved_position(app, &window, id) {
                let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
            }
        })
        .map_err(|e| e.to_string())
}

/// Open a plugin's window.
/// Async: on Windows, building a window from a synchronous command deadlocks.
#[tauri::command]
pub async fn plugin_open(app: tauri::AppHandle, id: String) -> Result<(), String> {
    if disabled(&app).contains(&id) {
        return Err("that plugin is turned off".into());
    }
    reopened(&app, &id);
    remember_window(&app, &id, Some("shown"));
    if dock::docks(&app, &id) {
        dock::sync(&app, Some(&id));
        return Ok(());
    }
    open_window(&app, &id, true)?;
    Ok(())
}

/// A plugin opened by hand (Open, or turned on) starts again after reloads.
fn reopened(app: &tauri::AppHandle, id: &str) {
    if let Ok(host) = host(app) {
        host.closed.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
    }
}

/// Whether a plugin should be started now: loaded, turned on, not closed with
/// × this run, and not already running.
fn should_start(p: &PluginInfo, off: &[String], closed: &HashSet<String>, running: bool) -> bool {
    p.error.is_none() && !off.contains(&p.id) && !closed.contains(&p.id) && !running
}

/// Start every plugin that is turned on and not running, its window shown or
/// hidden as it was last left (shown the first time): at start, and when a
/// plugin is installed or its folder loaded. One closed with × this run stays
/// closed.
pub fn restore_windows(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<AppState>() else { return };
    let Ok(host) = host(app) else { return };
    let windows: serde_json::Map<String, Value> =
        state.settings.get(WINDOWS_KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let off = disabled(app);
    let closed = host.closed.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for p in host.list() {
        // Docked plugins start in the dock, below.
        if dock::docks(app, &p.id) {
            continue;
        }
        if !should_start(&p, &off, &closed, app.get_webview_window(&format!("plugin-{}", p.id)).is_some()) {
            continue;
        }
        let visible = windows.get(&p.id).and_then(Value::as_str) != Some("hidden");
        if let Err(e) = open_window(app, &p.id, visible) {
            tracing::info!("Plugins: not starting {}: {e}", p.id);
        }
    }
    dock::sync(app, None);
}

/// Bring back every open plugin window and widget (the "plugin windows"
/// hotkey): minimized, off every screen, or buried under other windows. One
/// its plugin has hidden stays hidden: it shows itself when it has something
/// to say. Shown on top without taking focus from the game.
pub fn bring_back_windows(app: &tauri::AppHandle) {
    for (label, window) in app.webview_windows() {
        if !(is_plugin_window(&label) || widgets::parse_label(&label).is_some()) {
            continue;
        }
        if !window.is_visible().unwrap_or(false) || crate::platform::window::is_hidden_in_place(&window) {
            continue;
        }
        let _ = window.unminimize();
        let screens: Vec<_> = window
            .available_monitors()
            .unwrap_or_default()
            .iter()
            .map(|monitor| {
                let area = monitor.work_area();
                (area.position.x, area.position.y, area.size.width, area.size.height)
            })
            .collect();
        if let (Ok(at), Ok(size)) = (window.outer_position(), window.outer_size()) {
            if !within_reach(at.x, at.y, size.width, &screens) {
                if let Some(&(x, y, _, _)) = screens.first() {
                    let _ = window.set_position(tauri::PhysicalPosition::new(x + 40, y + 40));
                }
            }
        }
        crate::platform::window::show_on_top_without_focus(&window);
    }
}

/// A plugin shows or hides its own window (`window` permission), for one that
/// watches in the background and appears only when it has something to say.
/// Shown on top without taking focus from the game.
#[tauri::command]
pub fn plugin_window(app: tauri::AppHandle, window: tauri::WebviewWindow, plugin: Option<String>, visible: bool) -> Result<(), String> {
    let id = window_plugin(&window, plugin.as_deref())?;
    require(&app, &id, "window")?;
    // Docked: its tab shown or hidden, the dock with the tabs. One hiding
    // itself stays when its tab is the dock's last and the dock the last way
    // back, as a window would.
    if window.label() == dock::LABEL {
        if visible {
            dock::note_raised(&app, &id);
        } else if let Some(before) = dock::take_raised(&id) {
            // Hiding after it showed itself: the dock as it was then. Its tab
            // goes if it wasn't open, the dock if it was hidden, and the page
            // goes back to the tab it was on (Hidden Cubes' alert ending
            // left the dock up, on its first tab).
            if !before.tab_was_shown {
                remember_window(&app, &id, Some("hidden"));
            }
            dock::sync(&app, None);
            if !before.dock_was_up {
                dock::hide_now(&app);
            }
            let _ = app.emit_to(dock::LABEL, "dock-return", id.as_str());
            return Ok(());
        }
        if !visible && dock::shown_members(&app) == [id.clone()] && !crate::app::way_back_without(&app, dock::LABEL) {
            return Ok(());
        }
        remember_window(&app, &id, Some(if visible { "shown" } else { "hidden" }));
        // Shown: on its own tab (a widget double-clicked opened the dock on
        // whichever tab was open before).
        dock::sync(&app, visible.then_some(id.as_str()));
        return Ok(());
    }
    // Hidden in place where the window manager would place it anew on
    // showing (Linux), so it comes back where it was left.
    if visible && game_hides(&app, &id) {
        // Shown when AION2 opens (the plugin is set to hide while it's closed).
        hold_for_game(&window);
    } else if visible {
        if !crate::platform::window::reveal_in_place(&window) {
            show_in_place(&app, &window, &id, || crate::platform::window::show_on_top_without_focus(&window));
        }
    } else {
        // A plugin hiding itself (Event Timers when idle) stays shown when
        // it is the last way back: the meter hidden, no other plugin window
        // and no unlocked widget on screen.
        if !crate::app::way_back_without(&app, window.label()) {
            return Ok(());
        }
        crate::platform::window::hide_in_place(&window);
    }
    remember_window(&app, &id, Some(if visible { "shown" } else { "hidden" }));
    Ok(())
}

/// The window fitted to `height` of content (CSS px), for something brief
/// like an alert, or back to the size it had (`None`). The host page hides
/// its title bar meanwhile (pluginHost.js), so the window is the content
/// alone. The size before the first fit is kept until the restore.
#[tauri::command]
pub fn plugin_window_fit(app: tauri::AppHandle, window: tauri::WebviewWindow, plugin: Option<String>, height: Option<f64>) -> Result<(), String> {
    static BEFORE: std::sync::OnceLock<Mutex<HashMap<String, tauri::PhysicalSize<u32>>>> = std::sync::OnceLock::new();
    let id = window_plugin(&window, plugin.as_deref())?;
    require(&app, &id, "window")?;
    // Docked: the dock keeps its size (it holds other plugins). Its tab is
    // shown and opened instead, so the alert is seen; the page shows the
    // plugin at the dock's top meanwhile (pluginDock.js).
    if window.label() == dock::LABEL {
        if height.is_some() {
            dock::note_raised(&app, &id);
            remember_window(&app, &id, Some("shown"));
            dock::sync(&app, Some(&id));
        }
        return Ok(());
    }
    let before = BEFORE.get_or_init(Mutex::default);
    match height {
        Some(h) => {
            let h = h.clamp(40.0, 1200.0);
            let scale = window.scale_factor().map_err(|e| e.to_string())?;
            let now = window.inner_size().map_err(|e| e.to_string())?;
            before.lock().unwrap_or_else(|p| p.into_inner()).entry(id).or_insert(now);
            let width = now.to_logical::<f64>(scale).width;
            let _ = window.set_min_size(Some(tauri::LogicalSize::new(240.0, h.min(230.0))));
            window.set_size(tauri::LogicalSize::new(width, h)).map_err(|e| e.to_string())?;
        }
        None => {
            if let Some(size) = before.lock().unwrap_or_else(|p| p.into_inner()).remove(&id) {
                window.set_size(size).map_err(|e| e.to_string())?;
                let _ = window.set_min_size(Some(tauri::LogicalSize::new(240.0, 230.0_f64.min(size.height as f64))));
            }
        }
    }
    Ok(())
}

/// The window's × button: hide it (see below).
#[tauri::command]
pub fn plugin_close(app: tauri::AppHandle, window: tauri::WebviewWindow, plugin: Option<String>) -> Result<(), String> {
    // Hidden, not stopped: a plugin that is on keeps running (its widgets,
    // its alerts) until it is turned off in Settings; Open shows it again.
    // Closing it stopped everything it did, which players did not expect.
    let id = window_plugin(&window, plugin.as_deref())?;
    remember_window(&app, &id, Some("hidden"));
    // Docked: its tab goes; the dock with the last one (dock::sync).
    if window.label() == dock::LABEL {
        dock::sync(&app, None);
        return Ok(());
    }
    crate::platform::window::hide_in_place(&window);
    crate::app::keep_something_on_screen(&app, window.label());
    Ok(())
}

// ── Plugin windows ─────────────────────────────────────────────────────────

/// What a plugin window's host page needs to start: the manifest, the run's
/// events so far (the page delivers those it has not yet), and the character
/// (for `events:character`).
#[tauri::command]
pub fn plugin_hello(app: tauri::AppHandle, window: tauri::WebviewWindow, plugin: Option<String>, fresh: Option<bool>) -> Result<Value, String> {
    let id = window_plugin(&window, plugin.as_deref())?;
    tracing::debug!("Plugins: {id} starts (in {})", window.label());
    // A plugin starting afresh knows none of the widgets an earlier run of it
    // left open (moved into the dock, or out of it, it keeps them): it would
    // never close them, and they stayed on screen frozen beside the anchor
    // grid (#108). They go; the plugin opens again what it wants. Not on a
    // page that started over by itself: it has opened its widgets already.
    if fresh == Some(true) {
        widgets::close_all(&app, &id);
    }
    let host = host(&app)?;
    let manifest = host.with(&id, |l| l.manifest.clone()).ok_or("plugin not loaded")?;
    let replay: Vec<Value> = host
        .log
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&id)
        .map(|(l, _)| l.iter().filter_map(|t| serde_json::from_str(t).ok()).collect())
        .unwrap_or_default();
    let character =
        if manifest.permissions.iter().any(|p| p == "events:character") { character(&app) } else { None };
    let reviewed = host.with(&id, |l| l.reviewed).unwrap_or(false);
    let language = app.try_state::<AppState>().and_then(|s| s.settings.get("dpsMeter.language")).unwrap_or_else(|| "en".into());
    Ok(serde_json::json!({
        "manifest": manifest,
        "replay": replay,
        "run": host.run,
        "character": character,
        "language": language,
        "theme": plugin_theme(&app),
        "visible": window_shown(&app, &id),
        "reviewed": reviewed,
        "game": crate::platform::window_detector::find_aion2_window(),
        "ping": app.try_state::<AppState>().and_then(|s| s.ping_tracker.current_ping_ms()),
    }))
}

/// Debug builds only: feed a game packet (hex, with its length prefix) to the
/// plugins as if it had arrived from the network, to test them end to end.
#[tauri::command]
pub fn plugin_debug_frame(app: tauri::AppHandle, hex: String) -> Result<(), String> {
    if !cfg!(debug_assertions) {
        return Err("only in debug builds".into());
    }
    let clean: String = hex.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let bytes = (0..clean.len() / 2)
        .map(|i| u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
        .map_err(|e| e.to_string())?;
    host(&app)?.on_frame(&app, &bytes);
    Ok(())
}

/// Whether plugin `id`'s window (or dock tab) is shown, as it was left.
fn window_shown(app: &tauri::AppHandle, id: &str) -> bool {
    let windows: serde_json::Map<String, Value> =
        app.try_state::<AppState>().and_then(|s| s.settings.get(WINDOWS_KEY)).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    windows.get(id).and_then(Value::as_str) != Some("hidden")
}

/// Where the Settings colours plugin windows take are kept.
const THEME_KEY: &str = "dpsMeter.pluginTheme";

/// The Settings colours for plugin windows, `{bg, panel, accent}` as
/// `#rrggbb` (the Settings window reads them, `sharePluginTheme`), or the
/// meter's own until Settings has said.
fn plugin_theme(app: &tauri::AppHandle) -> Value {
    app.try_state::<AppState>()
        .and_then(|s| s.settings.get(THEME_KEY))
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|t| clean_theme(&t))
        .unwrap_or_else(|| serde_json::json!({ "bg": "#0a0e16", "panel": "#0a0e16", "accent": "#8fc2ff" }))
}

/// A theme with exactly its three colours, each `#rrggbb`; None otherwise.
/// It reaches every plugin's page as CSS, so nothing else gets through.
fn clean_theme(theme: &Value) -> Option<Value> {
    let color = |key: &str| {
        let c = theme.get(key)?.as_str()?;
        (c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|ch| ch.is_ascii_hexdigit())).then(|| c.to_ascii_lowercase())
    };
    Some(serde_json::json!({ "bg": color("bg")?, "panel": color("panel")?, "accent": color("accent")? }))
}

/// The Settings colours changed (or Settings opened): kept, and plugin
/// windows told when they differ from what they have.
#[tauri::command]
pub fn set_plugin_theme(app: tauri::AppHandle, theme: Value) -> Result<(), String> {
    let theme = clean_theme(&theme).ok_or("not a theme")?;
    let state = app.try_state::<AppState>().ok_or("not ready")?;
    let text = theme.to_string();
    if state.settings.get(THEME_KEY).as_deref() == Some(text.as_str()) {
        return Ok(());
    }
    state.settings.set(THEME_KEY, &text);
    broadcast(&app, "theme", theme);
    Ok(())
}

/// An event for plugin `id`'s page: its own window, or the dock while it is
/// docked, whose page hands it to that plugin's frame. Once: the pages hear
/// events sent to any window, and each keeps only its own plugins'
/// (pluginHost.js, pluginDock.js).
pub(crate) fn emit_plugin(app: &tauri::AppHandle, id: &str, event: Value) {
    let _ = app.emit_to(plugin_target(app, id), "plugin-event", event);
}

/// Several numbered events of one plugin as one message ("plugin-events",
/// an array the page hands on one by one, as "plugin-event"s).
fn emit_plugins(app: &tauri::AppHandle, id: &str, events: Vec<Value>) {
    let _ = app.emit_to(plugin_target(app, id), "plugin-events", events);
}

/// The window a plugin's page runs in: the dock, or its own.
fn plugin_target(app: &tauri::AppHandle, id: &str) -> String {
    if dock::docks(app, id) { dock::LABEL.to_string() } else { format!("plugin-{id}") }
}

/// Tell every open plugin window something every plugin may know, such as the
/// meter's language when the user changes it.
pub fn broadcast(app: &tauri::AppHandle, event: &str, data: Value) {
    for (label, window) in app.webview_windows() {
        if is_plugin_window(&label) {
            let _ = window.emit("plugin-event", serde_json::json!({ "event": event, "data": data }));
        }
    }
}

/// A screenshot of part of the plugin's own window (permission "screenshot",
/// meter 2.0.81 on): `x`, `y`, `width`, `height` in its window's CSS px (the
/// frame's host adds where the frame sits) and `scale` its devicePixelRatio.
/// To the clipboard, and a PNG named for the plugin in the meter's screenshot
/// folder: `{ clipboard, file }`.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn plugin_screenshot(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    plugin: Option<String>,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    scale: f64,
) -> Result<Value, String> {
    let id = window_plugin(&window, plugin.as_deref())?;
    require(&app, &id, "screenshot")?;
    if ![x, y, width, height, scale].iter().all(|v| v.is_finite()) || width < 1.0 || height < 1.0 {
        return Err("not an area".into());
    }
    let path = crate::app::screenshot_dir(&app).join(format!("{id}_{}.png", chrono::Local::now().format("%Y%m%d_%H%M%S")));
    tauri::async_runtime::spawn_blocking(move || {
        let (clipboard, saved) = crate::platform::screenshot::capture(&window, x, y, width, height, scale, None, Some(&path));
        serde_json::json!({ "clipboard": clipboard, "file": saved.then(|| path.display().to_string()) })
    })
    .await
    .map_err(|e| e.to_string())
}

fn require(app: &tauri::AppHandle, id: &str, permission: &str) -> Result<(), String> {
    let has = host(app)?.with(id, |l| l.manifest.permissions.iter().any(|p| p == permission)).unwrap_or(false);
    if has { Ok(()) } else { Err(format!("the plugin lacks the {permission:?} permission")) }
}

/// A server's field boss timers, as players' meters reported them
/// (a2tools.app's feed, `boss_report.rs`), for plugins with
/// `feed:boss-timers`: the published file as it is, or None when the server
/// has none yet. Kept a minute, so plugins asking often fetch it once.
#[tauri::command]
pub async fn plugin_boss_timers(app: tauri::AppHandle, window: tauri::WebviewWindow, plugin: Option<String>, server: u32) -> Result<Option<Value>, String> {
    let id = window_plugin(&window, plugin.as_deref())?;
    require(&app, &id, "feed:boss-timers")?;
    if !(1000..10000).contains(&server) {
        return Err("not a server id".into());
    }
    boss_timers(&app, server).await
}

/// A server's Abyss siege as players' meters see it (a2tools.app,
/// `boss_report.rs`): who holds each artifact and each Dimensional Core's
/// state and HP, the published file as it is, or None when the server has
/// none yet. For plugins with `feed:boss-timers`. Meter 2.0.72 on.
#[tauri::command]
pub async fn plugin_abyss_siege(app: tauri::AppHandle, window: tauri::WebviewWindow, plugin: Option<String>, server: u32) -> Result<Option<Value>, String> {
    let id = window_plugin(&window, plugin.as_deref())?;
    require(&app, &id, "feed:boss-timers")?;
    if !(1000..10000).contains(&server) {
        return Err("not a server id".into());
    }
    feed_file(&app, "abyss-artifacts", server).await
}

#[cfg(feature = "online")]
async fn boss_timers(app: &tauri::AppHandle, server: u32) -> Result<Option<Value>, String> {
    feed_file(app, "boss-timers", server).await
}

#[cfg(not(feature = "online"))]
async fn boss_timers(_app: &tauri::AppHandle, _server: u32) -> Result<Option<Value>, String> {
    Err("this meter is built without network features".into())
}

/// One server's file of a feed on the CDN (`feeds/<feed>/<server>.json`).
#[cfg(feature = "online")]
async fn feed_file(app: &tauri::AppHandle, feed: &'static str, server: u32) -> Result<Option<Value>, String> {
    static CACHE: std::sync::OnceLock<Mutex<HashMap<(&'static str, u32), (Instant, Option<Value>)>>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Mutex::default);
    let key = (feed, server);
    // As long as the CDN keeps the file (max-age 2): a plugin following a
    // boss that is being fought reads it every few seconds, for its HP as it
    // falls and its kill (Event Timers' pinned bosses).
    if let Some((at, value)) = cache.lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
        if at.elapsed() < Duration::from_secs(2) {
            return Ok(value.clone());
        }
    }
    let state = app.try_state::<AppState>().ok_or("no app state")?;
    let url = format!("https://cdn.a2tools.app/feeds/{feed}/{server}.json");
    let res = state.http.get(&url).timeout(Duration::from_secs(15)).send().await.map_err(|e| e.to_string())?;
    let value = match res.status() {
        s if s == reqwest::StatusCode::NOT_FOUND => None,
        s if s.is_success() => Some(serde_json::from_str(&res.text().await.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?),
        s => return Err(format!("the feed answered {s}")),
    };
    cache.lock().unwrap_or_else(|p| p.into_inner()).insert(key, (Instant::now(), value.clone()));
    Ok(value)
}

#[cfg(not(feature = "online"))]
async fn feed_file(_app: &tauri::AppHandle, _feed: &'static str, _server: u32) -> Result<Option<Value>, String> {
    Err("this meter is built without network features".into())
}

#[tauri::command]
pub fn plugin_storage_get(app: tauri::AppHandle, window: tauri::WebviewWindow, plugin: Option<String>, key: String) -> Result<Option<String>, String> {
    let id = window_plugin(&window, plugin.as_deref())?;
    require(&app, &id, "storage")?;
    Ok(std::fs::read_to_string(data_file(&app, &id, &key)?).ok())
}

#[tauri::command]
pub fn plugin_storage_set(app: tauri::AppHandle, window: tauri::WebviewWindow, plugin: Option<String>, key: String, value: String) -> Result<(), String> {
    let id = window_plugin(&window, plugin.as_deref())?;
    require(&app, &id, "storage")?;
    if value.len() > MAX_STORED {
        return Err("value larger than 1 MB".into());
    }
    let file = data_file(&app, &id, &key)?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        if !file.exists() && std::fs::read_dir(dir).map_or(0, |d| d.count()) >= MAX_STORED_KEYS {
            return Err(format!("at most {MAX_STORED_KEYS} storage keys"));
        }
    }
    std::fs::write(file, value).map_err(|e| e.to_string())
}

// ── The plugin files ───────────────────────────────────────────────────────

/// The `a2` object plugins load from `/_sdk/a2.js`.
const SDK: &str = include_str!("a2.js");

/// Serve `/<id>/<path>` from that plugin's folder, and `/_sdk/a2.js`.
/// The content policy for a plugin's files: its own folder and the SDK, and
/// nothing from anywhere else (no network at all).
///
/// The sources are named outright. A plugin page runs in a sandboxed frame,
/// whose origin is opaque, and WebKit (the Linux webview) matches `'self'`
/// against that opaque origin, so with `'self'` alone it refused the page's
/// own scripts and styles and plugins showed up unstyled and empty (2.0.60).
/// Chromium (WebView2) matches `'self'` against the URL instead. On Linux the
/// scheme itself is allowed too, in case WebKit does not match a path on a
/// custom scheme: that lets a plugin load another plugin's files, which gives
/// it nothing (each runs in the same sandbox, with only its own permissions).
fn content_policy(id: &str) -> String {
    let base = if cfg!(windows) { "http://a2plugin.localhost" } else { "a2plugin://localhost" };
    let mut own = format!("'self' {base}/_sdk/a2.js");
    if valid_id(id) {
        own.push_str(&format!(" {base}/{id}/"));
    }
    if !cfg!(windows) {
        own.push_str(" a2plugin:");
    }
    format!(
        "default-src 'none'; script-src {own}; style-src {own} 'unsafe-inline'; img-src {own} data:;          font-src {own} data:; connect-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'"
    )
}

pub fn serve(app: &tauri::AppHandle, path: &str) -> tauri::http::Response<Vec<u8>> {
    let path = percent_decode(path.trim_start_matches('/'));
    let policy = content_policy(path.split('/').next().unwrap_or(""));
    let respond = |status: u16, kind: &str, body: Vec<u8>| {
        tauri::http::Response::builder()
            .status(status)
            .header("Content-Type", kind)
            .header("Content-Security-Policy", policy.as_str())
            .header("X-Content-Type-Options", "nosniff")
            .body(body)
            .unwrap_or_default()
    };
    if path == "_sdk/a2.js" {
        return respond(200, "text/javascript; charset=utf-8", SDK.as_bytes().to_vec());
    }
    let Some((id, rest)) = path.split_once('/') else { return respond(404, "text/plain", Vec::new()) };
    let Ok(host) = host(app) else { return respond(404, "text/plain", Vec::new()) };
    let Some(dir) = host.with(id, |l| l.dir.clone()) else { return respond(404, "text/plain", Vec::new()) };
    // Inside the plugin's folder only.
    let (Ok(root), Ok(file)) = (dir.canonicalize(), dir.join(rest).canonicalize()) else {
        return respond(404, "text/plain", Vec::new());
    };
    if !file.starts_with(&root) || !file.is_file() {
        return respond(404, "text/plain", Vec::new());
    }
    // On Linux a plugin page gets its scripts, styles and images inlined:
    // WebKitGTK (2.54 at least) refuses every file a sandboxed frame (an
    // opaque origin) asks for from a custom scheme ("Unsafe attempt to load
    // URL … from origin null"), before the page's policy is even consulted,
    // so plugins showed their bare HTML and nothing ran (2.0.60, 2.0.61).
    // Inlining keeps the frame's opaque-origin sandbox, which
    // `allow-same-origin` (the other way out) would give up.
    if !cfg!(windows) && file.extension().is_some_and(|e| e == "html") {
        if let Ok(html) = std::fs::read_to_string(&file) {
            let (page, hashes) = inline_page(&html, &root, file.parent().unwrap_or(&root));
            let page = with_meter_fonts(&page);
            let policy = inlined_policy(id, &hashes);
            return tauri::http::Response::builder()
                .status(200)
                .header("Content-Type", "text/html; charset=utf-8")
                .header("Content-Security-Policy", policy.as_str())
                .header("X-Content-Type-Options", "nosniff")
                .body(page.into_bytes())
                .unwrap_or_default();
        }
    }
    let kind = match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "jpg" | "jpeg" => "image/jpeg",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    };
    match std::fs::read(&file) {
        Ok(body) => respond(200, kind, body),
        Err(_) => respond(404, "text/plain", Vec::new()),
    }
}

/// A plugin page with its `<script src>`, stylesheet `<link>` and `<img src>`
/// replaced by the files themselves (images as `data:` URLs), and the SHA-256
/// of each inlined script for the policy. Only the SDK and files inside the
/// plugin's folder are inlined; any other tag is left as it is (and stays
/// refused).
fn inline_page(html: &str, root: &Path, page_dir: &Path) -> (String, Vec<String>) {
    let file_of = |src: &str| -> Option<PathBuf> {
        if src.contains(':') || src.starts_with('/') {
            return None;
        }
        let file = page_dir.join(percent_decode(src.split(['?', '#']).next()?)).canonicalize().ok()?;
        file.starts_with(root).then_some(file)
    };
    let read = |src: &str| -> Option<String> {
        if src.trim_start_matches('/') == "_sdk/a2.js" {
            return Some(SDK.to_string());
        }
        std::fs::read_to_string(file_of(src)?).ok()
    };
    let image = |src: &str| -> Option<String> {
        let file = file_of(src)?;
        let kind = match file.extension()?.to_str()? {
            "png" => "image/png",
            "svg" => "image/svg+xml",
            "webp" => "image/webp",
            "gif" => "image/gif",
            "jpg" | "jpeg" => "image/jpeg",
            _ => return None,
        };
        Some(format!("data:{kind};base64,{}", base64(&std::fs::read(file).ok()?)))
    };
    let mut out = String::with_capacity(html.len() * 2);
    let mut hashes = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest.find('>') else { break };
        let tag = &rest[..=end];
        let lower = tag.to_ascii_lowercase();
        if lower.starts_with("<script") && rest[end + 1..].to_ascii_lowercase().starts_with("</script>") {
            if let Some(code) = attribute(tag, "src").and_then(|src| read(&src)) {
                // "</script" inside the code would end the element early;
                // and the page's parser turns CR LF and CR into LF before the
                // policy hashes a script, so a file with Windows line endings
                // (a plugin developer's, or the SDK built from a Windows
                // checkout) was refused, and no plugin ran (2026-10-28).
                let code = code.replace("</script", "<\\/script").replace("\r\n", "\n").replace('\r', "\n");
                hashes.push(base64(&sha2::Sha256::digest(code.as_bytes())));
                out.push_str("<script>");
                out.push_str(&code);
                out.push_str("</script>");
                rest = &rest[end + 1 + "</script>".len()..];
                continue;
            }
        } else if lower.starts_with("<link") && attribute(tag, "rel").is_some_and(|r| r.eq_ignore_ascii_case("stylesheet")) {
            if let Some(css) = attribute(tag, "href").and_then(|href| read(&href)) {
                out.push_str("<style>");
                out.push_str(&css.replace("</style", "<\\/style"));
                out.push_str("</style>");
                rest = &rest[end + 1..];
                continue;
            }
        } else if lower.starts_with("<img") {
            if let Some((src, data)) = attribute(tag, "src").and_then(|src| image(&src).map(|d| (src, d))) {
                out.push_str(&tag.replacen(&src, &data, 1));
                rest = &rest[end + 1..];
                continue;
            }
        }
        out.push_str(tag);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    (out, hashes)
}

/// An attribute's value in an opening tag, quoted or not.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let at = from + i;
        from = at + name.len();
        if !lower[..at].chars().last().is_some_and(char::is_whitespace) {
            continue;
        }
        let Some(after) = tag[from..].trim_start().strip_prefix('=') else { continue };
        let after = after.trim_start();
        let value = match after.chars().next()? {
            q @ ('"' | '\'') => after[1..].split(q).next()?,
            _ => after.split(|c: char| c.is_whitespace() || c == '>' || c == '/').next()?,
        };
        return Some(value.to_string());
    }
    None
}

/// Standard base64, with padding.
fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { ABC[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// The policy for an inlined page: only its own inlined scripts run (by
/// hash), its inlined styles apply, and nothing loads from anywhere.
/// The meter's Linux fonts, under the names plugins ask for: Linux has
/// neither Segoe UI, Pretendard nor Bahnschrift, so plugin text fell back to
/// the system face while the meter itself shipped Saira and Pretendard. A
/// plugin page cannot load the meter's files (see `serve`), so they come in
/// the page itself, as `data:` URLs its policy allows. Latin only, as each
/// page carries them: other scripts use the system's fonts as before.
fn meter_fonts_style() -> &'static str {
    static STYLE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    STYLE.get_or_init(|| {
        let pretendard = base64(include_bytes!("fonts/PretendardLatin.woff2"));
        let saira = base64(include_bytes!("fonts/Saira.woff2"));
        let face = |family: &str, data: &str, extra: &str| {
            format!("@font-face{{font-family:\"{family}\";src:url(data:font/woff2;base64,{data}) format(\"woff2\");{extra}}}")
        };
        let text = "font-weight:45 920;unicode-range:U+0000-00FF,U+0131,U+0152-0153,U+02BB-02BC,U+02C6,U+02DA,U+02DC,U+2000-206F,U+2074,U+20AC,U+2122,U+2190-2199,U+2212,U+2215;";
        let narrow = "font-weight:100 900;font-stretch:50% 125%;";
        format!(
            "<style>{}{}{}</style>",
            face("Pretendard", &pretendard, text),
            face("Segoe UI", &pretendard, text),
            face("Bahnschrift", &saira, narrow),
        )
    })
}

/// A plugin page with the meter's fonts at the start of its head.
fn with_meter_fonts(page: &str) -> String {
    let at = page
        .to_ascii_lowercase()
        .find("<head")
        .and_then(|i| page[i..].find('>').map(|j| i + j + 1))
        .unwrap_or(0);
    format!("{}{}{}", &page[..at], meter_fonts_style(), &page[at..])
}

fn inlined_policy(id: &str, hashes: &[String]) -> String {
    let scripts: String = hashes.iter().map(|h| format!(" 'sha256-{h}'")).collect();
    let own = if valid_id(id) { format!("a2plugin://localhost/{id}/") } else { "'none'".into() };
    format!(
        "default-src 'none'; script-src{}; style-src 'unsafe-inline'; img-src {own} data:; font-src {own} data:; \
         connect-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'",
        if scripts.is_empty() { " 'none'".to_string() } else { scripts }
    )
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        // Read as bytes: a `%` before a non-ASCII character would split it
        // as a str, and the panic ends the protocol handler (audit L7).
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn percent_decode_takes_any_text() {
        assert_eq!(super::percent_decode("a%20b%2Fc"), "a b/c");
        assert_eq!(super::percent_decode("%é%"), "%é%");
        assert_eq!(super::percent_decode("x%4"), "x%4");
    }

    #[test]
    fn plugin_windows_reach_only_their_own_commands() {
        for label in ["plugin-ping", "plugins-dock", "pw_ping_ping"] {
            assert!(allowed_from(label, "plugin_storage_get"), "{label}");
            assert!(allowed_from(label, "open_url"), "{label}");
            for cmd in ["show_update_window", "send_logs_to_dev", "plugin_add_folder", "plugin_install", "update_settings", "capture_screenshot", "set_run_as_admin"] {
                assert!(!allowed_from(label, cmd), "{label} {cmd}");
            }
        }
        for label in ["main", "settings", "history", "details-123"] {
            assert!(allowed_from(label, "plugin_add_folder"), "{label}");
            assert!(allowed_from(label, "show_update_window"), "{label}");
        }
    }

    use super::{allowed_from, base64, content_policy, inline_page, inlined_policy, with_meter_fonts, prompt_texts, should_start, take_ready};
    use super::{update_prompt, updates_to_offer, within_reach, Pending, PluginAvailable, PluginInfo, LANGUAGES, SETTLE};
    use std::collections::{HashMap, HashSet};
    use std::time::{Duration, Instant};

    fn available(id: &str, from: &str, to: &str, new_permissions: &[&str]) -> PluginAvailable {
        PluginAvailable {
            id: id.into(),
            name: format!("Plugin {id}"),
            from: from.into(),
            to: to.into(),
            new_permissions: new_permissions.iter().map(|p| p.to_string()).collect(),
            changes: Vec::new(),
        }
    }

    /// Linux plugin pages get the meter's fonts, inside their head, under
    /// the names plugins use.
    #[test]
    fn plugin_pages_carry_the_meter_fonts_in_their_head() {
        let page = with_meter_fonts("<!doctype html><html><head><title>x</title></head><body></body></html>");
        let head = page.find("<head>").unwrap() + "<head>".len();
        assert!(page[head..].starts_with("<style>@font-face{font-family:\"Pretendard\""), "{}", &page[..200]);
        for family in ["Segoe UI", "Bahnschrift"] {
            assert!(page.contains(&format!("font-family:\"{family}\"")), "{family}");
        }
        assert!(page.ends_with("<title>x</title></head><body></body></html>"));
        // No head: at the start.
        assert!(with_meter_fonts("<p>hi</p>").starts_with("<style>"));
    }

    #[test]
    fn an_update_said_no_to_is_not_offered_again_but_a_newer_one_is() {
        let declined = HashMap::from([("a".to_string(), "1.1.0".to_string()), ("b".to_string(), "2.0.0".to_string())]);
        let offered = updates_to_offer(
            vec![(available("a", "1.0.0", "1.1.0", &[]), ()), (available("b", "2.0.0", "2.1.0", &[]), ()), (available("c", "0.1", "0.2", &[]), ())],
            &declined,
        );
        let ids: Vec<_> = offered.iter().map(|(a, _)| a.id.as_str()).collect();
        assert_eq!(ids, ["b", "c"]);
    }

    #[test]
    fn the_start_prompt_lists_each_update_and_what_it_newly_asks_for() {
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/data");
        let en = prompt_texts(Some(&data), "en");
        let one = update_prompt(&en, &[&available("a", "1.0.0", "1.1.0", &[])]);
        assert_eq!(one, "A plugin update is available:\n\nPlugin a 1.0.0 → 1.1.0\n\nUpdate now?");
        let (a, b) = (available("a", "1.0.0", "1.1.0", &[]), available("b", "2.0", "2.1", &["storage", "decoder:pets"]));
        let two = update_prompt(&en, &[&a, &b]);
        assert!(two.starts_with("Plugin updates are available:"));
        assert!(two.contains("Plugin b 2.0 → 2.1\n    The new version also asks to:\n    • Save its own data on this computer\n    • Read selected game fields (may include other players): pets"));
        let de = update_prompt(&prompt_texts(Some(&data), "de"), &[&b]);
        assert!(de.contains("Jetzt aktualisieren?") && de.contains("Ausgewählte Spieldaten lesen (können andere Spieler betreffen): pets"), "{de}");
    }

    /// What's new: each version's notes under its update, in the player's
    /// language, or English where there's none in it.
    #[test]
    fn the_start_prompt_says_what_each_update_adds() {
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/data");
        let note = |version: &str, pairs: &[(&str, &str)]| super::Change {
            version: version.into(),
            text: pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        };
        let mut a = available("a", "1.0.0", "1.2.0", &["storage"]);
        a.changes = vec![note("1.2.0", &[("en", "Rows line up"), ("de", "Zeilen in einer Linie")]), note("1.1.0", &[("en", "A new voice")])];
        let en = update_prompt(&prompt_texts(Some(&data), "en"), &[&a]);
        assert!(en.contains("Plugin a 1.0.0 → 1.2.0\n    – Rows line up\n    – A new voice\n    The new version also asks to:"), "{en}");
        let de = update_prompt(&prompt_texts(Some(&data), "de"), &[&a]);
        assert!(de.contains("– Zeilen in einer Linie\n    – A new voice"), "{de}");
    }

    #[test]
    fn a_theme_is_three_hex_colours_and_nothing_else() {
        let ok = serde_json::json!({ "bg": "#0B1322", "panel": "#0e182b", "accent": "#22b8cf", "extra": "x" });
        assert_eq!(super::clean_theme(&ok), Some(serde_json::json!({ "bg": "#0b1322", "panel": "#0e182b", "accent": "#22b8cf" })));
        for bad in [
            serde_json::json!({ "bg": "#0b1322", "panel": "#0e182b" }),
            serde_json::json!({ "bg": "red", "panel": "#0e182b", "accent": "#22b8cf" }),
            serde_json::json!({ "bg": "#0b1322;}body{", "panel": "#0e182b", "accent": "#22b8cf" }),
            serde_json::json!({ "bg": "#0b13", "panel": "#0e182b", "accent": "#22b8cf" }),
        ] {
            assert_eq!(super::clean_theme(&bad), None, "{bad}");
        }
    }

    #[test]
    fn the_update_prompt_has_words_in_every_language() {
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/data");
        let en = prompt_texts(Some(&data), "en");
        for lang in LANGUAGES {
            let t = prompt_texts(Some(&data), lang);
            assert!(t.perm_decoder.contains("{name}"), "{lang}");
            if lang != "en" {
                for (word, english) in [(&t.title, &en.title), (&t.many, &en.many), (&t.update_now, &en.update_now), (&t.failed, &en.failed), (&t.perm_storage, &en.perm_storage)] {
                    assert_ne!(word, english, "{lang} is translated");
                }
            }
        }
        assert_eq!(prompt_texts(None, "ko").update_now, en.update_now, "no data: the built-in English");
    }

    #[test]
    fn a_reload_starts_enabled_plugins_but_not_one_closed_with_x_or_turned_off() {
        let plugin = |id: &str, error: Option<&str>| PluginInfo {
            id: id.into(),
            name: id.into(),
            version: "1.0.0".into(),
            description: String::new(),
            path: String::new(),
            permissions: Vec::new(),
            reviewed: true,
            error: error.map(str::to_string),
        };
        let off = vec!["off".to_string()];
        let closed = HashSet::from(["closed".to_string()]);
        assert!(should_start(&plugin("a", None), &off, &closed, false));
        assert!(!should_start(&plugin("a", None), &off, &closed, true), "already running");
        assert!(!should_start(&plugin("off", None), &off, &closed, false), "turned off");
        assert!(!should_start(&plugin("closed", None), &off, &closed, false), "closed with × this run");
        assert!(!should_start(&plugin("a", Some("bad manifest")), &off, &closed, false), "failed to load");
    }

    #[test]
    fn a_window_left_on_a_screen_goes_back_there_and_one_off_every_screen_does_not() {
        // Two 1920x1080 screens side by side, the left with a 40 px taskbar.
        let screens = [(0, 0, 1920, 1040), (1920, 0, 1920, 1080)];
        assert!(within_reach(100, 100, 300, &screens));
        assert!(within_reach(2500, 900, 300, &screens), "on the second screen");
        assert!(within_reach(-250, 100, 300, &screens), "mostly off the left edge, 50 px still showing");
        assert!(!within_reach(-280, 100, 300, &screens), "20 px showing is not enough to grab");
        assert!(!within_reach(4000, 100, 300, &screens), "where a third screen was");
        assert!(!within_reach(100, -20, 300, &screens), "its bar above the top");
        assert!(!within_reach(100, 1020, 300, &screens), "its bar behind the taskbar");
        assert!(within_reach(4000, 100, 300, &[]), "no screens known: trust the saved place");
    }

    #[test]
    fn a_page_gets_its_own_scripts_styles_and_images_inlined() {
        let dir = std::env::temp_dir().join(format!("a2plugin-inline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("plugin")).unwrap();
        std::fs::write(dir.join("plugin/plugin.js"), "go('</script>');").unwrap();
        std::fs::write(dir.join("plugin/style.css"), "body{color:red}").unwrap();
        std::fs::write(dir.join("plugin/dot.png"), [1u8, 2, 3]).unwrap();
        std::fs::write(dir.join("secret.js"), "nope").unwrap();
        let root = dir.join("plugin").canonicalize().unwrap();
        let html = concat!(
            "<link rel=\"stylesheet\" href=\"style.css\" />",
            "<script src=\"/_sdk/a2.js\"></script>",
            "<script src=\"plugin.js\"></script>",
            "<script src=\"../secret.js\"></script>",
            "<script src=\"https://example.com/x.js\"></script>",
            "<img alt=\"\" src=\"dot.png\">",
        );
        let (page, hashes) = inline_page(html, &root, &root);
        assert!(page.contains("<style>body{color:red}</style>"), "{page}");
        assert!(page.contains("window.a2 = Object.freeze"), "the SDK");
        assert!(page.contains("<script>go('<\\/script>');</script>"), "{page}");
        assert!(page.contains("src=\"data:image/png;base64,AQID\""), "{page}");
        // Outside the folder or elsewhere: left as a tag, which the policy refuses.
        assert!(page.contains("<script src=\"../secret.js\"></script>"));
        assert!(page.contains("https://example.com/x.js"));
        assert_eq!(hashes.len(), 2);
        let policy = inlined_policy("plugin", &hashes);
        assert!(policy.contains(&format!("script-src 'sha256-{}' 'sha256-{}';", hashes[0], hashes[1])), "{policy}");
        assert!(policy.contains("connect-src 'none'"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn base64_of_a_sha256() {
        use sha2::Digest;
        // printf abc | sha256sum | xxd -r -p | base64
        assert_eq!(base64(&sha2::Sha256::digest(b"abc")), "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=");
        assert_eq!(base64(b"ab"), "YWI=");
    }

    #[test]
    fn the_policy_names_the_plugins_own_files() {
        let base = if cfg!(windows) { "http://a2plugin.localhost" } else { "a2plugin://localhost" };
        let policy = content_policy("pet-souls");
        for directive in ["script-src", "style-src"] {
            let sources = policy.split(';').find(|d| d.trim().starts_with(directive)).unwrap();
            // Not 'self' alone: WebKit refuses everything in a sandboxed frame then.
            assert!(sources.contains(&format!("{base}/pet-souls/")), "{sources}");
            assert!(sources.contains(&format!("{base}/_sdk/a2.js")), "{sources}");
        }
        assert!(policy.contains("connect-src 'none'"));
        // A path that is not a plugin id adds no folder.
        assert!(!content_policy("../x").contains("/../x/"));
    }

    fn event(plugin: &str, name: &str, settle: bool, at: Instant) -> Pending {
        Pending { at, settle, read_ms: 0, plugin: plugin.into(), event: name.into(), data: serde_json::Value::Null }
    }

    fn names(events: &[Pending]) -> Vec<&str> {
        events.iter().map(|p| p.event.as_str()).collect()
    }

    /// A soul pickup (immediate) goes at once, unless the login's pet list,
    /// which waits for the character record, came before it: then it waits
    /// behind it, so the list never replaces counts the pickup already raised.
    #[test]
    fn an_immediate_event_goes_at_once_but_not_ahead_of_its_plugins_waiting_ones() {
        let now = Instant::now();
        let mut pending = vec![event("pets", "soul", false, now), event("cube", "spawn", false, now)];
        assert_eq!(names(&take_ready(&mut pending, now)), ["soul", "spawn"]);
        assert!(pending.is_empty());

        let mut pending = vec![
            event("pets", "list", true, now),
            event("pets", "soul", false, now),
            event("cube", "spawn", false, now),
        ];
        assert_eq!(names(&take_ready(&mut pending, now)), ["spawn"], "another plugin's event isn't held");
        assert_eq!(names(&pending), ["list", "soul"]);
        let later = now + SETTLE + Duration::from_millis(1);
        assert_eq!(names(&take_ready(&mut pending, later)), ["list", "soul"]);
        assert!(pending.is_empty());
    }
}

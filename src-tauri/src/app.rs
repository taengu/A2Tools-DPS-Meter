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

use std::collections::HashSet;
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
}

/// The overlay's click-through lock: while locked, clicks go through the meter
/// to the game, and it cannot be dragged. Only its own lock button stays
/// clickable, so the same button unlocks it; a hotkey does too.
///
/// A window either takes the mouse or ignores it, all of it, so the button
/// is kept clickable by watching the pointer: while it is over the button the
/// window takes the mouse again. That needs the pointer's position outside
/// the window, which Wayland does not give (`platform::window::cursor_position`),
/// so the lock is offered only where it is available.
#[derive(Default)]
pub struct OverlayLock {
    locked: std::sync::atomic::AtomicBool,
    /// The lock button in the main window's page: CSS-pixel x, y, width,
    /// height, and the page's scale. Sent by the page when it lays out.
    button: Mutex<Option<(f64, f64, f64, f64, f64)>>,
    /// Whether the pointer watch is running.
    watching: std::sync::atomic::AtomicBool,
    /// What the window was last told (`true` = ignore the mouse). Every change
    /// goes through `sync_click_through` under this lock, so a late pointer
    /// check can never leave an unlocked window click-through.
    applied: Mutex<Option<bool>>,
}

/// Tell the main window whether to ignore the mouse: when locked, unless the
/// pointer is over the lock button.
fn sync_click_through(app: &tauri::AppHandle, lock: &OverlayLock, over_button: bool) {
    use std::sync::atomic::Ordering;
    let mut applied = lock.applied.lock();
    let ignore = lock.locked.load(Ordering::SeqCst) && !over_button;
    if *applied == Some(ignore) {
        return;
    }
    if let Some(main) = app.get_webview_window("main") {
        if main.set_ignore_cursor_events(ignore).is_ok() {
            *applied = Some(ignore);
        }
    }
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

/// The buffs and debuffs of the live fight on a target (Details' Buffs section).
#[tauri::command]
async fn get_fight_buffs(app: tauri::AppHandle, target_id: i32) -> Result<Option<crate::combat::fight_buffs::LiveFightBuffs>, String> {
    crate::blocking::CALCULATIONS.run(move || {
        details_reader(&app.state::<AppState>()).live_fight_buffs(target_id)
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
fn save_fight_records(app: &tauri::AppHandle, state: &AppState, records: Vec<FightRecord>) {
    for record in &records {
        let _ = state.fight_history.save_fight(record);
        if !record.is_train {
            if let Err(e) = share::save_slice(&state.app_data_dir, record, &state.data_storage) {
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
    if state.settings.get(share::AUTO_UPLOAD_KEY).as_deref() == Some("true") {
        let now = crate::clock::now_ms();
        for record in records.into_iter()
            .filter(|r| share::wants_auto_upload(&state.app_data_dir, r, now))
        {
            auto_upload(app.clone(), record);
        }
    }
}

/// Save every fight on the meter now, before its combat data is cleared: a
/// zone change, the end of a party, the reset button, the reload hotkey or
/// quitting. Each cleared it unsaved, and leaving an instance right after a
/// kill lost everything since the last 30-second save (issue #19).
fn save_fights_before_reset(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<AppState>() else { return };
    let _saving = state.fight_save.lock();
    if state.data_storage.damage_generation() <= 0 {
        return;
    }
    let records = state.dps_calculator.lock().snapshot_boss_fights_force();
    if !records.is_empty() {
        tracing::info!("Saving {} fight(s) before combat data is cleared", records.len());
    }
    save_fight_records(app, &state, records);
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
/// The point is auditability: it produces the exact `.a2es` and `.upload.json`
/// an upload would send, so a user can open them — `a2t-inspect` reads the
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
    *state.account_seen.lock() = Some(who.clone());
    Ok(who)
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
        let payload = match &outcome {
            Ok(()) => serde_json::json!({ "connected": true }),
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

/// Store a setting and tell every window about it.
///
/// Settings are edited in their own window, so without this broadcast the meter
/// keeps rendering with whatever it read at startup — toggling something like
/// "Round DPS" would appear to do nothing until the app restarted. Only real
/// changes are emitted (see `Settings::set`), so the originating window's echo
/// stops here rather than bouncing between windows.
/// The All Targets time range (Settings), in milliseconds.
const ALL_TARGETS_WINDOW_KEY: &str = "dpsMeter.allTargetsWindowMs";

fn apply_all_targets_window(state: &AppState, value: &str) {
    if let Ok(ms) = value.trim().parse::<i64>() {
        state.dps_calculator.lock().set_all_targets_window_ms(ms);
    }
}

#[tauri::command]
fn update_settings(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    key: String,
    value: String,
) {
    if state.settings.set(&key, &value) {
        if key == ALL_TARGETS_WINDOW_KEY {
            apply_all_targets_window(&state, &value);
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
fn set_language(state: tauri::State<'_, AppState>, language: String) {
    tracing::info!("Language change requested: {}", language);
    if let Some(ref data_dir) = state.i18n_data_dir {
        i18n::lookup::load_language(&state.skill_lookup, &state.npc_lookup, data_dir, &language);
    } else {
        tracing::warn!("No i18n data dir available for language reload");
    }
    state.settings.set("dpsMeter.language", &language);
}

#[tauri::command]
fn set_debug_logging(state: tauri::State<'_, AppState>, enabled: bool) {
    logging::logger::set_debug_enabled(enabled, &state.app_data_dir);
    state.settings.set("dpsMeter.debugLoggingEnabled", if enabled { "true" } else { "false" });
}

#[tauri::command]
fn set_packet_logging(state: tauri::State<'_, AppState>, enabled: bool) {
    logging::logger::set_packet_log_enabled(enabled, &state.app_data_dir);
    state.settings.set("dpsMeter.saveRawPackets", if enabled { "true" } else { "false" });
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
fn quit_app(app: tauri::AppHandle) {
    save_fights_before_reset(&app);
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

#[tauri::command]
fn read_cached_icon(state: tauri::State<'_, AppState>, key: String) -> Option<String> {
    if !crate::history::fight_history::is_plain_name(&key) {
        return None;
    }
    let path = state.app_data_dir.join("icon_cache").join(&key);
    std::fs::read_to_string(&path).ok()
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
    platform::window::cursor_position().is_some()
}

#[tauri::command]
fn set_overlay_locked(app: tauri::AppHandle, locked: bool) {
    apply_overlay_lock(&app, locked);
}

#[tauri::command]
fn is_overlay_locked(state: tauri::State<'_, AppState>) -> bool {
    state.overlay_lock.locked.load(std::sync::atomic::Ordering::SeqCst)
}

/// Where the lock button is in the main window's page, so it stays clickable
/// while the rest of the window lets clicks through.
#[tauri::command]
fn set_lock_button_rect(state: tauri::State<'_, AppState>, x: f64, y: f64, width: f64, height: f64, scale: f64) {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    *state.overlay_lock.button.lock() = Some((x, y, width, height, scale));
}

#[tauri::command]
fn is_capture_suspended(state: tauri::State<'_, AppState>) -> bool {
    state.capture_suspended.load(std::sync::atomic::Ordering::SeqCst)
}

#[tauri::command]
fn log_from_ui(message: String) {
    // A problem only the webview can see (an icon the CDN would not serve, say),
    // for debug.log. The UI keeps these few; this keeps each one short.
    let message: String = message.chars().take(300).collect();
    tracing::warn!("UI: {message}");
}

#[tauri::command]
fn write_cached_icon(state: tauri::State<'_, AppState>, key: String, data: String) {
    if !crate::history::fight_history::is_plain_name(&key) {
        return;
    }
    let cache_dir = state.app_data_dir.join("icon_cache");
    let _ = std::fs::create_dir_all(&cache_dir);
    let path = cache_dir.join(&key);
    let _ = std::fs::write(&path, &data);
}


#[cfg(feature = "online")]
#[tauri::command]
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
    if !platform::updater::supported() || package_url.is_empty() {
        tracing::info!("Update {} available (running {}); this install updates through its package manager", latest, current);
        return Ok(false);
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
fn package_hash_matches(expected: &str, actual_hex: &str) -> bool {
    let expected = expected.trim();
    expected.len() == 64
        && expected.bytes().all(|b| b.is_ascii_hexdigit())
        && expected.eq_ignore_ascii_case(actual_hex)
}

#[cfg(feature = "online")]
async fn download_and_install_update(app: &tauri::AppHandle, url: &str, expected_sha256: &str) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    use futures_util::StreamExt;
    use sha2::{Digest, Sha256};

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
    let total_size = response.content_length().unwrap_or(0);
    let file_name = url_owned.rsplit('/').next().unwrap_or("update");
    let msi_path = std::env::temp_dir().join(file_name);

    let mut file = tokio::fs::File::create(&msi_path).await.map_err(|e| e.to_string())?;
    let mut downloaded: u64 = 0;
    let mut stream = response.bytes_stream();
    let mut last_pct: u64 = 0;
    let mut hasher = Sha256::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        hasher.update(&chunk);
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;
        if total_size > 0 {
            let pct = (downloaded * 100 / total_size).min(100);
            if pct != last_pct {
                last_pct = pct;
                let _ = app_clone.emit("download-progress", pct);
                tracing::info!("Download: {}%", pct);
            }
        }
    }
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);

    let actual: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
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
    platform::updater::run_installer(&msi_path, &install_dir)?;

    // Give installer time to start, then exit
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    flush_settings_before_exit(&app_clone);
    app_clone.exit(0);
    Ok(())
}

#[cfg(feature = "online")]
#[tauri::command]
async fn fetch_url(state: tauri::State<'_, AppState>, url: String) -> Result<String, String> {
    state
        .http
        .get(&url)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())
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
    let Ok(bytes) = response.bytes().await else {
        return RosterFetch::Failed;
    };
    // A roster for ten thousand supporters is about 80 KB; anything far past
    // that is not one of ours.
    if bytes.len() > 8 * 1024 * 1024 {
        return RosterFetch::Failed;
    }
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
async fn send_logs_to_dev() -> Result<serde_json::Value, String> {
    Err(NOT_IN_THIS_BUILD.into())
}

#[cfg(not(feature = "online"))]
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn show_update_window(
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
) -> Result<bool, String> {
    let _ = (current, latest, msi_url, arch_url, deb_url, rpm_url, msi_sha256, arch_sha256, deb_sha256, rpm_sha256);
    Ok(false)
}

#[cfg(not(feature = "online"))]
#[tauri::command]
async fn fetch_url(url: String) -> Result<String, String> {
    let _ = url;
    Err(NOT_IN_THIS_BUILD.into())
}

/// Which optional parts this build has. `online` is false in the private
/// build: the pages hide the account, uploads, Streaming, Discord activity,
/// Send Logs to Dev and the update prompt.
#[tauri::command]
fn build_features() -> serde_json::Value {
    serde_json::json!({ "online": cfg!(feature = "online") })
}

#[tauri::command]
fn open_url(url: String) {
    platform::shell::open_url(&url);
}

#[tauri::command]
fn resize_window(app: tauri::AppHandle, width: f64, height: f64, scale: Option<f64>) {
    // Only the overlay auto-sizes itself. The details window is sized to a
    // whole monitor by open_details_window and must never be resized from JS.
    let Some(window) = app.get_webview_window("main") else { return };
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
    platform::window::set_size(&window, size);
}

/// Linux can load a hidden WebKit window. Map it only after restoring its
/// position and sizing the meter, so its initial 400x300 rectangle never flashes.
#[tauri::command]
fn main_window_ready(window: tauri::WebviewWindow) {
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
    let window = tauri::WebviewWindowBuilder::new(
        app,
        "details",
        tauri::WebviewUrl::App("index.html".into()),
    )
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
#[tauri::command]
async fn open_settings_window(app: tauri::AppHandle) -> Result<(), String> {
    let ready = platform::window_startup::request_settings_open();
    if let Some(existing) = app.get_webview_window("settings") {
        if !ready {
            return Ok(());
        }
        let _ = existing.show();
        let _ = existing.unminimize();
        let _ = existing.set_always_on_top(true);
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
    let window = tauri::WebviewWindowBuilder::new(
        app,
        "settings",
        tauri::WebviewUrl::App("index.html".into()),
    )
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
        // Raise it if it was put away, but do not steal focus from a window
        // that is already on screen — the click came from an overlay sitting
        // on top of a full-screen game, and pulling focus would tab out of it.
        let hidden = !window.is_visible().unwrap_or(true)
            || window.is_minimized().unwrap_or(false);
        if hidden {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        } else if kind == "fight" {
            // Re-asking for a fight that is already open means "show me that
            // one", so bring it forward even though it was never hidden.
            let _ = window.set_focus();
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
    let step = app.webview_windows().keys().filter(|l| l.starts_with("details-")).count() as f64;
    let offset = (step % 6.0) * 34.0;

    let window = tauri::WebviewWindowBuilder::new(
        app,
        label,
        tauri::WebviewUrl::App("index.html".into()),
    )
    .initialization_script("window.__A2_VIEW__ = 'details';")
    .title("A2Tools DPS Meter — Fight")
    .decorations(false)
    .transparent(false)
    .always_on_top(true)
    .resizable(true)
    .skip_taskbar(false)
    .inner_size(1180.0, 760.0)
    .min_inner_size(520.0, 360.0)
    .background_color(tauri::window::Color(10, 14, 22, 255))
    .build()
    .map_err(|e| e.to_string())?;
    platform::window::set_size(&window, tauri::Size::Logical(tauri::LogicalSize { width: 1180.0, height: 760.0 }));

    center_on_overlay_monitor(app, &window);
    if offset > 0.0 {
        if let Ok(pos) = window.outer_position() {
            let shift = offset as i32;
            let _ = window.set_position(tauri::Position::Physical(tauri::PhysicalPosition {
                x: pos.x + shift,
                y: pos.y + shift,
            }));
        }
    }
    Ok(())
}

/// The History window. Persistent by design — it is the browser you pick fights
/// from, and it stays put while those fights open in windows of their own.
fn open_history_window_inner(app: &tauri::AppHandle) -> Result<(), String> {
    let window = tauri::WebviewWindowBuilder::new(
        app,
        "history",
        tauri::WebviewUrl::App("index.html".into()),
    )
    .initialization_script("window.__A2_VIEW__ = 'history';")
    .title("A2Tools DPS Meter — Battle History")
    .decorations(false)
    .transparent(false)
    .always_on_top(true)
    .resizable(true)
    .skip_taskbar(false)
    .inner_size(1100.0, 720.0)
    .min_inner_size(480.0, 360.0)
    .background_color(tauri::window::Color(10, 14, 22, 255))
    .build()
    .map_err(|e| e.to_string())?;
    platform::window::set_size(&window, tauri::Size::Logical(tauri::LogicalSize { width: 1100.0, height: 720.0 }));

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
    let window = tauri::WebviewWindowBuilder::new(
        app,
        "details",
        tauri::WebviewUrl::App("index.html".into()),
    )
    .initialization_script("window.__A2_VIEW__ = 'details';")
    .title("A2Tools DPS Meter — Details")
    .decorations(false)
    .transparent(false)
    .always_on_top(true)
    .resizable(true)
    .skip_taskbar(false)
    .inner_size(1180.0, 760.0)
    .min_inner_size(520.0, 360.0)
    // Visible, with the app background painted behind the load — same reason as
    // the monitor-filling path: a hidden WebView2 window may never load at all.
    .background_color(tauri::window::Color(10, 14, 22, 255))
    .build()
    .map_err(|e| e.to_string())?;
    platform::window::set_size(&window, tauri::Size::Logical(tauri::LogicalSize { width: 1180.0, height: 760.0 }));

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
        let dir = folder
            .filter(|f| !f.trim().is_empty())
            .map(std::path::PathBuf::from)
            .or_else(platform::screenshot::default_folder)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let name = filename
            .filter(|n| !n.trim().is_empty() && !n.contains(['/', '\\']))
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

/// Where screenshots go when no folder has been chosen.
#[tauri::command]
fn default_screenshot_folder() -> String {
    platform::screenshot::default_folder()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

/// Let the player pick the screenshot folder. `None` if they cancel.
#[tauri::command]
async fn choose_screenshot_folder(
    webview_window: tauri::WebviewWindow,
    current: Option<String>,
) -> Option<String> {
    platform::screenshot::pick_folder(&webview_window, current.as_deref())
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
fn finish_window_resize(window: tauri::WebviewWindow, cancel: bool) -> Result<bool, String> {
    if platform::window::compositor_resize_supported(&window) {
        if !cancel && platform::window::resize_pointer_down(&window) != Some(false) {
            return Ok(false);
        }
        let size = window.inner_size().map_err(|e| e.to_string())?;
        platform::window::set_size(&window, tauri::Size::Physical(size));
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
pub fn run() {
    // Before anything starts a thread: it may set environment variables.
    let process_note = platform::process::prepare();
    logging::logger::init_logging();
    if let Some(note) = process_note {
        tracing::info!("{note}");
    }

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
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri::plugin::Builder::<_, ()>::new("settings-persistence")
            .on_event(|app, event| {
                // Flush before normal exit or restart, with Exit as a final safeguard.
                if matches!(event, tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit) {
                    flush_settings_before_exit(app);
                }
            })
            .build())
        .plugin(tauri_plugin_process::init())
        // Closing the meter quits, even with Details, History or a kept
        // Settings window still open.
        .on_window_event(|window, event| {
            if window.label() == "main"
                && let tauri::WindowEvent::CloseRequested { api, .. } = event
            {
                api.prevent_close();
                quit_app(window.app_handle().clone());
            }
        })
        .setup(|app| {
            // Resolve data directory
            let app_data_dir = app.path().app_data_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."));
            let _ = std::fs::create_dir_all(&app_data_dir);

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

            let settings = Settings::new(app_data_dir.clone());
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
            };
            let capture_suspended = state.capture_suspended.clone();

            app.manage(state);

            // On KDE, a KWin rule keeps the meter above a borderless game
            // (see platform::kwin_rules). Once: a rule the player removes
            // stays removed. Off the setup thread, as it runs KDE's tools.
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    const KEY: &str = "dpsMeter.kwinRuleAdded";
                    let state = handle.state::<AppState>();
                    let done = state.settings.get(KEY).as_deref() == Some("true");
                    match platform::window_rules::keep_above_fullscreen(done) {
                        Ok(true) if !done => {
                            state.settings.set(KEY, "true");
                        }
                        Ok(_) => {}
                        Err(e) => tracing::warn!("Could not add the KWin rule: {e}"),
                    }
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
                platform::window_startup::arm_main_fallback(window);
            }

            // Check if Npcap is available before starting capture
            let npcap_available = platform::pcap::library_available();
            if !npcap_available {
                tracing::error!("Npcap is not installed — packet capture disabled");
            }

            // Start capture pipeline
            let (tx, rx) = mpsc::channel::<CapturedPayload>(4096);

            let capturer = PcapCapturer::new(tx);
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

            // Register global hotkeys from saved settings (or defaults)
            let hotkey_handle = app.handle().clone();
            let hotkey_manager = platform::hotkeys::HotkeyManager::new();

            let reload_label = app.state::<AppState>().settings
                .get("dpsMeter.hotkey").unwrap_or_default();
            let toggle_label = app.state::<AppState>().settings
                .get("dpsMeter.toggleWindowHotkey").unwrap_or_default();
            let lock_label = app.state::<AppState>().settings
                .get("dpsMeter.lockHotkey").unwrap_or_default();

            let (reload_mods, reload_vk) = platform::hotkeys::parse_hotkey_label(&reload_label)
                .unwrap_or((0x0002 | 0x0001, 0x52)); // Default: Ctrl+Alt+R
            let (toggle_mods, toggle_vk) = platform::hotkeys::parse_hotkey_label(&toggle_label)
                .unwrap_or((0x0002 | 0x0001, 0x26)); // Default: Ctrl+Alt+Up
            let (lock_mods, lock_vk) = platform::hotkeys::parse_hotkey_label(&lock_label)
                .unwrap_or((0x0002 | 0x0001, 0x4C)); // Default: Ctrl+Alt+L

            hotkey_manager.start(
                reload_mods, reload_vk,
                toggle_mods, toggle_vk,
                lock_mods, lock_vk,
                {
                    let h = hotkey_handle.clone();
                    move || {
                        tracing::info!("Hotkey: reload triggered");
                        save_fights_before_reset(&h);
                        if let Some(state) = h.try_state::<AppState>() {
                            state.dps_calculator.lock().restart_target_selection(true);
                            state.data_storage.forget_guessed_nicknames();
                        }
                        // Notify frontend to clear UI
                        let _ = h.emit("combat-reset", ());
                        let _ = h.emit("dps-update", &entity::dps_data::DpsData::new());
                    }
                },
                {
                    let h = hotkey_handle.clone();
                    move || {
                        // Toggle window visibility
                        if let Some(window) = h.get_webview_window("main") {
                            if window.is_visible().unwrap_or(false) {
                                let _ = window.hide();
                            } else {
                                let _ = window.show();
                                let _ = window.set_always_on_top(true);
                                let _ = window.set_focus();
                            }
                        }
                    }
                },
                {
                    let h = hotkey_handle;
                    move || {
                        // Toggle the click-through lock, and tell the page so
                        // its button and saved setting follow.
                        let locked = h
                            .try_state::<AppState>()
                            .is_some_and(|s| s.overlay_lock.locked.load(std::sync::atomic::Ordering::SeqCst));
                        if platform::window::cursor_position().is_none() && !locked {
                            return; // the lock is not offered here
                        }
                        apply_overlay_lock(&h, !locked);
                        let _ = h.emit("overlay-lock-changed", !locked);
                    }
                },
            );

            // Periodic DPS update emission (every 500ms)
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_millis(500));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut tick_count: u64 = 0;
                let mut hide_delay: u64 = 0; // ticks to wait before hiding
                loop {
                    interval.tick().await;
                    tick_count += 1;

                    if let Some(state) = handle.try_state::<AppState>() {
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
                                let _ = handle.emit("dps-update", &dps);
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

                        if let Some(ping) = state.ping_tracker.current_ping_ms() {
                            let _ = handle.emit("ping-update", ping);
                        }

                        // --- Auto-hide when AION2 loses focus (every tick) ---
                        let auto_hide = tick_count > 20
                            && state.settings.get("dpsMeter.autoHideMeter")
                                .unwrap_or_default() == "true";
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
                                if aion_fg || is_self_fg {
                                    hide_delay = 0;
                                    if !is_visible || is_minimized {
                                        platform::window::show_on_top_without_focus(&window);
                                        // Notify frontend to recalculate window size
                                        // (content may have changed while minimized)
                                        let _ = window.emit("force-resize", ());
                                    }
                                } else if is_visible && !is_minimized {
                                    // Wait 3 ticks (1.5s) before hiding to avoid
                                    // flickering during alt-tab transitions
                                    hide_delay += 1;
                                    if hide_delay >= 3 {
                                        platform::window::minimize_off_top(&window);
                                    }
                                }
                            }
                        }

                        // --- Save window position every ~5 seconds (every 10 ticks) ---
                        if tick_count % 10 == 0 {
                            if let Some(window) = handle.get_webview_window("main") {
                                if let Ok(pos) = window.outer_position() {
                                    // Don't save minimized/hidden positions
                                    if pos.x > -10000 && pos.y > -10000 {
                                        state.settings.set("window.x", &pos.x.to_string());
                                        state.settings.set("window.y", &pos.y.to_string());
                                    }
                                }
                            }
                            // These float independently of the overlay, so each
                            // remembers where it was left. Per-fight windows
                            // (details-*) are deliberately absent: they are
                            // opened to be read and closed, and keying geometry
                            // by fight id would grow settings without bound.
                            for label in ["details", "settings", "history"] {
                                // While Details is pinned to a monitor its rect
                                // comes from the setting, not from the user.
                                // Saving it poisons the windowed geometry: turn
                                // the setting off and Details would reopen
                                // full-size on that same screen.
                                if label == "details"
                                    && details_monitor_setting(&handle).is_some()
                                {
                                    continue;
                                }
                                if let Some(w) = handle.get_webview_window(label) {
                                    if !w.is_visible().unwrap_or(false) {
                                        continue;
                                    }
                                    if let Ok(pos) = w.outer_position() {
                                        if pos.x > -10000 && pos.y > -10000 {
                                            state.settings.set(&format!("window.{}.x", label), &pos.x.to_string());
                                            state.settings.set(&format!("window.{}.y", label), &pos.y.to_string());
                                        }
                                    }
                                    // Inner, not outer: restore applies it with
                                    // set_size, which sets the inner size. Saving
                                    // the outer size grew every tool window by its
                                    // border (16x9 px here) on each reopen.
                                    if let Ok(size) = w.inner_size() {
                                        if size.width > 100 && size.height > 100 {
                                            state.settings.set(&format!("window.{}.w", label), &size.width.to_string());
                                            state.settings.set(&format!("window.{}.h", label), &size.height.to_string());
                                        }
                                    }
                                    // Details is unpinned here, but the window
                                    // may still be sitting on the fill rect from
                                    // before the setting was switched off. A rect
                                    // that swallows a whole screen is not a
                                    // placement anyone chose by dragging, so it
                                    // never earns the marker.
                                    if label == "details" {
                                        let filling = match (w.outer_position(), w.outer_size()) {
                                            (Ok(pos), Ok(size)) => rect_covers_a_monitor(&handle, pos, size),
                                            _ => true,
                                        };
                                        if !filling {
                                            state.settings.set(DETAILS_USER_PLACED_KEY, "true");
                                        }
                                    }
                                }
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
                                save_fight_records(&handle, &state, records);
                            }
                        }
                        // Automatic uploads that failed and are due again,
                        // fighting or not: a meter left open after the
                        // connection came back catches up on its own.
                        #[cfg(feature = "online")]
                        if state.settings.get(share::AUTO_UPLOAD_KEY).as_deref() == Some("true") {
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
        .invoke_handler(tauri::generate_handler![
            get_app_version,
            get_dps_snapshot,
            get_skill_details,
            get_details_context,
            get_fight_buffs,
            get_fight_history,
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
            set_overlay_locked,
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
            fetch_url,
            show_update_window,
            build_features,
        ])
        .run(context)
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    #[cfg_attr(not(feature = "online"), allow(unused_imports))]
    use super::*;

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

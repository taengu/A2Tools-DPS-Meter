//! Field boss timers, shared: what the player's map shows is sent to
//! a2tools.app, which checks it, combines it with other players' and
//! publishes each server's timers for everyone's Event Timers.
//!
//! Opt-in, with the Supply Requests: Settings' "Sync useful data to
//! A2Tools" (`shares_data`, explicit "true" enables it). What is sent: the server,
//! the map, and each field boss on it: up (since when) or dead (back when).
//! Nothing about the player; the site counts reporters by a hash of their
//! address that changes daily, and keeps no address.
//!
//! A map is sent when it says something new for that server: a boss up or
//! down, a new appearance or respawn time, a boss added or gone. The same map
//! goes again at most every RESEND_SAME_MS, which keeps the site's
//! confirmations (two reporters agreeing) coming without a request for every
//! time the map is opened (a2tools.app's Workers CPU, 2026-10-16). A meter started
//! mid-game doesn't know the server until the game says it: the newest map
//! waits until then, and goes if it is still under MAX_AGE_MS old. Only the
//! player's own server's maps: an enemy map viewed from the menu carries
//! nothing, and a map read from inside a rift (its header not 0) isn't sent
//! until it is known whose server it shows.
//!
//! Field boss kills in the open world go too, as they happen, and a field
//! boss's HP when it changes (checked every 3 seconds), and once a minute
//! while it is fought and holds; a jump back up is marked as a reset
//! (`spawn_events`). a2tools.app
//! passes a drop of a percent or more, and a reset, on to Aion 2 Timers.
//!
//! The Abyss's Dimensional Cores (`capture::abyss_cores`) go the same way
//! during the Artifact Siege, to /api/boss-timers/cores: seen up, killed,
//! and their HP, by NPC code.
//!
//! And who holds each Abyss artifact (the corridors' Artifact Siege), as the
//! game tells the player with the Abyss map and on entering the Abyss
//! (`capture::abyss_artifacts`): sent when it changes, and with the same
//! resend as a map. Under the same setting.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::json;
use tauri::Manager;

use crate::app::AppState;
use crate::capture::abyss_artifacts::ArtifactState;
use crate::capture::boss_map::BossMap;
use crate::combat::data_storage::{BossEvent, BossEventKind, BossHp};

/// Settings' "Sync useful data to A2Tools": field boss timers and Supply
/// Requests, off until explicitly enabled.
pub const SHARE_DATA_KEY: &str = "dpsMeter.shareData";
/// The two switches it replaced (2.0.78): migrate only when both were
/// explicitly enabled, until the player sets the new one.
const OLD_KEYS: [&str; 2] = ["dpsMeter.shareBossTimers", "dpsMeter.shareSupplyRequests"];

/// Whether the meter shares field boss timers and Supply Requests with
/// a2tools.app, `get` reading a setting.
pub fn shares_data(get: impl Fn(&str) -> Option<String>) -> bool {
    match get(SHARE_DATA_KEY).as_deref() {
        Some(v) => v == "true",
        None => OLD_KEYS.iter().all(|k| get(k).as_deref() == Some("true")),
    }
}

fn sharing(state: &AppState) -> bool {
    shares_data(|k| state.settings.get(k))
}
const CHECK: Duration = Duration::from_secs(2);
/// The same map, unchanged, is sent again after this long at the earliest.
const RESEND_SAME_MS: i64 = 5 * 60_000;
/// A map read longer ago than this is not sent (waiting for the server).
const MAX_AGE_MS: i64 = 10 * 60_000;

/// The report the site's /api/boss-timers/report reads (format 1).
pub fn report(server_id: u16, map: &BossMap, now_ms: i64) -> serde_json::Value {
    let secs = |ms: i64| (ms > 0).then_some(ms / 1000);
    let bosses: Vec<_> = map
        .bosses
        .iter()
        .map(|b| {
            if b.up {
                json!({ "id": b.id, "state": "up", "upSince": secs(b.time_ms) })
            } else {
                json!({ "id": b.id, "state": "dead", "nextSpawn": secs(b.time_ms) })
            }
        })
        .collect();
    json!({
        "format": 1,
        "server": server_id,
        "map": map.map_id,
        "observedAt": now_ms / 1000,
        "bosses": bosses,
    })
}

/// The report the site's /api/boss-timers/artifacts reads (format 1): each
/// Artifact Siege (AbyssArtifactEvent id) with when its holding began and
/// resets, and each artifact (AbyssArtifact id) with its holder.
pub fn artifacts_report(server_id: u16, state: &ArtifactState, seen_ms: i64) -> serde_json::Value {
    let secs = |ms: i64| (ms > 0).then_some(ms / 1000);
    let wars: Vec<_> = state
        .wars
        .iter()
        .map(|w| json!({ "event": w.event_id, "since": secs(w.since_ms), "until": secs(w.until_ms) }))
        .collect();
    let artifacts: Vec<_> = state
        .artifacts
        .iter()
        .map(|a| json!({ "id": a.id, "event": a.event_id(), "owner": a.owner.name() }))
        .collect();
    json!({
        "format": 1,
        "server": server_id,
        "observedAt": seen_ms / 1000,
        "wars": wars,
        "artifacts": artifacts,
    })
}

/// Korea and Taiwan number their servers the same (10xx Elyos, 20xx
/// Asmodian), so a report from one of them names the game server's address
/// too, which a2tools.app tells the two regions apart by. Global ids say
/// their region already and go without it.
pub fn with_host(mut body: serde_json::Value, server_id: u16, host: Option<std::net::Ipv4Addr>) -> serde_json::Value {
    let shared_id = (1000..1100).contains(&server_id) || (2000..2100).contains(&server_id);
    if let (true, Some(host), Some(obj)) = (shared_id, host, body.as_object_mut()) {
        obj.insert("host".into(), json!(host.to_string()));
    }
    body
}

/// A kill or HP milestone not sent yet waits this long for the server to be
/// known (the meter started mid-game).
const EVENT_WAIT_MS: i64 = 5 * 60_000;

/// HP is sent this often, for the bosses whose percent changed.
const HP_EVERY: Duration = Duration::from_secs(3);

/// The request for a kill: (path, body), or None for a boss a2tools.app
/// doesn't know.
pub fn event_request(server_id: u16, event: &BossEvent) -> Option<(&'static str, serde_json::Value)> {
    if crate::capture::abyss_cores::is_core(event.npc) {
        let (state, at_ms) = match event.kind {
            BossEventKind::Killed { at_ms } => ("dead", at_ms),
            BossEventKind::Seen { at_ms } => ("up", at_ms),
        };
        let core = json!({ "npc": event.npc, "artifact": crate::capture::abyss_cores::artifact_of(event.npc),
                           "state": state, "at": at_ms / 1000 });
        return Some(("/api/boss-timers/cores", cores_body(server_id, vec![core], at_ms)));
    }
    let id = crate::capture::field_bosses::world_map_id(event.npc)?;
    match event.kind {
        BossEventKind::Killed { at_ms } => Some((
            "/api/boss-timers/kill",
            json!({ "format": 1, "server": server_id, "id": id, "diedAt": at_ms / 1000 }),
        )),
        // A world boss's appearance comes with the maps.
        BossEventKind::Seen { .. } => None,
    }
}

/// The cores report /api/boss-timers/cores reads (format 1): each core by
/// NPC code, with its artifact, and what was seen: up or dead at a time, or
/// its HP.
fn cores_body(server_id: u16, cores: Vec<serde_json::Value>, now_ms: i64) -> serde_json::Value {
    json!({ "format": 1, "server": server_id, "observedAt": now_ms / 1000, "cores": cores })
}

/// The orb kills waiting for the faction that took their corridor
/// (`abyss_cores::OrbKillers`), between the kill loop and the holders' one.
fn orb_killers() -> &'static std::sync::Mutex<crate::capture::abyss_cores::OrbKillers> {
    static KILLERS: std::sync::OnceLock<std::sync::Mutex<crate::capture::abyss_cores::OrbKillers>> =
        std::sync::OnceLock::new();
    KILLERS.get_or_init(Default::default)
}

/// Settled orb kills for /api/boss-timers/cores: the kill again, with the
/// faction that dealt it (Aion 2 Timers' live board decides each corridor by
/// it).
pub fn orb_killers_request(server_id: u16, kills: &[crate::capture::abyss_cores::OrbKill], now_ms: i64) -> Option<serde_json::Value> {
    let cores: Vec<_> = kills
        .iter()
        .map(|k| json!({ "npc": k.npc, "artifact": k.artifact, "state": "dead", "at": k.at_ms / 1000, "killer": k.killer }))
        .collect();
    (!cores.is_empty()).then(|| cores_body(server_id, cores, now_ms))
}

async fn send_orb_killers(state: &AppState, server: u16, kills: Vec<crate::capture::abyss_cores::OrbKill>) {
    if !sharing(state) {
        return;
    }
    let Some(body) = orb_killers_request(server, &kills, crate::clock::now_ms()) else { return };
    let body = with_host(body, server, state.data_storage.game_host());
    let url = format!("{}/api/boss-timers/cores", crate::account::base_url());
    let request = state.http.post(&url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .timeout(Duration::from_secs(15));
    let what: Vec<String> = kills.iter().map(|k| format!("{} {}", k.npc, k.killer)).collect();
    match request.send().await {
        Ok(res) => tracing::info!("Boss timers: orb killers sent ({}): {}", res.status(), what.join(", ")),
        Err(e) => tracing::info!("Boss timers: could not send orb killers: {e}"),
    }
}

/// The HP of the Abyss cores among `bosses`, for /api/boss-timers/cores.
pub fn cores_hp_request(server_id: u16, bosses: &[BossHp], now_ms: i64) -> Option<serde_json::Value> {
    let cores: Vec<_> = bosses
        .iter()
        .filter(|b| crate::capture::abyss_cores::is_core(b.npc))
        .map(|b| json!({ "npc": b.npc, "artifact": crate::capture::abyss_cores::artifact_of(b.npc),
                         "hp": b.percent, "reset": b.reset }))
        .collect();
    (!cores.is_empty()).then(|| cores_body(server_id, cores, now_ms))
}

/// The HP request for the bosses to report now.
pub fn hp_request(server_id: u16, bosses: &[BossHp]) -> Option<serde_json::Value> {
    let bosses: Vec<_> = bosses
        .iter()
        .filter_map(|b| {
            let id = crate::capture::field_bosses::world_map_id(b.npc)?;
            Some(if b.reset { json!({ "id": id, "hp": b.percent, "reset": true }) } else { json!({ "id": id, "hp": b.percent }) })
        })
        .collect();
    (!bosses.is_empty()).then(|| json!({ "format": 1, "server": server_id, "bosses": bosses }))
}

/// The server reports go under: the one the game named (your self record),
/// else the one this game server's address was last seen to be. A meter
/// started mid-game knows the address at once but not the server until a
/// login or teleport, and sent nothing for a fight meanwhile (Harvest Manager
/// Moshav, 2026-10-18). Each address's server is kept in settings
/// (`bossTimers.serverOf.<address>`) whenever both are known.
/// Learnt only from the game's own word (`own_server`), never from a guess.
/// What it finds also goes to the fight records (`note_host_server`), so a
/// fight then is saved, and its log filed, under that server too.
pub(crate) fn report_server(state: &AppState) -> u16 {
    let server = guess_report_server(state);
    // Never a server that doesn't exist: nothing is sent until one is known.
    if crate::capture::servers::exists(server) { server } else { 0 }
}

fn guess_report_server(state: &AppState) -> u16 {
    let own = state.data_storage.own_server();
    // Only a real game server's address names a server. Through a ping
    // reducer (ExitLag and the like) the game talks to a tunnel on this PC
    // (127.0.0.1) or the LAN, the same address for every server: learnt
    // from that, a character on another server was reported as this one.
    let host = state.data_storage.game_host().filter(|h| {
        !(h.is_loopback() || h.is_private() || h.is_link_local() || h.is_unspecified())
    });
    if own != 0 {
        if let Some(host) = host {
            let key = format!("bossTimers.serverOf.{host}");
            let value = own.to_string();
            if state.settings.get(&key).as_deref() != Some(value.as_str()) {
                state.settings.set(&key, &value);
            }
        }
        return own;
    }
    let learnt = host
        .and_then(|host| state.settings.get(&format!("bossTimers.serverOf.{host}")))
        .and_then(|v| v.trim().parse::<u16>().ok())
        .unwrap_or(0);
    state.data_storage.note_host_server(learnt);
    if learnt != 0 {
        return learnt;
    }
    // As before: the party's server, when the party says.
    state.data_storage.local_profile().server_id
}

/// Kills and HP milestones, sent within a second of happening.
fn spawn_events(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut waiting: Vec<(i64, BossEvent)> = Vec::new();
        let mut last_hp = std::time::Instant::now();
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let Some(state) = app.try_state::<AppState>() else { continue };
            let now = crate::clock::now_ms();
            // HP every 3 seconds, the bosses whose percent changed; dropped
            // when it can't go (HP is only worth anything now).
            if last_hp.elapsed() >= HP_EVERY {
                last_hp = std::time::Instant::now();
                let changed = state.data_storage.take_boss_hp(now);
                let server = report_server(&state);
                let off = !sharing(&state);
                let requests = [("/api/boss-timers/hp", hp_request(server, &changed)),
                                ("/api/boss-timers/cores", cores_hp_request(server, &changed, now))];
                for (path, body) in requests {
                    if !sharing(&state) {
                        break;
                    }
                    let (false, false, Some(body)) = (off, server == 0, body) else { continue };
                    let body = with_host(body, server, state.data_storage.game_host());
                    let url = format!("{}{path}", crate::account::base_url());
                    let request = state.http.post(&url)
                        .header(reqwest::header::CONTENT_TYPE, "application/json")
                        .body(body.to_string())
                        .timeout(Duration::from_secs(10));
                    match request.send().await {
                        // Refused: said, as nothing else would show it (an HP
                        // bar that stayed full on Blooming Korin, 2026-10-22).
                        Ok(res) if !res.status().is_success() => {
                            let status = res.status();
                            let why = crate::bounded_http::text(res, 64 * 1024).await.unwrap_or_default();
                            tracing::info!("Boss timers: HP refused ({path}, {status}): {}", why.chars().take(200).collect::<String>());
                        }
                        Ok(_) => {}
                        Err(e) => tracing::debug!("Boss timers: could not send HP ({path}): {e}"),
                    }
                }
            }
            let events = state.data_storage.take_boss_events();
            if !sharing(&state) {
                waiting.clear();
                *orb_killers().lock().unwrap_or_else(|p| p.into_inner()) = Default::default();
                continue;
            }
            for event in &events {
                if let BossEventKind::Killed { at_ms } = event.kind {
                    if crate::capture::abyss_cores::is_core(event.npc) {
                        orb_killers().lock().unwrap_or_else(|p| p.into_inner()).killed(event.npc, at_ms);
                    }
                }
            }
            waiting.extend(events.into_iter().map(|e| (now, e)));
            // Orb kills no holder came for in time: "unknown".
            let expired = orb_killers().lock().unwrap_or_else(|p| p.into_inner()).expired(now);
            if !expired.is_empty() && sharing(&state) {
                let server = report_server(&state);
                if server != 0 {
                    send_orb_killers(&state, server, expired).await;
                }
            }
            if waiting.is_empty() {
                continue;
            }
            if !sharing(&state) {
                waiting.clear();
                continue;
            }
            let server = report_server(&state);
            if server == 0 {
                waiting.retain(|(at, _)| now - at < EVENT_WAIT_MS);
                continue;
            }
            for (_, event) in std::mem::take(&mut waiting) {
                if !sharing(&state) {
                    break;
                }
                let Some((path, body)) = event_request(server, &event) else { continue };
                let body = with_host(body, server, state.data_storage.game_host());
                let url = format!("{}{path}", crate::account::base_url());
                let request = state.http.post(&url)
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(body.to_string())
                    .timeout(Duration::from_secs(15));
                match request.send().await {
                    Ok(res) => tracing::info!("Boss timers: {:?} of {} sent ({})", event.kind, event.npc, res.status()),
                    Err(e) => tracing::info!("Boss timers: could not send {:?} of {}: {e}", event.kind, event.npc),
                }
            }
        }
    });
}

pub fn spawn(app: tauri::AppHandle) {
    spawn_events(app.clone());
    tauri::async_runtime::spawn(async move {
        let mut seq = 0u64;
        // (server, map) -> when sent, and what it said.
        let mut sent: HashMap<(u16, u32), (i64, String)> = HashMap::new();
        let mut artifacts_seq = 0u64;
        // Server -> when the artifact owners were sent, and what they said.
        let mut artifacts_sent: HashMap<u16, (i64, String)> = HashMap::new();
        loop {
            tokio::time::sleep(CHECK).await;
            let Some(state) = app.try_state::<AppState>() else { continue };
            send_artifacts(&state, &mut artifacts_seq, &mut artifacts_sent).await;
            let Some((next, maps)) = state.data_storage.boss_maps_since(seq) else { continue };
            if !sharing(&state) {
                seq = next;
                continue;
            }
            // The server isn't known yet (the meter started mid-game): keep
            // the maps for when it is.
            let server = report_server(&state);
            if server == 0 {
                continue;
            }
            seq = next;
            let now = crate::clock::now_ms();
            for map in maps.iter().filter(|m| m.header == 0 && now - m.seen_ms <= MAX_AGE_MS) {
                if !sharing(&state) {
                    break;
                }
                let said = serde_json::to_string(&report(server, map, 0)["bosses"]).unwrap_or_default();
                if let Some((at, before)) = sent.get(&(server, map.map_id)) {
                    if *before == said && now - at < RESEND_SAME_MS {
                        continue;
                    }
                }
                sent.insert((server, map.map_id), (now, said));
                let url = format!("{}/api/boss-timers/report", crate::account::base_url());
                let body = with_host(report(server, map, now), server, state.data_storage.game_host());
                let request = state.http.post(&url)
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(body.to_string())
                    .timeout(Duration::from_secs(15));
                match request.send().await {
                    Ok(res) => {
                        let status = res.status();
                        let text = crate::bounded_http::text(res, 64 * 1024).await.unwrap_or_default();
                        tracing::info!("Boss timers: map {} for server {} sent ({status}): {}", map.map_id, server,
                            text.chars().take(300).collect::<String>());
                    }
                    Err(e) => tracing::info!("Boss timers: could not send map {}: {e}", map.map_id),
                }
            }
        }
    });
}

/// The Abyss artifacts' holders, when the game has said something since
/// `seq` that wasn't sent for this server lately.
async fn send_artifacts(state: &AppState, seq: &mut u64, sent: &mut HashMap<u16, (i64, String)>) {
    let Some((next, artifacts, seen_ms)) = state.data_storage.abyss_artifacts_since(*seq) else { return };
    if !sharing(&state) {
        *seq = next;
        return;
    }
    // Kept for when the server is known, as a map is.
    let server = report_server(state);
    if server == 0 {
        return;
    }
    *seq = next;
    let now = crate::clock::now_ms();
    if artifacts.artifacts.is_empty() || now - seen_ms > MAX_AGE_MS {
        return;
    }
    // The holders settle the orb kills waiting for them.
    let settled = orb_killers().lock().unwrap_or_else(|p| p.into_inner()).holders_seen(&artifacts, seen_ms);
    if !settled.is_empty() {
        send_orb_killers(state, server, settled).await;
    }
    if !sharing(state) {
        return;
    }
    let said = serde_json::to_string(&artifacts_report(server, &artifacts, 0)).unwrap_or_default();
    if let Some((at, before)) = sent.get(&server) {
        if *before == said && now - at < RESEND_SAME_MS {
            return;
        }
    }
    sent.insert(server, (now, said));
    let url = format!("{}/api/boss-timers/artifacts", crate::account::base_url());
    let body = with_host(artifacts_report(server, &artifacts, seen_ms), server, state.data_storage.game_host());
    let request = state.http.post(&url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .timeout(Duration::from_secs(15));
    match request.send().await {
        Ok(res) => tracing::info!("Boss timers: Abyss artifact holders for server {server} sent ({})", res.status()),
        Err(e) => tracing::info!("Boss timers: could not send the Abyss artifact holders: {e}"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sharing_requires_an_explicit_choice_here_or_for_both_previous_switches() {
        use std::collections::HashMap;
        let with = |pairs: &[(&str, &str)]| {
            let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            super::shares_data(move |k| m.get(k).cloned())
        };
        assert!(!with(&[]), "off before a choice");
        assert!(!with(&[("dpsMeter.shareSupplyRequests", "true")]));
        assert!(with(&[("dpsMeter.shareSupplyRequests", "true"), ("dpsMeter.shareBossTimers", "true")]));
        assert!(!with(&[("dpsMeter.shareBossTimers", "false")]), "boss timers turned off before: off");
        assert!(!with(&[("dpsMeter.shareSupplyRequests", "false")]), "Supply Requests turned off before: off");
        assert!(with(&[("dpsMeter.shareBossTimers", "false"), ("dpsMeter.shareData", "true")]), "the new switch decides once set");
        assert!(!with(&[("dpsMeter.shareData", "false")]));
        assert!(!with(&[("dpsMeter.shareData", "yes")]));
        assert!(!with(&[("dpsMeter.shareData", "")]));
    }

    use super::*;
    use crate::capture::boss_map::BossOnMap;

    #[test]
    fn a_kill_and_the_hp_name_the_boss_by_its_world_map_id() {
        let kill = BossEvent { npc: 2100079, kind: BossEventKind::Killed { at_ms: 1_791_993_638_495 } };
        let (path, body) = event_request(1301, &kill).unwrap();
        assert_eq!(path, "/api/boss-timers/kill");
        assert_eq!(body, json!({ "format": 1, "server": 1301, "id": 101006, "diedAt": 1_791_993_638 }));
        assert!(event_request(1301, &BossEvent { npc: 1, kind: BossEventKind::Killed { at_ms: 0 } }).is_none());
        let b = |npc, percent, reset| BossHp { npc, percent, reset };
        let hp = hp_request(1301, &[b(2100079, 49, false), b(1, 50, false)]).unwrap();
        assert_eq!(hp, json!({ "format": 1, "server": 1301, "bosses": [{ "id": 101006, "hp": 49 }] }));
        let reset = hp_request(1301, &[b(2100079, 100, true)]).unwrap();
        assert_eq!(reset["bosses"][0], json!({ "id": 101006, "hp": 100, "reset": true }));
        assert!(hp_request(1301, &[b(1, 50, false)]).is_none());
    }

    /// Korea and Taiwan share 10xx/20xx: their reports name the game
    /// server's address (Taiwan's 1016 was 210.242.123.144). Global ids,
    /// or no address known yet, go as before.
    #[test]
    fn a_shared_korea_taiwan_id_names_the_game_server() {
        let host = Some(std::net::Ipv4Addr::new(210, 242, 123, 144));
        let body = || json!({ "format": 1, "server": 1016 });
        assert_eq!(with_host(body(), 1016, host)["host"], "210.242.123.144");
        assert_eq!(with_host(body(), 2018, host)["host"], "210.242.123.144");
        assert!(with_host(body(), 1016, None).get("host").is_none());
        assert!(with_host(body(), 1301, host).get("host").is_none());
        assert!(with_host(body(), 1101, host).get("host").is_none());
    }

    #[test]
    fn a_core_goes_by_npc_code_with_its_artifact_and_a_world_boss_as_before() {
        let seen = BossEvent { npc: 2600080, kind: BossEventKind::Seen { at_ms: 1_792_333_380_500 } };
        let (path, body) = event_request(2301, &seen).unwrap();
        assert_eq!(path, "/api/boss-timers/cores");
        assert_eq!(body, json!({ "format": 1, "server": 2301, "observedAt": 1_792_333_380,
            "cores": [{ "npc": 2600080, "artifact": 1002, "state": "up", "at": 1_792_333_380 }] }));
        let killed = BossEvent { npc: 2600079, kind: BossEventKind::Killed { at_ms: 1_000_000 } };
        assert_eq!(event_request(2301, &killed).unwrap().1["cores"][0]["artifact"], serde_json::Value::Null);
        // A world boss: its kill as before, no "seen" (the maps say that).
        assert!(event_request(2301, &BossEvent { npc: 2100079, kind: BossEventKind::Seen { at_ms: 0 } }).is_none());
        let hp = [BossHp { npc: 2600516, percent: 63, reset: false }, BossHp { npc: 2100079, percent: 40, reset: false }];
        assert_eq!(cores_hp_request(2301, &hp, 2_000_000).unwrap()["cores"],
            json!([{ "npc": 2600516, "artifact": 2001, "hp": 63, "reset": false }]));
        assert_eq!(hp_request(2301, &hp).unwrap()["bosses"], json!([{ "id": 101006, "hp": 40 }]));
    }

    #[test]
    fn the_artifact_report_names_each_siege_and_each_artifacts_holder() {
        use crate::capture::abyss_artifacts::{Artifact, ArtifactWar, Owner};
        let state = ArtifactState {
            wars: vec![
                ArtifactWar { event_id: 1001, since_ms: 1_792_264_375_000, until_ms: 1_792_436_400_000 },
                ArtifactWar { event_id: 2001, since_ms: 0, until_ms: 0 },
            ],
            artifacts: vec![
                Artifact { id: 1001, owner: Owner::Elyos },
                Artifact { id: 1002, owner: Owner::Asmodian },
                Artifact { id: 2001, owner: Owner::None },
            ],
        };
        let body = artifacts_report(2015, &state, 1_792_333_380_000);
        assert_eq!(
            body,
            json!({
                "format": 1, "server": 2015, "observedAt": 1_792_333_380,
                "wars": [
                    { "event": 1001, "since": 1_792_264_375, "until": 1_792_436_400 },
                    { "event": 2001, "since": null, "until": null },
                ],
                "artifacts": [
                    { "id": 1001, "event": 1001, "owner": "elyos" },
                    { "id": 1002, "event": 1001, "owner": "asmodian" },
                    { "id": 2001, "event": 2001, "owner": "none" },
                ],
            })
        );
    }

    #[test]
    fn a_report_names_the_server_map_and_each_bosss_state_and_time() {
        let map = BossMap {
            map_id: 1010,
            header: 0,
            seen_ms: 1_791_993_638_495,
            bosses: vec![
                BossOnMap { id: 101_006, up: true, time_ms: 1_791_993_400_000 },
                BossOnMap { id: 101_002, up: false, time_ms: 1_791_994_085_000 },
                BossOnMap { id: 2003, up: false, time_ms: 0 },
            ],
        };
        let r = report(1301, &map, 1_791_993_638_495);
        assert_eq!(r["format"], 1);
        assert_eq!((r["server"].as_u64(), r["map"].as_u64(), r["observedAt"].as_i64()), (Some(1301), Some(1010), Some(1_791_993_638)));
        assert_eq!(r["bosses"][0], json!({ "id": 101_006, "state": "up", "upSince": 1_791_993_400 }));
        assert_eq!(r["bosses"][1], json!({ "id": 101_002, "state": "dead", "nextSpawn": 1_791_994_085 }));
        assert_eq!(r["bosses"][2]["nextSpawn"], serde_json::Value::Null);
    }
}

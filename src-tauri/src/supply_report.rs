//! The Supply Requests boards, shared: what the game sends at login
//! (`capture::supply_requests`) goes to a2tools.app, whose Profiteer shows
//! each server's current requests and what they're worth.
//!
//! Opt-in, with the field boss timers: Settings' "Sync useful data to
//! A2Tools" (`boss_report::shares_data`, explicit "true" enables it). What
//! is sent: the server, and each board whose window is open (its id, tab,
//! window and request ids). Not the character, nor its progress on them.
//!
//! Rarely: the boards change only at the daily and weekly resets, so a meter
//! sends a server's boards only when they differ from what it last sent for
//! that server, and never more than once an hour. What was sent is kept with
//! the settings, so a restart or a relog doesn't send them again: about once
//! a day per meter.

use std::time::Duration;

use serde_json::json;
use tauri::Manager;

use crate::app::AppState;
use crate::capture::supply_requests::Board;

const CHECK: Duration = Duration::from_secs(30);
/// A server's boards go at most this often, whatever changes.
const MIN_GAP_MS: i64 = 60 * 60_000;

/// The boards still open at `now_ms`, as the site's
/// /api/profiteer/supply reads them (format 1).
pub fn report(server_id: u16, boards: &[Board], now_ms: i64) -> Option<serde_json::Value> {
    let open: Vec<_> = boards
        .iter()
        .filter(|b| b.start_ms <= now_ms && now_ms < b.end_ms)
        .map(|b| json!({ "id": b.id, "tab": b.tab, "start": b.start_ms / 1000, "end": b.end_ms / 1000, "requests": b.requests }))
        .collect();
    (!open.is_empty()).then(|| json!({ "format": 1, "server": server_id, "observedAt": now_ms / 1000, "boards": open }))
}

/// What a report says, for telling whether it changed: its boards, without
/// the time it was seen.
fn signature(report: &serde_json::Value) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    report["boards"].to_string().hash(&mut h);
    format!("{:016x}", h.finish())
}

fn sent_key(server: u16) -> String {
    format!("supplyRequests.sent.{server}")
}

/// Whether a report with this signature goes now, given what was last sent
/// for the server (`"<ms>|<signature>"`).
fn due(last: Option<&str>, signature: &str, now_ms: i64) -> bool {
    let Some((at, before)) = last.and_then(|l| l.split_once('|')) else { return true };
    let at: i64 = at.parse().unwrap_or(0);
    before != signature && now_ms - at >= MIN_GAP_MS
}

pub fn spawn(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut seq = 0u64;
        loop {
            tokio::time::sleep(CHECK).await;
            let Some(state) = app.try_state::<AppState>() else { continue };
            let Some((next, boards)) = state.data_storage.supply_requests_since(seq) else { continue };
            if !crate::boss_report::shares_data(|k| state.settings.get(k)) {
                seq = next;
                continue;
            }
            // The server isn't known yet (the meter started mid-game, or the
            // login is still going): keep the boards for when it is.
            let server = crate::boss_report::report_server(&state);
            if server == 0 {
                continue;
            }
            seq = next;
            let now = crate::clock::now_ms();
            let Some(body) = report(server, &boards, now) else { continue };
            let said = signature(&body);
            if !due(state.settings.get(&sent_key(server)).as_deref(), &said, now) {
                continue;
            }
            let url = format!("{}/api/profiteer/supply", crate::account::base_url());
            let request = state.http.post(&url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.to_string())
                .timeout(Duration::from_secs(15));
            match request.send().await {
                Ok(res) if res.status().is_success() => {
                    state.settings.set(&sent_key(server), &format!("{now}|{said}"));
                    tracing::info!("Supply Requests: {} boards for server {server} sent", body["boards"].as_array().map_or(0, |b| b.len()));
                }
                Ok(res) => tracing::info!("Supply Requests: server {server} refused ({})", res.status()),
                Err(e) => tracing::info!("Supply Requests: could not send for server {server}: {e}"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board(id: u32, tab: u8, start_s: i64, end_s: i64) -> Board {
        Board { id, tab, start_ms: start_s * 1000, end_ms: end_s * 1000 + 999, requests: vec![1011020, 1011118] }
    }

    #[test]
    fn only_open_boards_go_and_nothing_of_the_character() {
        let now = 1_792_500_000_000;
        let boards = vec![board(5401417, 1, 1_792_479_601, 1_792_566_000), board(5400001, 1, 1_792_300_000, 1_792_400_000)];
        let r = report(1304, &boards, now).unwrap();
        assert_eq!(r["server"], 1304);
        assert_eq!(r["boards"].as_array().unwrap().len(), 1, "the closed board stays home");
        assert_eq!(r["boards"][0], json!({ "id": 5401417, "tab": 1, "start": 1_792_479_601, "end": 1_792_566_000, "requests": [1011020, 1011118] }));
        assert!(report(1304, &boards[1..], now).is_none());
    }

    #[test]
    fn the_same_boards_never_go_twice_and_new_ones_wait_an_hour() {
        let now = 1_792_500_000_000;
        assert!(due(None, "a", now), "never sent");
        assert!(!due(Some(&format!("{}|a", now - 86_400_000)), "a", now), "the same boards, even a day on");
        assert!(due(Some(&format!("{}|a", now - 2 * 3_600_000)), "b", now), "new boards");
        assert!(!due(Some(&format!("{}|a", now - 600_000)), "b", now), "new, but sent ten minutes ago");
    }

    #[test]
    fn the_signature_ignores_when_the_boards_were_seen() {
        let boards = vec![board(5401417, 1, 1_792_479_601, 1_792_566_000)];
        let a = report(1304, &boards, 1_792_500_000_000).unwrap();
        let b = report(1304, &boards, 1_792_500_600_000).unwrap();
        assert_eq!(signature(&a), signature(&b));
    }
}

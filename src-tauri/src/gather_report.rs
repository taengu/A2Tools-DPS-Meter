//! Gathers, shared: how each went and the gathering skills the player had
//! for it (`capture::gathering`) go to a2tools.app, which checks the game's
//! listed chances (Plentiful Harvest, Appraiser, Lady Luck) against many
//! players' gathers, skill level by skill level.
//!
//! With Settings' "Sync useful data to A2Tools" (`boss_report::shares_data`).
//! What is sent: the server, and per gather its time, the item, the result
//! (quality, count, extra copies, the add-on item), what its start decided
//! (upgraded at the start or by the Appraiser, crits, Lady Luck) and the
//! learned gathering skills' levels. Not the character, nor where.
//!
//! In batches: once `BATCH` gathers wait, or the oldest has waited
//! `MAX_WAIT_S`. A batch the site doesn't take waits for the next try.

use std::time::Duration;

use serde_json::json;
use tauri::Manager;

use crate::app::AppState;
use crate::capture::gathering::Gathered;

const CHECK: Duration = Duration::from_secs(60);
const BATCH: usize = 50;
const MAX_WAIT_S: i64 = 15 * 60;
/// At most this many in one report.
const MAX_REPORT: usize = 200;

/// The report the site's /api/gathering/report reads (format 1).
pub fn report(server: u16, gathers: &[Gathered]) -> serde_json::Value {
    let gathers: Vec<_> = gathers
        .iter()
        .map(|g| {
            json!({
                "at": g.at, "item": g.item, "status": g.status, "count": g.count, "bonus": g.bonus,
                "extra": g.extra_item, "flags": g.start.flags, "crit": g.start.crit, "instant": g.start.instant,
                "luck": g.start.lady_luck, "skills": g.skills,
            })
        })
        .collect();
    json!({ "format": 1, "server": server, "gathers": gathers })
}

/// Whether the gathers waiting go now.
fn due(waiting: Option<(usize, i64)>, now_s: i64) -> bool {
    waiting.is_some_and(|(n, oldest)| n >= BATCH || now_s - oldest >= MAX_WAIT_S)
}

pub fn spawn(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(CHECK).await;
            let Some(state) = app.try_state::<AppState>() else { continue };
            if !crate::boss_report::shares_data(|k| state.settings.get(k)) {
                // Not shared: none are kept for later either.
                drop(state.data_storage.take_gathers());
                continue;
            }
            if !due(state.data_storage.gathers_waiting(), crate::clock::now_ms() / 1000) {
                continue;
            }
            let server = crate::boss_report::report_server(&state);
            if server == 0 {
                continue;
            }
            let mut gathers = state.data_storage.take_gathers();
            let later = gathers.split_off(gathers.len().min(MAX_REPORT));
            let body = report(server, &gathers);
            let url = format!("{}/api/gathering/report", crate::account::base_url());
            let request = state.http.post(&url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.to_string())
                .timeout(Duration::from_secs(15));
            let sent = match request.send().await {
                Ok(res) if res.status().is_success() => {
                    tracing::info!("Gathering: {} gathers sent for server {server}", gathers.len());
                    true
                }
                // Refused as wrong: trying again won't help.
                Ok(res) if res.status() == reqwest::StatusCode::BAD_REQUEST => {
                    tracing::info!("Gathering: server {server} refused {} gathers ({})", gathers.len(), res.status());
                    true
                }
                Ok(res) => {
                    tracing::info!("Gathering: not taken now ({})", res.status());
                    false
                }
                Err(e) => {
                    tracing::info!("Gathering: could not send: {e}");
                    false
                }
            };
            let mut back = if sent { Vec::new() } else { gathers };
            back.extend(later);
            if !back.is_empty() {
                state.data_storage.put_back_gathers(back);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::gathering::StartOutcome;

    #[test]
    fn a_report_holds_the_gather_and_the_skills_only() {
        let g = Gathered {
            at: 1_792_500_000, item: 610_670_045, status: 1, count: 1, bonus: 1, extra_item: 0,
            start: StartOutcome { node: 39_649, flags: 0x83, crit: true, instant: false, lady_luck: false },
            skills: vec![(800, 10), (51_200, 3)],
        };
        let r = report(2002, &[g]);
        assert_eq!(r["server"], 2002);
        assert_eq!(r["gathers"][0], json!({ "at": 1_792_500_000, "item": 610_670_045, "status": 1, "count": 1, "bonus": 1,
            "extra": 0, "flags": 0x83, "crit": true, "instant": false, "luck": false, "skills": [[800, 10], [51_200, 3]] }));
        assert!(r["gathers"][0].get("node").is_none(), "not where");
    }

    #[test]
    fn a_batch_goes_when_full_or_old() {
        let now = 1_792_500_000;
        assert!(!due(None, now));
        assert!(!due(Some((10, now - 60)), now));
        assert!(due(Some((50, now - 60)), now));
        assert!(due(Some((1, now - 15 * 60)), now));
    }
}

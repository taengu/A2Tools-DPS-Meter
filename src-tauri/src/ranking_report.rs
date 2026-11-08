//! The ranking tables, shared: a table the player opens in the game
//! (`capture::rankings`) goes to a2tools.app, which keeps the newest of each.
//!
//! Under Settings' "Sync useful data to A2Tools" (`boss_report::shares_data`).
//! What is sent: the player's server (which region's tables these are), the
//! table, the class tab, when the game updates it next, and its places as
//! the game shows them to everyone. Nothing says who opened it.
//!
//! Rarely: a table is the same until its next update (the countdown in the
//! window's corner), so a meter sends each table, for each class, once per
//! update, however often it is opened. What was sent is kept with the
//! settings, so a restart doesn't send it again. Gzipped: a top 500 is about
//! 15 KB.

use std::io::Write;
use std::time::Duration;

use serde_json::json;
use tauri::Manager;

use crate::app::AppState;
use crate::capture::rankings::Ranking;

const CHECK: Duration = Duration::from_secs(5);

/// The report the site's /api/rankings/report reads (format 1).
pub fn report(server_id: u16, table: &Ranking, now_ms: i64) -> serde_json::Value {
    let entries: Vec<_> = table
        .entries
        .iter()
        .map(|e| {
            let record = serde_json::from_str::<serde_json::Value>(&e.record).unwrap_or(serde_json::Value::Null);
            json!({
                "rank": e.rank,
                "id": e.character_id,
                "server": e.server,
                "name": e.name,
                "legion": e.legion,
                "level": e.level,
                "class": e.class,
                "value": e.value,
                "record": record,
                "change": e.rank_change,
                "grade": e.grade,
            })
        })
        .collect();
    json!({
        "format": 1,
        "server": server_id,
        "observedAt": now_ms / 1000,
        "table": table.table,
        "class": table.class,
        "nextUpdate": table.next_update_ms / 1000,
        "entries": entries,
    })
}

fn sent_key(server: u16, table: &Ranking) -> String {
    format!("rankings.sent.{server}.{}.{}", table.table, table.class)
}

/// Whether a table goes, given the next update of the one last sent.
fn due(last: Option<&str>, table: &Ranking) -> bool {
    last.and_then(|l| l.parse::<i64>().ok()) != Some(table.next_update_ms)
}

fn gzip(body: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(body)?;
    gz.finish()
}

pub fn spawn(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(CHECK).await;
            let Some(state) = app.try_state::<AppState>() else { continue };
            let tables = state.data_storage.take_rankings();
            if tables.is_empty() || !crate::boss_report::shares_data(|k| state.settings.get(k)) {
                continue;
            }
            // Not known yet (the meter started mid-game): the tables go
            // when they are opened again.
            let server = crate::boss_report::report_server(&state);
            if server == 0 || tables.iter().any(|t| t.header != 0) {
                continue;
            }
            for table in tables {
                let key = sent_key(server, &table);
                if !due(state.settings.get(&key).as_deref(), &table) {
                    continue;
                }
                let now = crate::clock::now_ms();
                let body = crate::boss_report::with_host(report(server, &table, now), server, state.data_storage.game_host());
                let Ok(body) = gzip(body.to_string().as_bytes()) else { continue };
                let url = format!("{}/api/rankings/report", crate::account::base_url());
                let request = state.http.post(&url)
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .header(reqwest::header::CONTENT_ENCODING, "gzip")
                    .body(body)
                    .timeout(Duration::from_secs(20));
                match request.send().await {
                    // Taken, or refused for good: either way, not again until
                    // the table updates.
                    Ok(res) if !res.status().is_server_error() => {
                        state.settings.set(&key, &table.next_update_ms.to_string());
                        let status = res.status();
                        // A refusal says why, in a short {"error": ...}.
                        let why = if status.is_client_error() {
                            res.text().await.unwrap_or_default().chars().take(200).collect()
                        } else {
                            String::new()
                        };
                        tracing::info!("Rankings: table {} class {} for server {server}: {status} {why}", table.table, table.class);
                    }
                    Ok(res) => tracing::info!("Rankings: server {server} could not take table {} ({})", table.table, res.status()),
                    Err(e) => tracing::info!("Rankings: could not send table {} for server {server}: {e}", table.table),
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::rankings::RankEntry;

    fn table(next: i64) -> Ranking {
        let entry = RankEntry {
            rank: 1,
            character_id: 187_089,
            server: 1305,
            name: "Dak".into(),
            legion: "MANDURI".into(),
            level: 45,
            class: 8,
            value: 17_458_004,
            record: r#"{"kill_count":8069,"death_count":456,"assist_count":7298}"#.into(),
            rank_change: -1,
            grade: 14,
        };
        Ranking { header: 0, table: 1, class: 8, entries: vec![entry], next_update_ms: next }
    }

    #[test]
    fn a_table_goes_with_its_places_and_record() {
        let r = report(1304, &table(1_794_158_072_585), 1_794_155_000_000);
        assert_eq!((r["server"].as_u64(), r["table"].as_u64(), r["class"].as_u64()), (Some(1304), Some(1), Some(8)));
        assert_eq!(r["nextUpdate"], 1_794_158_072);
        assert_eq!(r["entries"][0]["name"], "Dak");
        assert_eq!(r["entries"][0]["change"], -1);
        assert_eq!(r["entries"][0]["record"]["kill_count"], 8069);
    }

    #[test]
    fn a_table_goes_once_per_update() {
        let t = table(1_794_158_072_585);
        assert!(due(None, &t), "never sent");
        assert!(!due(Some("1794158072585"), &t), "the same update");
        assert!(due(Some("1794154472585"), &t), "the update before");
    }
}

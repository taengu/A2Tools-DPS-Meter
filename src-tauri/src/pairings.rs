//! Which servers fight which: each server's Abyss and Spacetime Rift
//! opponents, as NC announces them (Server Matchmaking notices, every two
//! weeks or so). The game names only the Abyss pairing on its connection
//! (`capture::matchup`, `0A 40`); the rift's is shown in the Season menu but
//! never sent where the meter can read it. So a2tools.app publishes the list
//! (`feeds/pairings.json` in this repo, uploaded by `feeds/upload.sh`) and the
//! meter reads it at start and every few hours: a new pairing reaches meters
//! without a release.
//!
//! Plugins get the current character's opponents as `pairings` (Event Timers:
//! the rift enemy's world bosses).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

use serde::Deserialize;

/// The list, from the CDN; then, if that cannot be read (Cloudflare can
/// challenge a network, and an app cannot pass a challenge: a player in Egypt
/// could not open it, 2026-11-15), the same file in this repo on GitHub.
#[cfg(feature = "online")]
const URLS: [&str; 2] = [
    "https://cdn.a2tools.app/feeds/pairings.json",
    "https://raw.githubusercontent.com/taengu/A2Tools-DPS-Meter/main/feeds/pairings.json",
];
/// How often the list is read again: a new one comes with a notice, so a
/// meter left running picks it up the same day.
#[cfg(feature = "online")]
const EVERY: std::time::Duration = std::time::Duration::from_secs(6 * 3600);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Opponents {
    pub abyss: Option<u16>,
    pub rift: Option<u16>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Feed {
    /// When this round of pairings began (the notice's date).
    #[serde(default)]
    pub from: String,
    pub servers: HashMap<String, Opponents>,
}

static FEED: RwLock<Option<Feed>> = RwLock::new(None);
static SEQ: AtomicU64 = AtomicU64::new(0);

pub fn parse(json: &str) -> Option<Feed> {
    serde_json::from_str::<Feed>(json).ok().filter(|f| !f.servers.is_empty())
}

fn set(feed: Feed) {
    tracing::info!("Server pairings: {} servers, from {}", feed.servers.len(), feed.from);
    *FEED.write().unwrap_or_else(|p| p.into_inner()) = Some(feed);
    SEQ.fetch_add(1, Ordering::Relaxed);
}

/// Bumped each time a list is read, so a change can be told.
pub fn seq() -> u64 {
    SEQ.load(Ordering::Relaxed)
}

/// `server`'s opponents, as plugins get them: `{ server, abyss, rift, from }`,
/// or None when the list is not read yet or does not have the server.
pub fn for_server(server: u16) -> Option<serde_json::Value> {
    let feed = FEED.read().unwrap_or_else(|p| p.into_inner());
    let feed = feed.as_ref()?;
    let o = feed.servers.get(&server.to_string())?;
    Some(serde_json::json!({ "server": server, "abyss": o.abyss, "rift": o.rift, "from": feed.from }))
}

/// After a failed read, the next tries: soon at first (a connection that
/// fails once often works a moment later: a player in Egypt got a reset, then
/// the list), then hourly.
#[cfg(feature = "online")]
const RETRY_SECS: [u64; 5] = [15, 30, 60, 120, 300];

/// Read the list now, then every `EVERY`. A failed read keeps the last list
/// and tries again after `RETRY_SECS`, then hourly.
#[cfg(feature = "online")]
pub fn spawn(client: reqwest::Client) {
    tauri::async_runtime::spawn(async move {
        let mut failures = 0usize;
        loop {
            let read = async {
                for url in URLS {
                    let text = async {
                        client.get(url).timeout(std::time::Duration::from_secs(30)).send().await.ok()?
                            .error_for_status().ok()?.text().await.ok()
                    };
                    // A challenge page is HTML, not the list: parse refuses it.
                    if let Some(feed) = text.await.as_deref().and_then(parse) {
                        return Some(feed);
                    }
                }
                None
            };
            let wait = match read.await {
                Some(feed) => {
                    failures = 0;
                    set(feed);
                    EVERY
                }
                None => {
                    let secs = RETRY_SECS.get(failures).copied().unwrap_or(3600);
                    failures += 1;
                    tracing::info!("Server pairings: not read, trying again in {secs} s");
                    std::time::Duration::from_secs(secs)
                }
            };
            tokio::time::sleep(wait).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_published_list_is_read() {
        let feed = parse(include_str!("../../feeds/pairings.json")).expect("feeds/pairings.json");
        // Kaisinel: Triniel for both. Vaizel: Siel in the Abyss, Zemurru for the rift.
        assert_eq!(feed.servers["1304"], Opponents { abyss: Some(2303), rift: Some(2303) });
        assert_eq!(feed.servers["1303"], Opponents { abyss: Some(1301), rift: Some(2315) });
        // Every pairing goes both ways.
        for (server, o) in &feed.servers {
            for (kind, other) in [("abyss", o.abyss), ("rift", o.rift)] {
                let Some(other) = other else { continue };
                let back = feed.servers[&other.to_string()];
                let back = if kind == "abyss" { back.abyss } else { back.rift };
                assert_eq!(back.map(|b| b.to_string()), Some(server.clone()), "{server} {kind}");
            }
        }
    }

    #[test]
    fn a_broken_or_empty_list_is_not_taken() {
        assert!(parse("{").is_none());
        assert!(parse(r#"{"servers": {}}"#).is_none());
    }
}

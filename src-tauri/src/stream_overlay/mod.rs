//! Stream overlay: the meter's rows served over the local network, for OBS
//! running on another PC (a two-PC streaming setup cannot capture the meter's
//! window, which lives on the game PC).
//!
//! Off unless turned on in Settings. While on, a small HTTP server listens on
//! every interface at the chosen port and serves a self-contained page plus a
//! Server-Sent Events feed of what the meter shows. Every request must carry
//! the random key from the settings; "New key" replaces it and cuts off every
//! link handed out before. Read-only: there is no route that changes anything,
//! and nothing but the rows, the target and the timer is served.
//!
//! Binding to all interfaces makes Windows Defender Firewall ask the first
//! time. The meter does not add a rule itself; the Settings text says to
//! allow it on private networks.

pub mod server;
pub mod snapshot;

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::Serialize;
use tauri::Manager as _;

use crate::app::AppState;
use crate::config::settings::Settings;

pub use server::{Hooks, PageLabels, RunningServer};

pub const ENABLED_KEY: &str = "dpsMeter.streamOverlay";
pub const PORT_KEY: &str = "dpsMeter.streamOverlayPort";
pub const TOKEN_KEY: &str = "dpsMeter.streamOverlayKey";
pub const DEFAULT_PORT: u16 = 18731;

/// Languages the meter ships UI text for; anything else falls back to English.
const LANGUAGES: [&str; 10] = ["de", "en", "es", "fr", "ja", "ko", "pt", "ru", "zh-Hans", "zh-Hant"];

/// What Settings shows.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub enabled: bool,
    pub running: bool,
    /// The port asked for (or listening on, when running).
    pub port: u16,
    /// One Browser Source URL per LAN address of this PC.
    pub urls: Vec<String>,
    /// Why it is not running although enabled, e.g. the port is taken.
    pub error: Option<String>,
}

struct Running {
    server: RunningServer,
    requested_port: u16,
}

/// Owns the server and starts or stops it to match the settings.
pub struct Manager {
    bind_ip: IpAddr,
    running: Mutex<Option<Running>>,
    error: Mutex<Option<String>>,
}

impl Default for Manager {
    fn default() -> Self {
        Self::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
    }
}

impl Manager {
    /// `bind_ip` is every interface in the app; tests use loopback.
    pub fn new(bind_ip: IpAddr) -> Self {
        Self { bind_ip, running: Mutex::new(None), error: Mutex::new(None) }
    }

    /// Start, stop, restart or re-key the server so it matches `settings`.
    pub fn sync(&self, settings: &Settings, hooks: impl FnOnce() -> Hooks) -> Status {
        let enabled = enabled(settings);
        let port = port(settings);
        let mut running = self.running.lock();
        if !enabled {
            if let Some(r) = running.take() {
                r.server.stop();
                tracing::info!("Stream overlay stopped");
            }
            *self.error.lock() = None;
        } else {
            let key = match ensure_token(settings) {
                Ok(key) => key,
                Err(error) => {
                    if let Some(r) = running.take() {
                        r.server.stop();
                    }
                    *self.error.lock() = Some(error);
                    drop(running);
                    return self.status(settings);
                }
            };
            match running.as_ref() {
                Some(r) if r.requested_port == port => r.server.set_key(key),
                _ => {
                    if let Some(r) = running.take() {
                        r.server.stop();
                    }
                    let addr = SocketAddr::new(self.bind_ip, port);
                    match RunningServer::start(addr, key, hooks()) {
                        Ok(server) => {
                            tracing::info!("Stream overlay listening on {}", server.local_addr());
                            *running = Some(Running { server, requested_port: port });
                            *self.error.lock() = None;
                        }
                        Err(e) => {
                            tracing::warn!("Stream overlay could not listen on {addr}: {e}");
                            *self.error.lock() = Some(describe_bind_error(&e, port));
                        }
                    }
                }
            }
        }
        drop(running);
        self.status(settings)
    }

    pub fn status(&self, settings: &Settings) -> Status {
        let enabled = enabled(settings);
        let running = self.running.lock();
        let port = running.as_ref().map(|r| r.server.local_addr().port()).unwrap_or_else(|| port(settings));
        let token = settings.get(TOKEN_KEY).unwrap_or_default();
        let urls = if enabled && !token.is_empty() {
            lan_ipv4s().into_iter().map(|ip| overlay_url(ip, port, &token)).collect()
        } else {
            Vec::new()
        };
        Status {
            enabled,
            running: running.is_some(),
            port,
            urls,
            error: if enabled { self.error.lock().clone() } else { None },
        }
    }

    /// The address the server is listening on, if it is.
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.running.lock().as_ref().map(|r| r.server.local_addr())
    }
}

fn enabled(settings: &Settings) -> bool {
    settings.get(ENABLED_KEY).as_deref() == Some("true")
}

fn port(settings: &Settings) -> u16 {
    settings.get(PORT_KEY).and_then(|p| p.trim().parse().ok()).unwrap_or(DEFAULT_PORT)
}

fn describe_bind_error(e: &std::io::Error, port: u16) -> String {
    match e.kind() {
        std::io::ErrorKind::AddrInUse => format!("port {port} is already in use"),
        std::io::ErrorKind::PermissionDenied => format!("not allowed to listen on port {port}"),
        _ => e.to_string(),
    }
}

pub fn overlay_url(ip: Ipv4Addr, port: u16, token: &str) -> String {
    format!("http://{ip}:{port}/overlay?key={token}")
}

/// The overlay key, made on first use.
fn ensure_token(settings: &Settings) -> Result<String, String> {
    match settings.get(TOKEN_KEY).filter(|t| t.len() >= 16) {
        Some(token) => Ok(token),
        None => {
            let token = new_token()?;
            settings.set(TOKEN_KEY, &token);
            Ok(token)
        }
    }
}

/// Replace the key, which cuts off every link handed out with the old one.
pub fn regenerate_token(settings: &Settings) -> Result<(), String> {
    settings.set(TOKEN_KEY, &new_token()?);
    Ok(())
}

/// 128 random bits as hex: URL-safe and easy to compare.
fn new_token() -> Result<String, String> {
    token_with_random(getrandom::fill)
}

fn token_with_random(fill: impl FnOnce(&mut [u8]) -> Result<(), getrandom::Error>) -> Result<String, String> {
    let mut bytes = [0u8; 16];
    fill(&mut bytes).map_err(|_| "Cannot obtain a secure random overlay key; the overlay was not started or re-keyed".to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// This PC's private IPv4 addresses, the one with the default route first.
///
/// Found by asking the routing table which source address it would use for a
/// few destinations: "connecting" a UDP socket sends nothing, it only picks
/// the route. No OS-specific interface listing needed.
pub fn lan_ipv4s() -> Vec<Ipv4Addr> {
    const PROBES: [&str; 5] = ["8.8.8.8:53", "192.168.0.1:9", "192.168.1.1:9", "10.0.0.1:9", "172.16.0.1:9"];
    let mut found: Vec<Ipv4Addr> = Vec::new();
    for probe in PROBES {
        let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else { continue };
        if socket.connect(probe).is_err() {
            continue;
        }
        if let Ok(SocketAddr::V4(local)) = socket.local_addr() {
            let ip = *local.ip();
            if is_lan(ip) && !found.contains(&ip) {
                found.push(ip);
            }
        }
    }
    found
}

/// Private ranges plus the shared CGNAT range (Tailscale and the like).
fn is_lan(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_private() || (a == 100 && (64..128).contains(&b))
}

// ── app glue ───────────────────────────────────────────────────────────────

/// Match the server to the settings; called at startup and on every change.
pub fn sync(app: &tauri::AppHandle) -> Status {
    let state = app.state::<AppState>();
    state.stream_overlay.sync(&state.settings, || hooks(app.clone()))
}

fn hooks(app: tauri::AppHandle) -> Hooks {
    let for_snapshot = app.clone();
    Hooks {
        snapshot: Arc::new(move || {
            let Some(state) = for_snapshot.try_state::<AppState>() else { return String::new() };
            // The meter window's own path (`get_dps_snapshot`): cached until
            // new damage arrives, so a watcher costs a clone most ticks.
            let dps = state.dps_calculator.lock().get_dps();
            let opts = snapshot::ViewOptions::from_settings(|k| state.settings.get(k));
            serde_json::to_string(&snapshot::build(&dps, opts)).unwrap_or_default()
        }),
        labels: Arc::new(move |requested| {
            let state = app.try_state::<AppState>();
            let meter_lang = state.as_ref().and_then(|s| s.settings.get("dpsMeter.language"));
            let lang = requested
                .or(meter_lang.as_deref())
                .and_then(|l| LANGUAGES.iter().find(|known| known.eq_ignore_ascii_case(l)))
                .copied()
                .unwrap_or("en");
            let data_dir = state.as_ref().and_then(|s| s.i18n_data_dir.clone());
            page_labels(data_dir.as_deref(), lang)
        }),
    }
}

/// Overlay class key, its `classes.*` key in the UI text, and English.
const CLASS_LABELS: [(&str, &str, &str); 9] = [
    ("gladiator", "GLADIATOR", "Gladiator"),
    ("templar", "TEMPLAR", "Templar"),
    ("ranger", "RANGER", "Ranger"),
    ("assassin", "ASSASSIN", "Assassin"),
    ("sorcerer", "SORCERER", "Sorcerer"),
    ("cleric", "CLERIC", "Cleric"),
    ("spiritmaster", "ELEMENTALIST", "Spiritmaster"),
    ("chanter", "CHANTER", "Chanter"),
    ("brawler", "FIGHTER", "Brawler"),
];

/// The page's labels, from the UI text for `lang` (one of `LANGUAGES`).
fn page_labels(data_dir: Option<&std::path::Path>, lang: &str) -> PageLabels {
    let load = |lang: &str| -> Option<serde_json::Value> {
        let path = data_dir?.join("i18n").join("ui").join(format!("{lang}.json"));
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    };
    let ui = load(lang);
    let en = load("en");
    let text_at = |group: &[&str], key: &str, fallback: &str| -> String {
        [ui.as_ref(), en.as_ref()]
            .into_iter()
            .flatten()
            .find_map(|v| group.iter().fold(v, |v, g| &v[*g])[key].as_str().map(str::to_string))
            .unwrap_or_else(|| fallback.to_string())
    };
    let text = |key: &str, fallback: &str| text_at(&["settings", "streamOverlay"], key, fallback);
    PageLabels {
        lang: lang.to_string(),
        waiting: text("waiting", "Waiting for combat…"),
        reconnecting: text("reconnecting", "Reconnecting to the meter…"),
        dps_suffix: text_at(&["meter"], "dpsSuffix", "/s"),
        classes: CLASS_LABELS
            .iter()
            .map(|(key, ui_key, en)| (key.to_string(), text_at(&["classes"], ui_key, en)))
            .collect(),
        player: text("player", "Player"),
    }
}

#[cfg(test)]
mod tests {
    use super::server::tests::{get, test_hooks};
    use super::*;

    fn settings() -> (Settings, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "a2t-stream-overlay-{}-{}",
            std::process::id(),
            new_token().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        (Settings::new(dir.clone()), dir)
    }

    #[test]
    fn the_server_follows_the_toggle() {
        let (settings, dir) = settings();
        let manager = Manager::new(IpAddr::V4(Ipv4Addr::LOCALHOST));
        settings.set(PORT_KEY, "0");

        let status = manager.sync(&settings, || test_hooks("{}"));
        assert!(!status.enabled && !status.running);
        assert!(manager.local_addr().is_none(), "off by default");

        settings.set(ENABLED_KEY, "true");
        let status = manager.sync(&settings, || test_hooks("{}"));
        assert!(status.running, "{status:?}");
        assert_eq!(status.error, None);
        let addr = manager.local_addr().unwrap();
        let token = settings.get(TOKEN_KEY).unwrap();
        assert_eq!(token.len(), 32);
        assert_eq!(get(addr, &format!("/overlay/snapshot?key={token}")).0, 200);

        // A new key: same server, old links refused.
        regenerate_token(&settings).unwrap();
        manager.sync(&settings, || panic!("re-keying must not restart the server"));
        let new_token = settings.get(TOKEN_KEY).unwrap();
        assert_ne!(token, new_token);
        assert_eq!(get(addr, &format!("/overlay/snapshot?key={token}")).0, 403);
        assert_eq!(get(addr, &format!("/overlay/snapshot?key={new_token}")).0, 200);

        settings.set(ENABLED_KEY, "false");
        let status = manager.sync(&settings, || test_hooks("{}"));
        assert!(!status.running && status.urls.is_empty());
        assert!(manager.local_addr().is_none());
        assert!(std::net::TcpStream::connect(addr).is_err(), "the port is closed when off");

        settings.flush().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_taken_port_is_reported_not_fatal() {
        let (settings, dir) = settings();
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = taken.local_addr().unwrap().port();
        let manager = Manager::new(IpAddr::V4(Ipv4Addr::LOCALHOST));
        settings.set(ENABLED_KEY, "true");
        settings.set(PORT_KEY, &port.to_string());
        let status = manager.sync(&settings, || test_hooks("{}"));
        assert!(status.enabled && !status.running);
        assert!(status.error.is_some());

        // Moving to a free port recovers.
        settings.set(PORT_KEY, "0");
        let status = manager.sync(&settings, || test_hooks("{}"));
        assert!(status.running && status.error.is_none());
        settings.set(ENABLED_KEY, "false");
        manager.sync(&settings, || test_hooks("{}"));
        settings.flush().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tokens_are_random_hex() {
        let (a, b) = (new_token().unwrap(), new_token().unwrap());
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn a_random_source_failure_does_not_fall_back_to_a_predictable_key() {
        assert!(token_with_random(|_| Err(getrandom::Error::UNSUPPORTED)).is_err());
        let token = token_with_random(|bytes| { bytes.fill(0xAB); Ok(()) }).unwrap();
        assert_eq!(token, "ab".repeat(16));
    }

    #[test]
    fn urls_and_addresses() {
        assert_eq!(
            overlay_url(Ipv4Addr::new(192, 168, 1, 20), 18731, "abc"),
            "http://192.168.1.20:18731/overlay?key=abc"
        );
        assert!(is_lan(Ipv4Addr::new(10, 1, 2, 3)));
        assert!(is_lan(Ipv4Addr::new(100, 100, 1, 1)));
        assert!(!is_lan(Ipv4Addr::new(127, 0, 0, 1)));
        assert!(!is_lan(Ipv4Addr::new(8, 8, 8, 8)));
        for ip in lan_ipv4s() {
            assert!(is_lan(ip));
        }
    }

    #[test]
    fn page_labels_come_from_the_ui_text() {
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/data");
        let de = page_labels(Some(&data), "de");
        let en = page_labels(Some(&data), "en");
        assert_eq!(de.lang, "de");
        assert!(!de.waiting.is_empty() && !en.waiting.is_empty());
        assert_ne!(de.waiting, en.waiting);
        assert_eq!(en.dps_suffix, "/s", "after DPS, as the meter window shows it");
        assert_eq!(en.classes["spiritmaster"], "Spiritmaster");
        assert_eq!(en.classes.len(), 9);
        assert_ne!(de.classes["gladiator"], "", "class names in the page's language");
        let none = page_labels(None, "en");
        assert_eq!(none.waiting, "Waiting for combat…");
    }
}

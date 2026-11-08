//! Live field boss updates for plugins: a plugin follows some of a server's
//! field bosses (`a2.followBosses`, permission `feed:boss-timers`), and
//! a2tools.app pushes each change to them as it arrives (its HP as it falls,
//! a kill, a spawn), over one WebSocket per server while any plugin follows a
//! boss there. Each update reaches the plugins as a "bossLive" event:
//!
//!   { type: "state", server, bosses: { <id>: <entry> } }   on following
//!   { type: "boss", server, boss: <id>, entry: <entry> }    on each change
//!   { type: "status", server, connected: bool }             the connection
//!
//! An entry is as in the feed file (feeds/boss-timers/<server>.json). The feed
//! stays the fallback: a plugin keeps reading it, more slowly while live.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message;

use super::{host, require, window_plugin};

/// Most bosses a plugin may follow on a server.
const MAX_FOLLOW: usize = 50;
/// A keep-alive this often; the server answers it without waking.
const PING_EVERY: Duration = Duration::from_secs(30);
/// A connection that has said nothing for this long (two keep-alives without
/// a "pong") is dead, though nothing closed it: dropped and made again.
/// Sending alone doesn't notice (a send into a dead route still succeeds), so
/// it would look connected while no update came.
const SILENT_FOR: Duration = Duration::from_secs(75);

#[derive(Default)]
struct State {
    /// What each plugin follows, by plugin and server.
    follows: HashMap<(String, u32), HashSet<String>>,
    /// Each server's connection: the set it should follow.
    conns: HashMap<u32, watch::Sender<Vec<String>>>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Mutex::default)
}

/// Follow these bosses of `server` (replacing what the calling plugin followed
/// there); none stops following.
#[tauri::command]
pub fn plugin_boss_follow(app: tauri::AppHandle, window: tauri::WebviewWindow, plugin: Option<String>, server: u32, bosses: Vec<String>) -> Result<(), String> {
    let id = window_plugin(&window, plugin.as_deref())?;
    require(&app, &id, "feed:boss-timers")?;
    if !(1000..10000).contains(&server) {
        return Err("not a server id".into());
    }
    let bosses: HashSet<String> = bosses
        .into_iter()
        .filter(|b| !b.is_empty() && b.len() <= 16 && b.chars().all(|c| c.is_ascii_digit()))
        .take(MAX_FOLLOW)
        .collect();
    let mut st = state().lock().unwrap_or_else(|p| p.into_inner());
    if bosses.is_empty() {
        st.follows.remove(&(id, server));
    } else {
        st.follows.insert((id, server), bosses);
    }
    apply(&app, &mut st, server);
    Ok(())
}

/// A plugin's window has gone: it follows nothing now.
pub fn forget_plugin(app: &tauri::AppHandle, id: &str) {
    let mut st = state().lock().unwrap_or_else(|p| p.into_inner());
    let servers: Vec<u32> = st.follows.keys().filter(|(p, _)| p == id).map(|(_, s)| *s).collect();
    for server in servers {
        st.follows.remove(&(id.to_string(), server));
        apply(app, &mut st, server);
    }
}

/// Bring `server`'s connection in line with what the plugins follow there:
/// open it, change what it follows, or close it.
fn apply(app: &tauri::AppHandle, st: &mut State, server: u32) {
    let mut wanted: Vec<String> = st
        .follows
        .iter()
        .filter(|((_, s), _)| *s == server)
        .flat_map(|(_, b)| b.iter().cloned())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    wanted.sort();
    match st.conns.get(&server) {
        Some(tx) if !wanted.is_empty() => {
            if *tx.borrow() != wanted {
                let _ = tx.send(wanted);
            }
        }
        Some(_) => {
            // An empty set ends the connection's task.
            if let Some(tx) = st.conns.remove(&server) {
                let _ = tx.send(Vec::new());
            }
        }
        None if !wanted.is_empty() => {
            let (tx, rx) = watch::channel(wanted);
            st.conns.insert(server, tx);
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                run(app, server, rx).await;
                // Ended (nothing followed, or refused): a later follow opens
                // a new one. Only this connection's entry, not a newer one's.
                let mut st = state().lock().unwrap_or_else(|p| p.into_inner());
                if st.conns.get(&server).is_some_and(|tx| tx.is_closed() || tx.borrow().is_empty()) {
                    st.conns.remove(&server);
                }
            });
        }
        None => {}
    }
}

/// The HP of world bosses the player is fighting, as their own meter reads
/// it, to plugins at once ("bossHp": { server, bosses: [{ id, hp }] }, ids
/// the feed's): the site's word comes back later, or not at all when
/// reports can't reach it, and a boss's icon stayed at 24% through the
/// fight (Allula, 2026-11-08).
pub fn tell_own_hp(app: &tauri::AppHandle, data: Value) {
    if let Ok(host) = host(app) {
        host.tell_permitted(app, "feed:boss-timers", "bossHp", data);
    }
}

fn tell(app: &tauri::AppHandle, data: Value) {
    if let Ok(host) = host(app) {
        host.tell_permitted(app, "feed:boss-timers", "bossLive", data);
    }
}

fn live_url(server: u32) -> String {
    let base = crate::account::base_url();
    let ws = base.strip_prefix("https://").map(|rest| format!("wss://{rest}")).unwrap_or(base);
    format!("{ws}/api/boss-timers/live?server={server}")
}

/// One server's connection, for as long as anything is followed there:
/// reconnects after 2 s, doubling to a minute, and follows the set again.
async fn run(app: tauri::AppHandle, server: u32, mut rx: watch::Receiver<Vec<String>>) {
    let mut backoff = Duration::from_secs(2);
    loop {
        let set = rx.borrow().clone();
        if set.is_empty() {
            return;
        }
        // A boss update is a few hundred bytes; a message past this is not one (#131).
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
            max_message_size: Some(1 << 20),
            max_frame_size: Some(1 << 20),
            ..Default::default()
        };
        let connect = tokio_tungstenite::connect_async_with_config(live_url(server), Some(config), false);
        let connected = tokio::time::timeout(Duration::from_secs(15), connect).await;
        let mut ws = match connected {
            Ok(Ok((ws, _))) => ws,
            Ok(Err(e)) => {
                // Not there (a site without live updates yet): try again in 5
                // minutes, so it connects once it is; the feed meanwhile.
                let missing = matches!(&e, tokio_tungstenite::tungstenite::Error::Http(r) if r.status().as_u16() == 404);
                tracing::debug!("Boss live {server}: no connection ({e})");
                tell(&app, serde_json::json!({ "type": "status", "server": server, "connected": false }));
                let wait = if missing { Duration::from_secs(300) } else { backoff };
                if wait_or_changed(&mut rx, wait).await {
                    return;
                }
                backoff = (backoff * 2).min(Duration::from_secs(60));
                continue;
            }
            Err(_) => {
                if wait_or_changed(&mut rx, backoff).await {
                    return;
                }
                backoff = (backoff * 2).min(Duration::from_secs(60));
                continue;
            }
        };
        backoff = Duration::from_secs(2);
        tracing::info!("Boss live {server}: connected, following {}", set.len());
        let follow = |set: &[String]| Message::Text(serde_json::json!({ "op": "follow", "bosses": set }).to_string().into());
        if ws.send(follow(&set)).await.is_err() {
            continue;
        }
        tell(&app, serde_json::json!({ "type": "status", "server": server, "connected": true }));
        let mut ping = tokio::time::interval(PING_EVERY);
        ping.tick().await;
        let mut heard = tokio::time::Instant::now();
        let closed = loop {
            tokio::select! {
                msg = ws.next() => match msg {
                    Some(Ok(Message::Text(text))) => {
                        heard = tokio::time::Instant::now();
                        if text.as_str() == "pong" {
                            continue;
                        }
                        if let Ok(mut data) = serde_json::from_str::<Value>(text.as_str()) {
                            if data.is_object() {
                                data["server"] = Value::from(server);
                                tell(&app, data);
                            }
                        }
                    }
                    Some(Ok(Message::Close(frame))) => {
                        // 4004: the site does not carry this server.
                        if frame.as_ref().is_some_and(|f| u16::from(f.code) == 4004) {
                            tracing::info!("Boss live {server}: not carried by the site");
                            return;
                        }
                        break false;
                    }
                    Some(Ok(_)) => heard = tokio::time::Instant::now(),
                    Some(Err(_)) | None => break false,
                },
                changed = rx.changed() => {
                    if changed.is_err() {
                        break true;
                    }
                    let set = rx.borrow().clone();
                    if set.is_empty() {
                        break true;
                    }
                    if ws.send(follow(&set)).await.is_err() {
                        break false;
                    }
                }
                _ = ping.tick() => {
                    if heard.elapsed() >= SILENT_FOR {
                        tracing::info!("Boss live {server}: nothing heard for {} s, reconnecting", heard.elapsed().as_secs());
                        break false;
                    }
                    if ws.send(Message::Text("ping".into())).await.is_err() {
                        break false;
                    }
                }
            }
        };
        let _ = ws.close(None).await;
        tell(&app, serde_json::json!({ "type": "status", "server": server, "connected": false }));
        if closed {
            tracing::info!("Boss live {server}: nothing followed, closed");
            return;
        }
        tracing::info!("Boss live {server}: connection lost, reconnecting");
        if wait_or_changed(&mut rx, backoff).await {
            return;
        }
    }
}

/// Wait `delay` before trying again; true if meanwhile nothing is followed.
async fn wait_or_changed(rx: &mut watch::Receiver<Vec<String>>, delay: Duration) -> bool {
    let sleep = tokio::time::sleep(delay);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            _ = &mut sleep => return rx.borrow().is_empty(),
            changed = rx.changed() => {
                if changed.is_err() || rx.borrow().is_empty() {
                    return true;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_live_address_is_the_api_over_websocket() {
        assert!(live_url(2101).starts_with("wss://"));
        assert!(live_url(2101).ends_with("/api/boss-timers/live?server=2101"));
    }
}

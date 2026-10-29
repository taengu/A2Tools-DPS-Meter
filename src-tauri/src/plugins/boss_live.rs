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
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{watch, Semaphore};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use super::{emit_plugin_if_current, host, plugin_generation, require, window_plugin};

/// Most bosses a plugin may follow on a server.
const MAX_FOLLOW: usize = 50;
const MAX_SERVERS_PER_PLUGIN: usize = 8;
const MAX_SERVERS: usize = 32;
const MAX_SUBSCRIPTIONS: usize = 256;
const MAX_BOSSES_PER_CONNECTION: usize = 200;
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
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

impl State {
    /// Limit the aggregate, not just the length of one request. Replacing or
    /// removing an existing subscription must still work at the limit.
    fn follow(&mut self, id: String, server: u32, bosses: HashSet<String>) -> Result<(), String> {
        let key = (id, server);
        if bosses.is_empty() {
            self.follows.remove(&key);
            return Ok(());
        }
        if !self.follows.contains_key(&key) {
            if self.follows.len() >= MAX_SUBSCRIPTIONS {
                return Err("too many boss subscriptions".into());
            }
            if self.follows.keys().filter(|(plugin, _)| plugin == &key.0).count() >= MAX_SERVERS_PER_PLUGIN {
                return Err("a plugin may follow at most 8 servers".into());
            }
            let servers: HashSet<u32> = self.follows.keys().map(|(_, server)| *server).collect();
            if !servers.contains(&server) && servers.len() >= MAX_SERVERS {
                return Err("too many live boss servers".into());
            }
        }
        let wanted: HashSet<&str> = self.follows.iter()
            .filter(|(other, _)| other.1 == server && **other != key)
            .flat_map(|(_, ids)| ids.iter().map(String::as_str))
            .chain(bosses.iter().map(String::as_str))
            .collect();
        if wanted.len() > MAX_BOSSES_PER_CONNECTION {
            return Err("too many bosses followed on this server".into());
        }
        self.follows.insert(key, bosses);
        Ok(())
    }
}

fn connection_slots() -> Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| Arc::new(Semaphore::new(MAX_SERVERS))).clone()
}

fn websocket_config() -> WebSocketConfig {
    WebSocketConfig {
        write_buffer_size: 16 * 1024,
        max_write_buffer_size: 256 * 1024,
        max_message_size: Some(256 * 1024),
        max_frame_size: Some(64 * 1024),
        ..Default::default()
    }
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
    let key = (id.clone(), server);
    let previous = st.follows.get(&key).cloned();
    st.follow(id, server, bosses)?;
    if let Err(why) = apply(&app, &mut st, server) {
        match previous {
            Some(bosses) => { st.follows.insert(key, bosses); }
            None => { st.follows.remove(&key); }
        }
        return Err(why);
    }
    Ok(())
}

/// A plugin's window has gone: it follows nothing now.
pub fn forget_plugin(app: &tauri::AppHandle, id: &str) {
    let mut st = state().lock().unwrap_or_else(|p| p.into_inner());
    let servers: Vec<u32> = st.follows.keys().filter(|(p, _)| p == id).map(|(_, s)| *s).collect();
    for server in servers {
        st.follows.remove(&(id.to_string(), server));
        let _ = apply(app, &mut st, server);
    }
}

/// Bring `server`'s connection in line with what the plugins follow there:
/// open it, change what it follows, or close it.
fn apply(app: &tauri::AppHandle, st: &mut State, server: u32) -> Result<(), String> {
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
            // Acquire before spawning: rapid follow/unfollow calls cannot
            // accumulate tasks still connecting or closing old sockets.
            let permit = connection_slots().try_acquire_owned().map_err(|_| "live boss connections are busy; try again shortly")?;
            let (tx, rx) = watch::channel(wanted);
            st.conns.insert(server, tx);
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let _permit = permit;
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
    Ok(())
}

/// Snapshot recipients before waiting for remote work. Looking up their
/// generation only after an await would label an old reply as new activity
/// when a plugin was disabled and enabled again in the meantime.
fn recipients(app: &tauri::AppHandle) -> Vec<(String, u64)> {
    let Ok(host) = host(app) else { return Vec::new() };
    let ids: Vec<String> = host.loaded.read().unwrap_or_else(|p| p.into_inner()).iter()
        .filter(|plugin| plugin.manifest.permissions.iter().any(|p| p == "feed:boss-timers"))
        .map(|plugin| plugin.manifest.id.clone()).collect();
    ids.into_iter().filter_map(|id| plugin_generation(app, &id).map(|generation| (id, generation))).collect()
}

fn tell(app: &tauri::AppHandle, data: Value, recipients: &[(String, u64)]) {
    for (id, generation) in recipients {
        let event = serde_json::json!({ "plugin": id, "event": "bossLive", "data": data });
        emit_plugin_if_current(app, id, event, *generation);
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
        let origin = recipients(&app);
        let connected = tokio::time::timeout(Duration::from_secs(15),
            tokio_tungstenite::connect_async_with_config(live_url(server), Some(websocket_config()), false)).await;
        let mut ws = match connected {
            Ok(Ok((ws, _))) => ws,
            Ok(Err(e)) => {
                // Not there (a site without live updates yet): try again in 5
                // minutes, so it connects once it is; the feed meanwhile.
                let missing = matches!(&e, tokio_tungstenite::tungstenite::Error::Http(r) if r.status().as_u16() == 404);
                tracing::debug!("Boss live {server}: no connection ({e})");
                tell(&app, serde_json::json!({ "type": "status", "server": server, "connected": false }), &origin);
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
        if !matches!(tokio::time::timeout(WRITE_TIMEOUT, ws.send(follow(&set))).await, Ok(Ok(()))) {
            continue;
        }
        tell(&app, serde_json::json!({ "type": "status", "server": server, "connected": true }), &origin);
        let mut ping = tokio::time::interval(PING_EVERY);
        ping.tick().await;
        let mut heard = tokio::time::Instant::now();
        let mut origin = recipients(&app);
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
                                tell(&app, data, &origin);
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
                    if !matches!(tokio::time::timeout(WRITE_TIMEOUT, ws.send(follow(&set))).await, Ok(Ok(()))) {
                        break false;
                    }
                }
                _ = ping.tick() => {
                    if heard.elapsed() >= SILENT_FOR {
                        tracing::info!("Boss live {server}: nothing heard for {} s, reconnecting", heard.elapsed().as_secs());
                        break false;
                    }
                    if !matches!(tokio::time::timeout(WRITE_TIMEOUT, ws.send(Message::Text("ping".into()))).await, Ok(Ok(()))) {
                        break false;
                    }
                }
            }
            origin = recipients(&app);
        };
        let _ = tokio::time::timeout(WRITE_TIMEOUT, ws.close(None)).await;
        tell(&app, serde_json::json!({ "type": "status", "server": server, "connected": false }), &origin);
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

    fn bosses(start: usize, count: usize) -> HashSet<String> {
        (start..start + count).map(|id| id.to_string()).collect()
    }

    #[test]
    fn many_small_requests_cannot_bypass_the_per_plugin_server_limit() {
        let mut st = State::default();
        for server in 1000..1000 + MAX_SERVERS_PER_PLUGIN as u32 {
            st.follow("timers".into(), server, bosses(1, 1)).unwrap();
        }
        assert!(st.follow("timers".into(), 2000, bosses(1, 1)).is_err());
        assert_eq!(st.follows.len(), MAX_SERVERS_PER_PLUGIN);
        st.follow("timers".into(), 1000, bosses(2, 1)).unwrap();
        st.follow("timers".into(), 1000, HashSet::new()).unwrap();
        st.follow("timers".into(), 2000, bosses(1, 1)).unwrap();
        assert_eq!(st.follows.len(), MAX_SERVERS_PER_PLUGIN);
    }

    #[test]
    fn plugins_share_a_global_server_and_subscription_budget() {
        let mut st = State::default();
        for server in 1000..1000 + MAX_SERVERS as u32 {
            st.follow(format!("plugin{}", (server - 1000) / MAX_SERVERS_PER_PLUGIN as u32), server, bosses(1, 1)).unwrap();
        }
        assert!(st.follow("another".into(), 2000, bosses(1, 1)).is_err());
        assert_eq!(st.follows.len(), MAX_SERVERS);
        // Another reader of a server already connected needs no new socket.
        st.follow("another".into(), 1000, bosses(1, 1)).unwrap();

        let mut st = State::default();
        for plugin in 0..MAX_SUBSCRIPTIONS / MAX_SERVERS_PER_PLUGIN {
            for server in 1000..1000 + MAX_SERVERS_PER_PLUGIN as u32 {
                st.follow(format!("plugin{plugin}"), server, bosses(1, 1)).unwrap();
            }
        }
        assert!(st.follow("extra".into(), 1000, bosses(1, 1)).is_err());
        assert_eq!(st.follows.len(), MAX_SUBSCRIPTIONS);
    }

    #[test]
    fn many_plugins_cannot_grow_one_connection_without_limit() {
        let mut st = State::default();
        for plugin in 0..MAX_BOSSES_PER_CONNECTION / MAX_FOLLOW {
            st.follow(format!("plugin{plugin}"), 1000, bosses(plugin * MAX_FOLLOW, MAX_FOLLOW)).unwrap();
        }
        assert!(st.follow("extra".into(), 1000, bosses(MAX_BOSSES_PER_CONNECTION, 1)).is_err());
        // Replacing the same subscription does not double count its entries.
        st.follow("plugin0".into(), 1000, bosses(0, MAX_FOLLOW)).unwrap();
        st.follow("plugin0".into(), 1000, HashSet::new()).unwrap();
        st.follow("extra".into(), 1000, bosses(MAX_BOSSES_PER_CONNECTION, MAX_FOLLOW)).unwrap();
    }

    #[tokio::test]
    async fn an_oversized_websocket_frame_is_refused() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
            let _ = ws.send(Message::Binary(vec![0; 64 * 1024 + 1])).await;
        });
        let (mut client, _) = tokio_tungstenite::connect_async_with_config(
            format!("ws://{address}/"), Some(websocket_config()), false).await.unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(5), client.next()).await.unwrap();
        assert!(matches!(reply, Some(Err(tokio_tungstenite::tungstenite::Error::Capacity(_)))));
        server.await.unwrap();
    }
}

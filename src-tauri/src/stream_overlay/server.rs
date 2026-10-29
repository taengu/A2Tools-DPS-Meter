//! The stream overlay's HTTP server: a few GET routes, behind a key.
//!
//! Hand-rolled on tokio rather than a web framework: it answers four fixed
//! paths with GET only, never reads a request body, and closes every
//! connection after its one response. That is a page of code; a framework
//! would be most of a megabyte of dependencies for it.
//!
//! Every request has to carry `key=<token>` in its query, compared in
//! constant time, or it gets a 403 — before the path is even looked at, so a
//! stranger on the network cannot learn which paths exist.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{watch, Semaphore};

use super::snapshot::class_icon;

/// How often the feed asks the meter for a fresh snapshot while anyone watches.
const FEED_INTERVAL: Duration = Duration::from_millis(333);
/// SSE comment sent this often so OBS and any proxy keep the stream open.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(15);
/// A client has this long to send its request line and headers.
const HEAD_TIMEOUT: Duration = Duration::from_secs(5);
/// A client that stops receiving cannot keep a response task indefinitely.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_HEAD: usize = 8 * 1024;
/// Open connections at once. A scene or two in OBS uses one or two.
const MAX_CONNECTIONS: usize = 32;
/// Leave room for another streaming PC while allowing a browser's parallel
/// icon requests alongside several event feeds.
const MAX_PEER_CONNECTIONS: usize = MAX_CONNECTIONS / 2;

const PAGE: &str = include_str!("page.html");

/// The meter's current view as JSON (`snapshot::build`, serialised).
pub type SnapshotFn = Arc<dyn Fn() -> String + Send + Sync>;
/// The page's labels for a requested language (`None` = the meter's own).
pub type LabelsFn = Arc<dyn Fn(Option<&str>) -> PageLabels + Send + Sync>;

/// What the server needs from the app.
#[derive(Clone)]
pub struct Hooks {
    pub snapshot: SnapshotFn,
    pub labels: LabelsFn,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageLabels {
    pub lang: String,
    pub waiting: String,
    pub reconnecting: String,
    /// After a DPS figure, as the meter window shows it ("/s").
    #[serde(rename = "dpsSuffix")]
    pub dps_suffix: String,
    /// Class key to class name, for a row that has no name.
    pub classes: std::collections::BTreeMap<String, String>,
    /// For another player whose class is not known yet.
    pub player: String,
}

/// Constant-time comparison of a presented key with the real one. Length is
/// not secret (every key is the same length); the contents are.
pub fn key_matches(given: &str, expected: &str) -> bool {
    let (a, b) = (given.as_bytes(), expected.as_bytes());
    if expected.is_empty() || a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

struct Shared {
    key: RwLock<String>,
    key_revision: watch::Sender<u64>,
    hooks: Hooks,
    feed: watch::Sender<Arc<str>>,
    slots: Arc<Semaphore>,
    peers: Arc<Mutex<HashMap<IpAddr, usize>>>,
}

struct PeerSlot {
    peers: Arc<Mutex<HashMap<IpAddr, usize>>>,
    peer: IpAddr,
}

impl PeerSlot {
    fn acquire(peers: &Arc<Mutex<HashMap<IpAddr, usize>>>, peer: IpAddr) -> Option<Self> {
        let mut counts = peers.lock();
        let count = counts.entry(peer).or_default();
        if *count >= MAX_PEER_CONNECTIONS {
            return None;
        }
        *count += 1;
        Some(Self { peers: peers.clone(), peer })
    }
}

impl Drop for PeerSlot {
    fn drop(&mut self) {
        let mut counts = self.peers.lock();
        if let Some(count) = counts.get_mut(&self.peer) {
            *count -= 1;
            if *count == 0 {
                counts.remove(&self.peer);
            }
        }
    }
}

/// A running server. Dropping it shuts it down; `stop` also waits until the
/// port is free again, so the same port can be bound straight after.
pub struct RunningServer {
    addr: SocketAddr,
    shared: Arc<Shared>,
    shutdown: watch::Sender<bool>,
    closed: Option<std::sync::mpsc::Receiver<()>>,
}

impl RunningServer {
    /// Bind `addr` and serve. Binding happens here, synchronously, so a port
    /// that is taken is reported to the caller rather than lost in a task.
    pub fn start(addr: SocketAddr, key: String, hooks: Hooks) -> std::io::Result<Self> {
        let listener = std::net::TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;

        let (feed, _) = watch::channel::<Arc<str>>(Arc::from(""));
        let (key_revision, _) = watch::channel(0);
        let shared = Arc::new(Shared {
            key: RwLock::new(key),
            key_revision,
            hooks,
            feed,
            slots: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
            peers: Arc::new(Mutex::new(HashMap::new())),
        });
        let (shutdown, _) = watch::channel(false);
        let (closed_tx, closed_rx) = std::sync::mpsc::channel::<()>();

        let accept_shared = shared.clone();
        let mut accept_stop = shutdown.subscribe();
        let conn_stop = shutdown.subscribe();
        tauri::async_runtime::spawn(async move {
            let listener = match tokio::net::TcpListener::from_std(listener) {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!("Stream overlay: listener failed: {e}");
                    let _ = closed_tx.send(());
                    return;
                }
            };
            loop {
                tokio::select! {
                    _ = accept_stop.changed() => break,
                    accepted = listener.accept() => {
                        let Ok((stream, peer)) = accepted else { continue };
                        // Reserve before spawning. Excess sockets are simply closed:
                        // writing a 503 would itself need an unbounded extra task.
                        let Ok(slot) = accept_shared.slots.clone().try_acquire_owned() else { continue };
                        let Some(peer_slot) = PeerSlot::acquire(&accept_shared.peers, peer.ip()) else { continue };
                        let shared = accept_shared.clone();
                        let stop = conn_stop.clone();
                        tauri::async_runtime::spawn(async move {
                            let (_slot, _peer_slot) = (slot, peer_slot);
                            handle(stream, &shared, stop).await;
                        });
                    }
                }
            }
            drop(listener);
            let _ = closed_tx.send(());
        });

        let feed_shared = shared.clone();
        let mut feed_stop = shutdown.subscribe();
        tauri::async_runtime::spawn(async move {
            let mut tick = tokio::time::interval(FEED_INTERVAL);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = feed_stop.changed() => break,
                    _ = tick.tick() => {}
                }
                // Nobody watching: do not touch the meter at all.
                if feed_shared.feed.receiver_count() == 0 {
                    continue;
                }
                let make = feed_shared.hooks.snapshot.clone();
                let Ok(json) = tokio::task::spawn_blocking(move || make()).await else { continue };
                if &**feed_shared.feed.borrow() != json.as_str() {
                    feed_shared.feed.send_replace(Arc::from(json));
                }
            }
        });

        Ok(Self { addr, shared, shutdown, closed: Some(closed_rx) })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Swap the key. Cancel existing connections, including blocked writes.
    pub fn set_key(&self, key: String) {
        let mut current = self.shared.key.write();
        if *current == key {
            return;
        }
        *current = key;
        self.shared.key_revision.send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    /// Shut down and wait (briefly) for the port to be released.
    pub fn stop(mut self) {
        let _ = self.shutdown.send(true);
        if let Some(closed) = self.closed.take() {
            let _ = closed.recv_timeout(Duration::from_secs(2));
        }
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

struct Request {
    method: String,
    path: String,
    query: Vec<(String, String)>,
}

impl Request {
    fn param(&self, name: &str) -> Option<&str> {
        self.query.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

async fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let room = MAX_HEAD.checked_sub(buf.len())?;
        if room == 0 {
            return None;
        }
        let limit = room.min(chunk.len());
        let n = stream.read(&mut chunk[..limit]).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            buf.truncate(end);
            return String::from_utf8(buf).ok();
        }
    }
}

fn parse_request(head: &str) -> Option<Request> {
    let line = head.lines().next()?;
    let mut parts = line.split(' ');
    let method = parts.next()?.to_string();
    let target = parts.next()?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let query = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let decode = |s: &str| {
                urlencoding::decode(&s.replace('+', " ")).map(|c| c.into_owned()).unwrap_or_default()
            };
            (decode(k), decode(v))
        })
        .collect();
    Some(Request { method, path: path.to_string(), query })
}

async fn handle(stream: TcpStream, shared: &Shared, mut stop: watch::Receiver<bool>) {
    let mut revision = shared.key_revision.subscribe();
    if *stop.borrow() {
        return;
    }
    let response_stop = stop.clone();
    // Cancellation covers header reads, snapshot creation and every write, rather
    // than only the moments between SSE frames. A stalled peer is revoked too.
    tokio::select! {
        biased;
        _ = stop.changed() => {},
        _ = revision.changed() => {},
        _ = handle_request(stream, shared, response_stop) => {},
    }
}

async fn handle_request(mut stream: TcpStream, shared: &Shared, stop: watch::Receiver<bool>) {
    let head = match tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut stream)).await {
        Ok(Some(head)) => head,
        _ => return,
    };
    let Some(req) = parse_request(&head) else {
        let _ = respond(&mut stream, 400, "text/plain; charset=utf-8", &[], b"bad request").await;
        return;
    };
    if req.method != "GET" {
        let _ = respond(&mut stream, 405, "text/plain; charset=utf-8", &[("Allow", "GET")], b"GET only").await;
        return;
    }
    let given = req.param("key").unwrap_or("").to_string();
    if !key_matches(&given, &shared.key.read()) {
        let _ = respond(&mut stream, 403, "text/plain; charset=utf-8", &[], b"forbidden").await;
        return;
    }

    match req.path.as_str() {
        "/overlay" | "/overlay/" => {
            let labels = (shared.hooks.labels)(req.param("lang"));
            // `<` escaped so nothing in a label can close the script element.
            let config = serde_json::to_string(&labels).unwrap_or_else(|_| "{}".into()).replace('<', "\\u003c");
            let body = PAGE.replace("__OVERLAY_CONFIG__", &config);
            let csp = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
                       img-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'";
            let _ = respond(
                &mut stream,
                200,
                "text/html; charset=utf-8",
                &[("Content-Security-Policy", csp)],
                body.as_bytes(),
            )
            .await;
        }
        "/overlay/snapshot" => {
            let make = shared.hooks.snapshot.clone();
            let json = tokio::task::spawn_blocking(move || make()).await.unwrap_or_default();
            let _ = respond(&mut stream, 200, "application/json", &[], json.as_bytes()).await;
        }
        "/overlay/events" => events(stream, shared, given, stop).await,
        path => {
            let icon = path
                .strip_prefix("/overlay/class/")
                .and_then(|p| p.strip_suffix(".png"))
                .and_then(class_icon);
            match icon {
                Some(png) => {
                    let _ = respond(&mut stream, 200, "image/png", &[], png).await;
                }
                None => {
                    let _ = respond(&mut stream, 404, "text/plain; charset=utf-8", &[], b"not found").await;
                }
            }
        }
    }
}

/// Server-Sent Events: the snapshot whenever it changes, a comment now and
/// then to keep the stream alive. Ends when the key changes, the server
/// stops, or the client goes away.
async fn events(mut stream: TcpStream, shared: &Shared, key: String, mut stop: watch::Receiver<bool>) {
    let head = "HTTP/1.1 200 OK\r\n\
                Content-Type: text/event-stream; charset=utf-8\r\n\
                Cache-Control: no-store\r\n\
                X-Content-Type-Options: nosniff\r\n\
                Referrer-Policy: no-referrer\r\n\
                Connection: close\r\n\r\n\
                retry: 2000\n\n";
    if write_event(&mut stream, head.as_bytes()).await.is_err() {
        return;
    }
    let mut feed = shared.feed.subscribe();
    let mut last_sent = String::new();
    let mut check = tokio::time::interval(Duration::from_secs(1));
    let mut since_beat = Duration::ZERO;
    loop {
        let current: Arc<str> = feed.borrow_and_update().clone();
        if !current.is_empty() && *current != *last_sent {
            let frame = format!("data: {current}\n\n");
            if write_event(&mut stream, frame.as_bytes()).await.is_err() {
                return;
            }
            last_sent = current.to_string();
        }
        tokio::select! {
            _ = stop.changed() => return,
            changed = feed.changed() => if changed.is_err() { return },
            _ = check.tick() => {
                if !key_matches(&key, &shared.key.read()) {
                    return;
                }
                since_beat += Duration::from_secs(1);
                if since_beat >= HEARTBEAT_EVERY {
                    since_beat = Duration::ZERO;
                    if write_event(&mut stream, b": ping\n\n").await.is_err() {
                        return;
                    }
                }
            }
        }
    }
}

async fn write_event(stream: &mut TcpStream, bytes: &[u8]) -> std::io::Result<()> {
    tokio::time::timeout(WRITE_TIMEOUT, stream.write_all(bytes)).await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "overlay write timed out"))?
}

async fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    extra: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Service Unavailable",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\n\
         Connection: close\r\n",
        body.len()
    );
    for (name, value) in extra {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    tokio::time::timeout(WRITE_TIMEOUT, async {
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(body).await?;
        stream.flush().await?;
        stream.shutdown().await
    }).await.map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "overlay response timed out"))?
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{Read, Write};

    pub fn test_hooks(json: &'static str) -> Hooks {
        Hooks {
            snapshot: Arc::new(move || json.to_string()),
            labels: Arc::new(|lang| PageLabels {
                lang: lang.unwrap_or("en").to_string(),
                waiting: "Waiting <b>".into(),
                reconnecting: "Reconnecting".into(),
                dps_suffix: "/s".into(),
                classes: Default::default(),
                player: "Player".into(),
            }),
        }
    }

    /// One plain blocking request; returns the status code and the body.
    pub fn get(addr: SocketAddr, target: &str) -> (u16, String) {
        request(addr, &format!("GET {target} HTTP/1.1\r\nHost: x\r\n\r\n"))
    }

    fn request(addr: SocketAddr, raw: &str) -> (u16, String) {
        let mut s = std::net::TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.write_all(raw.as_bytes()).unwrap();
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);
        let text = String::from_utf8_lossy(&out).to_string();
        let status = text.split(' ').nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
        let body = text.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
        (status, body)
    }

    fn local() -> SocketAddr {
        "127.0.0.1:0".parse().unwrap()
    }

    #[test]
    fn keys_compare_whole_and_exactly() {
        assert!(key_matches("abc123", "abc123"));
        assert!(!key_matches("abc124", "abc123"));
        assert!(!key_matches("abc12", "abc123"));
        assert!(!key_matches("abc1234", "abc123"));
        assert!(!key_matches("", ""), "an unset key opens nothing");
        assert!(!key_matches("ABC123", "abc123"));
    }

    #[test]
    fn every_route_wants_the_key_and_get() {
        let server = RunningServer::start(local(), "k3y".into(), test_hooks(r#"{"v":1}"#)).unwrap();
        let addr = server.local_addr();
        assert_eq!(get(addr, "/overlay").0, 403);
        assert_eq!(get(addr, "/overlay?key=nope").0, 403);
        assert_eq!(get(addr, "/overlay/snapshot?key=k3").0, 403);
        assert_eq!(get(addr, "/anything").0, 403, "unknown paths do not answer without the key");
        assert_eq!(get(addr, "/anything?key=k3y").0, 404);
        assert_eq!(request(addr, "POST /overlay?key=k3y HTTP/1.1\r\nContent-Length: 0\r\n\r\n").0, 405);

        let (status, body) = get(addr, "/overlay/snapshot?key=k3y");
        assert_eq!((status, body.as_str()), (200, r#"{"v":1}"#));

        let (status, page) = get(addr, "/overlay?key=k3y&lang=de");
        assert_eq!(status, 200);
        assert!(page.contains(r#""lang":"de""#));
        assert!(page.contains(r"Waiting \u003cb>"), "labels cannot break out of the script");
        assert!(!page.contains("Waiting <b>"));
        assert!(!page.contains("__OVERLAY_CONFIG__"));

        let (status, png) = get(addr, "/overlay/class/cleric.png?key=k3y");
        assert_eq!(status, 200);
        assert!(png.len() > 8);
        assert_eq!(get(addr, "/overlay/class/../x.png?key=k3y").0, 404);
        server.stop();
    }

    #[test]
    fn a_new_key_shuts_out_the_old_one() {
        let server = RunningServer::start(local(), "old".into(), test_hooks("{}")).unwrap();
        let addr = server.local_addr();
        assert_eq!(get(addr, "/overlay/snapshot?key=old").0, 200);
        server.set_key("new".into());
        assert_eq!(get(addr, "/overlay/snapshot?key=old").0, 403);
        assert_eq!(get(addr, "/overlay/snapshot?key=new").0, 200);
        server.stop();
    }

    fn wait_until(mut condition: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while !condition() {
            assert!(std::time::Instant::now() < deadline, "server did not settle within one second");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn the_header_limit_includes_the_terminator() {
        let server = RunningServer::start(local(), "k".into(), test_hooks("{}")).unwrap();
        let mut head = "GET /overlay/snapshot?key=k HTTP/1.1\r\nHost: x\r\nX-Fill: ".to_string();
        head.extend(std::iter::repeat_n('a', MAX_HEAD - 4 - head.len()));
        head.push_str("\r\n\r\n");
        assert_eq!(head.len(), MAX_HEAD);
        assert_eq!(request(server.local_addr(), &head).0, 200);
        head.insert(head.len() - 4, 'a');
        assert_eq!(request(server.local_addr(), &head).0, 0, "an oversized header is closed before routing");
        server.stop();
    }

    #[test]
    fn excess_slow_connections_do_not_create_more_handlers() {
        let server = RunningServer::start(local(), "k".into(), test_hooks("{}")).unwrap();
        let mut held = Vec::new();
        for _ in 0..MAX_PEER_CONNECTIONS {
            held.push(std::net::TcpStream::connect(server.local_addr()).unwrap());
        }
        wait_until(|| server.shared.slots.available_permits() == MAX_CONNECTIONS - MAX_PEER_CONNECTIONS);
        for _ in 0..20 {
            let mut excess = std::net::TcpStream::connect(server.local_addr()).unwrap();
            excess.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
            let mut byte = [0];
            match excess.read(&mut byte) {
                Ok(0) => {},
                Err(e) if matches!(e.kind(), std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted) => {},
                result => panic!("excess connection was not promptly closed: {result:?}"),
            }
        }
        assert_eq!(server.shared.slots.available_permits(), MAX_CONNECTIONS - MAX_PEER_CONNECTIONS);
        assert_eq!(server.shared.peers.lock().len(), 1);
        let shared = server.shared.clone();
        server.stop();
        wait_until(|| shared.slots.available_permits() == MAX_CONNECTIONS);
        assert!(shared.peers.lock().is_empty(), "peer bookkeeping expires with its last connection");
    }

    #[test]
    fn rotating_the_key_cancels_a_stalled_event_write() {
        let server = RunningServer::start(local(), "old".into(), test_hooks("{}")).unwrap();
        let mut slow = std::net::TcpStream::connect(server.local_addr()).unwrap();
        slow.write_all(b"GET /overlay/events?key=old HTTP/1.1\r\n\r\n").unwrap();
        wait_until(|| server.shared.feed.receiver_count() == 1);
        // The client never drains its receive buffer. This frame is larger
        // than the socket's send window, so its write has to wait for it.
        server.shared.feed.send_replace(Arc::from("x".repeat(16 * 1024 * 1024)));
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(server.shared.slots.available_permits(), MAX_CONNECTIONS - 1);
        server.set_key("new".into());
        wait_until(|| server.shared.slots.available_permits() == MAX_CONNECTIONS);
        assert_eq!(server.shared.feed.receiver_count(), 0);
        assert_eq!(get(server.local_addr(), "/overlay/snapshot?key=new").0, 200);
        server.stop();
    }

    #[test]
    fn the_event_stream_pushes_the_snapshot() {
        let server = RunningServer::start(local(), "k".into(), test_hooks(r#"{"rows":[]}"#)).unwrap();
        let mut s = std::net::TcpStream::connect(server.local_addr()).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.write_all(b"GET /overlay/events?key=k HTTP/1.1\r\n\r\n").unwrap();
        let mut got = String::new();
        let mut buf = [0u8; 512];
        while !got.contains("data: ") || !got.ends_with("\n\n") {
            let n = s.read(&mut buf).unwrap();
            assert!(n > 0, "stream closed early: {got}");
            got.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        assert!(got.starts_with("HTTP/1.1 200 OK"));
        assert!(got.contains("text/event-stream"));
        assert!(got.contains(r#"data: {"rows":[]}"#));
        // Settings sync calls set_key even when only a display setting changed.
        // That must not interrupt an existing OBS source.
        server.set_key("k".into());
        server.shared.feed.send_replace(Arc::from(r#"{"rows":[1]}"#));
        while !got.contains(r#"data: {"rows":[1]}"#) {
            let n = s.read(&mut buf).unwrap();
            assert!(n > 0, "unchanged key closed the source");
            got.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        server.stop();
        // The stream ends with the server.
        let mut rest = Vec::new();
        let _ = s.read_to_end(&mut rest);
    }
}

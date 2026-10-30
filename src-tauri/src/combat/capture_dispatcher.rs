use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::capture::captured_payload::CapturedPayload;
use crate::capture::combat_port_detector::CombatPortDetector;
use crate::capture::stream_assembler::StreamAssembler;
use crate::capture::tcp_order::{Piece, TcpOrder};
use crate::capture::stream_processor::StreamProcessor;
use crate::combat::data_storage::DataStorage;
use crate::combat::ping_tracker::PingTracker;
use crate::i18n::lookup::{NpcLookup, SkillLookup};
use crate::platform::window_detector;

/// Pre-lock combat signatures: a cheap gate deciding which packets are worth
/// running through the parser before a port is locked. The port only actually
/// locks when the parser extracts *real damage* (see `run`), so this gate just
/// limits parse attempts — it does not by itself decide the lock.
///
/// The game's per-record terminator `?? 00 36` had leading byte `0x06`
/// pre-2026-06; the June 2026 update changed it to `0x0E` (`06 00 36` ->
/// `0E 00 36`), which silently broke the old single-magic port detection while
/// leaving the parser itself unaffected. We accept both leading bytes so the
/// gate survives that transition.
const COMBAT_SIGNATURES: [&[u8]; 2] = [
    &[0x0E, 0x00, 0x36], // current (post June 2026) record terminator
    &[0x06, 0x00, 0x36], // legacy terminator (pre June 2026)
];
/// How many signature-bearing server->client packets a single flow must produce
/// *within SIGNATURE_WINDOW_MS* before it may lock the port. The live game stream
/// emits the record terminator ~19x/sec even while idle (movement/heartbeat), so it
/// clears this in well under a second. A coincidental loopback service (e.g. a local
/// helper on port 16005 during a game cold-start) produces the 3-byte pattern only a
/// handful of times over minutes and never reaches the threshold inside the window —
/// so it can no longer hijack the lock. The window (vs. a plain running total) means
/// only a *high-rate* flow qualifies, not one that slowly drips coincidental matches.
const SIGNATURE_LOCK_THRESHOLD: u32 = 12;
/// Sliding window for the signature-rate lock; the THRESHOLD packets must land within
/// this span. Reset the per-flow count whenever the gap since the last hit exceeds it.
const SIGNATURE_WINDOW_MS: i64 = 3_000;
const TLS_CONTENT_TYPES: [u8; 4] = [0x14, 0x15, 0x16, 0x17];
const TLS_VERSIONS: [u8; 5] = [0x00, 0x01, 0x02, 0x03, 0x04];
const WINDOW_CHECK_STOPPED_MS: i64 = 10_000;
const WINDOW_CHECK_RUNNING_MS: i64 = 60_000;
/// Connections read while unlocked, each with its own parser: past this, the
/// one whose signature went quiet longest makes room. Spoofed packets that
/// carry a signature would otherwise grow a parser each (#131).
const MAX_CANDIDATE_FLOWS: usize = 16;
// Keep first-segment decisions even when an assembler is discarded. Evicting
// a TLS decision could admit the middle of that encrypted stream as game data.
// At the limit, existing connections continue; new ones wait until the game is
// observed closed or the meter restarts.
const MAX_TRACKED_CONNECTIONS: usize = 4096;
pub(crate) const STALE_CONNECTION_MS: i64 = 120_000;
/// Once locked, a connection on another port takes over the lock when the
/// locked port has sent nothing for this long and the new one sustains the
/// lock's signature rate: switching server opens a new connection to the
/// other server's port, and the meter waited STALE_CONNECTION_MS on the old
/// one, missing what the game sends on entering the world (the pet list,
/// once per entry: a player's Pet Soul Tracker never synced, 2026-10-19).
pub(crate) const PORT_MOVE_QUIET_MS: i64 = 5_000;
/// While the locked port is quiet, what other connections send is kept (at
/// most this many connections, this much each) until one proves to be the
/// game, then replayed in order, so its first packets are read too.
const PORT_MOVE_FLOWS: usize = 6;
const PORT_MOVE_FLOW_BYTES: usize = 4 * 1024 * 1024;

/// A connection seen while the locked port was quiet.
#[derive(Default)]
struct WaitingFlow {
    packets: Vec<CapturedPayload>,
    bytes: usize,
    /// Signature hits in the window, and the last one.
    hits: (u32, i64),
}

/// The connections seen while the locked port is quiet, until one sustains
/// the game's signature rate (see PORT_MOVE_QUIET_MS).
#[derive(Default)]
struct WaitingFlows {
    flows: HashMap<(u16, u16), WaitingFlow>,
}

impl WaitingFlows {
    /// Keep `cap` (not on the locked port, which is quiet). When its
    /// connection proves to be the game: the port the game now sends from,
    /// and the connection's packets so far, in order, to read.
    fn offer(&mut self, cap: CapturedPayload, now: i64) -> Option<(u16, Vec<CapturedPayload>)> {
        if looks_like_tls(&cap.data) {
            return None;
        }
        let key = (cap.src_port.min(cap.dst_port), cap.src_port.max(cap.dst_port));
        if !self.flows.contains_key(&key) && self.flows.len() >= PORT_MOVE_FLOWS {
            return None;
        }
        let flow = self.flows.entry(key).or_default();
        if flow.bytes + cap.data.len() > PORT_MOVE_FLOW_BYTES {
            return None;
        }
        // The signature only ever comes from the server's side.
        let signed = contains_any(&cap.data, &COMBAT_SIGNATURES);
        if signed {
            if now - flow.hits.1 > SIGNATURE_WINDOW_MS {
                flow.hits.0 = 0;
            }
            flow.hits.0 += 1;
            flow.hits.1 = now;
        }
        let server_port = cap.src_port;
        flow.bytes += cap.data.len();
        flow.packets.push(cap);
        if !(signed && flow.hits.0 >= SIGNATURE_LOCK_THRESHOLD) {
            return None;
        }
        let packets = self.flows.remove(&key).map(|f| f.packets).unwrap_or_default();
        self.flows.clear();
        Some((server_port, packets))
    }

    fn clear(&mut self) {
        self.flows.clear();
    }
}
/// Once locked, the game's stream on another device takes over the lock when
/// the locked device has carried nothing from the game's port for this long
/// and the other one sustains the lock's signature rate. A relay or ping
/// reducer repeats the stream on a second device while the locked one stays
/// busy, so it never moves the lock; a VPN connecting mid-session, or an
/// instance server routed differently, does. Before this the meter waited
/// STALE_CONNECTION_MS, missing two minutes of a raid.
const DEVICE_MOVE_QUIET_MS: i64 = 5_000;
/// While no port is locked, how often to log what the capture is seeing. Before
/// the lock every gate is silent, so without this a meter that never locks
/// leaves a log that cannot say why.
const UNLOCKED_REPORT_MS: i64 = 30_000;

/// Per-device packet counts while unlocked, for the periodic report. It is
/// written when a packet arrives, so a capture that sees nothing at all stays
/// silent; the device list at startup covers that case.
#[derive(Default)]
struct UnlockedStats {
    /// device -> (packets, packets carrying a combat signature)
    by_device: HashMap<String, (u64, u64)>,
    /// Packets dropped because no AION2 window was found.
    no_window: u64,
}

impl UnlockedStats {
    fn note(&mut self, cap: &CapturedPayload) {
        let device = cap.device_name.clone().unwrap_or_else(|| "?".into());
        let entry = self.by_device.entry(device).or_default();
        entry.0 += 1;
        if contains_any(&cap.data, &COMBAT_SIGNATURES) {
            entry.1 += 1;
        }
    }

    fn report(&self, window_found: bool) -> String {
        let mut devices: Vec<_> = self.by_device.iter().collect();
        devices.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
        let mut out = format!(
            "Not locked yet (last {} s): AION2 window {}",
            UNLOCKED_REPORT_MS / 1000,
            if window_found { "found" } else { "NOT found" }
        );
        if self.no_window > 0 {
            out += &format!(", {} packets ignored for that", self.no_window);
        }
        if !window_found {
            // What might have been the game, so the next log says why it was
            // not recognised (a localised title, a launcher, ...).
            let candidates = window_detector::describe_candidates();
            if candidates.is_empty() {
                out += " (no window or program mentions \"aion\")";
            } else {
                out += &format!(" (look-alikes: {})", candidates.join(" | "));
            }
        }
        for (device, (packets, marked)) in devices.iter().take(8) {
            out += &format!("; {}: {} packets, {} with game markers", device, packets, marked);
        }
        out
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Routes captured packets through port detection, filtering, and the parsing pipeline.
pub struct CaptureDispatcher {
    data_storage: Arc<DataStorage>,
    skill_lookup: Arc<SkillLookup>,
    npc_lookup: Arc<NpcLookup>,
    port_detector: Arc<CombatPortDetector>,
    ping_tracker: Arc<PingTracker>,
    dot_skill_ids: std::collections::HashSet<i32>,
    suspended: Arc<AtomicBool>,
}

impl CaptureDispatcher {
    pub fn new(
        data_storage: Arc<DataStorage>,
        skill_lookup: Arc<SkillLookup>,
        npc_lookup: Arc<NpcLookup>,
        port_detector: Arc<CombatPortDetector>,
        ping_tracker: Arc<PingTracker>,
    ) -> Self {
        Self {
            data_storage,
            skill_lookup,
            npc_lookup,
            port_detector,
            ping_tracker,
            dot_skill_ids: std::collections::HashSet::new(),
            suspended: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn set_dot_skill_ids(&mut self, ids: std::collections::HashSet<i32>) {
        self.dot_skill_ids = ids;
    }

    /// Share the "suspended" switch with whoever flips it (the header's
    /// suspend button, through `suspend_capture`). While it is on, captured
    /// packets are dropped, so nothing is counted and the fight timer stops.
    pub fn use_suspend_flag(&mut self, flag: Arc<AtomicBool>) {
        self.suspended = flag;
    }

    /// Run the dispatch loop, consuming packets from the channel.
    pub async fn run(&self, mut receiver: mpsc::Receiver<CapturedPayload>) {
        let mut assemblers: HashMap<(u16, u16), (StreamAssembler, StreamProcessor, TcpOrder)> = HashMap::new();
        // Per-flow count of signature-bearing packets seen while still unlocked, used
        // for the no-combat-needed signature lock (see SIGNATURE_LOCK_THRESHOLD).
        // Per-flow signature-rate tracker: (count_in_window, last_hit_ms).
        let mut sig_hits: HashMap<(u16, u16), (u32, i64)> = HashMap::new();
        let mut connections = ConnectionFilter::default();
        let mut last_window_check_ms: i64 = 0;
        let mut is_aion_running = false;
        let mut window_logged: Option<bool> = None;
        let mut unlocked_stats = UnlockedStats::default();
        let mut last_unlocked_report_ms = now_ms();
        // Locked: when the game's port last sent on the locked device, and the
        // signature rate of that port on other devices (see DEVICE_MOVE_QUIET_MS).
        let mut last_on_locked_device_ms: i64 = 0;
        let mut elsewhere: HashMap<String, (u32, i64)> = HashMap::new();
        // The game server's address, as last told to DataStorage.
        let mut last_game_host: Option<std::net::Ipv4Addr> = None;
        // Wall clock minus capture clock. Npcap sets its clock from the
        // system's once, when the driver loads, and the two drift apart over
        // days; a fight is over 10 s after its last hit by the wall clock.
        // Measured when the queue is empty, so the packet is fresh and no
        // backlog is counted in.
        let mut capture_clock_offset: Option<i64> = None;
        // Connections seen while the locked port is quiet (PORT_MOVE_QUIET_MS),
        // and the packets of the one that took the lock, to read again.
        let mut waiting = WaitingFlows::default();
        let mut replay: std::collections::VecDeque<CapturedPayload> = std::collections::VecDeque::new();

        loop {
            let cap = match replay.pop_front() {
                Some(cap) => cap,
                None => match receiver.recv().await {
                    Some(cap) => cap,
                    None => break,
                },
            };
            if self.suspended.load(Ordering::SeqCst) {
                continue;
            }

            // Check AION window
            let now = now_ms();
            let interval = if is_aion_running { WINDOW_CHECK_RUNNING_MS } else { WINDOW_CHECK_STOPPED_MS };
            if now - last_window_check_ms >= interval {
                last_window_check_ms = now;
                let title = window_detector::find_aion2_window_title();
                let running = title.is_some();
                if window_logged != Some(running) {
                    match &title {
                        Some(t) => info!("AION2 window found: {:?}", t),
                        None => info!(
                            "No AION2 window found (looking for a title starting with \"AION2\", or a window owned by AION2.exe); packets are ignored until there is one"
                        ),
                    }
                    window_logged = Some(running);
                }
                if !running && is_aion_running {
                    self.port_detector.reset();
                    self.ping_tracker.reset();
                    assemblers.clear();
                    sig_hits.clear();
                    connections = ConnectionFilter::default();
                }
                is_aion_running = running;
            }

            // While unlocked, count what arrives on each device and report it
            // now and then, so a log from a meter that never locks says why.
            if self.port_detector.current_port().is_none() {
                unlocked_stats.note(&cap);
                if !is_aion_running {
                    unlocked_stats.no_window += 1;
                }
                if now - last_unlocked_report_ms >= UNLOCKED_REPORT_MS {
                    info!("{}", unlocked_stats.report(is_aion_running));
                    unlocked_stats = UnlockedStats::default();
                    last_unlocked_report_ms = now;
                }
            } else {
                last_unlocked_report_ms = now;
            }

            if !is_aion_running {
                continue;
            }

            // Stale connection check
            if is_aion_running && self.port_detector.current_port().is_some() {
                let last_parsed = self.port_detector.last_parsed_at_ms();
                if last_parsed > 0 && now - last_parsed > STALE_CONNECTION_MS {
                    info!("No packets parsed for {}ms, resetting lock", now - last_parsed);
                    self.port_detector.reset();
                    self.ping_tracker.reset();
                    assemblers.clear();
                    sig_hits.clear();
                }
            }

            let current_port = self.port_detector.current_port();
            let locked_device = self.port_detector.current_device();

            // Device filter
            if let Some(ref dev) = locked_device {
                // Counted from the lock: a relay's second copy of the stream
                // right after it doesn't move it.
                if last_on_locked_device_ms == 0 {
                    last_on_locked_device_ms = now;
                }
                let from_game_port = current_port.is_some_and(|port| cap.src_port == port);
                if !device_matches(dev, cap.device_name.as_deref()) {
                    let Some(other) = cap.device_name.as_deref().map(str::trim).filter(|d| !d.is_empty()) else { continue };
                    if !from_game_port || !contains_any(&cap.data, &COMBAT_SIGNATURES) {
                        continue;
                    }
                    // A device picked in Settings stays the one captured: the
                    // lock never moves off it to another.
                    if self.port_detector.preferred_device().is_some_and(|pref| !device_matches(&pref, Some(other))) {
                        continue;
                    }
                    let slot = elsewhere.entry(other.to_string()).or_insert((0, now));
                    if now - slot.1 > SIGNATURE_WINDOW_MS {
                        slot.0 = 0;
                    }
                    slot.0 += 1;
                    slot.1 = now;
                    let quiet = now - last_on_locked_device_ms >= DEVICE_MOVE_QUIET_MS;
                    if slot.0 < SIGNATURE_LOCK_THRESHOLD || !quiet {
                        continue;
                    }
                    // The stream continues on the other device; its own
                    // connection gets a fresh assembler below.
                    self.port_detector.move_device(other);
                    elsewhere.clear();
                    assemblers.clear();
                    last_on_locked_device_ms = now;
                } else if from_game_port {
                    last_on_locked_device_ms = now;
                }
            } else {
                last_on_locked_device_ms = 0;
                elsewhere.clear();
            }

            // Preferred device filter
            if current_port.is_none() {
                if let Some(ref pref) = self.port_detector.preferred_device() {
                    if !device_matches(pref, cap.device_name.as_deref()) {
                        continue;
                    }
                }
            }

            // Port filter. While the locked port is quiet, another connection
            // carrying the game's signature rate takes the lock (a server
            // switch), its packets so far read first (see PORT_MOVE_QUIET_MS).
            if let Some(port) = current_port {
                if cap.src_port == port {
                    self.port_detector.mark_from_port();
                    waiting.clear();
                }
                if cap.src_port != port && cap.dst_port != port {
                    let quiet = self.port_detector.port_quiet_ms(now).is_some_and(|q| q >= PORT_MOVE_QUIET_MS);
                    if !quiet {
                        continue;
                    }
                    if let Some((new_port, packets)) = waiting.offer(cap, now) {
                        self.port_detector.move_port(new_port);
                        assemblers.clear();
                        sig_hits.clear();
                        replay.extend(packets);
                    }
                    continue;
                }
            }

            // Feed to ping tracker — also marks connection alive to prevent stale reset
            if let Some(port) = current_port {
                let had_ping_before = self.ping_tracker.current_ping_ms();
                self.ping_tracker.on_packet(&cap, port);
                let has_ping_now = self.ping_tracker.current_ping_ms();
                // If a new ping was received, mark the connection as active
                if has_ping_now != had_ping_before {
                    self.port_detector.mark_packet_parsed();
                }
            }

            // Only parse server->client (src == locked port)
            if let Some(port) = current_port {
                if cap.src_port != port {
                    continue;
                }
                if cap.src_ip.is_some() && cap.src_ip != last_game_host {
                    last_game_host = cap.src_ip;
                    self.data_storage.set_game_host(cap.src_ip);
                }
            }

            // Pre-lock filters. Once a port is locked these checks are skipped
            // entirely (the port/direction filters above already gate traffic),
            // keeping the hot path cheap during heavy combat.
            let unlocked = current_port.is_none();
            if unlocked {
                if looks_like_tls(&cap.data) {
                    continue;
                }
                if !contains_any(&cap.data, &COMBAT_SIGNATURES) {
                    continue;
                }
            }

            if !connections.admit(&cap) {
                continue;
            }

            // Get or create assembler
            let a = cap.src_port.min(cap.dst_port);
            let b = cap.src_port.max(cap.dst_port);
            let key = (a, b);

            if unlocked && assemblers.len() >= MAX_CANDIDATE_FLOWS && !assemblers.contains_key(&key) {
                let quietest = assemblers.keys().copied().min_by_key(|k| sig_hits.get(k).map_or(0, |h| h.1));
                if let Some(k) = quietest {
                    assemblers.remove(&k);
                    sig_hits.remove(&k);
                }
            }

            let (assembler, processor, order) = assemblers.entry(key).or_insert_with(|| {
                let mut proc = StreamProcessor::new(self.data_storage.clone(), self.skill_lookup.clone(), self.npc_lookup.clone());
                proc.set_dot_skill_ids(self.dot_skill_ids.clone());
                (StreamAssembler::new(), proc, TcpOrder::default())
            });

            if unlocked {
                self.port_detector.register_candidate(cap.src_port, key, cap.device_name.as_deref());
                // Count this signature-bearing packet against its source port (the
                // signature only ever travels server->client). The lock decision is
                // made below, after process_chunk, so we don't touch `assemblers`
                // while the assembler for this flow is still borrowed.
                // Windowed signature rate: reset the count if too long since the last
                // hit, so only a sustained high-rate flow (the live game) accumulates.
                let now = now_ms();
                let slot = sig_hits.entry(key).or_insert((0, now));
                if now - slot.1 > SIGNATURE_WINDOW_MS {
                    slot.0 = 0;
                }
                slot.0 += 1;
                slot.1 = now;
            }

            // A flow locks the port only by sustaining the game's signature RATE
            // (SIGNATURE_LOCK_THRESHOLD hits within SIGNATURE_WINDOW_MS). We deliberately
            // do NOT lock on a single parsed-damage event any more: a coincidental
            // loopback service can momentarily misparse as "damage" and steal the lock
            // during a cold start (observed locking onto port 16005 instead of the game).
            // Real combat produces a flood of signatures too, so the rate gate covers
            // both idle and combat while staying robust. Spawns/names are still parsed
            // into the store pre-lock, so mobs seen before the first fight stay identified.
            // Hits are timed when they were captured, not when they are parsed:
            // a parser held up for a minute (a save waited on, a busy machine)
            // then read the whole queue at once, and a two-minute boss fight
            // came out two seconds long, at 700k DPS. Set and cleared around
            // the parse, with no await between, so nothing else on this thread
            // reads a capture time as "now".
            let captured_at = cap.capture_time_ms().map(|at| {
                if capture_clock_offset.is_none() || receiver.is_empty() {
                    capture_clock_offset = Some(now - at);
                }
                at + capture_clock_offset.unwrap_or(0)
            });
            // The server's bytes in its order (`tcp_order`): late segments put
            // back, retransmitted bytes read once, a hole reported. A payload
            // with no sequence number (0: a test's) is read as it comes.
            let pieces = if cap.tcp_seq == 0 {
                vec![Piece::Bytes(cap.data.clone())]
            } else {
                order.push(cap.tcp_seq, &cap.data, cap.captured_at_ms)
            };
            processor.set_override_timestamp(captured_at);
            let mut parsed = false;
            for piece in pieces {
                match piece {
                    Piece::Gap { missing } => {
                        warn!(
                            "{} bytes from port {} never reached the capture (on {}): reading on from the next whole packet",
                            missing,
                            cap.src_port,
                            cap.device_name.as_deref().unwrap_or("?")
                        );
                        assembler.lose_bytes();
                    }
                    Piece::Bytes(data) => {
                        let ordered = CapturedPayload {
                            src_port: cap.src_port,
                            dst_port: cap.dst_port,
                            data,
                            device_name: cap.device_name.clone(),
                            captured_at_ms: cap.captured_at_ms,
                            src_ip: cap.src_ip,
                            dst_ip: cap.dst_ip,
                            tcp_seq: cap.tcp_seq,
                            tcp_ack: cap.tcp_ack,
                        };
                        // Log raw packet if packet logging is enabled
                        crate::logging::logger::log_packet(&ordered);
                        // And keep it in memory for a while, so a boss fight can be shared
                        // without packet logging having been on. See `share::ring`.
                        crate::share::ring::record(&ordered);
                        parsed |= assembler.process_chunk(&ordered.data, processor);
                    }
                }
            }
            processor.set_override_timestamp(None);

            let signature_locked =
                unlocked && sig_hits.get(&key).map(|(c, _)| *c).unwrap_or(0) >= SIGNATURE_LOCK_THRESHOLD;
            if signature_locked && self.port_detector.current_port().is_none() {
                self.port_detector.confirm_candidate(cap.src_port, cap.dst_port, cap.device_name.as_deref());
                // On lock, GC the orphaned candidate assemblers (the relay's
                // duplicate external flows) so only the locked flow is processed.
                if self.port_detector.current_port().is_some() {
                    assemblers.retain(|k, _| *k == key);
                    sig_hits.clear();
                }
            }

            if parsed {
                self.port_detector.mark_packet_parsed();
            }
        }
    }
}

fn looks_like_tls(data: &[u8]) -> bool {
    if data.len() < 3 {
        return false;
    }
    let content_type = data[0];
    let major = data[1];
    let minor = data[2];
    TLS_CONTENT_TYPES.contains(&content_type) && major == 0x03 && TLS_VERSIONS.contains(&minor)
}

/// The game server also talks TLS from its port, on a second connection. A
/// connection whose first segment is a TLS record is left out whole: of the
/// log, the slices and the parser. Decided once per connection, so no byte
/// inside the game's stream is ever skipped.
#[derive(Default)]
struct ConnectionFilter {
    tls: HashMap<(u16, u16), bool>,
}

impl ConnectionFilter {
    fn admit(&mut self, cap: &CapturedPayload) -> bool {
        let connection = (cap.src_port.min(cap.dst_port), cap.src_port.max(cap.dst_port));
        if let Some(&tls) = self.tls.get(&connection) {
            return !tls;
        }
        if self.tls.len() >= MAX_TRACKED_CONNECTIONS {
            return false;
        }
        let tls = is_tls_record(&cap.data);
        if tls {
            tracing::info!("Connection {} -> {} carries TLS: not the game, left out", cap.src_port, cap.dst_port);
        }
        self.tls.insert(connection, tls);
        !tls
    }
}

/// A whole TLS record header: content type 20-23, version 3.x, and a length
/// a record can have (at most 2^14 + 256 bytes).
fn is_tls_record(data: &[u8]) -> bool {
    if data.len() < 5 || !looks_like_tls(data) {
        return false;
    }
    let len = u16::from_be_bytes([data[3], data[4]]) as usize;
    (1..=16_384 + 256).contains(&len)
}

fn contains_bytes(data: &[u8], needle: &[u8]) -> bool {
    needle.len() <= data.len() && data.windows(needle.len()).any(|w| w == needle)
}

fn contains_any(data: &[u8], needles: &[&[u8]]) -> bool {
    needles.iter().any(|n| contains_bytes(data, n))
}

fn device_matches(locked: &str, packet_device: Option<&str>) -> bool {
    match packet_device {
        Some(d) if !d.trim().is_empty() => d.trim().eq_ignore_ascii_case(locked),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tls_record_header_is_told_from_game_bytes() {
        // TLS application data, 1402 bytes, as a second connection sends it.
        assert!(is_tls_record(&[0x17, 0x03, 0x03, 0x05, 0x7a, 0x88]));
        // A server hello.
        assert!(is_tls_record(&[0x16, 0x03, 0x03, 0x00, 0x5d]));
        // Game frames: a map load and a damage record.
        assert!(!is_tls_record(&[0x34, 0x21, 0x36, 0x01, 0x00, 0x00]));
        assert!(!is_tls_record(&[0x24, 0x04, 0x38, 0xfe, 0x9e, 0x02]));
        // Too short, or a length no record has.
        assert!(!is_tls_record(&[0x17, 0x03, 0x03, 0x05]));
        assert!(!is_tls_record(&[0x17, 0x03, 0x03, 0xff, 0xff]));
    }

    #[test]
    fn each_connection_has_its_own_stream_name() {
        use crate::capture::captured_payload::stream_key;
        assert_eq!(stream_key(13328, 50349), "Client:50349:13328");
        assert_ne!(stream_key(13328, 50349), stream_key(13328, 50350));
        // Readers take the server port from after the last ':'.
        assert_eq!(stream_key(13328, 50349).rsplit(':').next(), Some("13328"));
    }

    #[test]
    fn a_tls_connection_from_the_game_port_is_left_out_whole() {
        let seg = |client: u16, data: &[u8]| CapturedPayload { src_port: 13328, dst_port: client, ..cap("eth0", data) };
        let mut filter = ConnectionFilter::default();
        // The game's connection, then a TLS one from the same server port, interleaved.
        assert!(filter.admit(&seg(50349, &[0x24, 0x04, 0x38, 0xfe, 0x9e, 0x02])));
        assert!(!filter.admit(&seg(50350, &[0x17, 0x03, 0x03, 0x05, 0x7a, 0x88, 0x71])));
        assert!(filter.admit(&seg(50349, &[0x34, 0x21, 0x36, 0x01])));
        // Later TLS segments, whatever they start with, stay out.
        assert!(!filter.admit(&seg(50350, &[0x88, 0x71, 0x04, 0x38])));
        // A game segment that happens to start like a TLS header later on is
        // not judged again: its connection is already known as the game's.
        assert!(filter.admit(&seg(50349, &[0x17, 0x03, 0x03, 0x00, 0x10])));
    }

    #[test]
    fn connection_limit_preserves_game_and_tls_decisions() {
        let mut filter = ConnectionFilter::default();
        let game = flow(13328, 50_000, &[0x24, 0x04, 0x38]);
        let tls = flow(13328, 50_001, &[0x17, 0x03, 0x03, 0x00, 0x10]);
        assert!(filter.admit(&game));
        assert!(!filter.admit(&tls));
        for port in 1..MAX_TRACKED_CONNECTIONS as u16 {
            filter.admit(&flow(13328, port, &[0x24, 0x04, 0x38]));
        }
        assert_eq!(filter.tls.len(), MAX_TRACKED_CONNECTIONS);
        for port in 50_002..51_002 {
            assert!(!filter.admit(&flow(13328, port, &[0x24, 0x04, 0x38])));
        }
        assert!(filter.admit(&game), "known game connection continues when full");
        assert!(!filter.admit(&flow(13328, 50_001, &[0x88, 0x71, 0x04, 0x38])), "TLS continuation stays excluded");
        assert_eq!(filter.tls.len(), MAX_TRACKED_CONNECTIONS);
    }

    fn cap(device: &str, data: &[u8]) -> CapturedPayload {
        CapturedPayload {
            src_port: 1,
            dst_port: 2,
            data: data.to_vec(),
            device_name: Some(device.into()),
            captured_at_ms: 0,
            src_ip: None,
            dst_ip: None,
            tcp_seq: 0,
            tcp_ack: 0,
        }
    }

    fn flow(src: u16, dst: u16, data: &[u8]) -> CapturedPayload {
        let mut c = cap("eth0", data);
        c.src_port = src;
        c.dst_port = dst;
        c
    }

    /// A server switch: a new connection that sustains the game's signature
    /// rate takes the lock, its packets so far (the login's, signature or
    /// not) handed back in order to be read.
    #[test]
    fn a_new_game_connection_takes_the_lock_with_its_first_packets() {
        let mut waiting = WaitingFlows::default();
        let sig = COMBAT_SIGNATURES[0];
        let login = flow(13329, 50000, &[0x0B, 0x00, 0x90, 1, 2, 3]);
        assert!(waiting.offer(login, 1_000).is_none());
        let mut got = None;
        for i in 0..SIGNATURE_LOCK_THRESHOLD {
            let mut data = vec![0x10, i as u8];
            data.extend_from_slice(sig);
            got = waiting.offer(flow(13329, 50000, &data), 1_000 + i as i64 * 10);
            if got.is_some() {
                break;
            }
        }
        let (port, packets) = got.expect("the new connection takes the lock");
        assert_eq!(port, 13329);
        assert_eq!(packets.len() as u32, SIGNATURE_LOCK_THRESHOLD + 1);
        assert_eq!(packets[0].data, vec![0x0B, 0x00, 0x90, 1, 2, 3], "the login's packet comes first");
        assert!(waiting.flows.is_empty());
    }

    /// Not every quiet moment moves the lock: a connection without the
    /// signature rate, or TLS, never takes it.
    #[test]
    fn other_connections_never_take_the_lock() {
        let mut waiting = WaitingFlows::default();
        for i in 0..50 {
            assert!(waiting.offer(flow(8080, 50001, &[1, 2, 3, i]), 1_000 + i as i64).is_none());
        }
        assert!(waiting.offer(flow(13329, 50002, &[0x17, 0x03, 0x03, 0x00, 0x10]), 2_000).is_none());
        // Hits too far apart don't add up.
        let sig = COMBAT_SIGNATURES[0];
        for i in 0..40 {
            let mut data = vec![0x10];
            data.extend_from_slice(sig);
            assert!(waiting.offer(flow(13330, 50003, &data), 1_000 + i * (SIGNATURE_WINDOW_MS + 1)).is_none());
        }
    }

    #[test]
    fn the_detector_follows_a_moved_port() {
        let d = CombatPortDetector::new();
        d.register_candidate(13328, (13328, 50000), Some("Npcap Loopback Adapter"));
        d.confirm_candidate(13328, 50000, Some("Npcap Loopback Adapter"));
        assert_eq!(d.locked_port_now(), Some(13328));
        assert!(d.port_quiet_ms(now_ms()).is_some_and(|q| q < 1_000));
        d.move_port(13329);
        assert_eq!((d.locked_port_now(), d.current_port()), (Some(13329), Some(13329)));
        assert_eq!(d.current_device().as_deref(), Some("Npcap Loopback Adapter"));
    }

    #[test]
    fn unlocked_report_names_each_device_and_the_window() {
        let mut stats = UnlockedStats::default();
        stats.note(&cap("NordLynx Tunnel", &[0x0E, 0x00, 0x36, 0x01]));
        stats.note(&cap("NordLynx Tunnel", &[0x01, 0x02]));
        stats.note(&cap("Realtek", &[0x01]));
        stats.no_window = 3;
        let line = stats.report(false);
        assert!(line.contains("AION2 window NOT found, 3 packets ignored for that"), "{line}");
        assert!(line.contains("NordLynx Tunnel: 2 packets, 1 with game markers"), "{line}");
        assert!(line.contains("Realtek: 1 packets, 0 with game markers"), "{line}");
        assert!(line.find("NordLynx").unwrap() < line.find("Realtek").unwrap(), "busiest first");
    }
}

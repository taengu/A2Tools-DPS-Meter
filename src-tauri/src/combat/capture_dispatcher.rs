use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::capture::captured_payload::{CapturedPayload, FlowKey};
use crate::capture::combat_port_detector::CombatPortDetector;
use crate::capture::stream_assembler::StreamAssembler;
use crate::capture::tcp_order::{Piece, TcpOrder};
use crate::capture::stream_processor::StreamProcessor;
use crate::combat::data_storage::DataStorage;
use crate::combat::ping_tracker::PingTracker;
use crate::i18n::lookup::{NpcLookup, SkillLookup};
use crate::platform::window_detector;

/// Pre-lock combat signatures: a cheap gate deciding which packets are worth
/// running through the parser before a port is locked. A sustained rate of
/// these signatures selects a flow; it does not authenticate the sender.
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
/// this span, measured from the oldest retained hit rather than the previous one.
const SIGNATURE_WINDOW_MS: i64 = 3_000;
const MAX_ACTIVE_FLOWS: usize = 16;
const FLOW_IDLE_MS: i64 = 30_000;
const FLOW_SWEEP_MS: i64 = 1_000;
const MAX_FILTER_FLOWS: usize = 128;
const TLS_CONTENT_TYPES: [u8; 4] = [0x14, 0x15, 0x16, 0x17];
const TLS_VERSIONS: [u8; 5] = [0x00, 0x01, 0x02, 0x03, 0x04];
const WINDOW_CHECK_STOPPED_MS: i64 = 10_000;
const WINDOW_CHECK_RUNNING_MS: i64 = 60_000;
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
const PORT_MOVE_FLOW_PACKETS: usize = 4096;

#[derive(Default)]
struct SignatureWindow {
    hits: VecDeque<i64>,
}

impl SignatureWindow {
    fn note(&mut self, now: i64) -> bool {
        // A clock correction starts a fresh window.
        if self.hits.back().is_some_and(|&last| now < last) {
            self.hits.clear();
        }
        while self.hits.front().is_some_and(|&first| now.saturating_sub(first) > SIGNATURE_WINDOW_MS) {
            self.hits.pop_front();
        }
        self.hits.push_back(now);
        while self.hits.len() > SIGNATURE_LOCK_THRESHOLD as usize {
            self.hits.pop_front();
        }
        self.ready()
    }

    fn ready(&self) -> bool {
        self.hits.len() >= SIGNATURE_LOCK_THRESHOLD as usize
    }
}

fn admit_flow(seen: &mut HashMap<FlowKey, i64>, key: &FlowKey, now: i64, limit: usize) -> bool {
    if !seen.contains_key(key) && seen.len() >= limit {
        return false;
    }
    seen.insert(key.clone(), now);
    true
}

/// A connection seen while the locked port was quiet.
#[derive(Default)]
struct WaitingFlow {
    packets: Vec<CapturedPayload>,
    bytes: usize,
    hits: SignatureWindow,
    last_seen: i64,
}

/// The connections seen while the locked port is quiet, until one sustains
/// the game's signature rate (see PORT_MOVE_QUIET_MS).
#[derive(Default)]
struct WaitingFlows {
    flows: HashMap<FlowKey, WaitingFlow>,
}

impl WaitingFlows {
    /// Keep `cap` (not on the locked port, which is quiet). When its
    /// connection proves to be the game: the port the game now sends from,
    /// and the connection's packets so far, in order, to read.
    fn offer(&mut self, cap: CapturedPayload, now: i64) -> Option<(u16, Vec<CapturedPayload>)> {
        if looks_like_tls(&cap.data) {
            return None;
        }
        self.flows.retain(|_, flow| now.saturating_sub(flow.last_seen) <= FLOW_IDLE_MS);
        let key = cap.direction_key();
        if !self.flows.contains_key(&key) && self.flows.len() >= PORT_MOVE_FLOWS {
            return None;
        }
        let flow = self.flows.entry(key.clone()).or_default();
        if cap.data.len() > PORT_MOVE_FLOW_BYTES.saturating_sub(flow.bytes)
            || flow.packets.len() >= PORT_MOVE_FLOW_PACKETS {
            return None;
        }
        // The signature only ever comes from the server's side.
        let signed = contains_any(&cap.data, &COMBAT_SIGNATURES);
        let ready = signed && flow.hits.note(now);
        let server_port = cap.src_port;
        flow.bytes += cap.data.len();
        flow.last_seen = now;
        flow.packets.push(cap);
        if !ready {
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
    pub async fn run(&self, receiver: mpsc::Receiver<CapturedPayload>) {
        self.run_with_window_check(receiver, window_detector::find_aion2_window_title).await;
    }

    async fn run_with_window_check(&self, mut receiver: mpsc::Receiver<CapturedPayload>, window_title: fn() -> Option<String>) {
        let mut assemblers: HashMap<FlowKey, (StreamAssembler, StreamProcessor, TcpOrder)> = HashMap::new();
        // Per-flow count of signature-bearing packets seen while still unlocked, used
        // for the no-combat-needed signature lock (see SIGNATURE_LOCK_THRESHOLD).
        let mut sig_hits: HashMap<FlowKey, SignatureWindow> = HashMap::new();
        let mut flow_seen: HashMap<FlowKey, i64> = HashMap::new();
        let mut last_flow_sweep_ms = 0;
        let mut locked_flow: Option<FlowKey> = None;
        let mut connections = ConnectionFilter::default();
        let mut last_window_check_ms: i64 = 0;
        let mut is_aion_running = false;
        let mut window_logged: Option<bool> = None;
        let mut unlocked_stats = UnlockedStats::default();
        let mut last_unlocked_report_ms = now_ms();
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
            if now.saturating_sub(last_flow_sweep_ms) >= FLOW_SWEEP_MS {
                flow_seen.retain(|_, last| now.saturating_sub(*last) <= FLOW_IDLE_MS);
                assemblers.retain(|key, _| flow_seen.contains_key(key));
                sig_hits.retain(|key, _| flow_seen.contains_key(key));
                last_flow_sweep_ms = now;
            }
            let interval = if is_aion_running { WINDOW_CHECK_RUNNING_MS } else { WINDOW_CHECK_STOPPED_MS };
            if now - last_window_check_ms >= interval {
                last_window_check_ms = now;
                let title = window_title();
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
                    flow_seen.clear();
                    connections.clear();
                    locked_flow = None;
                    waiting.clear();
                    replay.clear();
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
                    flow_seen.clear();
                    connections.clear();
                    locked_flow = None;
                    waiting.clear();
                    replay.clear();
                }
            }

            let current_port = self.port_detector.current_port();
            let locked_device = self.port_detector.current_device();
            let connection = cap.flow_key();

            // A manually selected capture device remains authoritative.
            if let Some(ref pref) = self.port_detector.preferred_device() {
                if !device_matches(pref, cap.device_name.as_deref()) {
                    continue;
                }
            }

            // Follow the complete connection, including its device and both
            // addresses. An unrelated host using the same ports must not feed
            // the locked assembler, ping tracker or last-activity clock.
            // Reconnects, server changes and VPN moves share the same bounded
            // handoff: the old flow must be quiet and one new flow must sustain
            // the signature rate before its retained packets are replayed.
            if let Some(port) = current_port {
                if locked_flow.as_ref().is_some_and(|flow| flow != &connection) {
                    let quiet = self.port_detector.port_quiet_ms(now).is_some_and(|q| q >= PORT_MOVE_QUIET_MS);
                    if !quiet {
                        continue;
                    }
                    if let Some((new_port, packets)) = waiting.offer(cap, now) {
                        self.port_detector.move_port(new_port);
                        if let Some(device) = packets.last().and_then(|p| p.device_name.as_deref()) {
                            if locked_device.as_deref().is_none_or(|old| !device_matches(old, Some(device))) {
                                self.port_detector.move_device(device);
                            }
                        }
                        locked_flow = packets.last().map(CapturedPayload::flow_key);
                        self.port_detector.mark_from_port();
                        self.ping_tracker.reset();
                        assemblers.clear();
                        sig_hits.clear();
                        flow_seen.clear();
                        connections.clear();
                        replay.extend(packets);
                    }
                    continue;
                }
                if cap.src_port != port && cap.dst_port != port {
                    continue;
                }
                if cap.src_port == port {
                    self.port_detector.mark_from_port();
                    waiting.clear();
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

            if !connections.admit_at(&cap, now) {
                continue;
            }

            // Get or create assembler
            let key = cap.direction_key();
            if !admit_flow(&mut flow_seen, &key, now, MAX_ACTIVE_FLOWS) {
                continue;
            }

            let (assembler, processor, order) = assemblers.entry(key.clone()).or_insert_with(|| {
                let mut proc = StreamProcessor::new(self.data_storage.clone(), self.skill_lookup.clone(), self.npc_lookup.clone());
                proc.set_dot_skill_ids(self.dot_skill_ids.clone());
                (StreamAssembler::new(), proc, TcpOrder::default())
            });

            if unlocked {
                self.port_detector.register_candidate(cap.src_port, (cap.src_port, cap.dst_port), cap.device_name.as_deref());
                // Count this signature-bearing packet against its source port (the
                // signature only ever travels server->client). The lock decision is
                // made below, after process_chunk, so we don't touch `assemblers`
                // while the assembler for this flow is still borrowed.
                sig_hits.entry(key.clone()).or_default().note(now);
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
            // without TCP metadata (a text replay or a test) is read as it
            // comes. Sequence zero on a live, addressed segment is valid.
            let pieces = if !cap.has_tcp_sequence() {
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
                unlocked && sig_hits.get(&key).is_some_and(SignatureWindow::ready);
            if signature_locked && self.port_detector.current_port().is_none() {
                self.port_detector.confirm_candidate(cap.src_port, cap.dst_port, cap.device_name.as_deref());
                // On lock, GC the orphaned candidate assemblers (the relay's
                // duplicate external flows) so only the locked flow is processed.
                if self.port_detector.current_port().is_some() {
                    locked_flow = Some(connection);
                    // A signature-selected stream that never parses anything
                    // must still become stale, rather than retain a zero clock.
                    self.port_detector.mark_packet_parsed();
                    assemblers.retain(|k, _| *k == key);
                    flow_seen.retain(|k, _| *k == key);
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
    flows: HashMap<FlowKey, (bool, i64)>,
}

impl ConnectionFilter {
    #[cfg(test)]
    fn admit(&mut self, cap: &CapturedPayload) -> bool {
        self.admit_at(cap, now_ms())
    }

    fn admit_at(&mut self, cap: &CapturedPayload, now: i64) -> bool {
        self.flows.retain(|_, (_, last)| now.saturating_sub(*last) <= FLOW_IDLE_MS);
        let key = cap.flow_key();
        if !self.flows.contains_key(&key) && self.flows.len() >= MAX_FILTER_FLOWS {
            return false;
        }
        let entry = self.flows.entry(key).or_insert_with(|| (is_tls_record(&cap.data), now));
        entry.1 = now;
        !entry.0
    }

    fn clear(&mut self) {
        self.flows.clear();
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

    #[tokio::test]
    async fn an_unrelated_host_with_matching_ports_cannot_replace_the_locked_host() {
        let _turn = crate::share::ring::TEST_SERIAL.lock();
        crate::share::ring::clear();
        let storage = Arc::new(DataStorage::new());
        let detector = Arc::new(CombatPortDetector::new());
        let dispatcher = CaptureDispatcher::new(
            storage.clone(), Arc::new(SkillLookup::new()), Arc::new(NpcLookup::new()),
            detector.clone(), Arc::new(PingTracker::new()),
        );
        let mut game = flow(13328, 50000, COMBAT_SIGNATURES[0]);
        game.device_name = Some("Npcap Loopback Adapter".into());
        game.src_ip = Some("192.0.2.1".parse().unwrap());
        game.dst_ip = Some("192.0.2.2".parse().unwrap());
        let (sender, receiver) = mpsc::channel(32);
        for _ in 0..SIGNATURE_LOCK_THRESHOLD + 1 {
            sender.send(game.clone()).await.unwrap();
        }
        let mut stranger = game.clone();
        stranger.src_ip = Some("192.0.2.3".parse().unwrap());
        sender.send(stranger).await.unwrap();
        let mut other_device = game.clone();
        other_device.device_name = Some("other device".into());
        sender.send(other_device).await.unwrap();
        drop(sender);
        dispatcher.run_with_window_check(receiver, || Some("AION2 test".into())).await;
        assert_eq!(detector.current_port(), Some(game.src_port));
        assert_eq!(storage.game_host(), game.src_ip);
        assert_eq!(detector.current_device(), game.device_name);
        crate::share::ring::clear();
    }

    #[tokio::test]
    async fn zero_sequence_file_replay_chunks_with_current_timestamps_are_all_read() {
        let _turn = crate::share::ring::TEST_SERIAL.lock();
        crate::share::ring::clear();
        let detector = Arc::new(CombatPortDetector::new());
        let device = "Npcap Loopback Adapter";
        detector.register_candidate(13328, (13328, 50000), Some(device));
        detector.confirm_candidate(13328, 50000, Some(device));
        let dispatcher = CaptureDispatcher::new(
            Arc::new(DataStorage::new()), Arc::new(SkillLookup::new()), Arc::new(NpcLookup::new()),
            detector, Arc::new(PingTracker::new()),
        );
        let (sender, receiver) = mpsc::channel(2);
        for bytes in [&[0x77][..], &[0x88][..]] {
            let mut replay = flow(13328, 50000, bytes);
            replay.device_name = Some(device.into());
            replay.captured_at_ms = 1_791_237_434_407;
            assert!(replay.capture_time_ms().is_some());
            assert!(!replay.has_tcp_sequence());
            sender.send(replay).await.unwrap();
        }
        drop(sender);
        dispatcher.run_with_window_check(receiver, || Some("AION2 test".into())).await;
        let packets = crate::share::ring::snapshot();
        assert_eq!(packets.iter().map(|p| p.bytes.clone()).collect::<Vec<_>>(), vec![vec![0x77], vec![0x88]]);
        crate::share::ring::clear();
        let mut live = flow(13328, 50000, &[1]);
        live.src_ip = Some("192.0.2.1".parse().unwrap());
        live.dst_ip = Some("192.0.2.2".parse().unwrap());
        assert!(live.has_tcp_sequence(), "live sequence zero must be ordered");
    }

    #[test]
    fn endpoints_devices_and_directions_do_not_share_parser_state() {
        let mut first = flow(13328, 50000, &[1]);
        first.src_ip = Some("192.0.2.1".parse().unwrap());
        first.dst_ip = Some("192.0.2.2".parse().unwrap());
        let mut other = first.clone();
        other.src_ip = Some("192.0.2.3".parse().unwrap());
        assert_ne!(first.flow_key(), other.flow_key());
        assert_ne!(first.direction_key(), other.direction_key());
        assert_ne!(first.stream_key(), other.stream_key());
        other = first.clone();
        other.device_name = Some("vpn".into());
        assert_ne!(first.flow_key(), other.flow_key());
        assert_ne!(first.stream_key(), other.stream_key());
        let reverse = CapturedPayload {
            src_ip: first.dst_ip, dst_ip: first.src_ip,
            src_port: first.dst_port, dst_port: first.src_port,
            ..first.clone()
        };
        assert_eq!(first.flow_key(), reverse.flow_key());
        assert_ne!(first.direction_key(), reverse.direction_key());
        assert_eq!(first.stream_key(), first.clone().stream_key());
        assert_eq!(first.stream_key().rsplit(':').next(), Some("13328"));
        assert!(!first.stream_key().contains("192.0.2."));
    }

    #[test]
    fn a_slow_drip_does_not_accumulate_a_signature_lock() {
        let mut hits = SignatureWindow::default();
        let mut waiting = WaitingFlows::default();
        for i in 0..100 {
            let now = 1000 + i * (SIGNATURE_WINDOW_MS - 1);
            assert!(!hits.note(now));
            assert!(waiting.offer(flow(13328, 50000, COMBAT_SIGNATURES[0]), now).is_none());
        }
        let mut hits = SignatureWindow::default();
        for i in 0..SIGNATURE_LOCK_THRESHOLD {
            assert_eq!(hits.note(1000 + i as i64 * 100), i + 1 == SIGNATURE_LOCK_THRESHOLD);
        }
        assert!(!hits.note(500), "a backwards clock starts a new window");
    }

    #[test]
    fn candidates_with_matching_ports_cannot_combine_signature_hits() {
        let mut waiting = WaitingFlows::default();
        let mut a = flow(13328, 50000, COMBAT_SIGNATURES[0]);
        a.src_ip = Some("192.0.2.1".parse().unwrap());
        let mut b = a.clone();
        b.src_ip = Some("192.0.2.2".parse().unwrap());
        for i in 0..6 {
            assert!(waiting.offer(a.clone(), 1000 + i).is_none());
            assert!(waiting.offer(b.clone(), 1000 + i).is_none());
        }
        for i in 6..11 {
            assert!(waiting.offer(a.clone(), 1000 + i).is_none());
        }
        let (_, packets) = waiting.offer(a.clone(), 1011).expect("one endpoint sustained the rate");
        assert_eq!(packets.len(), SIGNATURE_LOCK_THRESHOLD as usize);
        assert!(packets.iter().all(|p| p.src_ip == a.src_ip));
    }

    #[test]
    fn flow_tracking_is_bounded_and_idle_tls_classifications_expire() {
        let mut seen = HashMap::new();
        let mut filter = ConnectionFilter::default();
        for port in 1..=1000 {
            let c = flow(13328, port, &[1]);
            let admitted = admit_flow(&mut seen, &c.direction_key(), 1000, MAX_ACTIVE_FLOWS);
            assert_eq!(admitted, port as usize <= MAX_ACTIVE_FLOWS);
            let filtered = filter.admit_at(&c, 1000);
            assert_eq!(filtered, port as usize <= MAX_FILTER_FLOWS);
        }
        assert_eq!(seen.len(), MAX_ACTIVE_FLOWS);
        assert_eq!(filter.flows.len(), MAX_FILTER_FLOWS);
        let tls = flow(13328, 2000, &[0x17, 0x03, 0x03, 0x00, 0x10]);
        assert!(!filter.admit_at(&tls, 1000 + FLOW_IDLE_MS + 1));
        assert_eq!(filter.flows.len(), 1);
        let game = flow(13328, 2000, COMBAT_SIGNATURES[0]);
        assert!(!filter.admit_at(&game, 1000 + FLOW_IDLE_MS + 2));
        assert!(filter.admit_at(&game, 1000 + 2 * FLOW_IDLE_MS + 3));
    }

    #[test]
    fn handoff_buffers_expire_and_bound_packet_count_as_well_as_bytes() {
        let mut waiting = WaitingFlows::default();
        for port in 1..=PORT_MOVE_FLOWS as u16 {
            waiting.offer(flow(13328, port, &[1]), 1000);
        }
        waiting.offer(flow(13328, 1000, &[1]), 1001);
        assert_eq!(waiting.flows.len(), PORT_MOVE_FLOWS);
        waiting.offer(flow(13328, 1000, &[1]), 1000 + FLOW_IDLE_MS + 1);
        assert_eq!(waiting.flows.len(), 1);
        for _ in 0..PORT_MOVE_FLOW_PACKETS + 10 {
            waiting.offer(flow(13328, 1000, &[1]), 1000 + FLOW_IDLE_MS + 2);
        }
        let state = waiting.flows.values().next().unwrap();
        assert_eq!(state.packets.len(), PORT_MOVE_FLOW_PACKETS);
        assert_eq!(state.bytes, PORT_MOVE_FLOW_PACKETS);
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

use std::collections::VecDeque;

use parking_lot::Mutex;

use crate::capture::captured_payload::CapturedPayload;
use crate::capture::stream_processor::read_varint;

/// .NET epoch offset: milliseconds between 0001-01-01 and 1970-01-01.
const DOTNET_EPOCH_OFFSET_MS: i64 = 62135596800000;
const MAX_PING_MS: i32 = 9999;
const MIN_PING_RS_BYTES: usize = 12;
const MAX_HISTORY: usize = 10_000;
const RECENT_RESPONSES: usize = 16;
/// The readings jitter is worked out over.
const JITTER_SAMPLES: usize = 10;
/// Physical size of the client's ping request frame. Its body is encrypted, but
/// nothing else the client sends is this size: 12 bytes while the timestamp was
/// wall-clock, 13 since it became an arbitrary-epoch clock. Once every ten
/// seconds either way.
const PING_RQ_FRAME_BYTES: std::ops::RangeInclusive<usize> = 12..=13;
/// The same frames as a relay tunnel wraps them: `05 23 <len>` then the frame.
const RELAYED_PING_RQ: [[u8; 4]; 2] = [[0x05, 0x23, 0x0C, 0x0F], [0x05, 0x23, 0x0D, 0x10]];
/// How far a request's clock offset may move between pings and still be the
/// same clock.
const OFFSET_TOLERANCE_MS: i64 = 15;
/// The newer client stamps its ping with Windows' performance counter in
/// milliseconds since boot, plus Unreal's fixed 16,777,216 s offset (worked out
/// by Karim; checked 2026-10-01 against a captured request: exact to the ms).
const UNREAL_CLOCK_OFFSET_MS: i64 = 16_777_216 * 1000;

/// How far the counter's reading may differ from request timing before it is
/// counted as a disagreement. Both are exact to a few ms when the counter is the
/// game's own.
const PERF_AGREEMENT_MS: i64 = 25;
/// Disagreements in a row before the counter stops being trusted.
const PERF_MISSES_TO_DISTRUST: u8 = 2;

/// Reads the performance counter (ms since boot) and the wall clock (Unix ms)
/// together. The OS supplies it (`platform::clock::perf_clock`); this module
/// only uses it, so it stays OS-neutral and builds for wasm32.
pub type PerfClock = fn() -> (i64, i64);

pub struct PingTracker {
    inner: Mutex<Inner>,
    /// The machine's performance counter, so the newer client's timestamp can
    /// be read directly. `None` where the OS has none to offer, and for replays
    /// of old captures, whose timestamps belong to the counter as it was then.
    perf_clock: Option<PerfClock>,
}

struct Inner {
    last_ping: Option<i32>,
    history: Vec<(i64, i32)>,
    /// Accepted echoed timestamps, so duplicates cannot retrain either clock.
    responses: VecDeque<i64>,
    /// Capture times of recent ping requests, oldest first.
    requests: VecDeque<i64>,
    /// Local capture clock minus the client's ping clock, once two pings agree
    /// on it.
    clock_offset: Option<i64>,
    /// Offsets the previous response's candidate requests implied.
    prev_offsets: Vec<i64>,
    /// Consecutive pings where the counter disagreed with request timing.
    perf_misses: u8,
    /// Set once the counter has disagreed too often: the OS's clock is not the
    /// one the game stamps with (possible under Wine), so stop using it for the
    /// rest of the session. Never trips where the clock is right.
    perf_distrusted: bool,
}

impl PingTracker {
    /// Without a performance counter: request pairing and the fallbacks only.
    /// What replays want, since a capture's timestamps are on the counter as it
    /// ran then and reading today's would give nonsense.
    pub fn new() -> Self {
        Self::with_perf_clock(None)
    }

    /// With the machine's performance counter, as the live meter runs.
    pub fn with_perf_clock(perf_clock: Option<PerfClock>) -> Self {
        Self {
            inner: Mutex::new(Inner {
                last_ping: None,
                history: Vec::new(),
                responses: VecDeque::new(),
                requests: VecDeque::new(),
                clock_offset: None,
                prev_offsets: Vec::new(),
                perf_misses: 0,
                perf_distrusted: false,
            }),
            perf_clock,
        }
    }

    /// Round trip for the newer client, from the performance counter alone:
    /// the echoed timestamp is the counter at the moment the request left, so
    /// shift it onto the wall clock the capture timestamps use. Both clocks are
    /// read together now, so a Windows time sync is followed rather than
    /// learned. `None` for a timestamp that is not on this clock (KR/TW's).
    fn rtt_from_perf_clock(&self, client_sent: i64, arrival_ms: i64) -> Option<i64> {
        let (counter_ms, wall_ms) = (self.perf_clock?)();
        let sent_counter_ms = client_sent.checked_sub(UNREAL_CLOCK_OFFSET_MS)?;
        if !(0..=counter_ms).contains(&sent_counter_ms) {
            return None;
        }
        let rtt = arrival_ms - (sent_counter_ms + (wall_ms - counter_ms));
        is_valid_rtt(rtt).then_some(rtt)
    }

    /// `server_port` is the locked combat port, which tells the two directions
    /// apart.
    pub fn on_packet(&self, cap: &CapturedPayload, server_port: u16) {
        if cap.dst_port == server_port && cap.src_port != server_port {
            if has_ping_rq(&cap.data) {
                let mut inner = self.inner.lock();
                inner.requests.push_back(cap.captured_at_ms);
                while inner
                    .requests
                    .front()
                    .is_some_and(|t| cap.captured_at_ms - t > MAX_PING_MS as i64)
                {
                    inner.requests.pop_front();
                }
            }
            return;
        }
        if cap.data.len() >= MIN_PING_RS_BYTES {
            self.try_ping_rs(&cap.data, cap.captured_at_ms);
        }
    }

    fn try_ping_rs(&self, data: &[u8], arrival_ms: i64) {
        let mut i = 0;
        while i + MIN_PING_RS_BYTES <= data.len() {
            if data[i] == 0x03
                && data[i + 1] == 0x36
                && data[i + 2] == 0x00
                && data[i + 3] == 0x00
            {
                let client_sent_raw = read_i64_le(data, i + 4);
                let mut inner = self.inner.lock();
                if inner.responses.contains(&client_sent_raw) {
                    i += 12;
                    continue;
                }

                // The newer client's timestamp is this machine's performance
                // counter, which the meter can read too: exact, and no request
                // needed. Otherwise timing the response against the request
                // that caused it needs no clock at all, so it comes next. Then
                // KR/TW's wall-clock .NET milliseconds, which the local clock
                // can be subtracted from directly; then, off Windows, the
                // offset learned from earlier requests.
                //
                // Neither echoed clock follows the wall clock live, so a Windows
                // time sync mid-session moves our clock and not the game's.
                // That is why a seen request overrides the wall-clock reading
                // rather than the reverse.
                let wall_clock_rtt = arrival_ms
                    .wrapping_sub(client_sent_raw.wrapping_sub(DOTNET_EPOCH_OFFSET_MS));
                let perf_rtt = if inner.perf_distrusted {
                    None
                } else {
                    self.rtt_from_perf_clock(client_sent_raw, arrival_ms)
                };
                let paired_rtt = inner.rtt_from_request(client_sent_raw, arrival_ms, perf_rtt);
                // Where both exist they should agree to a few ms. The counter
                // is only assumed to be the game's clock (not verified on every
                // OS), so a disagreement means trusting the request instead,
                // and repeated ones mean dropping the counter altogether.
                let mut perf_rtt = perf_rtt;
                if let (Some(perf), Some(paired)) = (perf_rtt, paired_rtt) {
                    if (perf - paired).abs() > PERF_AGREEMENT_MS {
                        perf_rtt = None;
                        inner.perf_misses += 1;
                        if inner.perf_misses >= PERF_MISSES_TO_DISTRUST && !inner.perf_distrusted {
                            inner.perf_distrusted = true;
                            tracing::warn!(
                                "Ping: the performance counter disagrees with request timing ({perf} ms vs {paired} ms); timing requests instead"
                            );
                        }
                    } else {
                        inner.perf_misses = 0;
                    }
                }
                let rtt_ms = perf_rtt
                    .or(paired_rtt)
                    .or_else(|| is_valid_rtt(wall_clock_rtt).then_some(wall_clock_rtt))
                    .or_else(|| inner.rtt_from_known_offset(client_sent_raw, arrival_ms));

                if let Some(rtt_ms) = rtt_ms {
                    let rtt_ms = rtt_ms as i32;
                    inner.responses.push_back(client_sent_raw);
                    if inner.responses.len() > RECENT_RESPONSES {
                        inner.responses.pop_front();
                    }
                    inner.last_ping = Some(rtt_ms);
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as i64;
                    inner.history.push((now, rtt_ms));
                    if inner.history.len() > MAX_HISTORY {
                        inner.history.remove(0);
                    }
                }
                i += 12;
            } else {
                i += 1;
            }
        }
    }

    pub fn current_ping_ms(&self) -> Option<i32> {
        self.inner.lock().last_ping
    }

    /// How much the ping moves from one reading to the next: the mean
    /// difference between the last JITTER_SAMPLES readings, in ms (#164,
    /// after talyaseen's). None until there are three.
    pub fn current_jitter_ms(&self) -> Option<f64> {
        let inner = self.inner.lock();
        let recent: Vec<i32> = inner.history.iter().rev().take(JITTER_SAMPLES).map(|&(_, ms)| ms).collect();
        if recent.len() < 3 {
            return None;
        }
        let total: i64 = recent.windows(2).map(|w| (i64::from(w[0]) - i64::from(w[1])).abs()).sum();
        Some(total as f64 / (recent.len() - 1) as f64)
    }

    pub fn get_ping_history(&self, start_ms: i64, end_ms: i64) -> Vec<(i64, i32)> {
        self.inner.lock().history.iter()
            .filter(|(ts, _)| *ts >= start_ms && *ts <= end_ms)
            .cloned()
            .collect()
    }

    pub fn reset(&self) {
        let mut inner = self.inner.lock();
        inner.last_ping = None;
        inner.history.clear();
        inner.responses.clear();
        inner.requests.clear();
        inner.clock_offset = None;
        inner.prev_offsets.clear();
        inner.perf_misses = 0;
        inner.perf_distrusted = false;
    }
}

impl Inner {
    /// Round trip for a response, timed against the request that caused it.
    ///
    /// Each recent request implies an offset between the echoed clock and ours.
    /// One request in the window is the answer outright; with several, the one
    /// agreeing with the known offset (or with the previous ping) is. The real
    /// offset repeats from ping to ping, so it is stored once two consecutive
    /// pings agree on it, for responses whose request goes unseen.
    fn rtt_from_request(&mut self, client_sent: i64, arrival_ms: i64, perf_rtt: Option<i64>) -> Option<i64> {
        let offsets: Vec<i64> = self
            .requests
            .iter()
            .filter(|&&t| (0..=MAX_PING_MS as i64).contains(&(arrival_ms - t)))
            .map(|&t| t.wrapping_sub(client_sent))
            .collect();
        let near = |o: i64, others: &[i64]| {
            others
                .iter()
                .copied()
                .find(|x| x.wrapping_sub(o).wrapping_abs() <= OFFSET_TOLERANCE_MS)
        };

        let picked = match offsets.len() {
            0 => None,
            1 => Some(offsets[0]),
            _ => self
                .clock_offset
                .and_then(|known| near(known, &offsets))
                .or_else(|| offsets.iter().copied().find(|&o| near(o, &self.prev_offsets).is_some())),
        };

        if let Some(offset) = picked {
            // A lone captured request may be an old retransmission. Reject it
            // before changing either clock when the learned offset and live
            // counter independently agree, but the candidate contradicts both.
            // After a time sync or on Wine those clocks disagree, so request
            // timing remains authoritative as before.
            let paired = arrival_ms.wrapping_sub(client_sent.wrapping_add(offset));
            if let (Some(perf), Some(known)) = (perf_rtt, self.rtt_from_known_offset(client_sent, arrival_ms)) {
                if (perf - known).abs() <= PERF_AGREEMENT_MS
                    && (paired - perf).abs() > PERF_AGREEMENT_MS
                    && (paired - known).abs() > PERF_AGREEMENT_MS
                {
                    return None;
                }
            }
            let agrees = self
                .clock_offset
                .is_some_and(|known| offset.wrapping_sub(known).wrapping_abs() <= OFFSET_TOLERANCE_MS);
            if agrees {
                // Follow it, so slow drift between the clocks cannot build up.
                self.clock_offset = Some(offset);
            } else if near(offset, &self.prev_offsets).is_some() {
                tracing::info!("Ping clock offset learned: {}ms", offset);
                self.clock_offset = Some(offset);
            } else if self.clock_offset.take().is_some() {
                // One of the clocks jumped (a Windows time sync does this). The
                // stored offset would now be off by the jump, so drop it until
                // two pings agree on the new one.
                tracing::info!("Ping clock offset no longer matches its requests; relearning");
            }
        }
        self.prev_offsets = offsets;
        picked
            .map(|o| arrival_ms.wrapping_sub(client_sent.wrapping_add(o)))
            .filter(|&r| is_valid_rtt(r))
    }

    /// Round trip from the echoed timestamp alone, once the offset is known.
    fn rtt_from_known_offset(&self, client_sent: i64, arrival_ms: i64) -> Option<i64> {
        self.clock_offset
            .map(|o| arrival_ms.wrapping_sub(client_sent.wrapping_add(o)))
            .filter(|&r| is_valid_rtt(r))
    }
}

fn is_valid_rtt(rtt_ms: i64) -> bool {
    (1..=MAX_PING_MS as i64).contains(&rtt_ms)
}

/// Whether an outbound payload carries a ping request.
///
/// Stricter than the parser's frame walk on purpose: that walk resyncs a byte
/// at a time through anything it cannot read, which is right for the server's
/// stream, but the client's is encrypted, and resyncing through a large
/// encrypted payload turns up made-up 12- and 13-byte "frames". So a payload
/// only counts if it frames cleanly from its first byte to its last.
fn has_ping_rq(data: &[u8]) -> bool {
    if data.windows(4).any(|w| RELAYED_PING_RQ.iter().any(|rq| w == rq)) {
        return true;
    }
    let mut offset = 0;
    let mut found = false;
    while offset < data.len() {
        let len = read_varint(data, offset);
        let size = len.value as i64 - 3;
        if len.length <= 0 || size <= 0 || offset + size as usize > data.len() {
            return false;
        }
        found |= PING_RQ_FRAME_BYTES.contains(&(size as usize));
        offset += size as usize;
    }
    found
}

fn read_i64_le(data: &[u8], offset: usize) -> i64 {
    let mut v: i64 = 0;
    for j in 0..8 {
        v |= (data[offset + j] as i64) << (j * 8);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_is_the_mean_change_between_the_last_readings() {
        let t = PingTracker::new();
        assert_eq!(t.current_jitter_ms(), None);
        t.inner.lock().history.extend([(1, 40), (2, 44)]);
        assert_eq!(t.current_jitter_ms(), None, "not from two");
        t.inner.lock().history.extend([(3, 40), (4, 50)]);
        // 4, 4, 10 over three steps.
        assert_eq!(t.current_jitter_ms(), Some(6.0));
        // Only the last ten count.
        t.inner.lock().history.extend((5..20).map(|i| (i, 30)));
        assert_eq!(t.current_jitter_ms(), Some(0.0));
    }

    const SERVER: u16 = 13328;
    const CLIENT: u16 = 50000;

    fn cap(src_port: u16, dst_port: u16, at: i64, data: Vec<u8>) -> CapturedPayload {
        CapturedPayload {
            src_port,
            dst_port,
            data,
            device_name: None,
            captured_at_ms: at,
            src_ip: None,
            dst_ip: None,
            tcp_seq: 0,
            tcp_ack: 0,
        }
    }

    fn request(at: i64) -> CapturedPayload {
        let mut data = vec![0x10];
        data.extend_from_slice(&[0xAB; 12]);
        cap(CLIENT, SERVER, at, data)
    }

    fn response(at: i64, client_sent: i64) -> CapturedPayload {
        let mut data = vec![0x18, 0x03, 0x36, 0x00, 0x00];
        data.extend_from_slice(&client_sent.to_le_bytes());
        data.extend_from_slice(&at.to_le_bytes());
        cap(SERVER, CLIENT, at, data)
    }

    #[test]
    fn wall_clock_timestamp_needs_no_request() {
        let t = PingTracker::new();
        let now = 1_790_000_000_000;
        t.on_packet(&response(now + 80, now + DOTNET_EPOCH_OFFSET_MS), SERVER);
        assert_eq!(t.current_ping_ms(), Some(80));
    }

    #[test]
    fn arbitrary_epoch_is_timed_against_the_request() {
        let t = PingTracker::new();
        let now = 1_790_000_000_000;
        // As captured 2026-10-01: a client clock ~203 days old, pings 10s apart.
        let clock = 17_552_452_660;
        t.on_packet(&request(now), SERVER);
        t.on_packet(&response(now + 64, clock), SERVER);
        assert_eq!(t.current_ping_ms(), Some(64));

        t.on_packet(&request(now + 10_000), SERVER);
        t.on_packet(&response(now + 10_090, clock + 10_000), SERVER);
        assert_eq!(t.current_ping_ms(), Some(90));

        // With the offset learned, a response whose request was never seen
        // still resolves.
        t.on_packet(&response(now + 20_045, clock + 20_000), SERVER);
        assert_eq!(t.current_ping_ms(), Some(45));
    }

    /// As measured 2026-10-01: a Windows time sync moved the PC clock +2.32s
    /// mid-session and the game's ping clock stayed where it was.
    #[test]
    fn a_pc_clock_jump_does_not_skew_ping() {
        let t = PingTracker::new();
        let now = 1_790_000_000_000;
        let clock = 17_552_452_660;
        for n in 0..2 {
            t.on_packet(&request(now + n * 10_000), SERVER);
            t.on_packet(&response(now + n * 10_000 + 60, clock + n * 10_000), SERVER);
        }
        assert_eq!(t.current_ping_ms(), Some(60));

        let jump = 2_320;
        t.on_packet(&request(now + 20_000 + jump), SERVER);
        t.on_packet(&response(now + 20_070 + jump, clock + 20_000), SERVER);
        assert_eq!(t.current_ping_ms(), Some(70), "timed against the request, not the stale offset");

        // The stale offset is gone, so an unpaired response reports nothing
        // rather than 2.4 seconds.
        t.on_packet(&response(now + 30_055 + jump, clock + 30_000), SERVER);
        assert_eq!(t.current_ping_ms(), Some(70));

        // Two paired pings on the new clock relearn it.
        for n in 4..6 {
            t.on_packet(&request(now + n * 10_000 + jump), SERVER);
            t.on_packet(&response(now + n * 10_000 + 80 + jump, clock + n * 10_000), SERVER);
        }
        t.on_packet(&response(now + 60_045 + jump, clock + 60_000), SERVER);
        assert_eq!(t.current_ping_ms(), Some(45));
    }

    /// KR/TW: the wall-clock timestamp is off by the jump too, so a seen
    /// request has to win over it.
    #[test]
    fn wall_clock_timestamp_defers_to_a_seen_request() {
        let t = PingTracker::new();
        let now = 1_790_000_000_000;
        let jump = 2_320;
        // The game stamped its old clock; ours has since moved forward.
        let sent = now + DOTNET_EPOCH_OFFSET_MS;
        t.on_packet(&request(now + jump), SERVER);
        t.on_packet(&response(now + jump + 65, sent), SERVER);
        assert_eq!(t.current_ping_ms(), Some(65));
    }

    #[test]
    fn arbitrary_epoch_without_a_request_reports_nothing() {
        let t = PingTracker::new();
        t.on_packet(&response(1_790_000_000_064, 17_552_452_660), SERVER);
        assert_eq!(t.current_ping_ms(), None);
    }

    #[test]
    fn requests_are_only_found_in_cleanly_framed_payloads() {
        // A lone request, and one coalesced with the 11-byte heartbeat.
        let rq = [vec![0x10], vec![0xAB; 12]].concat();
        let beat = [vec![0x0E], vec![0xCD; 10]].concat();
        assert!(has_ping_rq(&rq));
        assert!(has_ping_rq(&[beat.clone(), rq.clone()].concat()));
        assert!(!has_ping_rq(&beat));
        // Encrypted bulk data whose resync would stumble on a 0x10 byte: the
        // first "frame" does not fit, so the payload is not a request.
        let mut bulk = vec![0x7F; 4096];
        bulk[0] = 0xFF;
        bulk[100] = 0x10;
        assert!(!has_ping_rq(&bulk));
    }

    #[test]
    fn relay_wrapped_request_is_recognised() {
        let mut data = vec![0x01, 0x33, 0x00, 0x00, 0x05, 0x22, 0x02, 0x02, 0x2B];
        data.extend_from_slice(&RELAYED_PING_RQ[0]);
        data.extend_from_slice(&[0xCD; 11]);
        data.extend_from_slice(&[0x05, 0x25, 0x01, 0x01]);
        assert!(has_ping_rq(&data));
    }

    /// A machine 1,000 s after boot whose wall clock reads 1_790_000_000_000.
    fn fake_perf_clock() -> (i64, i64) {
        (1_000_000, 1_790_000_000_000)
    }

    #[test]
    fn a_stale_request_cannot_override_two_agreeing_clocks() {
        let t = PingTracker::with_perf_clock(Some(fake_perf_clock));
        let base = 1_789_999_970_000;
        for n in 0..2 {
            t.on_packet(&request(base + n * 10_000), SERVER);
            t.on_packet(&response(base + n * 10_000 + 60,
                970_000 + n * 10_000 + UNREAL_CLOCK_OFFSET_MS), SERVER);
        }
        let known = t.inner.lock().clock_offset;
        assert!(known.is_some());
        // An old request is retransmitted at 15s; the actual 20s request was
        // not captured. The sole candidate would imply 5060ms, but both the
        // learned offset and the native counter independently imply 60ms.
        t.on_packet(&request(base + 15_000), SERVER);
        t.on_packet(&response(base + 20_060, 990_000 + UNREAL_CLOCK_OFFSET_MS), SERVER);
        assert_eq!(t.current_ping_ms(), Some(60));
        assert_eq!(t.inner.lock().clock_offset, known);
        assert_eq!(t.inner.lock().perf_misses, 0);
        assert!(!t.inner.lock().perf_distrusted);
        t.on_packet(&response(base + 30_070, 1_000_000 + UNREAL_CLOCK_OFFSET_MS), SERVER);
        assert_eq!(t.current_ping_ms(), Some(70));
    }

    #[test]
    fn a_repeated_response_cannot_learn_an_offset_or_change_history() {
        let t = PingTracker::new();
        let now = 1_790_000_000_000;
        let clock = 17_552_452_660;
        t.on_packet(&request(now), SERVER);
        t.on_packet(&response(now + 60, clock), SERVER);
        t.on_packet(&response(now + 80, clock), SERVER);
        assert_eq!(t.current_ping_ms(), Some(60));
        assert_eq!(t.inner.lock().history.len(), 1);
        assert_eq!(t.inner.lock().clock_offset, None, "one distinct response cannot establish a clock");
        t.on_packet(&request(now + 10_000), SERVER);
        t.on_packet(&response(now + 10_070, clock + 10_000), SERVER);
        let known = t.inner.lock().clock_offset;
        assert!(known.is_some());
        // A delayed duplicate of an earlier response is ignored too.
        t.on_packet(&response(now + 10_080, clock), SERVER);
        assert_eq!(t.current_ping_ms(), Some(70));
        assert_eq!(t.inner.lock().history.len(), 2);
        assert_eq!(t.inner.lock().clock_offset, known);
    }

    #[test]
    fn counter_distrust_requires_distinct_responses_and_reset_retries_it() {
        let t = PingTracker::with_perf_clock(Some(fake_perf_clock));
        let base = 1_789_999_980_000;
        t.on_packet(&request(base), SERVER);
        t.on_packet(&response(base + 70, 979_500 + UNREAL_CLOCK_OFFSET_MS), SERVER);
        assert_eq!(t.inner.lock().perf_misses, 1);
        t.on_packet(&response(base + 90, 979_500 + UNREAL_CLOCK_OFFSET_MS), SERVER);
        assert_eq!(t.inner.lock().perf_misses, 1);
        assert!(!t.inner.lock().perf_distrusted);
        assert_eq!(t.current_ping_ms(), Some(70));
        t.on_packet(&request(base + 10_000), SERVER);
        t.on_packet(&response(base + 10_070, 989_500 + UNREAL_CLOCK_OFFSET_MS), SERVER);
        assert!(t.inner.lock().perf_distrusted, "a genuinely different Wine clock still falls back");
        t.reset();
        assert_eq!(t.inner.lock().perf_misses, 0);
        assert!(!t.inner.lock().perf_distrusted);
        // A new session can use the native counter even if capture missed its
        // request. Reusing an earlier echo also verifies reset clears dedup.
        t.on_packet(&response(base + 9_560, 989_500 + UNREAL_CLOCK_OFFSET_MS), SERVER);
        assert_eq!(t.current_ping_ms(), Some(60));
        assert_eq!(t.inner.lock().history.len(), 1);
    }

    #[test]
    fn a_counter_clock_sync_still_accepts_the_current_request() {
        use std::sync::atomic::{AtomicI64, Ordering};
        static JUMP: AtomicI64 = AtomicI64::new(0);
        fn clock() -> (i64, i64) {
            (1_000_000, 1_790_000_000_000 + JUMP.load(Ordering::Relaxed))
        }
        let t = PingTracker::with_perf_clock(Some(clock));
        let base = 1_789_999_970_000;
        for n in 0..2 {
            t.on_packet(&request(base + n * 10_000), SERVER);
            t.on_packet(&response(base + n * 10_000 + 60,
                970_000 + n * 10_000 + UNREAL_CLOCK_OFFSET_MS), SERVER);
        }
        let jump = 2_320;
        JUMP.store(jump, Ordering::Relaxed);
        t.on_packet(&request(base + 20_000 + jump), SERVER);
        t.on_packet(&response(base + 20_070 + jump, 990_000 + UNREAL_CLOCK_OFFSET_MS), SERVER);
        assert_eq!(t.current_ping_ms(), Some(70));
        assert_eq!(t.inner.lock().clock_offset, None, "the old offset is discarded after time sync");
        assert_eq!(t.inner.lock().perf_misses, 0);
        t.on_packet(&request(base + 30_000 + jump), SERVER);
        t.on_packet(&response(base + 30_065 + jump, 1_000_000 + UNREAL_CLOCK_OFFSET_MS), SERVER);
        assert_eq!(t.current_ping_ms(), Some(65));
        assert!(t.inner.lock().clock_offset.is_some());
    }

    #[test]
    fn genuine_high_latency_is_not_clipped() {
        let t = PingTracker::with_perf_clock(Some(fake_perf_clock));
        let base = 1_789_999_970_000;
        for n in 0..3 {
            let rtt = if n == 2 { 5_060 } else { 60 };
            t.on_packet(&request(base + n * 10_000), SERVER);
            t.on_packet(&response(base + n * 10_000 + rtt,
                970_000 + n * 10_000 + UNREAL_CLOCK_OFFSET_MS), SERVER);
            assert_eq!(t.current_ping_ms(), Some(rtt as i32));
        }
        assert_eq!(t.inner.lock().perf_misses, 0);
    }

    #[test]
    fn duplicate_tracking_is_bounded_and_remembers_only_accepted_responses() {
        let t = PingTracker::new();
        let now = 1_790_000_000_000;
        let first = now + DOTNET_EPOCH_OFFSET_MS;
        t.on_packet(&response(now, first), SERVER); // invalid zero RTT
        assert!(t.inner.lock().responses.is_empty());
        t.on_packet(&response(now + 60, first), SERVER);
        assert_eq!(t.current_ping_ms(), Some(60));
        for n in 1..=RECENT_RESPONSES as i64 {
            t.on_packet(&response(now + n * 10_000 + 60, first + n * 10_000), SERVER);
        }
        let inner = t.inner.lock();
        assert_eq!(inner.responses.len(), RECENT_RESPONSES);
        assert_eq!(inner.responses.front(), Some(&(first + 10_000)));
        assert_eq!(inner.history.len(), RECENT_RESPONSES + 1);
    }

    #[test]
    fn perf_counter_timestamp_needs_no_request() {
        let t = PingTracker::with_perf_clock(Some(fake_perf_clock));
        // Sent 10 s ago by the counter, which is 1_789_999_990_000 on the wall
        // clock; answered 64 ms later.
        let sent = 990_000 + UNREAL_CLOCK_OFFSET_MS;
        t.on_packet(&response(1_789_999_990_064, sent), SERVER);
        assert_eq!(t.current_ping_ms(), Some(64));
    }

    #[test]
    fn perf_counter_leaves_kr_tw_timestamps_alone() {
        let t = PingTracker::with_perf_clock(Some(fake_perf_clock));
        let now = 1_790_000_000_000;
        t.on_packet(&response(now + 80, now + DOTNET_EPOCH_OFFSET_MS), SERVER);
        assert_eq!(t.current_ping_ms(), Some(80), "read as wall-clock .NET ms, as before");
    }

    /// As captured 2026-10-01 04:01, after a Windows time sync moved the wall
    /// clock 2.32 s: the counter did not move, so reading it live is still
    /// exact. Wall minus counter is the boot time on the synced clock.
    #[test]
    fn perf_counter_matches_the_captured_request() {
        fn clock() -> (i64, i64) {
            let boot = 1_790_798_496_918 - 787_574_334;
            (800_000_000, boot + 800_000_000)
        }
        let t = PingTracker::with_perf_clock(Some(clock));
        t.on_packet(&response(1_790_798_497_088, 17_564_790_334), SERVER);
        assert_eq!(t.current_ping_ms(), Some(170), "170 ms, as timed against the request");
    }

    /// Under Wine the counter is assumed, not verified. A wrong one is caught by
    /// request timing and dropped, so it cannot report a wrong ping for long.
    #[test]
    fn a_counter_that_disagrees_with_requests_is_dropped() {
        // The counter reads 500 ms ahead of the clock the game stamped with.
        let t = PingTracker::with_perf_clock(Some(fake_perf_clock));
        let base = 1_789_999_990_000; // wall time of counter 990_000
        for n in 0..3 {
            let sent_wall = base + n * 10_000;
            t.on_packet(&request(sent_wall), SERVER);
            let sent_counter = 990_000 + n * 10_000 - 500;
            t.on_packet(&response(sent_wall + 70, sent_counter + UNREAL_CLOCK_OFFSET_MS), SERVER);
            assert_eq!(t.current_ping_ms(), Some(70), "ping {n} follows the request, not the counter");
        }
        assert!(t.inner.lock().perf_distrusted);
    }
}

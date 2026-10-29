//! The last stretch of captured traffic, held in memory, so a boss fight can be
//! turned into an Evidence Slice without packet logging having been on.
//!
//! Packet logging writes everything to disk and is off by default, for good
//! reason: a capture holds chat and bystanders. But a slice can only be cut
//! from the packets, so without this nobody could upload a fight unless they
//! had thought to turn logging on first.
//!
//! What is here never touches disk. It is the same segments the parser is
//! already being fed, kept for about an hour and then dropped; the only thing
//! ever written is the slice the builder cuts from it, which is allowlisted,
//! name-blinded and verified, and only for a fight the meter saved.

use std::collections::VecDeque;

use parking_lot::Mutex;

use crate::capture::captured_payload::CapturedPayload;
use crate::capture::evidence_slice::{CapturedPacket, LEAD_IN_MS, PRELUDE_MS, TAIL_MS};

/// Long enough for the slice's prelude plus a long fight. Older than this and
/// a segment can no longer be part of any slice worth cutting.
const KEEP_MS: i64 = PRELUDE_MS + LEAD_IN_MS + TAIL_MS + 35 * 60_000;
/// A ceiling that an hour of real play does not approach (a half-hour capture
/// is about 3 MB). It exists so nothing unexpected can grow this without bound.
const MAX_BYTES: usize = 96 * 1024 * 1024;
const MAX_PACKETS: usize = 100_000;

fn held_bytes(packet: &CapturedPacket) -> usize {
    std::mem::size_of::<CapturedPacket>() + packet.stream.capacity() + packet.bytes.capacity()
}

struct Ring {
    packets: VecDeque<CapturedPacket>,
    bytes: usize,
}

static RING: Mutex<Ring> = Mutex::new(Ring { packets: VecDeque::new(), bytes: 0 });
#[cfg(test)]
pub(crate) static TEST_SERIAL: Mutex<()> = Mutex::new(());

/// Remember one captured segment. Called for exactly the segments the packet
/// logger would have been given, and stamped the same way: with the packet's
/// capture time, as the time of dispatch falls on libpcap's 100 ms read grid.
pub fn record(cap: &CapturedPayload) {
    let at = cap.capture_time_ms().unwrap_or_else(crate::clock::now_ms);
    record_at(at, cap.stream_key(), &cap.data);
}

/// Remember one segment of `stream`, captured at `at`.
pub fn record_at(at: i64, stream: String, data: &[u8]) {
    if data.len().saturating_add(stream.capacity()).saturating_add(std::mem::size_of::<CapturedPacket>()) > MAX_BYTES {
        return;
    }
    let mut ring = RING.lock();
    let packet = CapturedPacket {
        captured_at_ms: at,
        stream,
        bytes: data.to_vec(),
    };
    ring.bytes += held_bytes(&packet);
    ring.packets.push_back(packet);
    while let Some(front) = ring.packets.front() {
        if front.captured_at_ms >= at.saturating_sub(KEEP_MS) && ring.bytes <= MAX_BYTES && ring.packets.len() <= MAX_PACKETS {
            break;
        }
        let gone = ring.packets.pop_front().map(|p| held_bytes(&p)).unwrap_or(0);
        ring.bytes -= gone;
    }
}

/// Everything currently held, oldest first.
pub fn snapshot() -> Vec<CapturedPacket> {
    RING.lock().packets.iter().cloned().collect()
}

#[cfg(test)]
pub fn clear() {
    let mut ring = RING.lock();
    ring.packets.clear();
    ring.bytes = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ring is one for the process; tests that fill it take turns.

    fn segment(captured_at_ms: i64, data: &[u8]) -> CapturedPayload {
        CapturedPayload {
            src_port: 7777,
            dst_port: 50000,
            data: data.to_vec(),
            device_name: None,
            captured_at_ms,
            src_ip: None,
            dst_ip: None,
            tcp_seq: 0,
            tcp_ack: 0,
        }
    }

    #[test]
    fn keeps_recent_segments_and_forgets_old_ones() {
        let _turn = TEST_SERIAL.lock();
        clear();
        let at = 1_791_237_434_407;
        record(&segment(at, &[1, 2, 3]));
        record(&segment(at + KEEP_MS + 1, &[4, 5]));
        let held = snapshot();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].bytes, vec![4, 5]);
        assert_eq!(held[0].stream, "Client:50000:7777");
        clear();
    }

    #[test]
    fn a_segment_is_stamped_with_its_capture_time() {
        let _turn = TEST_SERIAL.lock();
        clear();
        // Dispatched on the next 100 ms read, so the clock reads later.
        crate::clock::set_override(Some(1_791_237_434_500));
        record(&segment(1_791_237_434_407, &[1]));
        // A segment with no capture time (a test, a replay) takes the clock.
        record(&segment(0, &[2]));
        let held = snapshot();
        crate::clock::set_override(None);
        let stamps: Vec<i64> = held.iter().map(|p| p.captured_at_ms).collect();
        assert_eq!(stamps, vec![1_791_237_434_407, 1_791_237_434_500]);
        clear();
    }

    #[test]
    fn many_tiny_segments_are_bounded_including_metadata() {
        let _turn = TEST_SERIAL.lock();
        clear();
        for _ in 0..MAX_PACKETS + 1 {
            record_at(1000, "Client:50000:7777".into(), &[1]);
        }
        let ring = RING.lock();
        assert_eq!(ring.packets.len(), MAX_PACKETS);
        assert_eq!(ring.bytes, ring.packets.iter().map(held_bytes).sum::<usize>());
        assert!(ring.bytes > MAX_PACKETS, "metadata counts towards the budget");
        drop(ring);
        clear();
    }
}

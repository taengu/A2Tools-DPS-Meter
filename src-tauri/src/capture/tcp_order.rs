//! A connection's bytes in the order the server sent them.
//!
//! libpcap hands segments over as they arrive. On Wi-Fi they can come late
//! (after the ones that followed them), twice (a retransmission, cut where
//! the first copy was not), or not at all (lost before the capture saw them),
//! and the stream was read in arrival order. One segment missing from a
//! zone load turned the next bytes into a frame length: the walk then read
//! mid-packet bytes as lengths for minutes, a boss's spawn went with them, and
//! its fight was never shown (2026-10-27: Ritualist Garshim, an unnamed field
//! boss, Bloodfang Pnyn's HP).
//!
//! Each segment's sequence number says where its bytes go. Bytes already had
//! are dropped, a segment from ahead waits for the ones before it, and when
//! those do not come in time the hole is reported, so the reader can drop the
//! frame it was in and find the next one, rather than reading on through it.

/// How long a hole may wait for its bytes (capture time): Wi-Fi's late
/// segments come within a few milliseconds, a retransmission within a
/// round trip or two.
const GAP_WAIT_MS: i64 = 250;
/// Or this many bytes waiting behind it, whichever comes first.
const MAX_WAITING_BYTES: usize = 256 * 1024;
/// Tiny segments also consume allocations and sorting work. A byte quota
/// alone could retain hundreds of thousands of one-byte segments.
const MAX_WAITING_SEGMENTS: usize = 512;
/// A segment this far from where the stream is (either way) is another
/// stream in all but name (a reconnect from the same ports): start over.
const FAR: i64 = 4 * 1024 * 1024;

/// What the reader gets: bytes in order, or a hole of `missing` bytes.
#[derive(Debug, PartialEq, Eq)]
pub enum Piece {
    Bytes(Vec<u8>),
    Gap { missing: u32 },
}

#[derive(Default)]
pub struct TcpOrder {
    /// The sequence number of the next byte owed to the reader.
    next: Option<u32>,
    /// Segments from ahead of `next`, by sequence number, with when each came.
    waiting: Vec<(u32, Vec<u8>, i64)>,
    waiting_bytes: usize,
}

/// Where `seq` lies from `from`, in bytes, wrapping as sequence numbers do.
fn distance(from: u32, seq: u32) -> i64 {
    seq.wrapping_sub(from) as i32 as i64
}

impl TcpOrder {
    /// Take segment `data` starting at sequence number `seq`, captured at
    /// `at_ms`, and give back what can be read now, in order.
    pub fn push(&mut self, seq: u32, data: &[u8], at_ms: i64) -> Vec<Piece> {
        let mut out = Vec::new();
        if data.is_empty() {
            return out;
        }
        let Some(next) = self.next else {
            self.next = Some(seq.wrapping_add(data.len() as u32));
            out.push(Piece::Bytes(data.to_vec()));
            return out;
        };
        let d = distance(next, seq);
        if !(-FAR..=FAR).contains(&d) {
            // Not this stream's bytes any more: whatever waits is lost with it.
            self.waiting.clear();
            self.waiting_bytes = 0;
            self.next = Some(seq.wrapping_add(data.len() as u32));
            out.push(Piece::Gap { missing: 0 });
            out.push(Piece::Bytes(data.to_vec()));
            return out;
        }
        if d <= 0 {
            self.take(seq, data, &mut out);
            self.drain(&mut out);
        } else if !self.waiting.iter().any(|(s, b, _)| *s == seq && b.len() >= data.len()) {
            self.waiting.push((seq, data.to_vec(), at_ms));
            self.waiting_bytes += data.len();
            self.waiting.sort_by_key(|(s, _, _)| distance(next, *s));
        }
        // A hole that has waited long enough: its bytes are not coming.
        while let Some(&(first, _, since)) = self.waiting.first() {
            let late = at_ms > 0 && since > 0 && at_ms - since >= GAP_WAIT_MS;
            if !late && self.waiting_bytes <= MAX_WAITING_BYTES && self.waiting.len() < MAX_WAITING_SEGMENTS {
                break;
            }
            let next = self.next.unwrap_or(first);
            let missing = distance(next, first).max(0) as u32;
            out.push(Piece::Gap { missing });
            self.next = Some(first);
            self.drain(&mut out);
        }
        out
    }

    /// The part of `data` (at `seq`) past what the reader has had.
    fn take(&mut self, seq: u32, data: &[u8], out: &mut Vec<Piece>) {
        let next = self.next.expect("set before take");
        let had = (-distance(next, seq)).max(0) as usize;
        if had >= data.len() {
            return; // all of it already read: a retransmission
        }
        out.push(Piece::Bytes(data[had..].to_vec()));
        self.next = Some(seq.wrapping_add(data.len() as u32));
    }

    /// Every waiting segment the reader has now reached.
    fn drain(&mut self, out: &mut Vec<Piece>) {
        while let Some(&(seq, _, _)) = self.waiting.first() {
            let next = self.next.expect("set before drain");
            if distance(next, seq) > 0 {
                break;
            }
            let (seq, data, _) = self.waiting.remove(0);
            self.waiting_bytes -= data.len();
            self.take(seq, &data, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(pieces: &[Piece]) -> Vec<u8> {
        pieces.iter().flat_map(|p| match p {
            Piece::Bytes(b) => b.clone(),
            Piece::Gap { .. } => Vec::new(),
        }).collect()
    }

    #[test]
    fn segments_in_order_pass_straight_through() {
        let mut o = TcpOrder::default();
        assert_eq!(o.push(1000, b"abc", 1), vec![Piece::Bytes(b"abc".to_vec())]);
        assert_eq!(o.push(1003, b"de", 2), vec![Piece::Bytes(b"de".to_vec())]);
    }

    #[test]
    fn a_late_segment_is_put_back_in_its_place() {
        let mut o = TcpOrder::default();
        o.push(1000, b"abc", 1);
        assert!(o.push(1006, b"ghi", 2).is_empty(), "waits for def");
        assert_eq!(bytes(&o.push(1003, b"def", 3)), b"defghi");
    }

    #[test]
    fn a_retransmission_gives_only_what_was_not_had() {
        let mut o = TcpOrder::default();
        o.push(1000, b"abcdef", 1);
        assert!(o.push(1000, b"abcdef", 2).is_empty(), "the same again");
        assert!(o.push(1002, b"cd", 3).is_empty(), "a piece of it");
        // Cut elsewhere, running on past what was had.
        assert_eq!(bytes(&o.push(1004, b"efgh", 4)), b"gh");
    }

    #[test]
    fn a_hole_that_never_fills_is_reported_then_read_past() {
        let mut o = TcpOrder::default();
        o.push(1000, b"abc", 1_000);
        assert!(o.push(1010, b"klm", 1_010).is_empty());
        // Still in time: nothing yet.
        assert!(o.push(1013, b"no", 1_100).is_empty());
        let out = o.push(1015, b"pq", 1_300);
        assert_eq!(out[0], Piece::Gap { missing: 7 });
        assert_eq!(bytes(&out), b"klmnopq");
        // And on from there as usual.
        assert_eq!(o.push(1017, b"r", 1_301), vec![Piece::Bytes(b"r".to_vec())]);
    }

    #[test]
    fn sequence_numbers_wrap() {
        let mut o = TcpOrder::default();
        o.push(u32::MAX - 1, b"ab", 1);
        assert!(o.push(2, b"ef", 2).is_empty());
        assert_eq!(bytes(&o.push(0, b"cd", 3)), b"cdef");
    }

    #[test]
    fn a_segment_far_off_starts_the_stream_again() {
        let mut o = TcpOrder::default();
        o.push(1000, b"abc", 1);
        let out = o.push(900_000_000, b"xyz", 2);
        assert_eq!(out, vec![Piece::Gap { missing: 0 }, Piece::Bytes(b"xyz".to_vec())]);
    }

    #[test]
    fn tiny_segments_force_a_gap_before_the_queue_grows_past_its_count_limit() {
        let mut o = TcpOrder::default();
        o.push(1000, b"a", 1000);
        for i in 0..MAX_WAITING_SEGMENTS - 1 {
            assert!(o.push(1010 + 2 * i as u32, &[i as u8], 1000).is_empty());
        }
        let out = o.push(1010 + 2 * (MAX_WAITING_SEGMENTS - 1) as u32, &[255], 1000);
        assert_eq!(out, vec![Piece::Gap { missing: 9 }, Piece::Bytes(vec![0])]);
        assert!(o.waiting.len() < MAX_WAITING_SEGMENTS);
        assert_eq!(o.waiting_bytes, o.waiting.len());
        // The gap has advanced the stream correctly: the missing next byte
        // can still arrive and release the following queued segment.
        assert_eq!(bytes(&o.push(1011, b"z", 1000)), vec![b'z', 1]);
    }

    #[test]
    fn a_long_burst_of_sparse_one_byte_segments_has_bounded_state() {
        let mut o = TcpOrder::default();
        o.push(1, b"a", 1000);
        let mut delivered = 0;
        for i in 0..4096 {
            let out = o.push(10 + i * 2, &[i as u8], 1000);
            delivered += bytes(&out).len();
            assert!(o.waiting.len() < MAX_WAITING_SEGMENTS);
            assert!(o.waiting.capacity() <= MAX_WAITING_SEGMENTS);
            assert_eq!(o.waiting_bytes, o.waiting.len());
        }
        assert_eq!(delivered + o.waiting.len(), 4096);
    }
}

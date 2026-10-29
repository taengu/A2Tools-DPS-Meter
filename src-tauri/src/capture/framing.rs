//! Where one packet ends and the next begins.
//!
//! `StreamProcessor::consume_stream` used to walk this itself, inline with the
//! parsing it drives. It is split out here because the Evidence Slice builder
//! needs the *same* walk: it decides what to upload per packet, and a second,
//! subtly different framing implementation is exactly the kind of divergence
//! that turns "the server re-derived different numbers" into a permanent
//! mystery. One walk, two callers.
//!
//! The wire shape, as the parser understands it:
//!
//! ```text
//! 00 ...                      padding, skipped
//! 17 03 03 <u16 len> ...      a TLS record from another connection, skipped
//! <varint len> <payload>      a packet; len counts the payload plus 4
//! <varint len> FF FF <u32 size> <lz4>   a compressed bundle of further packets
//! ```
//!
//! So a frame spans `len - 4 + (bytes in the varint)`. With a one-byte varint
//! that is `len - 3`, which is how the rule was first written down; with a
//! two-byte varint it is `len - 2`, and reading that as `len - 3` framed every
//! packet of 126 bytes or more one byte short (a capture of 2026-10-04 lost
//! 3% of its packets to the desync that followed).
//!
//! TLS records turn up because the packet log and the share ring key a stream
//! by the server's port alone, and the game server also talks TLS from that
//! port on a second connection. Read as a game frame, `17` is a length of 23,
//! after which the walk lands in ciphertext and can swallow up to 64 KB of
//! real packets: replays and uploaded logs of 2026-07 and 2026-08 kills came
//! out 20-35% short of the boss's HP, and match it to within 0.5% with the
//! records skipped. No game frame in 538,000 checked starts like one.

use super::stream_processor::read_varint;

/// The largest packet the parser will believe. Past this it treats the length as
/// garbage and resynchronises a byte at a time.
const MAX_PACKET_BYTES: usize = 65535;
/// A length above this that runs past the buffer is treated as corruption rather
/// than as a TCP fragment worth waiting for: waiting on a garbage length would
/// swallow that many bytes of real packets as one bogus frame.
const MAX_FRAGMENT_WAIT_BYTES: usize = 16384;
/// Unless the frame shows a bundle's header (`FF FF`, then a decompressed size
/// no larger than a bundle may claim), which a garbage length almost never
/// lands on. Bundles run past 16 KB: the zone-load bundle carrying your own
/// self record was 17,072 bytes in a capture of 2026-10-09, and walking past
/// it lost that record (you unnamed for the next fight), the spawns with it,
/// and about 5 s of resync. Waited for up to the largest packet believed.
const MAX_BUNDLE_WAIT_BYTES: usize = MAX_PACKET_BYTES;
/// Refuse to allocate for a bundle claiming to decompress to more than this.
const MAX_DECOMPRESSED_BYTES: usize = 1_000_000;
/// Bundles nest. Four is far past anything observed and stops a crafted file
/// from recursing us to death. The outermost bundle is depth 1.
pub const MAX_BUNDLE_DEPTH: usize = 4;
/// A single parser read may contain many small bundles, each expanding close
/// to the individual limit. Bound their combined expansion and failed probes
/// as well as their nesting depth.
const MAX_DECOMPRESSED_TOTAL_BYTES: usize = 8 * 1024 * 1024;
const MAX_BUNDLE_ATTEMPTS: usize = 256;

/// Shared by every bundle opened while processing one input, including nested
/// and embedded bundles. Charge the declared output before asking LZ4 to
/// allocate; failed blocks do not refund the work already attempted.
#[derive(Debug)]
pub struct DecompressionBudget {
    bytes_left: usize,
    attempts_left: usize,
}

impl Default for DecompressionBudget {
    fn default() -> Self {
        Self::new(MAX_DECOMPRESSED_TOTAL_BYTES, MAX_BUNDLE_ATTEMPTS)
    }
}

impl DecompressionBudget {
    pub fn new(max_bytes: usize, max_attempts: usize) -> Self {
        Self { bytes_left: max_bytes, attempts_left: max_attempts }
    }

    pub fn is_exhausted(&self) -> bool {
        self.bytes_left == 0 || self.attempts_left == 0
    }

    pub fn decompress(&mut self, payload: &[u8]) -> Option<Vec<u8>> {
        let size = self.charge(payload)?;
        lz4_flex::decompress(&payload[6..], size).ok()
    }

    /// Reserve one expansion without allocating. A caller applying both an
    /// input budget and an operation budget can charge one, then decompress
    /// through the other, without opening the same block twice.
    pub fn charge(&mut self, payload: &[u8]) -> Option<usize> {
        if self.is_exhausted() {
            return None;
        }
        self.attempts_left -= 1;
        let size = bundle_output_size(payload)?;
        let Some(left) = self.bytes_left.checked_sub(size) else {
            self.bytes_left = 0;
            return None;
        };
        self.bytes_left = left;
        Some(size)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// A plain packet. `range` covers the whole thing, length prefix included.
    Packet,
    /// An `FF FF` LZ4 bundle holding further packets. `range` covers the whole
    /// frame; `payload_start` is the offset of the first `FF` within it.
    Bundle,
}

#[derive(Debug, Clone, Copy)]
pub struct Frame {
    pub kind: FrameKind,
    /// Byte range within the buffer this frame was walked from.
    pub start: usize,
    pub end: usize,
    /// Offset from `start` at which the payload begins (i.e. the length prefix
    /// width). For a bundle this is where the `FF FF` sits.
    pub payload_start: usize,
}

impl Frame {
    pub fn len(&self) -> usize {
        self.end - self.start
    }
    pub fn is_empty(&self) -> bool {
        self.end == self.start
    }
    pub fn bytes<'a>(&self, buffer: &'a [u8]) -> &'a [u8] {
        &buffer[self.start..self.end]
    }
    pub fn payload<'a>(&self, buffer: &'a [u8]) -> &'a [u8] {
        &buffer[self.start + self.payload_start..self.end]
    }
}

/// The result of walking a buffer: the frames found, and how many bytes were
/// consumed. Bytes past `consumed` are an incomplete trailing packet and must be
/// kept for the next read.
#[derive(Debug, Default)]
pub struct Framing {
    pub frames: Vec<Frame>,
    pub consumed: usize,
}

/// Walk `buffer` into frames, stopping at the first incomplete one.
///
/// This is a transcription of the walk `consume_stream` performed inline, and it
/// must stay one — including the resync behaviour, which is load-bearing: a
/// capture that starts mid-stream is the normal case, not the exception.
pub fn walk(buffer: &[u8]) -> Framing {
    let mut out = Framing::default();
    let mut offset = 0usize;

    while offset < buffer.len() {
        // 1. Skip zero padding.
        if buffer[offset] == 0x00 {
            offset += 1;
            continue;
        }

        if let Some(size) = tls_record_size(&buffer[offset..]) {
            if offset + size > buffer.len() {
                break; // the rest of it is still to come
            }
            offset += size;
            continue;
        }

        let length_info = read_varint(buffer, offset);
        if length_info.length <= 0 || length_info.value <= 0 {
            if offset + 5 > buffer.len() {
                break;
            }
            offset += 1;
            continue;
        }

        // 2. Physical size, varint included.
        let Some(total_packet_bytes) = frame_size(length_info.value, length_info.length) else {
            offset += 1;
            continue;
        };

        // Resync on invalid sizes.
        if total_packet_bytes > MAX_PACKET_BYTES {
            offset += 1;
            continue;
        }

        // 3. TCP fragmentation check (anti-stall gate).
        if offset + total_packet_bytes > buffer.len() {
            let limit = if bundle_header_at(buffer, offset + length_info.length as usize) {
                MAX_BUNDLE_WAIT_BYTES
            } else {
                MAX_FRAGMENT_WAIT_BYTES
            };
            if total_packet_bytes > limit {
                offset += 1;
                continue;
            }
            break; // Legitimate fragment — wait for more bytes.
        }

        // 4. Check for an FF FF compressed bundle.
        let payload_start = length_info.length as usize;
        let is_bundle = payload_start + 1 < total_packet_bytes
            && buffer[offset + payload_start] == 0xFF
            && buffer[offset + payload_start + 1] == 0xFF;

        out.frames.push(Frame {
            kind: if is_bundle { FrameKind::Bundle } else { FrameKind::Packet },
            start: offset,
            end: offset + total_packet_bytes,
            payload_start,
        });
        offset += total_packet_bytes;
    }

    out.consumed = offset;
    out
}

/// Where the first whole frame starts in `buffer`, bytes that came right
/// after a hole in the stream (`tcp_order`): the hole took the start of the
/// frame they continue, so the walk must not read them as a length.
///
/// A place counts when three frames walk whole from it, or two and then the
/// buffer's end or a frame still to come, or one bundle so. Its first two
/// frames must open as nearly every packet does (`COMMON_OPCODE_HIGH`):
/// lengths alone, a stretch of a frame's middle walked from its third byte to
/// the end of the stream (a test of the capture of 2026-10-27). Each place is
/// tried that far only, never to the end of the buffer: walked to the end
/// from each of 8,192 places, a debug build fell behind the capture, its
/// dropped packets made more holes, and the meter read nothing for good.
/// Looked for in the first `RESYNC_SEARCH` bytes. `None`: no such place yet,
/// wait for more.
pub fn resync_start(buffer: &[u8]) -> Option<usize> {
    let starts_here = |from: usize| -> bool {
        let mut at = from;
        let mut whole = 0;
        let mut first_is_bundle = false;
        loop {
            while at < buffer.len() && buffer[at] == 0x00 {
                at += 1;
            }
            if at >= buffer.len() {
                return whole >= 2 || (whole == 1 && first_is_bundle);
            }
            let length = read_varint(buffer, at);
            if length.length <= 0 || length.value <= 0 {
                return false;
            }
            let Some(size) = frame_size(length.value, length.length) else { return false };
            if size > MAX_PACKET_BYTES {
                return false;
            }
            let payload = at + length.length as usize;
            let opens_as_packets_do = buffer.get(payload + 1).is_some_and(|b| COMMON_OPCODE_HIGH.contains(b));
            if whole < 2 && !opens_as_packets_do {
                return false;
            }
            let bundle = bundle_header_at(buffer, payload);
            if at + size > buffer.len() {
                let limit = if bundle { MAX_BUNDLE_WAIT_BYTES } else { MAX_FRAGMENT_WAIT_BYTES };
                return size <= limit && (whole >= 2 || (whole == 1 && bundle));
            }
            if whole == 0 {
                first_is_bundle = bundle;
            }
            whole += 1;
            if whole >= 3 {
                return true;
            }
            at += size;
        }
    };
    (0..buffer.len().min(RESYNC_SEARCH)).find(|&from| buffer[from] != 0x00 && starts_here(from))
}

/// The second byte of 99.3% of packets' opcodes (55,954 frames of a capture
/// of 2026-10-27): `xx 36`, `xx 37`, `xx 38`, `00 8D` and a bundle's `FF FF`.
const COMMON_OPCODE_HIGH: [u8; 5] = [0x36, 0x37, 0x38, 0x8D, 0xFF];

/// How far into the bytes after a hole the next frame is looked for. The
/// largest packet is 64 KB, but a frame cut by a hole ends within one or two
/// segments of it.
pub const RESYNC_SEARCH: usize = 8192;

/// Whether a bundle's header starts at `at`: `FF FF`, then a little-endian
/// decompressed size that is non-zero and within what decompress_bundle
/// accepts. Only the header need have arrived.
fn bundle_header_at(buffer: &[u8], at: usize) -> bool {
    let Some(h) = buffer.get(at..at + 6) else {
        return false;
    };
    let size = u32::from_le_bytes([h[2], h[3], h[4], h[5]]) as usize;
    h[0] == 0xFF && h[1] == 0xFF && size > 0 && size <= MAX_DECOMPRESSED_BYTES
}

/// Bytes a TLS record at the start of `b` occupies, header included: content
/// type 20-23, version 3.1-3.4, then a big-endian length of at most 2^14 + 256.
fn tls_record_size(b: &[u8]) -> Option<usize> {
    if b.len() < 5 || !(0x14..=0x17).contains(&b[0]) || b[1] != 0x03 || !(0x01..=0x04).contains(&b[2]) {
        return None;
    }
    let len = u16::from_be_bytes([b[3], b[4]]) as usize;
    (len > 0 && len <= 16_384 + 256).then_some(5 + len)
}

/// Walk the *decompressed* contents of a bundle.
///
/// Same varint framing as [`walk`], but the resync rules differ and the
/// difference is deliberate, not an oversight in either place: the outer walk
/// reads a TCP stream that routinely starts mid-packet, so it resynchronises a
/// byte at a time. This one reads a buffer the game itself framed, so a length
/// that does not parse means the decompression or the framing assumption is
/// wrong, and walking further would invent packets. It stops instead.
pub fn walk_inner(buffer: &[u8]) -> Framing {
    let mut out = Framing::default();
    let mut offset = 0usize;

    while offset < buffer.len() {
        if buffer[offset] == 0x00 {
            offset += 1;
            continue;
        }

        let length_info = read_varint(buffer, offset);
        if length_info.length <= 0 || length_info.value <= 0 {
            break;
        }

        let Some(total) = frame_size(length_info.value, length_info.length) else {
            offset += 1;
            continue;
        };

        let end = offset + total;
        if end > buffer.len() {
            break;
        }

        let payload_start = length_info.length as usize;
        let is_nested_bundle = total > payload_start + 1
            && buffer[offset + payload_start] == 0xFF
            && buffer[offset + payload_start + 1] == 0xFF;

        out.frames.push(Frame {
            kind: if is_nested_bundle { FrameKind::Bundle } else { FrameKind::Packet },
            start: offset,
            end,
            payload_start,
        });

        offset += total;
    }

    out.consumed = offset;
    out
}

/// The stretches of `buffer[..end]` a record can sit in: each packet, and each
/// run of bytes between frames (padding, a walk resynchronising, the end of a
/// bundle that does not frame). A record never runs on into the next frame.
/// Bundles are left out: their records are read from the bundle opened, and
/// LZ4 keeps the bytes of two packets side by side in its literals.
pub fn regions(frames: &[Frame], end: usize) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::with_capacity(frames.len() * 2 + 1);
    let mut at = 0;
    for frame in frames {
        if frame.start > at {
            out.push(at..frame.start);
        }
        if frame.kind == FrameKind::Packet {
            out.push(frame.start..frame.end);
        }
        at = frame.end;
    }
    if end > at {
        out.push(at..end);
    }
    out
}

/// Bytes a frame occupies, given its length varint's value and width. `None`
/// when that is shorter than the varint itself.
pub fn frame_size(value: i32, varint_bytes: i32) -> Option<usize> {
    let size = value as i64 - 4 + varint_bytes as i64;
    (size >= varint_bytes as i64 && size > 0).then_some(size as usize)
}

/// The length varint's value for a frame whose bytes after the varint are
/// `body_len` long.
pub fn length_value(body_len: usize) -> u32 {
    body_len as u32 + 4
}

/// Decompress a bundle payload (one that starts at its `FF FF`).
///
/// `FF FF (2) + decompressed_size (4 LE) + lz4 block`. Returns `None` for
/// anything malformed, because a bundle that will not decompress is a resync
/// artefact rather than an error worth surfacing.
pub fn decompress_bundle(payload: &[u8]) -> Option<Vec<u8>> {
    let size = bundle_output_size(payload)?;
    lz4_flex::decompress(&payload[6..], size).ok()
}

fn bundle_output_size(payload: &[u8]) -> Option<usize> {
    if payload.len() < 7 || payload[..2] != [0xFF, 0xFF] {
        return None;
    }
    let size = u32::from_le_bytes([payload[2], payload[3], payload[4], payload[5]]) as usize;
    if size == 0 || size > MAX_DECOMPRESSED_BYTES {
        return None;
    }
    Some(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(mut v: u32) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let b = (v & 0x7F) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return out;
            }
            out.push(b | 0x80);
        }
    }

    /// `<varint len> <payload>`, len = payload + 4.
    fn packet(payload: &[u8]) -> Vec<u8> {
        let mut v = varint(length_value(payload.len()));
        v.extend_from_slice(payload);
        v
    }

    fn bundle(inner: &[u8]) -> Vec<u8> {
        packet(&bundle_payload(inner))
    }

    fn bundle_payload(inner: &[u8]) -> Vec<u8> {
        let mut payload = vec![0xFF, 0xFF];
        payload.extend_from_slice(&(inner.len() as u32).to_le_bytes());
        payload.extend_from_slice(&lz4_flex::compress(inner));
        payload
    }

    #[test]
    fn many_small_bundles_share_a_finite_expansion_budget() {
        let inner = vec![0; MAX_DECOMPRESSED_BYTES];
        let payload = bundle_payload(&inner);
        assert!(payload.len() < 8_000, "the input is small but expands to a megabyte");
        let mut budget = DecompressionBudget::default();
        let mut expanded = 0;
        while let Some(data) = budget.decompress(&payload) {
            assert_eq!(data, inner);
            expanded += data.len();
        }
        assert!(expanded <= MAX_DECOMPRESSED_TOTAL_BYTES);
        assert!(expanded > MAX_DECOMPRESSED_TOTAL_BYTES - MAX_DECOMPRESSED_BYTES);
        assert!(budget.decompress(&payload).is_none());
    }

    #[test]
    fn a_bundle_past_the_remaining_budget_marks_it_exhausted() {
        let payload = bundle_payload(&[1; 101]);
        let mut budget = DecompressionBudget::new(100, 10);
        assert!(budget.decompress(&payload).is_none());
        assert!(budget.is_exhausted());
        assert!(budget.decompress(&bundle_payload(&[1])).is_none());
    }

    #[test]
    fn malformed_blocks_do_not_refund_their_allocation_budget() {
        let mut bad = vec![0xFF, 0xFF];
        bad.extend_from_slice(&1_024u32.to_le_bytes());
        bad.push(0xF0); // literal length needs an extension byte that is absent
        let good = bundle_payload(&vec![1; 1_024]);
        let mut budget = DecompressionBudget::new(2_048, 10);
        assert!(budget.decompress(&bad).is_none());
        assert_eq!(budget.decompress(&good).unwrap().len(), 1_024);
        assert!(budget.decompress(&good).is_none());
        assert!(budget.is_exhausted());
    }

    #[test]
    fn tiny_and_invalid_bundles_also_exhaust_the_attempt_budget() {
        let good = bundle_payload(&[1]);
        let mut budget = DecompressionBudget::new(1_000, 3);
        assert!(budget.decompress(&[0xFF]).is_none());
        assert_eq!(budget.decompress(&good), Some(vec![1]));
        assert_eq!(budget.decompress(&good), Some(vec![1]));
        assert!(budget.decompress(&good).is_none());
        assert!(budget.is_exhausted());
    }

    fn body(op: u8, len: usize) -> Vec<u8> {
        let mut b = vec![op, 0x36];
        b.extend((0..len - 2).map(|i| (i % 251) as u8 | 1));
        b
    }

    /// After a hole (2026-10-27, port 13328, 10:19:42): a segment that is the
    /// middle of a zone-load message, then the bundle with the zone's spawns.
    /// Read from its first byte, `B0 0C` made a 1,582-byte frame of it and
    /// swallowed the bundle; read from where it resyncs, the bundle frames.
    #[test]
    fn after_a_hole_reading_starts_at_a_whole_frame() {
        let text = "B00C3792000087091000114A3C00022D00510000150140E8000402004201000C02120060014E2C960F2F72\
                    0002670A052700F400080000440E4446AC27B94750FA1647720AF000700A00000A57756E64657277616C64\
                    A700222A1E9009C0010E0036411F287BA1010000";
        let middle: Vec<u8> = (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect();
        let spawns = bundle(&[packet(&body(0x41, 40)), packet(&body(0x45, 30))].concat());
        let rest = [packet(&body(0x1D, 12)), packet(&body(0x0E, 7))].concat();
        let stream = [middle.clone(), spawns.clone(), rest.clone()].concat();

        let wrong = walk(&stream);
        assert!(!wrong.frames.iter().any(|f| f.kind == FrameKind::Bundle), "read from the start, the bundle is swallowed");

        let start = resync_start(&stream).expect("a frame start");
        let framed = walk(&stream[start..]);
        let bundle_at = middle.len() - start;
        assert!(framed.frames.iter().any(|f| f.kind == FrameKind::Bundle && f.start == bundle_at), "{start}");
        assert_eq!(framed.consumed, stream.len() - start);

        // With only the middle and part of the bundle there yet: wait.
        assert_eq!(resync_start(&middle[..60]), None);
    }

    #[test]
    fn a_bundle_over_16_kb_split_across_reads_is_waited_for() {
        // Incompressible inner packets, so the bundle stays past 16 KB as the
        // 17,072-byte zone-load bundle of 2026-10-09 did.
        let mut inner = Vec::new();
        let mut x: u32 = 12345;
        for _ in 0..12 {
            let payload: Vec<u8> = (0..1_600).map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                (x >> 16) as u8
            }).collect();
            inner.extend(packet(&payload));
        }
        let big = bundle(&inner);
        assert!(big.len() > MAX_FRAGMENT_WAIT_BYTES, "{} bytes", big.len());
        let mut stream = packet(&[0x23, 0x36, 0x01]);
        stream.extend(&big);
        stream.extend(packet(&[0x41, 0x36, 0x02]));

        // The first read ends part-way into the bundle: wait, don't resync.
        let first = &stream[..9_000];
        let f = walk(first);
        assert_eq!(f.frames.len(), 1);
        assert_eq!(f.consumed, packet(&[0x23, 0x36, 0x01]).len(), "the bundle is kept for the next read");

        // With the rest, the bundle frames whole and decompresses.
        let rest = &stream[f.consumed..];
        let g = walk(rest);
        assert_eq!(g.frames.len(), 2);
        assert_eq!(g.frames[0].kind, FrameKind::Bundle);
        assert_eq!(decompress_bundle(g.frames[0].payload(rest)).unwrap(), inner);
    }

    #[test]
    fn a_garbage_length_past_16_kb_without_a_bundle_header_still_resyncs() {
        // A length of 40,000 whose bytes are not a bundle, running past what
        // has arrived: not worth waiting for, so the walk moves on from it
        // (consumed past 0) instead of holding the stream for 40 KB.
        let mut buf = varint(40_000);
        buf.extend([0x12, 0x34, 0x00, 0x00, 0x00, 0x00]);
        buf.extend(packet(&[0x23, 0x36, 0x07]));
        assert!(walk(&buf).consumed > 0);
        // The same length on a bundle's header is waited for.
        let mut held = varint(40_000);
        held.extend([0xFF, 0xFF, 0x00, 0x00, 0x01, 0x00]);
        assert_eq!(walk(&held).consumed, 0);

        // FF FF with a size no bundle may claim is garbage too.
        let mut bad = varint(40_000);
        bad.extend([0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F]);
        assert!(!bundle_header_at(&bad, varint(40_000).len()));
    }

    #[test]
    fn walks_back_to_back_packets() {
        let mut buf = packet(&[0x23, 0x36, 0x01]);
        buf.extend(packet(&[0x41, 0x36, 0x02, 0x03]));
        let f = walk(&buf);
        assert_eq!(f.frames.len(), 2);
        assert_eq!(f.consumed, buf.len());
        assert_eq!(f.frames[0].payload(&buf), &[0x23, 0x36, 0x01]);
        assert_eq!(f.frames[1].payload(&buf), &[0x41, 0x36, 0x02, 0x03]);
    }

    #[test]
    fn the_length_counts_the_payload_plus_four() {
        // One-, two- and three-byte varints.
        for len in [3usize, 122, 123, 124, 200, 1_000, 16_379, 16_380, 20_000] {
            let payload = body(0x23, len);
            let mut buf = packet(&payload);
            let width = buf.len() - len;
            buf.extend(packet(&[0x41, 0x36, 0x07]));
            let f = walk(&buf);
            assert_eq!(f.frames.len(), 2, "len {len}");
            assert_eq!(f.frames[0].payload_start, width);
            assert_eq!(f.frames[0].payload(&buf), &payload[..], "len {len}");
            assert_eq!(f.frames[1].payload(&buf), &[0x41, 0x36, 0x07], "len {len}");
            assert_eq!(f.consumed, buf.len());
        }
        assert_eq!(varint(length_value(200)).len(), 2);
        assert_eq!(varint(length_value(20_000)).len(), 3);
    }

    #[test]
    fn a_tls_record_between_packets_is_skipped_whole() {
        let mut tls = vec![0x17, 0x03, 0x03, 0x05, 0x7a];
        // Ciphertext that, read as frames, would claim a 14 KB packet.
        tls.extend([0x88, 0x71]);
        tls.extend((0..0x057a - 2).map(|i| (i % 251) as u8 | 1));
        let mut buf = packet(&[0x23, 0x36, 0x01]);
        buf.extend(&tls);
        buf.extend(packet(&body(0x04, 200)));
        let f = walk(&buf);
        assert_eq!(f.consumed, buf.len());
        let got: Vec<_> = f.frames.iter().map(|fr| fr.payload(&buf).to_vec()).collect();
        assert_eq!(got, [vec![0x23, 0x36, 0x01], body(0x04, 200)]);

        // Arriving in two segments: the walk waits for the rest of the record.
        let cut = 3 + 1 + 600;
        let f = walk(&buf[..cut]);
        assert_eq!(f.frames.len(), 1);
        assert_eq!(f.consumed, 4, "stops at the record");
    }

    #[test]
    fn skips_zero_padding() {
        let mut buf = vec![0x00, 0x00];
        buf.extend(packet(&[0xAA, 0xBB]));
        let f = walk(&buf);
        assert_eq!(f.frames.len(), 1);
        assert_eq!(f.frames[0].payload(&buf), &[0xAA, 0xBB]);
    }

    #[test]
    fn stops_on_an_incomplete_trailing_packet() {
        let mut buf = packet(&[1, 2, 3]);
        let full = buf.len();
        buf.push(0x40); // claims 0x40 - 3 = 61 bytes, none of which are here
        let f = walk(&buf);
        assert_eq!(f.frames.len(), 1);
        assert_eq!(f.consumed, full, "the fragment must be left for the next read");
    }

    #[test]
    fn a_bundle_spans_its_length_and_its_packets_frame_inside() {
        let inner_payloads = vec![body(0x04, 5), body(0x05, 300), body(0x40, 40)];
        let inner: Vec<u8> = inner_payloads.iter().flat_map(|p| packet(p)).collect();
        // Small and large bundles: one- and two-byte varints.
        for pad in [0usize, 400] {
            let mut inner = inner.clone();
            inner.extend((0..pad).map(|i| packet(&body(0x23, 3 + i % 3))).flatten());
            let mut buf = bundle(&inner);
            buf.extend(packet(&[0x41, 0x36, 0x09]));
            let f = walk(&buf);
            assert_eq!(f.frames.len(), 2);
            assert_eq!(f.frames[0].kind, FrameKind::Bundle);
            assert_eq!(f.frames[1].payload(&buf), &[0x41, 0x36, 0x09]);
            let data = decompress_bundle(f.frames[0].payload(&buf)).expect("decompresses");
            assert_eq!(data, inner);
            let w = walk_inner(&data);
            assert_eq!(w.consumed, data.len());
            for (frame, want) in w.frames.iter().zip(&inner_payloads) {
                assert_eq!(frame.payload(&data), &want[..]);
            }
        }
    }

    #[test]
    fn a_nested_bundle_frames_like_any_other_packet() {
        let nested = bundle(&packet(&body(0x04, 200)));
        let mut inner = packet(&body(0x05, 150));
        inner.extend(&nested);
        inner.extend(packet(&body(0x41, 4)));
        let w = walk_inner(&inner);
        assert_eq!(w.consumed, inner.len());
        let kinds: Vec<_> = w.frames.iter().map(|f| f.kind).collect();
        assert_eq!(kinds, [FrameKind::Packet, FrameKind::Bundle, FrameKind::Packet]);
    }

    #[test]
    fn a_mixed_stream_frames_end_to_end_however_it_is_split() {
        let payloads: Vec<Vec<u8>> = vec![
            body(0x04, 30),
            body(0x05, 126),
            body(0x40, 700),
            body(0x23, 3),
            body(0x45, 16_380),
            body(0x04, 124),
        ];
        let mut stream = Vec::new();
        let mut want = Vec::new();
        for (n, p) in payloads.iter().enumerate() {
            if n == 2 {
                stream.extend([0x00, 0x00]);
                let b = bundle(&packet(&body(0x06, 250)));
                let width = b.iter().position(|x| x & 0x80 == 0).unwrap() + 1;
                want.push(b[width..].to_vec());
                stream.extend(b);
            }
            stream.extend(packet(p));
            want.push(p.clone());
        }

        let whole = walk(&stream);
        assert_eq!(whole.consumed, stream.len());
        let got: Vec<_> = whole.frames.iter().map(|f| f.payload(&stream).to_vec()).collect();
        assert_eq!(got, want);

        // Fed in TCP-sized pieces, keeping the unconsumed tail each time.
        for step in [1usize, 7, 100, 1460] {
            let mut pending = Vec::new();
            let mut got = Vec::new();
            for chunk in stream.chunks(step) {
                pending.extend_from_slice(chunk);
                let f = walk(&pending);
                got.extend(f.frames.iter().map(|fr| fr.payload(&pending).to_vec()));
                pending.drain(..f.consumed);
            }
            assert!(pending.is_empty(), "step {step}");
            assert_eq!(got, want, "step {step}");
        }
    }
}

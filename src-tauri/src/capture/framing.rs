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
//! <varint len> <payload>      a packet; len counts the payload plus 4
//! <varint len> FF FF <u32 size> <lz4>   a compressed bundle of further packets
//! ```
//!
//! So a frame spans `len - 4 + (bytes in the varint)`. With a one-byte varint
//! that is `len - 3`, which is how the rule was first written down; with a
//! two-byte varint it is `len - 2`, and reading that as `len - 3` framed every
//! packet of 126 bytes or more one byte short (a capture of 2026-10-04 lost
//! 3% of its packets to the desync that followed).

use super::stream_processor::read_varint;

/// The largest packet the parser will believe. Past this it treats the length as
/// garbage and resynchronises a byte at a time.
const MAX_PACKET_BYTES: usize = 65535;
/// A length above this that runs past the buffer is treated as corruption rather
/// than as a TCP fragment worth waiting for.
const MAX_FRAGMENT_WAIT_BYTES: usize = 16384;
/// Refuse to allocate for a bundle claiming to decompress to more than this.
const MAX_DECOMPRESSED_BYTES: usize = 1_000_000;

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
            if total_packet_bytes > MAX_FRAGMENT_WAIT_BYTES {
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
    if payload.len() < 7 {
        return None;
    }
    let size = u32::from_le_bytes([payload[2], payload[3], payload[4], payload[5]]) as usize;
    if size == 0 || size > MAX_DECOMPRESSED_BYTES {
        return None;
    }
    lz4_flex::decompress(&payload[6..], size).ok()
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
        let mut payload = vec![0xFF, 0xFF];
        payload.extend_from_slice(&(inner.len() as u32).to_le_bytes());
        payload.extend_from_slice(&lz4_flex::compress(inner));
        packet(&payload)
    }

    fn body(op: u8, len: usize) -> Vec<u8> {
        let mut b = vec![op, 0x36];
        b.extend((0..len - 2).map(|i| (i % 251) as u8 | 1));
        b
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

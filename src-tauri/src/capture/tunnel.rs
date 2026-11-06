//! Game traffic carried inside a tunnel's envelope (a VPN, "ping reducer" or
//! game accelerator), as one player's connections were (report XEVZHK,
//! 2026-11-04). Every frame on the connection is
//!
//! ```text
//! 01 <length u24 LE> <fields>
//! field: 05 <tag> <length u8> <value> | 09 <tag> <length u16 LE> <value>
//! ```
//!
//! starting `05 01 01 <type>` (9 data, 10 acknowledgement, 4 the opening) and
//! carrying the connection's 16-byte id in tag 3 (`05 03 10 ..`), a running
//! sequence number in tag `0x22`, and the game's own bytes in tag `0x23`.
//! Read as the game's stream, the envelope was skipped as noise and only some
//! packets were found, depending on the bytes of each connection's id: about
//! 900 damage records of a run's 21,272, none at all on others.
//!
//! The unwrapper recognises a connection by two whole envelope frames in a row
//! (nothing in the game's own stream looks like that) and from then on hands
//! the framing the tag `0x23` bytes, in order. A connection that is not
//! tunnelled is left alone.

/// The longest envelope frame accepted; the longest seen was under 4 KB.
const MAX_FRAME: usize = 64 * 1024;
/// The game's bytes.
const TAG_PAYLOAD: u8 = 0x23;

#[derive(Default)]
pub struct Unwrapper {
    /// The connection is tunnelled (two whole frames were seen).
    active: bool,
    /// Unwrapped game bytes not yet consumed by the framing.
    game: Vec<u8>,
}

/// The size of the envelope frame at `at`, when one starts there: `Some(None)`
/// for one that starts there but has not fully arrived.
fn frame_at(buf: &[u8], at: usize) -> Option<Option<usize>> {
    let head = buf.get(at..at + 8)?;
    if head[0] != 0x01 {
        return None;
    }
    let len = head[1] as usize | (head[2] as usize) << 8 | (head[3] as usize) << 16;
    // Every frame opens with its type field, `05 01 01 <type>`.
    if !(8..=MAX_FRAME).contains(&len) || head[4..7] != [0x05, 0x01, 0x01] {
        return None;
    }
    let size = 4 + len;
    Some((at + size <= buf.len()).then_some(size))
}

/// The game bytes (tag `0x23`) in the fields of a whole frame.
fn payload(frame: &[u8]) -> Vec<&[u8]> {
    let body = &frame[4..];
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 <= body.len() {
        let (tag, len, value_at) = match body[i] {
            0x05 => (body[i + 1], body[i + 2] as usize, i + 3),
            0x09 if i + 4 <= body.len() => (body[i + 1], body[i + 2] as usize | (body[i + 3] as usize) << 8, i + 4),
            // The opening frame ends in two bytes of another form: nothing
            // of the game's comes after.
            _ => break,
        };
        let Some(value) = body.get(value_at..value_at + len) else { break };
        if tag == TAG_PAYLOAD {
            out.push(value);
        }
        i = value_at + len;
    }
    out
}

impl Unwrapper {
    /// Whether two whole frames start at `at`, back to back.
    fn two_frames_at(buf: &[u8], at: usize) -> bool {
        matches!(frame_at(buf, at), Some(Some(size)) if matches!(frame_at(buf, at + size), Some(Some(_))))
    }

    /// Takes the stream's buffered bytes from their start: for a tunnelled
    /// connection, unwraps every whole frame into `game` and returns how many
    /// bytes of `buf` that used; `None` for a connection that is not (yet)
    /// known to be tunnelled, whose bytes are the game's own.
    pub fn feed(&mut self, buf: &[u8]) -> Option<usize> {
        let mut at = 0;
        if !self.active {
            // A meter started partway through a connection begins mid-frame:
            // the first place two frames follow each other.
            at = (0..buf.len()).find(|&i| buf[i] == 0x01 && Self::two_frames_at(buf, i))?;
            self.active = true;
        }
        loop {
            match frame_at(buf, at) {
                Some(Some(size)) => {
                    for part in payload(&buf[at..at + size]) {
                        self.game.extend_from_slice(part);
                    }
                    at += size;
                }
                // Waits for the rest of the frame.
                Some(None) => return Some(at),
                // Lost step (a hole in the capture): on at the next two frames.
                None if at + 8 > buf.len() => return Some(at),
                None => match (at + 1..buf.len()).find(|&i| buf[i] == 0x01 && Self::two_frames_at(buf, i)) {
                    Some(next) => at = next,
                    // None whole yet: keep the tail that may hold one.
                    None => return Some(buf.len().saturating_sub(MAX_FRAME).max(at)),
                },
            }
        }
    }

    /// The unwrapped game bytes waiting for the framing.
    pub fn game(&self) -> &[u8] {
        &self.game
    }

    /// Drops the first `n` game bytes, which the framing consumed.
    pub fn consumed(&mut self, n: usize) {
        self.game.drain(..n.min(self.game.len()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
    }

    /// A data frame as the tunnel sends one, carrying `game`.
    fn data(seq: u16, game: &[u8]) -> Vec<u8> {
        let mut body = hex("05 01 01 09 05 02 01 01 05 03 10 51 97 F3 77 0C 65 45 BE 8F 8D B3 22 21 A6 C8 41");
        body.extend([0x05, 0x22, 0x02, seq as u8, (seq >> 8) as u8]);
        if game.len() < 256 {
            body.extend([0x05, TAG_PAYLOAD, game.len() as u8]);
        } else {
            body.extend([0x09, TAG_PAYLOAD, game.len() as u8, (game.len() >> 8) as u8]);
        }
        body.extend_from_slice(game);
        body.extend(hex("05 25 01 01"));
        let mut frame = vec![0x01, body.len() as u8, (body.len() >> 8) as u8, 0];
        frame.extend(body);
        frame
    }

    fn ack() -> Vec<u8> {
        hex("01 25 00 00 05 01 01 0A 05 02 01 01 05 03 10 51 97 F3 77 0C 65 45 BE 8F 8D B3 22 21 A6 C8 41 05 2C 02 3D 0B 05 2B 02 3C 0B")
    }

    /// The opening frame of a connection, as captured (2026-11-04).
    const OPENING: &str = "01 1D 00 00 05 01 01 04 05 03 10 51 97 F3 77 0C 65 45 BE 8F 8D B3 22 21 A6 C8 41 05 1A 01 02 01 1C";

    #[test]
    fn a_tunnelled_connection_gives_the_game_bytes_in_order() {
        let damage = hex("1E 04 38 E3 25 00 00 E3 25 D9 8E 15 01 01 02 CF CC 6B 6C 01 00 00 00 E8 64 01 00");
        let big = vec![0xAB; 600];
        let mut stream = hex(OPENING);
        stream.extend(data(1, &damage[..10]));
        stream.extend(ack());
        stream.extend(data(2, &damage[10..]));
        stream.extend(data(3, &big));
        let mut u = Unwrapper::default();
        // However the bytes arrive, the same game bytes come out.
        for cut in [stream.len(), 50, 7] {
            let mut u2 = Unwrapper::default();
            let mut got = Vec::new();
            let mut pending = Vec::new();
            for chunk in stream.chunks(cut) {
                pending.extend_from_slice(chunk);
                if let Some(used) = u2.feed(&pending) {
                    pending.drain(..used);
                }
                got.extend_from_slice(u2.game());
                let n = u2.game().len();
                u2.consumed(n);
            }
            let mut want = damage.clone();
            want.extend(&big);
            assert_eq!(got, want, "in chunks of {cut}");
        }
        assert_eq!(u.feed(&stream), Some(stream.len()));
    }

    #[test]
    fn the_game_s_own_stream_is_left_alone() {
        let plain = hex("1E 04 38 E3 25 00 00 E3 25 D9 8E 15 01 01 02 CF CC 6B 6C 01 00 00 00 E8 64 01 00 0E 00 36 01 05 01 01");
        assert_eq!(Unwrapper::default().feed(&plain), None);
    }

    #[test]
    fn a_meter_started_mid_connection_finds_the_next_frames() {
        let mut stream = data(7, b"\x05\x00\x36\x01")[9..].to_vec();
        stream.extend(data(8, b"game"));
        stream.extend(ack());
        let mut u = Unwrapper::default();
        assert_eq!(u.feed(&stream), Some(stream.len()));
        assert_eq!(u.game(), b"game");
    }
}

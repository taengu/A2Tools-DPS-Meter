//! The game's Abyss pairing: the two servers that share an Abyss. Sent as you
//! enter the world and after a server move:
//!
//! ```text
//! 0F 0A 40 | 01 01 | 02 | 18 05 01 | FF 08 02 | D2 1B 00
//!   len op   kind?  count  1304 side 1  2303 side 2  ?
//! ```
//!
//! Seen 2026-11-15: Kaisinel (1304) with Triniel (2303), and Vaizel (1303,
//! side 2) with Siel (1301, side 1), both Elyos: a player's own server is not
//! always side 1, and the pairing can be within one faction. It is the Abyss
//! one, not the Spacetime Rift's: Vaizel's rift opponent was Zemurru (2315),
//! which this frame never named (the rift pairing comes some other way, not
//! found yet). The two leading bytes were 01 01 each time; the trailing u16
//! (7122, 7138, 7906) is unknown. Plugins get it as `abyssPairing`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::capture::stream_processor::read_varint;

const OPCODE: [u8; 2] = [0x0A, 0x40];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Matchup {
    /// The two leading bytes (01 01 so far).
    pub kind: [u8; 2],
    /// Each server in the pairing with its side (1, 2).
    pub servers: Vec<(u16, u8)>,
}

impl Matchup {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": self.kind,
            "servers": self.servers.iter().map(|&(server, side)| serde_json::json!({ "server": server, "side": side })).collect::<Vec<_>>(),
        })
    }
}

/// A `0A 40` frame read, if `packet` is one that makes sense: a few servers,
/// each a real server id (1000-2999) on side 1 or 2.
pub fn parse(packet: &[u8]) -> Option<Matchup> {
    let len = read_varint(packet, 0);
    let at = usize::try_from(len.length).ok().filter(|&n| n > 0)?;
    if packet.get(at..at + 2)? != OPCODE {
        return None;
    }
    let kind = [*packet.get(at + 2)?, *packet.get(at + 3)?];
    let count = usize::from(*packet.get(at + 4)?);
    if !(1..=8).contains(&count) {
        return None;
    }
    let servers = (0..count)
        .map(|i| {
            let p = at + 5 + i * 3;
            let server = u16::from_le_bytes([*packet.get(p)?, *packet.get(p + 1)?]);
            let side = *packet.get(p + 2)?;
            ((1000..=2999).contains(&server) && (1..=2).contains(&side)).then_some((server, side))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Matchup { kind, servers })
}

static LATEST: Mutex<Option<Matchup>> = Mutex::new(None);
static SEQ: AtomicU64 = AtomicU64::new(0);

/// The pairing the game just sent; counted only when it changed.
pub fn note(matchup: Matchup) {
    let mut latest = LATEST.lock().unwrap_or_else(|p| p.into_inner());
    if latest.as_ref() != Some(&matchup) {
        tracing::info!("Abyss pairing: {:?} (kind {:02x?})", matchup.servers, matchup.kind);
        *latest = Some(matchup);
        SEQ.fetch_add(1, Ordering::Relaxed);
    }
}

/// The latest pairing, if it changed since `seq`, with the new count.
pub fn since(seq: u64) -> Option<(u64, Matchup)> {
    let now = SEQ.load(Ordering::Relaxed);
    if now == seq {
        return None;
    }
    LATEST.lock().unwrap_or_else(|p| p.into_inner()).clone().map(|m| (now, m))
}

pub fn latest() -> Option<Matchup> {
    LATEST.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn kaisinel_against_triniel_is_read() {
        let m = parse(&hex("0f0a40010102180501ff0802d21b00")).unwrap();
        assert_eq!(m.kind, [1, 1]);
        assert_eq!(m.servers, vec![(1304, 1), (2303, 2)]);
    }

    #[test]
    fn other_frames_and_nonsense_are_not() {
        assert_eq!(parse(&hex("0f0a41010102180501ff0802d21b00")), None, "another opcode");
        assert_eq!(parse(&hex("0f0a40010102090001ff0802d21b00")), None, "server 9 is no server");
        assert_eq!(parse(&hex("0f0a40010102180507ff0802d21b00")), None, "side 7");
        assert_eq!(parse(&hex("060a4001010218")), None, "cut short");
    }
}

//! Dice Roll loot: an item that drops for a party or Force with Dice Roll on
//! is rolled for, and the game says so only in chat, which scrolls away. Read
//! 2026-11-04 from two captures: the user's (Artwork: Black Tentacle Lawa, 6
//! players) and a player's sent log (Dartan Ring, 18 players, one pass).
//!
//! ```text
//! 21 57 <roll id 8> <item id u32> <count u32> <deadline u64 ms>   the roll opens
//! 23 57 00 00 <roll id 8> <roll u32>                               your roll
//! 26 57 <item id u32> <player>  <roll u32>                         another's roll
//! 2B 57 <roll id 8> <item id u32> <player>                         a player passed
//! 28 57 <roll id 8> <item id u32> <player>                         who won it
//!
//! player: <server id u16> <name length u8> <name, UTF-8>
//! ```
//!
//! Little-endian throughout. The highest roll wins (106 to 9361 seen); the
//! winner comes as soon as everyone has rolled or passed, else at the
//! deadline (35 s after the roll opened). Another player's roll names the
//! item but not the roll: two rolls for the same item at once can't be told
//! apart. Your own names the roll but not the item, nor you. The user's
//! capture had no `21 57` for its roll, so a roll can be heard of first from
//! its rolls. A packet is read only when every byte falls into place.

use std::collections::VecDeque;

use serde::Serialize;

/// The most kept for plugins whose window opens later.
const MAX_EVENTS: usize = 500;

/// A player in a roll.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Player {
    pub server_id: u16,
    pub name: String,
}

/// One thing the game said about a roll.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum LootRoll {
    /// An item is up for a roll, until `ends_at` (unix ms).
    #[serde(rename_all = "camelCase")]
    Open { roll_id: String, item_id: u32, count: u32, ends_at: i64 },
    /// Your roll.
    #[serde(rename_all = "camelCase")]
    Mine { roll_id: String, roll: u32 },
    /// Another player's roll.
    #[serde(rename_all = "camelCase")]
    Roll { item_id: u32, player: Player, roll: u32 },
    /// A player passed.
    #[serde(rename_all = "camelCase")]
    Pass { roll_id: String, item_id: u32, player: Player },
    /// Who won the item.
    #[serde(rename_all = "camelCase")]
    Won { roll_id: String, item_id: u32, player: Player },
}

/// A roll event as plugins get it: when it was read, and what it was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LootRollEvent {
    pub at: i64,
    #[serde(flatten)]
    pub roll: LootRoll,
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let bytes = self.data.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(bytes)
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Option<u64> {
        self.take(8).map(|b| u64::from_le_bytes(b.try_into().unwrap_or_default()))
    }
    /// The roll id, as hex: plugins match rolls by it.
    fn roll_id(&mut self) -> Option<String> {
        Some(self.take(8)?.iter().map(|b| format!("{b:02x}")).collect())
    }
    fn player(&mut self) -> Option<Player> {
        let server_id = self.u16()?;
        let len = *self.take(1)?.first()? as usize;
        if len == 0 || len > 48 {
            return None;
        }
        let name = std::str::from_utf8(self.take(len)?).ok()?;
        if name.chars().any(char::is_control) {
            return None;
        }
        Some(Player { server_id, name: name.to_string() })
    }
    fn done(&self) -> bool {
        self.at == self.data.len()
    }
}

/// A roll packet, or None for anything else.
pub fn parse(packet: &[u8]) -> Option<LootRoll> {
    let li = crate::capture::stream_processor::read_varint(packet, 0);
    if li.length <= 0 {
        return None;
    }
    let start = li.length as usize;
    let body = packet.get(start + 2..)?;
    if packet.get(start + 1) != Some(&0x57) {
        return None;
    }
    let mut r = Reader { data: body, at: 0 };
    let roll = match packet[start] {
        0x21 => {
            let roll_id = r.roll_id()?;
            let item_id = r.u32()?;
            let count = r.u32()?;
            let ends_at = i64::try_from(r.u64()?).ok()?;
            LootRoll::Open { roll_id, item_id, count, ends_at }
        }
        0x23 => {
            if r.u16()? != 0 {
                return None;
            }
            let roll_id = r.roll_id()?;
            LootRoll::Mine { roll_id, roll: r.u32()? }
        }
        0x26 => {
            let item_id = r.u32()?;
            let player = r.player()?;
            LootRoll::Roll { item_id, player, roll: r.u32()? }
        }
        0x2B => LootRoll::Pass { roll_id: r.roll_id()?, item_id: r.u32()?, player: r.player()? },
        0x28 => LootRoll::Won { roll_id: r.roll_id()?, item_id: r.u32()?, player: r.player()? },
        _ => return None,
    };
    r.done().then_some(roll)
}

/// The roll events read, numbered, for plugins (as death recaps are).
#[derive(Default)]
pub struct LootRolls {
    events: VecDeque<LootRollEvent>,
    seq: u64,
}

impl LootRolls {
    pub fn push(&mut self, event: LootRollEvent) {
        self.events.push_back(event);
        while self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
        self.seq += 1;
    }

    /// The events read since `seq`, and the count to ask with next time;
    /// None when there are none.
    pub fn since(&self, seq: u64) -> Option<(u64, Vec<LootRollEvent>)> {
        if self.seq == seq {
            return None;
        }
        let new = (self.seq - seq).min(self.events.len() as u64) as usize;
        Some((self.seq, self.events.iter().skip(self.events.len() - new).cloned().collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
    }

    fn player(server_id: u16, name: &str) -> Player {
        Player { server_id, name: name.into() }
    }

    /// The Dartan Ring roll (report GTQ2KK, 2026-10-11): it opened, the
    /// player rolled 8015, Nyxaris 926, Nongkedtk passed, mRaz won.
    #[test]
    fn a_roll_from_open_to_winner() {
        assert_eq!(
            parse(&hex("1E 21 57 8B 55 0F 00 00 ED 00 00 CF 42 7F 12 01 00 00 00 46 AD 53 2B A1 01 00 00")),
            Some(LootRoll::Open { roll_id: "8b550f0000ed0000".into(), item_id: 310330063, count: 1, ends_at: 1791728266566 })
        );
        assert_eq!(
            parse(&hex("14 23 57 00 00 8B 55 0F 00 00 ED 00 00 4F 1F 00 00")),
            Some(LootRoll::Mine { roll_id: "8b550f0000ed0000".into(), roll: 8015 })
        );
        assert_eq!(
            parse(&hex("18 26 57 CF 42 7F 12 DD 05 07 4E 79 78 61 72 69 73 9E 03 00 00")),
            Some(LootRoll::Roll { item_id: 310330063, player: player(1501, "Nyxaris"), roll: 926 })
        );
        assert_eq!(
            parse(&hex("1E 2B 57 8B 55 0F 00 00 ED 00 00 CF 42 7F 12 DD 05 09 4E 6F 6E 67 6B 65 64 74 6B")),
            Some(LootRoll::Pass { roll_id: "8b550f0000ed0000".into(), item_id: 310330063, player: player(1501, "Nongkedtk") })
        );
        assert_eq!(
            parse(&hex("19 28 57 8B 55 0F 00 00 ED 00 00 CF 42 7F 12 DD 05 04 6D 52 61 7A")),
            Some(LootRoll::Won { roll_id: "8b550f0000ed0000".into(), item_id: 310330063, player: player(1501, "mRaz") })
        );
    }

    /// The user's Artwork roll (2026-11-04): T7 rolled 5070 and won.
    #[test]
    fn the_artwork_roll() {
        assert_eq!(
            parse(&hex("13 26 57 BA BF A7 1E 18 05 02 54 37 CE 13 00 00")),
            Some(LootRoll::Roll { item_id: 514310074, player: player(1304, "T7"), roll: 5070 })
        );
        assert_eq!(
            parse(&hex("17 28 57 A6 09 05 00 00 01 00 00 BA BF A7 1E 18 05 02 54 37")),
            Some(LootRoll::Won { roll_id: "a609050000010000".into(), item_id: 514310074, player: player(1304, "T7") })
        );
    }

    /// The same family's other packets (the login's, the instance reward
    /// box's), and a roll a byte short or long, are not read.
    #[test]
    fn other_packets_are_left_alone() {
        assert_eq!(parse(&hex("07 2D 57 00")), None);
        assert_eq!(parse(&hex("09 0F 57 00 00 00")), None);
        assert_eq!(parse(&hex("13 26 57 BA BF A7 1E 18 05 02 54 37 CE 13 00")), None);
        assert_eq!(parse(&hex("13 26 57 BA BF A7 1E 18 05 02 54 37 CE 13 00 00 00")), None);
        assert_eq!(parse(&hex("14 23 57 01 00 8B 55 0F 00 00 ED 00 00 4F 1F 00 00")), None);
    }

    #[test]
    fn plugins_get_each_event_once() {
        let mut rolls = LootRolls::default();
        assert!(rolls.since(0).is_none());
        let event = |roll| LootRollEvent { at: 1, roll: LootRoll::Mine { roll_id: "00".into(), roll } };
        rolls.push(event(1));
        rolls.push(event(2));
        let (seq, got) = rolls.since(0).unwrap();
        assert_eq!(got.len(), 2);
        assert!(rolls.since(seq).is_none());
        rolls.push(event(3));
        assert_eq!(rolls.since(seq).unwrap().1, vec![event(3)]);
    }

    #[test]
    fn plugins_read_it_as_json() {
        let e = LootRollEvent { at: 5, roll: LootRoll::Roll { item_id: 7, player: player(1304, "T7"), roll: 5070 } };
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            serde_json::json!({ "at": 5, "type": "roll", "itemId": 7, "player": { "serverId": 1304, "name": "T7" }, "roll": 5070 })
        );
    }
}

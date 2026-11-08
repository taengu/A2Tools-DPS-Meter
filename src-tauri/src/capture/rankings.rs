//! The ranking tables, as the game sends them when the player opens a
//! ranking tab (`55 8D`, read 2026-11-09 on Kaisinel, EU 1304): the top 500
//! of one table for one class tab (or for all classes, tab 0), of the
//! player's server: on Kaisinel about 70% of each table were Kaisinel's
//! characters, the rest other EU Elyos ids (1301-1323), seemingly characters
//! that came to Kaisinel (2026-11-09).
//!
//! ```text
//! <len varint> 55 8D <u16, 0 so far> <table u8> <class u8> <count varint>
//!   count x: <rank u32> <character id u32> <u16 0> <server u16>
//!            <len varint><name> <len varint><legion> <u16: 1 with a legion>
//!            <level u8> <7 zero bytes> <class u8> <value u64>
//!            <len varint><record, JSON> <rank change i32> <u32 0>
//!            <grade u32> <grade u32, the same again>
//! <next update u64, unix ms> <u32>
//! ```
//!
//! Tables: 1 Abyss, 3 Nightmare, 4 Transcendence, 5 Arena of Solitude,
//! 6 Arena of Co-Operation, 21 Ascension Trial. The value is the table's
//! points (Arena, Abyss), or something else where the record holds the
//! order (Nightmare's best_clear_point). The record says what the table
//! counts: play/win/lose, kill/death/assist, best clears.
//!
//! The next update is the countdown the window shows in its corner: every
//! table opened in one sitting carried the same one, 38 minutes on. A table
//! is the same until then, which is how `ranking_report` tells a table it
//! sent already. The u32 after it differs per table and may be the player's
//! own place: it isn't kept.

/// One place in a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankEntry {
    pub rank: u32,
    pub character_id: u32,
    pub server: u16,
    pub name: String,
    pub legion: String,
    pub level: u8,
    pub class: u8,
    pub value: u64,
    /// What the table counts for this character, as the game's JSON.
    pub record: String,
    /// Places up (+) or down (-) since the last update.
    pub rank_change: i32,
    pub grade: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranking {
    /// The two bytes before the table: 0 so far.
    pub header: u16,
    pub table: u8,
    /// The class tab the table is for: 0 "All" (every class's top 500
    /// together), or a class: 2 Gladiator, 3 Templar, 4 Assassin, 5 Ranger,
    /// 6 Sorcerer, 7 Spiritmaster, 8 Cleric, 9 Chanter (2 and 8 seen, the
    /// rest in the game's class order).
    pub class: u8,
    pub entries: Vec<RankEntry>,
    /// When the game updates the table next (unix ms).
    pub next_update_ms: i64,
}

const OPCODE: [u8; 2] = [0x55, 0x8D];
const MAX_ENTRIES: u64 = 1000;
const TRAILER: usize = 12;
/// Unix ms after 2017 and before 2103: anything else is not a time.
const EARLIEST_MS: i64 = 1_500_000_000_000;
const LATEST_MS: i64 = 4_200_000_000_000;

fn varint(b: &[u8], mut i: usize) -> Option<(u64, usize)> {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let x = *b.get(i)?;
        i += 1;
        v |= u64::from(x & 0x7F) << shift;
        if x & 0x80 == 0 {
            return Some((v, i));
        }
        shift += 7;
        if shift > 35 {
            return None;
        }
    }
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], i: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(i..i + 8)?.try_into().ok()?))
}

/// A length-prefixed UTF-8 string at `i`, and where it ends.
fn text(b: &[u8], i: usize) -> Option<(String, usize)> {
    let (len, start) = varint(b, i)?;
    let end = start.checked_add(usize::try_from(len).ok()?)?;
    Some((std::str::from_utf8(b.get(start..end)?).ok()?.to_owned(), end))
}

/// The entry starting at `i`, and where the next begins.
fn entry(b: &[u8], i: usize) -> Option<(RankEntry, usize)> {
    let rank = u32_at(b, i)?;
    let character_id = u32_at(b, i + 4)?;
    if u16_at(b, i + 8)? != 0 {
        return None;
    }
    let server = u16_at(b, i + 10)?;
    let (name, i) = text(b, i + 12)?;
    let (legion, i) = text(b, i)?;
    let level = *b.get(i + 2)?;
    if b.get(i + 3..i + 10)?.iter().any(|&x| x != 0) {
        return None;
    }
    let class = *b.get(i + 10)?;
    let value = u64_at(b, i + 11)?;
    let (record, i) = text(b, i + 19)?;
    let rank_change = u32_at(b, i)? as i32;
    let grade = u32_at(b, i + 8)?;
    Some((RankEntry { rank, character_id, server, name, legion, level, class, value, record, rank_change, grade }, i + 16))
}

/// The table in a `55 8D` record, or None when it isn't one or doesn't read
/// cleanly to its end.
pub fn parse(packet: &[u8]) -> Option<Ranking> {
    let (_, mut i) = varint(packet, 0)?;
    if packet.get(i..i + 2)? != OPCODE {
        return None;
    }
    i += 2;
    let header = u16_at(packet, i)?;
    let table = *packet.get(i + 2)?;
    let class = *packet.get(i + 3)?;
    let (count, next) = varint(packet, i + 4)?;
    if count > MAX_ENTRIES {
        return None;
    }
    i = next;
    let mut entries = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let (e, next) = entry(packet, i)?;
        // Places only go down the table (a tie shares one).
        if e.rank == 0 || entries.last().is_some_and(|p: &RankEntry| p.rank > e.rank) {
            return None;
        }
        entries.push(e);
        i = next;
    }
    if packet.len() != i + TRAILER {
        return None;
    }
    let next_update_ms = i64::try_from(u64_at(packet, i)?).ok()?;
    if !(EARLIEST_MS..LATEST_MS).contains(&next_update_ms) {
        return None;
    }
    Some(Ranking { header, table, class, entries, next_update_ms })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn the_abyss_table_reads_to_its_end() {
        // The first three of Kaisinel's Abyss top 500 (the count cut to 3).
        let r = parse(&hex(include_str!("testdata/ranking_abyss_top3.hex"))).unwrap();
        assert_eq!((r.header, r.table, r.class, r.entries.len()), (0, 1, 8, 3));
        assert_eq!(r.next_update_ms, 1_794_158_072_585);
        let dak = &r.entries[0];
        assert_eq!((dak.rank, dak.server, dak.name.as_str(), dak.legion.as_str()), (1, 1305, "Dak", "MANDURI"));
        assert_eq!((dak.level, dak.class), (45, 8));
        assert!(dak.record.starts_with("{\"kill_count\":"));
        assert!(r.entries.windows(2).all(|w| w[0].rank < w[1].rank));
    }

    #[test]
    fn other_records_and_damaged_tables_are_not_tables() {
        assert!(parse(&hex("05 1b 92 01 02")).is_none());
        let whole = hex(include_str!("testdata/ranking_abyss_top3.hex"));
        let mut cut = whole.clone();
        cut.truncate(whole.len() - 5);
        assert!(parse(&cut).is_none());
        let mut longer = whole;
        longer.push(0);
        assert!(parse(&longer).is_none());
    }
}

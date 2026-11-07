//! The field bosses on a map, as the game sends them when the player opens it
//! (`01 91`, 2026-10-15): which are up, and when the dead ones come back.
//!
//! ```text
//! <len varint> 01 91 <u16, 0 so far> <map id u32> <count varint>
//!   count x: <up u8: 0 or 1> <world-map boss id varint> [<x y z f32> when up]
//!            [<one byte some bosses carry>] <time u64, unix ms>
//! ```
//!
//! The time is when an up boss appeared, or when a dead one returns; 0 when
//! the game gives none. The Abyss's scheduled bosses carry 0 or their next
//! scheduled spawn, which can be days away (2006-2008 were five days out on
//! 2026-11-08), so a time up to AHEAD_MS ahead is believed. The boss id is the
//! game's WorldMapFieldNamed id (101006: Bodyguard Teegant in Verteron).
//!
//! Some bosses carry one more byte before the time, others never do, and its
//! meaning is unknown; so the time is read where it makes sense: right after
//! the id (or the position), or one byte later, whichever is a plausible
//! time (or 0) and leaves the next entry starting as an entry does.
//!
//! Only the home map and the Abyss come this way: the enemy side's map, viewed
//! from the map's menu, sends nothing.

/// A boss on the map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BossOnMap {
    /// The game's WorldMapFieldNamed id.
    pub id: u32,
    pub up: bool,
    /// Unix ms: since when it is up, or when it returns; 0 when not given.
    pub time_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BossMap {
    pub map_id: u32,
    /// The two bytes before the map id: 0 so far, perhaps the server when an
    /// enemy map is viewed from a rift; reports only go out when it is 0.
    pub header: u16,
    pub bosses: Vec<BossOnMap>,
    /// When the meter read it (unix ms).
    pub seen_ms: i64,
}

const OPCODE: [u8; 2] = [0x01, 0x91];
/// A time this far from now isn't one (a misread). Ahead, a week and a day:
/// the Abyss's weekly bosses give their next spawn, up to a week away. Three
/// days failed the whole Abyss map once 2006-2008 were five days out
/// (2026-11-08), and no map 20 was reported from then on.
const PAST_MS: i64 = 14 * 24 * 3600 * 1000;
const AHEAD_MS: i64 = 8 * 24 * 3600 * 1000;

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

fn u64_at(b: &[u8], i: usize) -> Option<i64> {
    let bytes: [u8; 8] = b.get(i..i + 8)?.try_into().ok()?;
    i64::try_from(u64::from_le_bytes(bytes)).ok()
}

/// Whether an entry could start at `i` (or the list ends there).
fn entry_starts(b: &[u8], i: usize, last: bool) -> bool {
    if last {
        return i <= b.len();
    }
    matches!(b.get(i), Some(0) | Some(1))
        && varint(b, i + 1).is_some_and(|(id, _)| (1_000..10_000_000).contains(&id))
}

/// The map in a `01 91` record, or None when it isn't one or doesn't read
/// cleanly. `now_ms` bounds what counts as a time.
pub fn parse(packet: &[u8], now_ms: i64) -> Option<BossMap> {
    let (_, mut i) = varint(packet, 0)?;
    if packet.get(i..i + 2)? != OPCODE {
        return None;
    }
    i += 2;
    let header = u16::from_le_bytes(packet.get(i..i + 2)?.try_into().ok()?);
    i += 2;
    let map_id = u32::from_le_bytes(packet.get(i..i + 4)?.try_into().ok()?);
    i += 4;
    let (count, next) = varint(packet, i)?;
    i = next;
    if count == 0 || count > 200 {
        return None;
    }
    let plausible = |t: i64| t == 0 || (now_ms - PAST_MS..=now_ms + AHEAD_MS).contains(&t);
    let mut bosses = Vec::with_capacity(count as usize);
    for n in 0..count {
        let up = match *packet.get(i)? {
            0 => false,
            1 => true,
            _ => return None,
        };
        let (id, after_id) = varint(packet, i + 1)?;
        let base = after_id + if up { 12 } else { 0 };
        let last = n + 1 == count;
        let (time_ms, end) = [0usize, 1].iter().find_map(|&extra| {
            let t = u64_at(packet, base + extra)?;
            let end = base + extra + 8;
            (plausible(t) && entry_starts(packet, end, last)).then_some((t, end))
        })?;
        bosses.push(BossOnMap { id: u32::try_from(id).ok()?, up, time_ms });
        i = end;
    }
    Some(BossMap { map_id, header, bosses, seen_ms: now_ms })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s = s.trim();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn verteron_lists_every_boss_with_who_is_up_and_when_the_dead_return() {
        let now = 1_791_992_733_074;
        let map = parse(&hex(include_str!("testdata/boss_map_v1.hex")), now).unwrap();
        assert_eq!((map.map_id, map.header, map.bosses.len()), (1010, 0, 24));
        let by_id = |id| map.bosses.iter().find(|b| b.id == id).unwrap();
        // Blooming Korin carries the extra byte and a position: up since 23:44:13.
        let korin = by_id(101_007);
        assert!(korin.up && ((now - korin.time_ms) / 1000 - 80).abs() <= 2);
        // Kernon of the West, up, no extra byte.
        assert!(by_id(101_003).up);
        // Rotten Kutar: dead, back 1.7 minutes later.
        let kutar = by_id(101_002);
        assert!(!kutar.up && ((kutar.time_ms - now) / 1000 - 104).abs() <= 2);
        assert_eq!(map.bosses.iter().filter(|b| b.up).count(), 4);
    }

    #[test]
    fn a_boss_due_back_is_up_since_exactly_then() {
        // Bodyguard Teegant: due 23:56:40 in the first map, up since then in this one.
        let first = parse(&hex(include_str!("testdata/boss_map_v1.hex")), 1_791_992_733_074).unwrap();
        let later = parse(&hex(include_str!("testdata/boss_map_v2.hex")), 1_791_993_638_495).unwrap();
        let due = first.bosses.iter().find(|b| b.id == 101_006).unwrap();
        let now = later.bosses.iter().find(|b| b.id == 101_006).unwrap();
        assert!(!due.up && now.up && due.time_ms == now.time_ms);
    }

    #[test]
    fn the_abyss_scheduled_bosses_come_without_a_time() {
        let map = parse(&hex(include_str!("testdata/boss_map_abyss.hex")), 1_791_993_086_569).unwrap();
        assert_eq!((map.map_id, map.bosses.len()), (20, 8));
        // Argo, the Spirit King (the extra byte), back in seven hours.
        assert!(map.bosses[0].id == 2001 && map.bosses[0].time_ms > 1_791_993_086_569);
        assert!(map.bosses[1..].iter().all(|b| b.time_ms == 0 && !b.up));
    }

    #[test]
    fn the_abyss_weekly_bosses_may_be_days_away() {
        // 2026-11-08 05:12 (UTC+8): Argo dead with no time, 2003-2005 due that
        // evening, 2006-2008 five days out; the first entry carries the extra byte.
        let now = 1_794_085_953_000;
        let map = parse(&hex(include_str!("testdata/boss_map_abyss_week.hex")), now).unwrap();
        assert_eq!((map.map_id, map.header, map.bosses.len()), (20, 0, 8));
        let ids: Vec<u32> = map.bosses.iter().map(|b| b.id).collect();
        assert_eq!(ids, [2004, 2003, 2001, 2005, 2002, 2006, 2007, 2008]);
        assert!(map.bosses.iter().all(|b| !b.up));
        assert_eq!(map.bosses[2].time_ms, 0);
        assert_eq!(map.bosses[0].time_ms, 1_794_168_300_000);
        assert!(map.bosses[5..].iter().all(|b| b.time_ms == 1_794_515_700_000));
        // Minutes earlier: 2002 up (a position, no time) and the extra byte 0x10.
        let up = "7501910000140000000800d40f10e0a1c3bca101000000d30fe0a1c3bca101000000d10f\
                  000000000000000000d50fe0a1c3bca101000001d20f5fe2fe468d3937c6009c28460000\
                  00000000000000d60f208978d1a101000000d70f208978d1a101000000d80f208978d1a1010000000000";
        let up: String = up.split_whitespace().collect();
        let map = parse(&hex(&up), now).unwrap();
        assert_eq!(map.bosses.len(), 8);
        assert!(map.bosses[4].id == 2002 && map.bosses[4].up && map.bosses[4].time_ms == 0);
        assert_eq!(map.bosses.iter().filter(|b| b.up).count(), 1);
    }

    #[test]
    fn other_records_and_damaged_ones_are_not_maps() {
        assert!(parse(&hex("05 1b 92 01 02".replace(' ', "").as_str()), 0).is_none());
        let mut cut = hex(include_str!("testdata/boss_map_v1.hex"));
        cut.truncate(120);
        assert!(parse(&cut, 1_791_992_733_074).is_none());
    }
}

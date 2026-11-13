//! A timed content's reward cards: the five a Dimensional Invasion shows at
//! its end, of which the player's tokens pick what they get. Sent once as the
//! cards come up, with every card's items, picked or not: what could have
//! been won, the fair sample for the items' odds (`reward_report` pools it
//! across players). Read 2026-11-13 from two invasions (Global, Elyos).
//!
//! ```text
//! 0A 57 00 00 <u32> <u32> <u32> <cards u8>
//!   card: <number u8> <bonus n u8> (<item u32> <count u32>)* <n u8> (<item u32> <count u32>)*
//!         <pickers u8> (<server u16> 00 <character u64> <name len u8> <name> <level u32> <tokens u32> <5 bytes>)*
//! ```
//!
//! Little-endian. The bonus section is on one or two cards a run (the
//! "jackpot": a piece of gear, Soul Codexes, Enhance Stones ×1200, Abyss
//! Points ×800), the rest of a card's items follow. One byte of unknown use
//! comes before the second card (0xeb and 0x7b in the two runs seen); a card
//! may follow up to 4 such bytes, its header found by its number, a count
//! and an item. Pickers name players: they are read only for the tokens
//! (`Card::tokens`), never kept or sent.

use serde::Serialize;

/// One card: its bonus items and its items, as (item id, count).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    pub bonus: Vec<(u32, u32)>,
    pub items: Vec<(u32, u32)>,
    /// The tokens put on it, everyone's (no names).
    pub tokens: u32,
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn item_ok(item: u32, count: u32) -> bool {
    (100_000_000..1_000_000_000).contains(&item) && count > 0
}

/// A card's header at `p`: its number, a bonus count, and (when it has
/// bonus items) an item after it.
fn header_at(b: &[u8], p: usize, number: u8) -> bool {
    match (b.get(p), b.get(p + 1)) {
        (Some(&n), Some(&bonus)) if n == number && bonus < 32 => {
            if bonus > 0 {
                matches!((u32_at(b, p + 2), u32_at(b, p + 6)), (Some(i), Some(c)) if item_ok(i, c))
            } else {
                b.get(p + 2).is_some_and(|&n| n > 0 && n < 64)
            }
        }
        _ => false,
    }
}

/// The cards an `0A 57` record shows, or None when it isn't one or they
/// don't all fall into place.
pub fn parse(packet: &[u8]) -> Option<Vec<Card>> {
    let len = super::stream_processor::read_varint(packet, 0);
    let at = usize::try_from(len.length).ok().filter(|&n| n > 0)?;
    if packet.get(at..at + 4)? != [0x0A, 0x57, 0, 0] {
        return None;
    }
    let b = &packet[at..];
    let count = *b.get(16)? as usize;
    if count == 0 || count > 10 {
        return None;
    }
    let items = |p: &mut usize| -> Option<Vec<(u32, u32)>> {
        let n = *b.get(*p)? as usize;
        *p += 1;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let (item, c) = (u32_at(b, *p)?, u32_at(b, *p + 4)?);
            item_ok(item, c).then_some(())?;
            out.push((item, c));
            *p += 8;
        }
        Some(out)
    };
    let mut p = 17;
    let mut cards = Vec::with_capacity(count);
    for number in 0..count as u8 {
        p = (p..p + 5).find(|&q| header_at(b, q, number))? + 1;
        let bonus = items(&mut p)?;
        let card_items = items(&mut p)?;
        let pickers = *b.get(p)? as usize;
        p += 1;
        let mut tokens = 0;
        for _ in 0..pickers {
            // <server u16> 00 <character u64> <name> <level u32> <tokens u32> <5 bytes>
            let name_len = *b.get(p + 11)? as usize;
            let after_name = p + 12 + name_len;
            tokens += u32_at(b, after_name + 4)?;
            p = after_name + 8 + 5;
            b.get(p - 1)?;
        }
        cards.push(Card { bonus, items: card_items, tokens });
    }
    Some(cards)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// An invasion of 2026-11-13 (23:34, Global): five cards, a bonus on the
    /// first (seven items) and the fourth (one), two tokens on the second and
    /// one on the third; the player's name and id masked.
    #[test]
    fn an_invasions_five_cards_are_read() {
        let cards = parse(&hex(include_str!("testdata/reward_cards_invasion.hex"))).expect("read");
        let shape: Vec<(usize, usize, u32)> = cards.iter().map(|c| (c.bonus.len(), c.items.len(), c.tokens)).collect();
        assert_eq!(shape, vec![(7, 12, 0), (0, 12, 2), (0, 13, 1), (1, 13, 0), (0, 13, 0)]);
        assert_eq!(cards[0].bonus[2], (930_100_023, 1200), "Enhance Stones ×1200 in the bonus");
        assert_eq!(cards[0].bonus[6], (930_100_003, 800), "Abyss Points ×800");
        assert_eq!(cards[3].bonus, vec![(115_030_069, 1)], "Holy Spirit Guard");
        assert_eq!(cards[4].items.last(), Some(&(930_100_011, 20)), "every card ends with Centuryroot Tokens ×20");
    }

    /// The invasion of 19:32 the same day: its bonus on the fourth card (one
    /// item) and the fifth (seven), three tokens on the second.
    #[test]
    fn another_invasions_cards_are_read() {
        let cards = parse(&hex(include_str!("testdata/reward_cards_invasion_2.hex"))).expect("read");
        let shape: Vec<(usize, usize, u32)> = cards.iter().map(|c| (c.bonus.len(), c.items.len(), c.tokens)).collect();
        assert_eq!(shape, vec![(0, 12, 0), (0, 12, 3), (0, 13, 0), (1, 13, 0), (7, 13, 0)]);
    }

    #[test]
    fn a_cut_record_is_not_read() {
        let mut p = hex(include_str!("testdata/reward_cards_invasion.hex"));
        p.truncate(400);
        assert_eq!(parse(&p), None);
    }
}

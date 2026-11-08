//! The Black Cloud Merchants' stock, as the game sends it each time a tab of
//! the shop is opened (`91 56`, read 2026-11-09 on Kaisinel): what is on sale
//! until the next refresh, on the hour.
//!
//! ```text
//! <len varint> 91 56 <u8, 0x65 Special and 0x68 Pets seen> <count varint>
//!   count x: <goods u32> <item u32> <count u64> <discount tier u8>
//!            <currency u8> <price before u64> <price u64>
//! <next refresh u64, unix ms> <u8>
//! ```
//!
//! - `goods` is the game's RandomShopGoods id, `item` its Item id (a pet's
//!   item says which pet it gives).
//! - The discount tier: 0 is 10% off, 1 20%, 2 30%, 3 50%.
//! - **A byte between some entries** (0x55 before the 2nd, 6th and 10th,
//!   0x05 before the 10th in another): an entry is read where it reads
//!   cleanly, at once or one byte on, as with the Supply Requests boards.

use std::collections::VecDeque;

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Goods {
    pub goods_id: u32,
    pub item_id: u32,
    pub count: u64,
    pub tier: u8,
    pub currency: u8,
    pub original_price: u64,
    pub price: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlackMarket {
    /// The byte before the list (which tab, it seems).
    pub kind: u8,
    pub goods: Vec<Goods>,
    /// When the stock changes next (unix ms).
    pub refresh_at: i64,
}

const OPCODE: [u8; 2] = [0x91, 0x56];
const ENTRY: usize = 34;
const MAX_GOODS: u64 = 64;
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

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], i: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(i..i + 8)?.try_into().ok()?))
}

/// The percent off of each discount tier.
const TIER_OFF: [u64; 4] = [10, 20, 30, 50];

/// The goods at `i`, when they read as goods. `strict`: only if the price is
/// the price before less the tier's discount, which tells an entry from the
/// same bytes read one off (a stray byte read as the entry's first).
fn goods(b: &[u8], i: usize, strict: bool) -> Option<Goods> {
    let g = Goods {
        goods_id: u32_at(b, i)?,
        item_id: u32_at(b, i + 4)?,
        count: u64_at(b, i + 8)?,
        tier: *b.get(i + 16)?,
        currency: *b.get(i + 17)?,
        original_price: u64_at(b, i + 18)?,
        price: u64_at(b, i + 26)?,
    };
    if strict {
        let off = *TIER_OFF.get(usize::from(g.tier))?;
        let matches = (1..=99).contains(&g.count) && g.price * 100 == g.original_price * (100 - off);
        return matches.then_some(g);
    }
    let plausible = (1..=9_999).contains(&g.count)
        && g.tier <= 7
        && g.currency != 0
        && g.price > 0
        && g.price <= g.original_price
        && g.original_price < 1_000_000_000_000;
    plausible.then_some(g)
}

fn time_at(b: &[u8], i: usize) -> Option<i64> {
    let t = i64::try_from(u64_at(b, i)?).ok()?;
    (EARLIEST_MS..LATEST_MS).contains(&t).then_some(t)
}

/// The stock in a `91 56` record, or None when it isn't one or doesn't read
/// cleanly.
pub fn parse(packet: &[u8]) -> Option<BlackMarket> {
    let (_, mut i) = varint(packet, 0)?;
    if packet.get(i..i + 2)? != OPCODE {
        return None;
    }
    i += 2;
    let kind = *packet.get(i)?;
    let (count, next) = varint(packet, i + 1)?;
    if count == 0 || count > MAX_GOODS {
        return None;
    }
    i = next;
    let mut list = Vec::with_capacity(count as usize);
    for _ in 0..count {
        // As the discount says, at once or one byte on; failing that (a tier
        // not seen yet), whatever reads.
        let (g, at) = [(0usize, true), (1, true), (0, false), (1, false)]
            .iter()
            .find_map(|&(skip, strict)| goods(packet, i + skip, strict).map(|g| (g, i + skip)))?;
        list.push(g);
        i = at + ENTRY;
    }
    let refresh_at = time_at(packet, i).or_else(|| time_at(packet, i + 1))?;
    Some(BlackMarket { kind, goods: list, refresh_at })
}

/// A stock read, and when (unix ms).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlackMarketEvent {
    pub at: i64,
    #[serde(flatten)]
    pub market: BlackMarket,
}

const MAX_EVENTS: usize = 8;

/// The stocks read, numbered, for plugins (as Dice Roll loot is).
#[derive(Default)]
pub struct BlackMarkets {
    events: VecDeque<BlackMarketEvent>,
    seq: u64,
}

impl BlackMarkets {
    pub fn push(&mut self, event: BlackMarketEvent) {
        self.events.push_back(event);
        while self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
        self.seq += 1;
    }

    /// The stocks read since `seq`, and the count to ask with next time;
    /// None when there are none.
    pub fn since(&self, seq: u64) -> Option<(u64, Vec<BlackMarketEvent>)> {
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

    /// The Pets tab on Kaisinel, 2026-11-09 02:03: ten pets, a byte before
    /// the 2nd, 6th and 10th, the next refresh at 03:00 (UTC+8).
    const PETS: &str = "eb 02 91 56 68 0a \
        43 d5 01 00 ed ca df 1f 01 00 00 00 00 00 00 00 03 5d a0 86 01 00 00 00 00 00 50 c3 00 00 00 00 00 00 \
        55 7d d5 01 00 10 cb df 1f 01 00 00 00 00 00 00 00 00 5d a0 86 01 00 00 00 00 00 90 5f 01 00 00 00 00 00 \
        84 d5 01 00 18 cb df 1f 01 00 00 00 00 00 00 00 00 5d a0 86 01 00 00 00 00 00 90 5f 01 00 00 00 00 00 \
        06 d5 01 00 8c ca df 1f 01 00 00 00 00 00 00 00 01 5d a0 86 01 00 00 00 00 00 80 38 01 00 00 00 00 00 \
        1b d5 01 00 a2 ca df 1f 01 00 00 00 00 00 00 00 03 5d a0 86 01 00 00 00 00 00 50 c3 00 00 00 00 00 00 \
        55 17 d5 01 00 9e ca df 1f 01 00 00 00 00 00 00 00 02 5d a0 86 01 00 00 00 00 00 70 11 01 00 00 00 00 00 \
        73 d5 01 00 fe ca df 1f 01 00 00 00 00 00 00 00 00 5d a0 86 01 00 00 00 00 00 90 5f 01 00 00 00 00 00 \
        fa d4 01 00 7e ca df 1f 01 00 00 00 00 00 00 00 02 5d a0 86 01 00 00 00 00 00 70 11 01 00 00 00 00 00 \
        cd d4 01 00 4d ca df 1f 01 00 00 00 00 00 00 00 02 5d a0 86 01 00 00 00 00 00 70 11 01 00 00 00 00 00 \
        05 27 d5 01 00 53 ca df 1f 01 00 00 00 00 00 00 00 01 5d a0 86 01 00 00 00 00 00 80 38 01 00 00 00 00 00 \
        80 1f 88 bc a1 01 00 00 00";

    #[test]
    fn the_pets_tab_reads_with_its_prices_and_refresh() {
        let m = parse(&hex(PETS)).unwrap();
        assert_eq!((m.kind, m.goods.len()), (0x68, 10));
        assert_eq!(m.refresh_at, 1_794_164_400_000);
        // Drakan Guard (pet 1142): 100,000 at 50% off.
        let first = &m.goods[0];
        assert_eq!((first.goods_id, first.item_id, first.count), (120_131, 534_760_173, 1));
        assert_eq!((first.tier, first.original_price, first.price), (3, 100_000, 50_000));
        let prices: Vec<u64> = m.goods.iter().map(|g| g.price).collect();
        assert_eq!(prices, [50_000, 90_000, 90_000, 80_000, 50_000, 70_000, 90_000, 70_000, 70_000, 80_000]);
    }

    #[test]
    fn other_records_and_damaged_ones_are_not_stock() {
        assert!(parse(&hex("05 1b 92 01 02")).is_none());
        let mut cut = hex(PETS);
        cut.truncate(200);
        assert!(parse(&cut).is_none());
    }
}

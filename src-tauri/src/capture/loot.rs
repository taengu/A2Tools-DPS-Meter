//! Loot: the items you gain and what you gather, for plugins (Shugo's
//! Ledger's drop rates). Read 2026-11-12 from Global captures: gathering
//! Aria and Targena, and the Shugo Festival's rewards.
//!
//! ```text
//! 1B 56 <n u8> (<entry>)*                                   items gained
//!   entry: <change u8> <u8> <instance u32> <4 bytes> <item id u32> <stack total u64> <rest>
//! 31 8D 00 00 <node varint> <gathers u32> ...               a gather begins
//! 31 8D 04 3a <node varint> 00...                           refused (someone is on it)
//! 33 8D 00 3a <node varint> <item u32> <count u32> <bonus u32> <extra item u32> <extra count u32>
//!                                                           a gather's result
//! 1E 56 <n u8> (<drop>)* <u8>                               loot dropped
//!   drop: <object u32> <from x y z f32> <to x y z f32> <source entity u32> <u32>
//!         <item u32> <count u32> <owner kind u8> <owner id u64> <time u64 ms> 01 03
//! ```
//!
//! A drop is 63 bytes. Its source is the entity it fell from (a monster, the
//! festival's reward giver); its owner, kind 2, a character's id (the form
//! the login record gives yours, and the roster each member's): every drop
//! in sight comes, everyone's, and in a party each member gets drops of
//! their own (one record held two, from two monsters, for two members).
//! Owner kinds 1 and 5 are rarer, not understood. 5,004 records of
//! 2026-11-12 all read: 4,996 of one drop, the rest of 2 or 4.
//!
//! Little-endian. An item entry's change is 0 for an item that took a new
//! place (gear, a first stack), 1 for a stack that changed, 2 for one used
//! up (its total 0); its total is the stack after the change, not the
//! change. In 799 records of 2026-11-12: 625, 130, 36 and 8 entries led
//! with 1, 0, 2 and 1 again with its second byte 1. Its rest is 15 to 17 bytes for a
//! stack and about 90 for gear (its stats), which varies, so entries are found
//! by their heads: a game item id is nine digits (100000000 to 999999999),
//! the instance is not 0 and its top byte is, two of the four bytes are 0,
//! and the stack is at least 1. A gather's
//! `bonus` is the extra copies of its item (the stack grew by 2 when it was
//! 1); its extra item is the random add-on (a Life Crystal from Aria, a
//! Poko's Skin Bundle from most nodes), 0 when none.

use std::collections::{HashMap, VecDeque};

use serde::Serialize;

/// The most kept for plugins whose window opens later.
const MAX_EVENTS: usize = 500;

/// What happened to an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Change {
    New,
    Changed,
    Removed,
}

/// One item entry, as read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemEntry {
    pub item: u32,
    pub total: u64,
    pub change: Change,
}

/// What a gather did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Gather {
    /// A gather began on `node` (its `n`th on that node).
    #[serde(rename_all = "camelCase")]
    Started { node: u32, n: u32 },
    /// The node was taken: someone else is gathering it.
    #[serde(rename_all = "camelCase")]
    Refused { node: u32 },
    /// The gather's result.
    #[serde(rename_all = "camelCase")]
    Done { node: u32, item: u32, count: u32, bonus: u32, extra_item: u32, extra_count: u32 },
}

/// One item entry, as plugins get it: `gained` when known (a new item's
/// total; a stack seen before, how much it grew; 0 for one used up).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemGained {
    pub item: u32,
    pub total: u64,
    pub change: Change,
    pub gained: Option<u64>,
}

/// One drop, as read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Drop {
    pub source: u32,
    pub item: u32,
    pub count: u32,
    pub owner_kind: u8,
    pub owner: u64,
}

/// Whose a drop is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Owner {
    You,
    Party,
    Other,
}

/// A drop as plugins get it: what fell from which monster (its NPC code
/// when its spawn was seen), and whose it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DropSeen {
    pub source: u32,
    pub mob: Option<i32>,
    pub item: u32,
    pub count: u32,
    pub owner: Owner,
}

/// Where the player was.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Whereabouts {
    /// The map the last load named, 0 when none.
    pub map: i32,
    /// The dungeon, 0 when none (`DataStorage::current_dungeon_id`).
    pub dungeon: i32,
    /// The timed content the game last stated (`00 61`), and its state.
    pub content: u32,
    pub content_state: u8,
}

/// A loot event as plugins get it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LootEvent {
    /// Items gained at once (a pickup, a reward, a purchase).
    #[serde(rename_all = "camelCase")]
    Items { at: i64, items: Vec<ItemGained>, at_place: Whereabouts },
    /// A gather.
    #[serde(rename_all = "camelCase")]
    Gather { at: i64, gather: Gather, at_place: Whereabouts },
    /// Loot dropped in sight.
    #[serde(rename_all = "camelCase")]
    Drops { at: i64, drops: Vec<DropSeen>, at_place: Whereabouts },
    /// A monster died in sight: `by_you` when you (or your summons) hit it,
    /// `by_party` when a party member did.
    #[serde(rename_all = "camelCase")]
    Kill { at: i64, entity: i32, mob: i32, by_you: bool, by_party: bool, at_place: Whereabouts },
    /// The reward cards a timed content (the Dimensional Invasion, the Shugo
    /// Festival) shows at its end: every card's items, picked or not
    /// (`reward_cards`, no names).
    #[serde(rename_all = "camelCase")]
    Cards { at: i64, cards: Vec<super::reward_cards::Card>, at_place: Whereabouts },
}

impl LootEvent {
    pub fn at(&self) -> i64 {
        match self {
            LootEvent::Items { at, .. }
            | LootEvent::Gather { at, .. }
            | LootEvent::Drops { at, .. }
            | LootEvent::Kill { at, .. }
            | LootEvent::Cards { at, .. } => *at,
        }
    }
}

fn body<'a>(packet: &'a [u8], op: [u8; 2]) -> Option<&'a [u8]> {
    let len = super::stream_processor::read_varint(packet, 0);
    let at = usize::try_from(len.length).ok().filter(|&n| n > 0)?;
    (packet.get(at..at + 2)? == op).then(|| &packet[at + 2..])
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// An entry's head starts at `at`: its fields fall into place.
fn head_at(b: &[u8], at: usize) -> Option<ItemEntry> {
    let h = b.get(at..at + 22)?;
    let item = u32_at(h, 10)?;
    let instance = u32_at(h, 2)?;
    let total = u64_at(h, 14)?;
    let change = match h[0] {
        0 => Change::New,
        1 => Change::Changed,
        2 => Change::Removed,
        _ => return None,
    };
    // A gear entry's stats hold runs that look like a head but for these:
    // an instance is never 0, nor a stack still held.
    let stack_ok = if change == Change::Removed { total == 0 } else { (1..1 << 40).contains(&total) };
    let ok = h[1] <= 1
        && instance != 0
        && h[5] == 0
        && h[6] == 0
        && h[8] == 0
        && h[9] == 0
        && (100_000_000..1_000_000_000).contains(&item)
        && stack_ok;
    ok.then_some(ItemEntry { item, total, change })
}

/// The items a `1B 56` record gained, or None when it isn't one or its
/// entries don't all fall into place.
pub fn item_entries(packet: &[u8]) -> Option<Vec<ItemEntry>> {
    let b = body(packet, [0x1B, 0x56])?;
    let n = *b.first()? as usize;
    let mut at = 1;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        out.push(head_at(b, at)?);
        if i + 1 == n {
            break;
        }
        // The next head, past this one's rest (15 bytes at least).
        at = (at + 22 + 15..b.len()).find(|&p| head_at(b, p).is_some())?;
    }
    Some(out)
}

/// The drops a `1E 56` record holds, or None when it isn't one or they don't
/// all fall into place.
pub fn drops(packet: &[u8]) -> Option<Vec<Drop>> {
    const DROP: usize = 63;
    let b = body(packet, [0x1E, 0x56])?;
    let n = *b.first()? as usize;
    if n == 0 || b.len() != 1 + n * DROP + 1 {
        return None;
    }
    (0..n)
        .map(|i| {
            let d = &b[1 + i * DROP..1 + (i + 1) * DROP];
            let item = u32_at(d, 36)?;
            (100_000_000..1_000_000_000).contains(&item).then_some(())?;
            Some(Drop { source: u32_at(d, 28)?, item, count: u32_at(d, 40)?, owner_kind: d[44], owner: u64_at(d, 45)? })
        })
        .collect()
}

/// A gather record, or None.
pub fn gather(packet: &[u8]) -> Option<Gather> {
    use super::stream_processor::read_varint;
    if let Some(b) = body(packet, [0x31, 0x8D]) {
        let (flag, mark) = (*b.first()?, *b.get(1)?);
        let node = read_varint(b, 2);
        let after = usize::try_from(node.length).ok().filter(|&n| n > 0)? + 2;
        return match (flag, mark) {
            (0, 0) => Some(Gather::Started { node: node.value as u32, n: u32_at(b, after)? }),
            (4, 0x3a) => Some(Gather::Refused { node: node.value as u32 }),
            _ => None,
        };
    }
    let b = body(packet, [0x33, 0x8D])?;
    if b.get(..2)? != [0x00, 0x3a] {
        return None;
    }
    let node = read_varint(b, 2);
    let at = usize::try_from(node.length).ok().filter(|&n| n > 0)? + 2;
    let item = u32_at(b, at)?;
    (100_000_000..1_000_000_000).contains(&item).then_some(())?;
    Some(Gather::Done {
        node: node.value as u32,
        item,
        count: u32_at(b, at + 4)?,
        bonus: u32_at(b, at + 8)?,
        extra_item: u32_at(b, at + 12)?,
        extra_count: u32_at(b, at + 16)?,
    })
}

/// The loot events read, numbered, for plugins (as loot rolls are), and the
/// last stack total of each item seen, for what a change gained.
/// A run's reward cards read again this soon are the same run's.
const SAME_CARDS_MS: i64 = 120_000;

#[derive(Default)]
pub struct Loot {
    events: VecDeque<LootEvent>,
    seq: u64,
    totals: HashMap<u32, u64>,
}

impl Loot {
    /// Items read at `at`, `place` being where the player was.
    pub fn items(&mut self, at: i64, entries: &[ItemEntry], place: Whereabouts) {
        let items = entries
            .iter()
            .map(|e| {
                let before = self.totals.insert(e.item, e.total);
                let gained = match e.change {
                    Change::New => Some(e.total),
                    Change::Changed => before.map(|b| e.total.saturating_sub(b)),
                    Change::Removed => Some(0),
                };
                ItemGained { item: e.item, total: e.total, change: e.change, gained }
            })
            .collect();
        self.push(LootEvent::Items { at, items, at_place: place });
    }

    pub fn gather(&mut self, at: i64, gather: Gather, place: Whereabouts) {
        self.push(LootEvent::Gather { at, gather, at_place: place });
    }

    pub fn drops(&mut self, at: i64, drops: Vec<DropSeen>, place: Whereabouts) {
        self.push(LootEvent::Drops { at, drops, at_place: place });
    }

    pub fn kill(&mut self, at: i64, entity: i32, mob: i32, by_you: bool, by_party: bool, place: Whereabouts) {
        self.push(LootEvent::Kill { at, entity, mob, by_you, by_party, at_place: place });
    }

    /// A run's reward cards. The invasion sends its record twice: the same
    /// cards again within `SAME_CARDS_MS` are that, not another run.
    pub fn cards(&mut self, at: i64, cards: Vec<super::reward_cards::Card>, place: Whereabouts) -> bool {
        let repeat = self.events.iter().rev().any(|e| match e {
            LootEvent::Cards { at: when, cards: seen, .. } => at - when < SAME_CARDS_MS && *seen == cards,
            _ => false,
        });
        if repeat {
            return false;
        }
        self.push(LootEvent::Cards { at, cards, at_place: place });
        true
    }

    fn push(&mut self, event: LootEvent) {
        self.events.push_back(event);
        while self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
        self.seq += 1;
    }

    /// The events read since `seq`, and the count to ask with next time;
    /// None when there are none.
    pub fn since(&self, seq: u64) -> Option<(u64, Vec<LootEvent>)> {
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
        let s: String = s.split_whitespace().collect();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// One Aria picked up from a gather (2026-11-12): its stack now 90.
    #[test]
    fn a_pickup_names_its_item_and_stack() {
        let p = hex("2b 1b56010100690c0b0000870000d91566245a00000000000000013600000000000000000000000000290000");
        assert_eq!(item_entries(&p), Some(vec![ItemEntry { item: 610_670_041, total: 90, change: Change::Changed }]));
    }

    /// A Targena gather that also gave a Life Crystal: two entries, of two
    /// sizes.
    #[test]
    fn two_items_at_once_are_both_read() {
        let p = hex("52 1b560201008ec8100000670000abc0671e4600000000000000010a0000000000000000000000000029000001003d1d0e00006f0000da1566241000000000000000013b000000000000000000000000290000");
        assert_eq!(
            item_entries(&p),
            Some(vec![
                ItemEntry { item: 510_116_011, total: 70, change: Change::Changed },
                ItemEntry { item: 610_670_042, total: 16, change: Change::Changed },
            ])
        );
    }

    /// The invasion's reward cards, sent twice as they came up: one run.
    #[test]
    fn a_runs_cards_read_twice_are_one_event() {
        let cards = crate::capture::reward_cards::parse(&hex(include_str!("testdata/reward_cards_invasion.hex"))).unwrap();
        let mut loot = Loot::default();
        assert!(loot.cards(1_000, cards.clone(), Whereabouts::default()));
        assert!(!loot.cards(1_500, cards.clone(), Whereabouts::default()));
        // Another run, later, with the same cards would be another event.
        assert!(loot.cards(1_000 + SAME_CARDS_MS, cards, Whereabouts::default()));
        assert_eq!(loot.since(0).unwrap().1.len(), 2);
    }

    /// The Shugo Festival's rewards (2026-11-12, 3 tokens on one card): six
    /// stacks and nine pieces of gear, whose entries run 106 to 128 bytes.
    /// The record that follows names the gear again: nothing more gained.
    #[test]
    fn a_reward_with_gear_is_read_whole() {
        let rewards = item_entries(&hex(include_str!("testdata/item_gained_rewards.hex"))).expect("read");
        let items: Vec<u32> = rewards.iter().map(|e| e.item).collect();
        assert_eq!(
            items,
            vec![
                930_100_023, 930_100_011, 930_100_003,
                110_260_033, 110_260_033, 110_260_033,
                210_760_035, 210_760_035, 210_760_035,
                210_450_080, 210_450_080, 210_450_080,
                610_610_014, 610_610_013, 610_530_001,
            ]
        );
        assert_eq!(rewards.iter().filter(|e| e.change == Change::New).count(), 9, "the gear is new");
        assert_eq!(rewards[0].total, 176_372, "Enhance Stones after the reward");

        let again = item_entries(&hex(include_str!("testdata/item_gained_rewards_again.hex"))).expect("read");
        assert_eq!(again.len(), 9);
        let mut loot = Loot::default();
        loot.items(1, &rewards, Whereabouts::default());
        loot.items(2, &again, Whereabouts::default());
        let (_, events) = loot.since(1).unwrap();
        let LootEvent::Items { items, .. } = &events[0] else { panic!() };
        assert!(items.iter().all(|i| i.gained == Some(0)), "the same gear again gains nothing");
    }

    /// An item used up (2026-11-12): its stack is gone, nothing gained.
    #[test]
    fn an_item_used_up_reads_as_removed() {
        let p = hex("2b 1b56010200855e0200008e00006926b41f0000000000000000040000000000000000000000000000002600");
        assert_eq!(item_entries(&p), Some(vec![ItemEntry { item: 531_900_009, total: 0, change: Change::Removed }]));
    }

    /// Drops of 2026-11-12: one pet soul for one player, and a record of two
    /// drops from two monsters for two party members.
    #[test]
    fn drops_name_their_source_and_owner() {
        let one = hex("43 1e56017584000016b187c7bb6275c700d42f46779a87c7cdd374c700103046de5b00005f436200c243de1f0100000002ba2f0400000018055fcfeacfa1010000010301");
        assert_eq!(
            drops(&one),
            Some(vec![Drop { source: 23_518, item: 534_660_034, count: 1, owner_kind: 2, owner: 0x0518_0000_0004_2fba }])
        );
        let two = hex("8201 1e5602106d0000438385c7c72e4fc700984f46f84e85c72a604fc700404f46ae5f00000f7d62004044de1f0100000002d62a040000002005c876edcfa10100000103925d00002fe286c79f5c56c700f44a4691ed86c75ce755c700544b46ae5f0000cc7d62004044de1f01000000028d03040000001805be80edcfa1010000010302");
        let read = drops(&two).expect("two drops");
        assert_eq!(read.iter().map(|d| (d.source, d.owner)).collect::<Vec<_>>(),
            vec![(24_494, 0x0520_0000_0004_2ad6), (24_494, 0x0518_0000_0004_038d)]);
        assert_eq!(drops(&two[..two.len() - 1]), None, "cut short");
    }

    /// Gathering an Aria node three times (2026-11-12), and one refused.
    #[test]
    fn gathers_read_as_the_game_sends_them() {
        let start = hex("9b01 318d0000bf8401010000000100000100000005030120030000012a0300006e000000a00000000000000000000000050164");
        assert_eq!(gather(&start), Some(Gather::Started { node: 16_959, n: 1 }));
        assert_eq!(gather(&hex("15 318d043a978402000000000000000000000000")), Some(Gather::Refused { node: 33_303 }));
        let done = hex("1d 338d003abf8401d915662401000000010000000000000000000000");
        assert_eq!(
            gather(&done),
            Some(Gather::Done { node: 16_959, item: 610_670_041, count: 1, bonus: 1, extra_item: 0, extra_count: 0 })
        );
        let crystal = hex("1d 338d003ab98801da1566240100000001000000abc0671e01000000");
        assert_eq!(
            gather(&crystal),
            Some(Gather::Done { node: 17_465, item: 610_670_042, count: 1, bonus: 1, extra_item: 510_116_011, extra_count: 1 })
        );
    }
}

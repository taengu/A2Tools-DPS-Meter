//! Buffs, debuffs and stats: the server's abnormal and stat records, and a
//! timeline built from them.
//!
//! ```text
//! <len> 2a 38 <entity> 01 <flags u8> <instance> <abnormal u32> <length i64> <end i64>
//!             <caster> <level u8> [flags & 2: <skill u32>] <u8> <x f32> <y f32> <z f32>   added
//! <len> 2b 38 <entity> <flags u8> <instance> ... the same from <abnormal> on              changed
//! <len> 2c 38 <entity> <n u8> (<kind u8> <instance> <reason u8>
//!             [kind 7: <by entity> <skill u32> <skill effect u32>])*                      removed
//! <len> 4a 36 <entity> <n u8> (<stat u16> <value i32>)* <8 bytes>                         stats changed
//! <len> 49 36 <2 bytes> <n u8> (<stat u16> <value i32>)* <8 bytes>                        all stats
//! <len> 41 36 | 45 36 | 33 36 <entity> ... <n u8> (<entry>)* 07 02 | 0f ...              on at spawn
//!   entry: <flags u8> <instance> <abnormal u32> <length i64> <end i64>
//!          [flags & 1: <caster>] <level u8> [flags & 2: <skill u32>] [<u8>] <x f32> <y f32> <z f32>
//! ```
//!
//! - `abnormal` is a `SkillAbnormal` id; `skill` the skill that applied it;
//!   `level` that skill's level (a passive's base + board + gear level).
//! - `instance` is numbered per entity. A stacking buff is one instance per
//!   stack: Element Unification at 5 stacks is 5 live instances. A stack
//!   past the limit (SkillAbnormal `AbnormalOverlapCount`) pushes out the
//!   oldest, which for Element no record ends. A debuff that several players
//!   keep up is one instance; a change names whoever applied it last.
//! - `end` is the server's clock in ms since the epoch (2100-01-01 00:00 in
//!   Korea when the abnormal never ends, length -1); `length` runs from the
//!   instance's start to `end`. A change that renews a stack keeps the start;
//!   one that applies the abnormal again restarts it (the byte before the
//!   position is then 2).
//! - Reason 1 is the timer running out; kind 7 (reason 11) is a skill taking
//!   the abnormal off, the skill named; reason 5 a passive replaced by its new
//!   level at login. Other reasons are not decoded.
//! - Stats are `EStat` ids; percent stats in hundredths. `4a 36` comes for
//!   your own character only, with the new value of each stat that changed.
//! - A spawn record (`41 36` a summon or monster, `45 36` another player) and
//!   your self record (`33 36`, at each map load) list every abnormal the
//!   entity has on, in the add layout. A spirit's list holds its passive at
//!   the level of its summon skill. See `listed`.
//!
//! Every add, change and removal record in 19 captures of 2026-10-04 to
//! 2026-10-06 (336,702 adds and changes, 57,886 removals) reads to its last
//! byte with this layout.
//!
//! Ported from Seralth's Daevalog (GPL-3.0), `src-tauri/src/capture/abnormal.rs`.

use std::collections::{BTreeMap, HashMap};

use super::stream_processor::read_varint;

// Daevalog keeps these in `capture::opcodes`; this meter names opcodes where
// they are parsed, so the ones read here are spelled out.
const SPAWN: [u8; 2] = [0x41, 0x36];
const PLAYER_SPAWN: [u8; 2] = [0x45, 0x36];
const SELF_IDENTITY: [u8; 2] = [0x33, 0x36];
const MAP_LOAD: [u8; 2] = [0x21, 0x36];
const DEATH: [u8; 2] = [0x42, 0x36];

pub const ADDED: [u8; 2] = [0x2A, 0x38];
pub const CHANGED: [u8; 2] = [0x2B, 0x38];
pub const REMOVED: [u8; 2] = [0x2C, 0x38];
pub const STATS_CHANGED: [u8; 2] = [0x4A, 0x36];
pub const ALL_STATS: [u8; 2] = [0x49, 0x36];

/// The `end` of an abnormal that never ends: 2100-01-01 00:00 in Korea
/// (UTC+9), in all 7,815 such records of 29 captures.
const NEVER: i64 = 4_102_412_400_000;
/// Bytes after the skill: one byte, then the caster's position (x, y, z
/// floats; 26-96 units from the caster's own track, far from the target's).
const ADD_TAIL: usize = 13;
/// The field after the abnormal list of a spawn record, and of a player's.
const AFTER_SPAWN_LIST: &[u8] = &[0x07, 0x02];
const AFTER_PLAYER_LIST: &[u8] = &[0x0F];
/// 2024-01-01, before any timed abnormal in a list started.
const EARLIEST: i64 = 1_704_067_200_000;
/// Longer than any timed abnormal in a list lasts (the longest seen: 73 days).
const LONGEST: i64 = 400 * 86_400_000;
const STATS_TAIL: usize = 8;

/// The stack limit of each abnormal that stacks, from `abnormals.json`
/// (`{"abnormals": {"<id>": {"stacks": n}}}`); empty if it does not read.
pub fn stack_limits(json: &str) -> HashMap<u32, u32> {
    let table: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let Some(abnormals) = table["abnormals"].as_object() else { return HashMap::new() };
    abnormals
        .iter()
        .filter_map(|(id, a)| Some((id.parse().ok()?, u32::try_from(a["stacks"].as_u64()?).ok()?)))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub entity: i32,
    pub instance: i32,
    pub abnormal: u32,
    /// From the instance's start to `end_ms`; `None` when it never ends.
    pub length_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub caster: i32,
    pub level: u8,
    pub skill: Option<u32>,
    /// A change that applied the abnormal again (its start moves).
    pub restarted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    pub instance: i32,
    pub reason: u8,
    /// Kind 7: who took it off, with which skill and skill effect.
    pub by: Option<(i32, u32, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    Added(Applied),
    Changed(Applied),
    Removed { entity: i32, list: Vec<Removal> },
    /// `entity` is `None` for the whole sheet, which names nobody: it is yours.
    Stats { entity: Option<i32>, values: Vec<(u16, i32)> },
    /// Every abnormal on `entity` as it comes into view; `own` for your self
    /// record. A caster of 0 is one the entry does not name.
    Listed { entity: i32, own: bool, list: Vec<Applied> },
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn varint(&mut self) -> Option<i32> {
        let v = read_varint(self.b, self.at);
        if v.length <= 0 {
            return None;
        }
        self.at += v.length as usize;
        Some(v.value)
    }
    fn bytes<const N: usize>(&mut self) -> Option<[u8; N]> {
        let out = self.b.get(self.at..self.at + N)?.try_into().ok()?;
        self.at += N;
        Some(out)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.bytes::<1>()?[0])
    }
    fn left(&self) -> usize {
        self.b.len().saturating_sub(self.at)
    }
}

/// Read one framed packet (its length varint included). `None` for any other
/// packet, and for a record that does not end where the layout says.
pub fn parse(packet: &[u8]) -> Option<Record> {
    let len = read_varint(packet, 0);
    if len.length <= 0 {
        return None;
    }
    let o = len.length as usize;
    let op: [u8; 2] = packet.get(o..o + 2)?.try_into().ok()?;
    let mut r = Reader { b: packet, at: o + 2 };
    match op {
        ADDED | CHANGED => {
            let entity = r.varint()?;
            if op == ADDED && r.u8()? != 1 {
                return None;
            }
            let flags = r.u8()?;
            let instance = r.varint()?;
            let abnormal = u32::from_le_bytes(r.bytes()?);
            let length = i64::from_le_bytes(r.bytes()?);
            let end = i64::from_le_bytes(r.bytes()?);
            let caster = r.varint()?;
            let level = r.u8()?;
            let skill = if flags & 0x02 != 0 { Some(u32::from_le_bytes(r.bytes()?)) } else { None };
            let restarted = r.u8()? == 2;
            if r.left() != ADD_TAIL - 1 {
                return None;
            }
            let applied = Applied {
                entity,
                instance,
                abnormal,
                length_ms: (length >= 0).then_some(length),
                end_ms: (length >= 0 && end != NEVER).then_some(end),
                caster,
                level,
                skill,
                restarted,
            };
            Some(if op == ADDED { Record::Added(applied) } else { Record::Changed(applied) })
        }
        REMOVED => {
            let entity = r.varint()?;
            let n = r.u8()?;
            let mut list = Vec::with_capacity(n as usize);
            for _ in 0..n {
                let kind = r.u8()?;
                let instance = r.varint()?;
                let reason = r.u8()?;
                let by = if kind & 0x07 == 0x07 {
                    let who = r.varint()?;
                    Some((who, u32::from_le_bytes(r.bytes()?), u32::from_le_bytes(r.bytes()?)))
                } else {
                    None
                };
                list.push(Removal { instance, reason, by });
            }
            (r.left() == 0).then_some(Record::Removed { entity, list })
        }
        STATS_CHANGED | ALL_STATS => {
            let entity = if op == STATS_CHANGED {
                Some(r.varint()?)
            } else {
                r.bytes::<2>()?;
                None
            };
            let n = r.u8()?;
            let mut values = Vec::with_capacity(n as usize);
            for _ in 0..n {
                let stat = u16::from_le_bytes(r.bytes()?);
                values.push((stat, i32::from_le_bytes(r.bytes()?)));
            }
            (r.left() == STATS_TAIL).then_some(Record::Stats { entity, values })
        }
        SPAWN | PLAYER_SPAWN | SELF_IDENTITY => {
            let entity = r.varint()?;
            let after = if op == SPAWN { AFTER_SPAWN_LIST } else { AFTER_PLAYER_LIST };
            let list = (r.at..packet.len()).find_map(|at| listed(packet, at, entity, after))?;
            Some(Record::Listed { entity, own: op == SELF_IDENTITY, list })
        }
        _ => None,
    }
}

/// The abnormal list at `at`, if one is there: a count, then that many
/// entries in the add layout from the flags on. The caster is left out when
/// flags bit 0 is clear (only in `45 36`), and the byte before the position
/// is there in some entries and not in others; what decides it is not
/// known (the first entry has it in 27,111 of 27,129 lists, a later one in
/// 17,425 of 196,666 entries). The list sits behind fields of
/// varying length, so it is found by trying each start and both entry
/// lengths, and kept only when exactly one reading has every entry well
/// formed and ends where `after` follows.
///
/// Over 29 captures of 2026-10-04 to 2026-10-06 this found one list with
/// one reading in 7,455 `41 36`, 19,496 `45 36` and 178 `33 36` records;
/// all 223,795 entries are `SkillAbnormal` ids. Where the same instance was
/// seen in an add or change before, its caster and level agree every time
/// (5,183 entries), and in a self record every field agrees (2,116).
fn listed(b: &[u8], at: usize, entity: i32, after: &[u8]) -> Option<Vec<Applied>> {
    let n = *b.get(at)? as usize;
    if n == 0 || listed_entry(b, at + 1, entity).is_none() {
        return None;
    }
    let mut readings = vec![(Vec::with_capacity(n), at + 1)];
    for k in 0..n {
        let mut next = Vec::new();
        for (list, i) in &readings {
            let Some((entry, position)) = listed_entry(b, *i, entity) else { continue };
            for end in [position + 12, position + 13] {
                let fits = if k + 1 < n {
                    listed_entry(b, end, entity).is_some()
                } else {
                    b.get(end..end + after.len()) == Some(after)
                };
                if fits {
                    let mut list = list.clone();
                    list.push(entry.clone());
                    next.push((list, end));
                }
            }
        }
        if next.is_empty() || next.len() > 4 {
            return None;
        }
        readings = next;
    }
    (readings.len() == 1).then(|| readings.remove(0).0)
}

/// One list entry up to its position, and where the optional byte or the
/// position starts.
fn listed_entry(b: &[u8], at: usize, entity: i32) -> Option<(Applied, usize)> {
    let mut r = Reader { b, at };
    let flags = r.u8()?;
    if !matches!(flags, 0x10 | 0x11 | 0x13) {
        return None;
    }
    let instance = r.varint()?;
    let abnormal = u32::from_le_bytes(r.bytes()?);
    let length = i64::from_le_bytes(r.bytes()?);
    let end = i64::from_le_bytes(r.bytes()?);
    let endless = length == -1 && end == NEVER;
    let timed = (0..=LONGEST).contains(&length) && (EARLIEST..NEVER).contains(&end) && end - length >= EARLIEST;
    if !(1..1_000_000_000).contains(&abnormal) || !(endless || timed) {
        return None;
    }
    let caster = if flags & 0x01 != 0 { r.varint()? } else { 0 };
    let level = r.u8()?;
    let skill = if flags & 0x02 != 0 { Some(u32::from_le_bytes(r.bytes()?)) } else { None };
    let applied = Applied {
        entity,
        instance,
        abnormal,
        length_ms: timed.then_some(length),
        end_ms: timed.then_some(end),
        caster,
        level,
        skill,
        restarted: false,
    };
    Some((applied, r.at))
}

/// Why an abnormal ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// Its timer ran out (reason 1).
    Expired,
    /// A skill took it off (kind 7).
    TakenOff { skill: u32 },
    /// Removed for a reason not decoded.
    Removed(u8),
    /// Applied again by another caster: one debuff that several players
    /// keep up is one instance, its caster the last one to apply it.
    Recast,
    /// Its entity died or left the world (`42 36` flag 3 or 7).
    Gone,
    /// A map load: every entity but you is gone (`21 36`).
    MapLoad,
    /// Left out of a later list of everything on its entity (a spawn or
    /// self record) before its timer ran out: it ended unseen. Of 980
    /// instances ended so in 29 captures, 3 had a record later.
    NotListed,
    /// A newer stack went past the abnormal's stack limit and pushed this,
    /// the oldest, out. For Element no record says so.
    PushedOut,
    /// No record ended it: closed at its timer.
    Unseen,
    /// Still on when the capture ended.
    Open,
}

impl End {
    pub fn label(&self) -> String {
        match self {
            End::Expired => "expired".into(),
            End::TakenOff { skill } => format!("taken off by skill {skill}"),
            End::Removed(5) => "replaced by a new level".into(),
            End::Removed(r) => format!("removed, reason {r}"),
            End::Recast => "applied again by another caster".into(),
            End::Gone => "entity gone".into(),
            End::MapLoad => "map load".into(),
            End::PushedOut => "pushed out past the stack limit".into(),
            End::NotListed => "not in a later spawn record".into(),
            End::Unseen => "no end seen, closed at its timer".into(),
            End::Open => "open".into(),
        }
    }
}

/// One instance of an abnormal on one entity, from its add to its end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub entity: i32,
    pub instance: i32,
    pub abnormal: u32,
    pub caster: i32,
    pub skill: Option<u32>,
    pub level: u8,
    /// A passive or other abnormal without a timer.
    pub endless: bool,
    pub start_ms: i64,
    /// `None` while open.
    pub end_ms: Option<i64>,
    pub end: End,
    /// When the server last said it ends, on the server's clock.
    pub server_end_ms: Option<i64>,
}

impl Instance {
    fn from(a: &Applied, start_ms: i64) -> Instance {
        Instance {
            entity: a.entity,
            instance: a.instance,
            abnormal: a.abnormal,
            caster: a.caster,
            skill: a.skill,
            level: a.level,
            endless: a.length_ms.is_none(),
            start_ms,
            end_ms: None,
            end: End::Open,
            server_end_ms: a.end_ms,
        }
    }
}

/// One stat record: the new values of the stats that changed (or all of them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatEvent {
    pub ms: i64,
    pub entity: Option<i32>,
    pub whole_sheet: bool,
    pub values: Vec<(u16, i32)>,
}

/// Your stat sheet: the last value of each stat (`EStat` id), as the
/// records so far left it. Cooldown reduction is `CoolTimeDecrease` (215)
/// less `CoolTimeIncrease` (385, negative when it shortens them), in
/// hundredths of a percent: 300 and -130 for a sheet that shows -4.3%
/// (2026-11-10).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct StatSheet {
    pub entity: Option<i32>,
    pub values: BTreeMap<u16, i32>,
}

impl StatSheet {
    /// A whole sheet (`49 36`) replaces it; changes (`4a 36`) update it, and
    /// start it anew when they are about another character (a switch).
    pub fn apply(&mut self, e: &StatEvent) {
        if e.whole_sheet {
            self.values = e.values.iter().copied().collect();
            return;
        }
        if let Some(id) = e.entity
            && self.entity != Some(id)
        {
            if self.entity.is_some() {
                self.values.clear();
            }
            self.entity = Some(id);
        }
        self.values.extend(e.values.iter().copied());
    }
}

/// Your stats in a fight, for deriving from later (cooldowns, damage):
/// the sheet when it started and each change during it, `ms` from its
/// start.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FightStats {
    pub entity: Option<i32>,
    pub at_start: BTreeMap<u16, i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<StatChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatChange {
    pub ms: i64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub whole_sheet: bool,
    pub values: Vec<(u16, i32)>,
}

/// Stat records pinned for slices at most (a few hours of buffs coming and
/// going is far fewer); past it the oldest changes go.
const STAT_PACKETS_CAP: usize = 4_096;

/// Open instances at most: past it, every one not on you closes, as on a
/// map load. Endless ones close only so, and a crafted stream that never
/// loads a map could open them without end (audit L8); a real fight holds a
/// few hundred.
const LIVE_CAP: usize = 20_000;

/// Every abnormal instance and stat record of a capture, in capture time.
#[derive(Default)]
pub struct Timeline {
    live: HashMap<(i32, i32), Instance>,
    pub instances: Vec<Instance>,
    pub stats: Vec<StatEvent>,
    /// The sheet as of the oldest record in `stats`: those `prune` dropped,
    /// folded in, so a fight's sheet at its start is known however long ago
    /// the login was.
    stat_base: StatSheet,
    /// Stat records read so far: the plugins' `stats` event follows it.
    pub stat_seq: u64,
    /// The stat records themselves, as framed packets with their capture
    /// time: the last whole sheet and every change since. The login sheet
    /// comes hours before most fights, past the packets held for a slice;
    /// these go into it instead, so a log's stats can be derived later.
    pub stat_packets: Vec<(i64, Vec<u8>)>,
    map_loads: usize,
    /// The entity of the last `4a 36`: you.
    own: Option<i32>,
    /// Capture time minus server time at the last add: about 25-120 ms.
    lag_ms: i64,
    swept_ms: i64,
    /// Abnormal id -> the stacks one entity can hold, where more than one.
    stack_limits: HashMap<u32, u32>,
    /// Stacks pushed out at the time of the last read, as (entity,
    /// instance) and index in `instances`: a removal read at the same time
    /// says why instead.
    pushed: Vec<((i32, i32), usize)>,
    pushed_ms: i64,
}

impl Timeline {
    /// The stack limit of each abnormal that stacks (`abnormals.json`).
    pub fn set_stack_limits(&mut self, limits: HashMap<u32, u32>) {
        self.stack_limits = limits;
    }

    /// Read one framed packet received at `ms`.
    pub fn note(&mut self, ms: i64, packet: &[u8]) {
        let len = read_varint(packet, 0);
        if len.length <= 0 {
            return;
        }
        let o = len.length as usize;
        if ms >= self.swept_ms + 1_000 {
            self.swept_ms = ms;
            self.close_unseen(ms);
        }
        if ms != self.pushed_ms {
            self.pushed.clear();
            self.pushed_ms = ms;
        }
        match packet.get(o..o + 2) {
            // Your entity keeps its abnormals over a load; no record ends
            // anyone else's. A new id of yours comes just before the load.
            Some(op) if op == MAP_LOAD => {
                self.map_loads += 1;
                let own = self.own;
                self.close_where(ms, End::MapLoad, |k| Some(k.0) != own);
                return;
            }
            Some(op) if op == DEATH => {
                let mut r = Reader { b: packet, at: o + 2 };
                if let (Some(entity), Some(_), Some(flag)) = (r.varint(), r.varint(), r.varint())
                    && (flag == 3 || flag == 7)
                {
                    self.close_where(ms, End::Gone, |k| k.0 == entity);
                }
                return;
            }
            _ => {}
        }
        match parse(packet) {
            Some(Record::Added(a)) => {
                // A reused number ends the old instance first.
                if let Some(old) = self.live.remove(&(a.entity, a.instance)) {
                    self.close(old, ms, End::Removed(0));
                }
                if let (Some(length), Some(end)) = (a.length_ms, a.end_ms) {
                    self.lag_ms = (ms - (end - length)).clamp(0, 1_000);
                }
                if let Some(&limit) = self.stack_limits.get(&a.abnormal) {
                    self.push_out(ms, a.entity, a.abnormal, limit);
                }
                if self.live.len() >= LIVE_CAP {
                    let own = self.own;
                    self.close_where(ms, End::MapLoad, |k| Some(k.0) != own);
                }
                self.live.insert((a.entity, a.instance), Instance::from(&a, ms));
            }
            Some(Record::Changed(a)) => {
                let key = (a.entity, a.instance);
                // Another abnormal in the instance: the old one ends and the
                // new starts, as in a list. A second Power of the Storm took
                // over Earth's Blessing's instance with a change, and the
                // timeline drew it as Earth's Blessing (2026-11-09).
                if self.live.get(&key).is_some_and(|l| l.abnormal != a.abnormal) {
                    let old = self.live.remove(&key).unwrap();
                    self.close(old, ms, End::Removed(0));
                    self.live.insert(key, Instance::from(&a, ms));
                }
                if self.live.get(&key).is_some_and(|l| l.caster != a.caster) {
                    let mut next = self.live.remove(&key).unwrap();
                    self.close(next.clone(), ms, End::Recast);
                    next.caster = a.caster;
                    next.start_ms = ms;
                    self.live.insert(key, next);
                }
                self.seen(ms, &a);
            }
            Some(Record::Listed { entity, own, list }) => {
                if own {
                    self.own = Some(entity);
                }
                // The list is all the entity has on: the rest ended unseen,
                // at its timer if that ran out already.
                let gone: Vec<_> = self
                    .live
                    .keys()
                    .filter(|k| k.0 == entity && !list.iter().any(|a| a.instance == k.1))
                    .copied()
                    .collect();
                for k in gone {
                    let i = self.live.remove(&k).unwrap();
                    match i.server_end_ms.map(|e| e + self.lag_ms).filter(|&e| e <= ms) {
                        Some(at) => {
                            let at = at.max(i.start_ms);
                            self.close(i, at, End::Unseen);
                        }
                        None => self.close(i, ms, End::NotListed),
                    }
                }
                for a in &list {
                    // A number now used by another abnormal ends the old one.
                    let key = (entity, a.instance);
                    if self.live.get(&key).is_some_and(|l| l.abnormal != a.abnormal) {
                        let old = self.live.remove(&key).unwrap();
                        self.close(old, ms, End::Removed(0));
                    }
                    self.seen(ms, a);
                }
            }
            Some(Record::Removed { entity, list }) => {
                for r in list {
                    let end = match (r.reason, r.by) {
                        (_, Some((_, skill, _))) => End::TakenOff { skill },
                        (1, None) => End::Expired,
                        (reason, None) => End::Removed(reason),
                    };
                    if let Some(live) = self.live.remove(&(entity, r.instance)) {
                        self.close(live, ms, end);
                    } else if let Some(&(_, at)) = self.pushed.iter().find(|p| p.0 == (entity, r.instance)) {
                        self.instances[at].end = end;
                    }
                }
            }
            Some(Record::Stats { entity, values }) => {
                if entity.is_some() {
                    self.own = entity;
                }
                if !values.is_empty() {
                    if entity.is_none() {
                        self.stat_packets.clear();
                    } else if self.stat_packets.len() >= STAT_PACKETS_CAP {
                        // The sheet stays; the oldest change after it goes.
                        let first_change = usize::from(self.stats_sheet_pinned());
                        self.stat_packets.remove(first_change);
                    }
                    self.stat_packets.push((ms, packet.to_vec()));
                    self.stats.push(StatEvent { ms, entity, whole_sheet: entity.is_none(), values });
                    self.stat_seq += 1;
                }
            }
            None => {}
        }
    }

    /// An instance a change or a list names. One not seen yet has been on
    /// since before the capture (or before it came into view): it started
    /// `length` before its end.
    fn seen(&mut self, ms: i64, a: &Applied) {
        let key = (a.entity, a.instance);
        if !self.live.contains_key(&key) {
            let start = match (a.length_ms, a.end_ms) {
                (Some(length), Some(end)) => (end - length + self.lag_ms).min(ms),
                _ => ms,
            };
            self.live.insert(key, Instance::from(a, start));
        }
        if let Some(live) = self.live.get_mut(&key) {
            live.level = a.level;
            live.server_end_ms = a.end_ms;
            if a.skill.is_some() {
                live.skill = a.skill;
            }
        }
    }

    fn close(&mut self, mut i: Instance, ms: i64, end: End) {
        i.end_ms = Some(ms);
        i.end = end;
        self.instances.push(i);
    }

    fn close_where(&mut self, ms: i64, end: End, which: impl Fn(&(i32, i32)) -> bool) {
        let keys: Vec<_> = self.live.keys().filter(|k| which(k)).copied().collect();
        for k in keys {
            if let Some(i) = self.live.remove(&k) {
                self.close(i, ms, end);
            }
        }
    }

    /// End the oldest stacks of `abnormal` on `entity` that a new one would
    /// take past `limit`. Element (4 stacks) loses its oldest with no
    /// record: in 29 captures (2026-10-04 to 2026-10-06) 19 Elements were
    /// pushed out this way and none was named by a record again, and the 6
    /// Elemental Fusions that came while one of them would still have been
    /// on took 4 Elements, never it. A limit one lower is wrong 1,421
    /// times: the stack it would push out gets records later.
    fn push_out(&mut self, ms: i64, entity: i32, abnormal: u32, limit: u32) {
        let mut stacks: Vec<(i64, i32)> = self
            .live
            .iter()
            .filter(|(k, i)| k.0 == entity && i.abnormal == abnormal)
            .map(|(k, i)| (i.start_ms, k.1))
            .collect();
        stacks.sort();
        let over = (stacks.len() + 1).saturating_sub(limit as usize);
        for &(_, instance) in &stacks[..over] {
            if let Some(i) = self.live.remove(&(entity, instance)) {
                self.pushed.push(((entity, instance), self.instances.len()));
                self.close(i, ms, End::PushedOut);
            }
        }
    }

    /// Instances 2 s past their end with no record ending them.
    fn close_unseen(&mut self, ms: i64) {
        let lag = self.lag_ms;
        let late: Vec<_> = self
            .live
            .iter()
            .filter_map(|(k, i)| i.server_end_ms.filter(|e| ms > e + lag + 2_000).map(|e| (*k, e + lag)))
            .collect();
        for (k, at) in late {
            if let Some(i) = self.live.remove(&k) {
                let at = at.max(i.start_ms);
                self.close(i, at, End::Unseen);
            }
        }
    }

    pub fn map_loads(&self) -> usize {
        self.map_loads
    }

    /// Forget instances that ended before `before_ms` and stat records from
    /// before it, so a meter left running holds a bounded window. What is
    /// still on is kept, however long ago it started.
    pub fn prune(&mut self, before_ms: i64) {
        let kept: Vec<bool> = self.instances.iter().map(|i| i.end_ms.is_none_or(|e| e >= before_ms)).collect();
        // The stacks just pushed out stay (they ended now), but their
        // indices move with the ones dropped before them.
        for p in &mut self.pushed {
            p.1 = kept[..p.1].iter().filter(|&&k| k).count();
        }
        let mut k = kept.iter();
        self.instances.retain(|_| *k.next().unwrap_or(&true));
        for s in self.stats.iter().filter(|s| s.ms < before_ms) {
            self.stat_base.apply(s);
        }
        self.stats.retain(|s| s.ms >= before_ms);
    }

    /// Whether the first pinned stat record is a whole sheet (`49 36`).
    fn stats_sheet_pinned(&self) -> bool {
        self.stat_packets.first().is_some_and(|(_, p)| {
            let len = read_varint(p, 0);
            len.length > 0 && p.get(len.length as usize..len.length as usize + 2) == Some(&ALL_STATS[..])
        })
    }

    /// Your stat sheet as it was at `ms`.
    pub fn stat_sheet_at(&self, ms: i64) -> StatSheet {
        let mut sheet = self.stat_base.clone();
        for s in self.stats.iter().take_while(|s| s.ms <= ms) {
            sheet.apply(s);
        }
        sheet
    }

    /// Your stats from `start_ms` to `end_ms`: the sheet at the start and
    /// the records in between. None when no stat record was read yet.
    pub fn fight_stats(&self, start_ms: i64, end_ms: i64) -> Option<FightStats> {
        let sheet = self.stat_sheet_at(start_ms - 1);
        let changes: Vec<StatChange> = self
            .stats
            .iter()
            .filter(|s| s.ms >= start_ms && s.ms <= end_ms)
            .map(|s| StatChange { ms: s.ms - start_ms, whole_sheet: s.whole_sheet, values: s.values.clone() })
            .collect();
        if sheet.values.is_empty() && changes.is_empty() {
            return None;
        }
        Some(FightStats { entity: sheet.entity.or(self.own), at_start: sheet.values, changes })
    }

    /// Forget everything but the stack limits: a capture from another
    /// session, whose entity ids mean something else.
    pub fn clear(&mut self) {
        let stack_limits = std::mem::take(&mut self.stack_limits);
        *self = Timeline { stack_limits, ..Timeline::default() };
    }

    /// Instances held, ended and still on.
    pub fn len(&self) -> usize {
        self.instances.len() + self.live.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every instance on any time from `start_ms` to `end_ms`, the ones still
    /// on included.
    pub fn instances_between(&self, start_ms: i64, end_ms: i64) -> Vec<Instance> {
        let overlaps = |i: &&Instance| i.start_ms <= end_ms && i.end_ms.is_none_or(|e| e >= start_ms);
        let mut out: Vec<Instance> =
            self.instances.iter().chain(self.live.values()).filter(overlaps).cloned().collect();
        out.sort_by_key(|i| (i.start_ms, i.entity, i.instance));
        out
    }

    /// Every instance, the ones still on included (open, no end).
    pub fn all_instances(&self) -> Vec<Instance> {
        let mut out = self.instances.clone();
        out.extend(self.live.values().cloned());
        out.sort_by_key(|i| (i.start_ms, i.entity, i.instance));
        out
    }
}

/// An abnormal on one entity from one source, its stacks over time: the
/// instances of one abnormal id whose casters resolve to the same owner (a
/// spirit's to its summoner), joined while at least one is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub entity: i32,
    pub abnormal: u32,
    pub owner: i32,
    pub skills: Vec<u32>,
    pub level: u8,
    pub endless: bool,
    pub start_ms: i64,
    pub end_ms: Option<i64>,
    pub end: End,
    /// (time, live instances from then on); the last step is 0 unless open.
    pub stacks: Vec<(i64, u32)>,
}

/// Join instances into tracks. `owner` maps a caster to whoever stands
/// behind it.
pub fn tracks(instances: &[Instance], owner: impl Fn(i32) -> i32) -> Vec<Track> {
    let mut groups: BTreeMap<(i32, u32, i32), Vec<&Instance>> = BTreeMap::new();
    // One added and taken off in the same read was never on.
    for i in instances.iter().filter(|i| i.end_ms != Some(i.start_ms)) {
        groups.entry((i.entity, i.abnormal, owner(i.caster))).or_default().push(i);
    }
    let mut out = Vec::new();
    for ((entity, abnormal, owner), list) in groups {
        let mut events: Vec<(i64, i32, &Instance)> = Vec::new();
        for i in &list {
            events.push((i.start_ms, 1, i));
            if let Some(e) = i.end_ms {
                events.push((e, -1, i));
            }
        }
        events.sort_by_key(|(t, d, _)| (*t, *d));
        let mut live = 0i32;
        let mut current: Option<Track> = None;
        // All changes at one time count as one step, so a stack ended and
        // another started in the same read keeps the track going.
        for step in events.chunk_by(|a, b| a.0 == b.0) {
            let t = step[0].0;
            live = (live + step.iter().map(|e| e.1).sum::<i32>()).max(0);
            if current.is_none() && live == 0 {
                continue;
            }
            let track = current.get_or_insert_with(|| Track {
                entity,
                abnormal,
                owner,
                skills: Vec::new(),
                level: 0,
                endless: false,
                start_ms: t,
                end_ms: None,
                end: End::Open,
                stacks: Vec::new(),
            });
            for &(_, d, i) in step {
                if d > 0 {
                    if let Some(s) = i.skill.filter(|s| !track.skills.contains(s)) {
                        track.skills.push(s);
                    }
                    track.level = track.level.max(i.level);
                    track.endless |= i.endless;
                } else {
                    track.end = i.end;
                }
            }
            if track.stacks.last().is_none_or(|s| s.1 != live as u32) {
                track.stacks.push((t, live as u32));
            }
            if live == 0 {
                let mut done = current.take().unwrap();
                done.end_ms = Some(t);
                out.push(done);
            }
        }
        if let Some(mut open) = current {
            open.end = End::Open;
            out.push(open);
        }
    }
    out.sort_by_key(|t| (t.start_ms, t.entity, t.abnormal));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    // Records from a capture of 2026-10-06 (the local player is entity 6759,
    // two spirits 71747 and 28113), each named by its capture time.

    /// 21:58:30.225: Spirit's Benediction (abnormal 161900001) from skill
    /// 16190000 at level 3, on for 10 s.
    const BENEDICTION_ON: &str = "342a38e73401138202e165a6091027000000000000bc5cba14a1010000e73403300af7000047e44c47b46a22c700e85a46";
    /// 21:58:40.267: instance 258 taken off, reason 1, 10.04 s later.
    const BENEDICTION_OFF: &str = "0d2c38e7340100820201";
    /// 21:58:30.225 and 21:58:40.267: the stats it changes, on and off.
    const STATS_ON: &str = "294a36e7340441006c0700004c00e80300007b01041000007c01d00700000000000000000000";
    const STATS_OFF: &str = "294a36e734044100840300004c00000000007b01340800007c01000000000000000000000000";

    #[test]
    fn an_added_buff_names_its_skill_level_and_end() {
        let Some(Record::Added(a)) = parse(&hex(BENEDICTION_ON)) else { panic!() };
        assert_eq!(
            a,
            Applied {
                entity: 6759,
                instance: 258,
                abnormal: 161_900_001,
                length_ms: Some(10_000),
                end_ms: Some(1_791_349_120_188),
                caster: 6759,
                level: 3,
                skill: Some(16_190_000),
                restarted: false,
            }
        );
        assert_eq!(
            parse(&hex(BENEDICTION_OFF)),
            Some(Record::Removed { entity: 6759, list: vec![Removal { instance: 258, reason: 1, by: None }] })
        );
    }

    #[test]
    fn a_passive_never_ends_and_has_no_skill() {
        // 2026-10-05 14:57:00.695: Spirit Strike (167100001) at login, level 1.
        let Some(Record::Added(a)) =
            parse(&hex("2f2a38a15801110661bef509ffffffffffffffff8075d52abb030000a15801005e71f0c7196dd6c700dcbf46"))
        else {
            panic!()
        };
        assert_eq!((a.entity, a.instance, a.abnormal, a.length_ms, a.end_ms, a.skill), (11297, 6, 167_100_001, None, None, None));
    }

    #[test]
    fn a_renewed_stack_keeps_its_start_and_a_reapplied_one_restarts() {
        // 21:58:32.070: Element Unification (167800011) stack 254 renewed by a
        // new stack, 12.75 s after its start; stack 269 added.
        let Some(Record::Changed(c)) =
            parse(&hex("332b38e73413fe01cb6c000ace31000000000000f663ba14a1010000e73402e10a00010047e44c47b46a22c700e85a46"))
        else {
            panic!()
        };
        assert_eq!((c.instance, c.abnormal, c.length_ms, c.level, c.restarted), (254, 167_800_011, Some(12_750), 2, false));
        // 21:58:30.172: the Dimensional Control trigger (163300000) applied
        // again by a spirit's skill.
        let Some(Record::Changed(c)) =
            parse(&hex("342b38e73413f601a0c2bb09b80b0000000000003241ba14a1010000fcca0201f8c5f50002b2364a47e3ae1ec709e15b46"))
        else {
            panic!()
        };
        assert_eq!((c.instance, c.abnormal, c.length_ms, c.caster, c.restarted), (246, 163_300_000, Some(3_000), 42364, true));
    }

    #[test]
    fn a_skill_takes_off_four_elements_at_once() {
        // 21:58:33.217: Elemental Fusion (16300002) uses up four Elements.
        let Some(Record::Removed { entity, list }) = parse(&hex(
            "412c38e734040791020be734e2b7f8005dd427610780020be734e2b7f8005dd4276107f9010be734e2b7f8005dd4276107f5010be734e2b7f8005dd42761",
        )) else {
            panic!()
        };
        assert_eq!(entity, 6759);
        let taken: Vec<_> = list.iter().map(|r| (r.instance, r.reason, r.by)).collect();
        let by = Some((6759, 16_300_002, 1_630_000_221));
        assert_eq!(taken, [(273, 11, by), (256, 11, by), (249, 11, by), (245, 11, by)]);
    }

    #[test]
    fn stat_records_carry_the_new_values() {
        // Spirit's Benediction: PvP and PvE Damage Boost and Tolerance, the
        // values of the game's StatChange rows (+10%, +10%, +20%, +20%).
        assert_eq!(
            parse(&hex(STATS_ON)),
            Some(Record::Stats { entity: Some(6759), values: vec![(65, 1900), (76, 1000), (379, 4100), (380, 2000)] })
        );
        assert_eq!(
            parse(&hex(STATS_OFF)),
            Some(Record::Stats { entity: Some(6759), values: vec![(65, 900), (76, 0), (379, 2100), (380, 0)] })
        );
    }

    /// A login's stat records (2026-11-10): the stat page showed -4.3%
    /// cooldown, and CoolTimeDecrease 300 less CoolTimeIncrease -130 is it.
    const LOGIN_SHEET: &str = "91034a36f245400700060000000800060000000900050000000a00060000000b00060000000d00060000000e00060000000f00050000001000060000001100060000001300d40000001c00cc0200001f00050100002100d60000003400040600003a00030000003d002d00000046005802000071000a0000007d000f00000080001c02000085005e01000092008a0200009300c2010000c100ff390000c400d6060000c500000a0000c700350d0000cd00fe010000d7002c010000d80064000000da00ec0d00001a01300200003301211300003d01b90200008101ceffffff8201ceffffffa90136010000aa017c060000ab013c000000ac0122010000ad013c000000b00122010000ba0130020000bb01fa000000bc0130020000bd013c000000be013c000000bf013c000000c0013c000000c10132000000f00104010000f401fe010000f50154010000f60122010000f701e0010000f801e0010000fa01d6010000fb01b8010000fc0154010000fd0154010000fe01be000000ff01be000000390205000000ff39000000000000";
    const LOGIN_CHANGES: &str = "d1014a36f2452007002900000008002200000009000d0000000a000f0000000b00290000000e00220000000f000b00000010001100000011001a0000002c00d60600008000350200008500720100009f00fc0d0000c40024090000c500560e0000cd00c6020000da00950e00001a018e03000081017effffff820192ffffffaa016a090000ab0154010000ac013a020000ad0154010000b001ea010000ba018e030000bb019a010000bc018a020000bd01aa000000bf019a010000c00154010000c101820000000000000000000000";

    #[test]
    fn the_sheet_adds_up_to_the_stat_page() {
        let mut t = Timeline::default();
        t.note(1_000, &hex(LOGIN_SHEET));
        t.note(1_000, &hex(LOGIN_CHANGES));
        let sheet = t.stat_sheet_at(i64::MAX);
        assert_eq!(sheet.entity, Some(8946));
        assert_eq!(sheet.values.get(&215), Some(&300), "CoolTimeDecrease");
        assert_eq!(sheet.values.get(&385), Some(&-130), "CoolTimeIncrease, the later record's");
        assert_eq!(t.stat_packets.len(), 2);
        assert_eq!(t.stat_seq, 2);
        assert_eq!(t.fight_stats(500, 900), None, "before any record");

        // Kept for a fight long after, the login folded into the base.
        t.prune(5_000);
        let fight = t.fight_stats(10_000, 20_000).expect("the login sheet");
        assert_eq!(fight.at_start.get(&385), Some(&-130));
        assert!(fight.changes.is_empty());
    }

    #[test]
    fn another_character_starts_a_new_sheet() {
        let mut sheet = StatSheet::default();
        sheet.apply(&StatEvent { ms: 0, entity: Some(1), whole_sheet: false, values: vec![(215, 300), (128, 430)] });
        sheet.apply(&StatEvent { ms: 1, entity: Some(2), whole_sheet: false, values: vec![(215, 100)] });
        assert_eq!(sheet.values.into_iter().collect::<Vec<_>>(), vec![(215, 100)]);
    }

    #[test]
    fn a_record_cut_short_or_running_long_is_not_read() {
        let mut long = hex(BENEDICTION_ON);
        long.push(0);
        long[0] += 1;
        assert_eq!(parse(&long), None);
        let mut short = hex(BENEDICTION_OFF);
        short.pop();
        short[0] -= 1;
        assert_eq!(parse(&short), None);
    }

    #[test]
    fn the_timeline_ends_a_spirits_buff_when_it_leaves() {
        let mut t = Timeline::default();
        t.note(1_000, &hex(BENEDICTION_ON));
        // 21:58:30.225: the same buff on spirit 28113, then its `42 36` flag 7.
        t.note(1_000, &hex("342a38d1db01011303e165a6091027000000000000bc5cba14a1010000e73403300af7000047e44c47b46a22c700e85a46"));
        t.note(2_052, &hex("0b4236d1db010007"));
        t.note(11_042, &hex(BENEDICTION_OFF));
        let ends: Vec<_> = t.all_instances().iter().map(|i| (i.entity, i.start_ms, i.end_ms, i.end)).collect();
        assert_eq!(ends, [(6759, 1_000, Some(11_042), End::Expired), (28113, 1_000, Some(2_052), End::Gone)]);
    }

    #[test]
    fn a_debuff_kept_up_by_several_players_changes_hands() {
        // 2026-10-06, Kernon of the West (48776): one Curse (161400005),
        // instance 293, applied by entity 1168, then by the local player
        // 15740, then by 16129.
        let mut t = Timeline::default();
        t.note(10_974, &hex("352a3888fd020113a502c5c49e09581b000000000000b2a79214a101000090090aea46f6000037cbb3c7f0172dc700e44646"));
        t.note(33_279, &hex("342b3888fd0213a502c5c49e09581b000000000000cefe9214a1010000fc7a0d6c47f6000242f9b6c7d97b2dc700c43d46"));
        t.note(34_816, &hex("342b3888fd0213a502c5c49e09581b000000000000dc049314a1010000817e0d5847f600028a09b5c7229527c700284746"));
        let got: Vec<_> = t.all_instances().iter().map(|i| (i.instance, i.caster, i.start_ms, i.end_ms, i.end)).collect();
        assert_eq!(
            got,
            [
                (293, 1168, 10_974, Some(33_279), End::Recast),
                (293, 15740, 33_279, Some(34_816), End::Recast),
                (293, 16129, 34_816, None, End::Open),
            ]
        );
        // Your share of it, and the track of everyone's Curse together.
        let mine = tracks(&t.all_instances(), |c| c);
        assert_eq!(mine.iter().find(|t| t.owner == 15740).map(|t| (t.start_ms, t.end_ms)), Some((33_279, Some(34_816))));
        let all = tracks(&t.all_instances(), |_| 0);
        assert_eq!(all.iter().map(|t| (t.start_ms, t.end_ms, t.stacks.clone())).collect::<Vec<_>>(), [(10_974, None, vec![(10_974, 1)])]);
    }

    #[test]
    fn a_change_to_another_abnormal_ends_the_old_one_and_starts_the_new() {
        // 2026-11-08, log zDFeeFemDXOBDbNdPgglhA: Earth's Blessing (174000371)
        // added to entity 2660 as instance 889, then a change of instance 889
        // to Power of the Storm (182500411, skill 18250040) by entity 2842.
        let mut t = Timeline::default();
        t.note(81_259, &hex("342a38e4140113f906f3085f0a204e0000000000004296f2baa1010000e41414ef800901009305a14705a13bc733f90d47"));
        t.note(93_203, &hex("332b38e41413f9063bbce00a204e000000000000efc4f2baa10100009a1614387916010282b7a347389937c700f60d47"));
        let got: Vec<_> = t.all_instances().iter().map(|i| (i.instance, i.abnormal, i.caster, i.skill, i.start_ms, i.end_ms)).collect();
        assert_eq!(got[0].0, 889);
        assert_eq!((got[0].1, got[0].2, got[0].4, got[0].5), (174_000_371, 2660, 81_259, Some(93_203)));
        assert_eq!((got[1].1, got[1].2, got[1].3, got[1].4, got[1].5), (182_500_411, 2842, Some(18_250_040), 93_203, None));
    }

    #[test]
    fn a_change_to_an_abnormal_never_seen_added_dates_it_from_its_length() {
        // The 21:15:33.279 Curse change alone: 7 s long, ending at
        // 21:15:40.238 on the server's clock, so it started at 21:15:33.238.
        let mut t = Timeline::default();
        t.note(1_791_346_533_279, &hex("342b3888fd0213a502c5c49e09581b000000000000cefe9214a1010000fc7a0d6c47f6000242f9b6c7d97b2dc700c43d46"));
        let i = &t.all_instances()[0];
        assert_eq!((i.instance, i.caster, i.start_ms, i.server_end_ms), (293, 15740, 1_791_346_533_238, Some(1_791_346_540_238)));
    }

    #[test]
    fn a_map_load_ends_every_abnormal_but_yours() {
        let mut t = Timeline::default();
        t.note(1_000, &hex(STATS_ON));
        t.note(1_000, &hex(BENEDICTION_ON));
        t.note(1_000, &hex("342a38d1db01011303e165a6091027000000000000bc5cba14a1010000e73403300af7000047e44c47b46a22c700e85a46"));
        // 21:58:58.625: map 1010, load 3 of this session.
        t.note(1_500, &hex("34213603000000f20300008bc8da000000000049e14a47ad204dc7004c60465253bd420200000000000000000000000000"));
        let ends: Vec<_> = t.all_instances().iter().map(|i| (i.entity, i.end_ms, i.end)).collect();
        assert_eq!(ends, [(6759, None, End::Open), (28113, Some(1_500), End::MapLoad)]);
    }

    #[test]
    fn a_stack_pushed_out_without_a_record_closes_at_its_timer() {
        // 2026-10-05 14:58:49.395: an Element (163000003) on entity 11297,
        // 30 s. A fifth and sixth Element came before it ended; four newer
        // ones were used up by Elemental Fusion, and no record ended this one.
        let added = 1_791_237_529_395;
        let mut t = Timeline::default();
        t.note(added, &hex("342a38a158011372c32eb7093075000000000000f813140ea1010000919e0101f8c5f5000080f1f1c78035d6c700ccc046"));
        t.note(added + 33_000, &hex(BENEDICTION_OFF));
        let i = &t.all_instances()[0];
        // Its server end, 14:59:19.288, plus the 107 ms the add came late.
        assert_eq!((i.instance, i.end_ms, i.end), (114, Some(1_791_237_559_395), End::Unseen));
    }

    #[test]
    fn a_fifth_element_pushes_out_the_oldest() {
        // 2026-10-05 14:58:49 to 14:59:16, entity 11297: six Elements
        // (163000003, 4 stacks), then Elemental Fusion takes the four newest.
        let records = [
            (1_791_237_529_395, "342a38a158011372c32eb7093075000000000000f813140ea1010000919e0101f8c5f5000080f1f1c78035d6c700ccc046"),
            (1_791_237_534_795, "342a38a158011376c32eb70930750000000000004229140ea1010000919e0101f8c5f5000080f1f1c78035d6c700ccc046"),
            (1_791_237_545_595, "352a38a15801139001c32eb70930750000000000003f53140ea1010000e3bd0301f8c5f5000080f1f1c78035d6c700ccc046"),
            (1_791_237_548_695, "352a38a15801139401c32eb70930750000000000005b5f140ea1010000e3bd0301f8c5f5000080f1f1c78035d6c700ccc046"),
            (1_791_237_555_695, "352a38a15801139a01c32eb7093075000000000000e57a140ea101000097b60201f8c5f500008029f1c780ead5c70068c046"),
            (1_791_237_556_495, "352a38a15801139f01c32eb7093075000000000000057e140ea1010000ffa50101f8c5f500008074f1c78035d6c70098c046"),
            (1_791_237_556_595, "412c38a15804079f010ba158e2b7f8005dd427610790010ba158e2b7f8005dd42761079a010ba158e2b7f8005dd427610794010ba158e2b7f8005dd42761"),
        ];
        let read = |limits: HashMap<u32, u32>| {
            let mut t = Timeline::default();
            t.set_stack_limits(limits);
            for (ms, hex_text) in records {
                t.note(ms, &hex(hex_text));
            }
            t.note(1_791_237_600_000, &hex(BENEDICTION_OFF));
            t.all_instances().iter().map(|i| (i.instance, i.end_ms.unwrap(), i.end)).collect::<Vec<_>>()
        };
        let fusion = End::TakenOff { skill: 16_300_002 };
        assert_eq!(
            read(HashMap::from([(163_000_003, 4)])),
            [
                (114, 1_791_237_555_695, End::PushedOut),
                (118, 1_791_237_556_495, End::PushedOut),
                (144, 1_791_237_556_595, fusion),
                (148, 1_791_237_556_595, fusion),
                (154, 1_791_237_556_595, fusion),
                (159, 1_791_237_556_595, fusion),
            ]
        );
        // Without the limit the two oldest stay on to their timers (plus the
        // 58 ms the last add came late).
        let unlimited = read(HashMap::new());
        assert_eq!(unlimited[..2], [(114, 1_791_237_559_346, End::Unseen), (118, 1_791_237_564_796, End::Unseen)]);
    }

    #[test]
    fn a_removal_read_with_the_push_says_why() {
        // 2026-10-05 14:57:00 at login, entity 11297: Spirit Strike
        // (167100001) at level 1, 4 and 5; each new level comes with a
        // removal of the last, reason 5, in the same read.
        let mut t = Timeline::default();
        t.set_stack_limits(HashMap::from([(167_100_001, 1)]));
        for (ms, hex_text) in [
            (695, "2f2a38a15801110661bef509ffffffffffffffff8075d52abb030000a15801005e71f0c7196dd6c700dcbf46"),
            (795, "2f2a38a15801111061bef509ffffffffffffffff8075d52abb030000a15804005e71f0c7196dd6c700dcbf46"),
            (795, "152c38a15804000f05000a05000805000605"),
            (895, "2f2a38a15801111961bef509ffffffffffffffff8075d52abb030000a15805005e71f0c7196dd6c700dcbf46"),
            (895, "182c38a15805000d05000c05000905001105001005"),
        ] {
            t.note(ms, &hex(hex_text));
        }
        let ends: Vec<_> = t.all_instances().iter().map(|i| (i.instance, i.level, i.end_ms, i.end)).collect();
        assert_eq!(ends, [(6, 1, Some(795), End::Removed(5)), (16, 4, Some(895), End::Removed(5)), (25, 5, None, End::Open)]);
    }

    /// 2026-10-06 21:58:28.121: the Fire Spirit 28113 spawns (`41 36`; the
    /// legion name in its owner block replaced by x's).
    const SPIRIT_SPAWN: &str = "c9014136d1db015f1000b18e2c00400200394b47000f20c700c05b465c5a3943ce8301985598554f0a00004f0a000000\
        0000000000000000000000f837020064000000f04902000100000000000000a08601000000000000e204000101011101\
        40b39809ffffffffffffffff8075d52abb030000e7340d02705c4c47ab1020c79cf25a46070206671a00006c00000000\
        00b1040b787878787878787878787801000200000000000000000000000000000002cd008c000000d000500100002d00\
        0000dd1d030000";

    #[test]
    fn a_spirit_spawn_lists_its_passive_at_the_summon_level() {
        // Fire Spirit (161002304) from the player 6759 at level 13: Summon:
        // Fire Spirit was 10 + 3 from the board on the character page that
        // evening. Water, Wind, Earth and the Ancient Spirit spawned at 13,
        // 13, 12 and 10, also their skills' levels there.
        let Some(Record::Listed { entity, own, list }) = parse(&hex(SPIRIT_SPAWN)) else { panic!() };
        assert_eq!((entity, own), (28113, false));
        let passive = Applied {
            entity: 28113,
            instance: 1,
            abnormal: 161_002_304,
            length_ms: None,
            end_ms: None,
            caster: 6759,
            level: 13,
            skill: None,
            restarted: false,
        };
        assert_eq!(list, [passive]);
    }

    #[test]
    fn a_player_record_lists_buffs_with_and_without_a_caster() {
        // 2026-10-06 21:17:16.025: player 15988's record (`45 36`) cut to its
        // entity and its list: passives, two 5-minute scrolls that name no
        // caster, and a 30 s buff from a skill. The first entry has the byte
        // before the position, the second has none.
        let player = "b3024536f47c08110581db8f0affffffffffffffff8075d52abb030000f47c01008b73d6c7ab544ac7004c0a46110861\
            6f940affffffffffffffff8075d52abb030000f47c018b73d6c7ab544ac7004c0a461009273c5101e093040000000000\
            ae239614a101000001ffff7f7fffff7f7fffff7f7f100a3b3c5101e0930400000000008ecf9614a101000001ffff7f7f\
            ffff7f7fffff7f7f110be1548e0affffffffffffffff8075d52abb030000f47c068b73d6c7ab544ac7004c0a46110c21\
            62910affffffffffffffff8075d52abb030000f47c038b73d6c7ab544ac7004c0a46110dc1e8920affffffffffffffff\
            8075d52abb030000f47c038b73d6c7ab544ac7004c0a46131021ba2f0a307500000000000014df9414a1010000f47c04\
            d0c5040195f3c0c73ad236c71ba628460f";
        let Some(Record::Listed { entity, own, list }) = parse(&hex(player)) else { panic!() };
        assert_eq!((entity, own), (15988, false));
        let got: Vec<_> = list.iter().map(|a| (a.instance, a.abnormal, a.length_ms, a.caster, a.level, a.skill)).collect();
        assert_eq!(
            got,
            [
                (5, 177_200_001, None, 15988, 1, None),
                (8, 177_500_001, None, 15988, 1, None),
                (9, 22_101_031, Some(300_000), 0, 1, None),
                (10, 22_101_051, Some(300_000), 0, 1, None),
                (11, 177_100_001, None, 15988, 6, None),
                (12, 177_300_001, None, 15988, 3, None),
                (13, 177_400_001, None, 15988, 3, None),
                (16, 170_900_001, Some(30_000), 15988, 4, Some(17_090_000)),
            ]
        );
    }

    #[test]
    fn a_spawn_record_starts_what_it_lists_and_ends_what_it_leaves_out() {
        // Spirit's Benediction on the spirit 28113 (21:58:30.225), then its
        // spawn record (21:58:28.121), here out of their real order: the
        // record lists only the passive, so the buff ends there.
        let mut t = Timeline::default();
        t.note(1_000, &hex("342a38d1db01011303e165a6091027000000000000bc5cba14a1010000e73403300af7000047e44c47b46a22c700e85a46"));
        t.note(3_000, &hex(SPIRIT_SPAWN));
        let got: Vec<_> = t.all_instances().iter().map(|i| (i.instance, i.abnormal, i.level, i.start_ms, i.end_ms, i.end)).collect();
        assert_eq!(got, [(3, 161_900_001, 3, 1_000, Some(3_000), End::NotListed), (1, 161_002_304, 13, 3_000, None, End::Open)]);
        // A record that lists the same again keeps the start.
        t.note(4_000, &hex(SPIRIT_SPAWN));
        assert_eq!(t.all_instances().last().map(|i| (i.start_ms, i.end)), Some((3_000, End::Open)));
    }

    #[test]
    fn stacks_join_into_one_track_per_owner() {
        let inst = |instance, caster, start, end: Option<i64>| Instance {
            entity: 1,
            instance,
            abnormal: 167_800_011,
            caster,
            skill: Some(16_780_001),
            level: 2,
            endless: false,
            start_ms: start,
            end_ms: end,
            end: if end.is_some() { End::Expired } else { End::Open },
            server_end_ms: None,
        };
        // Caster 9 is a spirit of player 1.
        let owner = |c| if c == 9 { 1 } else { c };
        let list = [
            inst(1, 1, 0, Some(10)),
            inst(2, 9, 2, Some(10)),
            inst(3, 1, 5, Some(10)),
            inst(4, 1, 20, None),
            inst(5, 7, 3, Some(4)),
        ];
        let t = tracks(&list, owner);
        let got: Vec<_> = t.iter().map(|t| (t.owner, t.start_ms, t.end_ms, t.stacks.clone())).collect();
        assert_eq!(
            got,
            [
                (1, 0, Some(10), vec![(0, 1), (2, 2), (5, 3), (10, 0)]),
                (7, 3, Some(4), vec![(3, 1), (4, 0)]),
                (1, 20, None, vec![(20, 1)]),
            ]
        );
    }

    // The meter's own additions: a bounded timeline, and the stack limits
    // read from the table it ships.

    #[test]
    fn pruning_keeps_what_is_on_and_what_ended_since() {
        let mut t = Timeline::default();
        t.note(1_000, &hex(BENEDICTION_ON));
        t.note(11_042, &hex(BENEDICTION_OFF));
        // The spirit's passive, never ending.
        t.note(12_000, &hex(SPIRIT_SPAWN));
        t.prune(11_042);
        assert_eq!(t.len(), 2);
        t.prune(11_043);
        let left: Vec<_> = t.all_instances().iter().map(|i| (i.abnormal, i.end)).collect();
        assert_eq!(left, [(161_002_304, End::Open)]);
        assert_eq!(t.instances_between(0, 5_000), []);
        assert_eq!(t.instances_between(0, 12_000).len(), 1);
        t.clear();
        assert!(t.is_empty());
    }

    #[test]
    fn a_prune_between_a_push_and_its_removal_keeps_the_reason() {
        let mut t = Timeline::default();
        t.set_stack_limits(HashMap::from([(167_100_001, 1)]));
        // An old instance on another entity, ended long before.
        t.note(100, &hex(BENEDICTION_ON));
        t.note(200, &hex(BENEDICTION_OFF));
        t.note(695, &hex("2f2a38a15801110661bef509ffffffffffffffff8075d52abb030000a15801005e71f0c7196dd6c700dcbf46"));
        t.note(795, &hex("2f2a38a15801111061bef509ffffffffffffffff8075d52abb030000a15804005e71f0c7196dd6c700dcbf46"));
        t.prune(500);
        t.note(795, &hex("152c38a15804000f05000a05000805000605"));
        let ends: Vec<_> = t.all_instances().iter().map(|i| (i.instance, i.end)).collect();
        assert_eq!(ends, [(6, End::Removed(5)), (16, End::Open)]);
    }

    #[test]
    fn stack_limits_read_from_the_table() {
        let table = r#"{"abnormals":{"103":{"icon":"x","stacks":3},"100":{"icon":"y"},"163000003":{"stacks":4}}}"#;
        assert_eq!(stack_limits(table), HashMap::from([(103, 3), (163_000_003, 4)]));
        assert_eq!(stack_limits("not json"), HashMap::new());
        let shipped = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../src/data/abnormals.json")).unwrap();
        assert_eq!(stack_limits(&shipped).get(&163_000_003), Some(&4), "Element");
    }
}

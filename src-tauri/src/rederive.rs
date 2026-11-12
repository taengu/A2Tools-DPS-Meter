//! Deriving a fight from an uploaded Evidence Slice.
//!
//! This is what the log service actually runs. A client sends packets and a
//! summary; the summary is never trusted, because the client is open source and
//! a fork can put any number in it. Instead the service replays the packets
//! through *this* parser — the same code, compiled to `wasm32-unknown-unknown` —
//! and publishes what it derives.
//!
//! That is the whole reason the crate splits on the `desktop` feature and why CI
//! builds this half for wasm32. Nothing here may reach for Tauri, pcap, HTTP or
//! the Windows API.
//!
//! What it is worth being precise about: this proves the numbers were not typed
//! in, and that they came from published code. It does **not** prove the packets
//! are real — a determined forger can synthesise a self-consistent stream, and
//! `docs/INTEGRITY.md` says so. Corroboration between independent witnesses is
//! what raises that bar; this raises the floor.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::capture::evidence_slice;
use crate::capture::stream_processor::StreamProcessor;
use crate::combat::data_storage::DataStorage;
use crate::combat::dps_calculator::DpsCalculator;
use crate::combat::ping_tracker::PingTracker;
use crate::entity::damage_packet::ParsedDamagePacket;
use crate::entity::fight_record::FightRecord;
use crate::entity::special_damage::SpecialDamage;
use crate::i18n::lookup::{NpcLookup, SkillLookup};

/// One participant, as re-derived. Identified by the blinded token the slice
/// carries, never by a name — the slice does not contain one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DerivedActor {
    /// Session-scoped entity id. Meaningless across uploads; useful only for
    /// joining rows within this one.
    pub actor_id: i32,
    /// The blinded name as it appears in the slice, which the blind map relates
    /// back to a roster id.
    pub token: String,
    pub job_id: i32,
    pub damage: i64,
    pub dps: f64,
}

/// One target and what was done to it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DerivedTarget {
    pub target_id: i32,
    pub mob_code: i32,
    pub total_damage: i64,
    pub duration_ms: i64,
    pub actors: Vec<DerivedActor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DerivedEncounter {
    /// Which build derived this. Recorded rather than assumed: when the parser
    /// changes, old results are not retroactively invalidated — they are
    /// attributed to the version that produced them.
    pub parser_version: String,
    pub dungeon_id: i32,
    pub total_damage: i64,
    pub duration_ms: i64,
    pub targets: Vec<DerivedTarget>,
    /// Roster ids the slice declared, keyed by the token that replaced each
    /// name. The service joins these to accounts; the parser never sees a name.
    pub blind_map: HashMap<String, u64>,
    /// Packets the slice contained. A sanity signal, not a trust signal.
    pub records: usize,
}

#[derive(Debug, PartialEq)]
pub enum DeriveError {
    /// Not an Evidence Slice, or a version this build does not read.
    NotASlice,
    /// Parsed, but produced no damage — nothing to publish.
    NothingDerived,
    /// Parsed, and the boss is known, but it was fought for less than the
    /// 5 seconds a boss fight needs (`short_boss_fight`): a boss tagged in
    /// passing, not a fight a log is for.
    ShortFight,
}

impl std::fmt::Display for DeriveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeriveError::NotASlice => write!(f, "not an evidence slice"),
            DeriveError::NothingDerived => write!(f, "no damage derived from the slice"),
            DeriveError::ShortFight => write!(f, "the boss was fought for less than 5 seconds"),
        }
    }
}

/// Replay a slice and report what the parser makes of it.
///
/// Time comes from the slice's own per-record offsets, never a clock: on wasm32
/// there is no clock to read, and re-derivation has to be reproducible or the
/// same upload could produce different fights on different days.
pub fn derive(slice: &[u8]) -> Result<DerivedEncounter, DeriveError> {
    let (records, blind_map) = evidence_slice::decode(slice).ok_or(DeriveError::NotASlice)?;

    let storage = Arc::new(DataStorage::new());
    let mut processor = StreamProcessor::new(
        storage.clone(),
        Arc::new(SkillLookup::new()),
        Arc::new(NpcLookup::new()),
    );

    for (dt_ms, packet) in &records {
        // Offsets are relative to the fight start and can be negative during the
        // lead-in. The parser only ever compares timestamps, so a relative
        // timeline behaves identically to an absolute one and leaks no clock.
        processor.set_override_timestamp(Some(*dt_ms as i64));
        processor.consume_stream(packet);
    }
    processor.set_override_timestamp(None);

    let combat = storage.get_combat_snapshot_light();
    let mob_data = storage.get_mob_data();
    let nicknames = storage.get_nicknames();

    let mut targets: Vec<DerivedTarget> = Vec::new();
    let mut total_damage = 0i64;
    let mut duration_ms = 0i64;

    for (target_id, target) in &combat {
        if target.total_damage <= 0 {
            continue;
        }
        let span = (target.last_damage_time - target.first_damage_time).max(0);
        let seconds = (span as f64 / 1000.0).max(0.001);

        let mut actors: Vec<DerivedActor> = target
            .actors
            .iter()
            .filter(|(_, a)| a.total_damage > 0)
            .map(|(&actor_id, a)| DerivedActor {
                actor_id,
                token: nicknames.get(&actor_id).cloned().unwrap_or_default(),
                job_id: a.job.map(|j| j.class_prefix()).unwrap_or(0),
                damage: a.total_damage,
                dps: a.total_damage as f64 / seconds,
            })
            .collect();
        actors.sort_by(|a, b| b.damage.cmp(&a.damage).then(a.actor_id.cmp(&b.actor_id)));

        total_damage += target.total_damage;
        duration_ms = duration_ms.max(span);
        targets.push(DerivedTarget {
            target_id: *target_id,
            mob_code: mob_data.get(target_id).copied().unwrap_or(0),
            total_damage: target.total_damage,
            duration_ms: span,
            actors,
        });
    }

    if targets.is_empty() {
        return Err(DeriveError::NothingDerived);
    }
    // Deterministic order: a HashMap's iteration order is not stable, and the
    // service hashes this structure.
    targets.sort_by(|a, b| b.total_damage.cmp(&a.total_damage).then(a.target_id.cmp(&b.target_id)));

    Ok(DerivedEncounter {
        parser_version: crate::entity::fight_record::APP_VERSION.to_string(),
        dungeon_id: storage.current_dungeon_id(),
        total_damage,
        duration_ms,
        targets,
        blind_map,
        records: records.len(),
    })
}

/// A whole fight, re-derived: the same record the meter saves, so the site can
/// draw the same Details view from it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DerivedFight {
    pub parser_version: String,
    /// The boss fight, exactly as `DpsCalculator` snapshots one on the desktop.
    /// Its `actors[].nickname` values are blinded tokens (and, for everyone but
    /// a detected local player, masked tokens): meaningless for display. The
    /// service replaces them by `actorId` with the names the uploader chose to
    /// show, which are cosmetic; every number here is derived.
    pub record: FightRecord,
    /// The record's `total_damage` is an i32 and wraps past ~2.1 billion. This
    /// is the same total, as the parser actually summed it.
    pub total_damage: i64,
    /// Sorted, so the serialised record is byte-identical on every run.
    pub blind_map: std::collections::BTreeMap<String, u64>,
    pub records: usize,
    /// What the server checked about the slice itself, whoever built it. The
    /// service reports; the site decides what to refuse or keep off the
    /// leaderboards (see `SliceChecks`).
    pub checks: SliceChecks,
}

/// Checks on the slice that do not depend on the client that cut it.
///
/// The service re-derives every number, but only from what the client chose
/// to put in the slice: a client that drops records, or one whose capture
/// reads garbage (Ethernet padding taken for payload, 2026-10-06), gives
/// numbers that are faithfully derived and wrong. And a client that does not
/// blind, or blinds by older rules, would have names stored. The service
/// reports; the site decides what to refuse or keep off the leaderboards.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SliceChecks {
    /// Name-shaped runs that are not tokens the slice declares: names the
    /// blinder should have replaced. 0 for a slice this meter cut.
    pub unblinded_names: usize,
    /// Top-level records in the slice.
    pub records: usize,
    /// Of those, records lifted out of other packets (`LIFTED_HOST`). A clean
    /// capture needs a few: on the captures checked, 0.05-4% of records. A
    /// capture that loses the game's framing has its packets read as blobs,
    /// and nearly everything recovered from them is lifted: 84-97% on the
    /// padding-corrupted capture of 2026-10-06, whose kills read 77-80%.
    pub lifted: usize,
    /// Compressed bundles kept whole. Hundreds in a clean boss fight; a
    /// garbled capture keeps almost none (3 to 29 on that capture).
    pub bundles: usize,
    /// The target's death is in the slice: the fight was a kill.
    pub killed: bool,
    /// The target's max HP as the parser read it (0 when unknown).
    pub max_hp: i64,
    /// All damage the parser found on the target, unplaced summons included,
    /// against `max_hp`: on a kill, at least the max HP (more when the boss
    /// healed or the last hit overshot). Short of it, records are missing.
    pub damage: i64,
    /// The slice had no spawn for the boss, which was named by the adds that
    /// spawned during the fight (`boss_from_adds`). Written only when true,
    /// so every other fight's checks are unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    pub boss_named_by_adds: bool,
    /// The slice had no spawn for the boss, which was named by the boss the
    /// uploader's meter saw (`Claim`, `claimed_boss`), as nothing in the
    /// slice contradicted it. Written only when true.
    #[serde(default, skip_serializing_if = "is_false")]
    pub boss_named_by_client: bool,
    /// The dungeon's clear (`00 61` state 3, sent as its final boss dies)
    /// came this many ms after the fight's last hit (negative: before), when
    /// one came within `CLEAR_WINDOW_MS` of it. `killed` is then true even
    /// with no death record for the target, which a slice can lack. Written
    /// only when present, so slices without one are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleared_after_ms: Option<i64>,
}

/// How far from the fight's last hit a dungeon's clear may come and still be
/// this fight's: the clear follows the final boss's death within seconds (the
/// Shugo Festival's 3 s), and a fight's last hit is its killing blow.
const CLEAR_WINDOW_MS: std::ops::RangeInclusive<i64> = -3_000..=15_000;

/// When the dungeon's clear came, relative to `last_hit_ms` (both on the
/// slice's clock), if one came within `CLEAR_WINDOW_MS` of it: the arrival of
/// a `00 61` record whose state is 3.
fn clear_near(records: &[(i32, Vec<u8>)], last_hit_ms: i64) -> Option<i64> {
    records
        .iter()
        .filter(|(_, p)| {
            let len = crate::capture::stream_processor::read_varint(p, 0);
            usize::try_from(len.length).ok().filter(|&n| n > 0).is_some_and(|at| {
                p.get(at..at + 2) == Some(&[0x00, 0x61][..]) && p.get(at + 6) == Some(&3)
            })
        })
        .map(|(dt, _)| i64::from(*dt) - last_hit_ms)
        .filter(|after| CLEAR_WINDOW_MS.contains(after))
        .min_by_key(|after| after.abs())
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A boss known by the adds its fight spawns: `adds` are NPC codes that spawn
/// during this boss's fights and no other boss's of the same instance.
struct BossSign {
    dungeon: i32,
    boss: i32,
    adds: &'static [i32],
}

/// Abyssal Forge (Ludra). Its bosses spawn as the party enters, and a run
/// lasts up to an hour (two from 2026-11-11), so a boss pulled more than the
/// slice's half-hour prelude after entering reached the service with no spawn:
/// its fight replayed as an anonymous target and was refused as "no boss
/// fight", ~6,000 uploads by 2026-11-12. The meter's spawn ledger keeps the
/// spawn from then on; this names the boss in the slices already sent.
/// Learned from known logs (tools/a2t_why.rs: the NPCs spawned during each
/// fight; codes in at least 30% of one boss's fights and in none of another's)
/// and checked five-fold, each fight scored by signs learned without it
/// (2026-11-12): Normal 140 of 160 named right, Hard 75 of 123, none wrong;
/// the rest name no boss and stay refused. Fanatical Urugugu Canyon's bosses
/// lose their spawn the same way: its tiers 72 of 100, 74 of 100 and 75 of 92,
/// none wrong.
const BOSS_SIGNS: &[BossSign] = &[
    // Isolation Specialist Rahu
    BossSign {
        dungeon: 600082,
        boss: 2301006,
        adds: &[
            2920717, 2920718, 2920719, 2920720, 2920722, 2920723, 2920724, 2920725,
        ],
    },
    // Abyssal Wings Ketu
    BossSign {
        dungeon: 600082,
        boss: 2301007,
        adds: &[
            2920668, 2920669, 2920670, 2920671, 2920674, 2920675, 2920775, 2920808,
        ],
    },
    // Central Abyss Fusion Reactor
    BossSign { dungeon: 600082, boss: 2301008, adds: &[2301001, 2301002, 2301003, 2301004, 2301010, 2301011] },
    // Eternal Ludra
    BossSign {
        dungeon: 600082,
        boss: 2301014,
        adds: &[
            2920745, 2920746, 2920748, 2920750, 2920751, 2920752, 2920753, 2920754, 2920755, 2920757, 2920760,
            2920763, 2920764, 2920765, 2920769, 2920770, 2920771, 2920772, 2920792, 2920793, 2920805, 2920806,
            2920810,
        ],
    },
    // Isolation Specialist Rahu (Hard)
    BossSign { dungeon: 600083, boss: 2300282, adds: &[2920717, 2920720, 2920725] },
    // Abyssal Wings Ketu (Hard)
    BossSign { dungeon: 600083, boss: 2300283, adds: &[2920671, 2920674, 2920675, 2920775] },
    // Central Abyss Fusion Reactor (Hard)
    BossSign { dungeon: 600083, boss: 2300284, adds: &[2300277, 2300278, 2300279, 2300280, 2300286, 2300287] },
    // Eternal Ludra (Hard)
    BossSign {
        dungeon: 600083,
        boss: 2300290,
        adds: &[
            2920654, 2920745, 2920746, 2920748, 2920750, 2920751, 2920752, 2920753, 2920754, 2920755, 2920757,
            2920770, 2920771, 2920772, 2920793, 2920805, 2920806, 2920810, 2921505,
        ],
    },
    // Fanatical Urugugu Canyon, its three tiers (the Mau Totem spawns no adds of its own: never named)
    // Watchdog Kwapo (600091)
    BossSign {
        dungeon: 600091,
        boss: 2310908,
        adds: &[
            2920580, 2920581, 2920582, 2920583, 2920728, 2920730, 2920842, 2921150,
        ],
    },
    // Necromancer Duanka (600091)
    BossSign { dungeon: 600091, boss: 2310909, adds: &[2920589, 2920590, 2920591, 2920598, 2920726, 2920777] },
    // Ferocious Horn Nuakum (600091)
    BossSign { dungeon: 600091, boss: 2310910, adds: &[2920603, 2920604] },
    // Watchdog Kwapo (600092)
    BossSign { dungeon: 600092, boss: 2300908, adds: &[2920580, 2920581, 2920582, 2920583, 2920607, 2920728, 2921150] },
    // Necromancer Duanka (600092)
    BossSign { dungeon: 600092, boss: 2300909, adds: &[2920589, 2920590, 2920591, 2920592, 2920598, 2920726, 2920777] },
    // Ferocious Horn Nuakum (600092)
    BossSign { dungeon: 600092, boss: 2300910, adds: &[2920400, 2920601, 2920603, 2920604, 2920605] },
    // Watchdog Kwapo (600093)
    BossSign {
        dungeon: 600093,
        boss: 2320908,
        adds: &[
            2920580, 2920581, 2920582, 2920583, 2920607, 2920728, 2920842, 2921150,
        ],
    },
    // Necromancer Duanka (600093)
    BossSign {
        dungeon: 600093,
        boss: 2320909,
        adds: &[
            2920589, 2920590, 2920591, 2920592, 2920593, 2920598, 2920631, 2920726, 2920777,
        ],
    },
    // Ferocious Horn Nuakum (600093)
    BossSign { dungeon: 600093, boss: 2320910, adds: &[2920400, 2920601, 2920602, 2920603, 2920604] },
    // Every other dungeon whose bosses spawn adds of their own: learned the same way from 25 known
    // logs a boss (2026-11-13), kept only for dungeons with no fight named wrong.
    // Ultimate Berk
    BossSign { dungeon: 600001, boss: 2300171, adds: &[2920249, 2920311, 2920337, 2920439, 2920503, 2920665] },
    // Ultimate Berk
    BossSign { dungeon: 600002, boss: 2310171, adds: &[2920249, 2920311, 2920337, 2920665] },
    // Ultimate Berk
    BossSign { dungeon: 600004, boss: 2320171, adds: &[2920439, 2920665, 2920906, 2920913, 2920922, 2920924] },
    // Divine Auldor
    BossSign { dungeon: 600011, boss: 2310218, adds: &[2920207, 2920208, 2920209, 2920215] },
    // Divine Auldor
    BossSign { dungeon: 600012, boss: 2300218, adds: &[2920206, 2920207, 2920208, 2920209, 2920215, 2920262] },
    // Guardian Captain Raur
    BossSign { dungeon: 600013, boss: 2320206, adds: &[2920062, 2920214] },
    // Divine Auldor
    BossSign { dungeon: 600013, boss: 2320218, adds: &[2920206, 2920207, 2920208, 2920209, 2920215, 2920262] },
    // Kromede's Desire
    BossSign { dungeon: 600022, boss: 2300371, adds: &[2920339, 2920340, 2920342, 2920343, 2920345, 2920346, 2920347] },
    // Kromede's Desire
    BossSign {
        dungeon: 600023,
        boss: 2320371,
        adds: &[
            2920339, 2920340, 2920342, 2920343, 2920345, 2920346, 2920348, 2920904, 2920905,
        ],
    },
    // Phantasmal Lakshmi
    BossSign { dungeon: 600031, boss: 2310401, adds: &[2310402, 2310404, 2920027] },
    // Predator Saraswati
    BossSign { dungeon: 600031, boss: 2310403, adds: &[2310423, 2920048] },
    // Transcendent Bakarma
    BossSign { dungeon: 600031, boss: 2310471, adds: &[2920254, 2920258, 2920259] },
    // Phantasmal Lakshmi
    BossSign { dungeon: 600032, boss: 2300401, adds: &[2300402, 2920027] },
    // Predator Saraswati
    BossSign { dungeon: 600032, boss: 2300403, adds: &[2300420, 2300424, 2300426, 2920048] },
    // Transcendent Bakarma
    BossSign { dungeon: 600032, boss: 2300471, adds: &[2920254, 2920257, 2920258, 2920259, 2920260, 2920279, 2920305] },
    // Phantasmal Lakshmi
    BossSign { dungeon: 600033, boss: 2320401, adds: &[2300402, 2920027] },
    // Transcendent Bakarma
    BossSign {
        dungeon: 600033,
        boss: 2320471,
        adds: &[
            2920254, 2920257, 2920258, 2920259, 2920260, 2920279, 2920300, 2920305,
        ],
    },
    // Broodkeeper Nayatman
    BossSign { dungeon: 600042, boss: 2310508, adds: &[2920407, 2920408, 2920409] },
    // Decaying Durvati
    BossSign { dungeon: 600042, boss: 2310509, adds: &[2920154, 2920356, 2920626, 2920650, 2920651, 2920931] },
    // Atiel
    BossSign {
        dungeon: 600053,
        boss: 2320601,
        adds: &[
            2920411, 2920412, 2920413, 2920414, 2920415, 2920445, 2920446, 2920449, 2920465, 2920467, 2920469,
            2920470,
        ],
    },
    // Nazmun
    BossSign {
        dungeon: 600053,
        boss: 2320615,
        adds: &[
            2320608, 2920440, 2920441, 2920444, 2920485, 2920505, 2920506, 2920507,
        ],
    },
    // Gelcos
    BossSign { dungeon: 600053, boss: 2320616, adds: &[2320610, 2320617, 2920361, 2920369, 2920400] },
    // Atiel
    BossSign {
        dungeon: 600055,
        boss: 2340601,
        adds: &[
            2920411, 2920412, 2920413, 2920414, 2920415, 2920445, 2920446, 2920449, 2920465, 2920467, 2920469,
            2920470,
        ],
    },
    // Nazmun
    BossSign {
        dungeon: 600055,
        boss: 2340615,
        adds: &[
            2340608, 2920440, 2920441, 2920444, 2920485, 2920505, 2920506, 2920507,
        ],
    },
    // Gelcos
    BossSign { dungeon: 600055, boss: 2340616, adds: &[2340610, 2340617, 2920361, 2920369, 2920400] },
    // Atiel
    BossSign {
        dungeon: 600058,
        boss: 2343601,
        adds: &[
            2920411, 2920412, 2920413, 2920414, 2920415, 2920445, 2920446, 2920449, 2920465, 2920467, 2920469,
            2920470,
        ],
    },
    // Nazmun
    BossSign {
        dungeon: 600058,
        boss: 2343615,
        adds: &[
            2343608, 2920440, 2920441, 2920444, 2920485, 2920505, 2920506, 2920507,
        ],
    },
    // Gelcos
    BossSign { dungeon: 600058, boss: 2343616, adds: &[2343610, 2343617, 2920361, 2920369, 2920400] },
    // Siliator of Deceit
    BossSign { dungeon: 600063, boss: 2330707, adds: &[2920393, 2920394, 2920395, 2920410, 2920466] },
    // Ruthilis of Pain
    BossSign { dungeon: 600063, boss: 2330708, adds: &[2920355, 2920480, 2920481, 2920482, 2920483, 2920484] },
    // Talisra of the Void
    BossSign {
        dungeon: 600063,
        boss: 2330709,
        adds: &[
            2920519, 2920520, 2920521, 2920522, 2920526, 2920527, 2920539, 2920540, 2920541, 2920542, 2920563,
            2920564, 2920936, 2920937,
        ],
    },
    // Siliator of Deceit
    BossSign { dungeon: 600065, boss: 2350707, adds: &[2920393, 2920394, 2920395, 2920410, 2920466] },
    // Ruthilis of Pain
    BossSign { dungeon: 600065, boss: 2350708, adds: &[2920355, 2920480, 2920481, 2920482, 2920483, 2920484] },
    // Talisra of the Void
    BossSign {
        dungeon: 600065,
        boss: 2350709,
        adds: &[
            2920519, 2920520, 2920521, 2920522, 2920526, 2920527, 2920539, 2920540, 2920541, 2920542, 2920563,
            2920564, 2920936, 2920937,
        ],
    },
    // Siliator of Deceit
    BossSign { dungeon: 600068, boss: 2353707, adds: &[2920393, 2920394, 2920395, 2920410, 2920466] },
    // Ruthilis of Pain
    BossSign { dungeon: 600068, boss: 2353708, adds: &[2920355, 2920480, 2920481, 2920482, 2920483, 2920484] },
    // Talisra of the Void
    BossSign {
        dungeon: 600068,
        boss: 2353709,
        adds: &[
            2920519, 2920520, 2920521, 2920522, 2920523, 2920524, 2920526, 2920527, 2920539, 2920540, 2920541,
            2920542, 2920563, 2920564, 2920682, 2920683, 2920684, 2920685, 2920686,
        ],
    },
    // Tiere
    BossSign { dungeon: 600071, boss: 2310810, adds: &[2920362, 2920363, 2920364, 2920366] },
    // Thamon
    BossSign { dungeon: 600071, boss: 2310811, adds: &[2310805, 2310813, 2920371, 2920474] },
    // Tiere
    BossSign {
        dungeon: 600072,
        boss: 2300810,
        adds: &[
            2300802, 2300808, 2300814, 2920362, 2920363, 2920364, 2920366, 2920400,
        ],
    },
    // Thamon
    BossSign { dungeon: 600072, boss: 2300811, adds: &[2300817, 2300818, 2920371, 2920474] },
    // Tiere
    BossSign { dungeon: 600073, boss: 2320810, adds: &[2920362, 2920363, 2920364, 2920366, 2920400] },
    // Thamon
    BossSign { dungeon: 600073, boss: 2320811, adds: &[2300817, 2300818, 2920371, 2920474] },
    // Afflicted Bakarma
    BossSign { dungeon: 600122, boss: 2311301, adds: &[2920190, 2920993, 2920994] },
    // Multiverse Veltiras
    BossSign { dungeon: 600122, boss: 2311302, adds: &[2920612, 2920614, 2920850] },
    // Glassvein
    BossSign { dungeon: 600131, boss: 2301401, adds: &[2920917, 2920919, 2921012] },
    // Nazarak
    BossSign { dungeon: 600131, boss: 2301402, adds: &[2920942, 2920969, 2921021, 2921126, 2921213] },
    // Nazarak
    BossSign { dungeon: 600132, boss: 2311402, adds: &[2920942, 2920969, 2921126, 2921169, 2921213] },
    // Nathara
    BossSign { dungeon: 600132, boss: 2311403, adds: &[2920958, 2921004, 2921005, 2921156] },
    // Nazarak
    BossSign {
        dungeon: 600133,
        boss: 2321402,
        adds: &[
            8205, 2321401, 2321404, 2321405, 2321406, 2321407, 2321408, 2321409, 2321412, 2321414, 2321418,
            2920915, 2921219,
        ],
    },
    // Nathara
    BossSign {
        dungeon: 600133,
        boss: 2321403,
        adds: &[
            2920935, 2920958, 2920971, 2921004, 2921005, 2921132, 2921135, 2921136, 2921156,
        ],
    },
    // Wise Baumnute
    BossSign { dungeon: 600141, boss: 2300189, adds: &[2920842, 2921002, 2921182, 2921216] },
    // Expanding Xyrepe
    BossSign {
        dungeon: 600141,
        boss: 2300190,
        adds: &[
            2910045, 2920963, 2920964, 2920965, 2920966, 2920967, 2920968, 2921001, 2921014, 2921015, 2921016,
            2921204,
        ],
    },
    // Awakened Alturadon
    BossSign {
        dungeon: 600142,
        boss: 2300200,
        adds: &[
            2921027, 2921028, 2921029, 2921030, 2921053, 2921186, 2921188, 2921209,
        ],
    },
    // Wise Baumnute
    BossSign { dungeon: 600142, boss: 2300219, adds: &[2920842, 2921002, 2921182, 2921216] },
    // Expanding Xyrepe
    BossSign {
        dungeon: 600142,
        boss: 2300230,
        adds: &[
            2920963, 2920964, 2920965, 2920966, 2920967, 2920968, 2921001, 2921014, 2921015, 2921016,
        ],
    },
    // Willful Turgen
    BossSign { dungeon: 600145, boss: 2301701, adds: &[2921025, 2921376] },
    // Forbidden Hexbeast Griosa
    BossSign { dungeon: 600145, boss: 2301702, adds: &[2921145, 2921146, 2921151, 2921316] },
    // Basilus the False
    BossSign {
        dungeon: 600145,
        boss: 2301703,
        adds: &[
            2921248, 2921249, 2921250, 2921251, 2921254, 2921260, 2921264, 2921265, 2921266, 2921267, 2921268,
            2921269, 2921272, 2921317, 2921359, 2921383, 2921384, 2921385, 2921386, 2921391,
        ],
    },
    // Willful Turgen
    BossSign { dungeon: 600146, boss: 2301721, adds: &[2921025, 2921341, 2921376] },
    // Forbidden Hexbeast Griosa
    BossSign { dungeon: 600146, boss: 2301722, adds: &[2921145, 2921146, 2921151, 2921316] },
    // Basilus the False
    BossSign {
        dungeon: 600146,
        boss: 2301723,
        adds: &[
            2921248, 2921249, 2921250, 2921251, 2921252, 2921253, 2921254, 2921255, 2921256, 2921257, 2921260,
            2921264, 2921265, 2921266, 2921267, 2921268, 2921269, 2921271, 2921272, 2921278, 2921359, 2921383,
            2921384, 2921385, 2921386, 2921391, 2921777,
        ],
    },
    // Prideful Atiel
    BossSign { dungeon: 600162, boss: 2301748, adds: &[2921293, 2921295, 2921297, 2921298, 2921300, 2921302, 2921303] },
    // Lab Director Zylus
    BossSign { dungeon: 600162, boss: 2301749, adds: &[2921482, 2921483, 2921484, 2921485, 2921486, 2921487, 2921488] },
    // Overseer Gromkas
    BossSign { dungeon: 600162, boss: 2301750, adds: &[2920369, 2920400, 2921460] },
    // Prideful Atiel
    BossSign {
        dungeon: 600163,
        boss: 2300481,
        adds: &[
            2921293, 2921295, 2921296, 2921297, 2921299, 2921302, 2921304, 2921511,
        ],
    },
    // Lab Director Zylus
    BossSign { dungeon: 600163, boss: 2300555, adds: &[2921482, 2921483, 2921484, 2921486, 2921487, 2921488] },
    // Overseer Gromkas
    BossSign { dungeon: 600163, boss: 2300556, adds: &[2920369, 2920400, 2921460] },
    // Resurrected Auldor
    BossSign {
        dungeon: 600192,
        boss: 2300948,
        adds: &[
            2921620, 2921621, 2921622, 2921626, 2921627, 2921628, 2921629, 2921630, 2921631, 2921650, 2921733,
            2921734, 2921735, 2921736, 2921742,
        ],
    },
    // Resurrected Auldor
    BossSign {
        dungeon: 600193,
        boss: 2300970,
        adds: &[
            2921620, 2921621, 2921622, 2921623, 2921624, 2921626, 2921627, 2921628, 2921629, 2921630, 2921631,
            2921650, 2921656, 2921657, 2921674, 2921742,
        ],
    },
    // Submerged Emon
    BossSign { dungeon: 610010, boss: 2301501, adds: &[2920650, 2920658, 2920740, 2920786, 2920787, 2920811] },
    // Singing Ellyde
    BossSign { dungeon: 610010, boss: 2301503, adds: &[2920880, 2920881] },
    // Rotar
    BossSign { dungeon: 610020, boss: 2300259, adds: &[2921032, 2921033, 2921034, 2921035, 2921051] },
    // Kromede's Desolation
    BossSign {
        dungeon: 610020,
        boss: 2300261,
        adds: &[
            2921099, 2921104, 2921105, 2921108, 2921109, 2921110, 2921111, 2921139, 2921205,
        ],
    },
    // Rotar
    BossSign {
        dungeon: 610021,
        boss: 2300299,
        adds: &[
            2921032, 2921033, 2921034, 2921035, 2921037, 2921038, 2921040, 2921051,
        ],
    },
    // Kromede's Desolation
    BossSign {
        dungeon: 610021,
        boss: 2300317,
        adds: &[
            2921099, 2921104, 2921105, 2921106, 2921107, 2921108, 2921109, 2921110, 2921111, 2921139, 2921205,
            2921206, 2921224,
        ],
    },
    // Rotar
    BossSign {
        dungeon: 610023,
        boss: 2300353,
        adds: &[
            2921032, 2921033, 2921034, 2921035, 2921037, 2921038, 2921039, 2921040, 2921042, 2921043, 2921051,
        ],
    },
    // Kromede's Desolation
    BossSign {
        dungeon: 610023,
        boss: 2300355,
        adds: &[
            2921099, 2921102, 2921103, 2921104, 2921105, 2921106, 2921107, 2921108, 2921109, 2921110, 2921111,
            2921139, 2921205, 2921206, 2921207, 2921224,
        ],
    },
    // Askran
    BossSign {
        dungeon: 610030,
        boss: 2300436,
        adds: &[
            2920400, 2921360, 2921361, 2921363, 2921364, 2921365, 2921366, 2921369,
        ],
    },
    // Incomplete Braunt
    BossSign { dungeon: 610030, boss: 2300437, adds: &[2921281, 2921286, 2921403, 2921404, 2921405] },
    // Mad Klominster
    BossSign { dungeon: 610030, boss: 2300438, adds: &[2921389, 2921462] },
    // Askran
    BossSign {
        dungeon: 610033,
        boss: 2300445,
        adds: &[
            2920400, 2921361, 2921363, 2921364, 2921365, 2921366, 2921369, 2921370, 2921371, 2921372, 2921373,
        ],
    },
    // Incomplete Braunt
    BossSign { dungeon: 610033, boss: 2300446, adds: &[2300590, 2921281, 2921403, 2921404, 2921405] },
    // Mad Klominster
    BossSign { dungeon: 610033, boss: 2300447, adds: &[2921389, 2921462] },
    // Broodkeeper Nayatman
    BossSign { dungeon: 610044, boss: 2300154, adds: &[2300146, 2300149, 2300152, 2920407, 2920408, 2920409] },
    // Decaying Durvati
    BossSign {
        dungeon: 610044,
        boss: 2300155,
        adds: &[
            2920356, 2920357, 2920358, 2920359, 2920384, 2920931, 2921009, 2921011,
        ],
    },
    // Atiel
    BossSign {
        dungeon: 610052,
        boss: 2346601,
        adds: &[
            2920411, 2920412, 2920413, 2920414, 2920415, 2920445, 2920446, 2920449, 2920465, 2920467, 2920469,
            2920470,
        ],
    },
    // Nazmun
    BossSign { dungeon: 610052, boss: 2346615, adds: &[2920440, 2920441, 2920444, 2920485, 2920505, 2920506, 2920507] },
    // Gelcos
    BossSign { dungeon: 610052, boss: 2346616, adds: &[2346610, 2346617, 2346618, 2920361, 2920369, 2920400] },
    // Siliator of Deceit
    BossSign { dungeon: 610062, boss: 2356707, adds: &[2920393, 2920394, 2920395, 2920410, 2920466] },
    // Ruthilis of Pain
    BossSign { dungeon: 610062, boss: 2356708, adds: &[2920355, 2920480, 2920481, 2920482, 2920483, 2920484] },
    // Talisra of the Void
    BossSign {
        dungeon: 610062,
        boss: 2356709,
        adds: &[
            2920519, 2920522, 2920523, 2920524, 2920526, 2920527, 2920539, 2920540, 2920541, 2920542, 2920563,
            2920564, 2920687, 2920688, 2920689, 2920690, 2920691, 2920692, 2920693, 2920694, 2920695, 2920696,
            2920697, 2920698,
        ],
    },
    // Tyrant Ulmar
    BossSign {
        dungeon: 610110,
        boss: 2300792,
        adds: &[
            2920737, 2920834, 2921594, 2921595, 2921596, 2921598, 2921600, 2921601, 2921603, 2921604, 2921605,
            2921609, 2921781,
        ],
    },
    // Tyrant Ulmar
    BossSign {
        dungeon: 610111,
        boss: 2300890,
        adds: &[
            2921594, 2921595, 2921596, 2921597, 2921598, 2921599, 2921600, 2921601, 2921603, 2921605, 2921607,
            2921608, 2921609, 2921781,
        ],
    },
    // Scribe Lipika
    BossSign { dungeon: 610111, boss: 2300891, adds: &[2921602, 2921606, 2921648, 2921671, 2921672, 2921673, 2921685] },
    // Gluttonous Khurbas
    BossSign { dungeon: 610111, boss: 2300892, adds: &[2300890, 2921379, 2921380, 2921381, 2921670] },
    // Black Blood Blatt
    BossSign {
        dungeon: 620011,
        boss: 2301207,
        adds: &[
            2920854, 2920860, 2920861, 2920902, 2920944, 2920949, 2920951, 2920952, 2920953, 2920955,
        ],
    },
    // Terminator Bargott
    BossSign {
        dungeon: 620011,
        boss: 2301208,
        adds: &[
            20, 2920790, 2920972, 2920974, 2920995, 2920996, 2921017, 2921049, 2921069, 2921096, 2921097,
        ],
    },
    // Devouring Klawfly
    BossSign {
        dungeon: 620011,
        boss: 2301209,
        adds: &[
            2920890, 2920891, 2920903, 2920957, 2920986, 2920987, 2920990, 2921045, 2921075, 2921083,
        ],
    },
    // Iscariel
    BossSign {
        dungeon: 620021,
        boss: 2301059,
        adds: &[
            2921172, 2921173, 2921175, 2921177, 2921178, 2921179, 2921183, 2921184, 2921189, 2921192, 2921193,
            2921194, 2921195, 2921199, 2921230, 2921231, 2921234, 2921259, 2921263, 2921279, 2921291, 2921292,
            2921306, 2921307,
        ],
    },
    // Kaldrix
    BossSign {
        dungeon: 620021,
        boss: 2301060,
        adds: &[
            2921048, 2921054, 2921055, 2921057, 2921062, 2921063, 2921065, 2921082, 2921084, 2921085, 2921086,
            2921161, 2921203,
        ],
    },
    // Iscariel
    BossSign {
        dungeon: 620022,
        boss: 2301089,
        adds: &[
            2921171, 2921172, 2921173, 2921175, 2921177, 2921178, 2921179, 2921183, 2921184, 2921189, 2921192,
            2921193, 2921194, 2921195, 2921199, 2921230, 2921231, 2921234, 2921263, 2921279, 2921291, 2921292,
            2921306, 2921307, 2921319,
        ],
    },
    // Kaldrix
    BossSign {
        dungeon: 620022,
        boss: 2301090,
        adds: &[
            2921056, 2921065, 2921082, 2921087, 2921161, 2921312, 2921313, 2921314,
        ],
    },
    // Triton the Returned
    BossSign {
        dungeon: 620025,
        boss: 2301112,
        adds: &[
            28, 2921463, 2921464, 2921465, 2921466, 2921467, 2921468, 2921469, 2921507, 2921509, 2921510,
            2921551, 2921578, 2921584, 2921587, 2921589, 2921590, 2921591, 2921610,
        ],
    },
    // Deltras
    BossSign {
        dungeon: 620025,
        boss: 2301113,
        adds: &[
            2921444, 2921450, 2921489, 2921490, 2921491, 2921492, 2921493, 2921495, 2921496, 2921498, 2921499,
            2921500, 2921501, 2921502, 2921504,
        ],
    },
    // Deltras
    BossSign {
        dungeon: 620025,
        boss: 2301115,
        adds: &[
            2921435, 2921438, 2921449, 2921452, 2921457, 2921459, 2921516, 2921524, 2921526, 2921527, 2921530,
            2921531, 2921534, 2921535, 2921537, 2921540, 2921541, 2921544, 2921545, 2921569,
        ],
    },
    // Triton the Returned
    BossSign {
        dungeon: 620026,
        boss: 2301142,
        adds: &[
            28, 2921463, 2921464, 2921465, 2921466, 2921467, 2921468, 2921469, 2921507, 2921509, 2921510,
            2921551, 2921578, 2921584, 2921587, 2921589, 2921590, 2921591, 2921610,
        ],
    },
    // Deltras
    BossSign {
        dungeon: 620026,
        boss: 2301143,
        adds: &[
            2921444, 2921450, 2921490, 2921493, 2921495, 2921497, 2921500, 2921502, 2921504,
        ],
    },
    // Deltras
    BossSign {
        dungeon: 620026,
        boss: 2301145,
        adds: &[
            2921437, 2921438, 2921441, 2921449, 2921451, 2921452, 2921456, 2921457, 2921458, 2921523, 2921525,
            2921527, 2921529, 2921530, 2921532, 2921533, 2921534, 2921535, 2921536, 2921538, 2921539, 2921540,
            2921542, 2921543, 2921545, 2921569,
        ],
    },
    // Triton the Returned
    BossSign {
        dungeon: 620027,
        boss: 2301162,
        adds: &[
            28, 2921463, 2921465, 2921466, 2921467, 2921468, 2921469, 2921507, 2921509, 2921510, 2921551,
            2921578, 2921584, 2921587, 2921589, 2921590, 2921591, 2921610,
        ],
    },
    // Deltras
    BossSign {
        dungeon: 620027,
        boss: 2301163,
        adds: &[
            2921444, 2921450, 2921489, 2921490, 2921491, 2921492, 2921493, 2921494, 2921495, 2921496, 2921497,
            2921498, 2921499, 2921500, 2921501, 2921502, 2921503, 2921504,
        ],
    },
    // Deltras
    BossSign {
        dungeon: 620027,
        boss: 2301165,
        adds: &[
            2301119, 2921437, 2921438, 2921441, 2921449, 2921451, 2921452, 2921516, 2921523, 2921524, 2921525,
            2921528, 2921532, 2921533, 2921536, 2921538, 2921539, 2921542, 2921543, 2921546, 2921569,
        ],
    },
];
/// Codes of a boss's sign that must be seen, and the least fight length.
const SIGN_MIN_ADDS: usize = 2;
const SIGN_MIN_FIGHT_MS: i64 = 20_000;

/// Records, bundles and lifted records at the top level of a slice.
fn slice_structure(records: &[(i32, Vec<u8>)]) -> (usize, usize, usize) {
    let (mut lifted, mut bundles) = (0, 0);
    for (_, record) in records {
        let li = crate::capture::stream_processor::read_varint(record, 0);
        if li.length <= 0 {
            continue;
        }
        let o = li.length as usize;
        match record.get(o..o + 2) {
            Some(op) if op == evidence_slice::LIFTED_HOST => lifted += 1,
            Some([0xFF, 0xFF]) => bundles += 1,
            _ => {}
        }
    }
    (records.len(), lifted, bundles)
}

/// Put everything a HashMap produced into one fixed order.
///
/// The desktop never needed this, because nothing compared two of its records
/// byte for byte. The service does: the same slice must give the same bytes on
/// every run and on every platform (wasm32 and x86_64 hash differently), or a
/// stored log cannot be re-checked against a later re-derivation.
fn canonicalise(record: &mut FightRecord) {
    let key = |s: &crate::entity::details_context::DetailSkillEntry| (s.actor_id, s.code, s.is_dot);
    for list in [&mut record.details.skills, &mut record.details.heal_skills] {
        list.sort_by_key(key);
        for s in list.iter_mut() {
            s.hit_timestamps.sort_unstable();
        }
    }
    record.actors.sort_by_key(|a| a.actor_id);
    record.jobs.sort();
    record.job_ids.sort_unstable();
    // History's own (you, the kill, the HP's source): the log service reports
    // the kill in its checks, and its records stay as they were.
    record.local_actor_id = None;
    record.killed = None;
    record.max_hp_from_spawn = None;
    // Buffs come from the slice's abnormal records (meter 2.0.56 on). A slice
    // from an older meter has none, so its timeline is empty: say "no buff
    // data" (None) rather than "no buffs were seen". Every real fight has some.
    if record.buffs.as_ref().is_some_and(|b| b.is_empty()) {
        record.buffs = None;
    }
    // Stagger gauges (`05 E0`) are not on the slice's allowlist, so a slice
    // derives none: "no data", and fight.json keeps its shape.
    if record.stagger.as_ref().is_some_and(|w| w.is_empty()) {
        record.stagger = None;
    }
}

/// Bumped when the service derives differently from the meter of the same
/// version, so a2tools.app re-derives its logs: it re-derives every log whose
/// parser version is older than the service's. `2.0.48.1` sorts after
/// `2.0.48` and before `2.0.49`.
const SERVICE_REVISION: u32 = 5;

pub fn parser_version() -> String {
    format!("{}.{}", crate::entity::fight_record::APP_VERSION, SERVICE_REVISION)
}

/// Remove the rows of summons whose owner the parser could not find, and
/// their damage from the totals. Returns the damage removed.
///
/// A summon left unplaced is shown as a player of its own, and its damage
/// counts toward the fight but no one's: on a leaderboard that is a row that
/// is no one. A row counts as such a summon when it is unnamed and either
/// spawned as a summon, uses skills that name no class, or used at most two
/// skills for under 5% of the damage in a fight of 20 seconds or more. An
/// unnamed player runs a rotation, so is kept.
fn hide_unplaced_summons(
    record: &mut FightRecord,
    named: &std::collections::HashSet<i32>,
    spawned: &std::collections::HashSet<i32>,
) -> i64 {
    let mut damage: HashMap<i32, i64> = HashMap::new();
    let mut skills: HashMap<i32, std::collections::HashSet<i32>> = HashMap::new();
    for s in &record.details.skills {
        *damage.entry(s.actor_id).or_default() += s.dmg as i64;
        skills.entry(s.actor_id).or_default().insert(s.code);
    }
    let total: i64 = damage.values().sum();
    let long_fight = record.duration_ms >= 20_000;
    let summon: std::collections::HashSet<i32> = record.actors.iter()
        .filter(|a| !named.contains(&a.actor_id))
        .filter(|a| {
            let few_skills = skills.get(&a.actor_id).map_or(0, |s| s.len()) <= 2;
            let small = damage.get(&a.actor_id).copied().unwrap_or(0) * 20 < total;
            spawned.contains(&a.actor_id) || a.job_id == 0 || (few_skills && small && long_fight)
        })
        .map(|a| a.actor_id)
        .collect();
    if summon.is_empty() {
        return 0;
    }
    let removed: i64 = summon.iter().map(|id| damage.get(id).copied().unwrap_or(0)).sum();
    record.details.skills.retain(|s| !summon.contains(&s.actor_id));
    record.details.heal_skills.retain(|s| !summon.contains(&s.actor_id));
    record.actors.retain(|a| !summon.contains(&a.actor_id));
    record.total_damage = (record.total_damage as i64 - removed).max(0) as i32;
    record.details.total_target_damage = (record.details.total_target_damage as i64 - removed).max(0) as _;
    // The classes in the fight, from the rows that are left.
    let mut jobs: Vec<String> = Vec::new();
    let mut job_ids: Vec<i32> = Vec::new();
    for a in &record.actors {
        if !a.job.is_empty() && !jobs.contains(&a.job) {
            jobs.push(a.job.clone());
        }
        if a.job_id > 0 && !job_ids.contains(&a.job_id) {
            job_ids.push(a.job_id);
        }
    }
    record.jobs = jobs;
    record.job_ids = job_ids;
    removed
}

/// Replay a slice through the parser AND the combat aggregation, and return
/// the boss fight the desktop meter would have saved.
///
/// `npcs_json` and `skills_json` are the meter's own i18n tables (one
/// language). They are passed in rather than read from disk because the
/// service has no disk; without the NPC table no target counts as a boss and
/// nothing is derived.
pub fn derive_fight(
    slice: &[u8],
    npcs_json: &str,
    skills_json: &str,
    dot_ids_json: &str,
) -> Result<DerivedFight, DeriveError> {
    let decoded = evidence_slice::decode(slice).ok_or(DeriveError::NotASlice)?;
    derive_decoded(&decoded, npcs_json, skills_json, dot_ids_json)
}

/// A slice's records and blind map, as `evidence_slice::decode` gives them.
pub type Decoded = (Vec<(i32, Vec<u8>)>, HashMap<String, u64>);

/// The data tables a derive reads, parsed: the NPC table (762 KB of JSON),
/// the skill table and the DoT list. Read only while deriving, so the log
/// service parses them once per isolate (`derive_decoded_with`), not on every
/// request, twice when the first pass found nothing (~15 ms each).
pub struct Tables {
    npcs: Arc<NpcLookup>,
    skills: Arc<SkillLookup>,
    dot_ids: Option<std::collections::HashSet<i32>>,
}

impl Tables {
    pub fn load(npcs_json: &str, skills_json: &str, dot_ids_json: &str) -> Self {
        let npcs = Arc::new(NpcLookup::new());
        npcs.load_from_json(npcs_json);
        let skills = Arc::new(SkillLookup::new());
        skills.load_from_json(skills_json);
        let dot_ids = serde_json::from_str::<Vec<i32>>(dot_ids_json).ok().map(|ids| ids.into_iter().collect());
        Tables { npcs, skills, dot_ids }
    }
}

/// `derive_fight` on a slice already read, for the service, which reads it
/// as it unpacks (`evidence_slice::decode_from`).
pub fn derive_decoded(
    decoded: &Decoded,
    npcs_json: &str,
    skills_json: &str,
    dot_ids_json: &str,
) -> Result<DerivedFight, DeriveError> {
    derive_decoded_with(decoded, &Tables::load(npcs_json, skills_json, dot_ids_json))
}

/// `derive_decoded` with the tables already parsed.
pub fn derive_decoded_with(decoded: &Decoded, tables: &Tables) -> Result<DerivedFight, DeriveError> {
    derive_decoded_claiming(decoded, tables, None)
}

/// The boss and dungeon the uploader's meter saw for a fight, sent with it
/// when it is sent again because the slice alone named none (the meter's
/// sweep of refused fights). Used only when nothing else names the boss, and
/// only if the slice does not contradict it (`claimed_boss`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Claim {
    pub mob: i32,
    pub dungeon: i32,
}

/// `derive_decoded_with`, with the meter's `claim` for when nothing else
/// names the boss.
pub fn derive_decoded_claiming(decoded: &Decoded, tables: &Tables, claim: Option<Claim>) -> Result<DerivedFight, DeriveError> {
    derive_fight_with(decoded, tables, true, false, false)
        .or_else(|e| when_nothing_derived(decoded, tables, e, false, claim))
        .map(|(fight, _, _)| fight)
}

/// `derive_decoded_with`, and every hit its record counts (`FightHits`): the
/// same replay with `DataStorage::set_on_hit` watching, so the hits are the
/// ones the record was summed from, filed on the record's own rows.
pub fn derive_hits_with(decoded: &Decoded, tables: &Tables) -> Result<(DerivedFight, FightHits), DeriveError> {
    derive_hits_claiming(decoded, tables, None)
}

/// `derive_hits_with`, with the meter's `claim` (`derive_decoded_claiming`):
/// a log stored on one is derived again with it.
pub fn derive_hits_claiming(
    decoded: &Decoded,
    tables: &Tables,
    claim: Option<Claim>,
) -> Result<(DerivedFight, FightHits), DeriveError> {
    derive_fight_with(decoded, tables, true, false, true)
        .or_else(|e| when_nothing_derived(decoded, tables, e, true, claim))
        .map(|(fight, _, hits)| (fight, hits.unwrap_or_default()))
}

/// What is tried when the first derive finds nothing, on one replay of the
/// whole slice shared by every check: each replay is most of a derive's time,
/// and the recovery of refused uploads waited 13 seconds a slice on six of
/// them (2026-11-13).
///
/// 1. Every fight counted, not only the uploader's own. Slices from early
///    meters (2.0.30 to 2.0.37) have no self record and no dungeon, so
///    `is_our_fight` (2026-10-05, #19) takes the uploader's own boss for a
///    stranger's: 56 logs a re-derive could not update (2026-10-11). In an
///    instance every fight is already ours (`fight_is_ours`), so this is
///    tried only without one, or when the zone changed in the slice (the
///    derive stops short of that, and the zone after may not be the fight's).
/// 2. The boss named by the adds spawned during the fight (`boss_from_adds`).
/// 3. The boss the uploader's meter saw, when it sent one (`claimed_boss`).
/// 4. Else whether the fight was too short (`short_boss_fight`).
fn when_nothing_derived(
    decoded: &Decoded,
    tables: &Tables,
    error: DeriveError,
    collect_hits: bool,
    claim: Option<Claim>,
) -> Result<(DerivedFight, i64, Option<FightHits>), DeriveError> {
    if !matches!(error, DeriveError::NothingDerived) {
        return Err(error);
    }
    let (all, _, zone_changed) = replay_until(decoded, tables, decoded.0.len(), None);
    if all.current_dungeon_id() <= 0 || zone_changed {
        match derive_fight_with(decoded, tables, true, true, collect_hits) {
            Err(DeriveError::NothingDerived) => {}
            other => return other,
        }
    }
    if let Some((entity, code)) = boss_from_adds(decoded, tables, &all) {
        return derive_fight_assuming(decoded, tables, true, true, collect_hits, Some((entity, code, Named::Adds)));
    }
    if let Some((entity, code)) = claim.and_then(|c| claimed_boss(decoded, tables, &all, c)) {
        return derive_fight_assuming(decoded, tables, true, true, collect_hits, Some((entity, code, Named::Client)));
    }
    if short_boss_fight(&all, tables) {
        return Err(DeriveError::ShortFight);
    }
    Err(error)
}

/// What named a boss whose spawn the slice lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Named {
    Adds,
    Client,
}

/// The most damaged target, (entity, first hit, last hit), when it has no
/// spawn (no NPC code) and was fought for at least `min_ms`.
fn anonymous_top(all: &DataStorage, min_ms: i64) -> Option<(i32, i64, i64)> {
    let combat = all.get_combat_snapshot_light();
    let top = combat
        .values()
        .filter(|t| t.total_damage > 0)
        .max_by_key(|t| (t.total_damage, std::cmp::Reverse(t.target_id)))?;
    let (target, first, last) = (top.target_id, top.first_damage_time, top.last_damage_time);
    (all.mob_code(target).is_none() && last - first >= min_ms).then_some((target, first, last))
}

/// The NPC codes that spawned between `first` and `last` (one replay, to
/// the last hit).
fn fight_spawns(decoded: &Decoded, tables: &Tables, first: i64, last: i64) -> Option<std::collections::HashSet<i32>> {
    let records = &decoded.0;
    let start = records.iter().position(|(dt, _)| *dt as i64 >= first)?;
    let end = records.iter().skip(start).position(|(dt, _)| *dt as i64 > last).map_or(records.len(), |n| start + n);
    let (to_end, before, _) = replay_until(decoded, tables, end, Some(start));
    Some(
        to_end
            .get_mob_data()
            .into_iter()
            .filter(|(id, _)| !before.contains(id))
            .map(|(_, code)| code)
            .collect(),
    )
}

/// (entity, boss code) for the uploader's meter's `claim`, when nothing in
/// the slice says otherwise: a boss of the claimed dungeon (as the NPC table
/// has it, when it says), the slice's own dungeon that one or none, the most
/// damaged target without a spawn and fought for at least BOSS_FIGHT_MIN_MS
/// (SIGN_MIN_FIGHT_MS in a slice with no dungeon), and none of another boss's adds (`BOSS_SIGNS`) spawned during the fight.
/// The site keeps such a log off the boards unless its kill shows: a death
/// or clear in the slice, and damage of at least the boss's known health, so
/// a claim alone ranks nothing.
fn claimed_boss(decoded: &Decoded, tables: &Tables, all: &DataStorage, claim: Claim) -> Option<(i32, i32)> {
    let npcs = &tables.npcs;
    if claim.dungeon <= 0 || !npcs.is_boss(claim.mob) || npcs.is_training_dummy(claim.mob) {
        return None;
    }
    if npcs.dungeon_of(claim.mob).is_some_and(|d| d != claim.dungeon) {
        return None;
    }
    let own = all.current_dungeon_id();
    if own > 0 && own != claim.dungeon {
        return None;
    }
    // A slice with no dungeon of its own says least: then the fight must be
    // as long as the add rule asks (SIGN_MIN_FIGHT_MS), not just a boss
    // fight's least.
    let (target, first, last) = anonymous_top(all, if own > 0 { BOSS_FIGHT_MIN_MS } else { SIGN_MIN_FIGHT_MS })?;
    let others: Vec<&BossSign> =
        BOSS_SIGNS.iter().filter(|s| s.dungeon == claim.dungeon && s.boss != claim.mob).collect();
    if !others.is_empty() {
        let spawned = fight_spawns(decoded, tables, first, last)?;
        if others.iter().any(|s| s.adds.iter().any(|c| spawned.contains(c))) {
            return None;
        }
    }
    Some((target, claim.mob))
}

/// The storage after the slice's first `upto` records; with `mark`, the NPC
/// entities known just before record `mark` too; and whether the zone changed
/// (a zone reset or the party ending) after the fight began, as the derive's
/// own replay watches for.
fn replay_until(
    decoded: &Decoded,
    tables: &Tables,
    upto: usize,
    mark: Option<usize>,
) -> (Arc<DataStorage>, std::collections::HashSet<i32>, bool) {
    let storage = Arc::new(DataStorage::new());
    let mut processor = StreamProcessor::new(storage.clone(), tables.skills.clone(), tables.npcs.clone());
    if let Some(ids) = &tables.dot_ids {
        processor.set_dot_skill_ids(ids.clone());
    }
    let mut known = std::collections::HashSet::new();
    let mut zone_changed = false;
    for (n, (dt_ms, packet)) in decoded.0.iter().enumerate().take(upto) {
        if mark == Some(n) {
            known = storage.get_mob_data().into_keys().collect();
        }
        processor.set_override_timestamp(Some(*dt_ms as i64));
        let before = (storage.last_zone_reset_ms(), storage.party_ended_at_ms());
        processor.consume_stream(packet);
        if *dt_ms > 0 && (storage.last_zone_reset_ms(), storage.party_ended_at_ms()) != before {
            zone_changed = true;
        }
    }
    if mark == Some(upto) {
        known = storage.get_mob_data().into_keys().collect();
    }
    processor.set_override_timestamp(None);
    (storage, known, zone_changed)
}

/// The least fight a boss fight is (`DpsCalculator::snapshot_boss_fights`).
const BOSS_FIGHT_MIN_MS: i64 = 5_000;

/// Whether nothing derived because the boss fight was too short, and for no
/// other reason: a boss the NPC table names took damage, for less than
/// BOSS_FIGHT_MIN_MS, and no target without a spawn was fought for longer.
/// A slice whose main target is anonymous (a boss whose spawn is missing)
/// is never called short: what it holds is not known. `all`: the whole slice
/// replayed.
fn short_boss_fight(all: &DataStorage, tables: &Tables) -> bool {
    let combat = all.get_combat_snapshot_light();
    let span = |t: &crate::combat::data_storage::TargetCombatData| t.last_damage_time - t.first_damage_time;
    let mut boss_hit = false;
    for t in combat.values().filter(|t| t.total_damage > 0) {
        match all.mob_code(t.target_id) {
            Some(code) if tables.npcs.is_boss(code) => {
                if span(t) >= BOSS_FIGHT_MIN_MS {
                    return false;
                }
                boss_hit = true;
            }
            Some(_) => {}
            None if span(t) >= BOSS_FIGHT_MIN_MS => return false,
            None => {}
        }
    }
    boss_hit
}

/// (entity, boss code) for a slice whose most damaged target has no spawn,
/// in an instance with `BOSS_SIGNS`, when the NPCs that spawned while it was
/// being hit hold at least SIGN_MIN_ADDS of one boss's sign and none of any
/// other boss's. None otherwise: a fight is never filed on a guess. `all`:
/// the whole slice replayed; the spawns during the fight take one more
/// replay, to its last hit.
pub fn boss_from_adds(decoded: &Decoded, tables: &Tables, all: &DataStorage) -> Option<(i32, i32)> {
    let signs: Vec<&BossSign> = BOSS_SIGNS.iter().filter(|s| s.dungeon == all.current_dungeon_id()).collect();
    if signs.is_empty() {
        return None;
    }
    let (target, first, last) = anonymous_top(all, SIGN_MIN_FIGHT_MS)?;
    // The NPCs that spawned between the first hit and the last.
    let spawned = fight_spawns(decoded, tables, first, last)?;
    let seen = |s: &BossSign| s.adds.iter().filter(|c| spawned.contains(c)).count();
    let mut named = signs.iter().filter(|s| seen(s) >= SIGN_MIN_ADDS);
    let boss = named.next()?;
    if named.next().is_some() || signs.iter().any(|s| s.boss != boss.boss && seen(s) > 0) {
        return None;
    }
    Some((target, boss.boss))
}

/// `derive_fight` without `hide_unplaced_summons`: the fight as the parser
/// and the calculator give it, which is what the desktop meter saves, and the
/// damage the service would hide from it. For checking tools: a record from
/// the whole capture never goes through the hide, so comparing it with a
/// hidden one finds every hidden row as a difference. The service stores
/// `derive_fight`'s answer, not this one.
pub fn derive_fight_unhidden(
    slice: &[u8],
    npcs_json: &str,
    skills_json: &str,
    dot_ids_json: &str,
) -> Result<(DerivedFight, i64), DeriveError> {
    let decoded = evidence_slice::decode(slice).ok_or(DeriveError::NotASlice)?;
    derive_fight_with(&decoded, &Tables::load(npcs_json, skills_json, dot_ids_json), false, false, false)
        .map(|(fight, hidden, _)| (fight, hidden))
}

/// `derive_fight_unhidden` counting every boss fight in the slice, not only
/// the uploader's own (`DpsCalculator::set_every_fight`). For checking tools
/// that compare every fight in a capture: the service keeps the check.
pub fn derive_fight_unhidden_every(
    slice: &[u8],
    npcs_json: &str,
    skills_json: &str,
    dot_ids_json: &str,
) -> Result<(DerivedFight, i64), DeriveError> {
    let decoded = evidence_slice::decode(slice).ok_or(DeriveError::NotASlice)?;
    derive_fight_with(&decoded, &Tables::load(npcs_json, skills_json, dot_ids_json), false, true, false)
        .map(|(fight, hidden, _)| (fight, hidden))
}

/// The fight, and the damage `hide_unplaced_summons` removes from it (or
/// would, when `hide` is false and the record is returned whole), and with
/// `collect_hits` the record's hits (`FightHits`).
fn derive_fight_with(
    decoded: &Decoded,
    tables: &Tables,
    hide: bool,
    every_fight: bool,
    collect_hits: bool,
) -> Result<(DerivedFight, i64, Option<FightHits>), DeriveError> {
    derive_fight_assuming(decoded, tables, hide, every_fight, collect_hits, None)
}

/// `derive_fight_with`, with `assume` = (entity, NPC code, what named it)
/// taken as that entity's spawn when the slice has none (`boss_from_adds`,
/// `claimed_boss`).
fn derive_fight_assuming(
    decoded: &Decoded,
    tables: &Tables,
    hide: bool,
    every_fight: bool,
    collect_hits: bool,
    assume: Option<(i32, i32, Named)>,
) -> Result<(DerivedFight, i64, Option<FightHits>), DeriveError> {
    let (records, blind_map) = (&decoded.0, decoded.1.clone());
    let (npcs, skills, dot_ids) = (&tables.npcs, &tables.skills, &tables.dot_ids);
    let replay = |upto: usize| {
        let storage = Arc::new(DataStorage::new());
        // Every applied hit, as the hook sees it (raw ids): sorted out once
        // the record says which target and rows they belong to.
        let seen: Arc<std::sync::Mutex<Vec<RawHit>>> = Arc::default();
        if collect_hits {
            let seen = seen.clone();
            storage.set_on_hit(move |p| seen.lock().unwrap().push(RawHit::of(p)));
        }
        let mut processor = StreamProcessor::new(storage.clone(), skills.clone(), npcs.clone());
        // The live meter loads these too (app.rs); without them a DoT tick is
        // filed as a direct hit and the skill table splits differently.
        if let Some(ids) = &dot_ids {
            processor.set_dot_skill_ids(ids.clone());
        }
        // The first zone reset after the fight began, as an index into the
        // records: the record that caused it.
        let mut reset_at = None;
        for (n, (dt_ms, packet)) in records.iter().enumerate().take(upto) {
            processor.set_override_timestamp(Some(*dt_ms as i64));
            let before = (storage.last_zone_reset_ms(), storage.party_ended_at_ms());
            processor.consume_stream(packet);
            if reset_at.is_none() && *dt_ms > 0 && (storage.last_zone_reset_ms(), storage.party_ended_at_ms()) != before {
                reset_at = Some(n);
            }
        }
        (storage, processor, reset_at, seen)
    };
    // A teleport after the fight (a wipe sends everyone back; a kill is often
    // followed by leaving) is a zone change, and the meter clears combat on
    // one. The slice's tail runs 15 seconds past the last hit, so it can hold
    // that teleport, and the replay then had nothing left to derive: two
    // Gargaum wipes in a 2026-07 capture. The live meter had saved the fight
    // before; so stop the replay just short of the first reset after the pull.
    // The party ending resets combat too: the others leaving after a kill
    // leave you alone in it (38 logs at 2.0.76.4, 2026-10-23, once a roster
    // with a member from another world read whole).
    let (mut storage, mut processor, reset_at, mut seen) = replay(records.len());
    if let Some(n) = reset_at {
        (storage, processor, _, seen) = replay(n);
    }
    if let Some((entity, code, _)) = assume {
        storage.append_mob(entity, code);
    }
    // Keep the clock pinned through the snapshot. It asks for "now" to decide
    // whether a fight has ended; on wasm32 the wall clock panics, and on a
    // desktop it would make the answer depend on when the replay ran. A minute
    // past the last packet is unambiguously "ended".
    let end_ms = records.last().map(|(dt, _)| *dt as i64).unwrap_or(0) + 60_000;
    processor.set_override_timestamp(Some(end_ms));
    crate::clock::set_override(Some(end_ms));

    let totals: HashMap<i32, i64> = storage
        .get_combat_snapshot_light()
        .iter()
        .map(|(id, t)| (*id, t.total_damage))
        .collect();

    // Read before the calculator takes the storage: who is named, and which
    // entities spawned as summons (see `hide_unplaced_summons`).
    let named: std::collections::HashSet<i32> = storage.get_nicknames().keys().copied().collect();
    let spawned = storage.get_summon_spawn_ids();
    let dead = storage.get_dead_entities();

    let mut calc = DpsCalculator::new(storage, skills.clone(), npcs.clone(), Arc::new(PingTracker::new()));
    calc.set_every_fight(every_fight);
    let snapshot = calc.snapshot_boss_fights_force();
    let record = snapshot
        .into_iter()
        // The slice is cut around one fight, but adds and another boss can
        // share it: the lead-in reaches a minute back, so the previous boss of
        // a dungeon is often in it whole. The slice's clock starts at the
        // fight's first hit, so the fight is the boss fight that starts
        // nearest zero; the most damaged target only breaks a tie. Taking the
        // most damaged one alone filed a Judge Urahum upload as the Guardian
        // Captain Raur killed 37 seconds before it, and a scarecrow upload as
        // someone else's scarecrow (2026-10-03).
        .min_by_key(|r| (r.start_time_ms.abs(), -totals.get(&r.target_id).copied().unwrap_or(0), r.target_id))
        .ok_or(DeriveError::NothingDerived);
    // Still on the pinned clock: the rows' attribution, for the hits.
    let attribution = match &record {
        Ok(r) if collect_hits => Some(calc.target_attribution(r.target_id)),
        _ => None,
    };
    processor.set_override_timestamp(None);
    crate::clock::set_override(None);
    let mut record = record?;
    let hidden = if hide {
        hide_unplaced_summons(&mut record, &named, &spawned)
    } else {
        hide_unplaced_summons(&mut record.clone(), &named, &spawned)
    };
    canonicalise(&mut record);
    let on_target = totals.get(&record.target_id).copied().unwrap_or(0);
    let (count, lifted, bundles) = slice_structure(&records);
    let cleared_after_ms = clear_near(records, record.start_time_ms + record.duration_ms);
    let checks = SliceChecks {
        unblinded_names: evidence_slice::unblinded_names(&records, &blind_map),
        records: count,
        lifted,
        bundles,
        killed: dead.contains(&record.target_id) || cleared_after_ms.is_some(),
        boss_named_by_adds: assume.is_some_and(|a| a.2 == Named::Adds),
        boss_named_by_client: assume.is_some_and(|a| a.2 == Named::Client),
        cleared_after_ms,
        max_hp: record.details.max_hp as i64,
        damage: on_target,
    };

    let hits = attribution.map(|uid_of| {
        let raw = std::mem::take(&mut *seen.lock().unwrap());
        FightHits::sorted(raw, &record, &uid_of, |code| calc.row_skill_code(code))
    });

    Ok((
        DerivedFight {
            parser_version: parser_version(),
            total_damage: if hide { on_target - hidden } else { on_target },
            record,
            blind_map: blind_map.into_iter().collect(),
            records: records.len(),
            checks,
        },
        hidden,
        hits,
    ))
}

/// A hit as `DataStorage::set_on_hit` gives it: the packet's own ids.
#[derive(Debug, Clone)]
struct RawHit {
    time: i64,
    actor: i32,
    target: i32,
    skill: i32,
    dot: bool,
    damage: i32,
    multi: i32,
    flags: u16,
}

/// A hit's specials as the bits of `FightHits::f`.
pub const HIT_FLAGS: [(SpecialDamage, u16); 10] = [
    (SpecialDamage::Critical, 1),
    (SpecialDamage::Back, 2),
    (SpecialDamage::Frontal, 4),
    (SpecialDamage::Double, 8),
    (SpecialDamage::Perfect, 16),
    (SpecialDamage::Parry, 32),
    (SpecialDamage::ShieldBlock, 64),
    (SpecialDamage::IronWall, 128),
    (SpecialDamage::Regeneration, 256),
    (SpecialDamage::PerfectBlock, 512),
];

impl RawHit {
    fn of(p: &ParsedDamagePacket) -> Self {
        let flags = HIT_FLAGS.iter().filter(|(s, _)| p.specials().contains(s)).fold(0, |f, (_, bit)| f | bit);
        RawHit {
            time: p.timestamp(),
            actor: p.actor_id(),
            target: p.target_id(),
            skill: p.skill_code(),
            dot: p.is_dot(),
            damage: p.total_damage(),
            multi: p.multi_hit_count(),
            flags,
        }
    }
}

/// Every hit a fight's record counts, for tools that want more than its
/// per-skill totals. Columns, one entry per hit in time order, so a 2,000-hit
/// fight is tens of KB rather than hundreds:
///
/// - `t`: ms from the fight's start, as the record's `hitTimestamps`
/// - `s`: the hit's row, an index into `rows` (`[actorId, code, isDot]`,
///   the keys of the record's own `details.skills`)
/// - `d`: its damage, its multi-hit damage included, as the rows' `dmg` sums
/// - `m`: its multi-hit count
/// - `f`: its specials, the bits of `HIT_FLAGS` (crit 1, back 2, frontal 4,
///   double 8, perfect 16, parry 32, shield block 64, iron wall 128,
///   regeneration 256, perfect block 512)
///
/// Misses and resists are not hits here, as they are not in a row's `time`.
/// `matches` says whether the hits add up to every row exactly, count and
/// damage; a file that does not is still the parser's answer, and says so.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct FightHits {
    pub rows: Vec<(i32, i32, bool)>,
    pub t: Vec<i64>,
    pub s: Vec<u32>,
    pub d: Vec<i32>,
    pub m: Vec<i32>,
    pub f: Vec<u16>,
    pub matches: bool,
}

impl FightHits {
    fn sorted(
        mut raw: Vec<RawHit>,
        record: &FightRecord,
        uid_of: &HashMap<i32, i32>,
        row_code: impl Fn(i32) -> i32,
    ) -> Self {
        let start = record.details.start_time;
        let mut rows: Vec<(i32, i32, bool)> =
            record.details.skills.iter().map(|r| (r.actor_id, r.code, r.is_dot)).collect();
        rows.sort_unstable();
        rows.dedup();
        let index: HashMap<(i32, i32, bool), u32> =
            rows.iter().enumerate().map(|(i, k)| (*k, i as u32)).collect();
        raw.retain(|h| h.target == record.target_id);
        // Stable: hits in the same ms keep the order they were applied in.
        raw.sort_by_key(|h| h.time);
        let mut out = FightHits { rows, ..Default::default() };
        let mut sums: HashMap<u32, (i32, i64)> = HashMap::new();
        for h in raw {
            // An attacker the rows leave out (no owner found, or hidden as an
            // unplaced summon) is left out here too.
            let Some(&uid) = uid_of.get(&h.actor) else { continue };
            let Some(&row) = index.get(&(uid, row_code(h.skill), h.dot)) else { continue };
            let sum = sums.entry(row).or_default();
            sum.0 += 1;
            sum.1 += h.damage as i64;
            out.t.push(h.time - start);
            out.s.push(row);
            out.d.push(h.damage);
            out.m.push(h.multi);
            out.f.push(h.flags);
        }
        out.matches = record.details.skills.iter().all(|r| {
            let row = index[&(r.actor_id, r.code, r.is_dot)];
            let (n, dmg) = sums.get(&row).copied().unwrap_or_default();
            n == r.time && dmg.min(i32::MAX as i64) == r.dmg as i64
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boss_signs_name_bosses_of_their_instance_and_share_no_add() {
        let npcs = NpcLookup::new();
        npcs.load_from_json(include_str!("../../src/data/i18n/npcs/en.json"));
        for sign in BOSS_SIGNS {
            assert!(npcs.is_boss(sign.boss), "{} is not a boss", sign.boss);
            assert_eq!(npcs.dungeon_of(sign.boss), Some(sign.dungeon), "{} is not of {}", sign.boss, sign.dungeon);
            assert!(sign.adds.len() >= SIGN_MIN_ADDS, "{} has too few adds to be named", sign.boss);
            for other in BOSS_SIGNS.iter().filter(|o| o.dungeon == sign.dungeon && o.boss != sign.boss) {
                assert!(
                    sign.adds.iter().all(|c| !other.adds.contains(c)),
                    "{} and {} share an add",
                    sign.boss,
                    other.boss
                );
            }
        }
    }

    #[test]
    fn hits_are_filed_on_the_records_rows_and_checked_against_them() {
        let record: FightRecord = serde_json::from_value(serde_json::json!({
            "id": "auto_1_2", "bossName": "B", "targetId": 9, "startTimeMs": 0, "durationMs": 1000,
            "totalDamage": 350, "jobs": [],
            "details": {"targetId": 9, "maxHp": 0, "totalTargetDamage": 350, "battleTime": 1000,
                        "startTime": 1000, "pingHistory": [], "healSkills": [], "skills": [
                {"actorId": 7, "code": 11010000, "name": "", "time": 2, "dmg": 300, "multiHitCount": 0,
                 "multiHitDamage": 0, "crit": 1, "parry": 0, "back": 0, "perfect": 0, "double": 0,
                 "isDot": false, "regen": 0, "job": "", "hitTimestamps": [], "specs": []},
                {"actorId": 7, "code": 11010000, "name": "", "time": 1, "dmg": 50, "multiHitCount": 0,
                 "multiHitDamage": 0, "crit": 0, "parry": 0, "back": 0, "perfect": 0, "double": 0,
                 "isDot": true, "regen": 0, "job": "", "hitTimestamps": [], "specs": []}]},
            "actors": []
        }))
        .unwrap();
        let hit = |time, actor, target, skill, dot, damage, flags| RawHit {
            time, actor, target, skill, dot, damage, multi: 0, flags,
        };
        let raw = vec![
            // A summon (70) filed on its owner (7), a variant code on its base.
            hit(1500, 70, 9, 11010001, false, 200, 1),
            hit(1200, 7, 9, 11010000, false, 100, 0),
            hit(1300, 7, 9, 11010000, true, 50, 0),
            // Another target, and an attacker the rows leave out.
            hit(1250, 7, 8, 11010000, false, 999, 0),
            hit(1260, 66, 9, 11010000, false, 999, 0),
        ];
        let uid_of = HashMap::from([(7, 7), (70, 7)]);
        let fold = |code: i32| code - code % 10000;
        let hits = FightHits::sorted(raw.clone(), &record, &uid_of, fold);
        assert_eq!(hits.rows, vec![(7, 11010000, false), (7, 11010000, true)]);
        assert_eq!(hits.t, vec![200, 300, 500], "from the fight's start, in time order");
        assert_eq!(hits.s, vec![0, 1, 0]);
        assert_eq!(hits.d, vec![100, 50, 200]);
        assert_eq!(hits.f, vec![0, 0, 1]);
        assert!(hits.matches);
        // A hit the rows never counted: the file says it does not add up.
        let mut more = raw;
        more.push(hit(1600, 7, 9, 11010000, true, 5, 0));
        assert!(!FightHits::sorted(more, &record, &uid_of, fold).matches);
    }

    /// A dungeon's clear confirms the kill only when it comes as the fight
    /// ends: the run cleared (`00 61` state 3) 2 s after the last hit, not a
    /// clear long after (another fight's) nor a run merely going on.
    #[test]
    fn a_clear_as_the_fight_ends_is_a_kill() {
        let state = |n: u8| {
            let mut p = vec![0x18, 0x00, 0x61];
            p.extend(600_071u32.to_le_bytes());
            p.push(n);
            p.extend((-450_000i64).to_le_bytes());
            p.extend(150_000i64.to_le_bytes());
            p
        };
        let last_hit = 120_000;
        assert_eq!(clear_near(&[(5_000, state(2)), (122_000, state(3))], last_hit), Some(2_000));
        assert_eq!(clear_near(&[(5_000, state(2))], last_hit), None, "running is no clear");
        assert_eq!(clear_near(&[(200_000, state(3))], last_hit), None, "a clear well after this fight");
        assert_eq!(clear_near(&[(110_000, state(3))], last_hit), None, "a clear before the last hit");
    }

    #[test]
    fn slice_structure_counts_lifted_records_and_bundles() {
        let lifted = vec![0x09, 0xE5, 0xA2, 0x04, 0x38, 0x01, 0x02];
        let bundle = vec![0x0A, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x00];
        let damage = vec![0x08, 0x04, 0x38, 0x01, 0x02, 0x03];
        let records = vec![(0, lifted), (0, bundle.clone()), (0, bundle), (0, damage)];
        assert_eq!(slice_structure(&records), (4, 1, 2));
    }

    #[test]
    fn a_derived_record_keeps_its_buffs_and_an_empty_timeline_is_no_data() {
        let parse = |buffs: serde_json::Value| -> FightRecord {
            serde_json::from_value(serde_json::json!({
                "id": "auto_1_2", "bossName": "B", "targetId": 1, "startTimeMs": 0, "durationMs": 1000,
                "totalDamage": 1, "jobs": [],
                "details": {"targetId": 1, "maxHp": 0, "totalTargetDamage": 1, "battleTime": 1000,
                            "startTime": 0, "skills": [], "pingHistory": [], "healSkills": []},
                "actors": [],
                "buffs": buffs
            }))
            .unwrap()
        };
        // A slice with abnormal records (2.0.56 on): the timeline stays.
        let mut record = parse(serde_json::json!([{"on": 7, "id": 1, "by": 7, "segs": "0,1000,1,1", "up": 1000}]));
        canonicalise(&mut record);
        assert_eq!(record.buffs.as_ref().map(|b| b.len()), Some(1));
        // An older meter's slice has none: no data, and fight.json keeps its old shape.
        let mut record = parse(serde_json::json!([]));
        canonicalise(&mut record);
        assert!(record.buffs.is_none());
        assert!(serde_json::to_value(&record).unwrap().get("buffs").is_none(), "fight.json keeps its shape");
        record.stagger = Some(Vec::new());
        canonicalise(&mut record);
        assert!(serde_json::to_value(&record).unwrap().get("stagger").is_none(), "fight.json keeps its shape");
    }

    #[test]
    fn refuses_anything_that_is_not_a_slice() {
        assert_eq!(derive(b"").unwrap_err(), DeriveError::NotASlice);
        assert_eq!(derive(b"nope").unwrap_err(), DeriveError::NotASlice);
    }

    #[test]
    fn a_slice_with_no_damage_derives_nothing() {
        use crate::capture::evidence_slice::{build, encode, CapturedPacket};
        // An allowlisted opcode carrying no parseable damage.
        let payload = [0x23, 0x36, 0x00];
        let framed = {
            let total = payload.len() + 1;
            let mut v = vec![(total + 3) as u8];
            v.extend_from_slice(&payload);
            v
        };
        let slice = build(
            &[CapturedPacket {
                captured_at_ms: 0,
                stream: "Client:1".into(),
                bytes: framed,
            }],
            0,
            1_000,
            &Default::default(),
            [7; 32],
        )
        .expect("builds");
        assert_eq!(
            derive(&encode(&slice)).unwrap_err(),
            DeriveError::NothingDerived
        );
    }
}

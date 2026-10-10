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
use crate::entity::fight_record::FightRecord;
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
}

impl std::fmt::Display for DeriveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeriveError::NotASlice => write!(f, "not an evidence slice"),
            DeriveError::NothingDerived => write!(f, "no damage derived from the slice"),
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
}

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
    // Buffs come from the slice's abnormal records (meter 2.0.56 on). A slice
    // from an older meter has none, so its timeline is empty: say "no buff
    // data" (None) rather than "no buffs were seen". Every real fight has some.
    if record.buffs.as_ref().is_some_and(|b| b.is_empty()) {
        record.buffs = None;
    }
}

/// Bumped when the service derives differently from the meter of the same
/// version, so a2tools.app re-derives its logs: it re-derives every log whose
/// parser version is older than the service's. `2.0.48.1` sorts after
/// `2.0.48` and before `2.0.49`.
const SERVICE_REVISION: u32 = 1;

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
    derive_fight_with(slice, npcs_json, skills_json, dot_ids_json, true, false).map(|(fight, _)| fight)
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
    derive_fight_with(slice, npcs_json, skills_json, dot_ids_json, false, false)
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
    derive_fight_with(slice, npcs_json, skills_json, dot_ids_json, false, true)
}

/// The fight, and the damage `hide_unplaced_summons` removes from it (or
/// would, when `hide` is false and the record is returned whole).
fn derive_fight_with(
    slice: &[u8],
    npcs_json: &str,
    skills_json: &str,
    dot_ids_json: &str,
    hide: bool,
    every_fight: bool,
) -> Result<(DerivedFight, i64), DeriveError> {
    let (records, blind_map) = evidence_slice::decode(slice).ok_or(DeriveError::NotASlice)?;

    let npcs = Arc::new(NpcLookup::new());
    npcs.load_from_json(npcs_json);
    let skills = Arc::new(SkillLookup::new());
    skills.load_from_json(skills_json);

    let dot_ids: Option<std::collections::HashSet<i32>> =
        serde_json::from_str::<Vec<i32>>(dot_ids_json).ok().map(|ids| ids.into_iter().collect());
    let replay = |upto: usize| {
        let storage = Arc::new(DataStorage::new());
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
            let before = storage.last_zone_reset_ms();
            processor.consume_stream(packet);
            if reset_at.is_none() && *dt_ms > 0 && storage.last_zone_reset_ms() != before {
                reset_at = Some(n);
            }
        }
        (storage, processor, reset_at)
    };
    // A teleport after the fight (a wipe sends everyone back; a kill is often
    // followed by leaving) is a zone change, and the meter clears combat on
    // one. The slice's tail runs 15 seconds past the last hit, so it can hold
    // that teleport, and the replay then had nothing left to derive: two
    // Gargaum wipes in a 2026-07 capture. The live meter had saved the fight
    // before; so stop the replay just short of the first reset after the pull.
    let (mut storage, mut processor, reset_at) = replay(records.len());
    if let Some(n) = reset_at {
        (storage, processor, _) = replay(n);
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

    let mut calc = DpsCalculator::new(storage, skills, npcs, Arc::new(PingTracker::new()));
    calc.set_every_fight(every_fight);
    let snapshot = calc.snapshot_boss_fights_force();
    processor.set_override_timestamp(None);
    crate::clock::set_override(None);
    let mut record = snapshot
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
        .ok_or(DeriveError::NothingDerived)?;
    let hidden = if hide {
        hide_unplaced_summons(&mut record, &named, &spawned)
    } else {
        hide_unplaced_summons(&mut record.clone(), &named, &spawned)
    };
    canonicalise(&mut record);
    let on_target = totals.get(&record.target_id).copied().unwrap_or(0);
    let (count, lifted, bundles) = slice_structure(&records);
    let checks = SliceChecks {
        unblinded_names: evidence_slice::unblinded_names(&records, &blind_map),
        records: count,
        lifted,
        bundles,
        killed: dead.contains(&record.target_id),
        max_hp: record.details.max_hp as i64,
        damage: on_target,
    };

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
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        )
        .expect("builds");
        assert_eq!(
            derive(&encode(&slice)).unwrap_err(),
            DeriveError::NothingDerived
        );
    }
}

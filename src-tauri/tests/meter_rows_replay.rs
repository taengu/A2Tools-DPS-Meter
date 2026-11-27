//! Replays a capture the way the live meter runs it — parser, storage and the
//! 500 ms `get_dps` tick, with the 30 s history save beside it — and reports
//! what the main meter showed: rows named by a bare `#id`, and ticks where
//! the local player hit the target but had no row.
//!
//! A diagnostic: it prints and asserts nothing, and skips unless given
//! captures.
//!
//! - A2_METER_ROWS_CAPTURES: `;`-separated captures, each replayed as its own
//!   meter session (a restarted meter starts empty); `a+b+c` replays rotated
//!   logs of one session as one.
//! - A2_METER_ROWS_MODE: target mode (default bossTargets).
//! - A2_METER_ROWS_RESET_AT: `HH:MM:SS,...` times to press the reset button
//!   (A2_METER_ROWS_RESET_KIND=old for the reset that cleared every name).
//! - A2_METER_ROWS_ACTORS, A2_METER_ROWS_NAMES=id,..., A2_METER_ROWS_LOCALJOB:
//!   extra dumps. A2_METER_ROWS_DUMP_AT=HH:MM:SS,...: the rows on screen then.
//! - A2_METER_ROWS_OWN_BOSS: captures (as A2_METER_ROWS_CAPTURES) for
//!   `boss_mode_shows_only_our_bosses`, which asserts that Boss mode never put
//!   up an open-world boss that neither you, your summons nor your party hit.
//!
//! Times print in UTC+2 (the EU captures this was written for).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use a2tools_dps_meter_lib::capture::packet_accumulator::PacketAccumulator;
use a2tools_dps_meter_lib::capture::stream_processor::StreamProcessor;
use a2tools_dps_meter_lib::combat::data_storage::DataStorage;
use a2tools_dps_meter_lib::combat::dps_calculator::DpsCalculator;
use a2tools_dps_meter_lib::combat::ping_tracker::PingTracker;
use a2tools_dps_meter_lib::entity::fight_record::FightRecord;
use a2tools_dps_meter_lib::i18n::lookup::{NpcLookup, SkillLookup};
use a2tools_dps_meter_lib::share::read_capture;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src/data")
}

fn lookups() -> (Arc<NpcLookup>, Arc<SkillLookup>) {
    let npc = Arc::new(NpcLookup::new());
    let sk = Arc::new(SkillLookup::new());
    a2tools_dps_meter_lib::i18n::lookup::load_language(&sk, &npc, &data_dir(), "en");
    (npc, sk)
}

fn is_id_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_digit())
}

fn hms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.with_timezone(&chrono::FixedOffset::east_opt(2 * 3600).unwrap()).format("%H:%M:%S").to_string())
        .unwrap_or_default()
}

#[derive(Default, Debug)]
struct FightStats {
    target: i32,
    name: String,
    first_ms: i64,
    last_ms: i64,
    ticks: usize,
    max_rows: usize,
    id_row_ticks: usize,
    stale_id_ticks: usize,
    id_rows: HashSet<i32>,
    local_hit_ticks: usize,
    local_missing_ticks: usize,
    local_unknown_ticks: usize,
    local_missing_at: Vec<i64>,
}

/// Per-capture summary, for the assertions.
#[derive(Default, Debug)]
pub struct Summary {
    pub fights: usize,
    pub ticks: usize,
    pub id_row_ticks: usize,
    pub stale_id_ticks: usize,
    pub local_hit_ticks: usize,
    pub local_missing_ticks: usize,
    pub local_unknown_ticks: usize,
    pub saved_records: usize,
    pub saved_id_actors: usize,
    pub saved_local_missing: usize,
    /// Ticks where the game had named you and the `06 38` leader was you /
    /// someone else.
    pub scope_agree: usize,
    pub scope_disagree: usize,
    /// Ticks showing an open-world boss that neither the local player, their
    /// summons nor a party member had hit.
    pub foreign_boss_ticks: usize,
    /// Ticks showing a boss at all.
    pub boss_ticks: usize,
}

fn replay(paths: &[&Path], mode: &str, verbose: bool) -> Summary {
    let mut packets = Vec::new();
    for path in paths {
        packets.extend(read_capture(path).expect("capture readable"));
    }
    let (npc, sk) = lookups();
    let storage = Arc::new(DataStorage::new());
    let mut proc = StreamProcessor::new(storage.clone(), sk.clone(), npc.clone());
    if let Ok(text) = std::fs::read_to_string(data_dir().join("dot_skill_ids.json")) {
        if let Ok(ids) = serde_json::from_str::<Vec<i32>>(&text) {
            proc.set_dot_skill_ids(ids.into_iter().collect());
        }
    }
    let mut calc = DpsCalculator::new(storage.clone(), sk.clone(), npc.clone(), Arc::new(PingTracker::new()));
    calc.set_target_selection_mode(mode);
    let calc = Arc::new(Mutex::new(calc));
    let saved: Arc<Mutex<HashMap<String, (FightRecord, Option<i64>)>>> = Arc::new(Mutex::new(HashMap::new()));
    {
        let (calc, saved, store) = (calc.clone(), saved.clone(), Arc::downgrade(&storage));
        storage.set_before_reset(move || {
            let Some(s) = store.upgrade() else { return };
            if s.damage_generation() <= 0 {
                return;
            }
            let local = s.local_player_id();
            let records = calc.lock().unwrap().snapshot_boss_fights_force();
            let mut saved = saved.lock().unwrap();
            for r in records {
                saved.insert(r.id.clone(), (r, local));
            }
        });
    }

    let mut streams: HashMap<String, PacketAccumulator> = HashMap::new();
    let first = packets.first().map(|p| p.captured_at_ms).unwrap_or(0);
    let mut next_tick = first + 500;
    let mut next_save = first + 30_000;
    let mut fights: BTreeMap<(i32, i64), FightStats> = BTreeMap::new();
    let mut last_who: Option<(Option<i64>, Option<String>, bool)> = None;
    let mut last_roster: Vec<String> = Vec::new();
    // Every name an id was ever given, and when first.
    let mut named_at: HashMap<i32, (i64, String)> = HashMap::new();
    let mut summary = Summary::default();
    let mut id_rows_seen: HashMap<i32, (i64, i64)> = HashMap::new();
    let mut dumped: HashSet<String> = HashSet::new();

    // What the reset button / hotkey does (app.rs `reset_combat`), at these
    // capture times (HH:MM:SS, local).
    let mut resets: Vec<String> = std::env::var("A2_METER_ROWS_RESET_AT")
        .map(|v| v.split(',').map(str::to_string).collect())
        .unwrap_or_default();
    resets.sort();
    resets.reverse();
    for p in &packets {
        if resets.last().is_some_and(|r| hms(p.captured_at_ms) >= *r) {
            let r = resets.pop().unwrap();
            if verbose {
                println!("{} RESET (as the reset button) at {r}", hms(p.captured_at_ms));
            }
            calc.lock().unwrap().restart_target_selection(true);
            if std::env::var("A2_METER_ROWS_RESET_KIND").as_deref() == Ok("old") {
                storage.reset_nicknames();
            } else {
                storage.forget_guessed_nicknames();
            }
            storage.hide_party_placeholders();
        }
        proc.set_override_timestamp(Some(p.captured_at_ms));
        let acc = streams.entry(p.stream.clone()).or_insert_with(PacketAccumulator::new);
        acc.append(&p.bytes);
        let used = proc.consume_stream(acc.snapshot());
        if used > 0 {
            acc.discard_bytes(used);
        }

        let who = (storage.local_player_id(), storage.local_character_name(), storage.local_identity_from_self_record());
        if last_who.as_ref() != Some(&who) {
            if verbose {
                println!("{} identity -> id {:?} name {:?} self_record {}", hms(p.captured_at_ms), who.0, who.1, who.2);
            }
            last_who = Some(who.clone());
        }
        if p.captured_at_ms >= next_tick {
            next_tick = p.captured_at_ms + 500;
            let mut roster: Vec<String> = storage.get_party_members().keys().cloned().collect();
            roster.sort();
            if roster != last_roster && verbose {
                println!("{} roster -> {:?} dungeon {}", hms(p.captured_at_ms), roster, storage.current_dungeon_id());
                last_roster = roster;
            }
            for (id, name) in storage.get_nicknames() {
                named_at.entry(id).or_insert((p.captured_at_ms, name));
            }
            if storage.local_identity_from_game() {
                if let Some(leader) = storage.party_scope_leader() {
                    if Some(leader as i64) == storage.local_player_id() {
                        summary.scope_agree += 1;
                    } else {
                        summary.scope_disagree += 1;
                        if verbose && summary.scope_disagree < 5 {
                            println!("{} scope leader {leader} but local {:?}", hms(p.captured_at_ms), storage.local_player_id());
                        }
                    }
                }
            }
            let dps = calc.lock().unwrap().get_dps();
            if dps.map.is_empty() {
                continue;
            }
            let combat = storage.get_combat_snapshot_light();
            let summons = storage.get_summon_data();
            let key_target = dps.target_id;
            if let Ok(at) = std::env::var("A2_METER_ROWS_DUMP_AT") {
                let now = hms(p.captured_at_ms);
                if at.split(',').any(|t| t == now) && dumped.insert(now.clone()) {
                    let mut rows: Vec<_> = dps.map.iter().collect();
                    rows.sort_by(|a, b| b.1.amount.partial_cmp(&a.1.amount).unwrap_or(std::cmp::Ordering::Equal));
                    println!("{now} rows (target {} {:?}):", dps.target_id, dps.target_name);
                    for (id, d) in rows {
                        println!("    {id:>9} {:<16} {:<10} {:>12.0}", d.nickname, d.job, d.amount);
                    }
                }
            }
            let first_dmg = combat.get(&key_target).map(|t| t.first_damage_time).unwrap_or(0);
            let st = fights.entry((key_target, first_dmg)).or_insert_with(|| FightStats {
                target: key_target,
                name: dps.target_name.clone(),
                first_ms: p.captured_at_ms,
                ..Default::default()
            });
            st.last_ms = p.captured_at_ms;
            st.ticks += 1;
            st.max_rows = st.max_rows.max(dps.map.len());
            let id_rows: Vec<i32> = dps.map.iter().filter(|(_, d)| is_id_name(&d.nickname)).map(|(&id, _)| id).collect();
            if !id_rows.is_empty() {
                let names = storage.get_nicknames();
                let stale = id_rows.iter().filter(|id| names.contains_key(id) || summons.get(id).is_some_and(|o| names.contains_key(o))).count();
                if stale > 0 {
                    st.stale_id_ticks += 1;
                }
                st.id_row_ticks += 1;
                for &id in &id_rows {
                    st.id_rows.insert(id);
                    if verbose && !id_rows_seen.contains_key(&id) {
                        let d = &dps.map[&id];
                        let skills: HashSet<i32> = combat.values().filter_map(|t| t.actors.get(&id)).flat_map(|a| a.skills.keys().map(|k| k.0)).collect();
                        println!("{} new #id row {id} job '{}' amount {} known_player {} summon_of {:?} skills {:?} target {}", hms(p.captured_at_ms), d.job, d.amount, storage.is_known_player(id), summons.get(&id), skills, dps.target_id);
                    }
                    let e = id_rows_seen.entry(id).or_insert((p.captured_at_ms, p.captured_at_ms));
                    e.1 = p.captured_at_ms;
                }
            }
            // Did the local player hit the shown target(s)?
            let local = storage.local_player_id().map(|v| v as i32);
            if dps.target_is_boss {
                summary.boss_ticks += 1;
                let party = storage.get_party_members();
                let names = storage.get_nicknames();
                let ours = combat.get(&dps.target_id).is_some_and(|td| td.actors.keys().any(|&a| {
                    let owner = summons.get(&a).copied().unwrap_or(a);
                    Some(a) == local || Some(owner) == local || names.get(&owner).is_some_and(|n| party.contains_key(n))
                }));
                if !ours && storage.current_dungeon_id() <= 0 {
                    summary.foreign_boss_ticks += 1;
                    if verbose && summary.foreign_boss_ticks <= 3 {
                        println!("{} boss {} shown that no one of ours hit (local {:?})", hms(p.captured_at_ms), dps.target_id, local);
                    }
                }
            }
            let targets: Vec<i32> = dps.detail_target_ids.clone();
            match local {
                Some(lid) => {
                    let hit = targets.iter().filter_map(|t| combat.get(t)).any(|td| {
                        td.actors.keys().any(|&a| a == lid || summons.get(&a) == Some(&lid))
                    });
                    if hit {
                        st.local_hit_ticks += 1;
                        let lname = storage.local_character_name();
                        let present = dps.map.contains_key(&lid)
                            || lname.as_ref().is_some_and(|n| dps.map.values().any(|d| &d.nickname == n));
                        if !present {
                            st.local_missing_ticks += 1;
                            if st.local_missing_at.len() < 3 {
                                st.local_missing_at.push(p.captured_at_ms);
                            }
                        }
                    }
                }
                None => st.local_unknown_ticks += 1,
            }
        }
        if verbose && std::env::var("A2_METER_ROWS_LOCALJOB").is_ok() && p.captured_at_ms >= next_tick - 1 {
            if let Some(l) = storage.local_player_id() {
                let jobs: HashSet<String> = storage.get_combat_snapshot_light().values().filter_map(|t| t.actors.get(&(l as i32))).filter_map(|a| a.job.map(|j| j.class_name().to_string())).collect();
                if !jobs.is_empty() { println!("{} local {} jobs {:?}", hms(p.captured_at_ms), l, jobs); }
            }
        }
        if p.captured_at_ms >= next_save {
            next_save = p.captured_at_ms + 30_000;
            if storage.damage_generation() > 0 {
                let local = storage.local_player_id();
                let records = calc.lock().unwrap().snapshot_boss_fights();
                let mut saved = saved.lock().unwrap();
                for r in records {
                    saved.insert(r.id.clone(), (r, local));
                }
            }
        }
    }
    let local = storage.local_player_id();
    let records = calc.lock().unwrap().snapshot_boss_fights_force();
    for r in records {
        saved.lock().unwrap().insert(r.id.clone(), (r, local));
    }
    proc.set_override_timestamp(None);

    for st in fights.values() {
        summary.fights += 1;
        summary.ticks += st.ticks;
        summary.id_row_ticks += st.id_row_ticks;
        summary.stale_id_ticks += st.stale_id_ticks;
        summary.local_hit_ticks += st.local_hit_ticks;
        summary.local_missing_ticks += st.local_missing_ticks;
        summary.local_unknown_ticks += st.local_unknown_ticks;
        if verbose && st.ticks >= 4 {
            let named_later: Vec<String> = st
                .id_rows
                .iter()
                .map(|id| match named_at.get(id) {
                    Some((at, _)) => format!("{id}(named {})", hms(*at)),
                    None => format!("{id}(never)"),
                })
                .collect();
            println!(
                "fight {} '{}' {}-{} ticks {} rows<= {} idrow-ticks {} (stale {}) local hit {} missing {} unknown {} {:?} idrows {:?}",
                st.target,
                st.name,
                hms(st.first_ms),
                hms(st.last_ms),
                st.ticks,
                st.max_rows,
                st.id_row_ticks,
                st.stale_id_ticks,
                st.local_hit_ticks,
                st.local_missing_ticks,
                st.local_unknown_ticks,
                st.local_missing_at.iter().map(|&t| hms(t)).collect::<Vec<_>>(),
                named_later
            );
        }
    }
    if std::env::var("A2_METER_ROWS_ACTORS").is_ok() {
        let names = storage.get_nicknames();
        let mut by: HashMap<i32, (i64, Option<String>, i64, i64)> = HashMap::new();
        for t in storage.get_combat_snapshot_light().values() {
            for (&a, d) in &t.actors {
                let e = by.entry(a).or_insert((0, None, i64::MAX, 0));
                e.0 += d.total_damage;
                if e.1.is_none() { e.1 = d.job.map(|j| j.class_name().to_string()); }
                e.2 = e.2.min(t.first_damage_time);
                e.3 = e.3.max(d.last_damage_time);
            }
        }
        let mut v: Vec<_> = by.into_iter().collect();
        v.sort_by_key(|(_, d)| -d.0);
        for (id, (dmg, job, f, l)) in v.iter().take(40) {
            println!("actor {id} {:?} job {:?} dmg {dmg} {}-{} summon_of {:?}", names.get(id), job, hms(*f), hms(*l), storage.get_summon_data().get(id));
        }
    }
    if let Ok(ids) = std::env::var("A2_METER_ROWS_NAMES") {
        let names = storage.get_nicknames();
        for id in ids.split(',').filter_map(|v| v.parse::<i32>().ok()) {
            println!("name of {id}: {:?} (first {:?})", names.get(&id), named_at.get(&id));
        }
    }
    let saved = saved.lock().unwrap();
    let mut recs: Vec<_> = saved.values().collect();
    recs.sort_by_key(|(r, _)| r.start_time_ms);
    for (r, local) in recs {
        summary.saved_records += 1;
        let ids = r.actors.iter().filter(|a| is_id_name(&a.nickname) && !a.job.is_empty()).count();
        summary.saved_id_actors += ids;
        let local_in = local.is_some_and(|l| r.actors.iter().any(|a| a.actor_id as i64 == l));
        if !local_in {
            summary.saved_local_missing += 1;
        }
        if verbose {
            println!(
                "saved {} '{}' {} actors {} id-named(classed) {} local {:?} in {}",
                r.id,
                r.boss_name,
                hms(r.start_time_ms),
                r.actors.len(),
                ids,
                local,
                local_in
            );
        }
    }
    summary
}

#[test]
fn report_meter_rows() {
    let Ok(paths) = std::env::var("A2_METER_ROWS_CAPTURES") else {
        eprintln!("A2_METER_ROWS_CAPTURES unset; skipping");
        return;
    };
    let mode = std::env::var("A2_METER_ROWS_MODE").unwrap_or_else(|_| "bossTargets".into());
    for path in paths.split(';').filter(|p| !p.is_empty()) {
        println!("=== {path} ({mode})");
        let parts: Vec<&Path> = path.split('+').map(Path::new).collect();
        let s = replay(&parts, &mode, true);
        println!("summary {s:?}");
    }
}

/// Boss mode puts up only a boss that is ours, as the history saves it: a
/// world boss strangers fought out of sight (2026-10-08, eight players never
/// spawned near you, shown as `#id` rows) stayed on the meter for hours.
#[test]
fn boss_mode_shows_only_our_bosses() {
    let Ok(paths) = std::env::var("A2_METER_ROWS_OWN_BOSS") else {
        eprintln!("A2_METER_ROWS_OWN_BOSS unset; skipping");
        return;
    };
    for path in paths.split(';').filter(|p| !p.is_empty()) {
        let parts: Vec<&Path> = path.split('+').map(Path::new).collect();
        let s = replay(&parts, "bossTargets", false);
        println!("{path}: boss ticks {} foreign {}", s.boss_ticks, s.foreign_boss_ticks);
        assert!(s.boss_ticks > 0, "{path}: no boss shown at all");
        assert_eq!(s.foreign_boss_ticks, 0, "{path}: a stranger's boss shown");
    }
}

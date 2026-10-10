//! Other players' summons whose spawn arrives without an owner name.
//!
//! `packets_20261002_235935.txt` (A2_SUMMON_CAPTURE), 2026-10-03: a party
//! with two Elementalists, Thermia and the player recording, Nyxie, fights
//! Divine Auldor. Thermia's spirits spawn as kind-0x1F entities with no owner
//! name, so nothing in their spawn ties them to her; their damage records do
//! carry her power scalar (11910; Nyxie's is 11350). The meter showed each
//! spirit as its own `#id` row, while Thermia's own meter merged them.

use std::sync::Arc;

use a2tools_dps_meter_lib::capture::packet_accumulator::PacketAccumulator;
use a2tools_dps_meter_lib::capture::stream_processor::StreamProcessor;
use a2tools_dps_meter_lib::combat::data_storage::DataStorage;
use a2tools_dps_meter_lib::combat::dps_calculator::DpsCalculator;
use a2tools_dps_meter_lib::combat::ping_tracker::PingTracker;
use a2tools_dps_meter_lib::i18n::lookup::{NpcLookup, SkillLookup};
use a2tools_dps_meter_lib::share::read_capture;

const NYXIE: i64 = 13520;

#[test]
fn other_players_spirits_merge_into_their_owner() {
    let Ok(path) = std::env::var("A2_SUMMON_CAPTURE") else {
        eprintln!("A2_SUMMON_CAPTURE unset; skipping");
        return;
    };
    let storage = Arc::new(DataStorage::new());
    let mut processor =
        StreamProcessor::new(storage.clone(), Arc::new(SkillLookup::new()), Arc::new(NpcLookup::new()));
    let mut acc = PacketAccumulator::new();
    // At the captured times, as the log service replays: a saved fight needs
    // its real length.
    for p in read_capture(std::path::Path::new(&path)).unwrap() {
        processor.set_override_timestamp(Some(p.captured_at_ms));
        acc.append(&p.bytes);
        let used = processor.consume_stream(acc.snapshot());
        if used > 0 {
            acc.discard_bytes(used);
        }
    }
    // What the recording player's meter does: it knows itself as Nyxie (this
    // capture has no self record), as the UI's local binding sets it.
    storage.set_local_character_name(Some("Nyxie".into()));
    storage.set_local_player_id(Some(NYXIE));
    storage.set_permanent_nickname(NYXIE as i32, "Nyxie");

    let mut calc = DpsCalculator::new(
        storage.clone(),
        Arc::new(SkillLookup::new()),
        Arc::new(NpcLookup::new()),
        Arc::new(PingTracker::new()),
    );
    calc.set_target_selection_mode("allTargets");
    let dps = calc.get_dps();
    let mut rows: Vec<_> = dps.map.iter().collect();
    rows.sort_by(|a, b| b.1.amount.total_cmp(&a.1.amount));
    for (id, d) in &rows {
        println!("  #{id:<7} {:<12} {:<12} dmg={:>9.0}", d.nickname, d.job, d.amount);
    }
    let orphans: Vec<_> = rows
        .iter()
        .filter(|(id, d)| d.nickname.is_empty() || d.nickname == id.to_string())
        .map(|(id, d)| (**id, d.amount as i64))
        .collect();
    assert!(orphans.is_empty(), "unmerged rows: {orphans:?}");
    let named = |n: &str| rows.iter().find(|(_, d)| d.nickname == n).map(|(_, d)| d.amount).unwrap_or(0.0);
    // Thermia's own damage alone was 723,912 in this capture; her spirits add to it.
    assert!(named("Thermia") > 1_000_000.0, "Thermia {}", named("Thermia"));
    assert!(named("Nyxie") > 189_272.0, "Nyxie keeps her own spirits: {}", named("Nyxie"));

    // The fight as the player saw it: Divine Auldor alone, a minute long, where
    // the owners show far fewer skills than over the whole capture.
    let npcs = Arc::new(NpcLookup::new());
    npcs.load_from_json(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../src/data/i18n/npcs/en.json")).unwrap());
    let mut boss = DpsCalculator::new(storage.clone(), Arc::new(SkillLookup::new()), npcs.clone(), Arc::new(PingTracker::new()));
    boss.set_target_selection_mode("bossTargets");
    let dps = boss.get_dps();
    let mut rows: Vec<_> = dps.map.iter().collect();
    rows.sort_by(|a, b| b.1.amount.total_cmp(&a.1.amount));
    println!("boss only:");
    for (id, d) in &rows {
        println!("  #{id:<7} {:<12} {:<12} dmg={:>9.0}", d.nickname, d.job, d.amount);
    }
    let orphans: Vec<_> = rows
        .iter()
        .filter(|(id, d)| d.nickname.is_empty() || d.nickname == id.to_string())
        .map(|(id, d)| (**id, d.amount as i64))
        .collect();
    assert!(orphans.is_empty(), "unmerged rows in the boss fight: {orphans:?}");
    // Spirits whose skills name no class are merged, not dropped. The boss died,
    // so the total reads as its full HP (1,125,000); the meter never sees every
    // last hit, but the rows hold 1,108,484 of it, where dropping the classless
    // spirits left about 1.03M.
    let shown: f64 = rows.iter().map(|(_, d)| d.amount).sum();
    assert!(shown >= 0.98 * dps.target_total_damage as f64, "boss damage missing from the rows: {shown}");
    let live: std::collections::HashMap<i32, f64> = rows.iter().map(|(id, d)| (**id, d.amount)).collect();

    // History: the saved record of the same fight attributes each spirit the
    // same way. It used to fold every unclaimed Elementalist entity into
    // whichever of the two was named, or, with both named, none of them.
    let mut history = DpsCalculator::new(storage.clone(), Arc::new(SkillLookup::new()), npcs, Arc::new(PingTracker::new()));
    let records = history.snapshot_boss_fights_force();
    let auldor = records.iter().find(|r| r.mob_code == 2310218).expect("Divine Auldor saved");
    let mut per_actor: std::collections::HashMap<i32, i64> = std::collections::HashMap::new();
    for s in &auldor.details.skills {
        *per_actor.entry(s.actor_id).or_default() += s.dmg as i64;
    }
    println!("history: {per_actor:?}");
    assert_eq!(per_actor.len(), 5, "one row per player in history: {per_actor:?}");
    for id in [1792, NYXIE as i32] {
        let (saved, shown) = (per_actor[&id] as f64, live[&id]);
        assert!((saved - shown).abs() <= 0.01 * shown, "#{id}: history {saved}, meter {shown}");
    }
}

/// A Spiritmaster's spirits in an uploaded slice, as the log service derives
/// it.
///
/// `A2_SM_SLICE`: the slice of a2tools.app log `8iW_nvXE9po3RIVGrWgrxA`
/// (2026-10-10, Neglected Gadioton, Krao Cave). Five spirits spawned before
/// the slice began, so nothing in it links them to their owner. Their damage
/// carries the Spiritmaster's power scalars, and they cast 16xxxxxx skills,
/// which files them as players. The saved record never merged a player, so
/// each spirit kept a row of its own; the live meter merged them.
#[test]
fn spirits_spawned_before_the_slice_merge_into_their_owner() {
    let Ok(path) = std::env::var("A2_SM_SLICE") else {
        eprintln!("A2_SM_SLICE unset; skipping");
        return;
    };
    let slice = std::fs::read(path).unwrap();
    let fight = a2tools_dps_meter_lib::rederive::derive_fight(
        &slice,
        include_str!("../../src/data/i18n/npcs/en.json"),
        include_str!("../../src/data/i18n/skills/en.json"),
        include_str!("../../src/data/dot_skill_ids.json"),
    )
    .unwrap();
    let r = &fight.record;
    let ids: Vec<i32> = r.actors.iter().map(|a| a.actor_id).collect();
    println!("rows: {ids:?}");
    assert_eq!(ids, vec![1143, 6503, 8852, 8893, 9911], "one row per party member");
    // Hidden or merged, the spirits' 151,801 must land on the Spiritmaster.
    let sm: i64 = r.details.skills.iter().filter(|s| s.actor_id == 9911).map(|s| s.dmg as i64).sum();
    assert!(sm >= 803_708 + 150_000, "Spiritmaster row holds {sm}");
}

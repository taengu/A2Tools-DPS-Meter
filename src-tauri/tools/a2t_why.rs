//! `a2t-why`: why a slice derives no boss fight.
//!
//!   a2t-why <slice.a2es[.gz|.zst]> ...
//!
//! Replays each slice as the log service does (the whole slice, and again
//! cut short of the first zone reset after the pull) and prints one JSON line
//! per file: every target that took damage, its mob code, whether the NPC
//! table calls it a boss or a dummy, how long it was hit for and how hard,
//! and what the service's derive answers. No player names are printed.

use std::collections::HashSet;
use std::io::Read;
use std::sync::Arc;

use a2tools_dps_meter_lib::capture::evidence_slice;
use a2tools_dps_meter_lib::capture::stream_processor::StreamProcessor;
use a2tools_dps_meter_lib::combat::data_storage::DataStorage;
use a2tools_dps_meter_lib::i18n::lookup::{NpcLookup, SkillLookup};
use a2tools_dps_meter_lib::rederive::{derive_decoded_claiming, Claim, Tables};

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../src/data/");

fn read(path: &str) -> Option<Vec<u8>> {
    let raw = std::fs::read(path).ok()?;
    if raw.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        let mut out = Vec::new();
        zstd::stream::read::Decoder::new(&raw[..]).ok()?.read_to_end(&mut out).ok()?;
        Some(out)
    } else if raw.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(&raw[..]).read_to_end(&mut out).ok()?;
        Some(out)
    } else {
        Some(raw)
    }
}

fn main() {
    let npcs_json = std::fs::read_to_string(format!("{DATA}i18n/npcs/en.json")).unwrap();
    let skills_json = std::fs::read_to_string(format!("{DATA}i18n/skills/en.json")).unwrap();
    let dots_json = std::fs::read_to_string(format!("{DATA}dot_skill_ids.json")).unwrap();
    let tables = Tables::load(&npcs_json, &skills_json, &dots_json);
    let npcs = Arc::new(NpcLookup::new());
    npcs.load_from_json(&npcs_json);
    let skills = Arc::new(SkillLookup::new());
    skills.load_from_json(&skills_json);
    let dots: Option<HashSet<i32>> = serde_json::from_str::<Vec<i32>>(&dots_json).ok().map(|v| v.into_iter().collect());

    for path in std::env::args().skip(1) {
        let name = std::path::Path::new(&path).file_name().unwrap().to_string_lossy().to_string();
        let Some(raw) = read(&path) else {
            println!("{}", serde_json::json!({"file": name, "error": "unreadable"}));
            continue;
        };
        let Some(decoded) = evidence_slice::decode(&raw) else {
            println!("{}", serde_json::json!({"file": name, "error": "not_a_slice"}));
            continue;
        };
        let records = &decoded.0;
        // A2_CLAIM "mob:dungeon": the claim a meter's sweep would send.
        let claim = std::env::var("A2_CLAIM").ok().and_then(|v| {
            let (m, d) = v.split_once(':')?;
            Some(Claim { mob: m.parse().ok()?, dungeon: d.parse().ok()? })
        });
        let derive = match derive_decoded_claiming(&decoded, &tables, claim) {
            Ok(f) => serde_json::json!({"ok": true, "mob": f.record.mob_code, "by_adds": f.checks.boss_named_by_adds, "by_client": f.checks.boss_named_by_client}),
            Err(e) => serde_json::json!({"ok": false, "error": e.to_string()}),
        };

        let replay = |upto: usize| {
            let storage = Arc::new(DataStorage::new());
            // Every hit (time, target, damage), for the fight's timeline.
            let hits: Arc<std::sync::Mutex<Vec<(i64, i32, i64)>>> = Arc::default();
            {
                let hits = hits.clone();
                storage.set_on_hit(move |h| {
                    hits.lock().unwrap().push((h.timestamp(), h.target_id(), h.total_damage() as i64))
                });
            }
            let mut p = StreamProcessor::new(storage.clone(), skills.clone(), npcs.clone());
            if let Some(ids) = &dots {
                p.set_dot_skill_ids(ids.clone());
            }
            let mut reset_at = None;
            let mut known = 0usize;
            let mut spawned: Vec<(i32, i32)> = Vec::new();
            for (n, (dt, packet)) in records.iter().enumerate().take(upto) {
                p.set_override_timestamp(Some(*dt as i64));
                let before = (storage.last_zone_reset_ms(), storage.party_ended_at_ms());
                p.consume_stream(packet);
                // When each NPC code first appears: an add spawned during the
                // fight is the boss's own mechanic.
                let next_dt = records.get(n + 1).map(|r| r.0).unwrap_or(i32::MAX);
                if *dt >= 0 || next_dt >= 0 {
                    let mobs = storage.get_mob_data();
                    if mobs.len() != known {
                        known = mobs.len();
                        let have: HashSet<i32> = spawned.iter().map(|s| s.1).collect();
                        let at = if *dt >= 0 { *dt } else { i32::MIN };
                        spawned.extend(mobs.keys().filter(|id| !have.contains(id)).map(|&id| (at, id)));
                    }
                }
                if reset_at.is_none() && *dt > 0 && (storage.last_zone_reset_ms(), storage.party_ended_at_ms()) != before {
                    reset_at = Some(n);
                }
            }
            let hits = std::mem::take(&mut *hits.lock().unwrap());
            (storage, reset_at, spawned, hits)
        };
        let targets_of = |storage: &DataStorage| {
            let mobs = storage.get_mob_data();
            let mut out: Vec<serde_json::Value> = storage
                .get_combat_snapshot_light()
                .values()
                .filter(|t| t.total_damage > 0)
                .map(|t| {
                    let code = mobs.get(&t.target_id).copied();
                    serde_json::json!({
                        "code": code,
                        "npc": code.map(|c| npcs.get_npc_name(c)),
                        "boss": code.is_some_and(|c| npcs.is_boss(c)),
                        "dummy": code.is_some_and(|c| npcs.is_training_dummy(c)),
                        "first": t.first_damage_time,
                        "span": (t.last_damage_time - t.first_damage_time).max(0),
                        "damage": t.total_damage,
                        "actors": t.actors.len(),
                        "max_hp": storage.get_mob_hp(t.target_id),
                        "cur_hp": storage.get_mob_current_hp(t.target_id),
                        "dead": storage.get_dead_entities().contains(&t.target_id),
                    })
                })
                .collect();
            out.sort_by_key(|v| -v["damage"].as_i64().unwrap_or(0));
            out.truncate(8);
            out
        };
        let (full, reset_at, spawned, hits) = replay(records.len());
        // What the top anonymous target did itself: the skills it hit anyone
        // with, which name the boss when its spawn is missing.
        let snap = full.get_combat_snapshot();
        let top = snap.values().filter(|t| t.total_damage > 0).max_by_key(|t| t.total_damage).map(|t| t.target_id);
        let mut boss_skills: std::collections::BTreeMap<i32, i64> = Default::default();
        let mut boss_victims = 0;
        if let Some(top) = top {
            for t in snap.values() {
                if let Some(a) = t.actors.get(&top) {
                    boss_victims += 1;
                    for ((code, _), s) in &a.skills {
                        *boss_skills.entry(*code).or_default() += s.total_damage as i64;
                    }
                }
            }
        }
        let cut = reset_at.map(|n| replay(n).0);
        // NPCs spawned while the top target was being hit, by code.
        let mobs_all = full.get_mob_data();
        let window = top.and_then(|id| snap.get(&id)).map(|t| (t.first_damage_time, t.last_damage_time));
        let mut fight_spawns: Vec<i32> = spawned
            .iter()
            .filter(|(at, _)| window.is_some_and(|(a, b)| (*at as i64) >= a && (*at as i64) <= b))
            .filter_map(|(_, id)| mobs_all.get(id).copied())
            .collect();
        fight_spawns.sort();
        fight_spawns.dedup();
        // The top target's timeline: damage per 5 s from its first hit, the
        // pauses of 3 s or more with no damage on it, and its stagger windows
        // (gauge max, start and end from the first hit, broken or not).
        let mut buckets: Vec<i64> = Vec::new();
        let mut gaps: Vec<(i64, i64)> = Vec::new();
        let mut stagger: Vec<serde_json::Value> = Vec::new();
        if let (Some(id), Some((a, b))) = (top, window) {
            let mut times: Vec<(i64, i64)> = hits.iter().filter(|h| h.1 == id).map(|h| (h.0, h.2)).collect();
            times.sort();
            buckets = vec![0; ((b - a) / 5000 + 1) as usize];
            let mut last = a;
            for (t, d) in &times {
                if let Some(slot) = buckets.get_mut(((t - a).max(0) / 5000) as usize) {
                    *slot += d;
                }
                if t - last >= 3000 {
                    gaps.push((last - a, t - last));
                }
                last = *t;
            }
            for w in full.fight_stagger(a, b).into_iter().filter(|w| w.entity == id) {
                stagger.push(serde_json::json!({"max": w.max, "start": w.start, "end": w.end,
                                                "result": format!("{:?}", w.result)}));
            }
        }
        let mut seen: Vec<i32> = full.get_mob_data().values().copied().collect();
        seen.sort();
        seen.dedup();
        println!(
            "{}",
            serde_json::json!({
                "file": name,
                "records": records.len(),
                "from": records.first().map(|r| r.0),
                "to": records.last().map(|r| r.0),
                "dungeon": full.current_dungeon_id(),
                "early": records.iter().filter(|r| r.0 < -1_860_000).map(|r| r.0).collect::<Vec<_>>(),
                "reset_at": reset_at.map(|n| records[n].0),
                "derive": derive,
                "top_id": top,
                "npcs_seen": seen,
                "fight_spawns": fight_spawns,
                "buckets": buckets,
                "gaps": gaps,
                "stagger": stagger,
                "boss_victims": boss_victims,
                "boss_skills": boss_skills,
                "targets": targets_of(&full),
                "targets_cut": cut.as_ref().map(|s| targets_of(s)),
            })
        );
    }
}

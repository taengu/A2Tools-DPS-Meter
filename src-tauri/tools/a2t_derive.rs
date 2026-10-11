//! a2t-derive: prove the log service's derivation against real captures.
//!
//!   a2t-derive <app-data-dir> [fight-id ...]   saved fights a capture covers
//!   a2t-derive --capture <packets_*.txt>        every boss fight in a capture
//!   a2t-derive --capture <packets_*.txt> --partner <dir>
//!                                               the same, and each fight's slice
//!                                               cut by another meter (see below)
//!
//! For each fight: replay the WHOLE capture the way the live meter reads it,
//! cut the Evidence Slice an upload would send, run `rederive::derive_fight`
//! over the slice alone (the function the service runs, with the same data
//! tables), and compare the two record for record: per actor, per skill, hit
//! counts. The comparison is made before the service hides unplaced summons
//! (`hide_unplaced_summons`), which the whole-capture record never goes
//! through; the damage the hide removes is printed on its own line. A saved fight's record is shown too, but only as context: it was
//! made by whichever build was running that day, so it can differ from both
//! for reasons that have nothing to do with the slice.
//!
//! "identical" here is the claim the upload design rests on: a log's numbers
//! can be reproduced from the slice alone.
//!
//! `--partner` is the conformance check for other meters that upload to
//! a2tools.app (docs/third-party-meters.md). Their meter cuts a slice for each
//! boss fight in the same capture and writes them to `<dir>` (`.a2es` or
//! `.a2es.gz`, any names). Each is derived as the service would derive it and
//! held to the same standard as ours: the fight the whole capture shows, row
//! for row, with nothing left unblinded. Everything runs locally; neither
//! the capture nor the slices go anywhere.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use a2tools_dps_meter_lib::capture::evidence_slice::{self, CapturedPacket, NameMap};
use a2tools_dps_meter_lib::capture::packet_accumulator::PacketAccumulator;
use a2tools_dps_meter_lib::capture::stream_processor::StreamProcessor;
use a2tools_dps_meter_lib::combat::data_storage::DataStorage;
use a2tools_dps_meter_lib::combat::dps_calculator::DpsCalculator;
use a2tools_dps_meter_lib::combat::ping_tracker::PingTracker;
use a2tools_dps_meter_lib::entity::fight_record::FightRecord;
use a2tools_dps_meter_lib::i18n::lookup::{NpcLookup, SkillLookup};
use a2tools_dps_meter_lib::rederive::{derive_fight_unhidden, derive_fight_unhidden_every, DerivedFight};
use a2tools_dps_meter_lib::share::{find_captures, read_capture};

struct Tables {
    npcs: String,
    skills: String,
    dots: String,
    /// `--capture` compares every boss fight in the capture, so slices are
    /// derived counting every fight too, not only the uploader's own: other
    /// players' fights showed NO SLICE (Seralth, #37, "2 of 25").
    every_fight: bool,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: a2t-derive <app-data-dir> [fight-id ...] | --capture <file>");
        std::process::exit(2);
    }
    let data = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src/data");
    let t = Tables {
        npcs: std::fs::read_to_string(data.join("i18n/npcs/en.json")).expect("npcs/en.json"),
        skills: std::fs::read_to_string(data.join("i18n/skills/en.json")).expect("skills/en.json"),
        dots: std::fs::read_to_string(data.join("dot_skill_ids.json")).expect("dot_skill_ids.json"),
        every_fight: args[0] == "--capture",
    };

    let partner = args.iter().position(|a| a == "--partner")
        .and_then(|i| args.get(i + 1))
        .map(|dir| partner_slices(Path::new(dir), &t));
    let (tried, same) = if args[0] == "--capture" {
        from_capture(Path::new(args.get(1).expect("capture path")), &t, partner.as_deref())
    } else {
        from_history(Path::new(&args[0]), &args[1..], &t)
    };
    println!("\n{same} of {tried} fights: slice re-derived identically to the whole capture");
    std::process::exit(if same == tried && tried > 0 { 0 } else { 1 });
}

/// Saved fights that some capture in the data dir covers.
fn from_history(dir: &Path, wanted: &[String], t: &Tables) -> (usize, usize) {
    let captures = find_captures(dir);
    let mut fights: Vec<FightRecord> = std::fs::read_dir(dir.join("history"))
        .expect("history dir")
        .filter_map(|e| e.ok())
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|txt| serde_json::from_str::<FightRecord>(&txt).ok())
        .filter(|r| wanted.is_empty() || wanted.contains(&r.id))
        .collect();
    fights.sort_by_key(|r| r.start_time_ms);

    let (mut tried, mut same) = (0, 0);
    for saved in &fights {
        let packets = covering(&captures, saved.start_time_ms);
        if packets.is_empty() {
            continue;
        }
        let (storage, whole) = replay(&packets, t, false);
        let Some(w) = whole
            .into_iter()
            .filter(|r| r.mob_code == saved.mob_code)
            .min_by_key(|r| (r.start_time_ms - saved.start_time_ms).abs())
        else {
            continue; // the capture does not actually contain this fight
        };
        tried += 1;
        println!("\n== {} {} (saved record: total {}, whole capture: total {})",
                 saved.id, saved.boss_name, saved.details.total_target_damage,
                 w.details.total_target_damage);
        if check(&packets, &storage, &w, t) {
            same += 1;
        }
        ring_check(&packets, &storage, &w, t);
    }
    (tried, same)
}

/// The automatic path: the same segments through the in-memory ring, the
/// slice cut by `save_slice` as the auto-save would, read back and derived.
fn ring_check(packets: &[CapturedPacket], storage: &DataStorage, w: &FightRecord, t: &Tables) {
    use std::io::Read;
    // The ring is process-wide, like the live one. Feed each capture once, or
    // a second fight from the same capture sees every packet twice.
    static FED: Mutex<Vec<i64>> = Mutex::new(Vec::new());
    let mark = packets.first().map(|p| p.captured_at_ms).unwrap_or(0);
    let fresh = { let mut fed = FED.lock().unwrap(); if fed.contains(&mark) { false } else { fed.push(mark); true } };
    for p in packets.iter().filter(|_| fresh) {
        a2tools_dps_meter_lib::share::ring::record_at(p.captured_at_ms, p.stream.clone(), &p.bytes);
    }
    let dir = std::env::temp_dir().join(format!("a2t-ring-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let result = a2tools_dps_meter_lib::share::save_slice(&dir, w, storage).and_then(|bytes| {
        let gz = std::fs::read(a2tools_dps_meter_lib::share::slices_dir(&dir).join(format!("{}.a2es.gz", w.id)))
            .map_err(|e| e.to_string())?;
        let mut slice = Vec::new();
        flate2::read::GzDecoder::new(&gz[..]).read_to_end(&mut slice).map_err(|e| e.to_string())?;
        let (d, _) = derive(&slice, t).map_err(|e| format!("{e:?}"))?;
        Ok((bytes, d))
    });
    match result {
        Ok((bytes, d)) => {
            let same = d.record.details.total_target_damage == w.details.total_target_damage
                && d.record.details.skills.len() == w.details.skills.len()
                && d.record.mob_code == w.mob_code;
            println!("   from memory (no packet logging): {} bytes gzipped, total {} -> {}",
                     bytes, d.record.details.total_target_damage,
                     if same { "identical" } else { "DIFFERENT" });
        }
        Err(e) => println!("   from memory: failed: {e}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every boss fight in one capture file.
fn from_capture(path: &Path, t: &Tables, partner: Option<&[(String, DerivedFight, i64)]>) -> (usize, usize) {
    let packets = read_capture(path).expect("capture");
    // Every boss fight in the capture, whoever fought it: the meter keeps
    // only fights of yours or your party's (`is_our_fight`), which left out
    // an open-world boss with no party and you not among its actors
    // (Blooming Korin). A slice of it can still be compared.
    let (storage, mut whole) = replay(&packets, t, true);
    whole.sort_by_key(|r| r.start_time_ms);
    // Training dummies are never uploaded (the meter cuts no slice for them),
    // and in town many players hit several at once, so a slice cannot say
    // which dummy fight it is: Kazumi's capture, all four mismatches.
    let dummies = whole.iter().filter(|r| r.is_train).count();
    whole.retain(|r| !r.is_train);
    if dummies > 0 {
        println!("{dummies} training-dummy fights left out: never uploaded");
    }
    let (mut tried, mut same) = (0, 0);
    for w in &whole {
        tried += 1;
        println!("\n== {} {} ({} ms, {} actors, total {})", w.id, w.boss_name, w.duration_ms,
                 w.actors.len(), w.details.total_target_damage);
        let ours = check(&packets, &storage, w, t);
        let theirs = partner.is_none_or(|p| check_partner(p, w));
        if ours && theirs {
            same += 1;
        }
    }
    // A partner slice for a fight the whole capture does not show is a fight
    // one of the two readings lost: say so rather than skip it.
    let partners = partner.unwrap_or(&[]);
    for (name, d, _) in partners {
        let paired = whole.iter().any(|w| partner_of(partners, w).is_some_and(|(n, _, _)| n == name));
        if paired {
            continue;
        }
        tried += 1;
        println!("
== partner {name}: {} (target {}, mob {}, {} ms, total {}) matches no fight in the whole capture -> DOES NOT CONFORM",
                 d.record.boss_name, d.record.target_id, d.record.mob_code, d.record.duration_ms,
                 d.record.details.total_target_damage);
    }
    (tried, same)
}

/// Every slice in `dir`, derived as the log service derives it.
fn partner_slices(dir: &Path, t: &Tables) -> Vec<(String, DerivedFight, i64)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).expect("partner slice folder").flatten() {
        let path = entry.path();
        let file = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        if !(file.ends_with(".a2es") || file.ends_with(".a2es.gz")) {
            continue;
        }
        let Ok(raw) = std::fs::read(&path) else { continue };
        let slice = if raw.starts_with(&[0x1f, 0x8b]) {
            let mut s = Vec::new();
            if std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&raw[..]), &mut s).is_err() {
                continue;
            }
            s
        } else {
            raw
        };
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        match derive(&slice, t) {
            Ok((d, hidden)) => out.push((name, d, hidden)),
            Err(e) => println!("partner slice {name}: not derivable ({e:?})"),
        }
    }
    println!("{} partner slices derived", out.len());
    out
}

/// Hold another meter's slice of `w` to the standard ours is held to.
fn check_partner(partner: &[(String, DerivedFight, i64)], w: &FightRecord) -> bool {
    let Some((name, d, hidden)) = partner_of(partner, w) else {
        println!("   partner: NO SLICE for target {}", w.target_id);
        return false;
    };
    let rows = |r: &FightRecord| -> HashMap<(i32, i32, bool), (i64, i32)> {
        r.details.skills.iter().map(|s| ((s.actor_id, s.code, s.is_dot), (s.dmg as i64, s.time))).collect()
    };
    let (a, b) = (rows(w), rows(&d.record));
    let differing = a.keys().chain(b.keys()).collect::<HashSet<_>>()
        .into_iter().filter(|k| a.get(*k) != b.get(*k)).count();
    let c = &d.checks;
    let blinded = c.unblinded_names == 0;
    let identical = w.mob_code == d.record.mob_code && differing == 0
        && w.details.total_target_damage == d.record.details.total_target_damage;
    println!("   partner {name}: total {} / {}  rows differing {}  unblinded {}  lifted {}/{}  bundles {}  -> {}",
             w.details.total_target_damage, d.record.details.total_target_damage, differing,
             c.unblinded_names, c.lifted, c.records, c.bundles,
             if identical && blinded { "conforms" } else { "DOES NOT CONFORM" });
    print_hidden(*hidden, d);
    identical && blinded
}

fn covering(captures: &[PathBuf], at_ms: i64) -> Vec<CapturedPacket> {
    let mut all = Vec::new();
    for c in captures {
        let Ok(p) = read_capture(c) else { continue };
        let (Some(f), Some(l)) = (p.first(), p.last()) else { continue };
        if f.captured_at_ms <= at_ms && l.captured_at_ms >= at_ms {
            all.extend(p);
        }
    }
    all.sort_by_key(|p| p.captured_at_ms);
    all
}

fn lookups(t: &Tables) -> (Arc<NpcLookup>, Arc<SkillLookup>) {
    let npc = Arc::new(NpcLookup::new());
    npc.load_from_json(&t.npcs);
    let sk = Arc::new(SkillLookup::new());
    sk.load_from_json(&t.skills);
    (npc, sk)
}

/// The whole capture, reassembled and replayed the way the live meter reads it.
/// `every_fight` keeps boss fights the meter would leave as someone else's.
fn replay(packets: &[CapturedPacket], t: &Tables, every_fight: bool) -> (Arc<DataStorage>, Vec<FightRecord>) {
    let (npc, sk) = lookups(t);
    let storage = Arc::new(DataStorage::new());
    let mut proc = StreamProcessor::new(storage.clone(), sk.clone(), npc.clone());
    if let Ok(ids) = serde_json::from_str::<Vec<i32>>(&t.dots) {
        proc.set_dot_skill_ids(ids.into_iter().collect());
    }
    let mut streams: HashMap<String, PacketAccumulator> = HashMap::new();
    // Saved as the live meter saves them: every 30 seconds, a fight's record
    // rewritten while it runs and frozen once it has gone quiet. A snapshot
    // taken only at the end of the capture is not what anyone saw: by then a
    // player who changed entity id has had their damage moved off the old id,
    // and a dummy hit again has been reset, which made correct slices look wrong.
    let mut calc = DpsCalculator::new(storage.clone(), sk, npc, Arc::new(PingTracker::new()));
    calc.set_every_fight(every_fight);
    let calc = Arc::new(Mutex::new(calc));
    let saved: Arc<Mutex<HashMap<String, FightRecord>>> = Arc::new(Mutex::new(HashMap::new()));
    // And before combat is cleared (a zone change, the end of a party), as
    // the meter does since #19: a fight followed by a teleport before the
    // next tick was left at that tick's record, short of its end (Ultimate
    // Berk, Caretaker Sinandash), or lost when it had no tick yet.
    {
        let (calc, saved, store) = (calc.clone(), saved.clone(), Arc::downgrade(&storage));
        storage.set_before_reset(move || {
            if store.upgrade().is_none_or(|s| s.damage_generation() <= 0) {
                return;
            }
            let records = calc.lock().unwrap().snapshot_boss_fights_force();
            let mut saved = saved.lock().unwrap();
            for r in records {
                saved.insert(r.id.clone(), r);
            }
        });
    }
    let mut next_save = packets.first().map(|p| p.captured_at_ms + 30_000).unwrap_or(0);
    for p in packets {
        proc.set_override_timestamp(Some(p.captured_at_ms));
        let acc = streams.entry(p.stream.clone()).or_insert_with(PacketAccumulator::new);
        acc.append(&p.bytes);
        let used = proc.consume_stream(acc.snapshot());
        if used > 0 {
            acc.discard_bytes(used);
        }
        if p.captured_at_ms >= next_save {
            let records = calc.lock().unwrap().snapshot_boss_fights();
            let mut saved = saved.lock().unwrap();
            for r in records {
                saved.insert(r.id.clone(), r);
            }
            next_save = p.captured_at_ms + 30_000;
        }
    }
    // Fights still running when the capture stops: the meter saves those on
    // its next tick, which a capture that ends never reaches. (Fights already
    // frozen are not in this snapshot.)
    let records = calc.lock().unwrap().snapshot_boss_fights_force();
    let mut saved = std::mem::take(&mut *saved.lock().unwrap());
    for r in records {
        saved.insert(r.id.clone(), r);
    }
    proc.set_override_timestamp(None);
    (storage, saved.into_values().collect())
}

/// Cut the slice for `w`, derive it, and compare. True when identical.
fn check(packets: &[CapturedPacket], storage: &DataStorage, w: &FightRecord, t: &Tables) -> bool {
    // The names the meter resolved, which is what the upload blinds.
    let mut names: NameMap = NameMap::new();
    for (name, member) in storage.get_party_members() {
        names.insert(name.clone(), member.dbid);
    }
    for name in storage.get_nicknames().values() {
        names.entry(name.clone()).or_insert(0);
    }
    let slice = match evidence_slice::build(packets, w.start_time_ms, w.start_time_ms + w.duration_ms, &names, [7; 32]) {
        Ok(s) => evidence_slice::encode(&s),
        Err(e) => {
            println!("   slice failed: {e:?}");
            return false;
        }
    };
    let (d, hidden) = match derive(&slice, t) {
        Ok(d) => d,
        Err(e) => {
            println!("   derive failed: {e:?} ({} bytes of slice)", slice.len());
            // What the slice does hold, against the fight it should have held.
            if let Ok(enc) = a2tools_dps_meter_lib::rederive::derive(&slice) {
                let fought = enc.targets.iter().find(|t| t.target_id == w.target_id);
                println!("     the fight's target {} (mob {}): {}", w.target_id, w.mob_code,
                         match fought {
                             Some(t) => format!("in the slice as mob {}, {} damage over {} ms",
                                                t.mob_code, t.total_damage, t.duration_ms),
                             None => "not in the slice".to_string(),
                         });
                for t in enc.targets.iter().take(4) {
                    println!("     target {} mob {} damage {} over {} ms", t.target_id, t.mob_code,
                             t.total_damage, t.duration_ms);
                }
            }
            return false;
        }
    };
    let got = &d.record;
    {
        let names_in: Vec<String> = names.keys().cloned().collect();
        let leaked = evidence_slice::decode(&slice)
            .map(|(r, _)| evidence_slice::leaked_names(&r, &names_in))
            .unwrap_or(0);
        let c = &d.checks;
        println!(
            "   checks: unblinded {} leaked {}  lifted {}/{} ({:.1}%) bundles {}  killed {} damage {} max_hp {} -> {:.1}%",
            c.unblinded_names, leaked, c.lifted, c.records,
            if c.records > 0 { c.lifted as f64 * 100.0 / c.records as f64 } else { 0.0 },
            c.bundles, c.killed, c.damage, c.max_hp,
            if c.max_hp > 0 { c.damage as f64 * 100.0 / c.max_hp as f64 } else { 0.0 });
    }
    if let Ok(dir) = std::env::var("A2_WRITE_SLICE") {
        // What an upload sends, and what the service must answer, for testing
        // the deployed Worker against this build.
        let dir = Path::new(&dir);
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(dir.join(format!("{}.a2es.gz", w.id)),
                               a2tools_dps_meter_lib::share::gzip(&slice).unwrap_or_default());
        let _ = std::fs::write(dir.join(format!("{}.derived.json", w.id)),
                               serde_json::to_vec(&d).unwrap_or_default());
    }

    let rows = |r: &FightRecord| -> HashMap<(i32, i32, bool), (i64, i32)> {
        r.details.skills.iter().map(|s| ((s.actor_id, s.code, s.is_dot), (s.dmg as i64, s.time))).collect()
    };
    let (a, b) = (rows(w), rows(got));
    let keys: HashSet<_> = a.keys().chain(b.keys()).copied().collect();
    let mut diffs: Vec<_> = keys.into_iter().filter(|k| a.get(k) != b.get(k)).collect();
    diffs.sort();

    let same_boss = w.mob_code == got.mob_code;
    let same_dungeon = w.dungeon_id == got.dungeon_id;
    let identical = same_boss && diffs.is_empty()
        && w.details.total_target_damage == got.details.total_target_damage;
    // The region a log is filed under comes from this; a slice that loses
    // the record naming the server files the log as "unknown".
    let same_server = w.server_id == got.server_id;
    println!("   slice {} bytes ({} gzipped): boss {} target {}/{} {}  dungeon {}/{} {}  server {}/{} {}  total {} / {} over {} / {} ms  skill rows {} / {}  -> {}",
             slice.len(), gz_len(&slice), got.mob_code,
             w.target_id, got.target_id, if w.target_id == got.target_id { "ok" } else { "MISMATCH" },
             w.dungeon_id, got.dungeon_id,
             if same_dungeon { "ok" } else { "MISMATCH" },
             w.server_id, got.server_id, if same_server { "ok" } else { "MISMATCH" },
             w.details.total_target_damage, got.details.total_target_damage, w.duration_ms, got.duration_ms,
             a.len(), b.len(), if identical { "identical" } else { "DIFFERENT" });
    for k in diffs.iter().take(if std::env::var("A2_ALL_ROWS").is_ok() { usize::MAX } else { 12 }) {
        println!("     row {:?}: whole {:?} slice {:?}", k, a.get(k), b.get(k));
    }
    print_hidden(hidden, &d);
    identical
}

/// The slice derived as the log service derives it, but before
/// `hide_unplaced_summons`, and the damage that hide removes. The whole-capture
/// record never goes through the hide, so the two are compared before it:
/// compared after, every hidden row was a difference (Phantasm Kasia, 296
/// unnamed actors, 4.1% of the damage). What the service stores is the hidden
/// record; `print_hidden` says how far it is from the one compared.
fn derive(slice: &[u8], t: &Tables) -> Result<(DerivedFight, i64), a2tools_dps_meter_lib::rederive::DeriveError> {
    if t.every_fight {
        derive_fight_unhidden_every(slice, &t.npcs, &t.skills, &t.dots)
    } else {
        derive_fight_unhidden(slice, &t.npcs, &t.skills, &t.dots)
    }
}

/// The partner slice of `w`. A target id can be fought twice in one capture
/// (two Blooming Korin fights, Seralth's krao capture, #37), and a slice's
/// clock starts at its own fight, so its start cannot be matched; among the
/// slices of that target and boss, the one nearest in total and length.
fn partner_of<'a>(
    partner: &'a [(String, DerivedFight, i64)],
    w: &FightRecord,
) -> Option<&'a (String, DerivedFight, i64)> {
    partner
        .iter()
        .filter(|(_, d, _)| d.record.target_id == w.target_id && d.record.mob_code == w.mob_code)
        .min_by_key(|(_, d, _)| {
            let total = (d.record.details.total_target_damage - w.details.total_target_damage).abs();
            let length = (d.record.duration_ms - w.duration_ms).abs();
            (total, length)
        })
}

fn print_hidden(hidden: i64, d: &DerivedFight) {
    if hidden != 0 {
        println!("   the service hides unplaced summons: {} damage ({:.1}%), stored total {}",
                 hidden,
                 if d.total_damage > 0 { hidden as f64 * 100.0 / d.total_damage as f64 } else { 0.0 },
                 d.total_damage - hidden);
    }
}

fn gz_len(data: &[u8]) -> usize {
    a2tools_dps_meter_lib::share::gzip(data).map(|g| g.len()).unwrap_or(0)
}

//! The acceptance test for the Evidence Slice.
//!
//! Two things have to be true of a slice, and neither can be established by
//! reading the code:
//!
//! 1. **It still derives the same fight.** The allowlist in `evidence_slice.rs`
//!    is a hand-maintained list of the opcodes `stream_processor.rs` reads. A
//!    hand-maintained list drifts. So rather than trust it, replay the slice and
//!    compare the damage to what the full capture produced — if the allowlist
//!    dropped something the parser needed, the numbers move and this fails.
//!
//! 2. **No name survives.** The blinder is substring replacement over the names
//!    the meter resolved. This asserts against the real names in a real capture,
//!    which is the only way to find out that some record shape hid one.
//!
//! Needs the reference capture:
//!   A2_REPLAY_CAPTURE=.../packets_20260815_183732.txt cargo test --test evidence_slice_replay
//! Another capture also needs its end: A2_REPLAY_UNTIL=2099 reads all of it.

use std::collections::HashMap;
use std::sync::Arc;

use a2tools_dps_meter_lib::capture::evidence_slice::{self, CapturedPacket, NameMap};
use a2tools_dps_meter_lib::capture::packet_accumulator::PacketAccumulator;
use a2tools_dps_meter_lib::capture::stream_processor::StreamProcessor;
use a2tools_dps_meter_lib::combat::data_storage::DataStorage;
use a2tools_dps_meter_lib::i18n::lookup::{NpcLookup, SkillLookup};

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    let b = hex.as_bytes();
    if b.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(b.len() / 2);
    for pair in b.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

/// The reference capture ends with the party leaving the instance, and a zone
/// change resets combat (`note_zone_change`), which would leave us comparing two
/// empty snapshots and calling it a pass. Cut at the same point the identity
/// replay test does: just after the run's last damage, before the party zones.
const UNTIL: &str = "2026-08-15T18:47:30";

/// `TIMESTAMP|STREAMKEY|HEX`, where TIMESTAMP is ISO-8601 local time.
struct Line {
    at_ms: i64,
    key: String,
    bytes: Vec<u8>,
}

fn read_capture(path: &str) -> Vec<Line> {
    let text = std::fs::read_to_string(path).expect("capture file");
    // A2_REPLAY_UNTIL overrides the end for other captures.
    let until = std::env::var("A2_REPLAY_UNTIL").unwrap_or_else(|_| UNTIL.to_string());
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.splitn(3, '|').collect();
        if parts.len() != 3 {
            continue;
        }
        let raw_ts = parts[0].trim();
        // Lexicographic, which works because the stamps are ISO-8601.
        if raw_ts >= until.as_str() {
            break;
        }
        let Ok(ts) = chrono::DateTime::parse_from_rfc3339(raw_ts) else {
            continue;
        };
        let Some(bytes) = decode_hex(parts[2]) else {
            continue;
        };
        out.push(Line {
            at_ms: ts.timestamp_millis(),
            key: parts[1].to_string(),
            bytes,
        });
    }
    out
}

fn new_processor(storage: Arc<DataStorage>) -> StreamProcessor {
    StreamProcessor::new(storage, Arc::new(SkillLookup::new()), Arc::new(NpcLookup::new()))
}

/// Damage per (target, actor). Keyed by entity id, which blinding does not
/// touch — comparing by name would compare a name against its own token.
fn damage_by_entity(storage: &DataStorage) -> HashMap<(i32, i32), i64> {
    let mut out = HashMap::new();
    for (target_id, target) in storage.get_combat_snapshot_light() {
        for (actor_id, actor) in &target.actors {
            if actor.total_damage > 0 {
                out.insert((target_id, *actor_id), actor.total_damage);
            }
        }
    }
    out
}

/// Replay the capture the way the meter does: accumulate per TCP stream, then
/// hand whole buffers to the parser.
fn replay_full(lines: &[Line]) -> Arc<DataStorage> {
    let storage = Arc::new(DataStorage::new());
    let mut processor = new_processor(storage.clone());
    let mut streams: HashMap<String, PacketAccumulator> = HashMap::new();

    for line in lines {
        processor.set_override_timestamp(Some(line.at_ms));
        let acc = streams
            .entry(line.key.clone())
            .or_insert_with(PacketAccumulator::new);
        acc.append(&line.bytes);
        let consumed = processor.consume_stream(acc.snapshot());
        if consumed > 0 {
            acc.discard_bytes(consumed);
        }
    }
    storage
}

/// Replay a slice. Each record is already a complete framed packet, so there is
/// nothing to reassemble — which is the point: the server must not have to
/// re-do TCP reassembly, because reassembly ambiguity is both a nondeterminism
/// source and a forgery surface.
fn replay_slice(records: &[(i32, Vec<u8>)], fight_start_ms: i64) -> Arc<DataStorage> {
    let storage = Arc::new(DataStorage::new());
    let mut processor = new_processor(storage.clone());
    for (dt, packet) in records {
        processor.set_override_timestamp(Some(fight_start_ms + *dt as i64));
        processor.consume_stream(packet);
    }
    storage
}

fn capture_path() -> Option<String> {
    match std::env::var("A2_REPLAY_CAPTURE") {
        Ok(p) => Some(p),
        Err(_) => {
            eprintln!("A2_REPLAY_CAPTURE unset — skipping");
            None
        }
    }
}

/// Build a slice covering the whole capture, so the time window is a no-op and
/// what is being measured is the allowlist and the blinder alone.
fn build_whole_capture_slice(
    lines: &[Line],
) -> (evidence_slice::EvidenceSlice, Vec<String>, i64) {
    let full = replay_full(lines);

    // Every name the meter resolved, plus the roster — this is what the share
    // path will pass, and anything missing from it is a name nothing can blind.
    let mut names: NameMap = NameMap::new();
    let roster = full.get_party_members();
    for (name, member) in &roster {
        names.insert(name.clone(), member.dbid);
    }
    for name in full.get_nicknames().values() {
        names.entry(name.clone()).or_insert(0);
    }
    let plaintext: Vec<String> = names.keys().cloned().collect();

    let packets: Vec<CapturedPacket> = lines
        .iter()
        .map(|l| CapturedPacket {
            captured_at_ms: l.at_ms,
            stream: l.key.clone(),
            bytes: l.bytes.clone(),
        })
        .collect();

    let start = lines.first().map(|l| l.at_ms).unwrap_or(0);
    let end = lines.last().map(|l| l.at_ms).unwrap_or(0);

    let slice = evidence_slice::build(&packets, start, end, &names, [7; 32]).expect("slice builds");
    (slice, plaintext, start)
}

#[test]
fn evidence_slice_replays_to_the_same_numbers() {
    let Some(path) = capture_path() else { return };
    let lines = read_capture(&path);
    assert!(!lines.is_empty(), "capture parsed to nothing");

    let full = replay_full(&lines);
    let expected = damage_by_entity(&full);
    assert!(!expected.is_empty(), "the full capture produced no damage");

    let (slice, _, start) = build_whole_capture_slice(&lines);
    let replayed = replay_slice(&slice.records, start);
    let actual = damage_by_entity(&replayed);

    println!(
        "packets {} -> {} kept ({:.1}%), bytes {} -> {} ({:.1}%), {} bundles expanded",
        slice.stats.packets_seen,
        slice.stats.packets_kept,
        100.0 * slice.stats.packets_kept as f64 / slice.stats.packets_seen.max(1) as f64,
        slice.stats.bytes_seen,
        slice.stats.bytes_kept,
        100.0 * slice.stats.bytes_kept as f64 / slice.stats.bytes_seen.max(1) as f64,
        slice.stats.bundles_expanded,
    );
    println!(
        "damage rows: {} expected, {} from the slice",
        expected.len(),
        actual.len()
    );

    // How much of the fight came back, in total and per target.
    let want_total: i64 = expected.values().sum();
    let got_total: i64 = expected
        .keys()
        .map(|k| actual.get(k).copied().unwrap_or(0))
        .sum();
    println!(
        "total damage: {want_total} -> {got_total} ({:.4}%)",
        100.0 * got_total as f64 / want_total.max(1) as f64
    );

    // Per target, so a materially wrong boss cannot hide inside a healthy total.
    let mut per_target: HashMap<i32, (i64, i64)> = HashMap::new();
    for ((target, actor), want) in &expected {
        let got = actual.get(&(*target, *actor)).copied().unwrap_or(0);
        let e = per_target.entry(*target).or_insert((0, 0));
        e.0 += *want;
        e.1 += got;
    }

    // A target worth this much is a boss or a real pull; below it is trash, and
    // a boss log does not rank on trash.
    const MATERIAL: i64 = 1_000_000;
    // The parser finds some damage through an embedded scan that fires on
    // packets no opcode allowlist keeps. It is a rounding error at this scale,
    // and widening the allowlist to chase it would mean uploading packets we
    // cannot name — see the module docs.
    const TOLERANCE: f64 = 0.001; // 0.1%

    let mut failures = Vec::new();
    let mut trash_missing = 0i64;
    for (target, (want, got)) in &per_target {
        if *want < MATERIAL {
            trash_missing += want - got;
            continue;
        }
        let err = (want - got).abs() as f64 / (*want).max(1) as f64;
        println!(
            "  target {target}: {want} -> {got} ({:.4}%)",
            100.0 * *got as f64 / (*want).max(1) as f64
        );
        if err > TOLERANCE {
            failures.push((*target, *want, *got, err));
        }
    }
    println!("damage below the materiality threshold not reproduced: {trash_missing}");

    assert!(
        per_target.values().any(|(w, _)| *w >= MATERIAL),
        "no material target in the capture — this test would assert nothing"
    );
    assert!(
        failures.is_empty(),
        "material targets drifted beyond {:.2}%: {failures:?} — the allowlist is          missing an opcode the parser needs",
        TOLERANCE * 100.0
    );
}

#[test]
fn no_real_name_survives_in_a_slice_built_from_a_real_capture() {
    let Some(path) = capture_path() else { return };
    let lines = read_capture(&path);
    let (slice, plaintext, _) = build_whole_capture_slice(&lines);

    assert!(
        !plaintext.is_empty(),
        "no names were resolved, so this asserts nothing"
    );
    println!("checking {} resolved names against the slice", plaintext.len());

    // `build` already refuses to return a slice containing a known name, so this
    // is a second, independent pass over what would actually be uploaded.
    //
    // Decode and expand rather than scanning the encoded bytes: records are
    // re-compressed bundles, and a plaintext search over compressed data finds
    // nothing regardless of what is in there. Scanning the raw container would
    // be a test that always passes.
    let encoded = evidence_slice::encode(&slice);
    let (records, _) = evidence_slice::decode(&encoded).expect("decodes");
    let expanded: Vec<Vec<u8>> = records
        .iter()
        .map(|(_, r)| evidence_slice::expand(r))
        .collect();
    let expanded_bytes: usize = expanded.iter().map(|e| e.len()).sum();
    assert!(expanded_bytes > 0, "nothing to search — the scan would be vacuous");

    for name in &plaintext {
        let needle = name.as_bytes();
        if needle.len() < 3 {
            continue; // too short to be evidence of anything
        }
        for buf in &expanded {
            assert!(
                !buf.windows(needle.len()).any(|w| w == needle),
                "a resolved character name survived into the slice"
            );
        }
    }

    println!(
        "encoded slice: {} bytes, {} bytes of packet content searched",
        encoded.len(),
        expanded_bytes
    );

    // Set A2_DUMP_SLICE to write the artifact out and measure it compressed.
    if let Ok(dest) = std::env::var("A2_DUMP_SLICE") {
        std::fs::write(&dest, &encoded).expect("write slice");
        println!("wrote {dest}");
    }
}

/// Which opcodes carry damage that the allowlist is dropping?
///
/// Rather than reason about `try_parse_embedded_damage_packet` from the source,
/// feed the parser only the packets the allowlist rejects and see which ones
/// move the damage total. Whatever shows up here belongs on the allowlist.
#[test]
#[ignore = "diagnostic"]
fn which_dropped_opcodes_carry_damage() {
    let Some(path) = capture_path() else { return };
    let lines = read_capture(&path);

    // Expand everything to plain packets, per stream, exactly as the builder does.
    let mut dropped: Vec<Vec<u8>> = Vec::new();
    let mut streams: HashMap<String, PacketAccumulator> = HashMap::new();
    for line in &lines {
        let acc = streams
            .entry(line.key.clone())
            .or_insert_with(PacketAccumulator::new);
        acc.append(&line.bytes);
        let buf = acc.snapshot().to_vec();
        let mut plain = Vec::new();
        let consumed = expand_all(&buf, &mut plain);
        acc.discard_bytes(consumed);
        for p in plain {
            if !allowed(&p) {
                dropped.push(p);
            }
        }
    }
    println!("dropped packets: {}", dropped.len());

    let storage = Arc::new(DataStorage::new());
    let mut processor = new_processor(storage.clone());
    let mut hist: HashMap<[u8; 2], (usize, i64)> = HashMap::new();
    let mut running = 0i64;
    for (i, packet) in dropped.iter().enumerate() {
        processor.set_override_timestamp(Some(1_000_000 + i as i64));
        processor.consume_stream(packet);
        let total: i64 = storage
            .get_combat_snapshot_light()
            .values()
            .map(|t| t.total_damage)
            .sum();
        if total != running {
            let op = leading_opcode(packet).unwrap_or([0, 0]);
            let e = hist.entry(op).or_insert((0, 0));
            e.0 += 1;
            e.1 += total - running;
            running = total;
        }
    }

    let mut rows: Vec<_> = hist.into_iter().collect();
    rows.sort_by_key(|(_, (_, dmg))| std::cmp::Reverse(*dmg));
    println!("opcodes among dropped packets that produced damage:");
    for (op, (count, dmg)) in rows {
        println!("  {:02X} {:02X}  packets={count:<6} damage={dmg}", op[0], op[1]);
    }
}

fn leading_opcode(packet: &[u8]) -> Option<[u8; 2]> {
    let li = a2tools_dps_meter_lib::capture::stream_processor::read_varint(packet, 0);
    if li.length <= 0 {
        return None;
    }
    let o = li.length as usize;
    if o + 1 >= packet.len() {
        return None;
    }
    Some([packet[o], packet[o + 1]])
}

fn allowed(packet: &[u8]) -> bool {
    let Some(op) = leading_opcode(packet) else { return false };
    evidence_slice::ALLOWED_OPCODES
        .iter()
        .any(|(a, _)| **a == op)
}

/// Frame a buffer into plain packets, decompressing bundles. Returns consumed.
fn expand_all(buffer: &[u8], out: &mut Vec<Vec<u8>>) -> usize {
    use a2tools_dps_meter_lib::capture::framing::{self, FrameKind};
    fn inner(buf: &[u8], out: &mut Vec<Vec<u8>>, depth: usize) {
        if depth > 4 {
            return;
        }
        for f in framing::walk_inner(buf).frames {
            match f.kind {
                FrameKind::Packet => out.push(f.bytes(buf).to_vec()),
                FrameKind::Bundle => {
                    if let Some(d) = framing::decompress_bundle(f.payload(buf)) {
                        inner(&d, out, depth + 1);
                    }
                }
            }
        }
    }
    let w = framing::walk(buffer);
    for f in &w.frames {
        match f.kind {
            FrameKind::Packet => out.push(f.bytes(buffer).to_vec()),
            FrameKind::Bundle => {
                if let Some(d) = framing::decompress_bundle(f.payload(buffer)) {
                    inner(&d, out, 1);
                }
            }
        }
    }
    w.consumed
}

/// Nothing readable from the capture may survive into the slice.
///
/// The named-participant check above only covers names the *meter resolved*.
/// That is not the whole risk: a capture also carries bystanders' names, legion
/// names and public chat, none of which the meter ever puts in a map. Running
/// `a2t-inspect --strings` on an early slice found a stranger's name and a
/// recruitment message sitting in the clear, which is what prompted the
/// structural blinding pass.
///
/// So this derives the check from the capture instead of from a list: take every
/// readable run in the original, and assert none of them survived.
#[test]
fn no_readable_text_from_the_capture_survives_into_the_slice() {
    let Some(path) = capture_path() else { return };
    let lines = read_capture(&path);

    // Every length-prefixed string in the original. A name in this protocol is
    // `<u8 len><utf8>`; a run of bytes that merely happens to decode as
    // printable is not a string the game ever wrote, and blinding it would be
    // corrupting binary data rather than protecting anybody.
    let mut original: Vec<String> = Vec::new();
    let mut streams: HashMap<String, PacketAccumulator> = HashMap::new();
    for line in &lines {
        let acc = streams
            .entry(line.key.clone())
            .or_insert_with(PacketAccumulator::new);
        acc.append(&line.bytes);
        let buf = acc.snapshot().to_vec();
        let mut plain = Vec::new();
        let consumed = expand_all(&buf, &mut plain);
        acc.discard_bytes(consumed);
        for p in &plain {
            original.extend(length_prefixed_strings(p));
        }
    }
    original.sort();
    original.dedup();
    assert!(
        original.len() > 20,
        "only {} length-prefixed strings in the capture — this would assert little",
        original.len()
    );

    let (slice, _, _) = build_whole_capture_slice(&lines);
    let content: Vec<u8> = slice
        .records
        .iter()
        .flat_map(|(_, r)| evidence_slice::expand(r))
        .collect();

    let contains = |needle: &str| {
        let n = needle.as_bytes();
        !n.is_empty() && content.windows(n.len()).any(|w| w == n)
    };

    let survivors: Vec<&String> = original.iter().filter(|s| contains(s)).collect();
    println!(
        "{} length-prefixed strings in the capture, {} survived",
        original.len(),
        survivors.len()
    );
    for s in survivors.iter().take(20) {
        println!("  survived: {s:?}");
    }

    // A second, deliberately non-circular check. The strings below are real
    // content `a2t-inspect --strings` found in an early slice: players who were
    // never in the party, and public chat. Naming them here means this test
    // still guards the specific defect even if the structural rule above is
    // later loosened, since it does not depend on that rule's own predicate.
    const OBSERVED_THIRD_PARTY: &[&str] = &[
        "BaroqueWorks",
        "SoloLeveling",
        "TruemansD",
        "Megalobox",
        "1-man army",
        "850K 连刷",
    ];
    let mut anchored = 0;
    for s in OBSERVED_THIRD_PARTY {
        // Only assert on the ones this capture actually contains.
        if !original.iter().any(|o| o.contains(s)) {
            continue;
        }
        anchored += 1;
        assert!(!contains(s), "third-party content survived into the slice");
    }
    println!("{anchored} known third-party strings checked");

    assert!(
        survivors.is_empty(),
        "{} length-prefixed strings from the capture survived into the slice",
        survivors.len()
    );
}

/// Strings as the protocol writes them: `<u8 len><len bytes of text>`.
///
/// Six bytes and up, which is a threshold about the test rather than about the
/// blinder — the blinder handles names from two bytes. Searching half a megabyte
/// of packet content for a two- or three-character string finds it by chance
/// almost every time, so a shorter floor here would report noise as leaks and
/// teach everyone to ignore this test. Short names are covered by `build`'s own
/// verifier, which has no length floor, and by the anchored list above.
const MIN_TESTABLE_NAME: usize = 6;

fn length_prefixed_strings(buf: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < buf.len() {
        let len = buf[i] as usize;
        if !(MIN_TESTABLE_NAME..=40).contains(&len) || i + 1 + len > buf.len() {
            i += 1;
            continue;
        }
        let span = &buf[i + 1..i + 1 + len];
        match std::str::from_utf8(span) {
            Ok(s) if !s.chars().any(|c| c.is_control()) => {
                let alnum = s.chars().filter(|c| c.is_alphanumeric()).count();
                if alnum * 2 >= s.chars().count().max(1) {
                    out.push(s.to_string());
                }
                i += 1 + len;
            }
            _ => i += 1,
        }
    }
    out
}


/// What the server actually does with an upload.
///
/// `rederive::derive` is the entry point the log service calls: it takes the
/// bytes a client sent and returns the numbers the service will publish, having
/// trusted nothing the client computed. This asserts it reproduces the same
/// fight the meter did, and — the part that matters for wasm32 — that it does so
/// without ever reading a clock.
#[test]
fn the_server_derives_the_same_fight_from_an_uploaded_slice() {
    use a2tools_dps_meter_lib::rederive;

    let Some(path) = capture_path() else { return };
    let lines = read_capture(&path);

    let expected = damage_by_entity(&replay_full(&lines));
    let (slice, _, _) = build_whole_capture_slice(&lines);
    let encoded = evidence_slice::encode(&slice);

    let derived = rederive::derive(&encoded).expect("the service derives the fight");
    println!(
        "derived {} targets, {} damage, {} records, parser {}",
        derived.targets.len(),
        derived.total_damage,
        derived.records,
        derived.parser_version
    );

    // Same per-target totals as the meter produced, within the tolerance the
    // slice's opcode allowlist costs (see the acceptance test above).
    const MATERIAL: i64 = 1_000_000;
    let mut per_target: HashMap<i32, i64> = HashMap::new();
    for ((target, _), want) in &expected {
        *per_target.entry(*target).or_default() += *want;
    }
    let mut checked = 0;
    for target in &derived.targets {
        let Some(want) = per_target.get(&target.target_id) else {
            continue;
        };
        if *want < MATERIAL {
            continue;
        }
        checked += 1;
        let err = (want - target.total_damage).abs() as f64 / (*want).max(1) as f64;
        println!(
            "  target {}: meter {want}, service {} ({:.4}%)",
            target.target_id,
            target.total_damage,
            100.0 * target.total_damage as f64 / (*want).max(1) as f64
        );
        assert!(
            err < 0.001,
            "target {} drifted {:.3}% between the meter and the service",
            target.target_id,
            err * 100.0
        );
    }
    assert!(checked >= 1, "no material target was compared");

    // Every participant is a blinded token, never a name.
    let names = {
        let storage = replay_full(&lines);
        storage.get_party_members().into_keys().collect::<Vec<_>>()
    };
    for target in &derived.targets {
        for actor in &target.actors {
            assert!(
                !names.iter().any(|n| n == &actor.token),
                "a real character name reached the derived output"
            );
        }
    }

    // Deriving twice must give the same answer — no clock, no map ordering, no
    // anything that could make one upload publish two different results.
    let again = rederive::derive(&encoded).expect("derives again");
    assert_eq!(derived, again, "re-derivation is not deterministic");
}

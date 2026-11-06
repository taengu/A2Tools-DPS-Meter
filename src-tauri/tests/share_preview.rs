//! End-to-end test of the dry run — the path the "Preview upload" button takes.
//!
//! `share::preview` is the only place a user can inspect what sharing a fight
//! would send, so the thing worth asserting is that it produces both files, that
//! the slice is readable by `a2t-inspect`'s decoder, and that neither artifact
//! carries a character name from the capture it was built from.
//!
//!   A2_REPLAY_CAPTURE=.../packets_20260815_183732.txt cargo test --test share_preview

use a2tools_dps_meter_lib::capture::evidence_slice;
use a2tools_dps_meter_lib::entity::details_context::TargetDetailsResponse;
use a2tools_dps_meter_lib::entity::fight_record::FightRecord;
use a2tools_dps_meter_lib::share;

fn capture_path() -> Option<String> {
    match std::env::var("A2_REPLAY_CAPTURE") {
        Ok(p) => Some(p),
        Err(_) => {
            eprintln!("A2_REPLAY_CAPTURE unset — skipping");
            None
        }
    }
}

/// A record standing in for one the meter would have auto-saved, spanning the
/// capture. The dry run only reads its id, window, and actors.
fn record_spanning(start_ms: i64, duration_ms: i64) -> FightRecord {
    FightRecord {
        id: "auto_test_1".into(),
        boss_name: "Reference Boss".into(),
        target_id: 36448,
        start_time_ms: start_ms,
        duration_ms,
        total_damage: 192_065_363,
        world_boss_observed_damage: None,
        jobs: vec![],
        job_ids: vec![],
        details: TargetDetailsResponse {
            target_id: 36448,
            max_hp: 200_000_000,
            total_target_damage: 192_065_363,
            battle_time: duration_ms,
            start_time: 0,
            skills: Vec::new(),
            ping_history: Vec::new(),
            heal_skills: Vec::new(),
            deaths: None,
        },
        actors: Vec::new(),
        is_train: false,
        app_version: "2.0.22".into(),
        mob_code: 4242,
        dungeon_id: 600093,
        server_id: 0,
        buffs: None,
        stagger: None,
        local_actor_id: None,
        killed: None,
        max_hp_from_spawn: None,
    }
}

#[test]
fn the_dry_run_writes_both_files_and_neither_holds_a_name() {
    let Some(path) = capture_path() else { return };
    let src = std::path::PathBuf::from(&path);

    // The window the capture actually covers.
    let packets = share::read_capture(&src).expect("capture parses");
    assert!(!packets.is_empty(), "capture parsed to nothing");
    let start = packets.first().unwrap().captured_at_ms;
    let end = packets.last().unwrap().captured_at_ms;
    let record = record_spanning(start, end - start);

    let out_dir = std::env::temp_dir().join("a2tools-share-preview-test");
    let _ = std::fs::remove_dir_all(&out_dir);

    let result = share::preview(&record, std::slice::from_ref(&src), &out_dir)
        .expect("the dry run produces a preview");

    println!(
        "slice {} bytes, envelope {} bytes, {} of {} packets kept, {} names blinded",
        result.slice_bytes,
        result.envelope_bytes,
        result.packets_kept,
        result.packets_seen,
        result.names_blinded
    );
    println!("out dir: {}", result.out_dir);

    // Both files exist and are what they claim to be.
    let slice_bytes = std::fs::read(&result.slice_path).expect("slice written");
    let envelope = std::fs::read_to_string(&result.envelope_path).expect("envelope written");
    assert_eq!(slice_bytes.len(), result.slice_bytes);
    assert!(result.packets_kept > 0, "the slice kept nothing");
    assert!(result.sources.contains(&src.display().to_string()));

    // The compressed twin is what an upload would send, so it has to exist and
    // has to decompress to exactly the file next to it.
    let gz_path = std::path::Path::new(&result.slice_path).with_extension("a2es.gz");
    let gz = std::fs::read(&gz_path).expect("compressed slice written");
    assert_eq!(gz.len(), result.slice_compressed_bytes);
    // zstd since slice version 3 (share::compress_slice).
    assert!(gz.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]), "not zstd");
    let inflated = zstd::decode_all(&gz[..]).expect("decompresses");
    assert_eq!(inflated, slice_bytes, "the gz must round-trip to the slice");
    println!(
        "upload would send {} bytes ({:.1}x smaller than {} on disk)",
        gz.len(),
        slice_bytes.len() as f64 / gz.len().max(1) as f64,
        slice_bytes.len()
    );

    let (records, _) = evidence_slice::decode(&slice_bytes).expect("a2t-inspect can read it");
    assert!(!records.is_empty(), "the slice decoded to no records");
    // Records are framed packets and bundles; a bundle holds many kept packets,
    // so there are never more records than packets kept.
    assert!(
        records.len() <= result.packets_kept,
        "{} records from {} kept packets",
        records.len(),
        result.packets_kept
    );

    // The envelope is JSON and declares the slice it belongs to.
    let parsed: serde_json::Value = serde_json::from_str(&envelope).expect("envelope is JSON");
    assert_eq!(
        parsed["evidence"]["bytes"].as_u64(),
        Some(result.slice_bytes as u64)
    );
    assert_eq!(parsed["dungeonId"].as_i64(), Some(600093));

    // Neither artifact may carry a name from the capture. The names come from
    // the capture itself rather than a list, so this keeps working if the
    // fixture changes.
    let names = names_in(&src);
    assert!(names.len() >= 3, "expected the party's names, got {names:?}");
    let content: Vec<u8> = records
        .iter()
        .flat_map(|(_, r)| evidence_slice::expand(r))
        .collect();
    for name in &names {
        assert!(
            !envelope.contains(name.as_str()),
            "the upload envelope carried a character name"
        );
        let needle = name.as_bytes();
        assert!(
            !content.windows(needle.len()).any(|w| w == needle),
            "the evidence slice carried a character name"
        );
    }
    println!("{} character names checked against both files", names.len());

    let _ = std::fs::remove_dir_all(&out_dir);
}

/// Replay the capture far enough to learn who was in it.
fn names_in(path: &std::path::Path) -> Vec<String> {
    use a2tools_dps_meter_lib::capture::packet_accumulator::PacketAccumulator;
    use a2tools_dps_meter_lib::capture::stream_processor::StreamProcessor;
    use a2tools_dps_meter_lib::combat::data_storage::DataStorage;
    use a2tools_dps_meter_lib::i18n::lookup::{NpcLookup, SkillLookup};
    use std::collections::HashMap;
    use std::sync::Arc;

    let storage = Arc::new(DataStorage::new());
    let mut processor = StreamProcessor::new(
        storage.clone(),
        Arc::new(SkillLookup::new()),
        Arc::new(NpcLookup::new()),
    );
    let mut streams: HashMap<String, PacketAccumulator> = HashMap::new();
    for cap in share::read_capture(path).expect("capture") {
        processor.set_override_timestamp(Some(cap.captured_at_ms));
        let acc = streams
            .entry(cap.stream.clone())
            .or_insert_with(PacketAccumulator::new);
        acc.append(&cap.bytes);
        let consumed = processor.consume_stream(acc.snapshot());
        if consumed > 0 {
            acc.discard_bytes(consumed);
        }
    }
    let mut names: Vec<String> = storage.get_party_members().into_keys().collect();
    names.sort();
    names.dedup();
    names
}

#[test]
fn a_fight_no_capture_covers_says_so_rather_than_producing_nothing() {
    let record = record_spanning(1_000, 5_000);
    let out_dir = std::env::temp_dir().join("a2tools-share-preview-empty");
    let err = share::preview(&record, &[], &out_dir).unwrap_err();
    assert!(
        err.contains("Packet logging"),
        "the error should tell the user how to fix it, got: {err}"
    );
    assert!(!out_dir.exists(), "nothing should be written on failure");
}

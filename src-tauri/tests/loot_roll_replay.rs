//! Replays a capture with a Dice Roll in it through the stream processor and
//! checks the roll events plugins get (`capture::loot_rolls`).
//!
//! The capture is the user's own of 2026-11-04 (`packets_20261104_152440.txt`):
//! Artwork: Black Tentacle Lawa rolled for at 17:38:54, T7 winning with 5070.
//! Skips unless A2_LOOT_CAPTURE names it.

use std::collections::HashMap;
use std::sync::Arc;

use a2tools_dps_meter_lib::capture::loot_rolls::LootRoll;
use a2tools_dps_meter_lib::capture::packet_accumulator::PacketAccumulator;
use a2tools_dps_meter_lib::capture::stream_processor::StreamProcessor;
use a2tools_dps_meter_lib::combat::data_storage::DataStorage;
use a2tools_dps_meter_lib::i18n::lookup::{NpcLookup, SkillLookup};
use a2tools_dps_meter_lib::share;

#[test]
fn the_artwork_roll_reaches_plugins() {
    let Ok(path) = std::env::var("A2_LOOT_CAPTURE") else {
        eprintln!("A2_LOOT_CAPTURE unset; skipping");
        return;
    };
    let storage = Arc::new(DataStorage::new());
    // One processor and buffer per TCP stream, as the meter has.
    let mut streams: HashMap<String, (StreamProcessor, PacketAccumulator)> = HashMap::new();
    for cap in share::read_capture(std::path::Path::new(&path)).expect("capture") {
        let (processor, acc) = streams.entry(cap.stream.clone()).or_insert_with(|| {
            let p = StreamProcessor::new(storage.clone(), Arc::new(SkillLookup::new()), Arc::new(NpcLookup::new()));
            (p, PacketAccumulator::new())
        });
        acc.append(&cap.bytes);
        let consumed = processor.consume_stream(acc.snapshot());
        if consumed > 0 {
            acc.discard_bytes(consumed);
        }
    }

    let (_, events) = storage.loot_rolls_since(0).expect("roll events");
    let rolls: Vec<(String, u32)> = events
        .iter()
        .filter_map(|e| match &e.roll {
            LootRoll::Roll { item_id: 514310074, player, roll } => Some((player.name.clone(), *roll)),
            _ => None,
        })
        .collect();
    assert_eq!(
        rolls,
        [("HeavenDorian", 2846), ("Codix", 1944), ("T7", 5070), ("Exodus", 3985), ("Kayamo", 3278)]
            .map(|(n, r)| (n.to_string(), r))
    );
    assert!(events.iter().any(|e| matches!(&e.roll, LootRoll::Mine { roll: 2003, .. })), "the player's own 2003");
    assert!(
        events.iter().any(|e| matches!(&e.roll, LootRoll::Won { item_id: 514310074, player, .. } if player.name == "T7")),
        "T7 won it"
    );
    // It opened for 35 s (17:38:48 to 17:39:23, +08:00).
    assert!(
        matches!(&events[0].roll, LootRoll::Open { roll_id, item_id: 514310074, count: 1, ends_at: 1793785163033 } if roll_id == "a609050000010000"),
        "the roll opened first: {:?}",
        events[0]
    );
    assert_eq!(events.len(), 8, "nothing else read as a roll: {events:?}");
}

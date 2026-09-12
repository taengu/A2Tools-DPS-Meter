//! The `41 36` spawn mask widened from u16 to u32.
//!
//! That move relocates the subtree byte gating the inline name, and for a summon
//! that name is its *owner's* character name — the fallback that attributes a
//! pet's damage to the player when the record carries no parent_key. Reading it
//! from the wrong offset does not error; it silently finds no name, and the
//! symptom is pets drifting back into rows of their own.
//!
//! The reference capture predates the change, so replaying it can only show the
//! old format still works. These build both layouts by hand, which is the only
//! way to prove the new one parses before a post-patch capture exists.

use std::sync::Arc;

use a2tools_dps_meter_lib::capture::stream_processor::StreamProcessor;
use a2tools_dps_meter_lib::combat::data_storage::DataStorage;
use a2tools_dps_meter_lib::i18n::lookup::{NpcLookup, SkillLookup};

/// `<len varint> 41 36 <entity_id u8 varint> <mask> <subtree gate> <len><name>`
///
/// `mask_bytes` is what changed: two bytes before the patch, four after. The low
/// byte is the entity kind, and 0x5F is a summon — the only kind that takes the
/// name-based owner link.
fn spawn_packet(entity_id: u8, mask_bytes: &[u8], owner_name: &str) -> Vec<u8> {
    let mut body = vec![0x41, 0x36, entity_id];
    body.extend_from_slice(mask_bytes);
    body.push(0x01); // subtree gate: bit 0 set == an inline name follows
    body.push(owner_name.len() as u8);
    body.extend_from_slice(owner_name.as_bytes());
    // Tail padding so the parent_key scan has somewhere to run and find nothing.
    body.extend_from_slice(&[0x00; 24]);

    // The framing quirk: the declared length is the physical size plus three.
    let total = body.len() + 1;
    let mut packet = vec![(total + 3) as u8];
    packet.extend_from_slice(&body);
    packet
}

fn feed(packet: &[u8], owner_id: i32, owner_name: &str) -> Arc<DataStorage> {
    let storage = Arc::new(DataStorage::new());
    // The name link resolves through the nickname table, so the owner has to be
    // known before the spawn arrives — which is the real ordering too.
    storage.append_nickname_authoritative(owner_id, owner_name);

    let mut processor = StreamProcessor::new(
        storage.clone(),
        Arc::new(SkillLookup::new()),
        Arc::new(NpcLookup::new()),
    );
    processor.set_override_timestamp(Some(1_000));
    processor.consume_stream(packet);
    storage
}

// Must be under 128: a single-byte varint only reaches 0x7F, and 200 (0xC8) has
// the continuation bit set, so the parser would read it as the first byte of a
// longer varint and everything after would shift.
const SUMMON_ID: u8 = 120;
const OWNER_ID: i32 = 4099;
const OWNER: &str = "Misti";

#[test]
fn a_u32_mask_spawn_links_the_summon_to_its_owner() {
    // kind 0x5F (summon), bit 4 clear so it takes the name path rather than
    // hunting a parent_key that a synthetic packet does not have.
    let packet = spawn_packet(SUMMON_ID, &[0x5F, 0x00, 0x00, 0x00], OWNER);
    let storage = feed(&packet, OWNER_ID, OWNER);

    assert_eq!(
        storage.get_summon_data().get(&(SUMMON_ID as i32)),
        Some(&OWNER_ID),
        "a u32-mask spawn did not link its summon to the owner named in it"
    );
}

#[test]
fn a_u16_mask_spawn_still_links_after_the_change() {
    // Pre-patch captures and replays have to keep working.
    let packet = spawn_packet(SUMMON_ID, &[0x5F, 0x00], OWNER);
    let storage = feed(&packet, OWNER_ID, OWNER);

    assert_eq!(
        storage.get_summon_data().get(&(SUMMON_ID as i32)),
        Some(&OWNER_ID),
        "the old u16-mask layout stopped resolving"
    );
}

#[test]
fn a_spawn_naming_nobody_links_nothing() {
    // The guard that makes trying two offsets safe: a wrong guess lands on bytes
    // that do not decode as a name, and must produce no link rather than a
    // fabricated one.
    let packet = spawn_packet(SUMMON_ID, &[0x5F, 0x00, 0x00, 0x00], "Stranger");
    let storage = feed(&packet, OWNER_ID, OWNER);

    assert!(
        storage.get_summon_data().get(&(SUMMON_ID as i32)).is_none(),
        "a name nobody claims must not produce an owner link"
    );
}

//! Preparing a fight for sharing — and, for now, only ever writing it to disk.
//!
//! Nothing here opens a socket. The dry run exists so that "we do not upload
//! your character names" is a claim you can check rather than one you have to
//! believe: it writes the exact two files an upload would send, into a folder it
//! then opens for you, and `a2t-inspect` reads them back.
//!
//! The artifacts are deliberately separate:
//!
//! - `<fight>.a2es` — the Evidence Slice, the packets themselves. See
//!   `capture::evidence_slice`.
//! - `<fight>.a2es.gz` — the same thing, gzipped: byte for byte what an upload
//!   would put on the wire. Both are written because the compressed one is what
//!   gets sent and the uncompressed one is what is easy to check, and making
//!   people choose between those would defeat the point. (`a2t-inspect` reads
//!   either.)
//! - `<fight>.upload.json` — the derived summary. **No field of it holds a
//!   character name**, not even your own; participants are identified by
//!   `sha256(dbid)`, and the server learns who that is only if you register the
//!   character yourself. There is a test asserting no name from the fight
//!   appears anywhere in the JSON.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Weak};

use flate2::Compression;
use flate2::write::GzEncoder;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::capture::evidence_slice::{self, CapturedPacket, NameMap};
use crate::capture::packet_accumulator::PacketAccumulator;
use crate::capture::stream_processor::StreamProcessor;
use crate::combat::data_storage::DataStorage;
use crate::entity::fight_record::FightRecord;
use crate::i18n::lookup::{NpcLookup, SkillLookup};

/// How many damage values go into each anchor window, and how far the windows
/// step. Overlapping on purpose: a single missed large hit — range culling, a
/// dropped packet, a late join — then breaks one anchor instead of all four, so
/// two people who fought the same boss still match.
const ANCHOR_WINDOW: usize = 16;
const ANCHOR_STEP: usize = 8;
const ANCHOR_COUNT: usize = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Participant {
    pub slot: u32,
    pub job_id: i32,
    pub is_uploader: bool,
    /// `sha256(dbid)` — present only when the party roster named this actor.
    /// Absent for summons and for anyone who never appeared in a roster packet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<u16>,
    pub damage: i64,
    pub dps: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    pub present: bool,
    pub sha256: String,
    pub bytes: usize,
}

/// What an upload would send, minus the evidence blob itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadEnvelope {
    pub app_version: String,
    pub parser_version: String,
    pub client_start_ms: i64,
    pub duration_ms: i64,
    pub mob_code: i32,
    pub dungeon_id: i32,
    pub is_train: bool,
    pub total_damage: i64,
    pub boss_max_hp: i32,
    pub participants: Vec<Participant>,
    /// Fingerprints of the damage this fight produced, used to recognise that
    /// two uploads are the same encounter. Derived from server-computed damage
    /// values, which every observer of the fight sees identically.
    pub anchors: Vec<String>,
    pub evidence: Evidence,
}

/// What the dry run produced, for the UI to show.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewResult {
    pub out_dir: String,
    pub slice_path: String,
    pub envelope_path: String,
    pub slice_bytes: usize,
    /// What an upload would actually send — the slice gzipped.
    pub slice_compressed_bytes: usize,
    pub envelope_bytes: usize,
    pub packets_seen: usize,
    pub packets_kept: usize,
    pub bytes_seen: usize,
    pub bytes_kept: usize,
    pub names_blinded: usize,
    pub participants: usize,
    /// Captures that were read to build this.
    pub sources: Vec<String>,
}

/// Parse a `packets_*.txt` capture into the buffers the slice builder wants.
///
/// Whole file, not just the fight window: the builder reassembles TCP streams,
/// and starting halfway through one means framing from the middle of a packet.
pub fn read_capture(path: &Path) -> Result<Vec<CapturedPacket>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(3, '|');
        let (Some(ts), Some(key), Some(hex)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let Ok(when) = chrono::DateTime::parse_from_rfc3339(ts.trim()) else {
            continue;
        };
        let Some(bytes) = decode_hex(hex) else {
            continue;
        };
        out.push(CapturedPacket {
            captured_at_ms: when.timestamp_millis(),
            stream: key.to_string(),
            bytes,
        });
    }
    Ok(out)
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    let b = hex.as_bytes();
    if b.len() % 2 != 0 || b.is_empty() {
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

/// Gzip a buffer.
///
/// Worth doing even though a slice already holds LZ4-compressed bundles: the
/// framing, the repeated opcodes and the hex-free binary still compress about
/// 2.2x, measured on the reference run (467 KB -> 212 KB). For a diagnostic
/// capture, which is ASCII hex, it is closer to 3x — the difference between an
/// 8 MB upload and a 2.8 MB one, and the reason a size limit can be generous.
pub fn gzip(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut encoder = GzEncoder::new(Vec::with_capacity(data.len() / 2), Compression::best());
    encoder.write_all(data).map_err(|e| e.to_string())?;
    encoder.finish().map_err(|e| e.to_string())
}

/// Every capture the packet logger has written, newest first.
pub fn find_captures(app_data_dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = match std::fs::read_dir(app_data_dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("packets_") && n.ends_with(".txt"))
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort();
    out.reverse();
    out
}

/// Does this capture cover the fight?
fn covers(packets: &[CapturedPacket], start_ms: i64, end_ms: i64) -> bool {
    let (Some(first), Some(last)) = (packets.first(), packets.last()) else {
        return false;
    };
    first.captured_at_ms <= end_ms && last.captured_at_ms >= start_ms
}

/// Replay a capture to learn the character names in it.
///
/// The saved fight record cannot supply these: `obscure_nickname` masks every
/// party member before the record is written, so it holds `Ta****x`, and the
/// blinder needs the real bytes to find and replace them.
fn resolve_names(packets: &[CapturedPacket]) -> NameMap {
    let storage = std::sync::Arc::new(DataStorage::new());
    let mut processor = StreamProcessor::new(
        storage.clone(),
        std::sync::Arc::new(SkillLookup::new()),
        std::sync::Arc::new(NpcLookup::new()),
    );
    let mut streams: HashMap<&str, PacketAccumulator> = HashMap::new();
    for cap in packets {
        processor.set_override_timestamp(Some(cap.captured_at_ms));
        let acc = streams
            .entry(cap.stream.as_str())
            .or_insert_with(PacketAccumulator::new);
        acc.append(&cap.bytes);
        let consumed = processor.consume_stream(acc.snapshot());
        if consumed > 0 {
            acc.discard_bytes(consumed);
        }
    }
    processor.set_override_timestamp(None);

    let mut names = NameMap::new();
    for (name, member) in storage.get_party_members() {
        names.insert(name, member.dbid);
    }
    for name in storage.get_nicknames().into_values() {
        names.entry(name).or_insert(0);
    }
    names
}

/// `sha256(dbid)`, the id an upload carries instead of a name.
pub fn account_ref(dbid: u64) -> String {
    let digest = Sha256::digest(dbid.to_le_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Fingerprint the fight by the damage values it produced.
///
/// Distinct values, largest first: server-computed five- and six-figure integers
/// carry enough entropy that two parties killing the same boss at the same
/// moment still do not collide, while two *observers of one fight* agree
/// exactly.
fn anchors(record: &FightRecord) -> Vec<String> {
    let mut values: Vec<i64> = record
        .details
        .skills
        .iter()
        .flat_map(|s| [s.dmg as i64, s.max_dmg as i64])
        .filter(|&d| d > 0)
        .collect();
    values.sort_unstable_by(|a, b| b.cmp(a));
    values.dedup();

    let mut out = Vec::new();
    for w in 0..ANCHOR_COUNT {
        let from = w * ANCHOR_STEP;
        let to = (from + ANCHOR_WINDOW).min(values.len());
        if from >= to {
            break;
        }
        let mut hasher = Sha256::new();
        hasher.update(b"a2-anchor\x00");
        hasher.update((record.mob_code as u32).to_le_bytes());
        for v in &values[from..to] {
            hasher.update(v.to_le_bytes());
        }
        let digest = hasher.finalize();
        out.push(digest[..8].iter().map(|b| format!("{b:02x}")).collect());
    }
    out
}

/// Build the summary an upload would carry. Contains no character name.
pub fn build_envelope(record: &FightRecord, slice: &[u8]) -> UploadEnvelope {
    // Damage per actor, from the per-skill breakdown.
    let mut damage: HashMap<i32, i64> = HashMap::new();
    for skill in &record.details.skills {
        *damage.entry(skill.actor_id).or_default() += skill.dmg as i64;
    }

    let seconds = (record.duration_ms as f64 / 1000.0).max(0.001);
    let mut participants: Vec<Participant> = record
        .actors
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let dmg = damage.get(&a.actor_id).copied().unwrap_or(0);
            Participant {
                slot: i as u32 + 1,
                job_id: a.job_id,
                // The saved record does not mark the uploader, but it is the one
                // actor whose name was never masked — and we must not put that
                // name here to say so, hence the flag rather than the name.
                is_uploader: false,
                account_ref: (a.dbid != 0).then(|| account_ref(a.dbid)),
                server_id: (a.server_id != 0).then_some(a.server_id),
                damage: dmg,
                dps: dmg as f64 / seconds,
            }
        })
        .collect();
    participants.sort_by(|a, b| b.damage.cmp(&a.damage));
    for (i, p) in participants.iter_mut().enumerate() {
        p.slot = i as u32 + 1;
    }

    let digest = Sha256::digest(slice);
    UploadEnvelope {
        app_version: record.app_version.clone(),
        // Until the parser is built with its own git sha, the app version is the
        // best available statement of which code derived these numbers.
        parser_version: crate::entity::fight_record::APP_VERSION.to_string(),
        client_start_ms: record.start_time_ms,
        duration_ms: record.duration_ms,
        mob_code: record.mob_code,
        dungeon_id: record.dungeon_id,
        is_train: record.is_train,
        total_damage: record.total_damage as i64,
        boss_max_hp: record.details.max_hp,
        participants,
        anchors: anchors(record),
        evidence: Evidence {
            present: !slice.is_empty(),
            sha256: digest.iter().map(|b| format!("{b:02x}")).collect(),
            bytes: slice.len(),
        },
    }
}

/// Write the two files an upload would send, and return what was written.
///
/// Never touches the network. That is the whole point of it.
/// Cut the Evidence Slice for a saved fight from whichever captures cover it.
///
/// Returns the encoded slice and what building it kept and dropped. This is
/// the one place a slice is made from a capture; the preview writes it to
/// disk, an upload sends it.
pub fn slice_for(
    record: &FightRecord,
    captures: &[PathBuf],
) -> Result<(Vec<u8>, evidence_slice::EvidenceSlice, Vec<String>), String> {
    let start = record.start_time_ms;
    let end = record.start_time_ms + record.duration_ms;

    let mut packets: Vec<CapturedPacket> = Vec::new();
    let mut sources = Vec::new();
    for path in captures {
        let parsed = read_capture(path)?;
        if !covers(&parsed, start - evidence_slice::LEAD_IN_MS, end + evidence_slice::TAIL_MS) {
            continue;
        }
        sources.push(path.display().to_string());
        packets.extend(parsed);
    }
    if packets.is_empty() {
        return Err(
            "No packet capture covers this fight. Packet logging is off by default —              turn it on in Settings → Diagnostics and fight again, then preview."
                .into(),
        );
    }
    packets.sort_by_key(|p| p.captured_at_ms);

    let names = resolve_names(&packets);
    let mut slice = evidence_slice::build(&packets, start, end, &names).map_err(|e| e.to_string())?;
    without_roster_ids(&mut slice);
    let encoded = evidence_slice::encode(&slice);
    Ok((encoded, slice, sources))
}

/// Blank the roster ids in a slice's blind table before it is written or sent.
///
/// The builder records which roster id each blinded name belonged to, so a
/// service could join uploads to accounts. Nothing does, and a roster id is a
/// stable handle on a person: every party member's would leave the machine
/// with every upload, for no purpose. The tokens stay (the parser needs names
/// to be distinct), the ids do not.
fn without_roster_ids(slice: &mut evidence_slice::EvidenceSlice) {
    for id in slice.blind_map.values_mut() {
        *id = 0;
    }
}

pub fn preview(
    record: &FightRecord,
    captures: &[PathBuf],
    out_dir: &Path,
) -> Result<PreviewResult, String> {
    let (encoded, slice, sources) = slice_for(record, captures)?;
    let envelope = build_envelope(record, &encoded);
    let json = serde_json::to_string_pretty(&envelope).map_err(|e| e.to_string())?;

    let compressed = gzip(&encoded)?;

    std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    // Both: the `.a2es` is what `a2t-inspect` reads, the `.gz` is byte for byte
    // what an upload would put on the wire. Writing only the compressed one
    // would make the artifact harder to check, which is the opposite of why
    // this exists.
    let slice_path = out_dir.join(format!("{}.a2es", record.id));
    let compressed_path = out_dir.join(format!("{}.a2es.gz", record.id));
    let envelope_path = out_dir.join(format!("{}.upload.json", record.id));
    std::fs::write(&slice_path, &encoded).map_err(|e| e.to_string())?;
    std::fs::write(&compressed_path, &compressed).map_err(|e| e.to_string())?;
    std::fs::write(&envelope_path, json.as_bytes()).map_err(|e| e.to_string())?;

    Ok(PreviewResult {
        out_dir: out_dir.display().to_string(),
        slice_path: slice_path.display().to_string(),
        envelope_path: envelope_path.display().to_string(),
        slice_bytes: encoded.len(),
        slice_compressed_bytes: compressed.len(),
        envelope_bytes: json.len(),
        packets_seen: slice.stats.packets_seen,
        packets_kept: slice.stats.packets_kept,
        bytes_seen: slice.stats.bytes_seen,
        bytes_kept: slice.stats.bytes_kept,
        names_blinded: slice.stats.names_blinded,
        participants: envelope.participants.len(),
        sources,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::details_context::{DetailsActorSummary, TargetDetailsResponse};

    fn record_with(actors: Vec<DetailsActorSummary>) -> FightRecord {
        FightRecord {
            id: "auto_1_2".into(),
            boss_name: "Some Boss".into(),
            target_id: 1,
            start_time_ms: 1_700_000_000_000,
            duration_ms: 60_000,
            total_damage: 1_000,
            jobs: vec!["Sorcerer".into()],
            job_ids: vec![15],
            details: TargetDetailsResponse {
                target_id: 1,
                max_hp: 5_000,
                total_target_damage: 1_000,
                battle_time: 60_000,
                start_time: 0,
                skills: Vec::new(),
                ping_history: Vec::new(),
                heal_skills: Vec::new(),
            },
            actors,
            is_train: false,
            app_version: "2.0.22".into(),
            mob_code: 4242,
            dungeon_id: 600093,
            server_id: 0,
            buffs: None,
        }
    }

    fn actor(id: i32, nickname: &str, dbid: u64) -> DetailsActorSummary {
        DetailsActorSummary {
            actor_id: id,
            nickname: nickname.into(),
            job: "Sorcerer".into(),
            job_id: 15,
            party_heal: 0,
            regen: 0,
            damage_received: 0,
            hits_received: 0,
            dbid,
            server_id: (dbid >> 48) as u16,
            is_supporter: false,
            level: 0,
            gear_score: 0,
            combat_power: 0,
        }
    }

    #[test]
    fn the_envelope_contains_no_character_name() {
        let record = record_with(vec![
            actor(1, "Misti", 0x07de_0000_0000_1fee),
            actor(2, "Gr****e", 0x03f5_0000_0001_b9c0),
            actor(3, "九州依然在", 0x03f6_0000_0001_4b85),
        ]);
        let json = serde_json::to_string(&build_envelope(&record, b"slice")).unwrap();
        for name in ["Misti", "Gr****e", "九州依然在"] {
            assert!(
                !json.contains(name),
                "the upload envelope carried a character name: {name}"
            );
        }
        // And it does carry the id that replaces it.
        assert!(json.contains(&account_ref(0x07de_0000_0000_1fee)));
    }

    #[test]
    fn an_actor_with_no_roster_entry_gets_no_account_ref() {
        let record = record_with(vec![actor(9, "Summon", 0)]);
        let envelope = build_envelope(&record, b"");
        assert_eq!(envelope.participants.len(), 1);
        assert!(envelope.participants[0].account_ref.is_none());
        assert!(envelope.participants[0].server_id.is_none());
    }

    #[test]
    fn account_ref_is_stable_and_differs_per_id() {
        assert_eq!(account_ref(7), account_ref(7));
        assert_ne!(account_ref(7), account_ref(8));
        assert_eq!(account_ref(7).len(), 64);
    }

    #[test]
    fn the_envelope_records_the_difficulty_tier() {
        let record = record_with(vec![actor(1, "A", 1)]);
        let envelope = build_envelope(&record, b"");
        // 600093 is Conquest [Hard]; ranking it against 600091 would be wrong.
        assert_eq!(envelope.dungeon_id, 600093);
        assert_eq!(envelope.mob_code, 4242);
    }
}

// ===== slices kept automatically, and uploading them =====

#[cfg(feature = "online")]
pub mod dev_logs;
pub mod ring;

/// Where a fight's slice and its upload state live. Beside `history/`, not in
/// it: that directory is scanned as fight records.
pub fn slices_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("slices")
}

fn slice_path(app_data_dir: &Path, id: &str) -> PathBuf {
    slices_dir(app_data_dir).join(format!("{id}.a2es.gz"))
}

fn meta_path(app_data_dir: &Path, id: &str) -> PathBuf {
    slices_dir(app_data_dir).join(format!("{id}.json"))
}

/// What the meter remembers about one fight's slice.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SliceMeta {
    /// The local player's entity id in this fight: the one name an upload
    /// shows in full. `None` when the meter never identified the player.
    #[serde(default)]
    pub uploader_actor_id: Option<i32>,
    /// Set once uploaded.
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    /// Automatic uploads tried and failed so far. See `note_auto_upload_failure`.
    #[serde(default)]
    pub auto_attempts: u32,
    /// When to try the next automatic upload (ms since the epoch).
    #[serde(default)]
    pub retry_at_ms: i64,
    /// Automatic uploads have stopped for this fight: the failure was one a
    /// retry cannot fix, or the retries ran out. The History button still works.
    #[serde(default)]
    pub gave_up: bool,
}

fn read_meta(app_data_dir: &Path, id: &str) -> SliceMeta {
    std::fs::read_to_string(meta_path(app_data_dir, id))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_meta(app_data_dir: &Path, id: &str, meta: &SliceMeta) {
    if let Ok(json) = serde_json::to_string(meta) {
        let _ = crate::atomic_file::write(&meta_path(app_data_dir, id), json.as_bytes());
    }
}

// Serialize a metadata read/modify/write for its own file, without making
// unrelated fights or settings wait, and never across a network request.
static META_LOCKS: LazyLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn update_meta(app_data_dir: &Path, id: &str, update: impl FnOnce(&mut SliceMeta)) {
    let lock = {
        let mut locks = META_LOCKS.lock();
        locks.retain(|_, lock| lock.strong_count() > 0);
        let entry = locks.entry(meta_path(app_data_dir, id)).or_default();
        match entry.upgrade() {
            Some(lock) => lock,
            None => {
                let lock = Arc::new(Mutex::new(()));
                *entry = Arc::downgrade(&lock);
                lock
            }
        }
    };
    let _updating = lock.lock();
    let mut meta = read_meta(app_data_dir, id);
    update(&mut meta);
    write_meta(app_data_dir, id, &meta);
}

/// Every name the meter has resolved, which is what the blinder must remove.
pub fn names_from(storage: &DataStorage) -> NameMap {
    let mut names = NameMap::new();
    for (name, member) in storage.get_party_members() {
        names.insert(name.clone(), member.dbid);
    }
    for name in storage.get_nicknames().values() {
        names.entry(name.clone()).or_insert(0);
    }
    names
}

/// Cut and keep the slice for a fight the meter just saved, from memory.
///
/// Called by the auto-save each time it writes the record, so the slice grows
/// with the fight and the last write is the whole of it. Returns the gzipped
/// size. Failing is normal and quiet: a fight that began before the meter
/// started has no packets behind it.
pub fn save_slice(
    app_data_dir: &Path,
    record: &FightRecord,
    storage: &DataStorage,
) -> Result<usize, String> {
    let packets = ring::snapshot();
    let start = record.start_time_ms;
    if !covers(&packets, start, start + record.duration_ms) {
        return Err("no packets in memory for this fight".into());
    }
    let mut slice = evidence_slice::build(&packets, start, start + record.duration_ms, &names_from(storage))
        .map_err(|e| e.to_string())?;
    without_roster_ids(&mut slice);
    let compressed = gzip(&evidence_slice::encode(&slice))?;

    let dir = slices_dir(app_data_dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    crate::atomic_file::write(&slice_path(app_data_dir, &record.id), &compressed).map_err(|e| e.to_string())?;
    let uploader = uploader_in(record, storage.local_player_id(), storage.local_character_name());
    update_meta(app_data_dir, &record.id, |meta| meta.uploader_actor_id = uploader);
    Ok(compressed.len())
}

/// Who uploads `record`: an actor in the fight, never just the current id.
/// The local id can have moved on since (a zone change gives everyone new
/// ids), and was once a party placeholder row: two uploads named an uploader
/// who was not in the fight at all (issue #19). The local id when it is in
/// the fight, else the actor carrying the local character's name, else none.
fn uploader_in(record: &FightRecord, local_id: Option<i64>, local_name: Option<String>) -> Option<i32> {
    let local_id = local_id.map(|v| v as i32);
    if let Some(id) = local_id.filter(|id| record.actors.iter().any(|a| a.actor_id == *id)) {
        return Some(id);
    }
    let name = local_name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty())?;
    record.actors.iter().find(|a| a.nickname.trim() == name).map(|a| a.actor_id)
}

/// Remove a fight's slice along with the fight.
pub fn forget_slice(app_data_dir: &Path, id: &str) {
    let _ = std::fs::remove_file(slice_path(app_data_dir, id));
    let _ = std::fs::remove_file(meta_path(app_data_dir, id));
}

/// Drop slices whose fight is gone (history is pruned to a fixed count).
pub fn prune_slices(app_data_dir: &Path) {
    let Ok(rd) = std::fs::read_dir(slices_dir(app_data_dir)) else { return };
    for entry in rd.filter_map(|e| e.ok()) {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) { continue; }
        let name = entry.file_name().to_string_lossy().to_string();
        // Only completed files belong to pruning. A legacy or pid/counter
        // temporary may still be in use, including by another meter process.
        let Some(id) = name.strip_suffix(".a2es.gz").or_else(|| name.strip_suffix(".json")) else { continue };
        if !app_data_dir.join("history").join(format!("{id}.json")).exists() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(feature = "online")]
/// The setting that turns automatic uploads on. Off unless the player turns
/// it on: an upload publishes a fight, and that is theirs to decide.
pub const AUTO_UPLOAD_KEY: &str = "dpsMeter.autoUpload";

#[cfg(feature = "online")]
/// How long after the last hit a fight counts as over. The same rule the
/// snapshot uses to stop re-saving a boss.
const ENDED_AFTER_MS: i64 = 10_000;

#[cfg(feature = "online")]
/// Should the auto-save upload this fight now?
///
/// Only a finished fight, once, with its packets behind it. A boss still being
/// fought is re-saved every 30 seconds, and uploading those partial records
/// would publish a fight that has not happened yet.
pub fn wants_auto_upload(app_data_dir: &Path, record: &FightRecord, now_ms: i64) -> bool {
    let meta = read_meta(app_data_dir, &record.id);
    !record.is_train
        && now_ms - (record.start_time_ms + record.duration_ms) >= ENDED_AFTER_MS
        && slice_path(app_data_dir, &record.id).exists()
        && meta.url.is_none()
        // A fight that already failed once is the retry schedule's.
        && meta.auto_attempts == 0
}

#[cfg(feature = "online")]
/// How long to wait before each retry of a failed automatic upload, in
/// minutes; one more failure after the last and it stops.
const AUTO_RETRY_MINUTES: [i64; 6] = [1, 2, 5, 15, 30, 60];

#[cfg(feature = "online")]
/// How long to wait between tries while the keyring is locked.
const KEYRING_RETRY_MINUTES: i64 = 5;

#[cfg(feature = "online")]
/// An automatic upload of `id` failed. Schedule the next try, or stop: when
/// the failure is one waiting cannot fix (not signed in, a refused fight),
/// or the retries are used up. A locked keyring uses up no retries.
pub fn note_auto_upload_failure(app_data_dir: &Path, id: &str, failure: &UploadFailure, now_ms: i64) {
    update_meta(app_data_dir, id, |meta| {
        if failure.keyring_locked {
            meta.auto_attempts = meta.auto_attempts.max(1);
            meta.retry_at_ms = now_ms + KEYRING_RETRY_MINUTES * 60_000;
            return;
        }
        meta.auto_attempts += 1;
        match AUTO_RETRY_MINUTES.get(meta.auto_attempts as usize - 1) {
            Some(minutes) if failure.retryable => meta.retry_at_ms = now_ms + minutes * 60_000,
            _ => meta.gave_up = true,
        }
    });
}

#[cfg(feature = "online")]
/// Fights whose automatic upload failed and is due to be tried again.
pub fn auto_upload_retries_due(app_data_dir: &Path, now_ms: i64) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(slices_dir(app_data_dir)) else { return Vec::new() };
    rd.filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".json").map(str::to_string))
        .filter(|id| {
            let meta = read_meta(app_data_dir, id);
            meta.url.is_none()
                && meta.auto_attempts > 0
                && !meta.gave_up
                && meta.retry_at_ms <= now_ms
                && slice_path(app_data_dir, id).exists()
        })
        .collect()
}

/// Which saved fights can be uploaded, and which already were.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareStatus {
    pub has_slice: bool,
    pub url: Option<String>,
}

pub fn share_status(app_data_dir: &Path) -> HashMap<String, ShareStatus> {
    let mut out: HashMap<String, ShareStatus> = HashMap::new();
    let Ok(rd) = std::fs::read_dir(slices_dir(app_data_dir)) else { return out };
    for entry in rd.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(id) = name.strip_suffix(".a2es.gz") {
            out.entry(id.to_string())
                .or_insert(ShareStatus { has_slice: false, url: None })
                .has_slice = true;
        } else if let Some(id) = name.strip_suffix(".json") {
            let url = read_meta(app_data_dir, id).url;
            out.entry(id.to_string())
                .or_insert(ShareStatus { has_slice: false, url: None })
                .url = url;
        }
    }
    out
}

#[cfg(feature = "online")]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadResult {
    pub url: String,
    #[serde(default)]
    pub visibility: String,
    #[serde(default)]
    pub duplicate: bool,
}

/// The meter's current display language (`ko`, `en`, …).
///
/// Sent with an upload because a server id cannot tell Korea from Taiwan:
/// both number their servers 1001–1058 and 2001–2058. The language and the
/// computer's time zone are what the site has to go on; a player on Korean
/// servers almost always has one or the other Korean.
#[cfg_attr(not(feature = "online"), allow(dead_code))]
pub(crate) fn ui_language(settings: &crate::config::settings::Settings) -> String {
    // Settings writes are queued. Uploads must observe an accepted change even
    // when its disk write is still pending or failed.
    settings.get("dpsMeter.language").unwrap_or_default()
}

#[cfg(feature = "online")]
/// Upload a saved fight as a log.
///
/// Sends the slice and the names to show, never a number: the service derives
/// the fight from the slice with this same parser. The names are the ones the
/// saved record already holds, which the meter masked for everyone but the
/// local player when it wrote them; the service masks them again regardless.
pub async fn upload(
    client: &reqwest::Client,
    app_data_dir: &Path,
    record: &FightRecord,
    settings: &crate::config::settings::Settings,
) -> Result<UploadResult, String> {
    upload_detailed(client, app_data_dir, record, settings).await.map_err(|f| f.message)
}

#[cfg(feature = "online")]
/// Why an upload failed, and whether trying the same upload later could work.
#[derive(Debug, Clone)]
pub struct UploadFailure {
    pub message: String,
    /// Offline, a server error, or rate limited: worth another try later. Not
    /// signed in, or the service refused the fight: the same again would fail.
    pub retryable: bool,
    /// The keyring did not hand over the token. Retried for as long as it takes.
    pub keyring_locked: bool,
}

#[cfg(feature = "online")]
impl UploadFailure {
    fn retry(message: impl Into<String>) -> Self {
        Self { message: message.into(), retryable: true, keyring_locked: false }
    }
    fn fatal(message: impl Into<String>) -> Self {
        Self { message: message.into(), retryable: false, keyring_locked: false }
    }
    fn locked(message: impl Into<String>) -> Self {
        Self { message: message.into(), retryable: true, keyring_locked: true }
    }
}

#[cfg(feature = "online")]
/// `upload`, saying whether a failure is worth retrying.
pub async fn upload_detailed(
    client: &reqwest::Client,
    app_data_dir: &Path,
    record: &FightRecord,
    settings: &crate::config::settings::Settings,
) -> Result<UploadResult, UploadFailure> {
    let token = match crate::account::secret::load_stored(app_data_dir) {
        crate::account::secret::Stored::Token(token) => token,
        crate::account::secret::Stored::Locked => {
            return Err(UploadFailure::locked(
                "The desktop keyring is locked. Unlock the keyring to upload fights.",
            ))
        }
        crate::account::secret::Stored::Missing => {
            return Err(UploadFailure::fatal("Sign in under Settings → A2 Tools Account to upload fights."))
        }
    };

    let compressed = match std::fs::read(slice_path(app_data_dir, &record.id)) {
        Ok(bytes) => bytes,
        // Older fights, and fights recorded with packet logging on: cut it
        // from a capture if one covers the fight.
        Err(_) => {
            let captures = find_captures(app_data_dir);
            let (encoded, _, _) = slice_for(record, &captures).map_err(|_| {
                UploadFailure::fatal(
                    "This fight has no packets saved, so it cannot be verified or uploaded. \
                     Fights recorded from this version on can be.",
                )
            })?;
            gzip(&encoded).map_err(UploadFailure::fatal)?
        }
    };

    let meta = read_meta(app_data_dir, &record.id);
    let names: HashMap<String, String> = record
        .actors
        .iter()
        .map(|a| (a.actor_id.to_string(), a.nickname.clone()))
        .collect();
    let body = serde_json::json!({
        "slice": base64(&compressed),
        "names": names,
        "uploaderActorId": meta.uploader_actor_id,
        "fightStartMs": record.start_time_ms,
        "appVersion": crate::entity::fight_record::APP_VERSION,
        // Korea and Taiwan number their servers alike (10xx/20xx), so the
        // slice cannot say which a fight was on; these two settle it. See
        // `region_hints`.
        "uiLanguage": ui_language(settings),
        "utcOffsetMinutes": chrono::Local::now().offset().local_minus_utc() / 60,
    });

    let response = client
        .post(format!("{}/api/logs", crate::account::base_url()))
        .timeout(std::time::Duration::from_secs(60))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| UploadFailure::retry(format!("Could not reach a2tools.app: {e}")))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    let reply: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();

    if status.is_success() {
        let result: UploadResult = serde_json::from_value(reply)
            .map_err(|_| UploadFailure::retry("Unexpected reply from a2tools.app."))?;
        let _ = std::fs::create_dir_all(slices_dir(app_data_dir));
        update_meta(app_data_dir, &record.id, |meta| {
            meta.url = Some(result.url.clone());
            meta.visibility = Some(result.visibility.clone());
        });
        return Ok(result);
    }
    let code = status.as_u16();
    let message = match (code, reply.get("error").and_then(|e| e.as_str())) {
        (401, _) => "Your sign-in has expired. Connect your account again in Settings.".into(),
        (403, Some("insufficient_scope")) => {
            "This sign-in was made before uploads existed. Sign out and connect again in \
             Settings to allow them."
                .into()
        }
        (429, _) => "Too many uploads in the last hour. Try again later.".into(),
        (_, _) => reply
            .get("message")
            .and_then(|m| m.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("Upload failed ({status}).")),
    };
    // A server that is down, busy or rate limiting may take it later; one that
    // refused the fight, or the sign-in, will refuse it again.
    let retryable = code == 408 || code == 429 || status.is_server_error();
    Err(UploadFailure { message, retryable, keyring_locked: false })
}

/// Standard base64. Small enough that a dependency is not worth having.
#[cfg_attr(not(feature = "online"), allow(dead_code))]
pub(crate) fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(feature = "online")]
#[cfg(test)]
mod upload_tests {
    use super::*;

    fn fight(id: &str, start: i64, duration: i64, is_train: bool) -> FightRecord {
        let mut r: FightRecord = serde_json::from_value(serde_json::json!({
            "id": id, "bossName": "B", "targetId": 1, "startTimeMs": start,
            "durationMs": duration, "totalDamage": 1, "jobs": [],
            "details": {"targetId": 1, "maxHp": 0, "totalTargetDamage": 1, "battleTime": duration,
                        "startTime": 0, "skills": [], "pingHistory": [], "healSkills": []},
            "actors": []
        }))
        .unwrap();
        r.is_train = is_train;
        r
    }

    #[test]
    fn the_uploader_is_an_actor_in_the_fight() {
        let mut r = fight("u1", 0, 30_000, false);
        r.actors = serde_json::from_value(serde_json::json!([
            {"actorId": 5886, "nickname": "Tsuri", "job": "", "jobId": 17},
            {"actorId": 2883, "nickname": "An*a", "job": "", "jobId": 14}
        ])).unwrap();
        assert_eq!(uploader_in(&r, Some(5886), None), Some(5886));
        // An id from before a zone change, or a party placeholder: the name decides.
        assert_eq!(uploader_in(&r, Some(5844), Some("Tsuri".into())), Some(5886));
        assert_eq!(uploader_in(&r, Some(90_000_001), Some("Tsuri".into())), Some(5886));
        // Neither in the fight: no uploader rather than a wrong one.
        assert_eq!(uploader_in(&r, Some(5844), Some("Naicha".into())), None);
        assert_eq!(uploader_in(&r, None, None), None);
    }

    #[test]
    fn a_failed_auto_upload_is_retried_on_a_schedule_and_then_left() {
        let dir = std::env::temp_dir().join(format!("a2t-retry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(slices_dir(&dir)).unwrap();
        std::fs::write(slice_path(&dir, "f1"), b"slice").unwrap();
        write_meta(&dir, "f1", &SliceMeta::default());
        let t = 1_000_000;
        assert!(auto_upload_retries_due(&dir, t).is_empty(), "never failed: not a retry");
        note_auto_upload_failure(&dir, "f1", &UploadFailure::retry("offline"), t);
        assert!(auto_upload_retries_due(&dir, t + 59_000).is_empty(), "first retry after a minute");
        assert_eq!(auto_upload_retries_due(&dir, t + 60_000), vec!["f1".to_string()]);
        for n in 2..=AUTO_RETRY_MINUTES.len() {
            note_auto_upload_failure(&dir, "f1", &UploadFailure::retry("offline"), t);
            assert!(!read_meta(&dir, "f1").gave_up, "attempt {n}");
        }
        note_auto_upload_failure(&dir, "f1", &UploadFailure::retry("offline"), t);
        assert!(read_meta(&dir, "f1").gave_up, "out of retries");
        assert!(auto_upload_retries_due(&dir, i64::MAX).is_empty());

        std::fs::write(slice_path(&dir, "f2"), b"slice").unwrap();
        write_meta(&dir, "f2", &SliceMeta::default());
        note_auto_upload_failure(&dir, "f2", &UploadFailure::fatal("refused"), t);
        assert!(read_meta(&dir, "f2").gave_up, "a refused fight is not retried");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_locked_keyring_never_ends_the_auto_upload() {
        let dir = std::env::temp_dir().join(format!("a2t-locked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(slices_dir(&dir)).unwrap();
        std::fs::write(slice_path(&dir, "f1"), b"slice").unwrap();
        write_meta(&dir, "f1", &SliceMeta::default());
        let t = 1_000_000;
        for _ in 0..50 {
            note_auto_upload_failure(&dir, "f1", &UploadFailure::locked("locked"), t);
        }
        let meta = read_meta(&dir, "f1");
        assert!(!meta.gave_up);
        assert_eq!(meta.auto_attempts, 1, "a locked keyring uses up no retries");
        assert_eq!(auto_upload_retries_due(&dir, t + KEYRING_RETRY_MINUTES * 60_000), vec!["f1".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn updating_an_uploader_and_a_retry_preserves_both_without_blocking_other_fights() {
        let dir = std::env::temp_dir().join(format!("a2t-meta-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(slices_dir(&dir)).unwrap();
        write_meta(&dir, "f1", &SliceMeta {
            visibility: Some("unlisted".into()), ..Default::default()
        });
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first_dir = dir.clone();
        let first = std::thread::spawn(move || update_meta(&first_dir, "f1", |meta| {
            meta.uploader_actor_id = Some(9);
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        }));
        started_rx.recv().unwrap();
        let retry_dir = dir.clone();
        let retry = std::thread::spawn(move || {
            note_auto_upload_failure(&retry_dir, "f1", &UploadFailure::retry("offline"), 1_000);
        });
        let other_dir = dir.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let other = std::thread::spawn(move || {
            update_meta(&other_dir, "f2", |meta| meta.uploader_actor_id = Some(10));
            done_tx.send(()).unwrap();
        });
        let independent = done_rx.recv_timeout(std::time::Duration::from_secs(2));
        release_tx.send(()).unwrap();
        first.join().unwrap();
        retry.join().unwrap();
        other.join().unwrap();
        independent.expect("metadata for another fight waited");
        let meta = read_meta(&dir, "f1");
        assert_eq!(meta.uploader_actor_id, Some(9));
        assert_eq!(meta.visibility.as_deref(), Some("unlisted"));
        assert_eq!(meta.auto_attempts, 1);
        assert_eq!(meta.retry_at_ms, 61_000);
        assert_eq!(read_meta(&dir, "f2").uploader_actor_id, Some(10));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_upload_failures_do_not_lose_retry_attempts() {
        let dir = std::env::temp_dir().join(format!("a2t-meta-retries-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(slices_dir(&dir)).unwrap();
        write_meta(&dir, "f1", &SliceMeta { uploader_actor_id: Some(5), ..Default::default() });
        let start = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    start.wait();
                    note_auto_upload_failure(&dir, "f1", &UploadFailure::retry("offline"), 1_000);
                });
            }
        });
        let meta = read_meta(&dir, "f1");
        assert_eq!(meta.auto_attempts, 8);
        assert!(meta.gave_up);
        assert_eq!(meta.uploader_actor_id, Some(5));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_upload_waits_for_the_end_and_fires_once() {
        let dir = std::env::temp_dir().join(format!("a2t-auto-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(slices_dir(&dir)).unwrap();
        let boss = fight("auto_9_1000", 1_000, 60_000, false);
        let ended = 1_000 + 60_000 + ENDED_AFTER_MS;

        assert!(!wants_auto_upload(&dir, &boss, ended), "no slice, nothing to send");
        std::fs::write(slice_path(&dir, &boss.id), b"x").unwrap();
        assert!(!wants_auto_upload(&dir, &boss, ended - 1), "still being fought");
        assert!(wants_auto_upload(&dir, &boss, ended));
        assert!(!wants_auto_upload(&dir, &fight("auto_9_1000", 1_000, 60_000, true), ended),
                "a training dummy is never a log");
        write_meta(&dir, &boss.id, &SliceMeta { uploader_actor_id: None,
                   url: Some("https://a2tools.app/logs/x".into()), visibility: None, ..Default::default() });
        assert!(!wants_auto_upload(&dir, &boss, ended), "already uploaded");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn base64_matches_the_standard_vectors() {
        for (raw, want) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64(raw.as_bytes()), want);
        }
    }

    #[test]
    fn share_status_pairs_a_slice_with_its_upload() {
        let dir = std::env::temp_dir().join(format!("a2t-share-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(slices_dir(&dir)).unwrap();
        std::fs::write(slice_path(&dir, "auto_1_2"), b"x").unwrap();
        write_meta(&dir, "auto_1_2", &SliceMeta {
            uploader_actor_id: Some(5),
            url: Some("https://a2tools.app/logs/abc".into()),
            visibility: None,
            ..Default::default()
        });
        std::fs::write(slice_path(&dir, "auto_3_4"), b"x").unwrap();
        let status = share_status(&dir);
        assert!(status["auto_1_2"].has_slice);
        assert_eq!(status["auto_1_2"].url.as_deref(), Some("https://a2tools.app/logs/abc"));
        assert!(status["auto_3_4"].has_slice && status["auto_3_4"].url.is_none());

        // An orphaned completed slice is removed; temporary files are left
        // alone because pruning cannot prove that their writer has stopped.
        let leftover = slices_dir(&dir).join("auto_1_2.a2es.gz.7.0.tmp");
        std::fs::write(&leftover, b"x").unwrap();
        let legacy = slices_dir(&dir).join("auto_1_2.json.tmp");
        std::fs::write(&legacy, b"x").unwrap();
        let unknown = slices_dir(&dir).join("unrelated.txt");
        std::fs::write(&unknown, b"x").unwrap();
        let directory = slices_dir(&dir).join("unrelated.json");
        std::fs::create_dir(&directory).unwrap();
        std::fs::create_dir_all(dir.join("history")).unwrap();
        std::fs::write(dir.join("history").join("auto_3_4.json"), b"{}").unwrap();
        let dotted = "fight.json.extra";
        std::fs::write(slice_path(&dir, dotted), b"x").unwrap();
        write_meta(&dir, dotted, &SliceMeta::default());
        std::fs::write(dir.join("history").join(format!("{dotted}.json")), b"{}").unwrap();
        prune_slices(&dir);
        let status = share_status(&dir);
        assert!(!status.contains_key("auto_1_2"));
        assert!(leftover.exists());
        assert!(legacy.exists());
        assert!(unknown.exists());
        assert!(directory.is_dir());
        assert!(slice_path(&dir, dotted).exists());
        assert!(meta_path(&dir, dotted).exists());
        assert!(status.contains_key("auto_3_4"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

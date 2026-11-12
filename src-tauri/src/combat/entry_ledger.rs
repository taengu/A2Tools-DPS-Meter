//! What the game sends only as you enter an instance, kept for slices.
//!
//! The map load (`21 36`), party roster (`02 97`) and dungeon state (`00 61`:
//! the run's start, deadline and clear) say which dungeon a run is and where
//! it stands, and each monster's spawn (`41 36`) says what it is. All of them come
//! as the party enters. A boss pulled more than the slice's prelude after
//! that (Ludra's runs last up to two hours) had none of them in its slice,
//! and the log service refused it or filed it under the open world. So the
//! latest of each is kept here, per zone, and fed to the slice ahead of what
//! the packet ring still holds.
//!
//! On a load into another zone, the zone left is kept in memory for
//! `LEFT_ZONE_KEPT_MS`, for a fight in it whose slice is cut after the load
//! (`EntryPackets::for_fight`); a fight in the new zone never takes from it.
//!
//! A run outlives the meter: a player may restart it in the middle of one.
//! So the ledger of the instance you are in is also kept on disk
//! (`entry_ledger.json`) and read back at the next start. What is read back
//! is used only once the game confirms it is still the same instance: your
//! entity id, which every load hands out anew, must be the one it was saved
//! with. A map load before that, or an age past `MAX_AGE_MS` from the load
//! it was made in, drops it. A run that ended without a kill therefore
//! leaves nothing a later run can use.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Spawn records kept at most per zone. A zone holds a few hundred.
const SPAWN_CAP: usize = 4_096;

/// How old a ledger read back may be, counted from the map load it was made
/// in: the longest run (two hours) and some.
pub const MAX_AGE_MS: i64 = 150 * 60_000;

/// How long the zone just left is kept: a fight in it may still be saved,
/// and its slice cut, after the load out (the save before a reset, the
/// periodic save's last pass).
pub const LEFT_ZONE_KEPT_MS: i64 = 30 * 60_000;

const FILE: &str = "entry_ledger.json";

pub fn path_in(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(FILE)
}

/// A framed packet with its capture time.
pub type Stamped = (i64, Vec<u8>);

/// The ledger as kept on disk. Packets are hex.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Saved {
    /// Your entity id in the instance, as the game stated it.
    pub local_id: i64,
    /// The map it was made in, and the dungeon the roster named.
    pub map_id: i32,
    pub roster_dungeon: i32,
    pub map_load: (i64, String),
    pub roster: Option<(i64, String)>,
    /// None in a ledger saved before it was kept.
    #[serde(default)]
    pub run: Option<(i64, String)>,
    pub spawns: Vec<(i64, String)>,
}

/// Which of the kept records a framed packet is.
enum Kind {
    MapLoad(i32),
    Roster,
    Run,
    Spawn(i32),
}

fn kind(packet: &[u8]) -> Option<Kind> {
    use crate::capture::stream_processor::read_varint;
    let len = read_varint(packet, 0);
    let at = usize::try_from(len.length).ok().filter(|&n| n > 0)?;
    match packet.get(at..at + 2)? {
        [0x21, 0x36] => {
            let b = packet.get(at + 6..at + 10)?;
            Some(Kind::MapLoad(i32::from_le_bytes([b[0], b[1], b[2], b[3]])))
        }
        [0x02, 0x97] => Some(Kind::Roster),
        [0x00, 0x61] => Some(Kind::Run),
        [0x41, 0x36] => {
            let entity = read_varint(packet, at + 2);
            (entity.length > 0).then_some(Kind::Spawn(entity.value))
        }
        _ => None,
    }
}

/// One zone's records, from its map load on.
#[derive(Default)]
struct Zone {
    map_load: Option<Stamped>,
    roster: Option<Stamped>,
    run: Option<Stamped>,
    spawns: HashMap<i32, Stamped>,
}

impl Zone {
    fn map_id(&self) -> Option<i32> {
        match kind(&self.map_load.as_ref()?.1)? {
            Kind::MapLoad(map) => Some(map),
            _ => None,
        }
    }

    fn since(&self) -> Option<i64> {
        self.map_load.as_ref().map(|(t, _)| *t)
    }

    fn packets(&self) -> Vec<Stamped> {
        let mut out: Vec<Stamped> =
            self.map_load.iter().chain(&self.roster).chain(&self.run).chain(self.spawns.values()).cloned().collect();
        out.sort_by_key(|(t, _)| *t);
        out
    }
}

/// The records a slice may take: the zone you are in, and the one you left,
/// for a fight that started before you left it.
#[derive(Default, Clone)]
pub struct EntryPackets {
    current: Vec<Stamped>,
    /// When you left it, and its records.
    previous: Option<(i64, Vec<Stamped>)>,
}

impl EntryPackets {
    /// The records of the zone a fight starting at `start_ms` was in.
    pub fn for_fight(&self, start_ms: i64) -> &[Stamped] {
        match &self.previous {
            Some((left, packets)) if start_ms < *left => packets,
            _ => &self.current,
        }
    }
}

#[derive(Default)]
pub struct EntryLedger {
    current: Zone,
    /// The zone left last, and when.
    previous: Option<(i64, Zone)>,
    /// Read back from disk and not yet confirmed (see the module docs).
    restored: Option<Saved>,
    /// Changed since it was last written.
    dirty: bool,
    /// The local id the last write was made with.
    written_for: Option<i64>,
}

impl EntryLedger {
    /// A framed packet captured at `at`; anything but the three is ignored.
    pub fn note(&mut self, at: i64, packet: &[u8]) {
        match kind(packet) {
            Some(Kind::MapLoad(map)) => {
                if self.current.map_id() == Some(map) {
                    // A teleport inside the instance: new entity ids, so the
                    // spawns kept are of the old ones. The roster stands.
                    self.current.spawns.clear();
                } else {
                    // Another zone. The one left is kept a while. A roster
                    // that came since its load (the party formed, then
                    // entered) is the new one's too.
                    let left = std::mem::take(&mut self.current);
                    let since = left.since();
                    self.current.roster = left.roster.clone().filter(|(t, _)| since.is_none_or(|s| *t >= s));
                    if left.map_load.is_some() || !left.spawns.is_empty() {
                        self.previous = Some((at, left));
                    }
                }
                self.current.map_load = Some((at, packet.to_vec()));
                self.restored = None;
            }
            Some(Kind::Roster) => self.current.roster = Some((at, packet.to_vec())),
            // The run's state comes after the load into it, and its latest
            // has the start and deadline (or the clear).
            Some(Kind::Run) => self.current.run = Some((at, packet.to_vec())),
            Some(Kind::Spawn(entity)) => {
                let spawns = &mut self.current.spawns;
                if spawns.len() >= SPAWN_CAP && !spawns.contains_key(&entity) {
                    if let Some(&oldest) = spawns.iter().min_by_key(|(_, (t, _))| *t).map(|(id, _)| id) {
                        spawns.remove(&oldest);
                    }
                }
                spawns.insert(entity, (at, packet.to_vec()));
            }
            None => return,
        }
        self.dirty = true;
    }

    /// The records kept, oldest first; the zone left only while it is kept.
    pub fn packets(&mut self, now_ms: i64) -> EntryPackets {
        if self.previous.as_ref().is_some_and(|(left, _)| now_ms - left > LEFT_ZONE_KEPT_MS) {
            self.previous = None;
        }
        EntryPackets {
            current: self.current.packets(),
            previous: self.previous.as_ref().map(|(left, zone)| (*left, zone.packets())),
        }
    }

    /// Take a ledger read back at start-up, to be confirmed or dropped.
    pub fn restore(&mut self, saved: Saved) {
        self.restored = Some(saved);
    }

    pub fn has_restored(&self) -> bool {
        self.restored.is_some()
    }

    /// The game named you `local_id`. The restored ledger is this instance's
    /// when the id is the one it was saved with: its records join the ones
    /// seen since, which are newer and win. Otherwise it is dropped. Returns
    /// it when it was taken.
    pub fn confirm(&mut self, local_id: i64) -> Option<Saved> {
        let saved = self.restored.take()?;
        if saved.local_id != local_id {
            tracing::info!("entry ledger: read back for entity {}, you are {local_id}: dropped", saved.local_id);
            return None;
        }
        let unhex = |(t, h): &(i64, String)| hex_decode(h).map(|p| (*t, p));
        let zone = &mut self.current;
        if zone.map_load.is_none() {
            zone.map_load = unhex(&saved.map_load);
        }
        if zone.roster.is_none() {
            zone.roster = saved.roster.as_ref().and_then(unhex);
        }
        if zone.run.is_none() {
            zone.run = saved.run.as_ref().and_then(unhex);
        }
        for (t, packet) in saved.spawns.iter().filter_map(unhex) {
            if let Some(Kind::Spawn(entity)) = kind(&packet) {
                zone.spawns.entry(entity).or_insert((t, packet));
            }
        }
        self.dirty = true;
        tracing::info!("entry ledger: read back for map {} confirmed ({} spawns)", saved.map_id, zone.spawns.len());
        Some(saved)
    }

    /// What to write, if anything changed since the last write: `Some(None)`
    /// to remove the file (nothing worth keeping), `Some(Some(_))` to write.
    /// Only the ledger of the instance you are in is kept, with your id in it
    /// as the game stated it: the open world's comes and goes with what is in
    /// view, and one read back is of use only in the instance it was made in
    /// (the zone left stays in memory alone).
    pub fn take_save(&mut self, local_id: Option<i64>, roster_dungeon: i32) -> Option<Option<Saved>> {
        if self.restored.is_some() || (!self.dirty && self.written_for == local_id) {
            return None;
        }
        self.dirty = false;
        self.written_for = local_id;
        let hex = |(t, p): &Stamped| (*t, hex_encode(p));
        let zone = &self.current;
        let saved = match (zone.map_id(), &zone.map_load, local_id) {
            (Some(map), Some(load), Some(local_id)) if !super::data_storage::is_open_world_map(map) => {
                let mut spawns: Vec<&Stamped> = zone.spawns.values().collect();
                spawns.sort_by_key(|(t, _)| *t);
                Some(Saved {
                    local_id,
                    map_id: map,
                    roster_dungeon,
                    map_load: hex(load),
                    roster: zone.roster.as_ref().map(hex),
                    run: zone.run.as_ref().map(hex),
                    spawns: spawns.into_iter().map(hex).collect(),
                })
            }
            _ => None,
        };
        Some(saved)
    }
}

/// The ledger kept at `path`, if it is young enough; a stale or unreadable
/// one is removed.
pub fn load(path: &Path, now_ms: i64) -> Option<Saved> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<Saved>(&text) {
        Ok(saved) if now_ms - saved.map_load.0 <= MAX_AGE_MS => Some(saved),
        _ => {
            let _ = std::fs::remove_file(path);
            None
        }
    }
}

/// Write (or, for `None`, remove) the ledger at `path`. The desktop meter's
/// alone: the log service keeps none.
#[cfg(feature = "desktop")]
pub fn store(path: &Path, saved: Option<&Saved>) {
    match saved {
        Some(saved) => {
            if let Ok(json) = serde_json::to_vec(saved) {
                if let Err(e) = crate::atomic_file::write(path, &json) {
                    tracing::debug!("entry ledger: not written: {e}");
                }
            }
        }
        None => {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::evidence_slice::{self, CapturedPacket};
    use crate::combat::data_storage::DataStorage;

    const T0: i64 = 1_800_000_000_000;
    const MINUTE: i64 = 60_000;
    /// Ludra (Normal): an instance whose map id is its dungeon id.
    const LUDRA: i32 = 600_082;

    fn framed(payload: &[u8]) -> Vec<u8> {
        // As the slice tests frame one: the length counts 3 bytes more.
        let mut v = vec![(payload.len() + 4) as u8];
        v.extend_from_slice(payload);
        v
    }

    fn map_load(map: i32) -> Vec<u8> {
        let mut body = vec![0x21, 0x36, 0x01, 0x00, 0x00, 0x00];
        body.extend(map.to_le_bytes());
        body.extend([0; 8]);
        framed(&body)
    }

    /// Ludra running, its hour from entry (`00 61` state 2).
    fn run_state() -> Vec<u8> {
        let mut body = vec![0x00, 0x61];
        body.extend((LUDRA as u32).to_le_bytes());
        body.push(2);
        body.extend((T0 + 5_000).to_le_bytes());
        body.extend((T0 + 5_000 + 60 * MINUTE).to_le_bytes());
        framed(&body)
    }

    fn boss_spawn() -> Vec<u8> {
        framed(&[0x41, 0x36, 0x09, 0x11])
    }

    fn hit() -> Vec<u8> {
        framed(&[0x04, 0x38, 0x09])
    }

    fn at(ms: i64, f: impl FnOnce()) {
        crate::clock::set_override(Some(ms));
        f();
        crate::clock::set_override(None);
    }

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("a2t-entry-ledger-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        path_in(&dir)
    }

    /// Entered Ludra, the boss spawned, you are entity 4451: what the meter
    /// writes for it.
    #[cfg(feature = "desktop")]
    fn entered(path: &Path) {
        let s = DataStorage::new();
        at(T0, || s.note_entry_packet(&map_load(LUDRA)));
        at(T0 + 5_000, || s.note_entry_packet(&run_state()));
        at(T0 + MINUTE, || s.note_entry_packet(&boss_spawn()));
        s.set_local_identity_from_game(4451, Some("Me".into()));
        let saved = s.entry_ledger_to_save().expect("changed").expect("an instance's");
        store(path, Some(&saved));
        assert_eq!(s.entry_ledger_to_save(), None, "nothing new to write");
    }

    /// A slice as `share::save_slice` cuts it: the records kept from before
    /// what the ring holds (here, the pull alone), then the ring.
    fn slice_of_pull(s: &DataStorage, start: i64) -> evidence_slice::EvidenceSlice {
        let ring = vec![CapturedPacket { captured_at_ms: start, stream: "Client:1".into(), bytes: hit() }];
        let mut packets: Vec<CapturedPacket> = s
            .entry_packets()
            .for_fight(start)
            .iter()
            .filter(|(t, _)| *t < start)
            .map(|(t, p)| CapturedPacket { captured_at_ms: *t, stream: "Client:0:0".into(), bytes: p.clone() })
            .collect();
        packets.extend(ring);
        evidence_slice::build(&packets, start, start + 10_000, &Default::default(), [7; 32]).unwrap()
    }

    fn holds(slice: &evidence_slice::EvidenceSlice, packet: &[u8]) -> bool {
        slice.records.iter().any(|(_, p)| p == packet)
    }

    /// The meter restarted in the middle of a Ludra run: a pull 90 minutes
    /// after entry still has the boss's spawn and the map load that says
    /// which dungeon it is, once the game names you as the entity you were.
    #[test]
    #[cfg(feature = "desktop")]
    fn a_restart_mid_run_keeps_the_entry() {
        let path = temp_path("restart");
        entered(&path);

        let s = DataStorage::new();
        let pull = T0 + 90 * MINUTE;
        at(pull - MINUTE, || s.restore_entry_ledger(&path));
        assert!(s.entry_packets().for_fight(pull).is_empty(), "not before the game names you");
        s.set_local_identity_from_game(4451, Some("Me".into()));
        let slice = at_value(pull, || slice_of_pull(&s, pull));
        assert!(holds(&slice, &boss_spawn()), "the boss's spawn");
        assert!(holds(&slice, &map_load(LUDRA)), "the map load");
        // Its times relative to the pull, as the slice's own: the run began
        // 90 minutes before it, with an hour's limit.
        let run = slice.records.iter().find(|(_, p)| p[1..3] == [0x00, 0x61]).map(|(_, p)| p).expect("the run's state");
        let time = i64::from_le_bytes(run[8..16].try_into().unwrap());
        let deadline = i64::from_le_bytes(run[16..24].try_into().unwrap());
        assert_eq!((time, deadline - time), (T0 + 5_000 - pull, 60 * MINUTE));
        assert_eq!(s.current_dungeon_id(), LUDRA, "the meter knows where it is too");
        let _ = std::fs::remove_file(&path);
    }

    /// What was read back is dropped by a load into another zone, by another
    /// entity being you (the run ended while the meter was closed), and when
    /// it is older than a run can be.
    #[test]
    #[cfg(feature = "desktop")]
    fn a_ledger_read_back_from_elsewhere_is_dropped() {
        let path = temp_path("dropped");
        let pull = T0 + 90 * MINUTE;

        entered(&path);
        let s = DataStorage::new();
        at(pull - MINUTE, || {
            s.restore_entry_ledger(&path);
            s.note_entry_packet(&map_load(1000));
        });
        s.set_local_identity_from_game(4451, Some("Me".into()));
        let slice = at_value(pull, || slice_of_pull(&s, pull));
        assert!(!holds(&slice, &boss_spawn()) && !holds(&slice, &map_load(LUDRA)), "another map load came");

        let s = DataStorage::new();
        at(pull - MINUTE, || s.restore_entry_ledger(&path));
        s.set_local_identity_from_game(7702, Some("Me".into()));
        let slice = at_value(pull, || slice_of_pull(&s, pull));
        assert!(!holds(&slice, &boss_spawn()), "you are another entity: another instance");
        assert_eq!(s.current_dungeon_id(), 0);

        let s = DataStorage::new();
        at(T0 + MAX_AGE_MS + MINUTE, || s.restore_entry_ledger(&path));
        assert!(!path.exists(), "too old: removed");
        s.set_local_identity_from_game(4451, Some("Me".into()));
        assert!(s.entry_packets().for_fight(pull).is_empty());
    }

    /// A fight in the zone just left still gets its records for half an hour
    /// (its last save comes after the load out); a fight in the new zone
    /// never does, even on an entity id the old zone had.
    #[test]
    fn the_zone_left_is_kept_for_its_own_fights() {
        let mut ledger = EntryLedger::default();
        ledger.note(T0, &map_load(LUDRA));
        ledger.note(T0 + MINUTE, &boss_spawn());
        let boss_fight = T0 + 80 * MINUTE;
        let left = T0 + 100 * MINUTE;
        ledger.note(left, &map_load(1000));

        let kept = ledger.packets(left + MINUTE);
        assert!(kept.for_fight(boss_fight).iter().any(|(_, p)| *p == boss_spawn()), "the boss's fight");
        let new_zone = kept.for_fight(left + 2 * MINUTE);
        assert_eq!(new_zone.len(), 1, "the new zone's map load alone");
        assert_eq!(new_zone[0].1, map_load(1000));

        let later = ledger.packets(left + LEFT_ZONE_KEPT_MS + MINUTE);
        assert!(later.for_fight(boss_fight).iter().all(|(_, p)| *p != boss_spawn()), "dropped after half an hour");

        // Nothing of the open world goes to disk; the file goes.
        assert_eq!(ledger.take_save(Some(4451), 0), Some(None));
    }

    fn at_value<T>(ms: i64, f: impl FnOnce() -> T) -> T {
        crate::clock::set_override(Some(ms));
        let v = f();
        crate::clock::set_override(None);
        v
    }
}

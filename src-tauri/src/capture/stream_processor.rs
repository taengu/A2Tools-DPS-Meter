use std::collections::HashSet;
use std::sync::Arc;


use crate::combat::data_storage::{DataStorage, NoDamageHit};
use crate::entity::damage_packet::ParsedDamagePacket;
use crate::entity::job_class::JobClass;
use crate::entity::special_damage::{self, SpecialDamage};
use crate::i18n::lookup::{NpcLookup, SkillLookup};

/// VarInt decode result.
#[derive(Debug, Clone, Copy)]
pub struct VarIntResult {
    pub value: i32,
    pub length: i32,
}

impl VarIntResult {
    pub fn invalid() -> Self {
        Self { value: -1, length: -1 }
    }
}

/// Context for resolving a compact skill aggregation packet.
#[derive(Debug, Clone)]
struct PendingCompactSkillContext {
    actor_id: i32,
    skill_raw: i32,
}

/// Bounded LRU set for deduplication.
struct BoundedHashSet {
    set: Vec<String>,
    max_size: usize,
}

impl BoundedHashSet {
    fn new(max_size: usize) -> Self {
        Self {
            set: Vec::new(),
            max_size,
        }
    }

    fn contains(&self, key: &str) -> bool {
        self.set.iter().any(|s| s == key)
    }

    fn insert(&mut self, key: String) -> bool {
        if self.contains(&key) {
            return false;
        }
        if self.set.len() >= self.max_size {
            self.set.remove(0);
        }
        self.set.push(key);
        true
    }
}

/// The core binary protocol parser for AION 2 game packets.
/// Ported exactly from Kotlin StreamProcessor.
pub struct StreamProcessor {
    data_storage: Arc<DataStorage>,
    skill_lookup: Arc<SkillLookup>,
    npc_lookup: Arc<NpcLookup>,
    seen_embedded_hexes: BoundedHashSet,
    dot_damage_skill_ids: HashSet<i32>,
    pending_compact_skill_context: Option<PendingCompactSkillContext>,
    /// When set, override the timestamp on all created packets (for replay mode).
    override_timestamp: Option<i64>,
}

/// The `42 36` flag of an entity leaving the world, a spirit unsummoned among
/// others (197 of the 370 in a 2026-10-06 capture were linked spirits).
const DESPAWN_FLAG: i32 = 7;

impl StreamProcessor {
    pub fn new(data_storage: Arc<DataStorage>, skill_lookup: Arc<SkillLookup>, npc_lookup: Arc<NpcLookup>) -> Self {
        Self {
            data_storage,
            skill_lookup,
            npc_lookup,
            seen_embedded_hexes: BoundedHashSet::new(16_384),
            dot_damage_skill_ids: HashSet::new(), // loaded lazily
            pending_compact_skill_context: None,
            override_timestamp: None,
        }
    }

    pub fn set_dot_skill_ids(&mut self, ids: HashSet<i32>) {
        self.dot_damage_skill_ids = ids;
    }

    /// Set an override timestamp for all packets created by this processor.
    /// Used in replay mode to use capture-time timestamps instead of wall clock.
    ///
    /// This also pins the thread's clock (`crate::clock`), because the processor
    /// is not the only thing that reads "now" while consuming a packet:
    /// `DataStorage` makes idle-reset and zone-reset decisions against it. Those
    /// used to be taken against the replaying machine's wall clock, so a replay
    /// of the same capture could produce different fights depending on when it
    /// was run. The override is thread-local, so a replay on a blocking thread
    /// cannot disturb a live capture running alongside it.
    pub fn set_override_timestamp(&mut self, ts: Option<i64>) {
        self.override_timestamp = ts;
        crate::clock::set_override(ts);
    }

    /// Parse as many complete packets as possible from the buffer.
    /// Returns the number of bytes consumed.
    pub fn consume_stream(&mut self, buffer: &[u8]) -> usize {
        // Framing lives in `capture::framing` so the Evidence Slice builder can
        // split a capture the same way this does. See that module.
        let framing = super::framing::walk(buffer);
        let offset = framing.consumed;

        for frame in &framing.frames {
            match frame.kind {
                super::framing::FrameKind::Bundle => {
                    self.unwrap_bundle(frame.payload(buffer), 1);
                }
                super::framing::FrameKind::Packet => {
                    self.parse_perfect_packet(frame.bytes(buffer));
                    self.scan_embedded_bundles_for_identity(frame.bytes(buffer));
                }
            }
        }

        // The unconsumed tail (a frame still arriving) is its own region.
        let regions = super::framing::regions(&framing.frames, buffer.len());
        self.scan_records(buffer, &regions);

        offset
    }

    /// The scans for records that sit anywhere in a packet, each run over one
    /// frame (or stretch between frames) at a time, so no record borrows the
    /// next frame's bytes: a `2a 37` record ending `44 36 33 ..`, read on into
    /// the next frame's length `8f 01` and spawn `41 36`, named entity 51 "A"
    /// (2026-10-06). Each scan still runs over all of `data` before the next.
    fn scan_records(&mut self, data: &[u8], regions: &[std::ops::Range<usize>]) {
        // Embedded 04 8D ownership and loot records.
        for r in regions {
            self.scan_for_embedded_04_8d(&data[r.clone()]);
        }
        if data.len() >= 4 {
            self.scan_for_entity_hp(data);
        }
        // Embedded spawn opcodes (40/41/44/45 36). Player spawns (45 36) also
        // sit mid-packet, where parse_summon_packet at the packet front never
        // looks: party members announced only that way stayed unnamed (#id).
        for r in regions {
            self.scan_for_embedded_40_36(&data[r.clone()]);
        }
        // Bind the local player from the account character-select list, which
        // arrives as plaintext (uncompressed) and can land in a standalone packet.
        for r in regions {
            self.scan_char_list_self(&data[r.clone()]);
        }
        // Mask-driven id<->name records: the self record (33 36) and every other
        // player's record (45 36). This is the primary naming source: it covers
        // the already-loaded case where no login char-list is in the capture.
        for r in regions {
            self.scan_masked_identity(&data[r.clone()]);
        }
        // Party roster (names, levels, gear score, combat power).
        for r in regions {
            self.scan_party_roster(&data[r.clone()]);
        }
    }

    /// Who you are, from compressed bundles that sit inside another packet.
    ///
    /// The game sends your self record (`33 36`: name, server, class, level)
    /// on zone loads and then every few minutes, and those later copies arrive
    /// in a bundle carried inside a larger packet, where the framing never
    /// opens it. A meter started mid-session therefore never learned your
    /// level: five copies went unread in one hour of a capture (2026-10-04),
    /// the player levelling 29 to 30 among them. Only identity is read from
    /// these: what else they hold is left as it was, so no fight changes.
    fn scan_embedded_bundles_for_identity(&self, packet: &[u8]) {
        let mut i = 1;
        while i + 8 < packet.len() {
            if packet[i] != 0xFF || packet[i + 1] != 0xFF {
                i += 1;
                continue;
            }
            // `<varint len> FF FF <size u32> <lz4>`, sized as `framing` does.
            let bundle = (1..=3usize).rev().find_map(|n| {
                let at = i.checked_sub(n)?;
                let len = read_varint(packet, at);
                if len.length != n as i32 {
                    return None;
                }
                let end = at + super::framing::frame_size(len.value, len.length)?;
                let data = super::framing::decompress_bundle(packet.get(i..end)?)?;
                Some((end, data))
            });
            match bundle {
                Some((end, data)) => {
                    let walk = super::framing::walk_inner(&data);
                    let regions = super::framing::regions(&walk.frames, data.len());
                    for r in &regions {
                        self.scan_masked_identity(&data[r.clone()]);
                    }
                    for r in &regions {
                        self.scan_party_roster(&data[r.clone()]);
                    }
                    i = end;
                }
                None => i += 1,
            }
        }
    }

    /// `depth` is 1 for a bundle in the stream, one more for each bundle it
    /// sits in. Past `MAX_BUNDLE_DEPTH` it is skipped, as the slice builder does.
    fn unwrap_bundle(&mut self, payload: &[u8], depth: usize) {
        // payload starts at FF FF
        // Format: FF FF (2) + decompressed_size (4 LE) + LZ4 compressed data
        if payload.len() < 7 || depth > super::framing::MAX_BUNDLE_DEPTH {
            return;
        }

        let decompressed = match super::framing::decompress_bundle(payload) {
            Some(d) => d,
            None => return,
        };

        // Walk decompressed data as varint-framed inner packets. The walk lives
        // in `capture::framing` so the Evidence Slice builder splits a bundle
        // exactly the way this does.
        self.pending_compact_skill_context = None;

        let walk = super::framing::walk_inner(&decompressed);
        for frame in &walk.frames {
            match frame.kind {
                super::framing::FrameKind::Bundle => {
                    self.unwrap_bundle(frame.payload(&decompressed), depth + 1);
                }
                super::framing::FrameKind::Packet => {
                    let inner_packet = frame.bytes(&decompressed);
                    if let Some(ctx) = self.extract_pending_compact_skill_context(inner_packet) {
                        self.pending_compact_skill_context = Some(ctx);
                    }
                    self.parse_perfect_packet(inner_packet);
                }
            }
        }

        // The same record scans as a stream gets, over the decompressed data.
        let regions = super::framing::regions(&walk.frames, decompressed.len());
        self.scan_records(&decompressed, &regions);

        self.pending_compact_skill_context = None;
    }

    fn parse_perfect_packet(&mut self, packet: &[u8]) -> bool {
        if packet.len() < 3 {
            return false;
        }

        // Buffs, debuffs and stats, recorded alongside (see `capture::abnormal`).
        self.data_storage.note_abnormal_packet(packet);

        let parsed_damage = self.parsing_damage(packet, true, false);
        let parsed_ownership = self.parse_summon_ownership_packet(packet);
        let parsed_summon = self.parse_summon_packet(packet);
        let parsed_name = self.parse_actor_name_binding_rules(packet)
            || self.parse_loot_attribution_actor_name(packet)
            || self.parsing_nickname(packet);
        let parsed_hp = self.parse_hp_mp_update_packet(packet);
        self.parse_party_scope_packet(packet);
        self.parse_self_stats_packet(packet);
        self.parse_death_packet(packet);
        self.parse_zone_change_packet(packet);
        self.parse_map_load_packet(packet);

        if !parsed_damage && !parsed_name && !parsed_summon && !parsed_ownership && !parsed_hp {
            self.parse_dot_packet(packet);
        }

        parsed_damage || parsed_name
    }

    // ===== PARTY SCOPE (06 38) =====

    /// `<len> 06 38 <entity_id varint> ...`: a record the server sends about
    /// you and your party only. What it carries is not decoded; who it is
    /// about is what identifies your loot (see `DataStorage::note_party_scope`).
    fn parse_party_scope_packet(&self, packet: &[u8]) {
        let length_info = read_varint(packet, 0);
        if length_info.length <= 0 {
            return;
        }
        let offset = length_info.length as usize;
        if offset + 3 >= packet.len() || packet[offset] != 0x06 || packet[offset + 1] != 0x38 {
            return;
        }
        let id = read_varint(packet, offset + 2);
        if id.length > 0 {
            self.data_storage.note_party_scope(id.value);
        }
    }

    // ===== OWN STATS (4A 36) =====

    /// `<len> 4A 36 <entity_id varint> ...`: a record the server sends about
    /// the local player only (see `DataStorage::note_self_stats`). What it
    /// carries is not decoded.
    fn parse_self_stats_packet(&self, packet: &[u8]) {
        let length_info = read_varint(packet, 0);
        if length_info.length <= 0 {
            return;
        }
        let offset = length_info.length as usize;
        if offset + 3 >= packet.len() || packet[offset] != 0x4A || packet[offset + 1] != 0x36 {
            return;
        }
        let id = read_varint(packet, offset + 2);
        if id.length > 0 {
            self.data_storage.note_self_stats(id.value);
        }
    }

    // ===== ZONE CHANGE (23 36) =====

    /// Self/world teleport packet: `<len varint> 23 36 <entity_id varint = 0> <x><y><z> ...`.
    /// This fires once on entering a zone/instance (the local player is teleported in)
    /// and not during combat, so it drives a combat reset — the actual reset is gated
    /// by a lull + debounce in `note_zone_change`, so an in-combat teleport (a boss
    /// knockback/pull) never wipes an active fight. The June 2026 +1 opcode shift moved
    /// the spawn/death family but this position opcode (0x23) is unaffected.
    fn parse_zone_change_packet(&self, packet: &[u8]) {
        let length_info = read_varint(packet, 0);
        if length_info.length < 0 {
            return;
        }
        let offset = length_info.length as usize;
        if offset + 2 >= packet.len() {
            return;
        }
        if packet[offset] != 0x23 || packet[offset + 1] != 0x36 {
            return;
        }
        // entity id 0 == the local player being teleported (a zone load), as opposed
        // to another entity's routine position update.
        if packet[offset + 2] != 0x00 {
            return;
        }
        self.data_storage.note_zone_change();
    }

    // ===== MAP LOAD (21 36) =====

    /// `<len> 21 36 <u32 count> <u32 map id> ...`: sent on every zone load,
    /// naming the map from the game's Map table. A teleport inside an instance
    /// names the instance again; leaving names an open-world map. Seen on all
    /// 36 loads in a 2026-10-04 capture, each 52 bytes long.
    fn parse_map_load_packet(&self, packet: &[u8]) {
        let length_info = read_varint(packet, 0);
        if length_info.length <= 0 {
            return;
        }
        let offset = length_info.length as usize;
        if offset + 10 > packet.len() || packet[offset] != 0x21 || packet[offset + 1] != 0x36 {
            return;
        }
        let map_id = parse_u32_le(packet, offset + 6) as i32;
        self.data_storage.note_map_load(map_id);
    }

    // ===== DEATH PACKET (41 36) =====

    fn parse_death_packet(&self, packet: &[u8]) {
        let length_info = read_varint(packet, 0);
        if length_info.length < 0 {
            return;
        }
        let offset = length_info.length as usize;
        if offset + 1 >= packet.len() {
            return;
        }
        // Death opcode. Pre-2026-06 it was 0x3641 ([0x41,0x36]); the June 2026
        // update shifted the 0x36 spawn/death family by +1, so it is now 0x3642
        // ([0x42,0x36]). Accept both — the flag==3 check below rejects anything
        // that isn't actually a combat death.
        if packet[offset + 1] != 0x36 || (packet[offset] != 0x41 && packet[offset] != 0x42) {
            return;
        }
        let mut pos = offset + 2;

        let entity_info = read_varint(packet, pos);
        if entity_info.length <= 0 {
            return;
        }
        let entity_id = entity_info.value;
        pos += entity_info.length as usize;

        // Skip VarInt (always 0)
        let skip_info = read_varint(packet, pos);
        if skip_info.length <= 0 {
            return;
        }
        pos += skip_info.length as usize;

        // Death flag: 1 = zone-init (entity loaded dead), 3 = combat death,
        // 7 = gone from the world.
        let flag_info = read_varint(packet, pos);
        if flag_info.length <= 0 {
            return;
        }

        if flag_info.value == 3 {
            tracing::trace!("Death event: entity {} killed in combat", entity_id);
            self.data_storage.mark_entity_dead(entity_id);
        }
        // Only under the current opcode: under the old one (`41 36`, now the
        // spawn) a summon's spawn mask can read as flag 7.
        if flag_info.value == DESPAWN_FLAG && packet[offset] == 0x42 {
            self.data_storage.note_despawn(entity_id);
        }
    }

    // ===== DOT PACKET =====

    fn parse_dot_packet(&mut self, packet: &[u8]) {
        let length_info = read_varint(packet, 0);
        if length_info.length < 0 {
            return;
        }
        let offset = length_info.length as usize;

        if packet.len() <= offset + 1 {
            return;
        }
        if packet[offset] != 0x05 || packet[offset + 1] != 0x38 {
            return;
        }
        let mut offset = offset + 2;
        let target_info = read_varint(packet, offset);
        if target_info.length < 0 {
            return;
        }
        offset += target_info.length as usize;

        if offset >= packet.len() {
            return;
        }
        let effect_type = packet[offset] as u32;
        offset += 1;
        // Effect type: 0x02/0x0A = damage; 0x01/0x09 = heal; 0x0B = HoT.
        // 0x00 = status, 0x08 = buff. Exact match, NOT bitmask — 0x0B (HoT) has bit
        // 1 set and would leak through a mask.
        let is_damage = effect_type == 0x02 || effect_type == 0x0A;
        let is_heal = effect_type == 0x01 || effect_type == 0x09 || effect_type == 0x0B;
        if !is_damage && !is_heal {
            return;
        }

        let actor_info = read_varint(packet, offset);
        // Damage-on-self is rejected as noise; a self-HEAL is legitimate healing.
        if actor_info.length < 0 || (is_damage && actor_info.value == target_info.value) {
            tracing::debug!("DOT: bad actor or self-damage");
            return;
        }
        offset += actor_info.length as usize;

        let unknown_info = read_varint(packet, offset);
        if unknown_info.length < 0 {
            return;
        }
        offset += unknown_info.length as usize;

        if offset + 4 > packet.len() {
            return;
        }
        let skill_code = parse_u32_le(packet, offset) as i32 / 100;
        offset += 4;

        if !is_valid_skill_code(skill_code) {
            return;
        }

        let amount_info = read_varint(packet, offset);
        if amount_info.length < 0 || amount_info.value <= 0 || amount_info.value > 99_999_999 {
            return;
        }

        if is_heal {
            // Healing done — recorded per healer. No allowlist (the heal effect_type
            // is the gate). HoT = 0x0B. This runs on framed perfect packets only, so
            // there is no embedded over-read into adjacent records (the artifact that
            // produced bogus multi-million single-tick "heals" in offline scans).
            self.data_storage.append_heal(
                actor_info.value,
                skill_code,
                amount_info.value as i64,
                effect_type == 0x0B,
                self.override_timestamp.unwrap_or_else(crate::clock::now_ms),
            );
            return;
        }

        // Damage DoT: gated by the curated dot-skill allowlist.
        if !self.dot_damage_skill_ids.contains(&skill_code) {
            tracing::trace!("DOT: skill {} not in dot_ids (set size={})", skill_code, self.dot_damage_skill_ids.len());
            return;
        }

        let mut pdp = ParsedDamagePacket::new();
        if let Some(ts) = self.override_timestamp {
            pdp.set_timestamp(ts);
        }
        pdp.set_dot(true);
        pdp.set_target_id(target_info.value);
        pdp.set_actor_id(actor_info.value);
        pdp.set_skill_code(skill_code);
        pdp.set_damage(amount_info.value);

        if pdp.actor_id() != pdp.target_id() {
            self.data_storage.append_damage(pdp);
        }
    }

    // ===== HP/MP UPDATE =====

    fn parse_hp_mp_update_packet(&self, packet: &[u8]) -> bool {
        let length_info = read_varint(packet, 0);
        if length_info.length < 0 {
            return false;
        }
        let offset = length_info.length as usize;
        if offset + 1 >= packet.len() {
            return false;
        }
        if packet[offset] != 0x1B || packet[offset + 1] != 0x92 {
            return false;
        }

        let mut pos = offset + 2;
        let actor_info = read_varint(packet, pos);
        if actor_info.length <= 0 || actor_info.value < 100 || actor_info.value > 9_999_999 {
            return false;
        }
        let actor_id = actor_info.value;
        pos += actor_info.length as usize;

        let hp_info = read_varint(packet, pos);
        if hp_info.length <= 0 {
            return false;
        }
        pos += hp_info.length as usize;

        let hp_max_info = read_varint(packet, pos);
        if hp_max_info.length <= 0 || hp_max_info.value <= 0 || hp_max_info.value > 50_000_000 {
            return false;
        }

        // Always store HP — the entity may not be in mob_data yet if spawn
        // packet arrived before the capture started
        self.data_storage.append_mob_hp(actor_id, hp_max_info.value);

        true
    }

    // ===== SUMMON OWNERSHIP (04 8D) =====

    fn parse_summon_ownership_packet(&self, packet: &[u8]) -> bool {
        let Some((summon_id, owner_id, name)) = ownership_at_front(packet) else {
            return false;
        };

        // Only link confirmed summons
        if self.data_storage.is_confirmed_summon(summon_id) {
            self.data_storage.append_summon(owner_id, summon_id);
        }

        // Name field after owner ID
        if let Some((start, len)) = name {
            self.register_utf8_nickname(packet, owner_id, start, len);
        }

        true
    }

    // ===== EMBEDDED 04 8D SCAN =====

    // ===== LIVE ENTITY HP (8D <id> 02 01 00 <u32 LE current HP>) =====

    /// Live current-HP feed. Combat packets embed, per affected entity, a record
    /// `8D <entityId varint> <disc 3 bytes> <u32 LE current HP> 00 00 00 00`, where
    /// `disc` is `02 01 00` for NPCs/mobs and `01 01 01` for the local player. We
    /// capture the NPC readings so the boss bar can show REAL current HP — it
    /// declines as the boss is hit and jumps back up on a heal/phase reset (a
    /// Training Scarecrow floors at 1 then resets to full). Max HP is not in this
    /// record; it comes from the spawn packet or the observed peak (see
    /// `set_mob_current_hp`). The `02` discriminator keeps the player's own
    /// `01 01 01` record out of the mob HP store.
    fn scan_for_entity_hp(&self, data: &[u8]) {
        let mut i = 0;
        while i + 1 < data.len() {
            if data[i] != 0x8D {
                i += 1;
                continue;
            }
            let id_info = read_varint(data, i + 1);
            if id_info.length <= 0 || !(100..=9_999_999).contains(&id_info.value) {
                i += 1;
                continue;
            }
            let disc = i + 1 + id_info.length as usize;
            // Full record: `02 01 00 <u32 LE current HP> 00 00 00 00`. The 4 trailing
            // bytes (a second u32, always zero in this feed) are REQUIRED — without
            // them a look-alike sub-record `8D <id> 02 01 00 <other u32> <terminator>`
            // gets misread as a huge HP value and inflates the max (denominator),
            // which makes the boss-bar percentage read far too low.
            if disc + 11 > data.len() {
                i += 1;
                continue;
            }
            if data[disc] == 0x02
                && data[disc + 1] == 0x01
                && data[disc + 2] == 0x00
                && data[disc + 7] == 0x00
                && data[disc + 8] == 0x00
                && data[disc + 9] == 0x00
                && data[disc + 10] == 0x00
            {
                let h = disc + 3;
                let cur = u32::from_le_bytes([data[h], data[h + 1], data[h + 2], data[h + 3]]);
                // Sanity bound: real HP is well under this; rejects misparses.
                if cur <= 100_000_000 {
                    self.data_storage.set_mob_current_hp(id_info.value, cur as i32);
                }
                i = disc + 11;
                continue;
            }
            i += 1;
        }
    }

    fn scan_for_embedded_04_8d(&self, data: &[u8]) -> bool {
        let records = ownership_records(data);
        for r in &records {
            let (summon_id, owner_id, name) = (r.summon_id, r.owner_id, &r.name);
            if self.data_storage.is_confirmed_summon(summon_id) {
                self.data_storage.append_summon(owner_id, summon_id);
            }
            self.data_storage.append_nickname(owner_id, name);
            self.data_storage.note_player_server(name, r.server_id);
            // For a mob that was fought (it follows the mob's `35 38` despawn)
            // this is the loot owner, which so far has always been you.
            if !self.data_storage.is_confirmed_summon(summon_id)
                && self.data_storage.is_damage_target(summon_id)
                && self.data_storage.note_loot_owner(summon_id, owner_id, name)
            {
                tracing::info!("loot record: local player '{}' -> entity {}", name, owner_id);
            }
        }
        !records.is_empty()
    }

    // ===== EMBEDDED 40 36 SCAN =====

    fn scan_for_embedded_40_36(&mut self, data: &[u8]) {
        for (i, opcode, id) in embedded_spawns(data) {
            if opcode == 0x44 || opcode == 0x45 {
                // 44/45 36 = player spawn — extract name
                self.parse_player_spawn_name(data, i + 2);
            } else {
                // 40/41 36 = summon/mob spawn
                let mut real_id = id;
                if real_id > 1_000_000 {
                    real_id = (real_id & 0x3FFF) | 0x4000;
                }
                if !self.data_storage.is_mob(real_id) {
                    self.parse_summon_spawn_at(data, i + 2);
                }
            }
        }
    }

    /// Bind the local player from the account character-select list.
    ///
    /// At login (and after a relog) the game broadcasts the account's character
    /// list as a run of entries `03 <entity_id u32 LE> <name_len u8> <utf8 name>`,
    /// one per character on the account — including alts that are not in the world.
    /// When you are already loaded into a zone there is no in-world `45 36` spawn
    /// for the character you are playing, so this list is the only place the local
    /// player's name↔id pairing appears (verified against two live captures where
    /// the self id, e.g. 4715/6289 across a relog, only surfaced here).
    ///
    /// We bind ONLY the entry whose name matches the user-configured character
    /// name — that drives the existing local-player auto-bind. Alts are
    /// deliberately skipped so we never create a phantom entity that never
    /// fights. The entity id is u32 little-endian here, unlike the varint used by
    /// the `36`-family spawn opcodes. The entry has no reliable leading tag (a
    /// `03` seen in one class's list was coincidental), so we scan for the
    /// `<id u32-LE> <name_len> <name>` shape directly and let the exact
    /// character-name match reject false positives.
    fn scan_char_list_self(&self, data: &[u8]) {
        // Once the game has sent its self record this list has nothing to add,
        // and its ids are the list's own, not in-world entities: at character
        // select after playing Spirtmasta (entity 10044) it listed her as 7796,
        // and binding that moved "you" onto an entity that never fights.
        if self.data_storage.local_identity_from_game() {
            return;
        }
        let local_name = match self.data_storage.local_character_name() {
            Some(n) => n.trim().to_string(),
            None => return,
        };
        if local_name.is_empty() {
            return;
        }
        let mut i = 0;
        while i + 5 < data.len() {
            let entity_id =
                u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
            if !(100..=9_999_999).contains(&entity_id) {
                i += 1;
                continue;
            }
            let name_len = data[i + 4] as usize;
            if !NAME_FIELD_BYTES.contains(&name_len) {
                i += 1;
                continue;
            }
            let name_start = i + 5;
            let name_end = name_start + name_len;
            if name_end > data.len() {
                i += 1;
                continue;
            }
            if let Some(name) = exact_name(&data[name_start..name_end]) {
                if name == local_name {
                    self.data_storage.append_nickname_authoritative(entity_id as i32, &name);
                    tracing::info!(
                        "char-list: bound local player '{}' -> entity {}",
                        name,
                        entity_id
                    );
                    return;
                }
            }
            i += 1;
        }
    }

    /// Bind entity ids to character names from the game's masked identity records.
    ///
    /// The self record (`33 36`) and the other-player record (`45 36`) share one
    /// layout:
    ///
    /// ```text
    /// <opcode 2B> <entity_id varint> <mask1 u32 LE> <mask2 u8> [mask2 & 0x01] <len u8><utf8 name>
    /// ```
    ///
    /// The name is present exactly when bit 0 of `mask2` is set; the remaining
    /// bits of `mask2` vary with whatever else the record carries. Earlier
    /// versions of this parser keyed off the literal bytes that happened to sit
    /// in that position — `0B 37` for the self record, a bare `07` for player
    /// spawns — but those are just two observed `mask2` values. A patch that set
    /// any other bit silently stopped resolving names: in the reference capture
    /// the self record carries `mask2 = 0x37` and player records `0x07`, so the
    /// `0B 37` matcher found nothing and every party member stayed as `#id`.
    /// Reading the mask bit is version-stable across that kind of change.
    ///
    /// `33 36` is the record for the character *you* are playing. It appears for
    /// exactly one entity, so it identifies the local player outright — no need
    /// for the user to have typed their character name into settings, and it
    /// works when you are already loaded into a zone (where there is no login
    /// char-list and you never see your own spawn).
    fn scan_masked_identity(&self, data: &[u8]) {
        for record in masked_records(data) {
            let id = record.id;
            let Some(sanitized) = record.name else {
                // A tutorial character's placeholder: still you, with no name,
                // and on no server the record states.
                self.data_storage.note_self_server(0);
                if self.data_storage.set_local_identity_from_game(id as i64, None) {
                    tracing::info!("self record: unnamed tutorial character -> entity {}", id);
                }
                continue;
            };
            let after = record.field.end;
            self.data_storage.note_low_id_entity(id);
            self.data_storage
                .append_nickname_authoritative(id, &sanitized);
            if let Some((server, job)) = record.profile {
                // The game's word on who you are replaces whatever name was
                // configured. That name comes from the window title or the last
                // session, and both go stale: the title does not change when a
                // new character is created, and switching character or server
                // leaves the previous name behind.
                if self
                    .data_storage
                    .set_local_identity_from_game(id as i64, Some(sanitized.clone()))
                {
                    tracing::info!("self record: local player '{}' -> entity {}", sanitized, id);
                }
                self.data_storage.note_player_server(&sanitized, server);
                self.data_storage.note_self_server(server);
                // A byte, then level (u32). Confirmed by a level-up, 28
                // then 29 (Naicha, 2026-10-04), and against the roster's
                // levels for three other players. Only in the layout whose
                // class reads as one.
                let level = job
                    .and(data.get(after + 7..after + 11))
                    .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .filter(|l| (1..=99).contains(l));
                self.data_storage.note_self_profile(&sanitized, job, level);
            } else {
                tracing::debug!("player record: '{}' -> entity {}", sanitized, id);
            }
        }
    }

    // ===== PARTY ROSTER (02 97) =====

    /// Read the party roster the server broadcasts on any party change.
    ///
    /// ```text
    /// 02 97
    /// party_key    u32
    /// party_name   str            (u8 len + utf8)
    /// party_size   u8             party max size
    /// dungeon_id   u32
    /// unnamed      u8, u8
    /// leader_dbid  u64
    /// unnamed      u8, u8, u8
    /// member_count varint
    /// member × member_count:
    ///   presence_mask u8
    ///   slot          u8          1-based party slot
    ///   dbid          u64         account-level id; high u16 is the world id
    ///   nickname      str
    ///   unnamed       u32
    ///   level         u32
    ///   gear_score    u32         equip item level
    ///   server        u16, u16
    ///   unnamed       u8
    ///   combat_power  u64
    ///   unnamed       u16, u8
    /// ```
    ///
    /// This is the only packet that states who is in your party outright, so it
    /// is the authoritative roster — and it is where combat power comes from.
    /// It joins to in-world entities by NAME: `dbid` is an account id, unrelated
    /// to the session-scoped entity ids everything else uses.
    ///
    /// Empty slots are encoded with a zero mask and an empty name, in a short
    /// record of varying width; the walk re-acquires the next member past them.
    fn scan_party_roster(&self, data: &[u8]) {
        for roster in rosters(data) {
            tracing::debug!(
                "Party roster: {} members (complete={}, dungeon={})",
                roster.members.len(),
                roster.complete,
                roster.dungeon_id
            );
            self.data_storage.set_current_dungeon(roster.dungeon_id);
            self.data_storage.set_party_roster(roster.members, roster.complete);
        }
    }

    /// Extract the player name from a `44/45 36` player spawn sub-packet.
    ///
    /// Uses the same mask-gated layout as `scan_masked_identity`
    /// (`<id varint> <mask1 u32> <mask2 u8> [mask2 & 0x01] <len><utf8>`) rather
    /// than hunting for the literal `0x07` that older builds happened to put in
    /// the `mask2` slot.
    fn parse_player_spawn_name(&self, data: &[u8], offset_after_opcode: usize) {
        let Some((actor_id, sanitized, _)) = player_spawn_at(data, offset_after_opcode) else {
            return;
        };
        // 45/44 36 player spawn is an authoritative id↔name source.
        self.data_storage.note_low_id_entity(actor_id);
        self.data_storage.note_player_spawn(actor_id);
        self.data_storage
            .append_nickname_authoritative(actor_id, &sanitized);
    }

    // ===== SUMMON PACKET (40 36) =====

    fn parse_summon_packet(&mut self, packet: &[u8]) -> bool {
        let length_info = read_varint(packet, 0);
        if length_info.length < 0 {
            return false;
        }
        let offset = length_info.length as usize;
        if offset + 1 >= packet.len() {
            return false;
        }
        if packet[offset + 1] != 0x36 {
            return false;
        }
        // Player spawn: 0x3644 pre-2026-06, 0x3645 after the June 2026 +1 shift.
        if packet[offset] == 0x44 || packet[offset] == 0x45 {
            self.parse_player_spawn_name(packet, offset + 2);
            return false;
        }
        // Mob/summon spawn: 0x3640 pre-2026-06, 0x3641 after the shift.
        if packet[offset] != 0x40 && packet[offset] != 0x41 {
            return false;
        }
        self.parse_summon_spawn_at(packet, offset + 2)
    }

    /// Parse a `41 36` spawn record (NPCs, summons/pets and transient
    /// skill-effect entities — never a real player, who gets a `45 36`/`33 36`
    /// record instead).
    ///
    /// ```text
    /// 41 36 <entity_id varint> <mask u32 LE> <subtree…> ×3 … [mask & 0x0010] <parent_key u32 LE>
    /// ```
    ///
    /// The low byte of `mask` doubles as the entity kind: `0x0C`/`0x0D` = NPC,
    /// `0x5F` = summon/pet, `0x1C` = a short-lived skill-effect entity parented
    /// to the skill's *target*. The first subtree opens with its own `mask2` byte
    /// whose bit 0 signals an inline name string (for a summon that name is the
    /// owner's), followed by the `u32` model / NPC-type id.
    ///
    /// `mask & 0x0010` declares a `parent_key`, and for a summon that is its
    /// owner's entity id — the one link that works even when nobody has been
    /// named yet. It sits past three variable-length subtrees, so instead of
    /// walking those we anchor on the owner block that directly follows it (see
    /// `find_spawn_parent_key`).
    fn parse_summon_spawn_at(&mut self, packet: &[u8], offset_after_opcode: usize) -> bool {
        let mut offset = offset_after_opcode;
        let target_info = read_varint(packet, offset);
        if target_info.length < 0 {
            return false;
        }
        offset += target_info.length as usize;

        let mut real_actor_id = target_info.value;
        if real_actor_id > 1_000_000 {
            real_actor_id = (real_actor_id & 0x3FFF) | 0x4000;
        }

        // This entity spawned via a mob/summon spawn (40/41 36), never a player
        // spawn. Record it so a summon / spell-effect that deals class-band damage
        // isn't mistaken for a player and can be attributed to its owner.
        self.data_storage.note_summon_spawn(real_actor_id);
        self.data_storage.note_low_id_entity(real_actor_id);

        if offset + 2 >= packet.len() {
            self.extract_and_register_mob_type(packet, offset, real_actor_id);
            return false;
        }
        // Read the mask as a u32 regardless of which width the server sent. The
        // two fields we test live in the low half either way — `kind` is the low
        // byte and `parent_key` is gated by bit 4 — so a wide read is correct for
        // both formats and only picks up bytes we never look at.
        let mask = u32::from_le_bytes([
            packet[offset],
            packet[offset + 1],
            *packet.get(offset + 2).unwrap_or(&0),
            *packet.get(offset + 3).unwrap_or(&0),
        ]);
        let kind = packet[offset];

        // The mask width changed from u16 to u32, which moves the subtree byte
        // that gates the inline name. Nothing else in this record is sensitive to
        // it: `find_spawn_parent_key` scans forward rather than indexing, and the
        // mob-type scan anchors on `offset`. So rather than version-sniffing the
        // stream, try both positions and keep whichever actually yields a name.
        //
        // Getting this wrong is not cosmetic. For a summon the inline name is the
        // *owner's* character name, and it is the fallback that attributes a pet's
        // damage to its player when no parent_key is present. A silently
        // mispositioned gate shows up as summons drifting back into their own rows.
        let (spawn_name, cursor) = match spawn_name_at(packet, offset) {
            Some((name, field)) => (Some(name), field.end),
            None => (None, offset + MASK_U32_SUBTREE + 1),
        };

        // Mob type / boss flag / HP still come from the existing scan, which
        // anchors on the model field this cursor now sits on.
        let code = self.extract_and_register_mob_type(packet, offset, real_actor_id);

        // A monster's summon names a player too, the one it targets: a Blazing
        // Totem linked to the player it burned, and its Burn ticks on them
        // counted as their healing (2026-10-05). The NPC table says whose it
        // can be. Only this record's code: an id keeps the code of the last
        // entity under it, and a spirit spawns with none.
        if code.is_some_and(|c| self.npc_lookup.is_no_players_summon(c)) {
            return false;
        }

        // A `0x1C` effect entity is parented to the skill's TARGET, not its
        // caster, so its parent_key must never be treated as an owner. Its name,
        // when present, IS the caster's — that is the usable link for those.
        let is_summon = kind == 0x5F;

        if is_summon
            && mask & 0x0010 != 0
            && let Some(owner_id) = self.find_spawn_parent_key(packet, cursor, real_actor_id)
        {
            self.data_storage.note_low_id_entity(owner_id);
            self.data_storage
                .register_confirmed_summon_by_id(real_actor_id, owner_id);
            tracing::debug!(
                "Summon {} linked to owner {} via parent_key",
                real_actor_id,
                owner_id
            );
            self.name_summon_owner(owner_id, spawn_name.as_deref());
            return true;
        }

        // Fall back to the name the spawn carries: summons and skill-effect
        // entities are both labelled with their caster's character name.
        if let Some(name) = &spawn_name
            && let Some(owner_id) = self.data_storage.find_id_by_nickname(name)
            && owner_id != real_actor_id
        {
            self.data_storage
                .register_confirmed_summon_by_id(real_actor_id, owner_id);
            tracing::trace!(
                "Summon {} linked to owner {} via spawn name '{}'",
                real_actor_id,
                owner_id,
                name
            );
            return true;
        }

        // Last resort for spirits: the caster recorded on the entity's own buff
        // block, which is its owner. It reads back as the spirit itself for
        // some skills, hence `!= self`. Other players' spirits (`0x1F`, `0x1D`,
        // `0x5D`) spawn with no parent_key and no name, so this is their link
        // at spawn. Checked against the spirit/owner link records in five
        // captures (2026-10-04): 1,292 of 1,295 spirit spawns named the right
        // owner, none a wrong one, the rest none; mobs and effect entities
        // read back as themselves.
        if matches!(kind, 0x5F | 0x1F | 0x1D | 0x5D) {
            let owner_id = self.extract_summon_owner_from_spawn(packet, offset);
            if owner_id > 0 && owner_id != real_actor_id {
                self.data_storage
                    .register_confirmed_summon_by_id(real_actor_id, owner_id);
                return true;
            }
        }

        // A Sorcerer's lingering ground spell (Cold Storm, Bittercold Wind) also
        // spawns as `0x1F` (some as `0x5F`), but its buff block names the spell
        // itself. Its caster follows the spawn position as `07 02 06` or
        // `07 02 01` and a `u32`. Over every 0x1F/0x5F spawn in the check kit's
        // captures that deals class damage, this matched the existing link 2,759
        // times, added 4 (each a same-class player) and was wrong 0 times.
        if matches!(kind, 0x1F | 0x5F)
            && let Some(caster) = self.find_effect_caster(packet, offset, real_actor_id)
        {
            self.data_storage.note_low_id_entity(caster);
            self.data_storage
                .register_confirmed_summon_by_id(real_actor_id, caster);
            if kind == 0x5F {
                self.name_summon_owner(caster, spawn_name.as_deref());
            }
            return true;
        }

        false
    }

    /// A summon's (`0x5F`) spawn states its owner twice, by name and by
    /// entity id, so it names a player whose own spawn the meter missed.
    ///
    /// A player is named by their spawn (`45 36`) when they come into view,
    /// and you by your self record on a zone load. A meter started, or a
    /// capture begun, inside an instance has missed both, and a party member
    /// stays `#id` until something re-sends them: replaying a 2026-10-09
    /// dungeon run from just before its last boss, a Ranger's pets named
    /// her (entity 15961) from the first second, and her spawn came two
    /// minutes into the fight. Over the check kit's captures and that run,
    /// 329 summon spawns read this way, and each named its owner as the
    /// game's own records for that id did.
    ///
    /// Only for a summon. A `0x1F` effect's caster field can be the boss
    /// its mechanic hangs on, while its name is the player it targets: a
    /// boss whose own spawn was missed would have taken each party
    /// member's name in turn. A name the game stated elsewhere is never
    /// replaced (`append_nickname`'s gate).
    fn name_summon_owner(&self, owner_id: i32, name: Option<&str>) {
        let Some(name) = name else { return };
        if self.data_storage.has_nickname(owner_id) || self.data_storage.is_mob(owner_id) {
            return;
        }
        tracing::info!("Summon spawn: owner {} is '{}'", owner_id, name);
        self.data_storage.append_nickname(owner_id, name);
    }

    /// The caster of a ground spell: the `u32` after the `07 02 06` or
    /// `07 02 01` that follows the spawn position. Never the spell itself or a
    /// known mob.
    fn find_effect_caster(&self, packet: &[u8], start_offset: usize, self_id: i32) -> Option<i32> {
        let end = packet.len().min(start_offset + 240);
        let at = packet
            .get(start_offset..end)?
            .windows(3)
            .position(|w| w[0] == 0x07 && w[1] == 0x02 && (w[2] == 0x06 || w[2] == 0x01))?;
        let i = start_offset + at + 3;
        let caster = i32::from_le_bytes(packet.get(i..i + 4)?.try_into().ok()?);
        ((100..=9_999_999).contains(&caster) && caster != self_id && !self.data_storage.is_mob(caster))
            .then_some(caster)
    }

    /// Find the `parent_key` a `41 36` spawn declares via `mask & 0x0010`.
    ///
    /// The field sits behind three variable-length subtrees that are impractical
    /// to walk, so we anchor on the owner block that immediately follows it:
    ///
    /// ```text
    /// <parent_key u32 LE> <legion_id u32> <u16 = 0> <u16 server_id> <len u8> <utf8 legion name>
    /// ```
    ///
    /// preceded by the record's `mask & 0x0004` byte (constant `0x06`). Six
    /// independent constraints have to line up at once, which is why this pinned
    /// the owner on 81 of 81 summons in the reference capture with no false
    /// positives — including a Spiritmaster's 54 pets whose owner had never been
    /// named at the time they spawned.
    fn find_spawn_parent_key(&self, packet: &[u8], search_from: usize, self_id: i32) -> Option<i32> {
        let mut i = search_from.max(1);
        while i + 13 <= packet.len() {
            if packet[i - 1] == 0x06
                && let Some(parent) = parse_spawn_owner_block(packet, i, self_id)
            {
                return Some(parent);
            }
            i += 1;
        }
        None
    }

    fn extract_summon_owner_from_spawn(&self, packet: &[u8], start_offset: usize) -> i32 {
        let anchor: [u8; 8] = [0x80, 0x75, 0xD5, 0x2A, 0xBB, 0x03, 0x00, 0x00];
        let max_search = std::cmp::min(packet.len().saturating_sub(anchor.len()), start_offset + 120);
        for i in start_offset..=max_search {
            if packet[i..].starts_with(&anchor) {
                let owner_info = read_varint(packet, i + anchor.len());
                // Low ids are real (see `is_plausible_entity_id`).
                if owner_info.length > 0 && (1..=9_999_999).contains(&owner_info.value) {
                    return owner_info.value;
                }
            }
        }
        -1
    }

    /// The NPC code this record names, if it names one.
    fn extract_and_register_mob_type(&self, packet: &[u8], start_offset: usize, real_actor_id: i32) -> Option<i32> {
        let mut scan_offset = start_offset;
        let max_scan = std::cmp::min(packet.len().saturating_sub(2), start_offset + 60);

        while scan_offset < max_scan {
            if packet[scan_offset] == 0x00
                && (packet[scan_offset + 1] == 0x40 || packet[scan_offset + 1] == 0x00)
                && packet[scan_offset + 2] == 0x02
            {
                if scan_offset >= start_offset + 3 {
                    let b1 = packet[scan_offset - 3] as i32;
                    let b2 = packet[scan_offset - 2] as i32;
                    let b3 = packet[scan_offset - 1] as i32;
                    let mob_type_id = b1 | (b2 << 8) | (b3 << 16);
                    self.data_storage.append_mob(real_actor_id, mob_type_id);

                    // Register boss entities from NPC DB
                    if self.npc_lookup.is_boss(mob_type_id) {
                        self.data_storage.register_boss(real_actor_id);
                    }
                    if self.npc_lookup.is_training_dummy(mob_type_id) {
                        self.data_storage.register_training_dummy(real_actor_id);
                    }

                    // Try to extract HP
                    let mut hp_scan = scan_offset + 3;
                    let hp_end = std::cmp::min(packet.len().saturating_sub(2), hp_scan + 64);
                    while hp_scan < hp_end {
                        if packet[hp_scan] == 0x01 {
                            let current_hp = read_varint(packet, hp_scan + 1);
                            if current_hp.length > 0 && current_hp.value > 0 {
                                let max_hp = read_varint(packet, hp_scan + 1 + current_hp.length as usize);
                                if max_hp.length > 0 && max_hp.value >= current_hp.value {
                                    self.data_storage.append_mob_hp(real_actor_id, max_hp.value);
                                    break;
                                }
                            }
                        }
                        hp_scan += 1;
                    }
                    return Some(mob_type_id);
                }
                break;
            }
            scan_offset += 1;
        }
        None
    }

    // ===== ACTOR NAME BINDING =====

    fn parse_actor_name_binding_rules(&self, packet: &[u8]) -> bool {
        actor_name_fields(packet)
            .into_iter()
            .any(|(actor_id, name_start, name_length)| self.register_utf8_nickname(packet, actor_id, name_start, name_length))
    }

    fn register_utf8_nickname(&self, packet: &[u8], actor_id: i32, name_start: usize, name_length: usize) -> bool {
        if self.data_storage.has_nickname(actor_id) {
            return false;
        }
        if self.data_storage.is_summon(actor_id) {
            return false;
        }
        if name_length == 0 || name_length > 36 {
            return false;
        }
        let name_end = name_start + name_length;
        if name_end > packet.len() {
            return false;
        }
        let name_bytes = &packet[name_start..name_end];
        let name = match std::str::from_utf8(name_bytes) {
            Ok(s) => s,
            Err(_) => return false,
        };
        let sanitized = match sanitize_nickname(name) {
            Some(s) => s,
            None => return false,
        };
        self.data_storage.append_nickname(actor_id, &sanitized);
        true
    }

    // ===== LOOT ATTRIBUTION ACTOR NAME =====

    fn parse_loot_attribution_actor_name(&self, packet: &[u8]) -> bool {
        let mut candidates: std::collections::HashMap<i32, (String, Vec<u8>)> = std::collections::HashMap::new();
        for (actor_id, sanitized, field) in loot_actor_fields(packet) {
            let name_bytes = &packet[field];
            let existing = candidates.get(&actor_id);
            if existing.is_none() || name_bytes.len() > existing.unwrap().1.len() {
                candidates.insert(actor_id, (sanitized, name_bytes.to_vec()));
            }
        }

        if candidates.is_empty() {
            return false;
        }

        let mut found_any = false;
        let allow_prepopulate = candidates.len() > 1;

        for (actor_id, (name, _)) in &candidates {
            let existing = self.data_storage.get_nickname(*actor_id);
            let has_cjk = name.chars().any(|ch| {
                matches!(unicode_script(ch), UnicodeScript::Han | UnicodeScript::Hangul)
            });

            if !allow_prepopulate && !self.data_storage.actor_appears_in_combat(*actor_id) && !has_cjk {
                if existing.is_none() {
                    self.data_storage.cache_pending_nickname(*actor_id, name);
                }
                continue;
            }

            if existing.is_some() {
                continue;
            }

            self.data_storage.append_nickname(*actor_id, name);
            found_any = true;
        }

        found_any
    }

    // ===== NICKNAME PARSING =====

    fn parsing_nickname(&self, packet: &[u8]) -> bool {
        let fields = nickname_fields(packet);
        for (id, name, _) in &fields {
            self.data_storage.append_nickname(*id, name);
        }
        !fields.is_empty()
    }

    // ===== EMBEDDED DAMAGE PACKET =====

    fn try_parse_embedded_damage_packet(&mut self, packet: &[u8]) -> bool {
        if packet.len() < 6 {
            return false;
        }
        let mut parsed_any = false;
        let mut search_offset = 0;

        while search_offset + 1 < packet.len() {
            if packet[search_offset] != 0x04 || packet[search_offset + 1] != 0x38 {
                search_offset += 1;
                continue;
            }

            let _remaining_size = packet.len() - search_offset;
            let raw_key = to_hex_range(packet, search_offset, std::cmp::min(search_offset + 64, packet.len()));
            if self.seen_embedded_hexes.contains(&raw_key) {
                search_offset += 1;
                continue;
            }

            let mut headless = vec![0xFF, 0x01];
            headless.extend_from_slice(&packet[search_offset..]);

            if self.parsing_damage_inner(&headless, false, true) {
                self.seen_embedded_hexes.insert(raw_key);
                parsed_any = true;
                search_offset += 2;
            } else {
                search_offset += 1;
            }
        }
        parsed_any
    }

    // ===== DAMAGE PARSING =====

    fn parsing_damage(&mut self, packet: &[u8], allow_embedded_scan: bool, require_trusted: bool) -> bool {
        self.parsing_damage_inner(packet, allow_embedded_scan, require_trusted)
    }

    fn parsing_damage_inner(&mut self, packet: &[u8], allow_embedded_scan: bool, require_trusted: bool) -> bool {
        let length_info = read_varint(packet, 0);
        if length_info.length < 0 {
            return false;
        }
        let mut offset = length_info.length as usize;

        if offset >= packet.len() || offset + 1 >= packet.len() {
            return false;
        }

        // STRICT GATEKEEPER: 04 38
        if packet[offset] != 0x04 || packet[offset + 1] != 0x38 {
            if allow_embedded_scan {
                return self.try_parse_embedded_damage_packet(packet);
            }
            return false;
        }
        offset += 2;

        let mut parsed_any = false;
        let mask = 0x0F;

        while offset < packet.len() {
            let _checkpoint = offset;

            // Chained hit marker
            let mut is_chained = false;
            if offset + 1 < packet.len() && packet[offset] == 0x01 && packet[offset + 1] == 0x00 {
                offset += 2;
                is_chained = true;
            }

            if parsed_any && !is_chained {
                break;
            }

            // Target. `>= 100` is a resync gate for the varint walk, not a real
            // protocol bound — the game does hand out sub-100 entity ids, and a
            // player who draws one had every hit they took (and dealt, below)
            // silently dropped. Ids a spawn or identity record has confirmed are
            // let through; unconfirmed small values still bail out, so the gate
            // keeps doing its job.
            let target_value = match try_read_varint(packet, &mut offset) {
                Some(v) if self.data_storage.is_plausible_entity_id(v) => v,
                _ => { break; }
            };

            // Switch value
            let switch_value = match try_read_varint(packet, &mut offset) {
                Some(v) => v,
                None => break,
            };
            let and_result = switch_value & mask;
            // Switch bit 0x04: the record has a value. Without it, the hit
            // type says why: a miss or a resist, read below and counted.
            let no_value = matches!(and_result, 0 | 2);

            if !(4..=7).contains(&and_result) && !no_value {
                break;
            }

            // Unused flag
            if try_read_varint(packet, &mut offset).is_none() { break; }

            // Actor (same gate as the target above).
            let actor_value = match try_read_varint(packet, &mut offset) {
                Some(v) if self.data_storage.is_plausible_entity_id(v) => v,
                _ => { break; }
            };

            // Exact 4-byte skill ID
            if offset + 4 > packet.len() {
                break;
            }
            let mut exact_skill_code = i64::from(packet[offset] as u32)
                | (i64::from(packet[offset + 1] as u32) << 8)
                | (i64::from(packet[offset + 2] as u32) << 16)
                | (i64::from(packet[offset + 3] as u32) << 24);
            offset += 4;

            // Theostone raw item IDs
            if (3_000_000..=3_099_999).contains(&exact_skill_code) {
                exact_skill_code = exact_skill_code * 10 + 1;
            }

            if !(1..=299_999_999).contains(&exact_skill_code) {
                break;
            }

            // Skip 7-digit NPC skills
            if (1_000_000..=9_999_999).contains(&exact_skill_code) {
                break;
            }

            // Skip 1-byte UID field
            if offset < packet.len() {
                offset += 1;
            }

            let dummy_type = match try_read_varint(packet, &mut offset) {
                Some(v) => v,
                None => break,
            };
            let damage_type = dummy_type as u8;

            // Hit type 1 (Miss) and 6 (Resist) carry no damage: count them on
            // the skill and stop here, as the parser always did on these.
            if no_value {
                if !require_trusted && actor_value != target_value {
                    if let Some(kind) = NoDamageHit::from_hit_type(dummy_type) {
                        let skill = self.normalize_skill_id(exact_skill_code as i32);
                        self.data_storage.append_no_damage_hit(target_value, actor_value, skill, kind);
                    }
                }
                break;
            }

            let temp_v: usize = match and_result {
                5 => 12,
                6 => 10,
                7 => 14,
                _ => 8,
            };

            // Switch bit 0x02: the game's damage plotter follows the hit type,
            // `<flags byte> <restoration HP varint> <angle byte>`. The HP is
            // two bytes from 128 up; `temp_v` counts it as one.
            let mut specials = Vec::new();
            let mut plotter_extra = 0;
            if and_result & 0x02 != 0 {
                let Some(plotter) = read_plotter(packet, offset) else { break };
                // Flags byte, bit for bit the game's plotter fields. Verified
                // per skill against the game's Damage Analyzer: Perfect 0x04,
                // Double 0x08 (the game's HardHit, 강타). From the 2026-10-04
                // captures (hit sizes, issue #5): Shield Block 0x01, Parry 0x02,
                // Iron Wall 0x10, Regeneration 0x20, Perfect Block 0x40. 0x80
                // is not a plotter field: it mirrors switch bit 0x10 and is
                // fixed per skill.
                specials = special_damage::from_hit_flags(plotter.flags);
                // Angle byte. Verified against the combat log and the game's
                // Damage Analyzer (BackAttackCount, FrontAttackCount per skill):
                // 0x00 = no positional tag, 0x01 = Back, 0x02 = Front.
                match plotter.angle {
                    Some(0x01) => specials.push(SpecialDamage::Back),
                    Some(0x02) => specials.push(SpecialDamage::Frontal),
                    _ => {}
                }
                plotter_extra = plotter.hp_len - 1;
            }
            if damage_type == 3 {
                specials.push(SpecialDamage::Critical);
            }

            offset += temp_v + plotter_extra;
            if offset >= packet.len() {
                break;
            }

            // Struct data extraction
            let mut first_value = match try_read_varint(packet, &mut offset) {
                Some(v) => v,
                None => break,
            };
            let mut after_first_offset = offset;
            let mut second_value = match try_read_varint(packet, &mut offset) {
                Some(v) => v,
                None => break,
            };

            // Post-2026-06 layout shift: these records now carry a leading zero
            // pad plus a POWER SCALAR ahead of the real value, so the damage lands
            // one varint later than the parser historically expected. A
            // `first_value` of 0 is that pad — realign by one varint (first <- the
            // scalar, second <- the real value). Verified live: a Power Burst crit
            // read the scalar instead of its true 60876, which sits in this next
            // varint.
            //
            // The scalar is not a constant marker (as this once assumed): it is
            // the actor's damage multiplier in hundredths of a percent — mobs read
            // 10000 (= 100.00%), geared players 16000-22000 — and it shifts with
            // buffs. Crucially a summon inherits its OWNER's value, which is what
            // `note_power_scalar` records it for; see
            // `DpsCalculator::infer_summon_owners`.
            if first_value == 0 {
                let after_second_offset = offset;
                if let Some(third) = try_read_varint(packet, &mut offset) {
                    first_value = second_value;
                    after_first_offset = after_second_offset;
                    second_value = third;
                }
            }

            let first_is_damage = should_treat_first_value_as_damage(first_value, second_value, and_result, damage_type as i32);

            // When the damage is in `second_value`, `first_value` is the actor's
            // power scalar (see above). Recorded per actor so a summon whose spawn
            // packet never arrived can still be tied to its owner.
            if !first_is_damage && (1_000..=200_000).contains(&first_value) {
                self.data_storage.note_power_scalar(actor_value, first_value);
            }

            let mut final_damage = if first_is_damage {
                offset = after_first_offset;
                first_value
            } else {
                second_value
            };

            // The tail after the value, checked against the game's own Damage
            // Analyzer record of the same fight (2026-10-04): layout 4 carries
            // one varint, then switch bit 0x20 marks a hit that triggered
            // additional hits, as a count and that many damage values which the
            // value above already includes. Records that do not end cleanly
            // this way keep the older reading below.
            let strict_tail = if [4, 6].contains(&and_result) && exact_skill_code != 99_745_942 {
                parse_hit_tail(packet, offset, and_result, switch_value, final_damage)
            } else {
                None
            };

            // Multi-hit extra field
            if strict_tail.is_none() && (switch_value & 0x30) == 0x30 && offset < packet.len() {
                try_read_varint(packet, &mut offset);
            }

            let mut hit_count = 0;
            let pre_hit_offset = offset;

            if strict_tail.is_none() && offset < packet.len() {
                let is_marker_next = offset + 1 < packet.len()
                    && packet[offset + 1] == 0x00
                    && (1..=7).contains(&(packet[offset] as i32));

                if !is_marker_next {
                    if let Some(peek_val) = try_read_varint(packet, &mut offset) {
                        if (0..=25).contains(&peek_val) {
                            hit_count = peek_val;
                        } else {
                            let is_marker_after = offset + 1 < packet.len()
                                && packet[offset + 1] == 0x00
                                && (1..=7).contains(&(packet[offset] as i32));
                            if !is_marker_after {
                                if let Some(actual) = try_read_varint(packet, &mut offset) {
                                    if (0..=25).contains(&actual) {
                                        hit_count = actual;
                                    } else {
                                        offset = pre_hit_offset;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if final_damage < 0 || final_damage > 99_999_999 {
                break;
            }

            // Extract multi-hits
            let mut multi_hit_count = 0;
            let mut multi_hit_damage = 0;
            let mut first_multi_hit_value: Option<i32> = None;
            let mut all_multi_hits_match = true;

            if strict_tail.is_none() && hit_count > 0 && offset < packet.len() {
                let safe_max = std::cmp::min(hit_count, 25);
                let multi_hit_cap = std::cmp::max(final_damage, 500_000);
                let mut hits_read = 0;

                while hits_read < safe_max && offset < packet.len() {
                    let is_marker_next = offset + 1 < packet.len()
                        && packet[offset + 1] == 0x00
                        && (1..=7).contains(&(packet[offset] as i32));
                    let is_next_packet = offset + 1 < packet.len()
                        && packet[offset] == 0x04
                        && packet[offset + 1] == 0x38;

                    if is_marker_next || is_next_packet {
                        break;
                    }

                    let hit_value = match try_read_varint(packet, &mut offset) {
                        Some(v) => v,
                        None => break,
                    };

                    if hit_value > multi_hit_cap || hit_value < 50 {
                        multi_hit_damage = 0;
                        first_multi_hit_value = None;
                        all_multi_hits_match = true;
                        break;
                    }

                    match first_multi_hit_value {
                        None => first_multi_hit_value = Some(hit_value),
                        Some(fv) if fv != hit_value => all_multi_hits_match = false,
                        _ => {}
                    }

                    multi_hit_damage += hit_value;
                    hits_read += 1;
                }
                multi_hit_count = hits_read;
            }

            if strict_tail.is_none() && switch_value == 54 && hit_count > multi_hit_count && multi_hit_count == 1 {
                if let Some(fv) = first_multi_hit_value {
                    if all_multi_hits_match {
                        multi_hit_count = hit_count;
                        multi_hit_damage = fv * hit_count;
                    }
                }
            }

            if strict_tail.is_none() && should_use_repeated_hit_damage(switch_value, second_value, multi_hit_count, first_multi_hit_value, all_multi_hits_match) {
                final_damage = first_multi_hit_value.unwrap();
            }

            if let Some((end, field, count, damage)) = strict_tail {
                offset = end;
                hit_count = field;
                multi_hit_count = count;
                multi_hit_damage = damage;
            }

            if multi_hit_count > 0 && multi_hit_damage > 0 && final_damage > multi_hit_damage {
                final_damage -= multi_hit_damage;
            }

            // Compact skill context handling
            let pending = self.pending_compact_skill_context.clone();
            let aggregated_compact = pending.as_ref().is_some_and(|ctx| {
                exact_skill_code as i32 == 99_745_942
                    && actor_value == ctx.actor_id
                    && hit_count > 1
                    && multi_hit_damage > 0
                    && second_value > multi_hit_damage
            });

            let raw_for_spec = if aggregated_compact {
                pending.as_ref().unwrap().skill_raw
            } else {
                exact_skill_code as i32
            };
            let spec_flags = decode_spec_flags(raw_for_spec);
            let resolved_skill_code = if aggregated_compact {
                pending.as_ref().unwrap().skill_raw
            } else {
                self.normalize_skill_id(exact_skill_code as i32)
            };

            if aggregated_compact {
                final_damage = second_value - multi_hit_damage;
                self.pending_compact_skill_context = None;
            }

            // Heal/life-steal suffix: [0x03, 0x00] marker + HealAmount VarInt
            let mut heal_amount = 0;
            if offset + 1 < packet.len()
                && packet[offset] == 0x03
                && packet[offset + 1] == 0x00
            {
                offset += 2;
                if let Some(heal_val) = try_read_varint(packet, &mut offset) {
                    if heal_val > 0 && heal_val < 10_000_000 {
                        heal_amount = heal_val;
                    }
                }
            }

            if require_trusted && !self.is_trusted_recovered_damage_shape(actor_value, target_value, dummy_type as u8, final_damage, resolved_skill_code) {
                break;
            }

            if crate::entity::skill_group::restores_resource(exact_skill_code as i32) {
                // MP (or another resource) restored, not HP: neither damage
                // nor healing. A Water Spirit's attack sends one of these to
                // its Spiritmaster (16990002, 20 MP), filed under 100011.
            } else if actor_value != target_value {
                let mut pdp = ParsedDamagePacket::new();
                if let Some(ts) = self.override_timestamp {
                    pdp.set_timestamp(ts);
                }
                pdp.set_target_id(target_value);
                pdp.set_actor_id(actor_value);
                pdp.set_skill_code(resolved_skill_code);
                pdp.set_spec_flags(spec_flags);
                pdp.set_type(dummy_type);
                pdp.set_specials(specials);
                pdp.set_multi_hit_count(multi_hit_count);
                pdp.set_multi_hit_damage(multi_hit_damage);
                pdp.set_heal_amount(heal_amount);
                pdp.set_damage(final_damage);
                pdp.set_hex_payload(to_hex(packet));

                self.data_storage.append_damage(pdp);
            } else if final_damage > 1 && self.data_storage.is_known_player(actor_value) {
                // Self-cast `04 38` record from a known player: an instant SELF-HEAL
                // (Radiant Recovery / Absolution / Healing Light etc.). The general
                // parser drops actor==target as self-damage, but for these records the
                // `E6 6F` field is read as first_value and the real heal lands in
                // second_value, so `final_damage` is the correct heal amount. Recording
                // it as healing makes the HEAL view capture instant self-heals, not just
                // HoTs. (The cast-marker variant breaks out earlier on its and_result.)
                self.data_storage.append_heal(
                    actor_value,
                    resolved_skill_code,
                    final_damage as i64,
                    false,
                    self.override_timestamp.unwrap_or_else(crate::clock::now_ms),
                );
            }

            parsed_any = true;
        }

        parsed_any
    }

    fn extract_pending_compact_skill_context(&self, packet: &[u8]) -> Option<PendingCompactSkillContext> {
        let length_info = read_varint(packet, 0);
        if length_info.length <= 0 || length_info.length as usize >= packet.len() {
            return None;
        }
        let body = &packet[length_info.length as usize..];

        // Find marker: 08 3B/3D 38 00 00
        let mut marker_index: Option<usize> = None;
        for idx in 0..body.len().saturating_sub(4) {
            if body[idx] == 0x08
                && (body[idx + 1] == 0x3B || body[idx + 1] == 0x3D)
                && body[idx + 2] == 0x38
                && body[idx + 3] == 0x00
                && body[idx + 4] == 0x00
            {
                marker_index = Some(idx);
                break;
            }
        }
        let marker_index = marker_index?;

        // Find compact opcode 38
        let mut compact_opcode: Option<usize> = None;
        for idx in (marker_index + 5)..body.len() {
            if body[idx] == 0x38 {
                compact_opcode = Some(idx);
                break;
            }
        }
        let compact_opcode = compact_opcode?;
        if compact_opcode + 2 >= body.len() {
            return None;
        }

        let actor_info = read_varint(body, compact_opcode + 1);
        if actor_info.length <= 0 || actor_info.value < 100 {
            return None;
        }

        let uid_offset = compact_opcode + 1 + actor_info.length as usize;
        if uid_offset >= body.len() {
            return None;
        }
        let skill_offset = uid_offset + 1;
        if skill_offset + 3 > body.len() {
            return None;
        }

        let mut candidates = Vec::new();
        if skill_offset + 4 <= body.len() {
            let full_skill = parse_u32_le(body, skill_offset) as i32;
            candidates.push(full_skill);
        }
        let compact_skill = (body[skill_offset] as i32)
            | ((body[skill_offset + 1] as i32) << 8)
            | ((body[skill_offset + 2] as i32) << 16);
        candidates.push(compact_skill);

        for candidate in candidates {
            if self.is_known_skill_code(candidate) {
                return Some(PendingCompactSkillContext {
                    actor_id: actor_info.value,
                    skill_raw: self.normalize_skill_id(candidate),
                });
            }
        }
        None
    }

    // ===== HELPERS =====

    fn normalize_skill_id(&self, raw: i32) -> i32 {
        if (30_000_000..=30_999_999).contains(&raw) {
            return raw;
        }
        let base = raw - (raw % 10000);
        let base_name = self.skill_lookup.get_skill_name(base);
        if base_name.is_empty() {
            return raw;
        }
        let raw_name = self.skill_lookup.get_skill_name(raw);
        if raw_name.is_empty() {
            return base;
        }
        if raw_name != base_name {
            return raw;
        }
        base
    }

    fn is_known_skill_code(&self, skill_code: i32) -> bool {
        if !is_valid_skill_code(skill_code) {
            return false;
        }
        let normalized = self.normalize_skill_id(skill_code);
        if !is_valid_skill_code(normalized) {
            return false;
        }
        if (30_000_000..=30_999_999).contains(&normalized) {
            return true;
        }
        !self.skill_lookup.get_skill_name(normalized).is_empty()
            || !self.skill_lookup.get_skill_name(skill_code).is_empty()
    }

    fn is_trusted_recovered_damage_shape(&self, actor_id: i32, target_id: i32, damage_type: u8, damage: i32, skill_code: i32) -> bool {
        if actor_id == target_id || !(1..=3).contains(&(damage_type as i32)) || damage <= 0 {
            return false;
        }
        self.is_known_skill_code(skill_code)
    }
}

// ===== FREE FUNCTIONS =====

/// The varint that ends just before `end`, starting no earlier than
/// `min_start` and at most three bytes back, whose value is in `range`.
///
/// The last byte of a multi-byte varint has its high bit clear, so it is also
/// a valid one-byte varint on its own: entity 13978 is `9A 6D`, and `6D`
/// alone is 109. Read shortest first, every id from 12,800 up came out as
/// `id >> 7`, which still passes a range check, so names and loot went to the
/// wrong entity (issue #10). A varint cannot start right after a byte with its
/// high bit set, since that byte would continue into it, so the candidate not
/// preceded by one wins; the shortest valid one is only the fallback.
pub fn varint_ending_at(
    data: &[u8],
    end: usize,
    min_start: usize,
    range: std::ops::RangeInclusive<i32>,
) -> Option<i32> {
    let mut fallback = None;
    for v_len in 1..=3usize {
        let Some(v_start) = end.checked_sub(v_len) else { break };
        if v_start < min_start || !can_read_varint(data, v_start) {
            continue;
        }
        let v = read_varint(data, v_start);
        if v.length != v_len as i32 || !range.contains(&v.value) {
            continue;
        }
        let continued = v_start > 0 && data[v_start - 1] & 0x80 != 0;
        if !continued {
            return Some(v.value);
        }
        fallback.get_or_insert(v.value);
    }
    fallback
}

pub fn read_varint(bytes: &[u8], offset: usize) -> VarIntResult {
    let mut value: i32 = 0;
    let mut shift = 0;
    let mut count = 0;

    loop {
        if offset + count >= bytes.len() {
            return VarIntResult::invalid();
        }

        let byte_val = bytes[offset + count] as u32;
        count += 1;

        value |= ((byte_val & 0x7F) as i32) << shift;

        if byte_val & 0x80 == 0 {
            return VarIntResult { value, length: count as i32 };
        }

        shift += 7;
        if shift >= 32 {
            return VarIntResult::invalid();
        }
    }
}

fn try_read_varint(bytes: &[u8], offset: &mut usize) -> Option<i32> {
    let result = read_varint(bytes, *offset);
    if result.length <= 0 {
        return None;
    }
    *offset += result.length as usize;
    if result.value < 0 { None } else { Some(result.value) }
}

fn can_read_varint(bytes: &[u8], offset: usize) -> bool {
    if offset >= bytes.len() {
        return false;
    }
    let mut idx = offset;
    let mut count = 0;
    while idx < bytes.len() && count < 5 {
        let byte_val = bytes[idx] as u32;
        if byte_val & 0x80 == 0 {
            return true;
        }
        idx += 1;
        count += 1;
    }
    false
}

fn parse_u32_le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset], data[offset + 1], data[offset + 2], data[offset + 3],
    ])
}

fn is_valid_skill_code(skill_code: i32) -> bool {
    (1..=299_999_999).contains(&skill_code)
}

fn decode_spec_flags(raw: i32) -> [bool; 5] {
    let mut result = [false; 5];
    if (30_000_000..=30_999_999).contains(&raw) {
        return result;
    }
    let mut suffix = (raw % 10000) / 10;
    if suffix <= 0 {
        return result;
    }
    while suffix > 0 {
        let slot = suffix % 10;
        if slot < 1 || slot > 5 {
            return [false; 5];
        }
        result[(slot - 1) as usize] = true;
        suffix /= 10;
    }
    result
}

fn should_treat_first_value_as_damage(first_value: i32, second_value: i32, and_result: i32, damage_type: i32) -> bool {
    if !(1_000..=99_999_999).contains(&first_value) { return false; }
    if !(0..=25).contains(&second_value) { return false; }
    if first_value > 5_000_000 { return false; }
    and_result == 6 && damage_type == 3
}

/// The tail of a damage record after its value, when it has the shape the
/// game's own record confirms: `(end offset, layout-4 field, additional hits,
/// their damage)`. `None` when the bytes do not end cleanly at the next record.
///
/// A spirit's layout-4 record has no layout-4 field: the additional hits follow
/// the value directly (2026-10-04, the game's AdditionalHitCount per spirit
/// skill matches only when read this way). A player's record has the field.
fn parse_hit_tail(packet: &[u8], offset: usize, layout: i32, switch_value: i32, value: i32) -> Option<(usize, i32, i32, i32)> {
    parse_hit_tail_as(packet, offset, layout, switch_value, value)
        .or_else(|| (layout == 4).then(|| parse_hit_tail_as(packet, offset, 6, switch_value, value)).flatten())
}

fn parse_hit_tail_as(packet: &[u8], mut offset: usize, layout: i32, switch_value: i32, value: i32) -> Option<(usize, i32, i32, i32)> {
    let mut field = 0;
    if layout == 4 {
        field = try_read_varint(packet, &mut offset)?;
        if !(1..=25).contains(&field) {
            return None;
        }
    }
    let (mut count, mut damage) = (0, 0i64);
    if switch_value & 0x20 != 0 {
        count = try_read_varint(packet, &mut offset)?;
        if !(1..=25).contains(&count) {
            return None;
        }
        for _ in 0..count {
            let hit = try_read_varint(packet, &mut offset)?;
            if hit < 0 {
                return None;
            }
            damage += i64::from(hit);
        }
        if damage >= i64::from(value) {
            return None;
        }
    }
    let rest = &packet[offset.min(packet.len())..];
    let clean_end = rest.is_empty()
        || (rest.len() >= 2 && rest[1] == 0x00 && (1..=7).contains(&rest[0]))
        || rest.starts_with(&[0x04, 0x38]);
    clean_end.then_some((offset, field, count, damage as i32))
}

/// The damage plotter after a record's hit type: the flags byte, the HP a
/// Regeneration hit restored, and the angle byte (absent at the very end).
struct Plotter {
    flags: u8,
    // Only its length is used for now; the value is checked by the tests.
    #[cfg_attr(not(test), allow(dead_code))]
    hp: i32,
    hp_len: usize,
    angle: Option<u8>,
}

fn read_plotter(packet: &[u8], offset: usize) -> Option<Plotter> {
    let flags = *packet.get(offset)?;
    let mut at = offset + 1;
    let hp = try_read_varint(packet, &mut at)?;
    Some(Plotter { flags, hp, hp_len: at - offset - 1, angle: packet.get(at).copied() })
}

fn should_use_repeated_hit_damage(switch_value: i32, encoded_damage: i32, multi_hit_count: i32, first_multi_hit_value: Option<i32>, all_match: bool) -> bool {
    let repeated = match first_multi_hit_value {
        Some(v) => v,
        None => return false,
    };
    if switch_value != 54 { return false; }
    if multi_hit_count <= 0 || !all_match { return false; }
    let main_component = encoded_damage - multi_hit_count * repeated;
    if main_component > repeated { return false; }
    encoded_damage / 10 == repeated
}

/// Decode the body of a `02 97` party roster packet. `at` is the first byte
/// after the opcode. Returns the members that parsed cleanly, or `None` if the
/// header does not look like a roster (the opcode is scanned for in raw byte
/// streams, so the header checks double as the false-positive filter).
/// See `StreamProcessor::scan_party_roster` for the layout.
fn parse_party_roster_at(data: &[u8], at: usize) -> Option<Roster> {
    use crate::combat::data_storage::PartyMember;

    let mut o = at.checked_add(4)?; // party_key u32
    let name_len = *data.get(o)? as usize;
    o += 1;
    if !(1..=40).contains(&name_len) {
        return None;
    }
    std::str::from_utf8(data.get(o..o + name_len)?).ok()?;
    o += name_len;

    let party_size = *data.get(o)? as usize;
    o += 1;
    if !(1..=12).contains(&party_size) {
        return None;
    }
    // The instance the party is queued for / inside. Identifies both the dungeon
    // and its difficulty tier: Ferocious Horn Den is 600091/600092/600093 for
    // Exploration / Conquest [Normal] / Conquest [Hard].
    let dungeon_id = parse_u32_le(data.get(o..o + 4)?, 0) as i32;
    o += 4 + 2 + 8 + 3; // dungeon_id, 2 pad, leader_dbid, 3 pad
    let count_info = read_varint(data, o);
    if count_info.length <= 0 || !(1..=12).contains(&count_info.value) {
        return None;
    }
    o += count_info.length as usize;

    let count = count_info.value;
    let mut members = Vec::new();
    let mut name_fields = Vec::new();
    let mut complete = false;
    for _ in 0..count {
        if o + 20 > data.len() {
            break;
        }
        let slot = data[o + 1];
        o += 2; // presence_mask, slot
        let dbid = u64::from_le_bytes(data.get(o..o + 8)?.try_into().ok()?);
        o += 8;
        let server_id = (dbid >> 48) as u16;
        let nick_len = *data.get(o)? as usize;
        o += 1;
        // An empty name is a vacant slot. Vacant slots can sit between members
        // (slots 1 and 5 filled, 2-4 empty), so look past them for a later one;
        // when there is none, the roster ends here.
        if nick_len == 0 {
            match find_later_member(data, o, slot, count) {
                Some(next) => {
                    o = next;
                    continue;
                }
                None => {
                    complete = true;
                    break;
                }
            }
        }
        if nick_len > 40 || o + nick_len > data.len() {
            break;
        }
        let nickname = match std::str::from_utf8(&data[o..o + nick_len]) {
            Ok(s) => s.to_string(),
            Err(_) => break,
        };
        let name_field = o..o + nick_len;
        o += nick_len;
        if o + 12 > data.len() {
            break;
        }
        let job = crate::entity::job_class::JobClass::from_roster_class(parse_u32_le(data, o));
        o += 4;
        let level = parse_u32_le(data, o) as i32;
        o += 4;
        if !(1..=200).contains(&level) {
            break;
        }
        let gear_score = parse_u32_le(data, o) as i32;
        o += 4;
        if !(0..=1_000_000).contains(&gear_score) {
            break;
        }

        // The stretch between the gear score and combat power is not fixed
        // width — the same roster can carry an extra byte for one member and not
        // another, and a party-state change widens every record's tail. Anchor
        // on the member's world id instead: it is repeated here as a u16 and we
        // already know its value from the top half of `dbid`. Combat power then
        // sits a fixed distance past it.
        let Some(anchor) = find_u16(data, o, o + 10, server_id) else {
            break;
        };
        o = anchor + 2 + 2 + 1; // world id, a second u16, one tag byte
        let combat_power = u64::from_le_bytes(data.get(o..o + 8)?.try_into().ok()?);
        o += 8;
        if combat_power > 100_000_000 {
            break;
        }

        name_fields.push(name_field);
        members.push((
            nickname,
            PartyMember {
                slot,
                level,
                gear_score,
                combat_power: combat_power as i64,
                server_id,
                dbid,
                job,
            },
        ));

        if i32::from(slot) >= count {
            complete = true;
            break;
        }
        // The record tail is likewise variable, so re-acquire the next member by
        // its header: the following slot number, a world id in the top of its
        // dbid, and a decodable name right behind it. The next slot may be
        // vacant instead; the walk then reads it as one.
        match find_next_member(data, o, slot.wrapping_add(1))
            .or_else(|| find_vacant_slot(data, o, slot.wrapping_add(1)))
        {
            Some(next) => o = next,
            None => break,
        }
    }
    if members.is_empty() {
        return None;
    }
    Some(Roster { members, complete, dungeon_id, name_fields })
}

/// Find a little-endian `u16` equal to `wanted` in `data[from..to]`.
fn find_u16(data: &[u8], from: usize, to: usize, wanted: u16) -> Option<usize> {
    let end = to.min(data.len().saturating_sub(2));
    (from..=end).find(|&i| u16::from_le_bytes([data[i], data[i + 1]]) == wanted)
}

/// Re-acquire the start of the next party member record by its header shape:
/// `<mask u8> <slot u8> <dbid u64> <name_len u8> <utf8 name>`, where the slot is
/// known and the top `u16` of the dbid is a plausible world id.
fn find_next_member(data: &[u8], from: usize, expected_slot: u8) -> Option<usize> {
    find_member_within(data, from, 32, expected_slot)
}

/// A named member in a slot after `slot`, past the vacant records between.
/// A vacant record is about 35-37 bytes, of varying width like a member's.
fn find_later_member(data: &[u8], from: usize, slot: u8, count: i32) -> Option<usize> {
    let last = u8::try_from(count).ok()?;
    (slot.checked_add(1)?..=last)
        .find_map(|next| find_member_within(data, from, 48 * usize::from(next - slot), next))
}

/// A vacant record for `expected_slot` near `from`: a zero mask, the slot, a
/// zero dbid and an empty name.
fn find_vacant_slot(data: &[u8], from: usize, expected_slot: u8) -> Option<usize> {
    let end = (from + 32).min(data.len().saturating_sub(11));
    (from..=end).find(|&i| data[i] == 0 && data[i + 1] == expected_slot && data[i + 2..i + 11].iter().all(|&b| b == 0))
}

fn find_member_within(data: &[u8], from: usize, span: usize, expected_slot: u8) -> Option<usize> {
    let end = (from + span).min(data.len().saturating_sub(12));
    for i in from..=end {
        if data[i + 1] != expected_slot {
            continue;
        }
        let server_id = u16::from_le_bytes([data[i + 8], data[i + 9]]);
        if server_id == 0 || server_id > 9_999 {
            continue;
        }
        let name_len = data[i + 10] as usize;
        if name_len == 0 || name_len > 40 || i + 11 + name_len > data.len() {
            continue;
        }
        if std::str::from_utf8(&data[i + 11..i + 11 + name_len]).is_err() {
            continue;
        }
        return Some(i);
    }
    None
}

/// Validate the owner block that follows a spawn's `parent_key` and return the
/// parent id. See `StreamProcessor::find_spawn_parent_key`.
///
/// ```text
/// <parent_key u32 LE> <legion_id u32> <u16 = 0> <u16 server_id> <len u8> <utf8 legion name>
/// ```
fn parse_spawn_owner_block(packet: &[u8], at: usize, self_id: i32) -> Option<i32> {
    if at + 13 > packet.len() {
        return None;
    }
    let parent = parse_u32_le(packet, at);
    if parent == 0 || parent > 9_999_999 || parent as i32 == self_id {
        return None;
    }
    // Two-byte pad that is always zero, then a plausible world id.
    if u16::from_le_bytes([packet[at + 8], packet[at + 9]]) != 0 {
        return None;
    }
    let server_id = u16::from_le_bytes([packet[at + 10], packet[at + 11]]);
    if server_id == 0 || server_id > 9_999 {
        return None;
    }
    let name_len = packet[at + 12] as usize;
    if name_len > 40 || at + 13 + name_len > packet.len() {
        return None;
    }
    // A legion-less owner has an empty name here; anything else must decode.
    std::str::from_utf8(&packet[at + 13..at + 13 + name_len]).ok()?;
    Some(parent as i32)
}

fn find_pattern(data: &[u8], start: usize, pattern: &[u8]) -> Option<usize> {
    if data.len() < pattern.len() + start {
        return None;
    }
    for i in start..=data.len() - pattern.len() {
        if data[i..i + pattern.len()] == *pattern {
            return Some(i);
        }
    }
    None
}

fn to_hex_range(bytes: &[u8], start: usize, end: usize) -> String {
    let s = start.min(bytes.len());
    let e = end.min(bytes.len());
    bytes[s..e].iter().map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" ")
}

fn to_hex(bytes: &[u8]) -> String {
    to_hex_range(bytes, 0, bytes.len())
}

/// The name the game gives a character that has not been named yet: `$` then
/// letters and digits.
fn is_placeholder_name(raw: &str) -> bool {
    raw.strip_prefix('$')
        .is_some_and(|rest| rest.len() >= 4 && rest.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// Byte lengths a length-prefixed name field can have: 1 to 12 characters of
/// up to 4 UTF-8 bytes each.
const NAME_FIELD_BYTES: std::ops::RangeInclusive<usize> = 1..=48;

/// A character name read from a field whose length the packet states.
///
/// Names are 1 to 12 characters: letters in any script (Latin with accents,
/// Japanese, Hangul, Han…) and digits, with at least one letter. The whole
/// field must be the name; anything else means we are not on a name field.
/// Unlike `sanitize_nickname`, a one-character name is fine here: the stated
/// length is what guards against picking up junk.
fn exact_name(field: &[u8]) -> Option<String> {
    let name = std::str::from_utf8(field).ok()?;
    let chars = name.chars().count();
    let valid = (1..=12).contains(&chars)
        && name.chars().all(char::is_alphanumeric)
        && name.chars().any(char::is_alphabetic);
    valid.then(|| name.to_string())
}

fn sanitize_nickname(nickname: &str) -> Option<String> {
    let trimmed = nickname.split('\0').next().unwrap_or("").trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut result = String::new();
    let mut only_numbers = true;
    let mut has_cjk = false;

    for ch in trimmed.chars() {
        if !ch.is_alphanumeric() {
            if result.is_empty() {
                return None;
            }
            break;
        }
        if ch == '\u{FFFD}' {
            if result.is_empty() {
                return None;
            }
            break;
        }
        if ch.is_control() {
            if result.is_empty() {
                return None;
            }
            break;
        }
        result.push(ch);
        if ch.is_alphabetic() {
            only_numbers = false;
        }
        if is_cjk_char(ch) {
            has_cjk = true;
        }
    }

    if result.is_empty() || only_numbers {
        return None;
    }

    if result.chars().count() < 2 && !has_cjk {
        return None;
    }

    Some(result)
}

fn is_cjk_char(ch: char) -> bool {
    let cp = ch as u32;
    // CJK Unified Ideographs
    (0x4E00..=0x9FFF).contains(&cp)
    // Hangul Syllables
    || (0xAC00..=0xD7AF).contains(&cp)
    // CJK Extension A/B
    || (0x3400..=0x4DBF).contains(&cp)
    || (0x20000..=0x2A6DF).contains(&cp)
    // Hangul Jamo
    || (0x1100..=0x11FF).contains(&cp)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum UnicodeScript {
    Han,
    Hangul,
    Other,
}

/// Your server (u16) and class (u32, the roster's encoding), which follow the
/// name in a self record. The server must read as one; the class need not,
/// as Amber1's record on Nezekan (2026-10-01) has other bytes there.
fn self_profile(data: &[u8], after: usize) -> Option<(u16, Option<JobClass>)> {
    let rest = data.get(after..after + 2)?;
    let server = u16::from_le_bytes([rest[0], rest[1]]);
    let job = data
        .get(after + 2..after + 6)
        .and_then(|b| JobClass::from_roster_class(u32::from_le_bytes([b[0], b[1], b[2], b[3]])));
    (1000..3000).contains(&server).then_some((server, job))
}

fn unicode_script(ch: char) -> UnicodeScript {
    let cp = ch as u32;
    if (0x4E00..=0x9FFF).contains(&cp) || (0x3400..=0x4DBF).contains(&cp) || (0x20000..=0x2A6DF).contains(&cp) {
        UnicodeScript::Han
    } else if (0xAC00..=0xD7AF).contains(&cp) || (0x1100..=0x11FF).contains(&cp) {
        UnicodeScript::Hangul
    } else {
        UnicodeScript::Other
    }
}

// ===== WHERE RECORDS HOLD NAMES =====
//
// The parser's name readers, as functions of the bytes alone: what each reads
// and where in the packet the name sits. The parser reads names through them,
// and the Evidence Slice blinds names where they say a name is (`name_fields`),
// so the two can never disagree about where a name is.

/// The `04 8D` record at the front of a packet, as
/// `StreamProcessor::parse_summon_ownership_packet` reads it: the summon, its
/// owner, and the owner's name field (start, length) if one follows.
fn ownership_at_front(packet: &[u8]) -> Option<(i32, i32, Option<(usize, usize)>)> {
    let length_info = read_varint(packet, 0);
    if length_info.length < 0 {
        return None;
    }
    let offset = length_info.length as usize;
    if offset + 1 >= packet.len() {
        return None;
    }
    if packet[offset] != 0x04 || packet[offset + 1] != 0x8D {
        return None;
    }

    let mut pos = offset + 2;
    let summon_info = read_varint(packet, pos);
    if summon_info.length <= 0 || summon_info.value < 100 {
        return None;
    }
    let summon_id = summon_info.value;
    pos += summon_info.length as usize;

    // Skip 4-byte fixed field
    if pos + 4 > packet.len() {
        return None;
    }
    pos += 4;

    let owner_info = read_varint(packet, pos);
    if owner_info.length <= 0 || owner_info.value < 100 {
        return None;
    }
    let owner_id = owner_info.value;
    pos += owner_info.length as usize;

    if owner_id == summon_id {
        return None;
    }

    // Name field after owner ID
    let mut name = None;
    let meta_info = read_varint(packet, pos);
    if meta_info.length > 0 {
        pos += meta_info.length as usize;
        if pos < packet.len() {
            let name_len = packet[pos] as usize;
            if (1..=36).contains(&name_len) && pos + 1 + name_len <= packet.len() {
                name = Some((pos + 1, name_len));
            }
        }
    }
    Some((summon_id, owner_id, name))
}

/// A `04 8D` ownership or loot record found anywhere in `data`.
struct Ownership {
    summon_id: i32,
    owner_id: i32,
    server_id: u16,
    name: String,
    field: std::ops::Range<usize>,
}

/// Every `04 8D` record in `data` that names its owner, as
/// `StreamProcessor::scan_for_embedded_04_8d` reads them.
fn ownership_records(data: &[u8]) -> Vec<Ownership> {
    let mut out = Vec::new();
    let mut search_offset = 0;
    let pattern: [u8; 2] = [0x04, 0x8D];

    while search_offset + 1 < data.len() {
        let idx = find_pattern(data, search_offset, &pattern);
        if idx.is_none() {
            break;
        }
        let idx = idx.unwrap();

        search_offset = idx + 2;
        if search_offset >= data.len() {
            break;
        }

        let summon_info = read_varint(data, search_offset);
        if summon_info.length <= 0 || !(100..=9_999_999).contains(&summon_info.value) {
            continue;
        }
        let summon_id = summon_info.value;

        let fixed_field_start = search_offset + summon_info.length as usize;
        if fixed_field_start + 4 > data.len() {
            continue;
        }

        // The owner follows: `<owner varint> <server id u16 LE> <len><name>`.
        // The server id was once matched as the literal bytes `E0 07` /
        // `E2 07` (servers 2016 and 2018), which skipped every other server:
        // a Sorcerer on Ventus (1305, bytes `19 05`) never got a name. Any id
        // in the servers' 1000–2999 range is accepted now, and a candidate
        // only counts when the owner id sits wholly after the fixed field and
        // the whole name field is a name.
        let after_fixed = fixed_field_start + 4;
        // A zero there is no owner: the record a summon gets as it
        // despawns, all zeros. The scan below then ran on into whatever
        // followed, and found an "owner" in the next damage records: a
        // Cleric's Divine Aura went to entity 10210, named "M", and showed
        // as its own row (2026-10-04, Divine Auldor).
        if data.get(after_fixed).is_none_or(|&b| b == 0) {
            continue;
        }
        let scan_end = std::cmp::min(data.len().saturating_sub(2), after_fixed + 128);
        let mut found = None;
        for server_idx in after_fixed + 1..scan_end {
            let server_id = u16::from_le_bytes([data[server_idx], data[server_idx + 1]]);
            if !(1000..=2999).contains(&server_id) {
                continue;
            }
            // The owner `ed 74` (14957) ends in a byte that alone reads as
            // an id too (`74`, 116); see `varint_ending_at`.
            let owner_id = varint_ending_at(data, server_idx, after_fixed, 100..=99_999);
            let Some(owner_id) = owner_id.filter(|&id| id != summon_id) else {
                continue;
            };
            let name_len_idx = server_idx + 2;
            let name_len = data[name_len_idx] as usize;
            let name_end = name_len_idx + 1 + name_len;
            if !NAME_FIELD_BYTES.contains(&name_len) || name_end > data.len() {
                continue;
            }
            if let Some(name) = exact_name(&data[name_len_idx + 1..name_end]) {
                found = Some(Ownership { summon_id, owner_id, server_id, name, field: name_len_idx + 1..name_end });
                break;
            }
        }
        let Some(record) = found else {
            continue;
        };
        search_offset = record.field.end;
        out.push(record);
    }
    out
}

/// Every spawn opcode (40/41/44/45 36) in `data` that
/// `StreamProcessor::scan_for_embedded_40_36` reads: (where, the opcode's
/// first byte, entity id).
fn embedded_spawns(data: &[u8]) -> Vec<(usize, u8, i32)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 5 < data.len() {
        // Spawn family shifted +1 in June 2026: mob/summon 0x40->0x41,
        // player 0x44->0x45. Accept both old and new leading bytes.
        if data[i + 1] == 0x36 && matches!(data[i], 0x40 | 0x41 | 0x44 | 0x45) {
            if i > 0 && data[i - 1] == 0x00 {
                i += 2;
                continue;
            }
            let target_info = read_varint(data, i + 2);
            if target_info.length > 0 && (100..=9_999_999).contains(&target_info.value) {
                out.push((i, data[i], target_info.value));
            }
            i += 2 + target_info.length.max(0) as usize;
        } else {
            i += 1;
        }
    }
    out
}

/// Where a spawn's mask sits relative to `offset` (just past its entity id),
/// in the u16 and u32 formats: the subtree byte that gates the inline name.
const MASK_U16_SUBTREE: usize = 2;
const MASK_U32_SUBTREE: usize = 4;

/// The inline name of a `40/41 36` spawn (its owner's or caster's), and the
/// name field's bytes. `offset` is just past the entity id.
///
/// The mask width changed from u16 to u32, which moves the subtree byte that
/// gates the inline name. Nothing else in this record is sensitive to it, so
/// rather than version-sniffing the stream, both positions are tried, the
/// current format first so a live stream never depends on the fallback.
///
/// Getting this wrong is not cosmetic. For a summon the inline name is the
/// *owner's* character name, and it is the fallback that attributes a pet's
/// damage to its player when no parent_key is present. A silently
/// mispositioned gate shows up as summons drifting back into their own rows.
fn spawn_name_at(packet: &[u8], offset: usize) -> Option<(String, std::ops::Range<usize>)> {
    let read_name_at = |sub_offset: usize| -> Option<(String, std::ops::Range<usize>)> {
        let gate = *packet.get(offset + sub_offset)?;
        if gate & 0x01 == 0 {
            return None;
        }
        let cursor = offset + sub_offset + 1;
        let name_len = *packet.get(cursor)? as usize;
        if !NAME_FIELD_BYTES.contains(&name_len) || cursor + 1 + name_len > packet.len() {
            return None;
        }
        // The whole field must be a name. This check is what makes trying
        // two positions safe: a wrong guess almost never decodes cleanly.
        let name = exact_name(&packet[cursor + 1..cursor + 1 + name_len])?;
        Some((name, cursor + 1..cursor + 1 + name_len))
    };
    read_name_at(MASK_U32_SUBTREE).or_else(|| read_name_at(MASK_U16_SUBTREE))
}

/// A self record (`33 36`) or another player's record (`44 36`, `45 36`), as
/// `StreamProcessor::scan_masked_identity` reads it.
struct MaskedRecord {
    id: i32,
    /// The name field's bytes.
    field: std::ops::Range<usize>,
    /// `None` for a tutorial character's placeholder in your self record.
    name: Option<String>,
    /// Your server and class, in a self record.
    profile: Option<(u16, Option<JobClass>)>,
}

/// Every masked identity record in `data`:
/// `<opcode 2B> <entity_id varint> <mask1 u32 LE> <mask2 u8> [mask2 & 0x01] <len u8><utf8 name>`.
fn masked_records(data: &[u8]) -> Vec<MaskedRecord> {
    let mut out = Vec::new();
    if data.len() < 9 {
        return out;
    }
    let mut i = 0;
    while i + 8 < data.len() {
        if data[i + 1] != 0x36 {
            i += 1;
            continue;
        }
        // 0x33 = self, 0x45/0x44 = another player (pre/post the June 2026 shift).
        let is_self = data[i] == 0x33;
        if !is_self && data[i] != 0x45 && data[i] != 0x44 {
            i += 1;
            continue;
        }
        let id = read_varint(data, i + 2);
        if id.length <= 0 || !(1..=9_999_999).contains(&id.value) {
            i += 1;
            continue;
        }
        // mask1 is 4 bytes; mask2 is the byte after it and gates the name.
        let mask2_idx = i + 2 + id.length as usize + 4;
        if mask2_idx + 1 >= data.len() || data[mask2_idx] & 0x01 == 0 {
            i += 1;
            continue;
        }
        let name_len = data[mask2_idx + 1] as usize;
        if !NAME_FIELD_BYTES.contains(&name_len) || mask2_idx + 2 + name_len > data.len() {
            i += 1;
            continue;
        }
        let field = mask2_idx + 2..mask2_idx + 2 + name_len;
        let Ok(raw) = std::str::from_utf8(&data[field.clone()]) else {
            i += 1;
            continue;
        };
        // A new character plays the tutorial before it has a name; until
        // then the game calls it `$` plus random letters and digits (seen:
        // `$Kc03nyeQHr4`, entity 3877, on 2026-10-01). It is still you, so
        // bind the entity, but with no name: a placeholder would be noise,
        // and keeping the previous character's name would be wrong.
        if is_self && is_placeholder_name(raw) {
            i = field.end;
            out.push(MaskedRecord { id: id.value, field, name: None, profile: None });
            continue;
        }
        // The whole field must be one clean name; otherwise we landed
        // mid-record rather than on a real name string.
        let Some(sanitized) = exact_name(&data[field.clone()]) else {
            i += 1;
            continue;
        };
        // A self record without your server and class after the name is
        // not one. The last `33 36` of a `1d 37` record ending `33 36 33 36`
        // read with the next record's bytes as entity 16 and a two-letter
        // name (2026-10-05 17:42:14), and the meter took that for you.
        // Entity ids under 100 are real players, so the id cannot tell.
        let profile = if is_self {
            let Some(profile) = self_profile(data, field.end) else {
                i += 1;
                continue;
            };
            Some(profile)
        } else {
            None
        };
        i = field.end;
        out.push(MaskedRecord { id: id.value, field, name: Some(sanitized), profile });
    }
    out
}

/// A named player spawn's (`44 36`, `45 36`) id, name and name field, as
/// `StreamProcessor::parse_player_spawn_name` reads them. Uses the same
/// mask-gated layout as `masked_records`
/// (`<id varint> <mask1 u32> <mask2 u8> [mask2 & 0x01] <len><utf8>`) rather
/// than hunting for the literal `0x07` that older builds happened to put in
/// the `mask2` slot.
fn player_spawn_at(data: &[u8], offset_after_opcode: usize) -> Option<(i32, String, std::ops::Range<usize>)> {
    let actor_info = read_varint(data, offset_after_opcode);
    // Raid/invasion player ids run well past 99,999, so accept the full entity-id
    // range (matching the embedded-scan gate) or those spawns are silently dropped.
    if actor_info.length <= 0 || !(1..=9_999_999).contains(&actor_info.value) {
        return None;
    }
    let actor_id = actor_info.value;
    let mask2_idx = offset_after_opcode + actor_info.length as usize + 4;
    if mask2_idx + 1 >= data.len() || data[mask2_idx] & 0x01 == 0 {
        return None;
    }
    let name_len = data[mask2_idx + 1] as usize;
    if !NAME_FIELD_BYTES.contains(&name_len) || mask2_idx + 2 + name_len > data.len() {
        return None;
    }
    let field = mask2_idx + 2..mask2_idx + 2 + name_len;
    let name = exact_name(&data[field.clone()])?;
    Some((actor_id, name, field))
}

/// A party roster as `StreamProcessor::scan_party_roster` reads it.
struct Roster {
    members: Vec<(String, crate::combat::data_storage::PartyMember)>,
    complete: bool,
    dungeon_id: i32,
    /// Each member's name field, in member order.
    name_fields: Vec<std::ops::Range<usize>>,
}

/// Every party roster in `data`.
fn rosters(data: &[u8]) -> Vec<Roster> {
    let mut out = Vec::new();
    if data.len() < 32 {
        return out;
    }
    let mut i = 0;
    while i + 24 < data.len() {
        if data[i] != 0x02 || data[i + 1] != 0x97 {
            i += 1;
            continue;
        }
        match parse_party_roster_at(data, i + 2) {
            Some(roster) => {
                out.push(roster);
                i += 2;
            }
            None => i += 1,
        }
    }
    out
}

/// Where `parse_actor_name_binding_rules` reads a name: `07 <len> <name>` up to
/// 64 bytes after an anchor `36 <actor varint>`. (actor, name start, length)
fn actor_name_fields(packet: &[u8]) -> Vec<(i32, usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut last_anchor: Option<(i32, usize, usize)> = None; // (actor_id, start, end)

    while i < packet.len() {
        if packet[i] == 0x36 {
            // Skip spawn opcodes (40/41 36 mob, 44/45 36 player) — the
            // 0x36 family shifted +1 in June 2026.
            if i > 0 && matches!(packet[i - 1], 0x40 | 0x41 | 0x44 | 0x45) {
                i += 1;
                continue;
            }
            if i + 1 >= packet.len() {
                i += 1;
                continue;
            }
            let actor_info = read_varint(packet, i + 1);
            last_anchor = if actor_info.length > 0 && actor_info.value >= 100 {
                Some((actor_info.value, i, i + 1 + actor_info.length as usize))
            } else {
                None
            };
            i += 1;
            continue;
        }

        if packet[i] == 0x07
            && let Some((name_start, name_length)) = read_utf8_name(packet, i)
            && let Some((actor_id, _, end_idx)) = last_anchor
        {
            let distance = i as isize - end_idx as isize;
            if (0..=64).contains(&distance) {
                out.push((actor_id, name_start, name_length));
            }
        }
        i += 1;
    }
    out
}

fn read_utf8_name(packet: &[u8], anchor_index: usize) -> Option<(usize, usize)> {
    let length_index = anchor_index + 1;
    if length_index >= packet.len() {
        return None;
    }
    let name_length = packet[length_index] as usize;
    if !(1..=36).contains(&name_length) {
        return None;
    }
    let name_start = length_index + 1;
    let name_end = name_start + name_length;
    if name_end > packet.len() {
        return None;
    }
    let name_bytes = &packet[name_start..name_end];
    let name = std::str::from_utf8(name_bytes).ok()?;
    let sanitized = sanitize_nickname(name)?;
    if sanitized.is_empty() {
        return None;
    }
    Some((name_start, name_length))
}

/// Where `parse_loot_attribution_actor_name` reads a name:
/// `<actor varint> <F0..FF> <03|A3> <len> <name>`. (actor, name, whole field)
fn loot_actor_fields(packet: &[u8]) -> Vec<(i32, String, std::ops::Range<usize>)> {
    let mut out = Vec::new();
    let mut idx = 0;

    while idx + 2 < packet.len() {
        let marker = packet[idx] as u32;
        let marker_next = packet[idx + 1] as u32;
        let is_marker = (0xF0..=0xFF).contains(&marker) && (marker_next == 0x03 || marker_next == 0xA3);

        if is_marker {
            // Scan backward for actor ID
            let mut actor_info: Option<VarIntResult> = None;
            let min_offset = idx.saturating_sub(8);
            for actor_offset in min_offset..idx {
                if !can_read_varint(packet, actor_offset) {
                    continue;
                }
                let candidate = read_varint(packet, actor_offset);
                if candidate.length <= 0 || actor_offset + candidate.length as usize != idx {
                    continue;
                }
                if !(100..=99999).contains(&candidate.value) {
                    continue;
                }
                actor_info = Some(candidate);
                break;
            }

            let actor_info = match actor_info {
                Some(a) => a,
                None => { idx += 1; continue; }
            };

            let length_idx = idx + 2;
            if length_idx >= packet.len() {
                idx += 1;
                continue;
            }
            let name_length = packet[length_idx] as usize;
            if !(1..=36).contains(&name_length) {
                idx += 1;
                continue;
            }
            let name_start = length_idx + 1;
            let name_end = name_start + name_length;
            if name_end > packet.len() {
                idx += 1;
                continue;
            }
            let name = match std::str::from_utf8(&packet[name_start..name_end]) {
                Ok(s) => s,
                Err(_) => { idx = name_end; continue; }
            };
            let sanitized = match sanitize_nickname(name) {
                Some(s) => s,
                None => { idx = name_end; continue; }
            };
            out.push((actor_info.value, sanitized, name_start..name_end));
            idx = name_end;
            continue;
        }
        idx += 1;
    }
    out
}

/// Where `parsing_nickname` reads a name, by its three patterns. (id, name,
/// the name's bytes)
fn nickname_fields(packet: &[u8]) -> Vec<(i32, String, std::ops::Range<usize>)> {
    let mut out = Vec::new();
    let mut search_offset = 0;

    while search_offset + 2 < packet.len() {
        // PATTERN A: E2/E0 07 anchor
        if (packet[search_offset] == 0xE2 || packet[search_offset] == 0xE0)
            && packet[search_offset + 1] == 0x07
        {
            let len_idx = search_offset + 2;
            if len_idx < packet.len() {
                let name_len = packet[len_idx] as usize;
                if (2..=36).contains(&name_len) && len_idx + 1 + name_len <= packet.len() {
                    let np = &packet[len_idx + 1..len_idx + 1 + name_len];
                    if let Ok(possible_name) = std::str::from_utf8(np) {
                        if !possible_name.is_empty() && possible_name.chars().next().unwrap().is_alphanumeric() {
                            if let Some((sanitized, range)) = sanitized_at(packet, len_idx + 1, name_len) {
                                if sanitized.len() >= 2
                                    && let Some(id) = varint_ending_at(packet, search_offset, 0, 100..=9_999_999)
                                {
                                    out.push((id, sanitized, range));
                                    search_offset = len_idx + 1 + name_len;
                                    // Skip guild name
                                    search_offset = skip_guild_name(packet, search_offset);
                                }
                            }
                        }
                    }
                }
            }
        }

        // PATTERN B: 0F 1D 37 block anchor
        if search_offset + 2 < packet.len()
            && packet[search_offset] == 0x0F
            && packet[search_offset + 1] == 0x1D
            && packet[search_offset + 2] == 0x37
        {
            let id_offset = search_offset + 3;
            if can_read_varint(packet, id_offset) {
                let block_actor = read_varint(packet, id_offset);
                if (100..=9_999_999).contains(&block_actor.value) {
                    let mut block_scan = id_offset + block_actor.length as usize;
                    let block_end = std::cmp::min(packet.len(), block_scan + 500);

                    while block_scan + 3 < block_end {
                        // Stop at terminator. The leading byte changed
                        // 0x06 -> 0x0E in the June 2026 update; accept both.
                        if (packet[block_scan] == 0x06 || packet[block_scan] == 0x0E) && packet[block_scan + 1] == 0x00 && packet[block_scan + 2] == 0x36 {
                            break;
                        }
                        // Name must be preceded by 00 00
                        if packet[block_scan] == 0x00 && packet[block_scan + 1] == 0x00 {
                            let len_idx = block_scan + 2;
                            if len_idx < packet.len() {
                                let name_len = packet[len_idx] as usize;
                                if (2..=36).contains(&name_len) && len_idx + 1 + name_len <= packet.len() {
                                    let np = &packet[len_idx + 1..len_idx + 1 + name_len];
                                    if let Ok(possible_name) = std::str::from_utf8(np) {
                                        if !possible_name.is_empty() && possible_name.chars().next().unwrap().is_alphanumeric() {
                                            if let Some((sanitized, range)) = sanitized_at(packet, len_idx + 1, name_len) {
                                                if sanitized.len() >= 2 {
                                                    out.push((block_actor.value, sanitized, range));
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        block_scan += 1;
                    }
                }
            }
        }

        // PATTERN D: Terminator anchor (04/00 4C)
        if search_offset + 1 < packet.len() {
            let b0 = packet[search_offset] as u32;
            let b1 = packet[search_offset + 1] as u32;

            if (b0 == 0x04 || b0 == 0x00) && b1 == 0x4C {
                let id_idx = search_offset + 2;
                if can_read_varint(packet, id_idx) {
                    let player_info = read_varint(packet, id_idx);
                    if player_info.length > 0 && (100..=9_999_999).contains(&player_info.value) {
                        let stop_at = std::cmp::min(packet.len().saturating_sub(2), id_idx + 128);
                        let mut scan_idx = id_idx + player_info.length as usize;

                        while scan_idx < stop_at {
                            // Terminator: leading byte changed 0x06 -> 0x0E
                            // in the June 2026 update; accept both.
                            if (packet[scan_idx] == 0x06 || packet[scan_idx] == 0x0E)
                                && packet[scan_idx + 1] == 0x00
                                && packet[scan_idx + 2] == 0x36
                            {
                                // Look backwards for name
                                for test_len in 2..=36usize {
                                    if scan_idx < test_len + 1 + id_idx {
                                        continue;
                                    }
                                    let len_byte_idx = scan_idx - test_len - 1;
                                    if len_byte_idx <= id_idx {
                                        continue;
                                    }
                                    let possible_len = packet[len_byte_idx] as usize;
                                    if possible_len == test_len {
                                        let np = &packet[len_byte_idx + 1..len_byte_idx + 1 + test_len];
                                        if let Ok(possible_name) = std::str::from_utf8(np) {
                                            if !possible_name.is_empty() && possible_name.chars().next().unwrap().is_alphanumeric() {
                                                if let Some(found) = sanitized_at(packet, len_byte_idx + 1, test_len) {
                                                    if found.0.len() >= 2 {
                                                        // Try to find earlier name (player name vs guild)
                                                        let before_name = find_name_before(
                                                            packet, len_byte_idx,
                                                            id_idx + player_info.length as usize,
                                                        );
                                                        let (final_name, range) = before_name.unwrap_or(found);
                                                        out.push((player_info.value, final_name, range));
                                                        search_offset = scan_idx;
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                break;
                            }
                            scan_idx += 1;
                        }
                    }
                }
            }
        }

        search_offset += 1;
    }
    out
}

fn find_name_before(packet: &[u8], before_idx: usize, min_idx: usize) -> Option<(String, std::ops::Range<usize>)> {
    for test_len in 2..=36usize {
        for gap in 0..=1usize {
            if before_idx < gap + test_len + 1 {
                continue;
            }
            let name_len_idx = before_idx - gap - test_len - 1;
            if name_len_idx < min_idx {
                continue;
            }
            let possible_len = packet[name_len_idx] as usize;
            if possible_len != test_len {
                continue;
            }
            let np = &packet[name_len_idx + 1..name_len_idx + 1 + test_len];
            if let Ok(possible_name) = std::str::from_utf8(np) {
                if possible_name.is_empty() || !possible_name.chars().next().unwrap().is_alphanumeric() {
                    continue;
                }
                if let Some(found) = sanitized_at(packet, name_len_idx + 1, test_len) {
                    if found.0.len() >= 2 {
                        return Some(found);
                    }
                }
            }
        }
    }
    None
}

fn skip_guild_name(packet: &[u8], start_index: usize) -> usize {
    if start_index >= packet.len() {
        return start_index;
    }
    let mut offset = start_index;
    if packet[offset] == 0x00 {
        offset += 1;
        if offset >= packet.len() {
            return offset;
        }
    }
    let length = packet[offset] as usize;
    if !(1..=36).contains(&length) {
        return offset;
    }
    let name_start = offset + 1;
    let name_end = name_start + length;
    if name_end > packet.len() {
        return offset;
    }
    if std::str::from_utf8(&packet[name_start..name_end]).is_err() {
        return offset;
    }
    name_end
}

/// The name `sanitize_nickname` reads from `packet[start..start + len]`, and
/// where its bytes sit: the field's first run of letters and digits.
fn sanitized_at(packet: &[u8], start: usize, len: usize) -> Option<(String, std::ops::Range<usize>)> {
    let field = std::str::from_utf8(packet.get(start..start.checked_add(len)?)?).ok()?;
    let name = sanitize_nickname(field)?;
    let head = field.split('\0').next().unwrap_or("");
    let at = start + head.len() - head.trim_start().len();
    let end = at + name.len();
    (packet.get(at..end)? == name.as_bytes()).then_some((name, at..end))
}

/// A place where a packet holds a character name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameField {
    pub range: std::ops::Range<usize>,
}

/// Where one framed packet holds character names, as a replay reads that
/// packet: the readers `parse_perfect_packet` runs on it, and the record scans
/// over it (`scan_records`). Sorted by where they start; two readers that find
/// the same field give it once. Found whether or not the parser would take the
/// name (it may already know the entity): a name is a name either way.
///
/// The Evidence Slice blinds names here, at any length. That is the only way
/// to find a one-letter name, which the game allows: searched for, its one
/// byte turns up all through the packets.
pub fn name_fields(packet: &[u8]) -> Vec<NameField> {
    let mut out = Vec::new();
    let mut push = |range: std::ops::Range<usize>| out.push(NameField { range });
    // At the front: `parse_summon_ownership_packet` and `parse_summon_packet`.
    let length = read_varint(packet, 0);
    if length.length >= 0 {
        let offset = length.length as usize;
        if offset + 1 < packet.len() && packet[offset + 1] == 0x36 {
            if matches!(packet[offset], 0x44 | 0x45) {
                if let Some((_, _, range)) = player_spawn_at(packet, offset + 2) {
                    push(range);
                }
            } else if matches!(packet[offset], 0x40 | 0x41)
                && let Some(range) = spawn_owner(packet, offset + 2)
            {
                push(range);
            }
        }
    }
    if let Some((_, _, Some((start, len)))) = ownership_at_front(packet)
        && let Some((_, range)) = sanitized_at(packet, start, len)
    {
        push(range);
    }
    for (_, start, len) in actor_name_fields(packet) {
        if let Some((_, range)) = sanitized_at(packet, start, len) {
            push(range);
        }
    }
    for (_, _, range) in loot_actor_fields(packet) {
        if let Some((_, range)) = sanitized_at(packet, range.start, range.len()) {
            push(range);
        }
    }
    for (_, _, range) in nickname_fields(packet) {
        push(range);
    }
    scanned_name_fields(packet, &mut out);
    out.sort_by_key(|f| (f.range.start, f.range.end));
    out.dedup_by(|a, b| a.range == b.range);
    out
}

/// The name a `40/41 36` spawn carries, as `parse_summon_spawn_at` reads it.
fn spawn_owner(packet: &[u8], offset_after_opcode: usize) -> Option<std::ops::Range<usize>> {
    let id = read_varint(packet, offset_after_opcode);
    if id.length < 0 {
        return None;
    }
    let offset = offset_after_opcode + id.length as usize;
    if offset + 2 >= packet.len() {
        return None;
    }
    spawn_name_at(packet, offset).map(|(_, range)| range)
}

/// The records the scans read anywhere in a packet (see `scan_records`). The
/// character-select list (`scan_char_list_self`) is left out: it names only
/// the character the meter was told to look for.
fn scanned_name_fields(data: &[u8], out: &mut Vec<NameField>) {
    for record in ownership_records(data) {
        out.push(NameField { range: record.field });
    }
    for (i, opcode, _) in embedded_spawns(data) {
        if opcode == 0x44 || opcode == 0x45 {
            if let Some((_, _, range)) = player_spawn_at(data, i + 2) {
                out.push(NameField { range });
            }
        } else if let Some(range) = spawn_owner(data, i + 2) {
            // Read whether or not the parser already knows the id as a mob.
            out.push(NameField { range });
        }
    }
    for record in masked_records(data) {
        if record.name.is_some() {
            out.push(NameField { range: record.field });
        }
    }
    for roster in rosters(data) {
        for range in roster.name_fields {
            out.push(NameField { range });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::data_storage::SkillCombatData;

    /// A roster with slots 1 and 5 filled and 2-4 vacant (bytes from a
    /// 2026-08-15 capture, rearranged). The walk used to stop at slot 2.
    #[test]
    fn roster_reads_members_past_vacant_slots() {
        let roster = [
            "02 97 a5 f9 08 00 0b 38 35 30 4b 20 e8 bf 9e e5 88 b7 05 63 28 09 00 00 03 85 4b 01 00 00 00 f6 03 1f 02 00 05",
            "0c 01 85 4b 01 00 00 00 f6 03 0f e4 b9 9d e5 b7 9e e4 be 9d e7 84 b6 e5 9c a8 06 00 00 00 32 00 00 00 07 17 00 00 f6 03 f6 03 04 38 39 0d 00 00 00 00 00 00 01 01",
            "00 02 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 04 00 00 00 00 00 00 00 00 00 00 00 00",
            "00 03 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 04 00 00 00 00 00 00 00 00 00 00 00 00",
            "00 04 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 04 00 00 00 00 00 00 00 00 00 00 00 00",
            "0e 05 62 64 01 00 00 00 f6 03 02 4d 37 0e 00 00 00 32 00 00 00 5e 16 00 00 f6 03 f6 03 04 6a 03 0d 00 00 00 00 00 00 01 01",
            "02 0e 00 36",
        ]
        .join(" ");
        let data: Vec<u8> = roster.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect();
        let Roster { members, complete, .. } = parse_party_roster_at(&data, 2).expect("a roster");
        let slots: Vec<(String, u8)> = members.iter().map(|(n, m)| (n.clone(), m.slot)).collect();
        assert_eq!(slots, vec![("九州依然在".to_string(), 1), ("M7".to_string(), 5)]);
        assert!(complete);

        // Members in slots 1-2 and the rest vacant: complete, as before.
        let roster = ["02 97 a5 f9 08 00 0b 38 35 30 4b 20 e8 bf 9e e5 88 b7 05 63 28 09 00 00 03 85 4b 01 00 00 00 f6 03 1f 02 00 05", "0c 01 85 4b 01 00 00 00 f6 03 0f e4 b9 9d e5 b7 9e e4 be 9d e7 84 b6 e5 9c a8 06 00 00 00 32 00 00 00 07 17 00 00 f6 03 f6 03 04 38 39 0d 00 00 00 00 00 00 01 01", "0e 02 62 64 01 00 00 00 f6 03 02 4d 37 0e 00 00 00 32 00 00 00 5e 16 00 00 f6 03 f6 03 04 6a 03 0d 00 00 00 00 00 00 01 01", "00 03 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 04 00 00 00 00 00 00 00 00 00 00 00 00", "00 04 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 04 00 00 00 00 00 00 00 00 00 00 00 00", "00 05 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 04 00 00 00 00 00 00 00 00 00 00 00 00"].join(" ");
        let data: Vec<u8> = roster.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect();
        let Roster { members, complete, .. } = parse_party_roster_at(&data, 2).expect("a roster");
        assert_eq!(members.len(), 2);
        assert!(complete, "vacant slots after the last member end the roster");
    }

    /// A Spiritmaster's spirits send `04 38` records to their owner on each
    /// landed attack (2026-10-05 15:37). The Wind Spirit's (16990003) restores
    /// 1.5 % HP: 103 of 6871. The Water Spirit's (16990002) restores 20 MP
    /// (SkillEffect MpHeal 20), same layout; the game files it under 100011,
    /// so it showed as "Fire Spirit: Basic Attack" healing. A self-cast MP
    /// restore (15760007, 30 MP, 15:17:41) went in as a self-heal.
    #[test]
    fn mp_restores_are_not_healing() {
        let (storage, mut p) = processor();
        let mut hit = crate::entity::damage_packet::ParsedDamagePacket::new();
        hit.set_timestamp(1_000);
        hit.set_target_id(16720);
        hit.set_actor_id(10137);
        hit.set_skill_code(16040000);
        hit.set_type(2);
        hit.set_damage(500);
        storage.append_damage(hit.clone());
        hit.set_actor_id(2787);
        storage.append_damage(hit);
        storage.register_confirmed_summon_by_id(61001, 10137);
        storage.register_confirmed_summon_by_id(49482, 10137);

        assert!(feed(&mut p, "994f0400c9dc03333f03010602f7af446501000000ac52670100"));
        assert!(feed(&mut p, "994f0400ca8203323f0301060293af446501000000ac52140100"));
        assert!(feed(&mut p, "e3150400e315877af0005302c7dcef5d01000000865d1e0100"));

        // This meter does not yet record a spirit's HP restore on its owner
        // (the Wind Spirit's 103), so no heal is left at all: the self-cast
        // 30 MP is gone, and nothing went in as damage either.
        let heals = storage.get_heal_snapshot();
        let healed: Vec<_> = heals.iter().flat_map(|(a, s)| s.iter().map(move |(k, v)| (*a, k.0, v.total_heal))).collect();
        assert_eq!(healed, vec![]);
        let snapshot = storage.get_combat_snapshot();
        assert!(!snapshot.contains_key(&10137) && !snapshot.contains_key(&2787));
    }

    /// A heal tick from a capture (2026-10-05 17:31:46): target 5041, heal,
    /// actor 5041, effect 190000131, 7351. The effect is abnormal 19000013,
    /// Restore HP, which no skill owns: a full heal (5041's max HP was 6683
    /// and went to full). Its code, 1900001, is no skill but has a name.
    #[test]
    fn a_heal_from_no_skill_has_a_name() {
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/data");
        let skills = Arc::new(SkillLookup::new());
        let npcs = Arc::new(NpcLookup::new());
        crate::i18n::lookup::load_language(&skills, &npcs, &data, "en");
        let storage = Arc::new(DataStorage::new());
        let mut p = StreamProcessor::new(storage.clone(), skills.clone(), npcs);
        p.set_override_timestamp(Some(1_000));
        p.parse_dot_packet(&hex("130538b12701b127b601032c530bb739"));
        let heal = &storage.get_heal_snapshot()[&5041][&(1_900_001, false)];
        assert_eq!((heal.total_heal, heal.tick_count), (7351, 1));
        assert_eq!(skills.lookup_skill_name(1_900_001), "Restore HP");
        for lang in ["de", "es", "fr", "ja", "ko", "pt", "ru"] {
            let skills = SkillLookup::new();
            crate::i18n::lookup::load_language(&skills, &NpcLookup::new(), &data, lang);
            assert!(!skills.lookup_skill_name(1_900_001).is_empty(), "{lang}");
        }
    }

    /// A damage tick from a capture (2026-10-05 16:35:12): target 5377 (a
    /// player), actor 26813 (never seen spawning), effect 120001211, 51. The
    /// effect is abnormal 12000121, a Burn many monster skills share and no
    /// skill owns. Its code, 1200012, is no skill but has a name.
    #[test]
    fn a_damage_tick_from_no_skill_has_a_name() {
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/data");
        let skills = Arc::new(SkillLookup::new());
        let npcs = Arc::new(NpcLookup::new());
        crate::i18n::lookup::load_language(&skills, &npcs, &data, "en");
        let storage = Arc::new(DataStorage::new());
        let mut p = StreamProcessor::new(storage.clone(), skills.clone(), npcs);
        p.set_dot_skill_ids(HashSet::from([1_200_012]));
        p.set_override_timestamp(Some(1_000));
        p.parse_dot_packet(&hex("170538812a0abdd1019002bb1227073332931200"));
        let tick = &storage.get_combat_snapshot()[&5377].actors[&26813].skills[&(1_200_012, true)];
        assert_eq!((tick.total_damage, tick.hit_count), (51, 1));
        assert_eq!(skills.lookup_skill_name(1_200_012), "Burn");
        // Poison, Bleed and Burn, magic, physical and the rest.
        for code in [1_200_010, 1_200_011, 1_200_014, 1_200_015, 1_200_016, 1_200_018] {
            assert!(["Poison", "Bleed", "Burn"].contains(&skills.lookup_skill_name(code).as_str()), "{code}");
        }
        for lang in ["de", "es", "fr", "ja", "ko", "pt", "ru"] {
            let skills = SkillLookup::new();
            crate::i18n::lookup::load_language(&skills, &NpcLookup::new(), &data, lang);
            assert!(!skills.lookup_skill_name(1_200_015).is_empty(), "{lang}");
        }
    }

    /// Damage records from a live capture (2026-10-04, target 30001, actor
    /// 1395), each checked against the game's own Damage Analyzer record of
    /// the same fight: switch bit 0x20 marks a hit with additional hits.
    #[test]
    fn additional_hits_are_read_from_the_record_tail() {
        let hex = |s: &str| (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect::<Vec<u8>>();
        let parse = |record: &str| {
            let storage = Arc::new(DataStorage::new());
            let mut p = StreamProcessor::new(storage.clone(), Arc::new(SkillLookup::new()), Arc::new(NpcLookup::new()));
            p.set_override_timestamp(Some(1_000));
            let mut packet = vec![0x00, 0x04, 0x38];
            packet.extend(hex(record));
            packet[0] = packet.len() as u8;
            assert!(p.parsing_damage(&packet, false, false), "{record}");
            let combat = storage.get_combat_snapshot();
            let target = combat.values().next().unwrap();
            let skill = target.actors.values().next().unwrap().skills.values().next().unwrap().clone();
            (skill.total_damage, skill.hit_count, skill.multi_hit_count, skill.multi_hit_hits, skill.multi_hit_damage)
        };
        // Layout 6, switch 0x36: 1700 with two additional hits of 24.
        assert_eq!(parse("b1ea013600f30a40c0f4007a038000010b199b5f01000000ac52a40d021818"), (1700, 1, 1, 2, 48));
        // Layout 4, switch 0x34: the layout-4 field, then one additional hit of 89.
        assert_eq!(parse("b1ea013400f30ae0b7f800cd028bd3276101000000ac52d330010159"), (6227, 1, 1, 1, 89));
        // A spirit's layout-4 records (Summon: Wind Spirit, a Wind Spirit basic
        // attack): no layout-4 field, the additional hits right after the value.
        assert_eq!(parse("fe9e0224009e9b01c3f8f5000302792b156002000000ac52e10301060200"), (481, 1, 1, 1, 6));
        assert_eq!(parse("fe9e0224009e9b01c18601001702a7a2980001000000ac52e401030303030100"), (228, 1, 1, 3, 9));
        assert_eq!(parse("fe9e0204009e9b01c3f8f5000302792b156003000000ac52b7030300"), (439, 1, 0, 0, 0));
        // Layout 6, switch 0x16: no additional hits.
        assert_eq!(parse("b1ea011600f30a40c0f40063028000010b199b5f01000000ac52d007"), (976, 1, 0, 0, 0));
    }

    /// A damage record four bundles deep is read; five deep, it is not, the
    /// same depth the slice builder stops at.
    #[test]
    fn bundles_nest_four_deep_at_most() {
        let frame = |body: &[u8]| {
            let mut v = crate::capture::framing::length_value(body.len());
            let mut out = Vec::new();
            loop {
                let b = (v & 0x7F) as u8;
                v >>= 7;
                if v == 0 {
                    out.push(b);
                    break;
                }
                out.push(b | 0x80);
            }
            out.extend_from_slice(body);
            out
        };
        let bundle = |inner: &[u8]| {
            let mut body = vec![0xFF, 0xFF];
            body.extend_from_slice(&(inner.len() as u32).to_le_bytes());
            body.extend_from_slice(&lz4_flex::compress(inner));
            frame(&body)
        };
        let record = [&[0x04, 0x38][..], &hex("b1ea011600f30a40c0f40063028000010b199b5f01000000ac52d007")].concat();
        for (depth, read) in [(1, true), (4, true), (5, false), (40, false)] {
            let mut stream = frame(&record);
            for _ in 0..depth {
                stream = bundle(&stream);
            }
            let (storage, mut p) = processor();
            p.consume_stream(&stream);
            assert_eq!(!storage.get_combat_snapshot().is_empty(), read, "depth {depth}");
        }
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// Hand one `04 38` record (the bytes after the opcode) to the parser.
    fn feed(p: &mut StreamProcessor, record: &str) -> bool {
        let mut packet = vec![0x00, 0x04, 0x38];
        packet.extend(hex(record));
        packet[0] = packet.len() as u8;
        p.parsing_damage(&packet, false, false)
    }

    /// A Training Scarecrow's live HP (2026-10-07, entity 25839, whose spawn
    /// came before the meter started): hit down to 1, it stops there and
    /// comes back to full. That is how the meter knows it is a dummy without
    /// the NPC code.
    #[test]
    fn a_dummy_whose_spawn_was_missed_is_known_by_its_hp() {
        let (storage, mut p) = processor();
        let hp = |p: &mut StreamProcessor, hp: &str| {
            p.consume_stream(&hex(&format!("1400 8DEFC901 020100 {hp} 00000000").replace(' ', "")))
        };
        for reading in ["55E50000", "DF120000"] {
            assert_eq!(hp(&mut p, reading), 17);
        }
        assert_eq!(storage.get_mob_current_hp(25839), Some(0x12DF));
        assert!(!storage.is_hp_reset_dummy(25839), "hit, not yet at the floor");
        hp(&mut p, "01000000");
        assert!(!storage.is_hp_reset_dummy(25839), "at the floor: a mob about to die looks the same");
        hp(&mut p, "8F380100");
        assert!(storage.is_hp_reset_dummy(25839), "back up from 1 without dying");
    }

    /// A Wind Spirit's Malicious Whirlwind ticks on after the spirit is
    /// unsummoned (2026-10-06 21:16, spirit 25676 of player 15740 on target
    /// 48776). The game's Damage Analyzer counted the two ticks before the
    /// spirit's `42 36` flag 7 and neither after it; so does the meter.
    #[test]
    fn a_spirit_s_ticks_stop_counting_when_it_leaves() {
        let (storage, mut p) = processor();
        p.set_dot_skill_ids(HashSet::from([16_001_109]));
        let tick = hex("19053888fd020accc801881241c15f5ff7025828f400");
        // The owner's link record to the spirit (16770001).
        let link = "ccc8010400fc7ad1e3ff000102affdf46301000000e65bf4010100";
        feed(&mut p, link);
        assert_eq!(storage.get_summon_data().get(&25676), Some(&15740));
        p.parse_dot_packet(&tick);
        p.parse_dot_packet(&tick);
        p.parse_death_packet(&hex("0b4236ccc8010007"));
        p.parse_dot_packet(&tick);
        p.parse_dot_packet(&tick);
        let ticks = &storage.get_combat_snapshot()[&48776].actors[&25676].skills[&(16_001_109, true)];
        assert_eq!((ticks.hit_count, ticks.total_damage), (2, 750));

        // A spirit back under the same id (its link records resume) counts again.
        feed(&mut p, link);
        p.parse_dot_packet(&tick);
        let ticks = &storage.get_combat_snapshot()[&48776].actors[&25676].skills[&(16_001_109, true)];
        assert_eq!(ticks.hit_count, 3);
    }

    /// From a meter opened mid-session at the training dummies (2026-10-07,
    /// EU): a hit by the local player, entity 2737 (`b1 15`), on dummy 25839,
    /// and one of the `06 38` records the server sends about them several
    /// times a second. No self record came for 18 minutes.
    #[test]
    fn a_meter_opened_mid_session_finds_you_in_party_scope_records() {
        let (storage, mut p) = processor();
        let scope = hex("0e0638b115171dd200f700");
        let mut stream = hex("240438efc9010600b115c859d100fc0300000001c711c75101000000904eca050100");
        for _ in 0..23 {
            stream.extend(&scope);
        }
        assert_eq!(p.consume_stream(&stream), stream.len());
        assert_eq!(storage.local_player_id(), None);
        p.consume_stream(&scope);
        assert_eq!(storage.local_player_id(), Some(2737));
        assert!(storage.local_id_from_scope());
    }

    fn processor() -> (Arc<DataStorage>, StreamProcessor) {
        let storage = Arc::new(DataStorage::new());
        let mut p = StreamProcessor::new(storage.clone(), Arc::new(SkillLookup::new()), Arc::new(NpcLookup::new()));
        p.set_override_timestamp(Some(1_000));
        (storage, p)
    }

    fn skill_of(storage: &DataStorage, target: i32, actor: i32, skill: i32) -> SkillCombatData {
        storage.get_combat_snapshot()[&target].actors[&actor].skills[&(skill, false)].clone()
    }

    /// Player hits from the captures of 2026-10-04, one per flag the player
    /// side shows: the flags byte after the hit type, then the angle.
    #[test]
    fn hit_flags_are_read_from_player_records() {
        let parse = |record: &str, target: i32, actor: i32, skill: i32| {
            let (storage, mut p) = processor();
            assert!(feed(&mut p, record), "{record}");
            let s = skill_of(&storage, target, actor, skill);
            (s.total_damage, s.crit_count, s.parry_count, s.perfect_count, s.double_count, s.frontal_count)
        };
        // Flags 0x02, the target parried: 106 damage, front.
        assert_eq!(parse("e9de020600c80b9147ff003f02020002aff4b76301000000e4506a0100", 44905, 1480, 16730001), (106, 0, 1, 0, 0, 1));
        // A critical hit the target parried.
        assert_eq!(parse("fcc2010600bf6bc759d1003203020002c711c75101000000904e660100", 24956, 13759, 13720007), (102, 1, 1, 0, 0, 1));
        // Flags 0x04, Perfect, no angle.
        assert_eq!(parse("cac20406009e389147ff00e102040000aff4b76301000000a4587f0100", 74058, 7198, 16730001), (127, 0, 0, 1, 0, 0));
        // Flags 0x08, Double.
        assert_eq!(parse("bfe6020600d23310ffd6006f0208000055a2fb5302000000aa56d20e0200", 45887, 6610, 14090000), (1874, 0, 0, 0, 1, 0));
    }

    /// The flags only monsters' hits on players show in the captures of
    /// 2026-10-04 (`<target> 06 00 <actor> <skill> <uid> <hit type>`, then the
    /// plotter). The parser leaves these records out; the plotter is read
    /// the same way.
    #[test]
    fn hit_flags_of_received_hits() {
        let plotter = |record: &str, at: usize| {
            let p = read_plotter(&hex(record), at).unwrap();
            (special_damage::from_hit_flags(p.flags), p.hp, p.angle)
        };
        use SpecialDamage::*;
        // 0x01 Shield Block: 74 damage.
        assert_eq!(plotter("ca6f0600ca9804b8c112000102010002ebab530701000000904e4a0100", 13), (vec![ShieldBlock], 0, Some(2)));
        // 0x10 Iron Wall.
        assert_eq!(plotter("fe2a0600c68d03b8c112000102100002ebab530701000000904e740100", 13), (vec![IronWall], 0, Some(2)));
        // 0x20 Regeneration, 13 HP back from a hit of 68.
        assert_eq!(plotter("df7e0600a3b103b0b512000302200d02cbf84e0701000000904e440100", 13), (vec![Regeneration], 13, Some(2)));
        // 0x40 Perfect Block, with Shield Block or Parry; the hit does 1.
        assert_eq!(plotter("e9230600ee9e03e4cc120001024100021b09580701000000904e010100", 13), (vec![ShieldBlock, PerfectBlock], 0, Some(2)));
        assert_eq!(plotter("e06d0600aeb1024cba12000202420002bbc5500701000000904e010100", 13), (vec![Parry, PerfectBlock], 0, Some(2)));
    }

    /// Restoration HP is a varint: 177 takes two bytes (`b1 01`). Read as one
    /// byte, as the fixed skip did, the angle came out as Back (the `01`) and
    /// the value one varint early: the actor's scalar 10000 instead of 889.
    #[test]
    fn restoration_hp_is_a_varint() {
        let record = hex("ed080600f0ef04bab51200010220b10102b3fc4e0701000000904ef9060100");
        let p = read_plotter(&record, 13).unwrap();
        assert_eq!((p.flags, p.hp, p.hp_len, p.angle), (0x20, 177, 2, Some(2)));
        // Then 8 bytes, the scalar and the value: 177 HP is 20 % of 889, as on
        // every Regeneration hit with HP in the captures.
        let mut at = 13 + 1 + p.hp_len + 1 + 8;
        assert_eq!(try_read_varint(&record, &mut at), Some(10_000));
        assert_eq!(try_read_varint(&record, &mut at), Some(889));
        // The second two-byte record: 204 HP of 1020.
        let record = hex("ee0d0600849d03bab51200010220cc0101b3fc4e0701000000904efc070100");
        let p = read_plotter(&record, 13).unwrap();
        assert_eq!((p.hp, p.angle), (204, Some(1)));
        let mut at = 13 + 1 + p.hp_len + 1 + 8 + 2;
        assert_eq!(try_read_varint(&record, &mut at), Some(1020));
    }

    /// Hit type 1 (Miss) and 6 (Resist) records have no value. They count on
    /// the skill and leave damage, hits and targets as they were.
    #[test]
    fn misses_and_resists_count_without_damage() {
        // A Resist of Divine Punishment, the same cast (uid 5d) as a hit of
        // 1976 on the same target (2026-10-04 00:44:09).
        let (storage, mut p) = processor();
        assert!(feed(&mut p, "a8a3041400be77802a04015d02109aa06501000000dc51b80f010100"));
        assert!(!feed(&mut p, "a8a3040001be77802a04015d06139aa06501000000dc5101c9e1f5050100"));
        let s = skill_of(&storage, 70056, 15294, 17050240);
        assert_eq!((s.total_damage, s.hit_count, s.resist_count, s.miss_count), (1976, 1, 1, 0));
        assert_eq!(storage.get_combat_snapshot()[&70056].total_damage, 1976);

        // A Miss of Dimensional Control (2026-10-04 03:56:02), a skill that
        // never deals damage, after another hit of the same player.
        let miss = "e981020000899803172df9000101079d556101000000904e0100";
        let (storage, mut p) = processor();
        assert!(!feed(&mut p, miss));
        assert!(storage.get_combat_snapshot().is_empty(), "no target from a miss alone");
        let mut hit = ParsedDamagePacket::new();
        hit.set_timestamp(1_000);
        hit.set_target_id(33001);
        hit.set_actor_id(52233);
        hit.set_skill_code(16000000);
        hit.set_type(2);
        hit.set_damage(500);
        storage.append_damage(hit);
        assert!(!feed(&mut p, miss));
        let s = skill_of(&storage, 33001, 52233, 16330007);
        assert_eq!((s.total_damage, s.hit_count, s.miss_count, s.resist_count), (0, 0, 1, 0));
        assert_eq!(storage.get_combat_snapshot()[&33001].total_damage, 500);
    }

    /// Map loads from a live capture (2026-10-04): into Fire Temple, a
    /// teleport inside it, then out to World_L_A.
    #[test]
    fn only_a_map_load_into_the_open_world_ends_the_dungeon() {
        let storage = Arc::new(DataStorage::new());
        let p = StreamProcessor::new(storage.clone(), Arc::new(SkillLookup::new()), Arc::new(NpcLookup::new()));
        let hex = |s: &str| (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect::<Vec<u8>>();
        let load = |s: &str| p.parse_map_load_packet(&hex(s));
        storage.set_current_dungeon(600021);
        load("34213601000000d52709003b1a350000000000f7e646460d7fb0c60080b045409da54200000000000000000000004f0000");
        load("34213602000000d5270900d74c390000000000a8f805c610861245008036453ccd24c204000000000000000000004f0000");
        assert_eq!(storage.current_dungeon_id(), 600021);
        load("34213601000000f2030000dd7f3c00000000006868d047d0c62c470098da46fa63284300000000000000000000004f0000");
        assert_eq!(storage.current_dungeon_id(), 0);
        // The party stays together and its roster is sent again, still naming
        // the dungeon it was for: the player is still in the open world.
        storage.set_current_dungeon(600021);
        assert_eq!(storage.current_dungeon_id(), 0, "a roster after leaving does not bring the dungeon back");
        // Back in: the load names the instance.
        load("34213601000000d52709003b1a350000000000f7e646460d7fb0c60080b045409da54200000000000000000000004f0000");
        assert_eq!(storage.current_dungeon_id(), 600021);
    }

    #[test]
    fn world_layers_are_open_world_and_seals_are_not() {
        use crate::combat::data_storage::is_open_world_map;
        assert!(is_open_world_map(1010), "World_L_A");
        assert!(is_open_world_map(101021), "a layer of World_L_A");
        assert!(!is_open_world_map(310051), "Seal_Verteron_051");
        assert!(!is_open_world_map(600021), "Fire_Temple_Easy");
        assert!(!is_open_world_map(999_999_999), "unknown map");
    }

    #[test]
    fn another_players_spirit_is_linked_at_spawn_by_its_caster() {
        let storage = Arc::new(DataStorage::new());
        let mut p = StreamProcessor::new(storage.clone(), Arc::new(SkillLookup::new()), Arc::new(NpcLookup::new()));
        // `41 36 <47324> <mask, kind 0x1F> … <caster anchor> <6332>`, no parent_key, no name.
        let mut spirit = vec![0x41, 0x36, 0xdc, 0xf1, 0x02, 0x1f, 0x10, 0x00, 0xc6];
        spirit.extend([0x22; 24]);
        spirit.extend([0x80, 0x75, 0xd5, 0x2a, 0xbb, 0x03, 0x00, 0x00, 0xbc, 0x31, 0x0c, 0x02]);
        assert!(p.parse_summon_spawn_at(&spirit, 2));
        assert_eq!(storage.get_summon_data().get(&47324), Some(&6332));

        // A mob's caster field is no owner.
        let mut mob = spirit.clone();
        mob[2] = 0xdd;
        mob[5] = 0x0c;
        assert!(!p.parse_summon_spawn_at(&mob, 2));
        assert!(!storage.is_summon(47325));
    }

    #[test]
    fn a_monsters_summon_is_not_the_player_its_spawn_names() {
        let storage = Arc::new(DataStorage::new());
        let npcs = NpcLookup::new();
        npcs.load_from_json(r#"{"2920063":{"name":"Blazing Totem","isBoss":false},"2920149":{"name":"Wind Spirit","isBoss":false}}"#);
        let mut p = StreamProcessor::new(storage.clone(), Arc::new(SkillLookup::new()), Arc::new(npcs));
        // A player, the one the totem burns.
        storage.append_nickname_authoritative(3640, "Abcd");
        let mut hit = ParsedDamagePacket::new();
        hit.set_actor_id(3640);
        hit.set_target_id(900);
        hit.set_skill_code(11_010_000);
        hit.set_damage(100);
        storage.append_damage(hit);
        // Blazing Totem 51395 (2920063) from the scarecrow capture of
        // 2026-10-05, the name it carries made up: kind 0x1C, the u16 mask's
        // name field naming the player, then the NPC code.
        let spawn = |id: &str, code: &str| {
            let mut b = hex("4136");
            b.extend(hex(id));
            b.extend(hex("1c00010441626364"));
            b.extend(hex(code));
            b.extend(hex("0000020e6ccdc607af014800089b46a0411d43d46f0103030000000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000006011101819698"));
            b.extend(hex("00ffffffffffffffff8075d52abb030000c3910301020e6ccdc607af014800089b460676700000001e00000000"));
            b
        };
        assert!(!p.parse_summon_spawn_at(&spawn("c39103", "7f8e2c"), 2));
        assert!(!storage.is_summon(51395));
        let mut burn = ParsedDamagePacket::new();
        burn.set_actor_id(51395);
        burn.set_target_id(3640);
        burn.set_skill_code(1_200_012);
        burn.set_damage(1);
        burn.set_dot(true);
        burn.set_timestamp(2_000);
        storage.append_damage(burn);
        assert!(storage.get_heal_snapshot().is_empty(), "its Burn on the player is no healing");

        // A spirit's spawn naming its player still links.
        assert!(p.parse_summon_spawn_at(&spawn("c49103", "d58e2c"), 2));
        assert_eq!(storage.get_summon_data().get(&51396), Some(&3640));
    }

    #[test]
    fn a_sorcerers_ground_spell_is_linked_to_its_caster() {
        let storage = Arc::new(DataStorage::new());
        let mut p = StreamProcessor::new(storage.clone(), Arc::new(SkillLookup::new()), Arc::new(NpcLookup::new()));
        let hex = |s: &str| (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect::<Vec<u8>>();
        // Bittercold Wind entity 18249 from a party run (krao capture, 2026-10-05):
        // kind 0x1F, buff block naming itself, caster 14143 after `07 02 06`.
        let storm = |id_varint: &str, caster: &str| {
            let mut b = hex("4136");
            b.extend(hex(id_varint));
            b.extend(hex("1F00004B8E2C004002000CF0C7CFA9D0C70090C04672498542642F01A925A9258E0800008E08000000000000000000000000000010E9010064000000F04902000100000000000000A08601000000000090D00300010101110181969800FFFFFFFFFFFFFFFF8075D52ABB030000"));
            b.extend(hex(id_varint));
            b.extend(hex("0102000CF0C7CFA9D0C70090C046070206"));
            b.extend(hex(caster));
            b.extend(hex("02CD002800"));
            b
        };
        assert!(p.parse_summon_spawn_at(&storm("C98E01", "3F370000"), 2));
        assert_eq!(storage.get_summon_data().get(&18249), Some(&14143));

        // A marker naming the spell itself, or a mob, links nothing.
        assert!(!p.parse_summon_spawn_at(&storm("CA8E01", "4A470000"), 2));
        storage.append_mob(30000, 1);
        assert!(!p.parse_summon_spawn_at(&storm("CB8E01", "30750000"), 2));
        assert!(!storage.is_summon(18250) && !storage.is_summon(18251));

        // Some spawns carry `07 02 01` instead of `07 02 06`.
        let mut other = storm("CC8E01", "BF000000");
        let m = other.windows(3).position(|w| w == [0x07, 0x02, 0x06]).unwrap();
        other[m + 2] = 0x01;
        assert!(p.parse_summon_spawn_at(&other, 2));
        assert_eq!(storage.get_summon_data().get(&18252), Some(&191));
    }

    /// A party member whose spawn the meter missed is named by their pet's
    /// spawn, which carries the owner's name and id together. From a
    /// dungeon run (2026-10-09) replayed from just before the last boss: a
    /// Ranger's pet (`0x5F`) named her a second in, two minutes before her
    /// own spawn. Names, legion and ids are stand-ins; entity 1480 has been
    /// fighting entity 22567, the boss, whose spawn was missed too.
    #[test]
    fn a_pets_spawn_names_its_owner() {
        let (storage, mut p) = processor();
        assert!(feed(&mut p, "a7b0010600c80b9147ff003f02020002aff4b76301000000e4506a0100"));
        assert!(storage.get_nickname(1480).is_none());

        // The boss's mechanic (`0x1F`), named after the player it targets,
        // with the boss in its caster field: no name for the boss.
        let mechanic = hex(concat!(
            "4136a582011f000108",
            "4861776b65796531", // "Hawkeye1"
            "968f2c0040024fbd66c6c86b804600a05345600d7d43f3b301c0ee6dc0ee6d640000006400000000",
            "00000000000000000000000000000090650000000000000100000000000000000000000000000000",
            "000000010602110181969800ffffffffffffffff8075d52abb030000a5820101044fbd66c6c86b80",
            "4600a05345110284969800ffffffffffffffff8075d52abb030000a58201014fbd66c6c86b804600",
            "a05345070206",
            "27580000", // caster field: 22567
            "002500000000",
        ));
        p.parse_summon_spawn_at(&mechanic, 2);
        assert!(storage.get_nickname(22567).is_none());
        assert!(storage.get_nickname(1480).is_none());

        // The pet: owner 1480 as its parent_key and in its caster field.
        let pet = |id: &str, owner_name: &str| {
            let mut b = hex(&format!("4136{id}5f000108"));
            b.extend(owner_name.as_bytes());
            b.extend(hex(concat!(
                "ac902c00000200c46ac600043f460030524524e90b437e6301f73ef73efc090000fc090000000000",
                "00000000000000000018f0010064000000f04902000100000000000000a08601000000000000e204",
                "000101110181969800ffffffffffffffff8075d52abb030000b4c601010200c46ac600043f460030",
                "5245070206",
                "c8050000", // owner: 1480, the caster field and the parent_key
                "ad01000000000f0905",
                "4775696c64", // legion "Guild"
                "02000000000000000000000000000000000002cd0096000000d000340100002600000000",
            )));
            b
        };
        assert!(p.parse_summon_spawn_at(&pet("b4c601", "Hawkeye1"), 2));
        assert_eq!(storage.get_summon_data().get(&25396), Some(&1480));
        assert_eq!(storage.get_nickname(1480).as_deref(), Some("Hawkeye1"));

        // A name the game stated is kept.
        storage.append_nickname_authoritative(1480, "Bowmaster");
        assert!(p.parse_summon_spawn_at(&pet("b5c601", "Hawkeye1"), 2));
        assert_eq!(storage.get_nickname(1480).as_deref(), Some("Bowmaster"));
    }

    #[test]
    fn names_are_one_to_twelve_letters_or_digits_in_any_script() {
        for name in ["A", "é", "あ", "ApexZ", "Amber1", "Zoë", "Ñandú", "さくら", "桜子", "전사", "Abcdefghijkl"] {
            assert_eq!(exact_name(name.as_bytes()).as_deref(), Some(name), "{name}");
        }
        for field in [
            &b"Abcdefghijklm"[..], // 13 characters
            b"12345",              // no letter
            b"Apex Z",
            b"ApexZ\x06",
            b"\x05ApexZ",
            b"",
            &[0xC3][..], // cut-off UTF-8
        ] {
            assert_eq!(exact_name(field), None, "{field:?}");
        }
    }

    #[test]
    fn an_id_is_read_whole_not_from_its_last_byte() {
        // 13978 = 9A 6D; the 6D alone is 109 and must not win (issue #10).
        assert_eq!(varint_ending_at(&[0x01, 0x9A, 0x6D, 0xE2, 0x07], 3, 0, 100..=99_999), Some(13978));
        // 14957 = ED 74, a loot owner (2026-10-04).
        assert_eq!(varint_ending_at(&[0x01, 0xED, 0x74, 0x18, 0x05], 3, 0, 100..=99_999), Some(14957));
        // A small id is still one byte.
        assert_eq!(varint_ending_at(&[0x01, 0x6D, 0xE2, 0x07], 2, 0, 100..=99_999), Some(109));
        // 8765 = BD 44: 44 alone is 68, below range.
        assert_eq!(varint_ending_at(&[0x01, 0xBD, 0x44, 0xE2, 0x07], 3, 0, 100..=99_999), Some(8765));
    }

    /// The start of a self record from a live capture (2026-10-04): Naicha,
    /// entity 14957 (`ed 74`), server 1304 (`18 05`), class 30 = Cleric, a
    /// byte, level 28. The rest of the record is not needed and not kept.
    #[test]
    fn the_self_record_says_your_server_class_and_level() {
        let storage = Arc::new(DataStorage::new());
        let processor = StreamProcessor::new(storage.clone(), Arc::new(SkillLookup::new()), Arc::new(NpcLookup::new()));
        let hex = "3336ed745e91c12837064e616963686118051e000000011c0000007f0100007f0100001c000000d002040000000000";
        let record: Vec<u8> = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
        processor.scan_masked_identity(&record);
        let me = storage.local_profile();
        assert_eq!(me.name.as_deref(), Some("Naicha"));
        assert_eq!(storage.local_player_id(), Some(14957));
        assert_eq!((me.server_id, me.class, me.level), (1304, Some(crate::entity::job_class::JobClass::Cleric), Some(28)));
    }

    /// Two frames from a capture (2026-10-06 18:12:55): a `2a 37` record that
    /// ends `44 36 33 7c 42 17 40`, then a mob spawn whose length is `8f 01`.
    /// Read on across the frame end, that is a player record for entity 51
    /// (`33`) with mask2 `8f` and the one-byte name `01 41`, "A".
    fn record_cut_by_its_frame() -> Vec<u8> {
        hex(concat!(
            "1b2a37e880011d034a065afad1bff9d53a4436337c421740",
            "8f014136efb4031c000064902c0000026b519247339aa5c700540a4600d78b3fc7000107076400000064",
            "000000000000000000000000000000000000006400000064000000010000000000000000000000000000",
            "00000000000601110181969800ffffffffffffffff8075d52abb030000efb40301026b519247339aa5c7",
            "00540a46063ed40000002900000000",
        ))
    }

    #[test]
    fn a_record_ends_with_its_frame() {
        let stream = record_cut_by_its_frame();
        let mut bundle = vec![0xFF, 0xFF];
        bundle.extend_from_slice(&(stream.len() as u32).to_le_bytes());
        bundle.extend_from_slice(&lz4_flex::compress(&stream));
        let len = crate::capture::framing::length_value(bundle.len());
        let mut bundled = vec![(len as u8) | 0x80, (len >> 7) as u8];
        bundled.extend_from_slice(&bundle);
        for (how, bytes) in [("in the stream", stream), ("in a bundle", bundled)] {
            let (storage, mut p) = processor();
            p.consume_stream(&bytes);
            assert_eq!(storage.get_nickname(51), None, "{how}");
            // The spawn after it is still read: a mob, code 2920548.
            assert_eq!(storage.mob_code(55919), Some(2_920_548), "{how}");
        }
    }

    /// Four `1d 37` records from a capture (2026-10-05 17:42:14). The second
    /// ends `33 36 33 36`; its last `33 36`, then the third record's length
    /// byte and bytes, read as a self record for entity 16 with a two-letter
    /// name, and the meter took entity 16 for you.
    #[test]
    fn a_self_record_needs_your_server_and_class() {
        let (storage, mut p) = processor();
        p.consume_stream(&hex(
            "101d37f5282703ab13e776e776111d37fc792f031557ff33363336101d37a3132703c6b679be97bc111d37ab262f032250d8722f722f",
        ));
        assert_eq!(storage.local_player_id(), None);
        assert_eq!(storage.get_nickname(16), None);
    }

    /// `4A 36` records, sent about your own stats and no one else's, move the
    /// local player onto the entity they name when the self record that
    /// should have done it was missed. The packet's shape is from a player's
    /// log (2026-10-09), its entity id replaced.
    #[test]
    fn own_stats_records_name_the_local_entity() {
        let storage = Arc::new(DataStorage::new());
        let mut p = StreamProcessor::new(storage.clone(), Arc::new(SkillLookup::new()), Arc::new(NpcLookup::new()));
        storage.set_local_identity_from_game(5100, Some("Mine".into()));
        let record = hex("114A36B8300A0000020050165636"); // entity 6200
        p.consume_stream(&[record.clone(), record.clone()].concat());
        assert_eq!(storage.local_player_id(), Some(5100), "two records are not enough");
        p.consume_stream(&record);
        assert_eq!(storage.local_player_id(), Some(6200));
        assert!(storage.local_id_from_scope());
        assert_eq!(storage.get_nickname(6200), None);
    }

    /// A Sorcerer on Ventus (server 1305) killing a mob, from a player's log
    /// (2026-10-01): `04 8d <mob> <4 bytes> <owner 1454> <server 1305> <name>
    /// <server name>`. The server id used to be matched only as `E0 07` /
    /// `E2 07`, so this owner never got a name. (Whether the name is then bound
    /// depends on 1454 having been seen in combat; `identity_replay` covers that.)
    #[test]
    fn kill_record_names_its_owner_on_any_server() {
        let processor = StreamProcessor::new(
            Arc::new(DataStorage::new()),
            Arc::new(SkillLookup::new()),
            Arc::new(NpcLookup::new()),
        );
        let record = [
            &[0x04, 0x8d, 0xec, 0xde, 0x02, 0x72, 0x28, 0xe9, 0x00, 0xae, 0x0b, 0x19, 0x05, 0x05][..],
            b"ApexZ",
            &[0x06],
            b"Ventus",
            &[0x01, 0x00, 0x00, 0x00],
        ]
        .concat();
        assert!(processor.scan_for_embedded_04_8d(&record));

        // The same record with a name that runs into the next field is not one.
        let mut garbled = record.clone();
        garbled[13] = 0x07;
        assert!(!processor.scan_for_embedded_04_8d(&garbled));
    }
}

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::entity::damage_packet::ParsedDamagePacket;
use crate::entity::job_class::JobClass;
use crate::entity::special_damage::SpecialDamage;
use crate::entity::summon_resolver;

/// Maximum idle gap before a fight is considered ended and a new one begins.
const IDLE_RESET_MS: i64 = 30_000;

/// A zone-change auto-reset is ignored if any damage was recorded within this
/// window, so an in-combat self-teleport (boss knockback/pull) can't wipe an
/// active fight. Real zone transitions always follow a travel/load lull.
const ZONE_RESET_LULL_MS: i64 = 1_500;
/// Minimum spacing between two zone-change resets (debounce).
const ZONE_RESET_DEBOUNCE_MS: i64 = 4_000;

/// Capture time while replaying, wall clock while capturing. See `crate::clock`
/// — the idle-reset and zone-reset decisions below are timing decisions, so
/// reading the replaying machine's clock made replays non-deterministic.
/// How long party members who have not fought stay on the meter after the
/// last roster. See `DataStorage::party_placeholders_wanted`.
const PARTY_PLACEHOLDER_MS: i64 = 10 * 60 * 1000;
/// "Has not happened" for the times below. Not 0: a replayed slice's clock
/// starts before 0 (the lead-in runs at negative offsets from the pull), so
/// a 0 there read as "a moment ago" and suppressed every zone reset in the
/// lead-in, which ran a wiped pull's damage into the next pull (Gargaum,
/// 2026-07 capture: 114M derived for a 69M pull).
const NEVER_MS: i64 = i64::MIN;
/// How often, in damage records, unnamed party members are matched to the
/// roster by class. A fight brings a few hundred records a second, so this
/// names them within the first moments of combat.
const ROSTER_BIND_EVERY: u32 = 64;

fn now_ms() -> i64 {
    crate::clock::now_ms()
}

// ───── Aggregate data structures ─────

/// Healing done, aggregated per (healer actor, skill, is_hot). Healing is keyed by
/// the HEALER (not the boss target), since the meter shows "healing done" per player.
#[derive(Debug, Clone, Default)]
pub struct HealSkillData {
    pub total_heal: i64,
    pub tick_count: i32,
}

#[derive(Debug, Clone)]
pub struct SkillCombatData {
    pub skill_code: i32,
    pub is_dot: bool,
    pub hit_count: i32,
    pub total_damage: i32,
    pub min_damage: i32,
    pub max_damage: i32,
    pub crit_count: i32,
    pub back_count: i32,
    pub frontal_count: i32,
    pub parry_count: i32,
    pub perfect_count: i32,
    pub double_count: i32,
    pub smite_count: i32,
    pub powershard_count: i32,
    pub multi_hit_count: i32,
    pub multi_hit_damage: i32,
    pub multi_hit_hits: i32,
    pub heal_amount: i32,
    pub hit_timestamps: Vec<i64>,
    pub spec_flags: [bool; 5],
}

impl SkillCombatData {
    /// Clone every aggregate field but leave `hit_timestamps` empty.
    /// The timestamp Vec grows by one entry per hit (unbounded over a long
    /// fight) and is only ever consumed by `get_target_details` (the details
    /// panel chart). Every other consumer clones it for nothing, so the hot
    /// 500ms paths use this to keep per-tick clone cost flat over fight time.
    /// Spelled out manually rather than `Vec::new(), ..self.clone()` because
    /// the latter would copy `hit_timestamps` only to throw it away.
    fn clone_light(&self) -> Self {
        Self {
            skill_code: self.skill_code,
            is_dot: self.is_dot,
            hit_count: self.hit_count,
            total_damage: self.total_damage,
            min_damage: self.min_damage,
            max_damage: self.max_damage,
            crit_count: self.crit_count,
            back_count: self.back_count,
            frontal_count: self.frontal_count,
            parry_count: self.parry_count,
            perfect_count: self.perfect_count,
            double_count: self.double_count,
            smite_count: self.smite_count,
            powershard_count: self.powershard_count,
            multi_hit_count: self.multi_hit_count,
            multi_hit_damage: self.multi_hit_damage,
            multi_hit_hits: self.multi_hit_hits,
            heal_amount: self.heal_amount,
            hit_timestamps: Vec::new(),
            spec_flags: self.spec_flags,
        }
    }

    /// Add `other`'s hits to these: the same skill, recorded under two ids.
    fn absorb(&mut self, other: SkillCombatData) {
        self.hit_count += other.hit_count;
        self.total_damage = self.total_damage.saturating_add(other.total_damage);
        self.min_damage = self.min_damage.min(other.min_damage);
        self.max_damage = self.max_damage.max(other.max_damage);
        self.crit_count += other.crit_count;
        self.back_count += other.back_count;
        self.frontal_count += other.frontal_count;
        self.parry_count += other.parry_count;
        self.perfect_count += other.perfect_count;
        self.double_count += other.double_count;
        self.smite_count += other.smite_count;
        self.powershard_count += other.powershard_count;
        self.multi_hit_count += other.multi_hit_count;
        self.multi_hit_damage = self.multi_hit_damage.saturating_add(other.multi_hit_damage);
        self.multi_hit_hits += other.multi_hit_hits;
        self.heal_amount = self.heal_amount.saturating_add(other.heal_amount);
        self.hit_timestamps.extend(other.hit_timestamps);
        self.hit_timestamps.sort_unstable();
        for (mine, theirs) in self.spec_flags.iter_mut().zip(other.spec_flags) {
            *mine |= theirs;
        }
    }

    fn new(skill_code: i32, is_dot: bool) -> Self {
        Self {
            skill_code,
            is_dot,
            hit_count: 0,
            total_damage: 0,
            min_damage: i32::MAX,
            max_damage: 0,
            crit_count: 0,
            back_count: 0,
            frontal_count: 0,
            parry_count: 0,
            perfect_count: 0,
            double_count: 0,
            smite_count: 0,
            powershard_count: 0,
            multi_hit_count: 0,
            multi_hit_damage: 0,
            multi_hit_hits: 0,
            heal_amount: 0,
            hit_timestamps: Vec::new(),
            spec_flags: [false; 5],
        }
    }
}

/// Who the local player is playing. See `DataStorage::local_profile`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LocalProfile {
    pub name: Option<String>,
    /// 0 when unknown.
    pub server_id: u16,
    pub class: Option<JobClass>,
    pub level: Option<u32>,
}

/// One entry of the party roster packet (`0x9702`). Keyed by character name,
/// because the roster carries the account-level `dbid` rather than the
/// session-scoped entity id — the name is the only field that joins it to the
/// in-world entities the meter tracks.
#[derive(Debug, Clone, Default)]
pub struct PartyMember {
    /// 1-based party slot.
    pub slot: u8,
    pub level: i32,
    /// Equipment item level ("gear score").
    pub gear_score: i32,
    /// Combat power — the number the game shows on the character sheet.
    pub combat_power: i64,
    /// World/server id (the bracket tag next to a cross-server player's name).
    /// This is the top `u16` of `dbid`, kept separately because the roster parse
    /// anchors on it.
    pub server_id: u16,
    /// The roster's own id for this member, server-assigned and stable across
    /// renames — the whole 64 bits, of which `server_id` is the top sixteen.
    ///
    /// Kept because it is the only identifier here that is *not* a name. Log
    /// sharing needs to say "this row is the same person as that row" without
    /// putting a character name on the wire, and a name cannot do that job: it
    /// changes on rename, and it is re-usable by a stranger once freed, which
    /// would silently hand them the previous owner's consent.
    pub dbid: u64,
    /// The member's class, as the roster states it. Lets a member be named
    /// before their entity id is known: see `bind_roster_names_by_class`.
    pub job: Option<JobClass>,
}

#[derive(Debug, Clone)]
pub struct ActorCombatData {
    pub total_damage: i64,
    pub party_heal: i64,
    pub regen: i64,
    pub damage_received: i64,
    pub hits_received: i32,
    pub last_damage_time: i64,
    pub job: Option<JobClass>,
    /// Skills keyed by (raw_skill_code, is_dot)
    pub skills: HashMap<(i32, bool), SkillCombatData>,
}

impl ActorCombatData {
    /// Add everything `other` recorded: one character, under an old entity id.
    fn absorb(&mut self, other: ActorCombatData) {
        self.total_damage += other.total_damage;
        self.party_heal += other.party_heal;
        self.regen += other.regen;
        self.damage_received += other.damage_received;
        self.hits_received += other.hits_received;
        self.last_damage_time = self.last_damage_time.max(other.last_damage_time);
        self.job = self.job.or(other.job);
        for (key, skill) in other.skills {
            match self.skills.get_mut(&key) {
                Some(mine) => mine.absorb(skill),
                None => {
                    self.skills.insert(key, skill);
                }
            }
        }
    }

    fn new() -> Self {
        Self {
            total_damage: 0,
            party_heal: 0,
            regen: 0,
            damage_received: 0,
            hits_received: 0,
            last_damage_time: 0,
            job: None,
            skills: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TargetCombatData {
    pub target_id: i32,
    pub total_damage: i64,
    pub first_damage_time: i64,
    pub last_damage_time: i64,
    pub last_packet_id: i64,
    /// Per raw-actor aggregated combat data
    pub actors: HashMap<i32, ActorCombatData>,
    /// The instance this fight was in, as the party roster last named it at
    /// one of its hits; 0 in the open world.
    pub dungeon_id: i32,
}

impl TargetCombatData {
    fn new(target_id: i32, timestamp: i64) -> Self {
        Self {
            target_id,
            total_damage: 0,
            first_damage_time: timestamp,
            last_damage_time: timestamp,
            last_packet_id: -1,
            actors: HashMap::new(),
            dungeon_id: 0,
        }
    }
}

// ───── Main storage ─────

pub struct DataStorage {
    inner: RwLock<Inner>,
    damage_generation: AtomicI64,
    /// Wall-clock ms of the last damage record — gates the zone-change lull check.
    last_damage_ms: AtomicI64,
    /// Wall-clock ms of the last honored zone-change reset — debounce.
    last_zone_reset_ms: AtomicI64,
    /// Set when a zone change clears combat; the dps calculator consumes it to
    /// drop its cached snapshot / saved-target state on the next cycle.
    combat_reset_requested: AtomicBool,
    /// Run just before combat data is cleared by a zone change or the end of a
    /// party, with no lock of ours held, so the fights can still be saved.
    before_reset: RwLock<Option<Arc<dyn Fn() + Send + Sync>>>,
}

struct Inner {
    /// Aggregated combat data per target (replaces raw packet storage)
    target_combat: HashMap<i32, TargetCombatData>,
    /// Job class detected per actor (across all targets, for summon matching)
    actor_jobs: HashMap<i32, JobClass>,

    nickname_storage: HashMap<i32, String>,
    pending_nicknames: HashMap<i32, String>,
    permanent_nicknames: HashMap<i32, String>,
    summon_storage: HashMap<i32, i32>,
    mob_storage: HashMap<i32, i32>,
    /// Healing done per (healer actor) -> (skill_code, is_hot) -> aggregate.
    heal_storage: HashMap<i32, HashMap<(i32, bool), HealSkillData>>,
    /// Spawn-time / observed-peak MAX HP per entity (denominator for the HP bar).
    mob_hp_data: HashMap<i32, i32>,
    /// Live CURRENT HP per entity, from the in-place `8D <id> 02 01 00 <u32>` feed.
    mob_current_hp: HashMap<i32, i32>,
    known_player_ids: HashSet<i32>,
    /// Ids whose nickname came from an authoritative source (a 45/44 36 player
    /// spawn or the account char-list). Lower-confidence parsers may not steal
    /// such a name onto a different id. Lives parallel to `nickname_storage`:
    /// survives a combat flush, cleared by `reset_nicknames`, evicted alongside.
    authoritative_name_ids: HashSet<i32>,
    confirmed_summon_ids: HashSet<i32>,
    /// Ids that spawned via a `40/41 36` mob/summon spawn (as opposed to a
    /// `44/45 36` player spawn). A real player never spawns this way, so an
    /// entity here that deals class-band damage is a summon / spell-effect
    /// entity — it must not be flagged as a known player, and is a candidate for
    /// attribution to the same-class player.
    summon_spawn_ids: HashSet<i32>,
    /// Entity ids below the usual `>= 100` sanity floor that a spawn or identity
    /// record has proven real. The damage parser uses `>= 100` as a resync gate
    /// while walking varints, which silently discarded every hit from players
    /// whose session entity id happened to be tiny (observed live: an
    /// Elementalist at id 48 lost 939 hits plus all 114 of their pets). Ids
    /// confirmed here are allowed through that gate; unconfirmed low values are
    /// still rejected, so the gate keeps its resync value.
    low_id_entities: HashSet<i32>,
    /// Party roster from the `0x9702` packet, keyed by character name.
    party_members: HashMap<String, PartyMember>,
    /// When the last roster arrived, and whether its members who have not
    /// fought should still get rows. See `party_placeholders_wanted`.
    party_roster_at_ms: i64,
    party_placeholders_hidden: bool,
    /// Damage records since `bind_roster_names_by_class` last ran. Counted in
    /// records rather than time so a replay names players where a live meter
    /// did.
    damage_since_roster_bind: u32,
    /// Instance id the party is in, from the same packet. Encodes the dungeon and
    /// its difficulty tier; resolved to a name by the frontend's dungeon table.
    current_dungeon_id: i32,
    /// Power-scalar values observed per actor in its damage records. A summon
    /// inherits its owner's, so this links the two when no spawn packet (and
    /// therefore no `parent_key`) ever arrives — the case for a Cleric's Divine
    /// Aura, which the server creates without announcing. A set rather than one
    /// value because the scalar shifts as buffs come and go, and owner and summon
    /// are not always in the same buff state at the same instant.
    actor_power_scalars: HashMap<i32, HashSet<i32>>,
    hostile_target_ids: HashSet<i32>,
    dead_entity_ids: HashSet<i32>,
    /// Boss entity IDs identified from NPC DB boss flags
    boss_entity_ids: HashSet<i32>,
    /// Training dummies (scarecrows, punching bags) among the entities spawned,
    /// from the NPC table. Damage on them follows `held_dot_ticks`.
    training_dummy_ids: HashSet<i32>,
    /// On a training dummy, DoT ticks that landed after their actor's latest
    /// direct hit, keyed (target, actor). They are counted when that actor
    /// hits directly again; if the player has stopped attacking, they never
    /// are, and the fight's time ends at the last direct hit. A player asked
    /// for this (issue #6): DoTs ticking on after you stop dragged a training
    /// fight's DPS down. Bosses keep every tick, since there players stop
    /// attacking to dodge.
    held_dot_ticks: HashMap<(i32, i32), Vec<ParsedDamagePacket>>,
    /// Whether the current combat segment has any boss damage
    has_boss_in_segment: bool,
    current_target: i32,

    // Local player
    local_player_id: Option<i64>,
    /// Behind an Arc because `get_dps` reads it every 500ms and the set can hold
    /// thousands of entries; cloning it on each tick would be pure waste.
    supporters: std::sync::Arc<crate::supporters::Roster>,
    local_character_name: Option<String>,
    /// Set once the game itself has said who the local player is (the `33 36`
    /// self record). That outranks the window title and any name the UI has
    /// remembered, which can be a different character entirely. While set,
    /// `local_character_name` is the game's; `None` there means a tutorial
    /// character, which the game names with a `$`-prefixed placeholder until
    /// the player picks a name.
    local_identity_from_game: bool,
    /// Who the loot records (`04 8d` after a kill) say owns the drops, and
    /// what that has been used for. See `note_loot_owner`.
    loot_identity: LootIdentity,
    /// Each character's home server, by name, from the records that state it:
    /// the self record and loot records. See `fight_server_id`.
    player_servers: HashMap<String, u16>,
    /// The class and level your own self record last stated, with the name it
    /// was for, so a character switch does not carry the last one's over.
    self_profile: Option<(String, Option<JobClass>, Option<u32>)>,
}

#[derive(Default)]
struct LootIdentity {
    /// Each owner the loot records have named this session: the entity id
    /// they gave, and the kills (mob ids) they named them for. Kills, not
    /// records: the embedded scan sees one record again on every read of the
    /// buffer it sits in, which counted one kill dozens of times.
    owners: HashMap<String, (i32, HashSet<i32>)>,
    /// Entities the server has sent a `06 38` record about. Those go to you
    /// and your party, never to strangers fighting nearby, so a loot owner
    /// outside this set is someone else's kill. See `note_party_scope`.
    party_scope: HashSet<i32>,
    /// The local identity currently in force came from them, not from the
    /// self record.
    applied: bool,
}

impl DataStorage {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(Inner {
                target_combat: HashMap::new(),
                actor_jobs: HashMap::new(),
                nickname_storage: HashMap::new(),
                pending_nicknames: HashMap::new(),
                permanent_nicknames: HashMap::new(),
                summon_storage: HashMap::new(),
                mob_storage: HashMap::new(),
                heal_storage: HashMap::new(),
                mob_hp_data: HashMap::new(),
                mob_current_hp: HashMap::new(),
                known_player_ids: HashSet::new(),
                authoritative_name_ids: HashSet::new(),
                confirmed_summon_ids: HashSet::new(),
                summon_spawn_ids: HashSet::new(),
                low_id_entities: HashSet::new(),
                party_members: HashMap::new(),
                party_roster_at_ms: 0,
                damage_since_roster_bind: 0,
                party_placeholders_hidden: false,
                current_dungeon_id: 0,
                actor_power_scalars: HashMap::new(),
                hostile_target_ids: HashSet::new(),
                dead_entity_ids: HashSet::new(),
                boss_entity_ids: HashSet::new(),
                training_dummy_ids: HashSet::new(),
                held_dot_ticks: HashMap::new(),
                has_boss_in_segment: false,
                current_target: 0,
                local_player_id: None,
                supporters: std::sync::Arc::new(crate::supporters::Roster::default()),
                local_character_name: None,
                local_identity_from_game: false,
                loot_identity: LootIdentity::default(),
                player_servers: HashMap::new(),
                self_profile: None,
            }),
            damage_generation: AtomicI64::new(0),
            last_damage_ms: AtomicI64::new(NEVER_MS),
            last_zone_reset_ms: AtomicI64::new(NEVER_MS),
            combat_reset_requested: AtomicBool::new(false),
            before_reset: RwLock::new(None),
        }
    }

    /// Called when a self/world teleport (zone-change opcode) is seen. Resets
    /// combat data only if not in active combat (lull) and not recently reset
    /// (debounce), so the meter starts clean on entering a dungeon/instance
    /// without ever wiping an in-progress fight. Returns true if it reset.
    pub fn note_zone_change(&self) -> bool {
        let reset = self.zone_change_resets();
        if reset {
            self.run_before_reset();
        }
        // Every load leaves the instance behind, mid-fight or not. The roster
        // names the new one again within seconds if it is an instance; it never
        // sends 0 for the open world, so nothing else clears the id.
        self.inner.write().current_dungeon_id = 0;
        if !reset {
            return false;
        }
        // Preserve identity across the reset: a teleport within the same instance
        // keeps everyone's entity ids, so wiping nicknames/known-players/summons
        // would drop your party (and you) to raw ids until they happen to be
        // re-broadcast. Clear only the per-segment damage aggregates.
        self.flush_combat_only();
        self.combat_reset_requested.store(true, Ordering::Relaxed);
        tracing::info!("Zone change detected — combat data reset (identity preserved)");
        true
    }

    /// Whether a zone change now clears combat: not mid-fight, not just after
    /// another reset, and only with something to clear.
    fn zone_change_resets(&self) -> bool {
        let now = now_ms();
        if now.saturating_sub(self.last_damage_ms.load(Ordering::Relaxed)) < ZONE_RESET_LULL_MS {
            return false; // mid-combat teleport — ignore
        }
        if now.saturating_sub(self.last_zone_reset_ms.load(Ordering::Relaxed)) < ZONE_RESET_DEBOUNCE_MS {
            return false; // already reset moments ago
        }
        if self.inner.read().target_combat.is_empty() {
            return false; // nothing to clear
        }
        self.last_zone_reset_ms.store(now, Ordering::Relaxed);
        true
    }

    /// Have `hook` run before every automatic combat reset (zone change, end of
    /// a party). Combat data used to be cleared without saving: leaving an
    /// instance right after a kill lost everything since the last auto-save
    /// (issue #19).
    pub fn set_before_reset(&self, hook: impl Fn() + Send + Sync + 'static) {
        *self.before_reset.write() = Some(Arc::new(hook));
    }

    fn run_before_reset(&self) {
        let hook = self.before_reset.read().clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    /// When the last zone-change combat reset happened (clock ms), `NEVER_MS`
    /// if it has not.
    pub fn last_zone_reset_ms(&self) -> i64 {
        self.last_zone_reset_ms.load(Ordering::Relaxed)
    }

    /// Consumed by the dps calculator to drop its cached snapshot/saved-target
    /// state after a zone-change combat reset.
    pub fn take_combat_reset_requested(&self) -> bool {
        self.combat_reset_requested.swap(false, Ordering::Relaxed)
    }

    pub fn damage_generation(&self) -> i64 {
        self.damage_generation.load(Ordering::Relaxed)
    }

    pub fn set_local_character_name(&self, name: Option<String>) {
        self.inner.write().local_character_name = name;
    }

    pub fn local_character_name(&self) -> Option<String> {
        self.inner.read().local_character_name.clone()
    }

    /// Record who the game says the local player is. `name` is `None` for a
    /// tutorial character. Returns whether anything changed.
    pub fn set_local_identity_from_game(&self, id: i64, name: Option<String>) -> bool {
        let mut inner = self.inner.write();
        let changed = !inner.local_identity_from_game
            || inner.local_player_id != Some(id)
            || inner.local_character_name != name
            || inner.loot_identity.applied;
        inner.loot_identity.applied = false;
        set_game_identity(&mut inner, id, name);
        changed
    }

    /// The loot from a mob that just died belongs to `owner_id`, named `name`.
    ///
    /// The self record that names you arrives on login and zone loads, so a
    /// meter started mid-session can go a long while without it. Loot records
    /// fill that gap: they name the player whose kill it was, and mostly that
    /// is you. Not always: a kill by someone nearby reaches you too, inside
    /// another packet (2026-10-04, "Deityclaire" a second into a capture whose
    /// player was Naicha). So they are a vote. Until the self record arrives,
    /// you are the owner named most often, while that owner leads outright;
    /// a tie means not knowing. A configured name already matched to a player
    /// in the world is kept. Returns whether the local identity changed.
    ///
    /// Taking the first name and ignoring loot records for good once a second
    /// appeared left that capture with no local player at all.
    pub fn note_loot_owner(&self, mob_id: i32, owner_id: i32, name: &str) -> bool {
        let mut inner = self.inner.write();
        let loot = &mut inner.loot_identity;
        let entry = loot.owners.entry(name.to_string()).or_insert_with(|| (owner_id, HashSet::new()));
        entry.0 = owner_id;
        if entry.1.len() < 10_000 {
            entry.1.insert(mob_id);
        }
        // Only you and your party: a stranger farming nearby out-killed the
        // player in one capture, 3 to 2, and took over as "you".
        let scope = &loot.party_scope;
        let mut ranked: Vec<(&String, i32, usize)> = loot
            .owners
            .iter()
            .filter(|(_, (id, _))| scope.contains(id))
            .map(|(n, (id, kills))| (n, *id, kills.len()))
            .collect();
        ranked.sort_by(|a, b| b.2.cmp(&a.2));
        let leader = match ranked.as_slice() {
            [] => return false, // nobody of yours yet: not evidence either way
            [first] => Some((first.0.clone(), first.1)),
            [first, second, ..] if first.2 > second.2 => Some((first.0.clone(), first.1)),
            _ => None,
        };
        let Some((name, owner_id)) = leader else {
            let was_applied = std::mem::take(&mut loot.applied);
            tracing::info!("loot records name several players equally; not using them to identify you");
            if was_applied {
                // Back to not knowing: the UI's name and the self record decide.
                inner.local_identity_from_game = false;
                inner.local_player_id = None;
                return true;
            }
            return false;
        };
        let name = name.as_str();
        if inner.local_identity_from_game && !inner.loot_identity.applied {
            return false; // the self record has spoken
        }
        let configured_and_found = inner.local_player_id.is_some_and(|id| {
            let configured = inner.local_character_name.as_deref().map(str::trim);
            configured.is_some() && inner.nickname_storage.get(&(id as i32)).map(String::as_str) == configured
        });
        if configured_and_found && inner.local_character_name.as_deref().map(str::trim) != Some(name) {
            return false;
        }
        if inner.local_identity_from_game
            && inner.local_player_id == Some(owner_id as i64)
            && inner.local_character_name.as_deref() == Some(name)
        {
            return false;
        }
        inner.loot_identity.applied = true;
        set_game_identity(&mut inner, owner_id as i64, Some(name.to_string()));
        true
    }

    /// The server sent a `06 38` record about `entity_id`.
    ///
    /// Measured on every capture at hand: in the Global ones it names the
    /// local player hundreds of times and players nearby never; in older
    /// Korean/Taiwanese ones, party members too. So it marks you and your
    /// party, which is what tells your loot from a stranger's.
    pub fn note_party_scope(&self, entity_id: i32) {
        if !(100..=9_999_999).contains(&entity_id) {
            return;
        }
        let mut inner = self.inner.write();
        let scope = &mut inner.loot_identity.party_scope;
        if scope.len() < 10_000 {
            scope.insert(entity_id);
        }
    }

    /// `name`'s home server, as a self or loot record states it.
    pub fn note_player_server(&self, name: &str, server_id: u16) {
        if !(1000..3000).contains(&server_id) {
            return;
        }
        let mut inner = self.inner.write();
        // Bounded: one entry per character met, and a session meets hundreds.
        if inner.player_servers.len() < 10_000 || inner.player_servers.contains_key(name) {
            inner.player_servers.insert(name.to_string(), server_id);
        }
    }

    /// Your class and level, as your own self record states them.
    ///
    /// A record whose level did not read (a partial copy, the scan having met
    /// one in still-compressed bytes) keeps the level already known for that
    /// character rather than erasing it.
    pub fn note_self_profile(&self, name: &str, class: Option<JobClass>, level: Option<u32>) {
        let mut inner = self.inner.write();
        let (old_class, old_level) = match &inner.self_profile {
            Some((n, c, l)) if n == name => (*c, *l),
            _ => (None, None),
        };
        inner.self_profile = Some((name.to_string(), class.or(old_class), level.or(old_level)));
    }

    /// Who you are playing, as far as the game has said: name, server, class
    /// and level. Class falls back to the one your skills show, for a meter
    /// started before the self record came; level has no such fallback.
    pub fn local_profile(&self) -> LocalProfile {
        let server = self.fight_server_id();
        let inner = self.inner.read();
        let name = inner.local_character_name.clone();
        let (mut class, mut level) = (None, None);
        if let (Some(n), Some((pn, pc, pl))) = (name.as_deref(), inner.self_profile.as_ref()) {
            if n.trim() == pn.trim() {
                class = *pc;
                level = *pl;
            }
        }
        if class.is_none() {
            class = inner.local_player_id.and_then(|id| inner.actor_jobs.get(&(id as i32)).copied());
        }
        LocalProfile { name, server_id: server, class, level }
    }

    /// The server the fights being recorded are on: the local player's, else
    /// the party's. 0 when nothing has said.
    ///
    /// A server id names its region (`1304` is Europe), which is what this is
    /// for: uploaded logs are grouped by region. The local player's own server
    /// comes from the self record (or a loot record naming them); the roster's
    /// is the fallback, by majority, since in a cross-server party each member
    /// keeps their own server but all share the region.
    pub fn fight_server_id(&self) -> u16 {
        let inner = self.inner.read();
        let local = inner.local_character_name.as_deref().map(str::trim);
        if let Some(&server) = local.and_then(|n| inner.player_servers.get(n)) {
            return server;
        }
        if let Some(member) = local.and_then(|n| inner.party_members.get(n)) {
            if member.server_id != 0 {
                return member.server_id;
            }
        }
        let mut counts: HashMap<u16, usize> = HashMap::new();
        for member in inner.party_members.values() {
            if member.server_id != 0 {
                *counts.entry(member.server_id).or_default() += 1;
            }
        }
        counts.into_iter().max_by_key(|&(server, n)| (n, std::cmp::Reverse(server))).map_or(0, |(s, _)| s)
    }

    /// Whether the game itself named the local player (the self record or the
    /// character list), not a guess from loot records. Only this may stand
    /// over a name the player typed (issue #13): a loot guess can be a
    /// bystander's kill.
    pub fn local_identity_from_self_record(&self) -> bool {
        let inner = self.inner.read();
        inner.local_identity_from_game && !inner.loot_identity.applied
    }

    /// Whether the local player's identity came from the game rather than from
    /// the UI (window title, settings, a remembered name).
    pub fn local_identity_from_game(&self) -> bool {
        self.inner.read().local_identity_from_game
    }

    /// Replace the supporter roster. Called after each download.
    pub fn set_supporters(&self, roster: crate::supporters::Roster) {
        self.inner.write().supporters = std::sync::Arc::new(roster);
    }

    pub fn supporters(&self) -> std::sync::Arc<crate::supporters::Roster> {
        self.inner.read().supporters.clone()
    }

    pub fn set_local_player_id(&self, id: Option<i64>) {
        self.inner.write().local_player_id = id;
    }

    pub fn local_player_id(&self) -> Option<i64> {
        self.inner.read().local_player_id
    }

    pub fn append_damage(&self, pdp: ParsedDamagePacket) {
        let mut inner = self.inner.write();
        let skill_code = pdp.skill_code();
        let actor_id = pdp.actor_id();
        let target_id = pdp.target_id();

        // Not damage: a spirit and its owner naming each other.
        if let Some((summon, owner)) = owner_link(skill_code, actor_id, target_id) {
            if link_summon(&mut inner, summon, owner) {
                tracing::debug!("Summon {} linked to owner {} by skill {}", summon, owner, skill_code);
                self.damage_generation.fetch_add(1, Ordering::Relaxed);
            }
            return;
        }

        // NPC actors using NPC skills: track damage received on the player target, then skip
        let uses_npc_skill = (1_000_000..=9_999_999).contains(&skill_code);
        if inner.mob_storage.contains_key(&actor_id)
            && !inner.summon_storage.contains_key(&actor_id)
            && uses_npc_skill
        {
            // Track damage received on the player target
            let resolved_target = summon_resolver::resolve(target_id, &inner.summon_storage);
            if inner.known_player_ids.contains(&resolved_target) {
                let dmg = pdp.total_damage() as i64;
                for target_data in inner.target_combat.values_mut() {
                    if let Some(actor_data) = target_data.actors.get_mut(&resolved_target) {
                        actor_data.damage_received += dmg;
                        actor_data.hits_received += 1;
                        break;
                    }
                }
            }
            return;
        }

        // Track player skill usage. Exclude anything that spawned via a mob/summon
        // spawn (40/41 36) or has an owner: a real player never does, so such an
        // entity dealing class-band damage is a summon / spell-effect, not a player.
        if is_player_skill(skill_code)
            && !inner.confirmed_summon_ids.contains(&actor_id)
            && !inner.summon_spawn_ids.contains(&actor_id)
            && !inner.summon_storage.contains_key(&actor_id)
            && inner.known_player_ids.insert(actor_id)
        {
            purge_friendly_damage(&mut inner, actor_id);
        }

        // Party healing: player-on-player damage is actually healing/buffs
        if is_friendly_action(&inner, actor_id, target_id) {
            let heal_amount = pdp.total_damage();
            if heal_amount > 0 {
                // Record party heal on the actor's data in all targets they appear in
                for target_data in inner.target_combat.values_mut() {
                    if let Some(actor_data) = target_data.actors.get_mut(&actor_id) {
                        actor_data.party_heal += heal_amount as i64;
                        break;
                    }
                }
                // Also record per-skill so ally heals show in the HEAL view (the
                // self-heal path does this via append_heal; mirror it for ally heals).
                let e = inner
                    .heal_storage
                    .entry(actor_id)
                    .or_default()
                    .entry((pdp.skill_code(), false))
                    .or_default();
                e.total_heal += heal_amount as i64;
                e.tick_count += 1;
            }
            return;
        }

        // Track hostile targets
        let resolved = summon_resolver::resolve(actor_id, &inner.summon_storage);
        if inner.known_player_ids.contains(&resolved) {
            inner.hostile_target_ids.insert(target_id);
        }

        // Track actor job
        if let Some(job) = JobClass::convert_from_skill(skill_code) {
            inner.actor_jobs.entry(actor_id).or_insert(job);
        }

        // Boss encounter auto-reset: if this target is a boss and the current
        // segment has no boss yet, clear the trash segment so boss gets clean data.
        let is_boss_target = inner.boss_entity_ids.contains(&target_id);
        if is_boss_target && !inner.has_boss_in_segment && !inner.target_combat.is_empty() {
            tracing::info!("Boss encounter auto-reset: boss entity {} hit, clearing trash segment", target_id);
            inner.target_combat.clear();
            inner.held_dot_ticks.clear();
            inner.dead_entity_ids.clear();
            inner.has_boss_in_segment = true;
        } else if is_boss_target {
            inner.has_boss_in_segment = true;
        }

        if inner.training_dummy_ids.contains(&target_id) {
            let key = (target_id, actor_id);
            if pdp.is_dot() {
                inner.held_dot_ticks.entry(key).or_default().push(pdp);
                return;
            }
            // A direct hit: the DoT ticks since the last one count after all.
            for tick in inner.held_dot_ticks.remove(&key).unwrap_or_default() {
                apply_damage(&mut inner, &tick);
            }
        }
        apply_damage(&mut inner, &pdp);

        self.damage_generation.fetch_add(1, Ordering::Relaxed);
        self.last_damage_ms.store(now_ms(), Ordering::Relaxed);

        // Apply pending nickname
        apply_pending_nickname(&mut inner, actor_id);

        inner.damage_since_roster_bind += 1;
        if inner.damage_since_roster_bind >= ROSTER_BIND_EVERY {
            inner.damage_since_roster_bind = 0;
            bind_roster_names_by_class(&mut inner);
        }
    }

    pub fn append_mob(&self, mid: i32, code: i32) {
        let mut inner = self.inner.write();
        inner.mob_storage.insert(mid, code);

        // NPC unclassification: if this entity was previously classified as a player
        // (damage with player-band skills arrived before the 0x3640 spawn packet),
        // undo the classification and scrub ghost player damage from aggregates.
        if inner.known_player_ids.remove(&mid) {
            tracing::trace!("NPC unclassification: entity {} reclassified as mob (code {})", mid, code);
            // Subtract ghost player damage from target totals
            for target_data in inner.target_combat.values_mut() {
                if let Some(actor_data) = target_data.actors.remove(&mid) {
                    target_data.total_damage -= actor_data.total_damage;
                }
            }
        }
    }

    pub fn append_mob_hp(&self, mid: i32, hp: i32) {
        if hp > 0 {
            self.inner.write().mob_hp_data.insert(mid, hp);
        }
    }

    pub fn mark_entity_dead(&self, entity_id: i32) {
        self.inner.write().dead_entity_ids.insert(entity_id);
    }

    pub fn is_entity_dead(&self, entity_id: i32) -> bool {
        self.inner.read().dead_entity_ids.contains(&entity_id)
    }

    pub fn get_dead_entities(&self) -> HashSet<i32> {
        self.inner.read().dead_entity_ids.clone()
    }

    pub fn register_boss(&self, entity_id: i32) {
        self.inner.write().boss_entity_ids.insert(entity_id);
    }

    /// An entity the NPC table calls a training dummy. See `held_dot_ticks`.
    pub fn register_training_dummy(&self, entity_id: i32) {
        self.inner.write().training_dummy_ids.insert(entity_id);
    }

    pub fn is_boss(&self, entity_id: i32) -> bool {
        self.inner.read().boss_entity_ids.contains(&entity_id)
    }

    pub fn is_mob(&self, id: i32) -> bool {
        self.inner.read().mob_storage.contains_key(&id)
    }

    pub fn is_damage_target(&self, id: i32) -> bool {
        self.inner.read().target_combat.contains_key(&id)
    }

    pub fn is_summon(&self, id: i32) -> bool {
        self.inner.read().summon_storage.contains_key(&id)
    }

    pub fn is_confirmed_summon(&self, id: i32) -> bool {
        self.inner.read().confirmed_summon_ids.contains(&id)
    }

    /// Record that `id` appeared in a `40/41 36` mob/summon spawn (never a player
    /// spawn). Used to keep summon / spell-effect entities out of the known-player
    /// set and make them attributable to their same-class player.
    ///
    /// The game reuses entity ids, so a spawn is a new entity: whatever owner,
    /// summon or player mark the id had belongs to the one before. Entity 65746
    /// was one player's pet and then, 14 minutes later, another Spiritmaster's
    /// spirit, whose 7,032 damage went to the first owner (2026-10-04).
    pub fn note_summon_spawn(&self, id: i32) {
        let mut inner = self.inner.write();
        forget_entity(&mut inner, id);
        // A name the game gave this id outranks a spawn read out of a scan.
        if !inner.authoritative_name_ids.contains(&id) {
            inner.known_player_ids.remove(&id);
        }
        inner.summon_spawn_ids.insert(id);
    }

    /// A `44/45 36` player spawn for `id`: a player, not anyone's summon.
    pub fn note_player_spawn(&self, id: i32) {
        forget_entity(&mut self.inner.write(), id);
    }

    pub fn get_summon_spawn_ids(&self) -> HashSet<i32> {
        self.inner.read().summon_spawn_ids.clone()
    }

    /// Confirm that a sub-100 entity id is a real entity (seen in a spawn or an
    /// identity record), so the damage parser's `>= 100` resync gate lets it
    /// through. See `Inner::low_id_entities`.
    pub fn note_low_id_entity(&self, id: i32) {
        if (1..100).contains(&id) {
            self.inner.write().low_id_entities.insert(id);
        }
    }

    /// True when `id` passes the entity-id sanity gate used while walking damage
    /// records: anything at or above the usual floor, plus tiny ids the game has
    /// explicitly announced.
    pub fn is_plausible_entity_id(&self, id: i32) -> bool {
        if id >= 100 {
            return true;
        }
        id >= 1 && self.inner.read().low_id_entities.contains(&id)
    }

    /// Take a party roster from a `0x9702` packet.
    ///
    /// `complete` says whether every member the packet declared was decoded. A
    /// complete roster replaces what we had, so a member who left the party
    /// disappears; a partial one only updates the members it did decode, so a
    /// record this parser trips over costs that member a refresh rather than
    /// costing the whole party their combat power.
    pub fn set_party_roster(&self, members: Vec<(String, PartyMember)>, complete: bool) {
        if members.is_empty() {
            return;
        }
        let mut inner = self.inner.write();
        inner.party_roster_at_ms = now_ms();
        inner.party_placeholders_hidden = false;

        // Leaving, being kicked, or the party disbanding all show up the same way:
        // the next complete roster has you on your own. Going from a real party
        // down to one member means the party is over, so drop the roster rows and
        // ask for a combat reset — otherwise the meter keeps showing teammates who
        // are no longer with you.
        let was_in_party = inner.party_members.len() >= 2;
        let now_alone = complete && members.len() <= 1;
        let local_name = inner.local_character_name.clone();
        let dropped_self = complete
            && local_name.as_ref().is_some_and(|n| {
                !n.trim().is_empty() && !members.iter().any(|(name, _)| name.trim() == n.trim())
            });

        if was_in_party && (now_alone || dropped_self) {
            tracing::info!(
                "Party ended ({} -> {} members) — clearing party rows",
                inner.party_members.len(),
                members.len()
            );
            // Save the party's fights while the roster and instance still say
            // whose they are; the hook reads this storage, so no lock is held.
            drop(inner);
            self.run_before_reset();
            let mut inner = self.inner.write();
            inner.party_members.clear();
            inner.current_dungeon_id = 0;
            drop(inner);
            self.flush_combat_only();
            self.combat_reset_requested.store(true, Ordering::Relaxed);
            return;
        }

        if complete {
            inner.party_members.clear();
        }
        for (name, member) in members {
            inner.party_members.insert(name, member);
        }
        bind_roster_names_by_class(&mut inner);
    }

    pub fn set_current_dungeon(&self, dungeon_id: i32) {
        if dungeon_id > 0 {
            self.inner.write().current_dungeon_id = dungeon_id;
        }
    }

    pub fn current_dungeon_id(&self) -> i32 {
        self.inner.read().current_dungeon_id
    }

    pub fn get_party_members(&self) -> HashMap<String, PartyMember> {
        self.inner.read().party_members.clone()
    }

    /// Whether party members who have not fought should still be shown, with
    /// 0 damage, as a reminder of who is in the party.
    ///
    /// The game sends a roster on every party change, but nothing reliable
    /// when you go off on your own afterwards, so a dungeon party could stay
    /// on the meter long after (a player saw theirs 15 minutes on, through
    /// resets). These rows are for the start of a run: they show for
    /// `PARTY_PLACEHOLDER_MS` after the last roster, and a manual reset clears
    /// them until the next one. Members who fight are shown regardless.
    pub fn party_placeholders_wanted(&self) -> bool {
        let inner = self.inner.read();
        !inner.party_placeholders_hidden
            && now_ms() - inner.party_roster_at_ms < PARTY_PLACEHOLDER_MS
    }

    /// The player reset the meter: stop showing party members who have not
    /// fought, until the game sends the next roster.
    pub fn hide_party_placeholders(&self) {
        self.inner.write().party_placeholders_hidden = true;
    }

    /// Record a power-scalar reading for an actor. See `Inner::actor_power_scalars`.
    pub fn note_power_scalar(&self, actor_id: i32, scalar: i32) {
        if actor_id <= 0 || scalar <= 0 {
            return;
        }
        let mut inner = self.inner.write();
        let set = inner.actor_power_scalars.entry(actor_id).or_default();
        // Bounded: buff churn produces a handful of distinct values, not many.
        if set.len() < 16 {
            set.insert(scalar);
        }
    }

    pub fn get_power_scalars(&self) -> HashMap<i32, HashSet<i32>> {
        self.inner.read().actor_power_scalars.clone()
    }

    pub fn register_confirmed_summon_by_id(&self, summon_id: i32, owner_id: i32) {
        tracing::trace!("Summon confirmed (5F 00): {} owned by {}", summon_id, owner_id);
        if link_summon(&mut self.inner.write(), summon_id, owner_id) {
            self.damage_generation.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn append_summon(&self, summoner: i32, summon: i32) {
        let mut inner = self.inner.write();

        // Guards from Kotlin
        if inner.nickname_storage.contains_key(&summon) { return; }
        if inner.known_player_ids.contains(&summon) { return; }
        if inner.hostile_target_ids.contains(&summon) { return; }
        if inner.summon_storage.contains_key(&summoner) { return; }
        if inner.mob_storage.contains_key(&summoner) && !inner.summon_storage.contains_key(&summoner) { return; }

        // Job compatibility check
        let summon_job = inner.actor_jobs.get(&summon).copied();
        let owner_job = inner.actor_jobs.get(&summoner).copied();
        if let (Some(sj), Some(oj)) = (summon_job, owner_job) {
            if sj != oj { return; }
        }

        tracing::debug!("Summon linked: {} owned by {}", summon, summoner);
        inner.summon_storage.insert(summon, summoner);
    }

    /// Bind a nickname from a LOWER-CONFIDENCE source (fuzzy actor-name rules,
    /// loot attribution, nickname scan). Gated so it can't corrupt naming:
    ///  1. It may only name an id that is already a real entity — seen in combat,
    ///     a known player, spawn-authoritative, a summon, or already named. This
    ///     rejects names being bound to counter/terminator-derived junk ids (e.g.
    ///     the sequence counter in a `0E 00 36 <counter>` record), which would
    ///     otherwise evict a correct spawn name via the name-eviction rule.
    ///  2. It may not steal a name that an authoritative source already bound to a
    ///     different id.
    pub fn append_nickname(&self, uid: i32, nickname: &str) {
        let mut inner = self.inner.write();
        if !fuzzy_bind_allowed(&inner, uid, nickname) {
            return;
        }
        append_nickname_inner(&mut inner, uid, nickname);
    }

    /// Bind a nickname from an AUTHORITATIVE source (a masked identity record, a
    /// 45/44 36 player spawn, or the account char-list). Not gated — the id↔name
    /// pairing is stated by the protocol — and marks the id so fuzzy parsers
    /// can't later steal the name. A stale/junk prior binding of this name is
    /// evicted, reclaiming the name to the real id.
    ///
    /// Applied with `force`, so the length/script heuristics that protect against
    /// bad fuzzy scan results cannot reject a real name. Those heuristics cost a
    /// live capture its Ranger: a fuzzy parser had bound the LEGION name
    /// "BaroqueWorks" to that player, and the "don't replace a longer name with a
    /// short ASCII one" rule then refused their actual name, "M7".
    pub fn append_nickname_authoritative(&self, uid: i32, nickname: &str) {
        let mut inner = self.inner.write();
        inner.authoritative_name_ids.insert(uid);
        append_nickname_inner_with_force(&mut inner, uid, nickname, true);
    }

    pub fn set_permanent_nickname(&self, uid: i32, nickname: &str) {
        let mut inner = self.inner.write();
        inner.permanent_nicknames.insert(uid, nickname.to_string());
        // User explicitly set this in settings: authoritative, and force-apply to
        // bypass length/CJK heuristics that protect against bad packet scan results.
        inner.authoritative_name_ids.insert(uid);
        append_nickname_inner_with_force(&mut inner, uid, nickname, true);
    }

    pub fn cache_pending_nickname(&self, uid: i32, nickname: &str) {
        let mut inner = self.inner.write();
        if inner.nickname_storage.contains_key(&uid) { return; }
        inner.pending_nicknames.insert(uid, nickname.to_string());
    }

    pub fn has_nickname(&self, uid: i32) -> bool {
        self.inner.read().nickname_storage.contains_key(&uid)
    }

    pub fn get_nickname(&self, uid: i32) -> Option<String> {
        self.inner.read().nickname_storage.get(&uid).cloned()
    }

    /// Reverse lookup: find entity ID by nickname (for summon owner resolution).
    pub fn find_id_by_nickname(&self, name: &str) -> Option<i32> {
        let inner = self.inner.read();
        for (&id, nick) in &inner.nickname_storage {
            if nick == name {
                return Some(id);
            }
        }
        None
    }

    pub fn actor_appears_in_combat(&self, actor_id: i32) -> bool {
        let inner = self.inner.read();
        // Check if actor appears as an attacker in any target
        for target_data in inner.target_combat.values() {
            if target_data.actors.contains_key(&actor_id) {
                return true;
            }
        }
        // Check if actor is a target
        if inner.target_combat.contains_key(&actor_id) {
            return true;
        }
        inner.summon_storage.contains_key(&actor_id)
    }

    pub fn get_nicknames(&self) -> HashMap<i32, String> {
        self.inner.read().nickname_storage.clone()
    }

    pub fn get_summon_data(&self) -> HashMap<i32, i32> {
        self.inner.read().summon_storage.clone()
    }

    pub fn get_known_player_ids(&self) -> HashSet<i32> {
        self.inner.read().known_player_ids.clone()
    }

    pub fn is_known_player(&self, id: i32) -> bool {
        self.inner.read().known_player_ids.contains(&id)
    }

    pub fn get_mob_hp_data(&self) -> HashMap<i32, i32> {
        self.inner.read().mob_hp_data.clone()
    }

    pub fn get_mob_hp(&self, id: i32) -> Option<i32> {
        self.inner.read().mob_hp_data.get(&id).copied()
    }

    /// Record a live current-HP reading for an entity (from the `8D ... 02 01 00`
    /// feed). Also seeds/raises the entity's MAX HP from the observed peak, so a
    /// boss whose spawn packet was missed still gets a usable denominator (current
    /// HP never exceeds max in-game, so taking the max never overstates it).
    pub fn set_mob_current_hp(&self, id: i32, hp: i32) {
        if hp < 0 {
            return;
        }
        let mut inner = self.inner.write();
        inner.mob_current_hp.insert(id, hp);
        let max = inner.mob_hp_data.entry(id).or_insert(0);
        if hp > *max {
            *max = hp;
        }
    }

    pub fn get_mob_current_hp(&self, id: i32) -> Option<i32> {
        self.inner.read().mob_current_hp.get(&id).copied()
    }

    /// Record a heal tick done by `actor_id` with `skill_code` (is_hot marks a HoT).
    /// Keyed by the healer so "healing done" can be shown per player. Self-heals count.
    pub fn append_heal(&self, actor_id: i32, skill_code: i32, amount: i64, is_hot: bool) {
        if amount <= 0 || !self.is_plausible_entity_id(actor_id) {
            return;
        }
        let mut inner = self.inner.write();
        let e = inner
            .heal_storage
            .entry(actor_id)
            .or_default()
            .entry((skill_code, is_hot))
            .or_default();
        e.total_heal += amount;
        e.tick_count += 1;
    }

    pub fn get_heal_snapshot(&self) -> HashMap<i32, HashMap<(i32, bool), HealSkillData>> {
        self.inner.read().heal_storage.clone()
    }

    pub fn get_mob_data(&self) -> HashMap<i32, i32> {
        self.inner.read().mob_storage.clone()
    }

    pub fn set_current_target(&self, target: i32) {
        self.inner.write().current_target = target;
    }

    pub fn current_target(&self) -> i32 {
        self.inner.read().current_target
    }

    /// Get a snapshot of all target combat aggregates.
    /// This is cheap: clones a small map of aggregates, not raw packets.
    pub fn get_combat_snapshot(&self) -> HashMap<i32, TargetCombatData> {
        self.inner.read().target_combat.clone()
    }

    /// Like `get_combat_snapshot` but without per-skill `hit_timestamps`.
    /// `hit_timestamps` grows unbounded over a fight and is only needed by
    /// `get_target_details`. The 500ms hot paths (`get_dps`,
    /// `get_details_context`, boss auto-save) never read it, so this keeps
    /// their per-tick clone cost flat over fight duration instead of growing
    /// linearly — the root cause of the long-fight FPS drops.
    pub fn get_combat_snapshot_light(&self) -> HashMap<i32, TargetCombatData> {
        let inner = self.inner.read();
        inner
            .target_combat
            .iter()
            .map(|(&tid, td)| {
                let actors = td
                    .actors
                    .iter()
                    .map(|(&aid, ad)| {
                        let skills = ad
                            .skills
                            .iter()
                            .map(|(&k, sd)| (k, sd.clone_light()))
                            .collect();
                        (
                            aid,
                            ActorCombatData {
                                total_damage: ad.total_damage,
                                party_heal: ad.party_heal,
                                regen: ad.regen,
                                damage_received: ad.damage_received,
                                hits_received: ad.hits_received,
                                last_damage_time: ad.last_damage_time,
                                job: ad.job,
                                skills,
                            },
                        )
                    })
                    .collect();
                (
                    tid,
                    TargetCombatData {
                        target_id: td.target_id,
                        total_damage: td.total_damage,
                        first_damage_time: td.first_damage_time,
                        last_damage_time: td.last_damage_time,
                        last_packet_id: td.last_packet_id,
                        actors,
                        dungeon_id: td.dungeon_id,
                    },
                )
            })
            .collect()
    }

    /// Clear combat. Who owns which summon is kept: a summon is linked when it
    /// spawns or sends its owner a link record, and one that did that before the
    /// reset would otherwise stay unlinked for good. Ids that are reused get
    /// their links dropped by the new spawn (see `note_summon_spawn`).
    pub fn flush(&self) {
        let mut inner = self.inner.write();
        inner.target_combat.clear();
        inner.held_dot_ticks.clear();
        inner.training_dummy_ids.clear();
        inner.actor_jobs.clear();
        inner.known_player_ids.clear();
        inner.actor_power_scalars.clear();
        inner.hostile_target_ids.clear();
        inner.dead_entity_ids.clear();
        inner.has_boss_in_segment = false;
        inner.mob_hp_data.clear();
        inner.mob_current_hp.clear();
        inner.heal_storage.clear();
        inner.current_target = 0;
    }

    /// Clear only the per-segment combat/damage aggregates, preserving player
    /// identity: nicknames, known-player ids, summon ownership, and job classes.
    /// Used on a zone-change / in-instance-teleport reset so the meter starts on
    /// clean numbers without dropping who your party and you are.
    pub fn flush_combat_only(&self) {
        let mut inner = self.inner.write();
        inner.target_combat.clear();
        inner.held_dot_ticks.clear();
        inner.hostile_target_ids.clear();
        inner.dead_entity_ids.clear();
        inner.has_boss_in_segment = false;
        inner.mob_hp_data.clear();
        inner.mob_current_hp.clear();
        inner.heal_storage.clear();
        inner.current_target = 0;
    }

    /// Drop every summon link, for a capture from another session (a replay),
    /// whose entity ids mean something else.
    pub fn forget_summon_links(&self) {
        let mut inner = self.inner.write();
        inner.summon_storage.clear();
        inner.confirmed_summon_ids.clear();
        inner.summon_spawn_ids.clear();
    }

    pub fn reset_nicknames(&self) {
        let mut inner = self.inner.write();
        inner.nickname_storage.clear();
        inner.pending_nicknames.clear();
        inner.authoritative_name_ids.clear();
        let permanent: Vec<(i32, String)> = inner.permanent_nicknames.iter().map(|(&k, v)| (k, v.clone())).collect();
        for (uid, nick) in permanent {
            inner.nickname_storage.insert(uid, nick);
        }
    }
}

/// Gate for lower-confidence nickname bindings (see `append_nickname`).
fn fuzzy_bind_allowed(inner: &Inner, uid: i32, nickname: &str) -> bool {
    // (1) The id must already be a real entity. A counter/terminator-derived
    // junk id (e.g. `0E 00 36 <counter>`) never appears in combat, is never a
    // known player/summon, was never spawned, and has no name yet — so it is
    // rejected here, and can no longer evict a correct spawn name.
    let is_real_entity = inner.nickname_storage.contains_key(&uid)
        || inner.known_player_ids.contains(&uid)
        || inner.authoritative_name_ids.contains(&uid)
        || inner.summon_storage.contains_key(&uid)
        || inner.target_combat.contains_key(&uid)
        || inner
            .target_combat
            .values()
            .any(|t| t.actors.contains_key(&uid));
    if !is_real_entity {
        return false;
    }
    // (2) Don't let a fuzzy source steal a name an authoritative source already
    // bound to a different id.
    for (&id, name) in &inner.nickname_storage {
        if id != uid && name == nickname && inner.authoritative_name_ids.contains(&id) {
            return false;
        }
    }
    // (3) Don't let a fuzzy source rename an id the protocol already named. The
    // spawn packets carry the owner's LEGION name a few fields past their
    // character name, and the loose scanners happily bind that to the player —
    // which is how a live capture ended up showing a legion ("BaroqueWorks")
    // where a Ranger's name should have been.
    if inner.authoritative_name_ids.contains(&uid)
        && inner.nickname_storage.get(&uid).is_some_and(|n| n != nickname)
    {
        return false;
    }
    true
}

fn has_cjk(s: &str) -> bool {
    s.chars().any(|ch| {
        let cp = ch as u32;
        (0x4E00..=0x9FFF).contains(&cp) || (0xAC00..=0xD7AF).contains(&cp)
        || (0x3400..=0x4DBF).contains(&cp) || (0x20000..=0x2A6DF).contains(&cp)
        || (0x1100..=0x11FF).contains(&cp)
    })
}

/// Count one damage record into its target's and actor's aggregates.
fn apply_damage(inner: &mut Inner, pdp: &ParsedDamagePacket) {
    let skill_code = pdp.skill_code();
    let actor_id = pdp.actor_id();
    let target_id = pdp.target_id();
    let timestamp = pdp.timestamp();
    let packet_id = pdp.id();

    // Get or create target combat data
    let dungeon_id = inner.current_dungeon_id;
    let target_data = inner.target_combat.entry(target_id).or_insert_with(|| {
        TargetCombatData::new(target_id, timestamp)
    });

    // Idle reset check (30s gap)
    if target_data.last_damage_time > 0
        && timestamp - target_data.last_damage_time > IDLE_RESET_MS
    {
        tracing::info!("Idle reset: target {} — gap {}ms", target_id,
            timestamp - target_data.last_damage_time);
        *target_data = TargetCombatData::new(target_id, timestamp);
    }
    // The roster names the instance only once it arrives, so a hit before it
    // leaves the id for a later hit to fill.
    if dungeon_id != 0 {
        target_data.dungeon_id = dungeon_id;
    }

    // Update target timing
    if timestamp < target_data.first_damage_time {
        target_data.first_damage_time = timestamp;
    }
    if timestamp > target_data.last_damage_time {
        target_data.last_damage_time = timestamp;
    }
    let total_dmg = pdp.total_damage();
    target_data.total_damage += total_dmg as i64;
    target_data.last_packet_id = packet_id;

    // Update actor data within target
    let actor_data = target_data.actors.entry(actor_id).or_insert_with(ActorCombatData::new);
    actor_data.total_damage += total_dmg as i64;
    if timestamp > actor_data.last_damage_time {
        actor_data.last_damage_time = timestamp;
    }
    if actor_data.job.is_none() {
        actor_data.job = JobClass::convert_from_skill(skill_code);
    }

    // Update skill data
    let skill_key = (skill_code, pdp.is_dot());
    let skill_data = actor_data.skills.entry(skill_key).or_insert_with(|| {
        SkillCombatData::new(skill_code, pdp.is_dot())
    });
    skill_data.hit_count += 1;
    // saturating_add: per-skill totals are i32 and a long boss fight can
    // exceed i32::MAX — overflow panics in debug and wraps to negative in
    // release. Cap instead of crashing/wrapping.
    skill_data.total_damage = skill_data.total_damage.saturating_add(total_dmg);
    let hit_dmg = pdp.damage();
    if hit_dmg < skill_data.min_damage { skill_data.min_damage = hit_dmg; }
    if hit_dmg > skill_data.max_damage { skill_data.max_damage = hit_dmg; }
    if pdp.is_crit() { skill_data.crit_count += 1; }
    if pdp.specials().contains(&SpecialDamage::Back) { skill_data.back_count += 1; }
    if pdp.specials().contains(&SpecialDamage::Frontal) { skill_data.frontal_count += 1; }
    if pdp.specials().contains(&SpecialDamage::Parry) { skill_data.parry_count += 1; }
    if pdp.specials().contains(&SpecialDamage::Perfect) { skill_data.perfect_count += 1; }
    if pdp.specials().contains(&SpecialDamage::Double) { skill_data.double_count += 1; }
    if pdp.specials().contains(&SpecialDamage::Smite) { skill_data.smite_count += 1; }
    if pdp.specials().contains(&SpecialDamage::PowerShard) { skill_data.powershard_count += 1; }
    if pdp.multi_hit_count() > 0 {
        skill_data.multi_hit_count += 1;
        skill_data.multi_hit_damage = skill_data.multi_hit_damage.saturating_add(pdp.multi_hit_damage());
        skill_data.multi_hit_hits += pdp.multi_hit_count();
    }
    skill_data.heal_amount = skill_data.heal_amount.saturating_add(pdp.heal_amount());
    // Track regen (life-steal) on the actor aggregate
    if pdp.heal_amount() > 0 {
        actor_data.regen += pdp.heal_amount() as i64;
    }
    skill_data.hit_timestamps.push(timestamp);
    for (i, &flag) in pdp.spec_flags().iter().enumerate() {
        if flag { skill_data.spec_flags[i] = true; }
    }
}

/// Skill codes of the records a Spiritmaster's spirit sends its owner (about
/// once a second) and the owner sends its spirits. Damage-shaped `04 38`
/// records, but the amount is no damage, and the pair is the most reliable
/// owner link there is: across five captures (2026-10-04) 1,251 spirits were
/// linked this way and none to two owners in one lifetime. Seen: 16990002,
/// 16990003 and 16770000.
const SPIRIT_TO_OWNER: std::ops::RangeInclusive<i32> = 16_990_000..=16_999_999;
const OWNER_TO_SPIRIT: std::ops::RangeInclusive<i32> = 16_770_000..=16_779_999;

/// `(summon, owner)` if this record is a link record.
fn owner_link(skill_code: i32, actor_id: i32, target_id: i32) -> Option<(i32, i32)> {
    if SPIRIT_TO_OWNER.contains(&skill_code) {
        Some((actor_id, target_id))
    } else if OWNER_TO_SPIRIT.contains(&skill_code) {
        Some((target_id, actor_id))
    } else {
        None
    }
}

/// Link `summon` to `owner` as a confirmed summon. Returns whether anything
/// changed: the link records repeat every second.
fn link_summon(inner: &mut Inner, summon: i32, owner: i32) -> bool {
    if summon <= 0 || owner <= 0 || summon == owner {
        return false;
    }
    if inner.summon_storage.get(&summon) == Some(&owner) && inner.confirmed_summon_ids.contains(&summon) {
        return false;
    }
    // A link through the summon back to itself would hide both.
    if summon_resolver::resolve(owner, &inner.summon_storage) == summon {
        return false;
    }
    // Another owner means another entity under a reused id whose spawn went
    // unseen: what the old one did stays with its owner.
    if inner.summon_storage.get(&summon).is_some_and(|&old| old != owner) {
        forget_entity(inner, summon);
    }
    inner.confirmed_summon_ids.insert(summon);
    inner.known_player_ids.remove(&summon);
    inner.summon_storage.insert(summon, owner);
    purge_friendly_damage(inner, summon);
    true
}

/// A new entity under `id`: drop the old one's owner link and summon marks.
/// What a linked summon did moves onto its owner, where it was shown anyway,
/// so the new entity's owner does not inherit it.
fn forget_entity(inner: &mut Inner, id: i32) {
    inner.confirmed_summon_ids.remove(&id);
    inner.summon_spawn_ids.remove(&id);
    inner.actor_jobs.remove(&id);
    inner.hostile_target_ids.remove(&id);
    let Some(owner) = inner.summon_storage.remove(&id) else { return };
    let owner = summon_resolver::resolve(owner, &inner.summon_storage);
    if owner <= 0 || owner == id {
        return;
    }
    for target in inner.target_combat.values_mut() {
        if let Some(data) = target.actors.remove(&id) {
            target.actors.entry(owner).or_insert_with(ActorCombatData::new).absorb(data);
        }
    }
    if let Some(heals) = inner.heal_storage.remove(&id) {
        let mine = inner.heal_storage.entry(owner).or_default();
        for (key, h) in heals {
            let e = mine.entry(key).or_default();
            e.total_heal += h.total_heal;
            e.tick_count += h.tick_count;
        }
    }
    let held: Vec<(i32, i32)> = inner.held_dot_ticks.keys().filter(|k| k.1 == id).copied().collect();
    for key in held {
        let mut ticks = inner.held_dot_ticks.remove(&key).unwrap_or_default();
        for t in &mut ticks {
            t.set_actor_id(owner);
        }
        inner.held_dot_ticks.entry((key.0, owner)).or_default().extend(ticks);
    }
}

fn set_game_identity(inner: &mut Inner, id: i64, name: Option<String>) {
    inner.local_identity_from_game = true;
    inner.local_player_id = Some(id);
    inner.local_character_name = name;
}

fn append_nickname_inner(inner: &mut Inner, uid: i32, nickname: &str) {
    append_nickname_inner_with_force(inner, uid, nickname, false);
}

fn append_nickname_inner_with_force(inner: &mut Inner, uid: i32, nickname: &str, force: bool) {
    let existing = inner.nickname_storage.get(&uid);
    if let Some(existing) = existing {
        if existing == nickname {
            if let Some(ref local_name) = inner.local_character_name {
                if local_name.trim() == nickname.trim() {
                    inner.local_player_id = Some(uid as i64);
                }
            }
            return;
        }
        if !force {
            // Don't replace a CJK name with a shorter ASCII-only name (likely false positive)
            let existing_cjk = has_cjk(existing);
            let new_cjk = has_cjk(nickname);
            if existing_cjk && !new_cjk && nickname.len() < existing.len() {
                tracing::debug!("Nickname: keeping CJK '{}' for {}, rejecting ASCII '{}'", existing, uid, nickname);
                return;
            }
            // Don't replace a longer name with a short ASCII-only name (2-byte rule generalized)
            if !new_cjk && nickname.as_bytes().len() <= 5 && existing.as_bytes().len() > nickname.as_bytes().len() {
                tracing::debug!("Nickname: keeping '{}' for {}, rejecting shorter '{}'", existing, uid, nickname);
                return;
            }
        }
        tracing::trace!("Nickname: replacing '{}' with '{}' for {}{}",
            existing, nickname, uid, if force { " (forced)" } else { "" });
    } else {
        tracing::trace!("Nickname: setting '{}' for {}{}",
            nickname, uid, if force { " (forced)" } else { "" });
    }

    // Name eviction: character names are unique per server, so if this name
    // already belongs to a different entity ID, that old ID is stale. Evict the
    // old entity's name, player status, and summon mappings regardless of
    // whether the old entity was ever classified as a player.
    let evicted_ids: Vec<i32> = inner.nickname_storage.iter()
        .filter(|&(&old_id, old_name)| old_name == nickname && old_id != uid)
        .map(|(&old_id, _)| old_id)
        .collect();
    for old_id in evicted_ids {
        tracing::debug!("Name eviction: '{}' moved from entity {} to {}", nickname, old_id, uid);
        // When the game itself named both ids (a spawn or self record each
        // time), they are one character who came back as a new entity: after
        // dying, or as the local player does several times a fight. What the
        // old id did in this segment is theirs, so it moves to the new id. It
        // used to be deleted: a Cleric re-entering mid-pull lost ~55M of a
        // Gargaum fight (2026-07 capture). A name that only a fuzzy rule had
        // bound may have been on someone else, so that damage is still dropped.
        let same_character = force && inner.authoritative_name_ids.contains(&old_id);
        inner.nickname_storage.remove(&old_id);
        inner.known_player_ids.remove(&old_id);
        inner.authoritative_name_ids.remove(&old_id);
        inner.pending_nicknames.remove(&old_id);
        if same_character {
            for owner in inner.summon_storage.values_mut() {
                if *owner == old_id {
                    *owner = uid;
                }
            }
        } else {
            // Remove summon mappings pointing to the stale owner
            inner.summon_storage.retain(|_, &mut owner| owner != old_id);
        }
        for target_data in inner.target_combat.values_mut() {
            if let Some(actor_data) = target_data.actors.remove(&old_id) {
                if same_character {
                    target_data.actors.entry(uid).or_insert_with(ActorCombatData::new).absorb(actor_data);
                } else {
                    target_data.total_damage -= actor_data.total_damage;
                }
            }
        }
    }

    inner.nickname_storage.insert(uid, nickname.to_string());

    if !inner.confirmed_summon_ids.contains(&uid) {
        inner.summon_storage.remove(&uid);
    }

    if !inner.confirmed_summon_ids.contains(&uid) {
        let is_new = inner.known_player_ids.insert(uid);
        if is_new {
            purge_friendly_damage(inner, uid);
        }
    }

    if let Some(ref local_name) = inner.local_character_name {
        if local_name.trim() == nickname.trim() {
            inner.local_player_id = Some(uid as i64);
        }
    }
}

/// Name party members whose entity id no packet has tied to their name yet,
/// by class: the roster gives each member's class, and so does the damage of
/// each player on the meter.
///
/// A player's id and name arrive together in their spawn (`45 36`), which the
/// game sends when they come into view. Start the meter with the party
/// already together and those spawns have been and gone: a player's capture
/// started in a dungeon showed four of five members as bare ids through two
/// bosses, until a later area re-sent the spawns (2026-10-03). The roster
/// arrived within seconds.
///
/// Only a pairing nothing else could explain is used: exactly one roster
/// member of a class without an entity, and exactly one unnamed player of
/// that class fighting now. Two of a class on either side are left alone,
/// unless one of the players clearly runs a rotation and the others only
/// repeat a skill or two (an aura or a spirit the spawn never covered).
fn bind_roster_names_by_class(inner: &mut Inner) {
    if inner.party_members.len() < 2 {
        return;
    }
    let named: HashSet<&str> = inner.nickname_storage.values().map(|n| n.trim()).collect();
    let mut open: HashMap<JobClass, Vec<String>> = HashMap::new();
    for (name, member) in &inner.party_members {
        if let Some(job) = member.job
            && !named.contains(name.trim())
        {
            open.entry(job).or_default().push(name.clone());
        }
    }
    if open.is_empty() {
        return;
    }
    // Unnamed players in the current fight, with how many distinct skills each used.
    let mut skills: HashMap<i32, HashSet<i32>> = HashMap::new();
    for target in inner.target_combat.values() {
        for (&actor, data) in &target.actors {
            if inner.known_player_ids.contains(&actor)
                && !inner.nickname_storage.contains_key(&actor)
                && !inner.summon_storage.contains_key(&actor)
            {
                skills.entry(actor).or_default().extend(data.skills.keys().map(|&(code, _)| code));
            }
        }
    }
    // More unnamed players than unbound members means someone fighting is not
    // in the party (open world), and a stranger of the right class could be
    // the one matched. A rotation-less extra (a stray aura) counts here too;
    // that only delays naming until its owner's spawn does it.
    let open_names: usize = open.values().map(Vec::len).sum();
    if skills.len() > open_names {
        return;
    }
    let mut binds = Vec::new();
    for (job, names) in open {
        let [name] = names.as_slice() else { continue };
        let mut players: Vec<(i32, usize)> = skills
            .iter()
            .filter(|(id, _)| inner.actor_jobs.get(id) == Some(&job))
            .map(|(&id, s)| (id, s.len()))
            .collect();
        players.sort_by_key(|&(id, n)| (std::cmp::Reverse(n), id));
        let chosen = match players.as_slice() {
            [(id, _)] => *id,
            [(id, top), (_, next), ..] if *top >= 3 * *next => *id,
            _ => continue,
        };
        binds.push((chosen, name.clone()));
    }
    for (id, name) in binds {
        tracing::info!("Roster: {} is entity {}, the one unnamed player of their class", name, id);
        append_nickname_inner(inner, id, &name);
    }
}

fn apply_pending_nickname(inner: &mut Inner, uid: i32) {
    if inner.nickname_storage.contains_key(&uid) { return; }
    if let Some(pending) = inner.pending_nicknames.remove(&uid) {
        append_nickname_inner(inner, uid, &pending);
    }
}

fn is_friendly_action(inner: &Inner, actor_id: i32, target_id: i32) -> bool {
    let resolved_actor = summon_resolver::resolve(actor_id, &inner.summon_storage);
    let resolved_target = summon_resolver::resolve(target_id, &inner.summon_storage);
    inner.known_player_ids.contains(&resolved_actor) && inner.known_player_ids.contains(&resolved_target)
}

/// Remove friendly-fire damage from aggregates when a new player is identified.
fn purge_friendly_damage(inner: &mut Inner, _uid: i32) {
    let mut to_remove: Vec<(i32, Vec<i32>)> = Vec::new();

    for (&target_id, target_data) in &inner.target_combat {
        let mut actors_to_remove = Vec::new();
        for &actor_id in target_data.actors.keys() {
            if is_friendly_action(inner, actor_id, target_id) {
                actors_to_remove.push(actor_id);
            }
        }
        if !actors_to_remove.is_empty() {
            to_remove.push((target_id, actors_to_remove));
        }
    }

    for (target_id, actor_ids) in to_remove {
        if let Some(target_data) = inner.target_combat.get_mut(&target_id) {
            for actor_id in actor_ids {
                if let Some(actor_data) = target_data.actors.remove(&actor_id) {
                    target_data.total_damage -= actor_data.total_damage;
                }
            }
            if target_data.actors.is_empty() {
                inner.target_combat.remove(&target_id);
            }
        }
    }
}

pub fn is_player_skill(skill_code: i32) -> bool {
    // Class skills: 11M-19M (post-divide 110K-190K, encodes class in first 2 digits)
    // Alternate band: 3M-3.99M (post-divide 30K-39.9K)
    // Basic/special attacks: 100K-199K (post-divide 1K-1.9K)
    (11_000_000..=19_999_999).contains(&skill_code)
        || (3_000_000..=3_999_999).contains(&skill_code)
        || (100_000..=199_999).contains(&skill_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn who(s: &DataStorage) -> (Option<i64>, Option<String>, bool) {
        (s.local_player_id(), s.local_character_name(), s.local_identity_from_game())
    }

    #[test]
    fn fights_can_be_saved_before_a_zone_change_clears_them() {
        let s = Arc::new(DataStorage::new());
        let seen = Arc::new(std::sync::atomic::AtomicI64::new(-1));
        {
            let (s2, seen) = (s.clone(), seen.clone());
            // The hook can read the storage: no lock of ours is held.
            s.set_before_reset(move || {
                let damage: i64 = s2.get_combat_snapshot_light().values().map(|t| t.total_damage).sum();
                seen.store(damage, Ordering::Relaxed);
            });
        }
        let mut p = ParsedDamagePacket::new();
        p.set_actor_id(2259);
        p.set_target_id(50_000);
        p.set_skill_code(11010000);
        p.set_damage(700);
        s.append_damage(p);
        // A teleport well after the last hit.
        s.last_damage_ms.store(now_ms() - 10_000, Ordering::Relaxed);
        assert!(s.note_zone_change());
        assert_eq!(seen.load(Ordering::Relaxed), 700, "the hook saw the fight before it was cleared");
        assert!(s.get_combat_snapshot_light().is_empty());
    }

    #[test]
    fn loot_owner_is_you_until_the_self_record_says_otherwise() {
        let s = DataStorage::new();
        s.set_local_character_name(Some("Aveline".into()));
        s.note_party_scope(1454);
        assert!(s.note_loot_owner(900, 1454, "ApexZ"));
        assert_eq!(who(&s), (Some(1454), Some("ApexZ".into()), true));
        assert!(!s.note_loot_owner(900, 1454, "ApexZ"), "same owner again changes nothing");

        // A zone load brings the self record, which wins.
        assert!(s.set_local_identity_from_game(2001, Some("ApexZ".into())));
        assert!(!s.note_loot_owner(900, 1454, "ApexZ"));
        assert_eq!(who(&s), (Some(2001), Some("ApexZ".into()), true));
    }

    #[test]
    fn loot_naming_two_of_yours_equally_withdraws_the_guess_until_one_leads() {
        let s = DataStorage::new();
        s.note_party_scope(1454);
        s.note_party_scope(3583);
        s.note_loot_owner(900, 1454, "ApexZ");
        assert!(s.note_loot_owner(901, 3583, "Galaaadriel"), "the guess is withdrawn");
        assert_eq!(who(&s), (None, Some("ApexZ".into()), false));
        assert!(!s.note_loot_owner(900, 1454, "ApexZ"), "the same kill again is no new vote");
        assert_eq!(s.local_player_id(), None);
        assert!(s.note_loot_owner(902, 1454, "ApexZ"), "a second kill leads");
        assert_eq!(who(&s), (Some(1454), Some("ApexZ".into()), true));
    }

    /// Issue #12's two orders: you then a bystander, and a bystander then you.
    #[test]
    fn a_bystanders_loot_never_takes_over_whichever_comes_first() {
        let s = DataStorage::new();
        s.set_local_character_name(Some("PlayerA".into()));
        s.note_party_scope(13978);
        assert!(s.note_loot_owner(1, 13978, "PlayerA"));
        assert!(!s.note_loot_owner(2, 15855, "PlayerB"));
        assert_eq!(s.local_player_id(), Some(13978));

        let s = DataStorage::new();
        s.set_local_character_name(Some("PlayerA".into()));
        s.note_party_scope(13978);
        assert!(!s.note_loot_owner(1, 15855, "PlayerB"));
        assert!(s.note_loot_owner(2, 13978, "PlayerA"));
        assert_eq!(s.local_player_id(), Some(13978));
        assert!(!s.local_identity_from_self_record(), "a loot guess is not the game's own word");
    }

    #[test]
    fn a_strangers_kill_says_nothing_about_who_you_are() {
        // Loot from kills by players nearby reaches you too; the server's
        // `06 38` records, which name only you and your party, tell them apart.
        let s = DataStorage::new();
        s.note_party_scope(14957);
        assert!(!s.note_loot_owner(22965, 11937, "Deityclaire"));
        assert!(s.note_loot_owner(46643, 14957, "Naicha"));
        for mob in [40758, 63645, 74428] {
            assert!(!s.note_loot_owner(mob, 892, "Dandelion"));
        }
        assert_eq!(who(&s), (Some(14957), Some("Naicha".into()), true));
    }

    fn member(slot: u8) -> PartyMember {
        PartyMember { slot, level: 45, gear_score: 3000, combat_power: 39_000, ..Default::default() }
    }

    #[test]
    fn party_members_who_have_not_fought_are_shown_for_a_while() {
        let s = DataStorage::new();
        assert!(!s.party_placeholders_wanted(), "no roster yet");
        s.set_party_roster(vec![("Prenses".into(), member(2)), ("adam".into(), member(3))], true);
        assert!(s.party_placeholders_wanted());

        // A reset clears them until the next roster.
        s.hide_party_placeholders();
        assert!(!s.party_placeholders_wanted());
        s.set_party_roster(vec![("Prenses".into(), member(2)), ("adam".into(), member(3))], true);
        assert!(s.party_placeholders_wanted());

        // And they expire: a dungeon party 15 minutes on is not "your party".
        s.inner.write().party_roster_at_ms -= 15 * 60 * 1000;
        assert!(!s.party_placeholders_wanted());
        assert_eq!(s.get_party_members().len(), 2, "the roster itself is kept, for combat power");
    }

    fn hit(actor: i32, target: i32, at: i64, damage: i32, dot: bool) -> ParsedDamagePacket {
        let mut p = ParsedDamagePacket::new();
        p.set_actor_id(actor);
        p.set_target_id(target);
        p.set_skill_code(11010000);
        p.set_damage(damage);
        p.set_dot(dot);
        p.set_timestamp(at);
        p
    }

    fn of_class(slot: u8, job: JobClass) -> PartyMember {
        PartyMember { job: Some(job), ..member(slot) }
    }

    /// 64 hits from each of `actors`, taking turns, each using `skills`
    /// distinct skills of the class with skill prefix `prefix`.
    fn fight_together(s: &DataStorage, actors: &[i32], prefix: i32, skills: i32) {
        for i in 0..64 {
            for &actor in actors {
                let mut p = hit(actor, 900, i, 100, false);
                p.set_skill_code(prefix * 1_000_000 + 10_000 + (i as i32 % skills) * 10);
                s.append_damage(p);
            }
        }
    }

    fn fight(s: &DataStorage, actor: i32, prefix: i32, skills: i32) {
        fight_together(s, &[actor], prefix, skills);
    }

    #[test]
    fn a_record_without_a_readable_level_keeps_the_known_one() {
        let s = DataStorage::new();
        s.set_local_identity_from_game(14957, Some("Naicha".into()));
        s.note_self_profile("Naicha", Some(JobClass::Cleric), Some(29));
        s.note_self_profile("Naicha", Some(JobClass::Cleric), None);
        assert_eq!(s.local_profile().level, Some(29));
        s.note_self_profile("Naicha", Some(JobClass::Cleric), Some(30));
        assert_eq!(s.local_profile().level, Some(30), "a level-up replaces it");
        s.note_self_profile("Other", None, None);
        s.set_local_identity_from_game(1, Some("Other".into()));
        assert_eq!(s.local_profile().level, None, "another character starts unknown");
    }

    #[test]
    fn a_character_back_as_a_new_entity_keeps_their_damage() {
        let s = DataStorage::new();
        s.append_nickname_authoritative(101, "Cleric");
        s.append_damage(hit(101, 900, 1_000, 500, false));
        s.append_nickname_authoritative(202, "Cleric");
        s.append_damage(hit(202, 900, 2_000, 300, false));
        let snap = s.get_combat_snapshot();
        let boss = &snap[&900];
        assert!(!boss.actors.contains_key(&101));
        assert_eq!(boss.actors[&202].total_damage, 800, "the earlier hits moved with the name");
        assert_eq!(boss.total_damage, 800);

        // A name a fuzzy rule had put on some id says nothing about whose
        // damage that id dealt, so it is not moved.
        let s = DataStorage::new();
        s.append_damage(hit(303, 900, 1_000, 500, false));
        s.append_nickname(303, "Cleric");
        s.append_nickname_authoritative(404, "Cleric");
        assert!(!s.get_combat_snapshot()[&900].actors.contains_key(&404));
    }

    #[test]
    fn a_zone_change_resets_on_a_replays_clock_before_zero() {
        // A slice replays at offsets from the pull, so its lead-in runs at
        // negative times; "never" must still read as long ago there.
        let s = DataStorage::new();
        crate::clock::set_override(Some(-40_000));
        s.append_damage(hit(5, 900, -40_000, 100, false));
        crate::clock::set_override(Some(-30_000));
        assert!(s.note_zone_change(), "a wipe's teleport in the lead-in clears the pull before");
        crate::clock::set_override(None);
    }

    #[test]
    fn fights_are_on_your_server_else_your_partys() {
        let s = DataStorage::new();
        assert_eq!(s.fight_server_id(), 0, "nothing has said");
        let on = |server: u16| PartyMember { server_id: server, ..member(1) };
        s.set_party_roster(vec![("A".into(), on(2304)), ("B".into(), on(2304)), ("C".into(), on(1307))], true);
        assert_eq!(s.fight_server_id(), 2304, "the party's, by majority");
        s.set_local_identity_from_game(7, Some("C".into()));
        assert_eq!(s.fight_server_id(), 1307, "your own place in the roster");
        s.note_player_server("C", 1304);
        assert_eq!(s.fight_server_id(), 1304, "what your own record says");
        s.note_player_server("C", 99);
        assert_eq!(s.fight_server_id(), 1304, "a value no server has is not taken");
    }

    #[test]
    fn a_party_member_is_named_by_class_when_only_one_fits() {
        let s = DataStorage::new();
        s.set_party_roster(
            vec![("Glad".into(), of_class(1, JobClass::Gladiator)), ("Temp".into(), of_class(2, JobClass::Templar))],
            true,
        );
        fight(&s, 101, 11, 6);
        fight(&s, 102, 12, 6);
        assert_eq!(s.get_nickname(101).as_deref(), Some("Glad"));
        assert_eq!(s.get_nickname(102).as_deref(), Some("Temp"));
    }

    #[test]
    fn two_of_a_class_on_either_side_stay_unnamed() {
        // Two Gladiators in the roster, one fighting: either could be them.
        let s = DataStorage::new();
        s.set_party_roster(
            vec![("GladA".into(), of_class(1, JobClass::Gladiator)), ("GladB".into(), of_class(2, JobClass::Gladiator))],
            true,
        );
        fight(&s, 101, 11, 6);
        assert_eq!(s.get_nickname(101), None);

        // One Gladiator in the roster, two with equal rotations fighting.
        let s = DataStorage::new();
        s.set_party_roster(
            vec![("Glad".into(), of_class(1, JobClass::Gladiator)), ("Temp".into(), of_class(2, JobClass::Templar))],
            true,
        );
        fight_together(&s, &[101, 103], 11, 6);
        assert_eq!(s.get_nickname(101), None);
        assert_eq!(s.get_nickname(103), None);

        // Once the other is named by their own spawn, the one left is the match.
        s.append_nickname_authoritative(103, "Stranger");
        fight(&s, 101, 11, 6);
        assert_eq!(s.get_nickname(101).as_deref(), Some("Glad"));
    }

    #[test]
    fn strangers_fighting_alongside_stop_the_match() {
        // Open world: the party's Gladiator plus two players from outside it.
        // Only one is a Gladiator, but with more unnamed players than open
        // roster names the meter cannot know a stranger is not the one.
        let s = DataStorage::new();
        s.set_party_roster(
            vec![("Glad".into(), of_class(1, JobClass::Gladiator)), ("Me".into(), of_class(2, JobClass::Templar))],
            true,
        );
        s.append_nickname_authoritative(100, "Me");
        fight(&s, 100, 12, 6);
        for (actor, prefix) in [(102, 14), (104, 17), (101, 11)] {
            fight(&s, actor, prefix, 6);
        }
        assert_eq!(s.get_nickname(101), None);
    }

    #[test]
    fn a_member_whose_name_is_on_an_entity_is_not_bound_again() {
        let s = DataStorage::new();
        s.append_nickname_authoritative(101, "Glad");
        s.set_party_roster(
            vec![("Glad".into(), of_class(1, JobClass::Gladiator)), ("Temp".into(), of_class(2, JobClass::Templar))],
            true,
        );
        fight(&s, 101, 11, 6);
        fight(&s, 105, 11, 6);
        assert_eq!(s.get_nickname(101).as_deref(), Some("Glad"));
        assert_eq!(s.get_nickname(105), None);
    }

    fn totals(s: &DataStorage, target: i32) -> (i64, i64) {
        let snap = s.get_combat_snapshot();
        let t = &snap[&target];
        (t.total_damage, t.last_damage_time - t.first_damage_time)
    }

    #[test]
    fn on_a_training_dummy_dot_after_the_last_direct_hit_does_not_count() {
        let s = DataStorage::new();
        s.register_training_dummy(500);
        s.append_damage(hit(1454, 500, 1_000, 100, false));
        s.append_damage(hit(1454, 500, 2_000, 50, true));
        assert_eq!(totals(&s, 500), (100, 0), "the tick waits for the next direct hit");

        s.append_damage(hit(1454, 500, 3_000, 100, false));
        assert_eq!(totals(&s, 500), (250, 2_000), "a direct hit brings the tick in");

        // The player stops; their DoT ticks on.
        s.append_damage(hit(1454, 500, 4_000, 50, true));
        s.append_damage(hit(1454, 500, 5_000, 50, true));
        assert_eq!(totals(&s, 500), (250, 2_000), "time ends at the last direct hit");
    }

    #[test]
    fn on_anything_else_every_dot_tick_counts() {
        let s = DataStorage::new();
        s.append_damage(hit(1454, 600, 1_000, 100, false));
        s.append_damage(hit(1454, 600, 2_000, 50, true));
        assert_eq!(totals(&s, 600), (150, 1_000));
    }

    fn with_skill(mut p: ParsedDamagePacket, skill: i32) -> ParsedDamagePacket {
        p.set_skill_code(skill);
        p
    }

    fn dealt(s: &DataStorage, target: i32, actor: i32) -> i64 {
        s.get_combat_snapshot().get(&target).and_then(|t| t.actors.get(&actor)).map_or(0, |a| a.total_damage)
    }

    #[test]
    fn link_records_link_a_spirit_and_are_not_damage() {
        let s = DataStorage::new();
        s.append_nickname_authoritative(100, "Owner");
        // A spirit with no spawn link hits first; its damage waits under its id.
        s.append_damage(with_skill(hit(500, 900, 1_000, 300, false), 16_010_000));
        assert!(!s.is_summon(500));
        // Spirit to owner.
        s.append_damage(with_skill(hit(500, 100, 1_500, 20, false), 16_990_002));
        assert_eq!(s.get_summon_data().get(&500), Some(&100));
        assert!(s.is_confirmed_summon(500));
        // Owner to spirit.
        s.append_damage(with_skill(hit(100, 501, 1_600, 197, false), 16_770_000));
        assert_eq!(s.get_summon_data().get(&501), Some(&100));

        let snap = s.get_combat_snapshot();
        assert!(!snap.contains_key(&100), "the owner is no target");
        assert!(!snap.contains_key(&501), "nor is the spirit");
        assert_eq!(snap[&900].total_damage, 300);
        assert!(s.get_heal_snapshot().is_empty(), "nor is it healing");
        assert!(!s.is_known_player(500));
    }

    #[test]
    fn a_new_spawn_under_an_id_starts_without_the_old_owner() {
        let s = DataStorage::new();
        s.append_damage(with_skill(hit(500, 100, 1_000, 20, false), 16_990_002));
        s.append_damage(with_skill(hit(500, 900, 1_100, 300, false), 16_010_000));
        // The id comes back as someone else's spirit.
        s.note_summon_spawn(500);
        assert!(!s.is_summon(500));
        assert!(!s.is_confirmed_summon(500));
        assert_eq!(dealt(&s, 900, 100), 300, "the old spirit's damage stays its owner's");
        assert_eq!(dealt(&s, 900, 500), 0);
        s.append_damage(with_skill(hit(500, 200, 2_000, 20, false), 16_990_002));
        s.append_damage(with_skill(hit(500, 900, 2_100, 50, false), 16_010_000));
        assert_eq!(s.get_summon_data().get(&500), Some(&200));
        assert_eq!(dealt(&s, 900, 500), 50);
        assert_eq!(dealt(&s, 900, 100), 300);

        // A player spawn under a summon's id ends the summon too.
        s.note_player_spawn(500);
        assert!(!s.is_summon(500));
        assert_eq!(dealt(&s, 900, 200), 50);
    }

    #[test]
    fn a_link_to_another_owner_keeps_what_the_old_entity_did() {
        // The spawn between the two went unseen.
        let s = DataStorage::new();
        s.append_damage(with_skill(hit(500, 100, 1_000, 20, false), 16_990_002));
        s.append_damage(with_skill(hit(500, 900, 1_100, 300, false), 16_010_000));
        s.append_damage(with_skill(hit(200, 500, 5_000, 197, false), 16_770_000));
        assert_eq!(s.get_summon_data().get(&500), Some(&200));
        assert_eq!(dealt(&s, 900, 100), 300);
        assert_eq!(dealt(&s, 900, 500), 0);
    }

    #[test]
    fn a_summon_using_a_class_skill_is_no_player() {
        let s = DataStorage::new();
        s.register_confirmed_summon_by_id(500, 100);
        s.append_damage(with_skill(hit(500, 900, 1_000, 300, false), 16_010_000));
        assert!(!s.is_known_player(500));
        assert_eq!(s.get_summon_data().get(&500), Some(&100));

        // Spawned as a summon, owner not known yet.
        s.note_summon_spawn(501);
        s.append_damage(with_skill(hit(501, 900, 1_000, 300, false), 16_010_000));
        assert!(!s.is_known_player(501));

        // A link through the summon back to itself is refused.
        s.append_damage(with_skill(hit(100, 500, 1_000, 20, false), 16_990_002));
        assert_eq!(s.get_summon_data().get(&100), None);
    }

    #[test]
    fn summon_links_survive_a_reset() {
        let s = DataStorage::new();
        s.append_damage(with_skill(hit(500, 100, 1_000, 20, false), 16_990_002));
        s.register_confirmed_summon_by_id(501, 100);
        s.note_summon_spawn(502);
        s.flush();
        assert_eq!(s.get_summon_data().get(&500), Some(&100));
        assert_eq!(s.get_summon_data().get(&501), Some(&100));
        assert!(s.get_summon_spawn_ids().contains(&502));
        s.append_damage(with_skill(hit(502, 900, 2_000, 300, false), 16_010_000));
        assert!(!s.is_known_player(502), "still a summon after the reset");

        s.forget_summon_links();
        assert!(s.get_summon_data().is_empty());
    }

    #[test]
    fn a_configured_name_found_in_the_world_is_kept() {
        let s = DataStorage::new();
        s.set_local_character_name(Some("Misti".into()));
        s.append_nickname_authoritative(4099, "Misti");
        assert_eq!(s.local_player_id(), Some(4099));
        assert!(!s.note_loot_owner(900, 1454, "ApexZ"));
        assert_eq!(who(&s), (Some(4099), Some("Misti".into()), false));
    }
}

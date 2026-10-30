use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};

use crate::capture::abnormal;
use crate::combat::death_recap::{self, DeathRecaps};
use crate::combat::stagger::{End, Outcome, StaggerHit, StaggerLog, StaggerWindow};

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
/// A "no longer in a party" record this soon after the party ended is about
/// that same party. See `DataStorage::note_party_left`.
const PARTY_LEFT_AFTER_END_MS: i64 = 10 * 1000;
/// Whether a player other than you has damage on the meter (a summon counts
/// as its owner). With no local player known, any player counts.
fn others_on_meter(inner: &Inner) -> bool {
    let me = inner.local_player_id.and_then(|id| i32::try_from(id).ok());
    inner.target_combat.values().flat_map(|t| t.actors.keys()).any(|&actor| {
        let owner = inner.summon_storage.get(&actor).copied().unwrap_or(actor);
        Some(owner) != me && (inner.known_player_ids.contains(&owner) || inner.nickname_storage.contains_key(&owner))
    })
}

/// "Has not happened" for the times below. Not 0: a replayed slice's clock
/// starts before 0 (the lead-in runs at negative offsets from the pull), so
/// a 0 there read as "a moment ago" and suppressed every zone reset in the
/// lead-in, which ran a wiped pull's damage into the next pull (Gargaum,
/// 2026-07 capture: 114M derived for a 69M pull).
const NEVER_MS: i64 = i64::MIN;
/// Deaths kept for Details' DPS chart (`Inner::death_log`): far more than
/// a session's fights need, small enough never to matter.
const MAX_DEATHS: usize = 2_000;
/// How often, in damage records, unnamed party members are matched to the
/// roster by class. A fight brings a few hundred records a second, so this
/// names them within the first moments of combat.
const ROSTER_BIND_EVERY: u32 = 64;
/// How long the buff timeline keeps an abnormal after it ended: long enough
/// to look back over any fight, short enough that a meter left running all
/// day holds a bounded amount (a busy boss fight brings a few thousand
/// instances a minute).
const ABNORMAL_RETENTION_MS: i64 = 30 * 60 * 1000;
/// How often, in capture time, the timeline drops what is past retention.
const ABNORMAL_PRUNE_EVERY_MS: i64 = 60 * 1000;

/// Daily Duty's count among the content counts (`content_counts`): an id
/// the game's entries never have (the entry parser turns away id 0).
pub const DAILY_DUTY_ID: u32 = 0;

/// A Daily Duty offer of the board (see
/// `StreamProcessor::parse_daily_duty_packet`). State 01 is open, 02 taken,
/// 04 completed (handed in).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DutyOffer {
    pub offer: u32,
    pub quest: u32,
    /// Your quest number, once taken (what giving it up names).
    pub number: u32,
    pub state: u8,
}

/// The Daily Duty board: its offers, the next reset (unix ms, moved on when
/// played through it), and the reset the game sent it with.
#[derive(Debug, Clone)]
struct DutyBoard {
    offers: Vec<DutyOffer>,
    reset_ms: i64,
    sent_reset_ms: i64,
}

fn now_ms() -> i64 {
    crate::clock::now_ms()
}

/// Open-world map ids from the game's Map table: the overworld maps and their
/// world layers (the overworld itself, split off for quest scenes).
static OPEN_WORLD_MAPS: std::sync::LazyLock<HashSet<i32>> = std::sync::LazyLock::new(|| {
    #[derive(serde::Deserialize)]
    struct Table {
        maps: HashSet<i32>,
    }
    serde_json::from_str::<Table>(include_str!("../../../src/data/open_world_maps.json"))
        .map(|t| t.maps)
        .unwrap_or_default()
});

/// True for a map of the open world. Unknown ids (a map added by a later
/// patch) count as instances, which keeps the dungeon id as before.
pub fn is_open_world_map(map_id: i32) -> bool {
    OPEN_WORLD_MAPS.contains(&map_id)
}

/// The dungeon the player is in, from the roster's dungeon and the last map
/// load.
///
/// The roster (`02 97`) names the dungeon the PARTY is for, not where anyone
/// is: in captures it names the dungeon minutes before the load into it, while
/// the party is still in the open world (600163 on 2026-08-15, 610073 on
/// 2026-07-04), and again on every roster update after the party has left. A
/// roster update after leaving set the old id back, and fights in the open
/// world or other instances were filed under the last dungeon (76 public logs
/// of Vakron Sky Island, 600072, on open-world bosses).
///
/// A map load is where the player is. An instance's map id is its dungeon id
/// (600011, 600021, 600163 and 610073 in captures, on entry and on every
/// teleport inside), so:
/// - no map load seen yet (a meter opened mid-session, a slice whose prelude
///   holds none): the roster's dungeon, the only evidence there is;
/// - the open world: none;
/// - a dungeon's map (600000-699999, or the roster's own id): that dungeon;
/// - any other instance (a seal, a quest scene): none, whatever the roster
///   says.
fn dungeon_of_map(roster_dungeon: i32, map: Option<i32>) -> i32 {
    match map {
        None => roster_dungeon,
        Some(m) if is_open_world_map(m) => 0,
        Some(m) if (600_000..700_000).contains(&m) || (m > 0 && m == roster_dungeon) => m,
        Some(_) => 0,
    }
}

// ───── Aggregate data structures ─────

/// Healing done, aggregated per (healer actor, skill, is_hot). Healing is keyed by
/// the HEALER (not the boss target), since the meter shows "healing done" per player.
#[derive(Debug, Clone, Default)]
pub struct HealSkillData {
    pub total_heal: i64,
    pub tick_count: i32,
    /// Each tick: when it landed (ms, the capture clock) and how much it
    /// healed. Healing is kept per healer for the whole session; a fight's
    /// details count only the ticks inside its window (its totals, its HPS
    /// chart and its heal cast lanes).
    pub ticks: Vec<(i64, i64)>,
}

/// A hit the game reports with no damage, by its hit type (`EHitType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoDamageHit {
    /// Hit type 1.
    Miss,
    /// Hit type 6. In the 2026-10-04 captures nearly every one comes with a
    /// damage record of the same skill on the same target: what was resisted
    /// is the skill's effect, not its damage.
    Resist,
}

impl NoDamageHit {
    pub fn from_hit_type(hit_type: i32) -> Option<Self> {
        match hit_type {
            1 => Some(Self::Miss),
            6 => Some(Self::Resist),
            _ => None,
        }
    }
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
    pub shield_block_count: i32,
    pub parry_count: i32,
    pub perfect_count: i32,
    pub double_count: i32,
    pub iron_wall_count: i32,
    pub regeneration_count: i32,
    pub perfect_block_count: i32,
    /// Hits with no damage: hit type 1 (Miss) and 6 (Resist). Not in
    /// `hit_count`.
    pub miss_count: i32,
    pub resist_count: i32,
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
            shield_block_count: self.shield_block_count,
            parry_count: self.parry_count,
            perfect_count: self.perfect_count,
            double_count: self.double_count,
            iron_wall_count: self.iron_wall_count,
            regeneration_count: self.regeneration_count,
            perfect_block_count: self.perfect_block_count,
            miss_count: self.miss_count,
            resist_count: self.resist_count,
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
        self.shield_block_count += other.shield_block_count;
        self.parry_count += other.parry_count;
        self.perfect_count += other.perfect_count;
        self.double_count += other.double_count;
        self.iron_wall_count += other.iron_wall_count;
        self.regeneration_count += other.regeneration_count;
        self.perfect_block_count += other.perfect_block_count;
        self.miss_count += other.miss_count;
        self.resist_count += other.resist_count;
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
            shield_block_count: 0,
            parry_count: 0,
            perfect_count: 0,
            double_count: 0,
            iron_wall_count: 0,
            regeneration_count: 0,
            perfect_block_count: 0,
            miss_count: 0,
            resist_count: 0,
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
    /// Equipment item level: your own entry in the party roster, else the last
    /// self record's.
    pub gear_score: Option<i32>,
    /// Combat power, from the same two places.
    pub combat_power: Option<i64>,
}

/// A value the self record gave, if it was for this character.
fn stated_for<T: Copy>(stated: &Option<(String, T)>, me: Option<&str>) -> Option<T> {
    stated.as_ref().filter(|(name, _)| Some(name.trim()) == me).map(|(_, v)| *v)
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
    /// How the hits taken were met (shield blocks, parries...).
    pub received: crate::entity::details_context::ReceivedHits,
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
        self.received.add(other.received);
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
            received: Default::default(),
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
}

impl TargetCombatData {
    fn clone_light(&self) -> Self {
        let actors = self.actors.iter().map(|(&id, actor)| {
            (id, ActorCombatData {
                total_damage: actor.total_damage,
                party_heal: actor.party_heal,
                regen: actor.regen,
                damage_received: actor.damage_received,
                hits_received: actor.hits_received,
                received: actor.received,
                last_damage_time: actor.last_damage_time,
                job: actor.job,
                skills: actor.skills.iter().map(|(&key, skill)| (key, skill.clone_light())).collect(),
            })
        }).collect();
        Self {
            target_id: self.target_id,
            total_damage: self.total_damage,
            first_damage_time: self.first_damage_time,
            last_damage_time: self.last_damage_time,
            last_packet_id: self.last_packet_id,
            actors,
        }
    }

    fn new(target_id: i32, timestamp: i64) -> Self {
        Self {
            target_id,
            total_damage: 0,
            first_damage_time: timestamp,
            last_damage_time: timestamp,
            last_packet_id: -1,
            actors: HashMap::new(),
        }
    }
}

// ───── Main storage ─────

/// A field boss kill or HP milestone in the open world, for
/// `boss_report.rs` to share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BossEvent {
    /// The boss's NPC code (`capture::field_bosses`).
    pub npc: i32,
    pub kind: BossEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BossEventKind {
    /// Killed at this moment (unix ms).
    Killed { at_ms: i64 },
    /// Seen up: its spawn came, as it spawned or came into view (unix ms).
    /// Only for the Abyss's cores, whose maps say nothing of them.
    Seen { at_ms: i64 },
}

/// Unsent boss events kept at most (the sender drains them every second).
#[cfg(feature = "online")]
const MAX_BOSS_EVENTS: usize = 64;

/// A field boss counts as being fought while its HP changed, or it took
/// damage, this recently.
const IN_COMBAT_MS: i64 = 10_000;
/// The passive HP regen everyone gets ("Restore HP"), not a heal anyone cast.
const PASSIVE_REGEN_SKILL: i32 = 1_900_001;
/// While it is fought and its HP holds, the same HP goes again this often:
/// the site takes a repeat as "still being fought" once a minute and ignores
/// the rest. A change goes on the next pass (every 3 s), as always.
const BOSS_HP_KEEP_ALIVE_MS: i64 = 60_000;
/// A field boss's HP back up by this many points or more has reset (kited
/// out of range, or left alone until it recovered).
const RESET_RISE: u8 = 5;

/// A field boss's HP for the sender (`take_boss_hp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BossHp {
    /// The boss's NPC code.
    pub npc: i32,
    /// Whole percent left.
    pub percent: u8,
    /// Back up from what was last sent: it reset.
    pub reset: bool,
}


#[cfg(feature = "online")]
fn push_boss_event(inner: &mut Inner, event: BossEvent) {
    inner.boss_events.push_back(event);
    while inner.boss_events.len() > MAX_BOSS_EVENTS {
        inner.boss_events.pop_front();
    }
}

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
    /// Buffs, debuffs and stats as the server reports them (see
    /// `capture::abnormal`). Recorded only, nothing reads it into a fight
    /// yet. Its own lock: it is fed every packet, and nothing here needs it
    /// together with `inner`.
    abnormals: Mutex<AbnormalLog>,
    /// Stagger gauges and their windows (see `combat::stagger`). Its own
    /// lock, as `abnormals`: fed by its own records, read when a fight is
    /// saved.
    stagger: Mutex<StaggerLog>,
    /// Who killed you (see `combat::death_recap`). Its own lock, as
    /// `stagger`.
    death_recaps: Mutex<DeathRecaps>,
}

/// The timeline and when it was last pruned.
struct AbnormalLog {
    timeline: abnormal::Timeline,
    pruned_ms: i64,
}

struct Inner {
    /// Aggregated combat data per target (replaces raw packet storage)
    target_combat: HashMap<i32, TargetCombatData>,
    /// Job class detected per actor (across all targets, for summon matching)
    actor_jobs: HashMap<i32, JobClass>,

    nickname_storage: HashMap<i32, String>,
    /// Bumped whenever `nickname_storage` changes, so the meter redraws a
    /// name that arrives after the last hit (see `names_generation`).
    names_generation: u64,
    /// Changes when the party's members change, or it ends (see
    /// `party_generation`).
    party_generation: u64,
    /// Counted at each map load (`zone_generation`), and at each one that
    /// enters or leaves an instance (`instance_generation`).
    zone_generation: u64,
    instance_generation: u64,
    /// When the party last ended (`end_party`), clock ms; `NEVER_MS` if it
    /// has not.
    party_ended_at_ms: i64,
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
    /// Members who have left the roster, as it last had them. A fight is saved
    /// again every 30 s until the next one, and members who leave the party
    /// before its last save would otherwise go out with no level, gear score
    /// or combat power (Vakron, 2026-10-17: three of five, who left at the
    /// end of the run). See `fight_roster`.
    former_party_members: HashMap<String, PartyMember>,
    /// When the last roster arrived, and whether its members who have not
    /// fought should still get rows. See `party_placeholders_wanted`.
    party_roster_at_ms: i64,
    party_placeholders_hidden: bool,
    /// The party key of the last roster that had you in it; `None` outside a
    /// party. See `is_other_party`.
    own_party_key: Option<u32>,
    /// Whether a roster since the party began has had you in it. Only then
    /// does a whole roster without you say you left (see `set_party_roster_of`).
    self_in_roster: bool,
    /// Damage records since `bind_roster_names_by_class` last ran. Counted in
    /// records rather than time so a replay names players where a live meter
    /// did.
    damage_since_roster_bind: u32,
    /// Instance id the party is in. Encodes the dungeon and its difficulty
    /// tier; resolved to a name by the frontend's dungeon table. Derived from
    /// `roster_dungeon_id` and `current_map_id` by `dungeon_of_map`.
    current_dungeon_id: i32,
    /// The dungeon the party roster (`02 97`) names. It is the party's, not
    /// the player's: the roster names it while the party queues in the open
    /// world, and keeps naming it after the party has left.
    roster_dungeon_id: i32,
    /// The map the last zone load (`21 36`) named; None until one is seen.
    current_map_id: Option<i32>,
    /// Power-scalar values observed per actor in its damage records. A summon
    /// inherits its owner's, so this links the two when no spawn packet (and
    /// therefore no `parent_key`) ever arrives — the case for a Cleric's Divine
    /// Aura, which the server creates without announcing. A set rather than one
    /// value because the scalar shifts as buffs come and go, and owner and summon
    /// are not always in the same buff state at the same instant.
    actor_power_scalars: HashMap<i32, HashSet<i32>>,
    hostile_target_ids: HashSet<i32>,
    dead_entity_ids: HashSet<i32>,
    /// Every death with its time and, once back up, the revive's: monsters'
    /// (`42 36` flag 3) and party members' (`note_hp`). Details marks the
    /// players' on its DPS chart. The last `MAX_DEATHS` only.
    death_log: VecDeque<(i32, i64, Option<i64>)>,
    /// The maps the player last opened, one per map (`capture::boss_map`),
    /// and a count that moves with each, for `boss_maps_since`.
    boss_maps: Vec<crate::capture::boss_map::BossMap>,
    boss_map_seq: u64,
    /// Who holds each Abyss artifact, as last told (`capture::abyss_artifacts`):
    /// each war and artifact replaced by its newer word. When it was told,
    /// and a count that moves with each, for `abyss_artifacts_since`.
    abyss_artifacts: crate::capture::abyss_artifacts::ArtifactState,
    abyss_artifacts_seen_ms: i64,
    abyss_artifacts_seq: u64,
    /// The Supply Requests boards, as last told at login
    /// (`capture::supply_requests`), and a count that moves with each.
    supply_requests: Vec<crate::capture::supply_requests::Board>,
    supply_requests_seq: u64,
    /// Entities whose max HP came from their spawn record, so a percentage of
    /// it can be trusted (a peak seen later may be below the real max).
    spawn_hp_ids: HashSet<i32>,
    /// A field boss's HP now (NPC code, whole percent, when it last changed),
    /// per entity, and the percent last handed to the sender (`take_boss_hp`).
    boss_hp_now: HashMap<i32, (i32, u8, i64)>,
    /// Each fought boss's HP as last sent, and when (`take_boss_hp`).
    boss_hp_sent: HashMap<i32, (u8, i64)>,
    /// Field boss kills and HP milestones in the open world, not yet sent
    /// (`boss_report.rs`).
    boss_events: VecDeque<BossEvent>,
    /// Party members who also run A2Tools, by name (`party_presence.rs`).
    a2tools_users: HashSet<String>,
    /// Each party member's last HP (`1b 92`): falling to 0 from above is a
    /// death. The game sends no `42 36` death for a player (Ultimate Berk,
    /// 2026-10-11: 184 monster deaths, not one of the four that died).
    last_hp: HashMap<i32, i32>,
    /// Linked summons that left the world (`42 36` flag 7). Their damage over
    /// time ticks on, but the game's Damage Analyzer counts no tick after the
    /// spirit is gone, so the meter does not either. Kept through a reset, like
    /// the links; cleared when the id comes back.
    despawned_summon_ids: HashSet<i32>,
    /// Boss entity IDs identified from NPC DB boss flags
    boss_entity_ids: HashSet<i32>,
    /// Training dummies (scarecrows, punching bags) among the entities spawned,
    /// from the NPC table. Damage on them follows `held_dot_ticks`.
    training_dummy_ids: HashSet<i32>,
    /// Entities whose live HP was seen at exactly 1, the floor a training
    /// dummy stops at instead of dying.
    hp_floored_ids: HashSet<i32>,
    /// Entities that came back up from that floor without dying: training
    /// dummies, for those whose spawn (and with it the NPC code) the meter
    /// never saw. Town dummies are spawned once, so a meter started, or
    /// restarted, next to them never learned what they were, and Train mode
    /// showed nothing however long the player hit them (2026-10-07).
    hp_reset_dummy_ids: HashSet<i32>,
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

    /// The address of the game server the locked connection reads from.
    /// Korea and Taiwan share their server ids (10xx/20xx), so field boss
    /// reports from them carry it for a2tools.app to tell the two apart
    /// (`boss_report::with_host`).
    game_host: Option<std::net::Ipv4Addr>,
    /// See `note_host_server`.
    host_server: u16,

    /// Odyle energy, (basic, charged), as the game last sent it. See
    /// `StreamProcessor::parse_odyle_energy_packet`.
    odyle_energy: Option<(u32, u32)>,
    /// The character that energy is, for the window to remember it by
    /// across a meter restart, and the content counts and Daily Duty board
    /// with it. A login's list comes some seconds before the self record
    /// naming who logged in, so until that record it has no owner
    /// (`odyle_awaits_self_record`): the name still known then can be the
    /// character before a switch. An update or a board in a session the game
    /// has named (a meter started mid-session) is theirs.
    odyle_owner: Option<String>,
    odyle_awaits_self_record: bool,
    /// The game's entry counts by entry id (`0b 61` / `0c 61`), whose owner
    /// is Odyle energy's (they come in the same list).
    content_counts: std::collections::BTreeMap<u32, [u32; 2]>,
    /// The shop's purchase limits by item id: (bought, next reset unix ms).
    /// See `parse_shop_and_craft_packet`.
    shop_limits: std::collections::BTreeMap<u32, (u32, i64)>,
    /// Crafts made by recipe id, as the crafting window or a craft last said.
    craft_counts: std::collections::BTreeMap<u32, u32>,
    /// Kinah gained today with the game's time of it (unix ms), and Abyss
    /// Points this week with the week's limit and when seen (`a5 ff` /
    /// `a6 ff`, `a9 ff`). A login's (`a9 ff` just before its list, `a5 ff`
    /// just after) wait in `login_kinah` / `login_abyss_points` for the self
    /// record, so they never show under the character before.
    kinah_today: Option<(u64, i64)>,
    abyss_points: Option<(u64, u64, i64)>,
    login_kinah: Option<(u64, i64)>,
    login_abyss_points: Option<(u64, u64, i64)>,
    /// The Daily Duty board. See `parse_daily_duty_packet`.
    daily_duty: Option<DutyBoard>,
    /// The entry list's time entries (kind `01`) by id: the time and when it
    /// was seen (unix ms). An Abyss passage's usage time left (ms), the times
    /// of others. Gone again as a count.
    content_times: std::collections::BTreeMap<u32, (u64, i64)>,

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
    /// The server your last self record stated (0 when none, or when the
    /// last one was a tutorial character's). Outlives the local name, which
    /// is dropped while the game moves you to a new entity (`note_self_stats`)
    /// and comes back with the next self record: a fight saved in between is
    /// still yours, on your server. See `fight_server_id`.
    self_server: u16,
    /// The character `self_server` was stated for: another one the game
    /// names (a switch, named by loot before its own self record) forgets
    /// it, so it does not inherit the last one's server (#133).
    self_server_owner: Option<String>,
    /// The class and level your own self record last stated, with the name it
    /// was for, so a character switch does not carry the last one's over.
    self_profile: Option<(String, Option<JobClass>, Option<u32>)>,
    /// The gear score the last self record gave, with the name it was for.
    self_gear_score: Option<(String, i32)>,
    /// The combat power the last self record gave, with the name it was for.
    self_combat_power: Option<(String, i64)>,
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
    /// How many `06 38` records named each entity. See `scope_leader`.
    scope_counts: HashMap<i32, u32>,
    /// The local id in force was read from those counts, not stated by the
    /// game or chosen in the UI. See `note_party_scope`.
    local_from_scope: bool,
    /// The local id in force was read from `4A 36` records (see
    /// `note_self_stats`). `local_from_scope` is set with it, since such an id
    /// has no name either; this one keeps the `06 38` counts from moving it.
    local_from_stats: bool,
    /// The last entity `4A 36` records named in a row, other than the local
    /// id in force, and how many times in a row.
    stats_streak: Option<(i32, u32)>,
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
                names_generation: 0,
                party_generation: 0,
                zone_generation: 0,
                instance_generation: 0,
                party_ended_at_ms: NEVER_MS,
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
                former_party_members: HashMap::new(),
                party_roster_at_ms: 0,
                own_party_key: None,
                self_in_roster: false,
                damage_since_roster_bind: 0,
                party_placeholders_hidden: false,
                current_dungeon_id: 0,
                roster_dungeon_id: 0,
                current_map_id: None,
                actor_power_scalars: HashMap::new(),
                hostile_target_ids: HashSet::new(),
                dead_entity_ids: HashSet::new(),
                death_log: VecDeque::new(),
                boss_maps: Vec::new(),
                boss_map_seq: 0,
                abyss_artifacts: crate::capture::abyss_artifacts::ArtifactState { wars: Vec::new(), artifacts: Vec::new() },
                abyss_artifacts_seen_ms: 0,
                abyss_artifacts_seq: 0,
                supply_requests: Vec::new(),
                supply_requests_seq: 0,
                spawn_hp_ids: HashSet::new(),
                boss_hp_now: HashMap::new(),
                boss_hp_sent: HashMap::new(),
                boss_events: VecDeque::new(),
                a2tools_users: HashSet::new(),
                last_hp: HashMap::new(),
                despawned_summon_ids: HashSet::new(),
                boss_entity_ids: HashSet::new(),
                training_dummy_ids: HashSet::new(),
                hp_floored_ids: HashSet::new(),
                hp_reset_dummy_ids: HashSet::new(),
                held_dot_ticks: HashMap::new(),
                has_boss_in_segment: false,
                current_target: 0,
                game_host: None,
                host_server: 0,
                odyle_energy: None,
                odyle_owner: None,
                odyle_awaits_self_record: false,
                content_counts: Default::default(),
                shop_limits: Default::default(),
                craft_counts: Default::default(),
                kinah_today: None,
                abyss_points: None,
                login_kinah: None,
                login_abyss_points: None,
                daily_duty: None,
                content_times: Default::default(),
                local_player_id: None,
                supporters: std::sync::Arc::new(crate::supporters::Roster::default()),
                local_character_name: None,
                local_identity_from_game: false,
                loot_identity: LootIdentity::default(),
                player_servers: HashMap::new(),
                self_server: 0,
                self_server_owner: None,
                self_profile: None,
                self_gear_score: None,
                self_combat_power: None,
            }),
            damage_generation: AtomicI64::new(0),
            last_damage_ms: AtomicI64::new(NEVER_MS),
            last_zone_reset_ms: AtomicI64::new(NEVER_MS),
            combat_reset_requested: AtomicBool::new(false),
            before_reset: RwLock::new(None),
            abnormals: Mutex::new(AbnormalLog { timeline: abnormal::Timeline::default(), pruned_ms: NEVER_MS }),
            stagger: Mutex::new(StaggerLog::default()),
            death_recaps: Mutex::new(DeathRecaps::default()),
        }
    }

    /// Each stacking abnormal's stack limit (`abnormal::stack_limits`), so a
    /// stack pushed out past it ends where the game ends it.
    pub fn set_abnormal_stack_limits(&self, limits: HashMap<u32, u32>) {
        self.abnormals.lock().timeline.set_stack_limits(limits);
    }

    /// Hand one framed packet to the buff timeline, at the current time.
    pub fn note_abnormal_packet(&self, packet: &[u8]) {
        let ms = now_ms();
        let mut log = self.abnormals.lock();
        // A replay's clock can jump back, to another capture: prune again.
        if ms >= log.pruned_ms.saturating_add(ABNORMAL_PRUNE_EVERY_MS) || ms < log.pruned_ms {
            log.timeline.prune(ms.saturating_sub(ABNORMAL_RETENTION_MS));
            log.pruned_ms = ms;
        }
        log.timeline.note(ms, packet);
    }

    /// Every abnormal instance on any time from `start_ms` to `end_ms`.
    pub fn abnormal_instances(&self, start_ms: i64, end_ms: i64) -> Vec<abnormal::Instance> {
        self.abnormals.lock().timeline.instances_between(start_ms, end_ms)
    }

    /// Stat records (your own stat sheet) from `start_ms` to `end_ms`.
    pub fn stat_events(&self, start_ms: i64, end_ms: i64) -> Vec<abnormal::StatEvent> {
        let log = self.abnormals.lock();
        log.timeline.stats.iter().filter(|s| s.ms >= start_ms && s.ms <= end_ms).cloned().collect()
    }

    /// Instances held by the buff timeline, ended and still on.
    pub fn abnormal_count(&self) -> usize {
        self.abnormals.lock().timeline.len()
    }

    /// A fight's buffs and debuffs: the tracks (`abnormal::tracks`) on any
    /// time from `start_ms` to `end_ms` on `entities` and on their summons,
    /// every entity's when `entities` is empty. A caster is resolved to the
    /// owner of its summon chain, so a spirit's buff is its summoner's.
    pub fn fight_abnormal_tracks(&self, start_ms: i64, end_ms: i64, entities: &HashSet<i32>) -> Vec<abnormal::Track> {
        let links = self.get_summon_data();
        let instances: Vec<abnormal::Instance> = self
            .abnormal_instances(start_ms, end_ms)
            .into_iter()
            .filter(|i| {
                entities.is_empty()
                    || entities.contains(&i.entity)
                    || entities.contains(&summon_resolver::resolve(i.entity, &links))
            })
            .collect();
        abnormal::tracks(&instances, |caster| summon_resolver::resolve(caster, &links))
    }

    /// Forget the buff timeline, for a capture from another session.
    pub fn forget_abnormals(&self) {
        self.abnormals.lock().timeline.clear();
    }

    /// The stagger gauge an entity's spawn carries (its starting value).
    pub fn note_stagger_spawn(&self, entity: i32, max: u32, rest: u32) {
        self.stagger.lock().note_spawn(entity, max, rest);
    }

    /// How far `entity`'s stagger gauge is worn down, 0-100, if it has one.
    pub fn stagger_pct(&self, entity: i32) -> Option<f64> {
        let (max, rest) = self.stagger.lock().gauge(entity)?;
        (max > 0).then(|| f64::from(max.saturating_sub(rest)) * 100.0 / f64::from(max))
    }

    /// A gauge update (`05 E0 .. 03 01`), at the current time.
    pub fn note_stagger_gauge(&self, entity: i32, max: u32, rest: u32, status: u8) {
        tracing::debug!("Stagger gauge: entity {} at {}/{} status {}", entity, rest, max, status);
        self.stagger.lock().note_gauge(entity, max, rest, status, now_ms());
    }

    /// A gauge broke (`05 E0 .. 00 03 02`), at the current time.
    pub fn note_stagger_break(&self, entity: i32) {
        tracing::debug!("Stagger gauge: entity {} broken", entity);
        self.stagger.lock().note_break(entity, now_ms());
    }

    /// A hit with a value, or a DoT tick, that the next update of the
    /// target's gauge may take (`StaggerLog::note_hit`).
    pub fn note_stagger_hit(&self, target: i32, actor: i32, skill: i32, dot: bool) {
        self.stagger.lock().note_hit(target, actor, skill, now_ms(), dot);
    }

    /// An entry of a hit with no value (`StaggerLog::note_hit_entry`).
    pub fn note_stagger_hit_entry(&self, target: i32, actor: i32, skill: i32) {
        self.stagger.lock().note_hit_entry(target, actor, skill);
    }

    /// The gauge of `entity` as last known: (max, rest).
    #[cfg(test)]
    pub fn stagger_gauge(&self, entity: i32) -> Option<(u32, u32)> {
        self.stagger.lock().gauge(entity)
    }

    /// Stagger taken off the gauges of `entities` by each actor and skill
    /// from `start_ms` to `end_ms`, summons as themselves (the meter resolves
    /// them as it does damage). Deductions with no actor are left out.
    pub fn stagger_totals(&self, start_ms: i64, end_ms: i64, entities: &HashSet<i32>) -> HashMap<(i32, i32), i64> {
        let mut out: HashMap<(i32, i32), i64> = HashMap::new();
        for w in self.stagger.lock().windows_between(start_ms, end_ms) {
            if !entities.contains(&w.entity) {
                continue;
            }
            for h in w.hits.iter().filter(|h| h.actor != 0 && h.ms >= start_ms && h.ms <= end_ms) {
                *out.entry((h.actor, h.skill)).or_default() += h.value as i64;
            }
        }
        out
    }

    /// The stagger window open on `entity`, if any.
    #[cfg(test)]
    pub fn stagger_open_window(&self, entity: i32) -> Option<crate::combat::stagger::Window> {
        self.stagger.lock().open_window(entity).cloned()
    }

    /// The stagger windows with a deduction from `start_ms` to `end_ms`
    /// (`StaggerLog::windows_between`), for a fight starting at `start_ms`:
    /// times from its start, summons resolved to their owners.
    pub fn fight_stagger(&self, start_ms: i64, end_ms: i64) -> Vec<StaggerWindow> {
        let windows = self.stagger.lock().windows_between(start_ms, end_ms);
        let inner = self.inner.read();
        windows
            .into_iter()
            .map(|w| StaggerWindow {
                entity: w.entity,
                mob_code: inner.mob_storage.get(&w.entity).copied().unwrap_or(0),
                max: w.max,
                start: w.start_ms - start_ms,
                end: w.end_ms.unwrap_or(end_ms) - start_ms,
                result: w.outcome().unwrap_or(Outcome::Missed),
                ended: w.ended.unwrap_or(End::FightEnd),
                missing: w.missing,
                incomplete: w.incomplete,
                hits: w
                    .hits
                    .iter()
                    .map(|h| StaggerHit {
                        t: h.ms - start_ms,
                        actor: summon_resolver::resolve(h.actor, &inner.summon_storage),
                        skill: h.skill,
                        value: h.value,
                        ambiguous: h.ambiguous,
                    })
                    .collect(),
            })
            .collect()
    }

    /// Called when a self/world teleport (zone-change opcode) is seen. Resets
    /// combat data only if not in active combat (lull) and not recently reset
    /// (debounce), so the meter starts clean on entering a dungeon/instance
    /// without ever wiping an in-progress fight. Returns true if it reset.
    pub fn note_zone_change(&self) -> bool {
        let now = now_ms();
        if now.saturating_sub(self.last_damage_ms.load(Ordering::Relaxed)) < ZONE_RESET_LULL_MS {
            return false; // mid-combat teleport — ignore
        }
        if now.saturating_sub(self.last_zone_reset_ms.load(Ordering::Relaxed)) < ZONE_RESET_DEBOUNCE_MS {
            return false; // already reset moments ago
        }
        {
            let inner = self.inner.read();
            if inner.target_combat.is_empty() {
                return false; // nothing to clear
            }
        }
        self.last_zone_reset_ms.store(now, Ordering::Relaxed);
        self.run_before_reset();
        // Preserve identity across the reset: a teleport within the same instance
        // keeps everyone's entity ids, so wiping nicknames/known-players/summons
        // would drop your party (and you) to raw ids until they happen to be
        // re-broadcast. Clear only the per-segment damage aggregates.
        self.flush_combat_only();
        self.combat_reset_requested.store(true, Ordering::Relaxed);
        tracing::info!("Zone change detected — combat data reset (identity preserved)");
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

    /// When the meter last counted damage, dealt or taken (clock ms);
    /// `None` if it has not since the meter started. "Hide out of combat"
    /// (app.rs) times its wait from it.
    pub fn last_damage_ms(&self) -> Option<i64> {
        Some(self.last_damage_ms.load(Ordering::Relaxed)).filter(|&ms| ms != NEVER_MS)
    }

    /// When the last zone-change combat reset happened (clock ms), `NEVER_MS`
    /// if it has not.
    pub fn last_zone_reset_ms(&self) -> i64 {
        self.last_zone_reset_ms.load(Ordering::Relaxed)
    }

    /// When the party last ended (`end_party`, which resets combat too),
    /// clock ms; `NEVER_MS` if it has not.
    pub fn party_ended_at_ms(&self) -> i64 {
        self.inner.read().party_ended_at_ms
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
        if let Some(kinah) = inner.login_kinah.take() {
            inner.kinah_today = Some(kinah);
        }
        if let Some(points) = inner.login_abyss_points.take() {
            inner.abyss_points = Some(points);
        }
        let known = inner.odyle_energy.is_some()
            || !inner.content_counts.is_empty()
            || !inner.content_times.is_empty()
            || inner.daily_duty.is_some()
            || !inner.shop_limits.is_empty()
            || !inner.craft_counts.is_empty()
            || inner.kinah_today.is_some()
            || inner.abyss_points.is_some();
        if known && (inner.odyle_awaits_self_record || inner.odyle_owner.is_none()) {
            inner.odyle_owner = name.clone();
            inner.odyle_awaits_self_record = false;
        }
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
        // Your own-stats records have said which entity is you: loot can
        // name that entity, not move you off it.
        if inner.loot_identity.local_from_stats && inner.local_player_id != Some(owner_id as i64) {
            return false;
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
        let counts = &mut inner.loot_identity.scope_counts;
        if counts.len() >= 10_000 && !counts.contains_key(&entity_id) {
            return;
        }
        let count = counts.entry(entity_id).or_insert(0);
        *count += 1;
        // A meter opened mid-session has no self record until the next zone
        // load, and loot records only come with kills: at the training dummies
        // that was 18 minutes of not knowing who you are (2026-10-07), so no
        // fight of yours in Boss mode and your row unmarked. These records
        // name you all along, so until the game says otherwise, you are the
        // player they name far more often than anyone.
        if *count % 8 != 0 {
            return;
        }
        let undecided = inner.local_player_id.is_none() || inner.loot_identity.local_from_scope;
        if inner.local_identity_from_game || !undecided || inner.loot_identity.local_from_stats {
            return;
        }
        if let Some(leader) = scope_leader(&inner) {
            if inner.local_player_id != Some(leader as i64) {
                tracing::info!("party-scope records: local player -> entity {}", leader);
                inner.local_player_id = Some(leader as i64);
                inner.loot_identity.local_from_scope = true;
            }
        }
    }

    /// The server sent a `4A 36` record about `entity_id`.
    ///
    /// These go to the local player about the local player alone: in every
    /// capture at hand that has them (EU, 2026-10-01 to 2026-10-09) each
    /// entity they name is one the self record names as you, never a party
    /// member, a spirit or a mob. They start the moment a zone loads, ahead
    /// of the self record, and keep coming while you play.
    ///
    /// The self record (`33 36`) is sent on a zone load and, at best, every
    /// few minutes after, so an id it stated goes stale whenever a load's copy
    /// is missed: a 17 KB bundle at a dungeon's entrance threw the framing
    /// out and swallowed it (2026-10-09), and the meter kept the previous
    /// zone's id for two and a half minutes, a boss fight with it, your row
    /// saved as a masked `#id`. So when these name another entity three times
    /// running, that entity is you. It gets no name: the one the game last
    /// stated, like the UI's, can be another character's after a switch. The
    /// next self record names it.
    /// Returns whether the local identity changed.
    pub fn note_self_stats(&self, entity_id: i32) -> bool {
        const RECORDS_IN_A_ROW: u32 = 3;
        if !(100..=9_999_999).contains(&entity_id) {
            return false;
        }
        let mut inner = self.inner.write();
        if inner.local_player_id == Some(entity_id as i64) {
            inner.loot_identity.stats_streak = None;
            return false;
        }
        let streak = match inner.loot_identity.stats_streak {
            Some((id, n)) if id == entity_id => n + 1,
            _ => 1,
        };
        inner.loot_identity.stats_streak = Some((entity_id, streak));
        if streak < RECORDS_IN_A_ROW {
            return false;
        }
        tracing::info!(
            "own-stats records: local player -> entity {} (was {:?})",
            entity_id,
            inner.local_player_id
        );
        inner.loot_identity.stats_streak = None;
        inner.local_player_id = Some(entity_id as i64);
        inner.loot_identity.local_from_scope = true;
        inner.loot_identity.local_from_stats = true;
        inner.loot_identity.applied = false;
        if inner.local_identity_from_game {
            // What the game said is about an entity no longer you; the UI's
            // name may come back, but only as a name, not on this id.
            inner.local_identity_from_game = false;
            inner.local_character_name = None;
        }
        true
    }

    /// Whether the UI may make `actor_id` the local player (`bind_local_actor_id`).
    ///
    /// Each meter window (the meter, History, every Details window) sends
    /// back the id it last saw whenever that changes, and binding one puts
    /// the local name on it, which takes the name off whichever entity had
    /// it. So once the self record has named you, only an id the player typed
    /// (`manual`) may move you; an echo of an older one is dropped. A player's
    /// fight (2026-10-09) was saved with their own row a masked, nameless
    /// `#id`, though the self record had named them on that entity six
    /// minutes before and a replay of the packets keeps the name on it: a
    /// bind like this is the one path that takes both the name and "you"
    /// off an entity.
    pub fn ui_may_bind_local_id(&self, actor_id: i64, manual: bool) -> bool {
        manual || !self.local_identity_from_self_record() || self.local_player_id() == Some(actor_id)
    }

    /// The local id was read from `06 38` counts rather than stated by the
    /// game or the UI. Such an id has no name: the UI's is not attached to it,
    /// since after a character switch the window title and the remembered
    /// name can be another character's.
    pub fn local_id_from_scope(&self) -> bool {
        self.inner.read().loot_identity.local_from_scope
    }

    /// `name`'s home server, as a self or loot record states it.
    pub fn note_player_server(&self, name: &str, server_id: u16) {
        // Only a server that exists: a misread one stuck to a player and went
        // out with their boss reports (2258 for ten days, 2026-10-27).
        if !crate::capture::servers::exists(server_id) {
            return;
        }
        let mut inner = self.inner.write();
        // Bounded: one entry per character met, and a session meets hundreds.
        if inner.player_servers.len() < 10_000 || inner.player_servers.contains_key(name) {
            inner.player_servers.insert(name.to_string(), server_id);
        }
    }

    /// `name`'s home server, if a self or loot record has stated it; else 0.
    pub fn player_server(&self, name: &str) -> u16 {
        self.inner.read().player_servers.get(name.trim()).copied().unwrap_or(0)
    }

    /// The server to file `name` under in a saved fight, as the game's
    /// records state it. You: your self record's, the game's word on where you
    /// are, and Some(0), unknown, when none has come this session, never a
    /// guess (#128). Anyone else: what a self or loot record stated, None when
    /// nothing has, for the caller's roster value. Both come before the
    /// roster's, the top bits of an id a character moved to another server
    /// keeps, so it names the old one.
    pub fn stated_server(&self, name: &str) -> Option<u16> {
        let name = name.trim();
        let inner = self.inner.read();
        if inner.local_character_name.as_deref().map(str::trim) == Some(name) {
            return Some(inner.self_server);
        }
        inner.player_servers.get(name).copied()
    }

    /// The server a saved fight is filed under: your self record's, 0
    /// (unknown) when none has come this session. Not `fight_server_id`,
    /// whose fallbacks (your roster entry, the connected host's server, the
    /// party's majority) guess, and guessed wrong for a moved character or in
    /// a cross-server dungeon (#128). The game sends the record on every zone
    /// you enter.
    pub fn self_record_server(&self) -> u16 {
        self.inner.read().self_server
    }

    /// Your home server, as your own self record states it; 0 for a record
    /// that states none (a tutorial character), which forgets the last one.
    pub fn note_self_server(&self, server_id: u16) {
        if server_id == 0 || crate::capture::servers::exists(server_id) {
            let mut inner = self.inner.write();
            inner.self_server = server_id;
            inner.self_server_owner = inner.local_character_name.as_deref().map(|n| n.trim().to_string());
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

    /// Your gear score as the self record states it (sent on login and on
    /// every teleport).
    pub fn note_self_gear_score(&self, name: &str, gear_score: i32) {
        self.inner.write().self_gear_score = Some((name.to_string(), gear_score));
    }

    /// Your combat power as the self record states it.
    pub fn note_self_combat_power(&self, name: &str, combat_power: i64) {
        self.inner.write().self_combat_power = Some((name.to_string(), combat_power));
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
        // The roster's is the fresher while in a party (it follows gear
        // changes); the self record's is from the last teleport.
        let me = name.as_deref().map(str::trim);
        let roster = me.and_then(|n| inner.party_members.get(n));
        let gear_score =
            roster.map(|m| m.gear_score).filter(|&g| g > 0).or_else(|| stated_for(&inner.self_gear_score, me));
        let combat_power =
            roster.map(|m| m.combat_power).filter(|&c| c > 0).or_else(|| stated_for(&inner.self_combat_power, me));
        LocalProfile { name, server_id: server, class, level, gear_score, combat_power }
    }

    /// The server the fights being recorded are on: the local player's home
    /// server, else the party's. 0 when nothing has said.
    ///
    /// The local player's is the uploader's: the site files the log under it
    /// (server filter, region). It is not the instance's: a dungeon party can
    /// be cross-server, every member keeping their own id.
    ///
    /// A server id names its region (`1304` is Europe), which is what this is
    /// for: uploaded logs are grouped by region. The local player's own server
    /// comes from the self record (or a loot record naming them); the roster's
    /// is the fallback, by majority, since in a cross-server party each member
    /// keeps their own server but all share the region.
    pub fn fight_server_id(&self) -> u16 {
        let own = self.own_server();
        if own != 0 {
            return own;
        }
        let inner = self.inner.read();
        let local = inner.local_character_name.as_deref().map(str::trim);
        if let Some(member) = local.and_then(|n| inner.party_members.get(n)) {
            if member.server_id != 0 {
                return member.server_id;
            }
        }
        // A meter started mid-game: the server this game server's address
        // was last seen to be (`note_host_server`), before a party's
        // majority, which can be another server's.
        if inner.host_server != 0 {
            return inner.host_server;
        }
        let mut counts: HashMap<u16, usize> = HashMap::new();
        for member in inner.party_members.values() {
            if member.server_id != 0 {
                *counts.entry(member.server_id).or_default() += 1;
            }
        }
        counts.into_iter().max_by_key(|&(server, n)| (n, std::cmp::Reverse(server))).map_or(0, |(s, _)| s)
    }

    /// The local player's own server as the game stated it (a self or loot
    /// record), 0 when it hasn't yet. Unlike `fight_server_id`, never a
    /// guess from the party or the connection. The self record's first: a
    /// loot record states a moved character's old server (#133).
    pub fn own_server(&self) -> u16 {
        let inner = self.inner.read();
        if inner.self_server != 0 {
            return inner.self_server;
        }
        let local = inner.local_character_name.as_deref().map(str::trim);
        if let Some(&server) = local.and_then(|n| inner.player_servers.get(n)) {
            return server;
        }
        // No name in force, or one no record has placed: the last self
        // record's. Between a server move's own-stats records (which drop
        // the name) and the next self record, the fight just ended was saved
        // with no local name, and the roster majority filed it under a party
        // member's server (1014 for a 2014 player, 2026-08-15).
        inner.self_server
    }

    /// The server the game server's address was last seen to be (see
    /// `boss_report::report_server`); 0 when not known.
    pub fn note_host_server(&self, server: u16) {
        let mut inner = self.inner.write();
        if inner.host_server != server {
            inner.host_server = server;
        }
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
        let mut inner = self.inner.write();
        if inner.local_player_id != id {
            inner.loot_identity.local_from_scope = false;
            inner.loot_identity.local_from_stats = false;
        }
        inner.local_player_id = id;
    }

    pub fn local_player_id(&self) -> Option<i64> {
        self.inner.read().local_player_id
    }

    /// The Odyle energy the game sent: basic (spent first, recharged every
    /// cycle) and charged (from items). `whole_list`: the list sent at login
    /// and after a server transfer, whose owner the next self record names.
    pub fn set_odyle_energy(&self, basic: u32, charged: u32, whole_list: bool) {
        let mut inner = self.inner.write();
        own_list(&mut inner, whole_list);
        if inner.odyle_energy != Some((basic, charged)) {
            tracing::info!("Odyle energy: {basic} basic, {charged} charged");
            inner.odyle_energy = Some((basic, charged));
        }
    }

    /// The game's entry counts, without the energy or times (see
    /// `set_entry_list`).
    pub fn set_content_counts(&self, counts: crate::capture::stream_processor::ContentCounts, whole_list: bool) {
        let entries = crate::capture::stream_processor::ContentEntries { counts, times: Vec::new() };
        self.set_entry_list(entries, None, whole_list, now_ms());
    }

    /// The game's entry counts, its times seen at `now` (unix ms) and the
    /// Odyle energy among them, at once: the whole list (`whole_list`, at
    /// login and after a server transfer) or the entries that changed. A
    /// whole list replaces the last character's counts, times and energy,
    /// even one that didn't read (no entries, no `energy`). A time sent again
    /// unchanged keeps when it was first seen. Logged, so the ids not yet
    /// known can be matched to what the game shows.
    pub fn set_entry_list(
        &self,
        entries: crate::capture::stream_processor::ContentEntries,
        energy: Option<(u32, u32)>,
        whole_list: bool,
        now: i64,
    ) {
        let crate::capture::stream_processor::ContentEntries { counts, times } = entries;
        let mut inner = self.inner.write();
        own_list(&mut inner, whole_list);
        if let Some((basic, charged)) = energy.filter(|&e| inner.odyle_energy != Some(e)) {
            tracing::info!("Odyle energy: {basic} basic, {charged} charged");
        }
        if whole_list || energy.is_some() {
            inner.odyle_energy = energy;
        }
        if whole_list {
            inner.content_counts.clear();
            // Each character has its own Daily Duty board, sent right after
            // the list when it is sent at all (not at Nully's logins,
            // 2026-10-24): the last character's isn't theirs. The limit is
            // the server's, the sum of each character's today: a plugin
            // keeping each character's adds them up.
            inner.daily_duty = None;
            // The shop's limits and the crafts are the character's too
            // (a server-wide recipe's count is the same on each).
            inner.shop_limits.clear();
            inner.craft_counts.clear();
            // Kinah and Abyss Points too: the new character's come with the
            // login (held until its self record, `login_kinah`).
            inner.kinah_today = None;
            inner.abyss_points = None;
            inner.content_times.clear();
            let list: Vec<String> = counts.iter().map(|(id, v)| format!("{id}={}+{}", v[0], v[1])).collect();
            tracing::debug!("Content counts: {}", list.join(" "));
        }
        for (id, v) in counts {
            inner.content_times.remove(&id);
            if !whole_list && inner.content_counts.get(&id) != Some(&v) {
                tracing::debug!("Content count {id}: {} (+{})", v[0], v[1]);
            }
            inner.content_counts.insert(id, v);
        }
        for (id, time) in times {
            inner.content_counts.remove(&id);
            match inner.content_times.get(&id) {
                Some(&(t, _)) if t == time => {}
                _ => {
                    tracing::info!("Content time {id}: {time}");
                    inner.content_times.insert(id, (time, now));
                }
            }
        }
    }

    /// The entry list's times by id: `(time, seen at unix ms)`.
    pub fn content_times(&self) -> std::collections::BTreeMap<u32, (u64, i64)> {
        self.inner.read().content_times.clone()
    }

    /// The entry counts, and Daily Duty as id 0 (no entry of the game's):
    /// `[completed, taken]` today, both 0 once its reset has passed.
    pub fn content_counts(&self) -> std::collections::BTreeMap<u32, [u32; 2]> {
        content_counts_of(&self.inner.read(), now_ms())
    }

    /// The entry list as plugins see it, read at one moment: whose it is,
    /// the Odyle energy, the counts and the times.
    pub fn entry_list(&self) -> EntryList {
        let inner = self.inner.read();
        let now = now_ms();
        EntryList {
            owner: inner.odyle_owner.clone(),
            odyle_energy: inner.odyle_energy,
            counts: content_counts_of(&inner, now),
            shop_limits: shop_limits_of(&inner, now),
            crafts: inner.craft_counts.clone(),
            kinah_today: kinah_today_of(&inner, now),
            abyss_points: abyss_points_of(&inner, now),
            times: inner.content_times.clone(),
        }
    }

    /// Shop purchase limits: `(item id, bought, next reset unix ms)`, the
    /// whole list (`whole`, replacing the last) or the ones that changed.
    /// One shop's list seen (seven items, the same at each opening): if other
    /// shops send their own, each whole list would replace the other's.
    pub fn set_shop_limits(&self, limits: &[(u32, u32, i64)], whole: bool) {
        let mut inner = self.inner.write();
        own_list(&mut inner, false);
        if whole {
            inner.shop_limits.retain(|id, _| limits.iter().any(|l| l.0 == *id));
        }
        for &(id, bought, reset_ms) in limits {
            if inner.shop_limits.insert(id, (bought, reset_ms)) != Some((bought, reset_ms)) {
                tracing::info!("Shop limit {id}: {bought} bought (resets {reset_ms})");
            }
        }
    }

    /// The shop's purchase limits by item id, `(bought, next reset unix ms)`.
    /// Once its reset has passed, 0 bought and the next reset unknown (`None`)
    /// until the shop opens again: the game doesn't say how often each resets.
    pub fn shop_limits_at(&self, now: i64) -> std::collections::BTreeMap<u32, (u32, Option<i64>)> {
        shop_limits_of(&self.inner.read(), now)
    }

    /// Crafts made by recipe: the crafting window's whole list (`whole`) or
    /// one recipe after a craft.
    pub fn set_craft_counts(&self, counts: &[(u32, u32)], whole: bool) {
        let mut inner = self.inner.write();
        own_list(&mut inner, false);
        if whole {
            inner.craft_counts.clear();
        }
        for &(recipe, n) in counts {
            if !whole && inner.craft_counts.get(&recipe) != Some(&n) {
                tracing::info!("Craft count {recipe}: {n}");
            }
            inner.craft_counts.insert(recipe, n);
        }
    }

    pub fn craft_counts(&self) -> std::collections::BTreeMap<u32, u32> {
        self.inner.read().craft_counts.clone()
    }

    /// Kinah gained today as of `at` (the game's unix ms): at a login
    /// (`login`, held for the self record) or a gain.
    pub fn set_kinah_today(&self, kinah: u64, at: i64, login: bool) {
        let mut inner = self.inner.write();
        tracing::debug!("Kinah today: {kinah}{}", if login { " (login)" } else { "" });
        if login {
            inner.login_kinah = Some((kinah, at));
        } else {
            own_list(&mut inner, false);
            // Newer than a login's still held.
            inner.login_kinah = None;
            inner.kinah_today = Some((kinah, at));
        }
    }

    /// Kinah gained today at `now` (unix ms): 0 once a daily reset has
    /// passed since the game said it.
    pub fn kinah_today_at(&self, now: i64) -> Option<u64> {
        kinah_today_of(&self.inner.read(), now)
    }

    /// The week's Abyss Points and limit, seen at `at` (unix ms). Sent at
    /// logins and transfers only, so held for the self record.
    pub fn set_abyss_points(&self, points: u64, limit: u64, at: i64) {
        let mut inner = self.inner.write();
        tracing::debug!("Abyss Points: {points} of {limit}");
        inner.login_abyss_points = Some((points, limit, at));
    }

    /// The week's Abyss Points and limit at `now` (unix ms): 0 points once a
    /// weekly reset has passed since seen.
    pub fn abyss_points_at(&self, now: i64) -> Option<(u64, u64)> {
        abyss_points_of(&self.inner.read(), now)
    }

    /// Daily Duty at `now` (unix ms): `(completed, taken)`, taken counting
    /// the completed too (the game's n/5 is taken). 0 once the board's reset
    /// has passed, until an offer is taken (`daily_duty_taken`); None before
    /// a board was seen. Completed is state 04 (a
    /// turn-in, 2026-10-24; 03 would be ready to hand in, not counted).
    pub fn daily_duty_at(&self, now: i64) -> Option<(u32, u32)> {
        daily_duty_of(&self.inner.read(), now)
    }

    pub fn set_daily_duty_board(&self, offers: Vec<DutyOffer>, reset_ms: i64) {
        let taken = offers.iter().filter(|o| o.state != 1).count();
        let states: Vec<String> = offers.iter().filter(|o| o.state != 1).map(|o| format!("{}={:02x}", o.offer, o.state)).collect();
        tracing::info!("Daily Duty: {taken} of {} offers taken ({})", offers.len(), states.join(" "));
        let mut inner = self.inner.write();
        own_list(&mut inner, false);
        inner.daily_duty = Some(DutyBoard { offers, reset_ms, sent_reset_ms: reset_ms });
    }

    /// A quest taken at `now` (unix ms) (`20 8d 01`, which may not be only
    /// Daily Duty's): it counts when it is an offer of the board, and is
    /// known by the quest number from then. Past the board's reset the board
    /// is the day before's, and the game sends the new one only at the next
    /// login, transfer or turn-in: the new day starts with none taken (the
    /// old day's go, handed in or not), and a quest taken counts when it
    /// looks like a duty offer (`looks_like_duty`).
    pub fn daily_duty_taken(&self, offer: u32, quest: u32, number: u32, now: i64) {
        const DAY_MS: i64 = 24 * 60 * 60 * 1000;
        let mut inner = self.inner.write();
        let Some(board) = inner.daily_duty.as_mut() else { return };
        let duty = DutyOffer { offer, quest, number, state: 2 };
        let new_day = now >= board.reset_ms;
        if new_day && !looks_like_duty(&board.offers, &duty) {
            return;
        }
        if new_day {
            for o in board.offers.iter_mut() {
                o.number = 0;
                o.state = 1;
            }
            board.reset_ms += ((now - board.reset_ms) / DAY_MS + 1) * DAY_MS;
        }
        // The board is the day before's until the game sends the new one.
        let stale = board.reset_ms != board.sent_reset_ms;
        let like = looks_like_duty(&board.offers, &duty);
        match board.offers.iter_mut().find(|o| o.offer == offer && o.quest == quest) {
            Some(o) => *o = duty,
            None if stale && like => {
                // Counted by the rule alone: logged, to check it against
                // the board the game sends next.
                tracing::info!("Daily Duty: quest {quest} counted as like the board's, before the new board");
                board.offers.push(duty);
            }
            None => return,
        }
        own_list(&mut inner, false);
        tracing::info!("Daily Duty: offer {offer} taken (quest {quest}, number {number})");
    }

    /// A quest given up: if one of the board's, it is open again.
    pub fn daily_duty_given_up(&self, number: u32) {
        let mut inner = self.inner.write();
        if let Some(o) = inner.daily_duty.as_mut().and_then(|b| b.offers.iter_mut().find(|o| o.number == number && o.state != 1)) {
            o.number = 0;
            o.state = 1;
            tracing::info!("Daily Duty: offer {} given up", o.offer);
        }
    }


    pub fn odyle_energy(&self) -> Option<(u32, u32)> {
        self.inner.read().odyle_energy
    }

    /// The game server the locked connection reads from (see `game_host`).
    pub fn set_game_host(&self, host: Option<std::net::Ipv4Addr>) {
        self.inner.write().game_host = host;
    }

    pub fn game_host(&self) -> Option<std::net::Ipv4Addr> {
        self.inner.read().game_host
    }

    /// Whose the Odyle energy is, once the game has said (see `odyle_owner`).
    pub fn odyle_owner(&self) -> Option<String> {
        self.inner.read().odyle_owner.clone()
    }

    pub fn append_damage(&self, pdp: ParsedDamagePacket) {
        let mut inner = self.inner.write();
        let skill_code = pdp.skill_code();
        let actor_id = pdp.actor_id();
        let target_id = pdp.target_id();

        // Not damage: a spirit and its owner naming each other.
        if let Some((summon, owner)) = owner_link(skill_code, actor_id, target_id) {
            // Sent only while the spirit is out.
            inner.despawned_summon_ids.remove(&summon);
            if link_summon(&mut inner, summon, owner) {
                tracing::debug!("Summon {} linked to owner {} by skill {}", summon, owner, skill_code);
                self.damage_generation.fetch_add(1, Ordering::Relaxed);
            }
            return;
        }

        // A tick from a spirit that has left the world: the game does not
        // count it. A direct hit means the id is back.
        if pdp.is_dot() {
            if inner.despawned_summon_ids.contains(&actor_id) {
                return;
            }
        } else {
            inner.despawned_summon_ids.remove(&actor_id);
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
                if let Some(actor_data) = fight_of(&mut inner, resolved_target, Some(actor_id)) {
                    actor_data.damage_received += dmg;
                    actor_data.hits_received += 1;
                    actor_data.received.note(pdp.specials());
                    self.damage_generation.fetch_add(1, Ordering::Relaxed);
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

        // Another player's hit on you, for a death recap.
        if let Some(me) = inner.local_player_id.and_then(|id| i32::try_from(id).ok()) {
            let attacker = summon_resolver::resolve(actor_id, &inner.summon_storage);
            if summon_resolver::resolve(target_id, &inner.summon_storage) == me
                && attacker != me
                && inner.known_player_ids.contains(&attacker)
            {
                self.death_recaps.lock().note_hit(attacker, pdp.timestamp(), pdp.total_damage() as i64, skill_code);
            }
        }

        // Party healing: player-on-player damage is actually healing/buffs
        if is_friendly_action(&inner, actor_id, target_id) {
            let heal_amount = pdp.total_damage();
            if heal_amount > 0 {
                if let Some(actor_data) = fight_of(&mut inner, actor_id, None) {
                    actor_data.party_heal += heal_amount as i64;
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
                e.ticks.push((pdp.timestamp(), heal_amount as i64));
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

    /// A hit that did no damage, on the skill's row: only where the actor
    /// already has damage on this target, so no target or meter row appears
    /// and no total, hit count or fight time moves. False when not counted.
    pub fn append_no_damage_hit(&self, target_id: i32, actor_id: i32, skill_code: i32, kind: NoDamageHit) -> bool {
        let mut inner = self.inner.write();
        let Some(actor) = inner.target_combat.get_mut(&target_id).and_then(|t| t.actors.get_mut(&actor_id)) else {
            return false;
        };
        let skill = actor
            .skills
            .entry((skill_code, false))
            .or_insert_with(|| SkillCombatData::new(skill_code, false));
        match kind {
            NoDamageHit::Miss => skill.miss_count += 1,
            NoDamageHit::Resist => skill.resist_count += 1,
        }
        drop(inner);
        self.damage_generation.fetch_add(1, Ordering::Relaxed);
        true
    }

    pub fn append_mob(&self, mid: i32, code: i32) {
        let mut inner = self.inner.write();
        let before = inner.mob_storage.insert(mid, code);
        // An Abyss core seen up, once per entity (online builds only, as kills).
        #[cfg(feature = "online")]
        if before != Some(code)
            && crate::capture::abyss_cores::is_core(code)
            && inner.current_dungeon_id <= 0
            && !inner.dead_entity_ids.contains(&mid)
        {
            push_boss_event(&mut inner, BossEvent { npc: code, kind: BossEventKind::Seen { at_ms: now_ms() } });
        }
        #[cfg(not(feature = "online"))]
        let _ = before;
        // A spawn names the NPC: the table says what it is from here on.
        inner.hp_floored_ids.remove(&mid);
        inner.hp_reset_dummy_ids.remove(&mid);

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

    /// The max HP an entity's spawn record gave, if one came.
    pub fn spawn_max_hp(&self, id: i32) -> Option<i32> {
        let inner = self.inner.read();
        inner.spawn_hp_ids.contains(&id).then(|| inner.mob_hp_data.get(&id).copied()).flatten()
    }

    pub fn append_mob_hp(&self, mid: i32, hp: i32) {
        if hp > 0 {
            let mut inner = self.inner.write();
            inner.mob_hp_data.insert(mid, hp);
            inner.spawn_hp_ids.insert(mid);
            inner.boss_hp_now.remove(&mid);
            inner.boss_hp_sent.remove(&mid);
        }
    }

    /// The party members who also run A2Tools (`party_presence.rs`).
    pub fn set_a2tools_users(&self, names: HashSet<String>) {
        self.inner.write().a2tools_users = names;
    }

    pub fn a2tools_users(&self) -> HashSet<String> {
        self.inner.read().a2tools_users.clone()
    }

    /// The field bosses to report now (`boss_report.rs`, every 3 seconds):
    /// each whose HP changed since the last call, and each still being
    /// fought (its HP moving or it taking damage in the last IN_COMBAT_MS),
    /// changed or not. One whose HP came back up RESET_RISE points or more
    /// is marked as reset. A boss out of range sends nothing (no HP, no
    /// damage); back in range after resetting, its first reading is the jump.
    pub fn take_boss_hp(&self, now_ms: i64) -> Vec<BossHp> {
        let mut inner = self.inner.write();
        let bosses: Vec<(i32, i32, u8, i64)> =
            inner.boss_hp_now.iter().map(|(&id, &(npc, pct, changed))| (id, npc, pct, changed)).collect();
        let mut out = Vec::new();
        for (id, npc, pct, changed) in bosses {
            let hit = inner.target_combat.get(&id).map_or(0, |t| t.last_damage_time);
            let fought = now_ms - changed.max(hit) < IN_COMBAT_MS;
            let sent = inner.boss_hp_sent.get(&id).copied();
            let keep_alive_due = sent.is_some_and(|(_, at)| now_ms - at >= BOSS_HP_KEEP_ALIVE_MS);
            if sent.map(|(p, _)| p) == Some(pct) && !(fought && keep_alive_due) {
                continue;
            }
            let reset = sent.is_some_and(|(s, _)| pct >= s.saturating_add(RESET_RISE));
            inner.boss_hp_sent.insert(id, (pct, now_ms));
            out.push(BossHp { npc, percent: pct, reset });
        }
        out
    }

    /// The field boss kills seen since the last call.
    pub fn take_boss_events(&self) -> Vec<BossEvent> {
        self.inner.write().boss_events.drain(..).collect()
    }

    pub fn mark_entity_dead(&self, entity_id: i32) {
        let mut inner = self.inner.write();
        // A field boss killed in the open world: its exact time, to share
        // (online builds only: boss_report.rs sends it).
        #[cfg(feature = "online")]
        if inner.current_dungeon_id <= 0 && !inner.dead_entity_ids.contains(&entity_id) {
            if let Some(&code) = inner.mob_storage.get(&entity_id) {
                if crate::capture::field_bosses::is_reported(code) {
                    push_boss_event(&mut inner, BossEvent { npc: code, kind: BossEventKind::Killed { at_ms: now_ms() } });
                }
            }
        }
        inner.boss_hp_now.remove(&entity_id);
        inner.boss_hp_sent.remove(&entity_id);
        self.stagger.lock().note_gone(entity_id, now_ms(), true);
        inner.dead_entity_ids.insert(entity_id);
        inner.hp_floored_ids.remove(&entity_id);
        inner.death_log.push_back((entity_id, now_ms(), None));
        while inner.death_log.len() > MAX_DEATHS {
            inner.death_log.pop_front();
        }
    }

    /// A party member's HP (`1b 92`). Dropping to 0 from above logs a death;
    /// coming back up (revived) arms the next one.
    pub fn note_hp(&self, entity_id: i32, hp: i32) {
        let mut inner = self.inner.write();
        let before = inner.last_hp.insert(entity_id, hp);
        if hp == 0 && before.is_some_and(|b| b > 0) {
            inner.death_log.push_back((entity_id, now_ms(), None));
            while inner.death_log.len() > MAX_DEATHS {
                inner.death_log.pop_front();
            }
        } else if hp > 0 && before == Some(0) {
            let now = now_ms();
            if let Some(death) = inner.death_log.iter_mut().rev().find(|d| d.0 == entity_id && d.2.is_none()) {
                death.2 = Some(now);
            }
        }
    }

    /// `note_hp` for a player only: yours or one known by name, never a
    /// monster (whose deaths would crowd the players' out of the log).
    pub fn note_player_hp(&self, entity_id: i32, hp: i32) {
        {
            let inner = self.inner.read();
            let is_player = inner.local_player_id == Some(entity_id as i64)
                || inner.known_player_ids.contains(&entity_id);
            if !is_player || inner.mob_storage.contains_key(&entity_id) {
                return;
            }
        }
        self.note_hp(entity_id, hp);
    }

    /// The deaths from `from` to `to` (ms), oldest first: (entity, time, the
    /// revive's time if they are back up).
    /// A map the player opened, with its field bosses: kept until the same
    /// map is opened again.
    pub fn note_boss_map(&self, map: crate::capture::boss_map::BossMap) {
        let mut inner = self.inner.write();
        inner.boss_maps.retain(|m| m.map_id != map.map_id);
        inner.boss_maps.push(map);
        inner.boss_map_seq += 1;
    }

    /// What another player's record says of them, kept for a death recap.
    pub fn note_player_profile(&self, id: i32, profile: death_recap::Profile) {
        self.death_recaps.lock().note_profile(id, profile);
    }

    /// You were killed: the recap, unless this death was already told. It is
    /// kept only once handed back to `push_death_recap`.
    pub fn died(&self, killer: death_recap::Killer, at_ms: i64) -> Option<death_recap::DeathRecap> {
        let party: Vec<String> = self.get_party_members().into_keys().collect();
        self.death_recaps.lock().died(killer, at_ms, &party)
    }

    pub fn push_death_recap(&self, recap: death_recap::DeathRecap) {
        self.death_recaps.lock().push(recap);
    }

    /// The death recaps made since `seq` was read, and the count to ask with
    /// next time; None when there are none.
    pub fn death_recaps_since(&self, seq: u64) -> Option<(u64, Vec<death_recap::DeathRecap>)> {
        self.death_recaps.lock().since(seq)
    }

    /// The maps opened since `seq` was read (all of them, newest last), and
    /// the count to ask with next time; None when nothing is new.
    pub fn boss_maps_since(&self, seq: u64) -> Option<(u64, Vec<crate::capture::boss_map::BossMap>)> {
        let inner = self.inner.read();
        (inner.boss_map_seq != seq).then(|| (inner.boss_map_seq, inner.boss_maps.clone()))
    }

    /// Who holds the Abyss artifacts, as the game just said: each war and
    /// artifact it names replaces what was known of it.
    pub fn note_abyss_artifacts(&self, told: crate::capture::abyss_artifacts::ArtifactState) {
        let mut inner = self.inner.write();
        let known = &mut inner.abyss_artifacts;
        for war in told.wars {
            known.wars.retain(|w| w.event_id != war.event_id);
            known.wars.push(war);
        }
        for artifact in told.artifacts {
            known.artifacts.retain(|a| a.id != artifact.id);
            known.artifacts.push(artifact);
        }
        known.wars.sort_by_key(|w| w.event_id);
        known.artifacts.sort_by_key(|a| a.id);
        inner.abyss_artifacts_seen_ms = now_ms();
        inner.abyss_artifacts_seq += 1;
    }

    /// The artifact owners and when they were last told, if told since
    /// `seq`, and the count to ask with next time.
    pub fn abyss_artifacts_since(&self, seq: u64) -> Option<(u64, crate::capture::abyss_artifacts::ArtifactState, i64)> {
        let inner = self.inner.read();
        (inner.abyss_artifacts_seq != seq)
            .then(|| (inner.abyss_artifacts_seq, inner.abyss_artifacts.clone(), inner.abyss_artifacts_seen_ms))
    }

    /// The Supply Requests boards the game just sent (at login).
    pub fn note_supply_requests(&self, boards: Vec<crate::capture::supply_requests::Board>) {
        let mut inner = self.inner.write();
        inner.supply_requests = boards;
        inner.supply_requests_seq += 1;
    }

    /// The boards, when told since `seq`: (the new seq, the boards).
    pub fn supply_requests_since(&self, seq: u64) -> Option<(u64, Vec<crate::capture::supply_requests::Board>)> {
        let inner = self.inner.read();
        (inner.supply_requests_seq != seq).then(|| (inner.supply_requests_seq, inner.supply_requests.clone()))
    }

    pub fn deaths_between(&self, from: i64, to: i64) -> Vec<(i32, i64, Option<i64>)> {
        self.inner.read().death_log.iter().filter(|d| d.1 >= from && d.1 <= to).copied().collect()
    }

    /// `id` left the world. Only a linked summon is marked: see
    /// `Inner::despawned_summon_ids`. A stagger window open on it is missed.
    pub fn note_despawn(&self, id: i32) {
        self.stagger.lock().note_gone(id, now_ms(), false);
        let mut inner = self.inner.write();
        if inner.summon_storage.contains_key(&id) {
            inner.despawned_summon_ids.insert(id);
        }
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
        self.set_party_roster_of(None, members, complete);
    }

    /// Is this roster another party's? A whole roster without you, of a party
    /// that is not yours: the finder sends the rosters of other parties too,
    /// and a leader who re-forms a party sends one per party key, the same
    /// title and leader on each. Taken as yours, one of them said you had
    /// left, and the party's end cleared the fight in progress: 38 logs a
    /// re-derive at 2.0.76.4 could not update (2026-10-23). A roster stopped
    /// at a member visiting another world hid it until 0272d47 read them.
    /// When it is your own party's key, it is you who left. Until a roster
    /// with you in it has given your party's key, every roster is taken as
    /// before: in a slice your name and your roster entry can be blinded
    /// apart, and the only roster then has no "you" in it (2026-10-24).
    pub fn is_other_party(&self, key: u32, members: &[(String, PartyMember)], complete: bool) -> bool {
        let inner = self.inner.read();
        let Some(local) = inner.local_character_name.as_deref().map(str::trim).filter(|n| !n.is_empty()) else {
            return false;
        };
        complete
            && inner.own_party_key.is_some_and(|own| own != key)
            && !members.iter().any(|(name, _)| name.trim() == local)
    }

    /// `set_party_roster` for a roster of party `key`, as `0x9702` gives it.
    pub fn set_party_roster_of(&self, key: Option<u32>, members: Vec<(String, PartyMember)>, complete: bool) {
        if members.is_empty() {
            return;
        }
        let mut inner = self.inner.write();
        let local = inner.local_character_name.as_deref().map(str::trim).unwrap_or_default().to_string();
        if !local.is_empty() && members.iter().any(|(name, _)| name.trim() == local) {
            inner.self_in_roster = true;
            if key.is_some() {
                inner.own_party_key = key;
            }
        }
        inner.party_roster_at_ms = now_ms();
        inner.party_placeholders_hidden = false;
        // The meter's rows name the party: someone joining between fights
        // must show without waiting for a hit (see `names_generation`).
        inner.names_generation += 1;

        // Leaving, being kicked, or the party disbanding all show up the same way:
        // the next complete roster has you on your own. Going from a real party
        // down to one member means the party is over, so drop the roster rows and
        // ask for a combat reset — otherwise the meter keeps showing teammates who
        // are no longer with you.
        let was_in_party = inner.party_members.len() >= 2;
        let now_alone = complete && members.len() <= 1;
        let local_name = inner.local_character_name.clone();
        // Not when no roster has had you yet: in a slice your name and your
        // roster entry can be blinded apart, and each whole roster then
        // ended the party, and the fight with it (2026-10-24).
        let dropped_self = complete
            && inner.self_in_roster
            && local_name.as_ref().is_some_and(|n| {
                !n.trim().is_empty() && !members.iter().any(|(name, _)| name.trim() == n.trim())
            });

        if was_in_party && (now_alone || dropped_self) {
            tracing::info!(
                "Party ended ({} -> {} members) — clearing party rows",
                inner.party_members.len(),
                members.len()
            );
            drop(inner);
            self.end_party();
            return;
        }

        let joined = members.iter().any(|(name, _)| !inner.party_members.contains_key(name));
        let left = complete && inner.party_members.keys().any(|name| !members.iter().any(|(n, _)| n == name));
        if joined || left {
            inner.party_generation += 1;
        }
        if complete {
            let gone: Vec<String> = inner.party_members.keys().filter(|n| !members.iter().any(|(m, _)| m == *n)).cloned().collect();
            for name in gone {
                if let Some(member) = inner.party_members.remove(&name) {
                    inner.former_party_members.insert(name, member);
                }
            }
            inner.party_members.clear();
        }
        for (name, member) in members {
            inner.party_members.insert(name, member);
        }
        bind_roster_names_by_class(&mut inner);
    }

    /// You are no longer in a party: drop the roster rows and ask for a
    /// combat reset, so the meter stops showing teammates who are no longer
    /// with you. The fights so far are saved first.
    fn end_party(&self) {
        // Save the party's fights while the roster and instance still say
        // whose they are; the hook reads this storage, so no lock is held.
        self.run_before_reset();
        let mut inner = self.inner.write();
        let members = std::mem::take(&mut inner.party_members);
        inner.former_party_members.extend(members);
        inner.party_generation += 1;
        inner.party_ended_at_ms = now_ms();
        inner.own_party_key = None;
        inner.self_in_roster = false;
        inner.roster_dungeon_id = 0;
        inner.current_dungeon_id = dungeon_of_map(0, inner.current_map_id);
        drop(inner);
        self.flush_combat_only();
        self.combat_reset_requested.store(true, Ordering::Relaxed);
    }

    /// The game said you are no longer in a party: `13 92` for an open-world
    /// party, `1d 97` for a dungeon or raid party (the finder's, whose roster
    /// is `02 97`). You left, were kicked, or it disbanded. An open-world
    /// party sends no roster at all, and leaving a raid others are still in
    /// sends none that has you on your own, so this is the only sign. When
    /// the roster has just ended the party (`1d 97` came 3 s after it at
    /// 00:58 on 2026-10-14), it is not ended again.
    ///
    /// Nor is anything cleared when there is no party to clear: no roster and
    /// nobody but you on the meter. Should the game ever send one of these in
    /// another case (an invite declined or failed, a login, a zone change
    /// while solo), a fight of your own is not wiped.
    pub fn note_party_left(&self) {
        {
            let inner = self.inner.read();
            if now_ms().saturating_sub(inner.party_ended_at_ms) < PARTY_LEFT_AFTER_END_MS {
                return;
            }
            if inner.party_members.is_empty() && !others_on_meter(&inner) {
                tracing::info!("Party left, but no party and nobody else on the meter: nothing to clear");
                return;
            }
        }
        tracing::info!("Party left — clearing party rows");
        self.end_party();
    }

    /// A zone load named the map it loads (`21 36`). The map, once known, is
    /// what says where the player is; see `dungeon_of_map`.
    pub fn note_map_load(&self, map_id: i32) {
        let mut inner = self.inner.write();
        // A load hands out new entity ids: counts of the old ones would name
        // an entity that is gone.
        inner.loot_identity.scope_counts.clear();
        let from = inner.current_map_id.replace(map_id);
        inner.zone_generation += 1;
        if !(from.is_some_and(is_open_world_map) && is_open_world_map(map_id)) {
            inner.instance_generation += 1;
        }
        let dungeon = dungeon_of_map(inner.roster_dungeon_id, Some(map_id));
        if dungeon != inner.current_dungeon_id {
            tracing::debug!("Map {map_id}: dungeon {} -> {dungeon}", inner.current_dungeon_id);
            inner.current_dungeon_id = dungeon;
        }
    }

    /// The dungeon a party roster names. Taken as the player's only when no
    /// map load says otherwise (`dungeon_of_map`).
    pub fn set_current_dungeon(&self, dungeon_id: i32) {
        if dungeon_id > 0 {
            let mut inner = self.inner.write();
            inner.roster_dungeon_id = dungeon_id;
            inner.current_dungeon_id = dungeon_of_map(dungeon_id, inner.current_map_id);
        }
    }

    pub fn current_dungeon_id(&self) -> i32 {
        self.inner.read().current_dungeon_id
    }

    pub fn get_party_members(&self) -> HashMap<String, PartyMember> {
        self.inner.read().party_members.clone()
    }

    /// The roster a saved fight takes its players' level, gear score, combat
    /// power and identity from: the party now, and those who have left it as
    /// it last had them. Only for what a fight record keeps; who is in the
    /// party is `get_party_members`.
    pub fn fight_roster(&self) -> HashMap<String, PartyMember> {
        let inner = self.inner.read();
        let mut roster = inner.former_party_members.clone();
        roster.extend(inner.party_members.iter().map(|(n, m)| (n.clone(), m.clone())));
        roster
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
        let mut inner = self.inner.write();
        inner.party_placeholders_hidden = true;
        inner.names_generation += 1;
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
        rebind_roster_after_naming(&mut inner, nickname);
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
        rebind_roster_after_naming(&mut inner, nickname);
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
        if hp == 1 {
            inner.hp_floored_ids.insert(id);
        } else if hp > 1
            && inner.hp_floored_ids.contains(&id)
            && !inner.dead_entity_ids.contains(&id)
        {
            inner.hp_reset_dummy_ids.insert(id);
        }
        let max = inner.mob_hp_data.entry(id).or_insert(0);
        if hp > *max {
            *max = hp;
        }
        // A field boss in the open world whose max HP is known from its spawn:
        // its HP now, for the sender's next pass (online builds only, as kills).
        #[cfg(feature = "online")]
        if let Some(max) = inner.mob_hp_data.get(&id).copied()
            && inner.current_dungeon_id <= 0
            && max > 0
            && inner.spawn_hp_ids.contains(&id)
        {
            if let Some(&code) = inner.mob_storage.get(&id) {
                if crate::capture::field_bosses::is_reported(code) {
                    let pct = ((hp as i64 * 100) / max as i64).clamp(0, 100) as u8;
                    let changed = match inner.boss_hp_now.get(&id) {
                        Some(&(_, before, at)) if before == pct => at,
                        _ => now_ms(),
                    };
                    inner.boss_hp_now.insert(id, (code, pct, changed));
                }
            }
        }
    }

    /// Whether `id` behaved as a training dummy does: its HP stopped at 1 and
    /// came back up, without it dying. Only for an entity whose NPC code is
    /// unknown is this the answer; one whose spawn was seen goes by the table.
    pub fn is_hp_reset_dummy(&self, id: i32) -> bool {
        self.inner.read().hp_reset_dummy_ids.contains(&id)
    }

    pub fn get_mob_current_hp(&self, id: i32) -> Option<i32> {
        self.inner.read().mob_current_hp.get(&id).copied()
    }

    /// Record a heal tick done by `actor_id` with `skill_code` (is_hot marks a HoT).
    /// Keyed by the healer so "healing done" can be shown per player. Self-heals count.
    pub fn append_heal(&self, actor_id: i32, skill_code: i32, amount: i64, is_hot: bool, timestamp: i64) {
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
        e.ticks.push((timestamp, amount));
        drop(inner);
        self.damage_generation.fetch_add(1, Ordering::Relaxed);
    }

    pub fn get_heal_snapshot(&self) -> HashMap<i32, HashMap<(i32, bool), HealSkillData>> {
        let inner = self.inner.read();
        inner
            .heal_storage
            .iter()
            .filter(|(id, _)| !is_mob(&inner, **id))
            .map(|(&id, skills)| (id, skills.clone()))
            .collect()
    }

    /// Per-actor heal totals within a fight window. Iterates ticks without
    /// cloning the vectors — much cheaper than `get_heal_snapshot`.
    ///
    /// Passive HP regen ("Restore HP", 1900001) is left out: everyone gets it,
    /// and it would inflate every player's HPS. It is still stored, as
    /// Details shows it on its own.
    pub fn get_heal_totals_in_window(&self, from_ms: i64, to_ms: i64) -> HashMap<i32, i64> {
        let inner = self.inner.read();
        let mut totals: HashMap<i32, i64> = HashMap::new();
        for (&actor_id, skills) in &inner.heal_storage {
            if is_mob(&inner, actor_id) {
                continue;
            }
            let mut actor_total: i64 = 0;
            for (&(code, _), skill_data) in skills {
                if code == PASSIVE_REGEN_SKILL {
                    continue;
                }
                for &(ts, amount) in &skill_data.ticks {
                    if ts >= from_ms && ts <= to_ms {
                        actor_total += amount;
                    }
                }
            }
            if actor_total > 0 {
                totals.insert(actor_id, actor_total);
            }
        }
        totals
    }

    /// The NPC code entity `id` spawned as, if it is a known mob.
    pub fn mob_code(&self, id: i32) -> Option<i32> {
        self.inner.read().mob_storage.get(&id).copied()
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

    /// Full details need this target's timeline, never other targets' hits.
    pub fn get_target_snapshot(&self, target_id: i32) -> Option<TargetCombatData> {
        self.inner.read().target_combat.get(&target_id).cloned()
    }

    /// Like `get_combat_snapshot` but without per-skill `hit_timestamps`.
    /// `hit_timestamps` grows unbounded over a fight and is only needed by
    /// `get_target_details`. The 500ms hot paths (`get_dps`,
    /// `get_details_context`, boss auto-save) never read it, so this keeps
    /// their per-tick clone cost flat over fight duration instead of growing
    /// linearly — the root cause of the long-fight FPS drops.
    pub fn get_combat_snapshot_light(&self) -> HashMap<i32, TargetCombatData> {
        self.inner.read().target_combat.iter()
            .map(|(&tid, td)| (tid, td.clone_light()))
            .collect()
    }

    /// Hover needs one target's aggregates, never the per-hit timeline or
    /// unrelated targets. Release the capture lock before resolving actors.
    pub fn get_target_snapshot_light(&self, target_id: i32) -> Option<TargetCombatData> {
        self.inner.read().target_combat.get(&target_id).map(TargetCombatData::clone_light)
    }

    /// Clear combat. Who owns which summon is kept: a summon is linked when it
    /// spawns or sends its owner a link record, and one that did that before the
    /// reset would otherwise stay unlinked for good. Ids that are reused get
    /// their links dropped by the new spawn (see `note_summon_spawn`).
    pub fn flush(&self) {
        self.stagger.lock().forget_pending();
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
        inner.hp_floored_ids.clear();
        inner.hp_reset_dummy_ids.clear();
        inner.heal_storage.clear();
        inner.current_target = 0;
    }

    /// Clear only the per-segment combat/damage aggregates, preserving player
    /// identity: nicknames, known-player ids, summon ownership, and job classes.
    /// Used on a zone-change / in-instance-teleport reset so the meter starts on
    /// clean numbers without dropping who your party and you are.
    pub fn flush_combat_only(&self) {
        self.stagger.lock().forget_pending();
        let mut inner = self.inner.write();
        inner.target_combat.clear();
        inner.held_dot_ticks.clear();
        inner.hostile_target_ids.clear();
        inner.dead_entity_ids.clear();
        inner.has_boss_in_segment = false;
        inner.mob_hp_data.clear();
        inner.mob_current_hp.clear();
        inner.hp_floored_ids.clear();
        inner.hp_reset_dummy_ids.clear();
        inner.heal_storage.clear();
        inner.current_target = 0;
    }

    /// Drop every summon link, for a capture from another session (a replay),
    /// whose entity ids mean something else.
    pub fn forget_summon_links(&self) {
        // The buffs those ids had on are as foreign as the links, and so
        // are their stagger gauges.
        self.forget_abnormals();
        self.stagger.lock().clear();
        let mut inner = self.inner.write();
        inner.summon_storage.clear();
        inner.confirmed_summon_ids.clear();
        inner.summon_spawn_ids.clear();
        inner.despawned_summon_ids.clear();
    }

    /// What the reset button and hotkey do to names: forget the ones only a
    /// loose scan guessed, keep the ones the game stated.
    ///
    /// The game names a player once, in the spawn (`44/45 36`) sent when they
    /// come into view, and you in the self record (`33 36`) on a zone load.
    /// Nothing repeats them while everyone stays put, so a reset that cleared
    /// every name left the party, the players around and the local player as
    /// `#id` rows until the next zone load. Replayed with three resets in
    /// town, a player's two-hour capture (2026-10-07, EU) went from 23 ticks
    /// of Boss mode with such a row to 2,326, her own row among them, and a
    /// nameless row of yours was not marked as yours, so it could fall off
    /// the bottom of the meter. A wrong name from a loose scan is still
    /// dropped here, which is what the reset is for.
    pub fn forget_guessed_nicknames(&self) {
        let mut inner = self.inner.write();
        let local = inner.local_player_id.map(|id| id as i32);
        let local_name = inner.local_character_name.clone();
        let Inner { nickname_storage, authoritative_name_ids, .. } = &mut *inner;
        nickname_storage.retain(|id, name| {
            authoritative_name_ids.contains(id)
                || (Some(*id) == local && local_name.as_deref().map(str::trim) == Some(name.trim()))
        });
        inner.pending_nicknames.clear();
        let permanent: Vec<(i32, String)> = inner.permanent_nicknames.iter().map(|(&k, v)| (k, v.clone())).collect();
        for (uid, nick) in permanent {
            inner.nickname_storage.insert(uid, nick);
        }
        inner.names_generation += 1;
    }

    /// Changes when someone joins or leaves the party, or it ends: the meter
    /// then drops the rows it kept from the last fight, which name the old
    /// party. Not with every roster; the game resends it on each map load.
    pub fn party_generation(&self) -> u64 {
        self.inner.read().party_generation
    }

    /// Counts of map loads: every one, and those entering or leaving an
    /// instance (a dungeon, entered again with "go again" too; anything but
    /// the open world to the open world). The meter clears the last fight
    /// off the screen on one, as its setting says.
    pub fn zone_generations(&self) -> (u64, u64) {
        let inner = self.inner.read();
        (inner.zone_generation, inner.instance_generation)
    }

    /// Changes whenever a name is bound, replaced or dropped, and with each
    /// party roster (the meter's rows for members who haven't fought).
    pub fn names_generation(&self) -> u64 {
        self.inner.read().names_generation
    }

    pub fn reset_nicknames(&self) {
        let mut inner = self.inner.write();
        inner.names_generation += 1;
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
    for special in pdp.specials() {
        match special {
            SpecialDamage::ShieldBlock => skill_data.shield_block_count += 1,
            SpecialDamage::Parry => skill_data.parry_count += 1,
            SpecialDamage::Perfect => skill_data.perfect_count += 1,
            SpecialDamage::Double => skill_data.double_count += 1,
            SpecialDamage::IronWall => skill_data.iron_wall_count += 1,
            SpecialDamage::Regeneration => skill_data.regeneration_count += 1,
            SpecialDamage::PerfectBlock => skill_data.perfect_block_count += 1,
            SpecialDamage::Back | SpecialDamage::Frontal | SpecialDamage::Critical => {}
        }
    }
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
    inner.despawned_summon_ids.remove(&id);
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
            e.ticks.extend(h.ticks);
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

/// The entry counts with Daily Duty (`DataStorage::content_counts`).
fn content_counts_of(inner: &Inner, now: i64) -> std::collections::BTreeMap<u32, [u32; 2]> {
    let mut counts = inner.content_counts.clone();
    if let Some((done, taken)) = daily_duty_of(inner, now) {
        counts.insert(DAILY_DUTY_ID, [done, taken]);
    }
    counts
}

/// Daily Duty at `now` (`DataStorage::daily_duty_at`).
fn daily_duty_of(inner: &Inner, now: i64) -> Option<(u32, u32)> {
    let board = inner.daily_duty.as_ref()?;
    if now >= board.reset_ms {
        return Some((0, 0));
    }
    let count = |f: fn(u8) -> bool| board.offers.iter().filter(|o| f(o.state)).count() as u32;
    Some((count(|s| s >= 4), count(|s| s != 1)))
}

/// The entry list at one moment (`DataStorage::entry_list`).
#[derive(Debug, Clone, Default)]
pub struct EntryList {
    pub owner: Option<String>,
    pub odyle_energy: Option<(u32, u32)>,
    pub counts: std::collections::BTreeMap<u32, [u32; 2]>,
    /// `(bought, next reset)` by item id (`DataStorage::shop_limits_at`).
    pub shop_limits: std::collections::BTreeMap<u32, (u32, Option<i64>)>,
    pub crafts: std::collections::BTreeMap<u32, u32>,
    /// As `DataStorage::kinah_today_at` and `abyss_points_at`.
    pub kinah_today: Option<u64>,
    pub abyss_points: Option<(u64, u64)>,
    /// `(time, seen at unix ms)` by id.
    pub times: std::collections::BTreeMap<u32, (u64, i64)>,
}

/// The shop's limits at `now` (`DataStorage::shop_limits_at`).
fn shop_limits_of(inner: &Inner, now: i64) -> std::collections::BTreeMap<u32, (u32, Option<i64>)> {
    inner
        .shop_limits
        .iter()
        .map(|(&id, &(bought, reset_ms))| (id, if now >= reset_ms { (0, None) } else { (bought, Some(reset_ms)) }))
        .collect()
}

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const WEEK_MS: i64 = 7 * DAY_MS;
/// The daily reset, 07:00 UTC, and the weekly, Wednesday 07:00 UTC (the
/// Daily Duty board's, the Supply Requests', the shop's weekly limits'), as
/// offsets from the epoch (a Thursday).
const DAILY_RESET_MS: i64 = 7 * 60 * 60 * 1000;
const WEEKLY_RESET_MS: i64 = 6 * DAY_MS + DAILY_RESET_MS;

/// The last reset at or before `now`, of a reset every `period` from
/// `offset` (all unix ms).
fn last_reset(now: i64, period: i64, offset: i64) -> i64 {
    (now - offset).div_euclid(period) * period + offset
}

/// Kinah gained today at `now` (`DataStorage::kinah_today_at`).
fn kinah_today_of(inner: &Inner, now: i64) -> Option<u64> {
    let (kinah, at) = inner.kinah_today?;
    Some(if last_reset(now, DAY_MS, DAILY_RESET_MS) > at { 0 } else { kinah })
}

/// The week's Abyss Points at `now` (`DataStorage::abyss_points_at`).
fn abyss_points_of(inner: &Inner, now: i64) -> Option<(u64, u64)> {
    let (points, limit, at) = inner.abyss_points?;
    Some((if last_reset(now, WEEK_MS, WEEKLY_RESET_MS) > at { 0 } else { points }, limit))
}

/// Whether a quest taken that isn't on the board looks like a new day's duty
/// offer: a quest from the same thousand as one of the board's. Every board
/// quest seen was 2302xxx in decimal (49 quests, 2302010 to 2302172, on the
/// boards of 2026-10-24 and -25).
fn looks_like_duty(offers: &[DutyOffer], taken: &DutyOffer) -> bool {
    offers.iter().any(|o| o.quest / 1000 == taken.quest / 1000)
}

/// Who the entry-count list is (`odyle_owner`): a whole list waits for the
/// next self record; anything else, in a session the game has named
/// already (a meter started mid-session), is theirs.
fn own_list(inner: &mut Inner, whole_list: bool) {
    if whole_list {
        inner.odyle_owner = None;
        inner.odyle_awaits_self_record = true;
    } else if inner.odyle_owner.is_none() && !inner.odyle_awaits_self_record && inner.local_identity_from_game {
        inner.odyle_owner = inner.local_character_name.clone();
    }
}

fn set_game_identity(inner: &mut Inner, id: i64, name: Option<String>) {
    inner.loot_identity.local_from_scope = false;
    inner.loot_identity.local_from_stats = false;
    inner.loot_identity.stats_streak = None;
    inner.local_identity_from_game = true;
    inner.local_player_id = Some(id);
    // Another character: the last one's self record is not this one's.
    let named = name.as_deref().map(str::trim).filter(|n| !n.is_empty());
    if named.is_some() && inner.self_server_owner.as_deref().is_some_and(|o| Some(o) != named) {
        inner.self_server = 0;
        inner.self_server_owner = None;
    }
    inner.local_character_name = name;
}

/// The player the `06 38` records name far more than anyone: at least 24
/// times, and four times as often as the next player. Only entities that
/// fought as players count; the records also name the mobs you hit.
///
/// On the captures at hand: the local player 2,421 times in 18 minutes at the
/// training dummies, with a dozen other players around and none of them
/// named once (2026-10-07, EU).
fn scope_leader(inner: &Inner) -> Option<i32> {
    const MIN_RECORDS: u32 = 24;
    const LEAD: u32 = 4;
    let mut best: Option<(i32, u32)> = None;
    let mut second = 0;
    for (&id, &n) in &inner.loot_identity.scope_counts {
        let player = inner.known_player_ids.contains(&id)
            && !inner.summon_storage.contains_key(&id)
            && !inner.summon_spawn_ids.contains(&id)
            && !inner.mob_storage.contains_key(&id);
        if !player {
            continue;
        }
        match best {
            Some((_, top)) if n <= top => second = second.max(n),
            _ => {
                if let Some((_, top)) = best {
                    second = second.max(top);
                }
                best = Some((id, n));
            }
        }
    }
    let (id, n) = best?;
    (n >= MIN_RECORDS && n >= LEAD * second.max(1)).then_some(id)
}

/// For diagnostics and tests: who `scope_leader` would pick now.
impl DataStorage {
    pub fn party_scope_leader(&self) -> Option<i32> {
        scope_leader(&self.inner.read())
    }
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
        inner.names_generation += 1;
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
    inner.names_generation += 1;

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
/// When a party member has just been named, match the rest of the roster at
/// once. Naming one of two Spiritmasters settles the other by elimination,
/// but the match ran only every 64 damage records of the current fight, so
/// between pulls the other waited: 30 and 105 seconds in a 2026-10-02 run
/// with two Gladiators and two Spiritmasters, the second being the player.
fn rebind_roster_after_naming(inner: &mut Inner, nickname: &str) {
    if inner.party_members.contains_key(nickname.trim()) {
        bind_roster_names_by_class(inner);
    }
}

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

/// Where healing or damage taken by `actor` counts: neither has a target of
/// its own. The fight against `mob` when the actor is in it, else the target
/// the actor hit last (ties to the lowest id), so it is never left to the
/// map's order.
fn fight_of(inner: &mut Inner, actor: i32, mob: Option<i32>) -> Option<&mut ActorCombatData> {
    let fights = |tid: &i32| inner.target_combat.get(tid).is_some_and(|td| td.actors.contains_key(&actor));
    let tid = mob.filter(fights).or_else(|| {
        inner
            .target_combat
            .iter()
            .filter_map(|(&tid, td)| td.actors.get(&actor).map(|a| (a.last_damage_time, std::cmp::Reverse(tid))))
            .max()
            .map(|(_, std::cmp::Reverse(tid))| tid)
    })?;
    inner.target_combat.get_mut(&tid)?.actors.get_mut(&actor)
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

/// A mob is no healer. Protection Circle HoT ticks on a player carry a mob
/// in the healer field (the boss, 2026-10-05), and saved boss fights listed
/// the boss as a healer. A summon spawns like a mob, so a linked one is kept;
/// so is an id the game named as a player since.
fn is_mob(inner: &Inner, id: i32) -> bool {
    inner.mob_storage.contains_key(&id)
        && !inner.summon_storage.contains_key(&id)
        && !inner.known_player_ids.contains(&id)
        && !inner.nickname_storage.contains_key(&id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn your_own_and_a_known_players_hp_log_deaths_and_a_strangers_does_not() {
        let storage = DataStorage::new();
        storage.set_local_player_id(Some(2695));
        storage.append_nickname_authoritative(14106, "Faffy");
        crate::clock::set_override(Some(1_000));
        for id in [2695, 14106, 777] { storage.note_player_hp(id, 9_000); }
        crate::clock::set_override(Some(2_000));
        for id in [2695, 14106, 777] { storage.note_player_hp(id, 0); }
        crate::clock::set_override(Some(42_000));
        storage.note_player_hp(2695, 12_419); // you are revived 40 s later
        crate::clock::set_override(None);
        assert_eq!(storage.deaths_between(0, 50_000), vec![(2695, 2_000, Some(42_000)), (14106, 2_000, None)]);
    }

    fn who(s: &DataStorage) -> (Option<i64>, Option<String>, bool) {
        (s.local_player_id(), s.local_character_name(), s.local_identity_from_game())
    }

    #[test]
    fn naming_one_of_two_same_class_members_names_the_other_at_once() {
        let s = DataStorage::new();
        let sm = |slot| PartyMember { slot, job: Some(JobClass::Elementalist), ..Default::default() };
        s.set_party_roster(vec![("Thermi".into(), sm(1)), ("Nyxie".into(), sm(2))], true);
        // Two unnamed Spiritmasters, each running a rotation.
        for (actor, base) in [(1792, 16_010_000), (13520, 16_010_000)] {
            for i in 0..4 {
                let mut p = ParsedDamagePacket::new();
                p.set_actor_id(actor);
                p.set_target_id(900);
                p.set_skill_code(base + i * 10_000);
                p.set_damage(100);
                s.append_damage(p);
            }
        }
        assert!(s.get_nickname(13520).is_none(), "two of a class: the roster cannot tell");
        s.append_nickname_authoritative(1792, "Thermi");
        assert_eq!(s.get_nickname(13520).as_deref(), Some("Nyxie"), "the other one, by elimination");
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

    /// A meter started mid-game: the fights go under the server learnt for
    /// the connection, ahead of a party's majority; the game's own word
    /// still wins once it comes (Harvest Manager Moshav, 2026-10-18).
    #[test]
    fn fights_take_the_connection_server_until_the_game_says() {
        let s = DataStorage::new();
        let on = |server: u16| PartyMember { server_id: server, ..member(1) };
        s.set_party_roster(vec![("A".into(), on(2304)), ("B".into(), on(2304)), ("C".into(), on(1307))], true);
        assert_eq!(s.fight_server_id(), 2304, "the party's majority, as before");
        s.note_host_server(1304);
        assert_eq!(s.fight_server_id(), 1304, "the connection's server over the majority");
        assert_eq!(s.own_server(), 0, "never taken as the player's own");
        s.note_self_server(1305);
        assert_eq!(s.fight_server_id(), 1305, "the game's own word wins");
    }

    /// Another party's roster (the finder's, or the leader's next party) is
    /// not word that you left yours: it changes nothing. Your own party's
    /// roster without you still ends it (2026-10-23).
    #[test]
    fn another_partys_roster_leaves_yours_alone() {
        let s = DataStorage::new();
        s.set_local_character_name(Some("Me".into()));
        let ours = || vec![("Lead".into(), member(1)), ("Me".into(), member(2)), ("Tank".into(), member(3))];
        let theirs = vec![("Lead".into(), member(1)), ("Other".into(), member(2))];
        assert!(!s.is_other_party(8, &theirs, true), "no party of yours known yet: as before");
        assert!(!s.is_other_party(7, &ours(), true));
        s.set_party_roster_of(Some(7), ours(), true);
        assert!(s.is_other_party(8, &theirs, true), "a whole roster without you, of another key");
        assert!(!s.is_other_party(8, &theirs, false), "a partial one may have stopped before you");
        let gone = vec![("Lead".into(), member(1)), ("Tank".into(), member(3))];
        assert!(!s.is_other_party(7, &gone, true), "your own party without you");
        s.set_party_roster_of(Some(7), gone, true);
        assert!(s.get_party_members().is_empty(), "you left it");
    }

    /// Members leaving after the boss falls (Vakron, 2026-10-17) keep their
    /// combat power in the fight's later saves, but are no longer the party.
    #[test]
    fn a_fight_keeps_the_stats_of_members_who_have_left() {
        let s = DataStorage::new();
        let three = || vec![("Nyxera".into(), member(1)), ("Alfira".into(), member(2)), ("Sherhana".into(), member(3))];
        s.set_party_roster(three(), true);
        s.set_party_roster(vec![("Nyxera".into(), member(1)), ("Sherhana".into(), member(3))], true);
        assert!(!s.get_party_members().contains_key("Alfira"));
        assert_eq!(s.fight_roster().get("Alfira").map(|m| m.combat_power), Some(39_000));

        // The roster now wins over what a member had before they left.
        s.set_party_roster(vec![("Nyxera".into(), member(1)), ("Sherhana".into(), member(3)), ("Alfira".into(), PartyMember { combat_power: 41_000, ..member(2) })], true);
        assert_eq!(s.fight_roster().get("Alfira").map(|m| m.combat_power), Some(41_000));

        // The party ending keeps them too.
        s.set_party_roster(vec![("Nyxera".into(), member(1))], true);
        assert!(s.get_party_members().is_empty());
        assert_eq!(s.fight_roster().len(), 3);
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
    fn instances_entered_and_left_are_counted_apart_from_open_world_moves() {
        let s = DataStorage::new();
        let open = *OPEN_WORLD_MAPS.iter().next().unwrap();
        s.note_map_load(open);
        let (_, at_start) = s.zone_generations();
        s.note_map_load(open);
        assert_eq!(s.zone_generations().1, at_start, "a teleport in the open world");
        s.note_map_load(610_001);
        assert_eq!(s.zone_generations().1, at_start + 1, "into a dungeon");
        s.note_map_load(610_001);
        assert_eq!(s.zone_generations().1, at_start + 2, "go again: the same dungeon loaded again");
        s.note_map_load(open);
        assert_eq!(s.zone_generations(), (5, at_start + 3), "back to the open world");
    }

    #[test]
    fn a_fight_saved_while_the_game_moves_you_stays_on_your_server() {
        // A cross-server party: you on 2014, two members on 1014. The game
        // moves you (a server change after the boss), own-stats records name
        // a new entity, which drops the local name until the next self record.
        let s = DataStorage::new();
        let on = |server: u16| PartyMember { server_id: server, ..member(1) };
        s.set_party_roster(
            vec![("Me".into(), on(2014)), ("P1".into(), on(1014)), ("P2".into(), on(1014)), ("P3".into(), on(1013))],
            true,
        );
        s.set_local_identity_from_game(4099, Some("Me".into()));
        s.note_player_server("Me", 2014);
        s.note_self_server(2014);
        assert_eq!(s.fight_server_id(), 2014);
        for _ in 0..3 {
            s.note_self_stats(5120);
        }
        assert_eq!(s.local_character_name(), None, "the move drops the name");
        assert_eq!(s.fight_server_id(), 2014, "still your server, not the party's majority");
        s.note_self_server(99);
        assert_eq!(s.fight_server_id(), 2014, "a value no server has is not taken");
        s.note_self_server(0);
        assert_eq!(s.fight_server_id(), 1014, "a tutorial character's record forgets it");
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

    /// Bodyguard Teegant (NPC 2100079), a Verteron field boss.
    #[cfg(feature = "online")]
    #[test]
    fn a_field_boss_kill_in_the_open_world_is_queued_with_its_time() {
        let s = DataStorage::new();
        s.append_mob(500, 2100079);
        s.mark_entity_dead(500);
        s.mark_entity_dead(500); // the same death again: once
        let events = s.take_boss_events();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], BossEvent { npc: 2100079, kind: BossEventKind::Killed { .. } }));
        // Not a field boss, or inside a dungeon: nothing.
        s.append_mob(501, 2100080);
        s.mark_entity_dead(501);
        s.set_current_dungeon(600012);
        s.append_mob(502, 2100079);
        s.mark_entity_dead(502);
        assert!(s.take_boss_events().is_empty());
    }

    #[cfg(feature = "online")]
    #[test]
    fn a_fought_boss_goes_every_pass_and_a_reset_is_marked() {
        let s = DataStorage::new();
        s.append_mob(500, 2100079);
        s.append_mob_hp(500, 1000);
        s.set_mob_current_hp(500, 950);
        let t = now_ms();
        let pct = |v: Vec<BossHp>| v.into_iter().map(|h| (h.percent, h.reset)).collect::<Vec<_>>();
        assert_eq!(pct(s.take_boss_hp(t)), vec![(95, false)]);
        // Still being fought but unchanged: not again until a minute has
        // passed since it went (the site uses a repeat once a minute).
        assert!(s.take_boss_hp(t + 3_000).is_empty());
        // Left alone: nothing once the fight has gone quiet.
        assert!(s.take_boss_hp(t + 60_000).is_empty());
        s.set_mob_current_hp(500, 400);
        assert_eq!(pct(s.take_boss_hp(now_ms())), vec![(40, false)]);
        // Kited away and reset: back at full, marked.
        s.set_mob_current_hp(500, 1000);
        assert_eq!(pct(s.take_boss_hp(now_ms())), vec![(100, true)]);
        // Dead: no more HP for it.
        s.mark_entity_dead(500);
        assert!(s.take_boss_hp(now_ms()).is_empty());
        // Its spawn unseen: its max is a guess, so no percentages.
        s.append_mob(501, 2100079);
        s.set_mob_current_hp(501, 800);
        assert!(s.take_boss_hp(now_ms()).is_empty());
    }

    #[test]
    fn damage_taken_and_party_heal_land_on_the_fight_they_belong_to() {
        let s = DataStorage::new();
        // You and a party member, on fifty mobs; mob 830 last.
        for (i, t) in (800..850).filter(|&t| t != 830).chain([830]).enumerate() {
            s.append_mob(t, 1);
            s.append_damage(hit(100, t, 1_000 + i as i64, 500, false));
            s.append_damage(hit(200, t, 1_000 + i as i64, 500, false));
        }
        let taken = |s: &DataStorage, t: i32| s.get_combat_snapshot_light()[&t].actors[&100].damage_received;
        let before_hit = s.damage_generation();
        s.append_damage(with_skill(hit(810, 100, 3_000, 300, false), 1_200_001));
        assert_eq!(taken(&s, 810), 300, "on the fight with the mob that hit you");
        assert!(s.damage_generation() > before_hit, "incoming damage refreshes the live meter");

        // A mob you never hit: the fight you were in last.
        s.append_mob(900, 1);
        s.append_damage(with_skill(hit(900, 100, 3_100, 70, false), 1_200_001));
        assert_eq!(taken(&s, 830), 70);

        s.append_damage(hit(200, 100, 3_200, 400, false));
        let snapshot = s.get_combat_snapshot_light();
        assert_eq!(snapshot[&830].actors[&200].party_heal, 400);
        assert_eq!(snapshot.values().map(|t| t.actors[&200].party_heal).sum::<i64>(), 400);
    }

    #[test]
    fn only_players_and_their_summons_heal() {
        let s = DataStorage::new();
        s.append_mob(22809, 2310171);
        s.append_mob(500, 1);
        s.append_summon(14409, 500);
        s.append_nickname_authoritative(14274, "Templar");
        for (actor, skill) in [(22809, 18_730_003), (500, 16_770_000), (14274, 18_730_003), (14409, 2_011_101)] {
            s.append_heal(actor, skill, 100, true, 0);
        }
        let mut healers: Vec<i32> = s.get_heal_snapshot().into_keys().collect();
        healers.sort();
        assert_eq!(healers, vec![500, 14274, 14409], "the boss is not one");
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

    /// A dummy is one whose HP came back up from 1; a mob that died on the
    /// way, or one whose spawn said what it is, is not.
    #[test]
    fn hp_back_up_from_the_floor_marks_a_dummy() {
        let s = DataStorage::new();
        for (id, hp) in [(500, 40_000), (500, 1), (500, 119_700)] {
            s.set_mob_current_hp(id, hp);
        }
        assert!(s.is_hp_reset_dummy(500));

        // Died at the floor; the id later reused by something at full HP.
        s.set_mob_current_hp(501, 1);
        s.mark_entity_dead(501);
        s.set_mob_current_hp(501, 90_000);
        assert!(!s.is_hp_reset_dummy(501));

        // Never at 1: a heal or a phase is not a dummy.
        for hp in [50_000, 20_000, 60_000] {
            s.set_mob_current_hp(502, hp);
        }
        assert!(!s.is_hp_reset_dummy(502));

        // A spawn names the NPC, and the table decides from then on.
        s.append_mob(500, 2_300_401);
        assert!(!s.is_hp_reset_dummy(500));

        s.set_mob_current_hp(503, 1);
        s.set_mob_current_hp(503, 2);
        assert!(s.is_hp_reset_dummy(503));
        s.flush_combat_only();
        assert!(!s.is_hp_reset_dummy(503), "a zone change: ids may name other entities now");
    }

    fn player_hit(s: &DataStorage, actor: i32, target: i32) {
        let mut p = ParsedDamagePacket::new();
        p.set_actor_id(actor);
        p.set_target_id(target);
        p.set_skill_code(13_720_000);
        p.set_damage(100);
        s.append_damage(p);
    }

    /// The reset button forgets damage, and names only a loose scan guessed.
    /// The ones the game stated (spawns, the self record) are not sent again
    /// until everyone respawns, so dropping them left every row an `#id`.
    #[test]
    fn a_reset_keeps_the_names_the_game_stated() {
        let s = DataStorage::new();
        s.set_local_identity_from_game(2737, Some("Mine".into()));
        s.append_nickname_authoritative(2737, "Mine");
        s.append_nickname_authoritative(4227, "Spawned");
        player_hit(&s, 5001, 900);
        s.append_nickname(5001, "Guessed");
        s.set_permanent_nickname(6001, "Typed");
        let generation = s.names_generation();

        s.flush();
        s.forget_guessed_nicknames();
        assert_eq!(s.get_nickname(2737).as_deref(), Some("Mine"));
        assert_eq!(s.get_nickname(4227).as_deref(), Some("Spawned"));
        assert_eq!(s.get_nickname(6001).as_deref(), Some("Typed"));
        assert_eq!(s.get_nickname(5001), None, "a guess goes");
        assert_ne!(s.names_generation(), generation);

        // A local name from a loot record (no self record) is yours, and stays.
        let loot = DataStorage::new();
        loot.note_party_scope(1454);
        player_hit(&loot, 1454, 900);
        loot.append_nickname(1454, "Looter"); // as the parser does with a loot record
        assert!(loot.note_loot_owner(900, 1454, "Looter"));
        loot.forget_guessed_nicknames();
        assert_eq!(loot.get_nickname(1454).as_deref(), Some("Looter"));

        // Loading a replay still starts from nothing.
        s.reset_nicknames();
        assert_eq!(s.get_nickname(4227), None);
    }

    fn own_stats(s: &DataStorage, id: i32, times: usize) {
        for _ in 0..times {
            s.note_self_stats(id);
        }
    }

    /// A zone load whose self record was missed left the meter on the last
    /// zone's entity (2026-10-09: two and a half minutes, a boss fight saved
    /// with your row a masked `#id`). The `4A 36` records about your own stats
    /// name the new entity from the first seconds.
    #[test]
    fn own_stats_records_follow_you_past_a_missed_self_record() {
        let s = DataStorage::new();
        s.set_local_identity_from_game(4294, Some("Mine".into()));
        s.append_nickname_authoritative(4294, "Mine");
        player_hit(&s, 7001, 900);
        player_hit(&s, 8123, 900); // a party member

        own_stats(&s, 7001, 2);
        s.note_self_stats(4294); // still about the id in force: no streak
        own_stats(&s, 7001, 2);
        assert_eq!(s.local_player_id(), Some(4294), "not on two in a row");
        assert!(s.note_self_stats(7001), "three in a row");
        assert_eq!(s.local_player_id(), Some(7001));
        assert!(s.local_id_from_scope(), "an id without a name, like the 06 38 one");
        assert!(!s.local_identity_from_game());
        assert_eq!(s.local_character_name(), None, "the last character's name is not assumed");
        assert_eq!(s.get_nickname(7001), None);
        assert_eq!(s.get_nickname(4294).as_deref(), Some("Mine"), "the old entity keeps its name");

        // Neither a party member's 06 38 lead nor their loot moves you off it.
        scope(&s, 8123, 200);
        s.note_party_scope(7001);
        assert!(!s.note_loot_owner(900, 8123, "Partner"));
        assert_eq!(s.local_player_id(), Some(7001));
        // Your own loot names you, once it leads.
        s.note_loot_owner(901, 7001, "Mine");
        assert!(s.note_loot_owner(902, 7001, "Mine"));
        assert_eq!((s.local_player_id(), s.local_character_name().as_deref()), (Some(7001), Some("Mine")));

        // And the next self record has the last word.
        s.set_local_identity_from_game(7001, Some("Mine".into()));
        assert!(!s.local_id_from_scope());
        assert!(s.local_identity_from_self_record());
    }

    /// The fight of 2026-10-09: the self record named you on a new entity at
    /// a zone load, and the fight after it was saved with that entity
    /// nameless, masked and not you. Binding the id a window still showed
    /// from before the load does exactly that: the name goes to the old id.
    #[test]
    fn an_old_id_echoed_by_a_window_does_not_take_you_off_your_entity() {
        let s = DataStorage::new();
        s.set_local_identity_from_game(5100, Some("Mine".into()));
        s.append_nickname_authoritative(5100, "Mine");
        // Zone load: a new entity, named by the self record.
        s.set_local_identity_from_game(6200, Some("Mine".into()));
        s.append_nickname_authoritative(6200, "Mine");
        assert_eq!(s.get_nickname(5100), None);

        assert!(!s.ui_may_bind_local_id(5100, false), "an echo of the old id");
        assert!(s.ui_may_bind_local_id(6200, false));
        assert!(s.ui_may_bind_local_id(5100, true), "a typed id is the player's call");

        // What binding it did (bind_local_actor_id before this check)...
        s.set_local_player_id(Some(5100));
        s.set_permanent_nickname(5100, "Mine");
        assert_eq!(s.get_nickname(6200), None, "the failure: your entity loses its name");
        // ...is undone by your own-stats records, which keep naming 6200.
        own_stats(&s, 6200, 3);
        assert_eq!(s.local_player_id(), Some(6200));
    }

    fn scope(s: &DataStorage, id: i32, times: usize) {
        for _ in 0..times {
            s.note_party_scope(id);
        }
    }

    /// A meter opened mid-session knows who you are from the `06 38` records
    /// long before the next zone load or kill: they name you several times a
    /// second, and the players around you not at all.
    #[test]
    fn party_scope_records_name_you_until_the_game_does() {
        let s = DataStorage::new();
        s.set_local_character_name(Some("Remembered".into()));
        player_hit(&s, 2737, 25_839);
        player_hit(&s, 4227, 25_839);
        s.append_mob(25_839, 2_000_001);
        scope(&s, 25_839, 200); // the dummy you hit: not a player
        scope(&s, 2737, 23);
        assert_eq!(s.local_player_id(), None, "not on a handful of records");
        scope(&s, 2737, 1);
        assert_eq!(s.local_player_id(), Some(2737));
        assert!(s.local_id_from_scope());
        assert!(!s.local_identity_from_game());
        assert_eq!(s.get_nickname(2737), None, "no name comes with it");
        assert_eq!(s.local_character_name().as_deref(), Some("Remembered"));

        // The self record has the last word, and the counts no longer matter.
        s.set_local_identity_from_game(12_870, Some("Mine".into()));
        assert!(!s.local_id_from_scope());
        scope(&s, 2737, 200);
        assert_eq!(s.local_player_id(), Some(12_870));
    }

    #[test]
    fn party_scope_records_decide_nothing_without_a_clear_lead() {
        // Older Korean/Taiwanese captures name party members too.
        let s = DataStorage::new();
        player_hit(&s, 101, 900);
        player_hit(&s, 202, 900);
        for _ in 0..40 {
            scope(&s, 101, 3);
            scope(&s, 202, 1);
        }
        assert_eq!(s.local_player_id(), None, "three to one is no lead");
        scope(&s, 101, 40);
        assert_eq!(s.local_player_id(), Some(101), "four to one is");

        // A zone load hands out new ids: the old counts go with them.
        let s = DataStorage::new();
        player_hit(&s, 101, 900);
        scope(&s, 101, 16);
        s.note_map_load(1);
        scope(&s, 101, 16);
        assert_eq!(s.local_player_id(), None);

        // An id chosen in the UI is not overridden.
        let s = DataStorage::new();
        s.set_local_player_id(Some(303));
        player_hit(&s, 101, 900);
        scope(&s, 101, 200);
        assert_eq!(s.local_player_id(), Some(303));
    }

    /// The roster names the party's dungeon, not where the player is
    /// (`dungeon_of_map`). Map ids from captures: 1011 World_L_A layer,
    /// 610073 and 600072 instances, 151007 a non-dungeon instance.
    #[test]
    fn a_stale_roster_does_not_file_open_world_fights_under_the_last_dungeon() {
        // Meter opened mid-session: the roster is all there is.
        let s = DataStorage::new();
        s.set_current_dungeon(600072);
        assert_eq!(s.current_dungeon_id(), 600072);

        // Leave Vakron Sky Island: the load into the open world ends it, and
        // the roster updates the party sends afterwards do not restore it.
        s.note_map_load(1011);
        assert_eq!(s.current_dungeon_id(), 0);
        s.set_current_dungeon(600072);
        assert_eq!(s.current_dungeon_id(), 0, "roster in the open world");

        // Another instance that is no dungeon: none, whatever the roster says.
        s.note_map_load(151007);
        s.set_current_dungeon(600072);
        assert_eq!(s.current_dungeon_id(), 0);

        // A dungeon the roster does not name: the map's.
        s.note_map_load(600011);
        assert_eq!(s.current_dungeon_id(), 600011);
        s.set_current_dungeon(600072);
        assert_eq!(s.current_dungeon_id(), 600011, "the map wins over the roster");

        // An instance outside the dungeon table's range that the roster names.
        s.set_current_dungeon(710001);
        s.note_map_load(710001);
        assert_eq!(s.current_dungeon_id(), 710001);

        // Queued for a dungeon while in the open world, then loaded into it.
        let s = DataStorage::new();
        s.note_map_load(1010);
        s.set_current_dungeon(610073);
        assert_eq!(s.current_dungeon_id(), 0, "queued, not in it yet");
        s.note_map_load(610073);
        assert_eq!(s.current_dungeon_id(), 610073);
        s.note_map_load(610073);
        assert_eq!(s.current_dungeon_id(), 610073, "a teleport inside keeps it");
    }

    /// A "no longer in a party" record with no party to clear (no roster,
    /// nobody but you on the meter) leaves your fight alone; with another
    /// player on the meter (an open-world party has no roster), it clears.
    #[test]
    fn leaving_a_party_clears_only_when_there_was_one() {
        let s = DataStorage::new();
        s.note_party_left();
        assert!(!s.take_combat_reset_requested(), "an empty meter");

        s.set_local_player_id(Some(101));
        s.append_nickname_authoritative(101, "Nullception");
        s.append_damage(hit(101, 900, 1_000, 500, false));
        s.register_confirmed_summon_by_id(5_000, 101);
        s.append_damage(hit(5_000, 900, 1_500, 200, false));
        s.note_party_left();
        assert!(!s.take_combat_reset_requested(), "only you, and your summon");
        assert!(s.get_combat_snapshot()[&900].total_damage > 0, "your fight is kept");

        s.append_nickname_authoritative(202, "JinSoul");
        s.append_damage(hit(202, 900, 2_000, 300, false));
        s.note_party_left();
        assert!(s.take_combat_reset_requested(), "an open-world party: someone else on the meter");
    }
    #[test]
    fn a_moved_character_is_filed_under_the_server_its_records_state() {
        // An Israphel player whose id was made on Nemon (#128).
        let s = DataStorage::new();
        s.set_local_identity_from_game(7, Some("Arid".into()));
        // No self record yet: you are on no known server, not a guessed one.
        s.note_player_server("Arid", 2109);
        assert_eq!(s.stated_server("Arid"), Some(0));
        assert_eq!(s.self_record_server(), 0);
        s.note_self_server(2101);
        // A later loot record must not move you back.
        s.note_player_server("Arid", 2109);
        assert_eq!(s.stated_server("Arid"), Some(2101));
        assert_eq!(s.self_record_server(), 2101);
        // Someone else: what a loot record stated, else nothing (the roster's).
        s.note_player_server("Other", 1102);
        assert_eq!(s.stated_server("Other"), Some(1102));
        assert_eq!(s.stated_server("Nobody"), None);
        // Live profiles and reports too, not only saved fights.
        assert_eq!(s.own_server(), 2101);
    }

    #[test]
    fn a_switch_named_by_loot_does_not_inherit_the_last_server() {
        let s = DataStorage::new();
        s.set_local_identity_from_game(7, Some("Arid".into()));
        s.note_self_server(2101);
        // A zone move drops the name; the same character keeps its server.
        s.set_local_identity_from_game(8, Some("Arid".into()));
        assert_eq!(s.self_record_server(), 2101);
        // Another character, named by loot before its own self record.
        s.set_local_identity_from_game(9, Some("Alt".into()));
        assert_eq!(s.self_record_server(), 0);
        assert_eq!(s.stated_server("Alt"), Some(0));
        s.note_self_server(1102);
        assert_eq!(s.own_server(), 1102);
    }

}

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::combat::data_storage::{DataStorage, TargetCombatData};
use crate::combat::ping_tracker::PingTracker;
use crate::entity::details_context::*;
use crate::entity::dps_data::DpsData;
use crate::entity::fight_record::FightRecord;
use crate::entity::job_class::JobClass;
use crate::entity::personal_data::PersonalData;
use crate::entity::summon_resolver;
use crate::i18n::lookup::{NpcLookup, SkillLookup};

/// Synthetic row ids for party members whose entity id we do not know yet. Sits
/// above the entity-id range (real ids top out at 9,999,999) so it can never
/// collide, and stays positive because the frontend discards non-positive ids.
const PARTY_ROW_ID_BASE: i32 = 90_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetSelectionMode {
    BossTargets,
    MostDamage,
    MostRecent,
    LastHitByMe,
    AllTargets,
    TrainTargets,
}

impl TargetSelectionMode {
    pub fn from_id(id: &str) -> Self {
        match id {
            "bossTargets" => Self::BossTargets,
            "mostDamage" => Self::MostDamage,
            "mostRecent" => Self::MostRecent,
            "lastHitByMe" => Self::LastHitByMe,
            "allTargets" => Self::AllTargets,
            "trainTargets" => Self::TrainTargets,
            _ => Self::LastHitByMe,
        }
    }

    pub fn id(&self) -> &'static str {
        match self {
            Self::BossTargets => "bossTargets",
            Self::MostDamage => "mostDamage",
            Self::MostRecent => "mostRecent",
            Self::LastHitByMe => "lastHitByMe",
            Self::AllTargets => "allTargets",
            Self::TrainTargets => "trainTargets",
        }
    }
}

pub struct DpsCalculator {
    data_storage: Arc<DataStorage>,
    skill_lookup: Arc<SkillLookup>,
    npc_lookup: Arc<NpcLookup>,
    ping_tracker: Arc<PingTracker>,
    current_target: i32,
    last_dps_snapshot: Option<DpsData>,
    last_damage_gen: i64,
    target_selection_mode: TargetSelectionMode,
    last_known_local_id: Option<i64>,
    all_targets_window_ms: i64,
    nickname_job_cache: HashMap<String, String>,
    /// Boss targets saved as ended, with the time of their last hit then. A
    /// target hit again after that is saved again, so a fight with a long
    /// pause keeps its second half.
    saved_boss_targets: HashMap<i32, i64>,
}

impl DpsCalculator {
    pub fn new(
        data_storage: Arc<DataStorage>,
        skill_lookup: Arc<SkillLookup>,
        npc_lookup: Arc<NpcLookup>,
        ping_tracker: Arc<PingTracker>,
    ) -> Self {
        Self {
            data_storage,
            skill_lookup,
            npc_lookup,
            ping_tracker,
            current_target: 0,
            last_dps_snapshot: None,
            last_damage_gen: -1,
            target_selection_mode: TargetSelectionMode::BossTargets,
            last_known_local_id: None,
            all_targets_window_ms: 120_000,
            nickname_job_cache: HashMap::new(),
            saved_boss_targets: HashMap::new(),
        }
    }

    pub fn set_target_selection_mode(&mut self, id: &str) {
        let mode = TargetSelectionMode::from_id(id);
        if mode != self.target_selection_mode {
            // Recompute on the next update even without new damage: the
            // cached result still names the old mode and its target, so the
            // meter went on as if nothing had changed until someone hit
            // something.
            self.last_damage_gen = -1;
        }
        self.target_selection_mode = mode;
    }

    pub fn set_all_targets_window_ms(&mut self, ms: i64) {
        self.all_targets_window_ms = ms.clamp(10_000, 900_000);
    }

    pub fn mark_all_targets_saved(&mut self) {
        let combat = self.data_storage.get_combat_snapshot_light();
        for (&tid, td) in &combat {
            self.saved_boss_targets.insert(tid, td.last_damage_time);
        }
    }

    pub fn restart_target_selection(&mut self, clear_damage: bool) {
        self.current_target = 0;
        self.last_dps_snapshot = None;
        self.saved_boss_targets.clear();
        self.last_damage_gen = -1;
        if clear_damage {
            self.data_storage.flush();
        }
        self.data_storage.set_current_target(0);
    }

    pub fn get_dps(&mut self) -> DpsData {
        // A zone change flushed combat data; drop our cached snapshot and saved-target
        // state so the meter resets this cycle instead of returning the stale snapshot.
        if self.data_storage.take_combat_reset_requested() {
            self.last_dps_snapshot = None;
            self.saved_boss_targets.clear();
            self.last_damage_gen = -1;
            self.current_target = 0;
            self.data_storage.set_current_target(0);
        }

        let current_local_id = self.data_storage.local_player_id();
        if current_local_id != self.last_known_local_id {
            // The local player can churn entity ids several times per fight, so
            // this fires repeatedly. Only invalidate the snapshot so the "you"
            // highlight follows the new id — do NOT restart target selection,
            // which would drop the current target/segment on every change.
            self.last_known_local_id = current_local_id;
            self.last_damage_gen = -1;
        }

        // If no new damage since last cycle, return cached result
        let current_gen = self.data_storage.damage_generation();
        if current_gen == self.last_damage_gen && self.last_dps_snapshot.is_some() {
            return self.last_dps_snapshot.as_ref().unwrap().clone();
        }
        self.last_damage_gen = current_gen;

        // Get pre-computed aggregates (cheap — small map, not 17K packets).
        // Light snapshot: skips per-hit timestamps (unused here, grows unbounded).
        let combat_data = self.data_storage.get_combat_snapshot_light();
        let nickname_data = self.data_storage.get_nicknames();
        let summon_data = self.data_storage.get_summon_data();

        let mut dps_data = DpsData::new();
        dps_data.local_player_id = current_local_id;
        dps_data.dungeon_id = self.data_storage.current_dungeon_id();

        // Decide target
        let (target_ids, target_name, tracking_id) = self.decide_target(&combat_data, &nickname_data, &summon_data);
        dps_data.target_name = target_name;
        dps_data.target_mode = self.target_selection_mode.id().to_string();
        self.current_target = tracking_id;
        dps_data.target_id = self.current_target;
        dps_data.detail_target_ids = target_ids.iter().copied().collect();
        dps_data.detail_target_ids.sort_unstable();
        self.data_storage.set_current_target(self.current_target);

        // Boss HP bar source: spawn-time max HP of the single boss target. Only
        // meaningful for a single target (multi-target HP can't be summed sanely),
        // so leave it 0 otherwise and let the frontend hide the bar.
        let target_max_hp = if self.current_target != 0 {
            self.data_storage.get_mob_hp(self.current_target).unwrap_or(0) as i64
        } else {
            0
        };
        dps_data.target_max_hp = target_max_hp;

        // Real current HP from the live feed (-1 if none seen). Preferred over the
        // derived max-minus-damage bar when available.
        let target_current_hp = if self.current_target != 0 {
            self.data_storage
                .get_mob_current_hp(self.current_target)
                .map(|h| h as i64)
                .unwrap_or(-1)
        } else {
            -1
        };
        dps_data.target_current_hp = target_current_hp;

        // Collect actors from selected targets
        let mut combined_actors: HashMap<i32, i64> = HashMap::new();
        let mut combined_jobs: HashMap<i32, Option<JobClass>> = HashMap::new();
        for &tid in &target_ids {
            if let Some(target_data) = combat_data.get(&tid) {
                for (&actor_id, actor_data) in &target_data.actors {
                    *combined_actors.entry(actor_id).or_insert(0) += actor_data.total_damage;
                    if actor_data.job.is_some() && combined_jobs.get(&actor_id).and_then(|j| j.as_ref()).is_none() {
                        combined_jobs.insert(actor_id, actor_data.job);
                    }
                }
            }
        }

        // Calculate battle time
        let battle_time = if self.current_target != 0 {
            combat_data.get(&self.current_target)
                .map(|td| (td.last_damage_time - td.first_damage_time).max(0))
                .unwrap_or(0)
        } else if !target_ids.is_empty() {
            // Multi-target: use max battle time across selected targets
            target_ids.iter()
                .filter_map(|tid| combat_data.get(tid))
                .map(|td| (td.last_damage_time - td.first_damage_time).max(0))
                .max()
                .unwrap_or(0)
        } else {
            0
        };

        if (battle_time == 0 && combined_actors.is_empty()) || combined_actors.is_empty() {
            if let Some(ref mut snapshot) = self.last_dps_snapshot {
                snapshot.target_name = dps_data.target_name.clone();
                snapshot.target_mode = dps_data.target_mode.clone();
                snapshot.target_id = dps_data.target_id;
                snapshot.target_max_hp = target_max_hp;
                snapshot.target_total_damage = 0;
                snapshot.target_current_hp = target_current_hp;
                snapshot.dungeon_id = dps_data.dungeon_id;
                let mut snap = snapshot.clone();
                self.finalize_rows(&mut snap);
                return snap;
            }
            self.finalize_rows(&mut dps_data);
            self.last_dps_snapshot = Some(dps_data.clone());
            return dps_data;
        }

        // Build canonical nickname map from aggregates
        let canonical = build_nickname_canonical_map_from_aggregates(&combined_actors, &summon_data, &nickname_data, current_local_id.map(|v| v as i32));

        let mut total_damage: f64 = 0.0;

        // Build PersonalData from aggregates (no packet iteration!)
        for (&actor_id, &damage) in &combined_actors {
            let raw_uid = summon_resolver::resolve(actor_id, &summon_data);
            if raw_uid <= 0 { continue; }
            let nickname = resolve_nickname(raw_uid, &nickname_data, &summon_data);
            let uid = *canonical.get(&nickname).unwrap_or(&raw_uid);

            total_damage += damage as f64;

            let entry = dps_data.map.entry(uid).or_insert_with(|| {
                let cached_job = self.cached_job(&nickname);
                if let Some(job) = cached_job {
                    PersonalData::with_job(nickname.clone(), job)
                } else {
                    PersonalData::new(nickname.clone())
                }
            });

            if entry.nickname != nickname {
                entry.nickname = nickname.clone();
            }

            entry.amount += damage as f64;

            if entry.job.is_empty() {
                if let Some(job) = combined_jobs.get(&actor_id).and_then(|j| *j) {
                    entry.job = job.class_name().to_string();
                    self.cache_job(&nickname, job.class_name());
                }
            }
        }

        // Orphan summon inference: attribute an entity that is really a summon to
        // the player who owns it, for the summons the spawn packet never covered.
        //
        // Two things used to make this miss the case it exists for. It required the
        // owner to be NAMED, and it skipped anything in `known_player_ids` — but a
        // summon lands in that set automatically, because skills like Divine Aura
        // (17153450) sit in the player band and `append_damage` classifies any
        // actor using one as a player. So a Cleric's aura was filed as a player and
        // then never reconsidered, which is why it showed as its own `#id` row next
        // to an equally-unnamed Cleric.
        //
        // The discriminator is the power scalar carried in every damage record: a
        // summon inherits its owner's (see `DataStorage::actor_power_scalars`).
        // Matched against ground truth from a capture where the spawn packets DID
        // arrive — 81 known summon/owner pairs — scalar + class + "the owner has a
        // real rotation" decided 43 of them with **zero** wrong answers and never
        // merged a real player into another.
        let known_players = self.data_storage.get_known_player_ids();
        let scalars = self.data_storage.get_power_scalars();
        // Distinct skills per actor, taken from the combat aggregates — this fast
        // path builds PersonalData from totals and leaves `analyzed_data` empty,
        // so counting that instead would silently read zero for everyone.
        let mut skill_counts: HashMap<i32, HashSet<i32>> = HashMap::new();
        for &tid in &target_ids {
            if let Some(target_data) = combat_data.get(&tid) {
                for (&actor_id, actor_data) in &target_data.actors {
                    let e = skill_counts.entry(actor_id).or_default();
                    for &(code, _) in actor_data.skills.keys() {
                        e.insert(code);
                    }
                }
            }
        }
        let skill_counts: HashMap<i32, usize> =
            skill_counts.into_iter().map(|(k, v)| (k, v.len())).collect();
        let mut orphan_merges: Vec<(i32, i32)> = Vec::new();
        for (&uid, data) in &dps_data.map {
            if summon_data.contains_key(&uid) { continue; }
            if nickname_data.contains_key(&uid) { continue; }
            let job = &data.job;
            // A classless entity was dropped here, and with it its damage. Some
            // spirits only use skills that name no class (16110004, 100044…), so
            // another Elementalist's spirits lost about a tenth of a boss fight
            // (2026-10-03). One that never acted as a player can still go to
            // its owner by power scalar, matched against players of any class;
            // anything else classless is left for the row filter as before.
            let classless = job.is_empty();
            if classless && known_players.contains(&uid) { continue; }
            let my_skills = skill_counts.get(&uid).copied().unwrap_or(0);

            // Original path, unchanged: an entity never classified as a player,
            // attributed to the one NAMED same-class player on the meter. The
            // "named" test is what makes "exactly one candidate" meaningful here —
            // without it, other unnamed orphans of the same class count as
            // candidates and the rule stops firing at all.
            if !classless && !known_players.contains(&uid) {
                let same_job: Vec<_> = dps_data.map.iter()
                    .filter(|(oid, od)| **oid != uid && od.job == *job && nickname_data.contains_key(oid))
                    .map(|(&oid, _)| oid)
                    .collect();
                if same_job.len() == 1 {
                    orphan_merges.push((uid, same_job[0]));
                    continue;
                }
            }

            // Scalar path, for a summon that skill band alone made look like a
            // player. A summon spams one or two abilities; a real player runs a
            // rotation, so requiring the candidate to show at least three times as
            // many distinct skills keeps two genuine players apart even when their
            // scalars happen to coincide.
            let Some(my_scalars) = scalars.get(&uid) else { continue };
            if my_scalars.is_empty() || my_skills == 0 {
                continue;
            }
            // The skill-count guard is only for an actor that might be a real
            // player. One that was never classified as a player (it spawned as
            // a summon) cannot be, and holding it to the guard failed: another
            // player's Elementalist spirits use four to six skills each, more
            // than a third of what their owner showed in a one-minute boss
            // fight, so each stayed its own `#id` row (2026-10-03, two
            // Elementalists in one party). The owner must be a real player,
            // so one orphan never claims another that shares its scalar.
            let needs_rotation = known_players.contains(&uid);
            let owners: Vec<i32> = dps_data.map.iter()
                .filter(|(oid, od)| {
                    **oid != uid
                        && (od.job == *job || (classless && !od.job.is_empty()))
                        && known_players.contains(*oid)
                        && (!needs_rotation
                            || skill_counts.get(*oid).copied().unwrap_or(0) >= 3 * my_skills)
                        && scalars.get(*oid).is_some_and(|s| !s.is_disjoint(my_scalars))
                })
                .map(|(&oid, _)| oid)
                .collect();
            if owners.len() == 1 {
                tracing::debug!(
                    "Summon {} attributed to owner {} by power scalar {:?}",
                    uid, owners[0], my_scalars
                );
                orphan_merges.push((uid, owners[0]));
            }
        }
        // The instance rule from `get_target_details`: an actor of a class
        // only one party member has is that member, or their summon.
        let rows: Vec<(i32, String, usize, i64)> = dps_data.map.iter()
            .map(|(&id, d)| (id, d.job.clone(), skill_counts.get(&id).copied().unwrap_or(0), d.amount as i64))
            .collect();
        if let Some(owners) = self.instance_class_owners(self.data_storage.current_dungeon_id(), &rows) {
            let merged: HashSet<i32> = orphan_merges.iter().map(|(o, _)| *o).collect();
            for (id, job, _, _) in &rows {
                if merged.contains(id) {
                    continue;
                }
                if let Some(&owner) = owners.get(job) {
                    if owner != *id {
                        orphan_merges.push((*id, owner));
                    }
                }
            }
        }
        for (orphan, owner) in orphan_merges {
            if let Some(orphan_data) = dps_data.map.remove(&orphan) {
                if let Some(owner_data) = dps_data.map.get_mut(&owner) {
                    owner_data.merge_from(&orphan_data);
                }
            }
        }

        // Filter and compute DPS
        let local_ids = self.resolve_local_ids(&summon_data);
        let party_members = self.data_storage.get_party_members();
        let bt = battle_time.max(1000);
        let mut to_remove = Vec::new();
        for (&uid, data) in &mut dps_data.map {
            // Combat power joins on the character name: the roster carries an
            // account-level dbid, not the session entity id keyed here.
            data.combat_power = party_members
                .get(&data.nickname)
                .map(|m| m.combat_power)
                .unwrap_or(0);
            if data.job.is_empty() {
                if local_ids.as_ref().is_some_and(|ids| ids.contains(&uid)) {
                    // A class seen earlier this session survives a reset
                    // (issue #9); "Unknown" draws no icon.
                    data.job = self.cached_job(&data.nickname).unwrap_or_else(|| "Unknown".to_string());
                } else {
                    to_remove.push(uid);
                    continue;
                }
            }
            data.dps = data.amount / bt as f64 * 1000.0;
            data.damage_contribution = if total_damage > 0.0 {
                data.amount / total_damage * 100.0
            } else {
                0.0
            };
        }
        for uid in to_remove {
            dps_data.map.remove(&uid);
        }

        self.finalize_rows(&mut dps_data);

        dps_data.battle_time = battle_time;
        // total_damage here is the cumulative damage to the selected target(s).
        // Paired with target_max_hp it yields remaining = max(0, max_hp - dealt).
        dps_data.target_total_damage = total_damage as i64;
        // If the boss is dead, force the bar to empty. The derived remaining can
        // leave a sliver because the meter never observes every last hit (there is
        // no live boss HP packet), so a kill should still read 0%.
        if target_max_hp > 0
            && self.current_target != 0
            && self.data_storage.is_entity_dead(self.current_target)
        {
            dps_data.target_total_damage = target_max_hp;
            dps_data.target_current_hp = 0;
        }
        self.last_dps_snapshot = Some(dps_data.clone());
        dps_data
    }

    /// Give every party member a row as soon as they join, damage or not, so the
    /// meter shows the group you are actually in rather than only whoever has
    /// swung. The roster is keyed by character name — its `dbid` is an account
    /// id, unrelated to the session entity ids used everywhere else — so bind to
    /// the entity id when it is known and otherwise use a synthetic key placed
    /// above the entity-id range (ids top out at 9,999,999), so it cannot collide
    /// with a real one. The placeholder disappears on its own once real damage
    /// arrives under the player's true id. Not a negative key: the frontend drops
    /// non-positive ids as junk.
    /// Everything that has to happen to a row set before it goes on screen,
    /// in one place so a new return path cannot quietly skip half of it.
    fn finalize_rows(&self, dps_data: &mut DpsData) {
        self.add_party_rows(dps_data);
        self.mark_supporters(dps_data);
    }

    /// Flag supporters so the UI can render their names gold.
    ///
    /// Resolved here rather than in the frontend because the roster is hashed
    /// and the join needs `dbid`, which the frontend never sees. Runs on the
    /// 500ms tick, so it returns immediately when there is no roster — which is
    /// the normal case until one is published.
    fn mark_supporters(&self, dps_data: &mut DpsData) {
        let roster = self.data_storage.supporters();
        if roster.is_empty() {
            return;
        }
        let party = self.data_storage.get_party_members();
        for row in dps_data.map.values_mut() {
            let name = row.nickname.trim();
            if name.is_empty() {
                continue;
            }
            let dbid = party.get(name).map(|m| m.dbid).unwrap_or(0);
            row.is_supporter = roster.contains(name, dbid);
        }
    }

    fn add_party_rows(&self, dps_data: &mut DpsData) {
        if !self.data_storage.party_placeholders_wanted() {
            return;
        }
        let party_members = self.data_storage.get_party_members();
        if party_members.is_empty() {
            return;
        }
        let present: HashSet<String> = dps_data
            .map
            .values()
            .map(|d| d.nickname.trim().to_string())
            .collect();
        for (name, member) in &party_members {
            if present.contains(name.trim()) {
                continue;
            }
            let uid = self
                .data_storage
                .find_id_by_nickname(name)
                .filter(|id| !dps_data.map.contains_key(id))
                .unwrap_or(PARTY_ROW_ID_BASE + member.slot.min(64) as i32);
            let mut entry = PersonalData::new(name.clone());
            // The row filter drops anything without a job. These have not
            // attacked yet: their class from the roster, else from earlier
            // this session, else "Unknown", which draws no icon (issue #9).
            entry.job = member
                .job
                .map(|j| j.class_name().to_string())
                .or_else(|| self.cached_job(name))
                .unwrap_or_else(|| "Unknown".to_string());
            entry.combat_power = member.combat_power;
            dps_data.map.entry(uid).or_insert(entry);
        }
    }

    fn decide_target(
        &mut self,
        combat_data: &HashMap<i32, TargetCombatData>,
        nickname_data: &HashMap<i32, String>,
        summon_data: &HashMap<i32, i32>,
    ) -> (HashSet<i32>, String, i32) {
        let mob_data = self.data_storage.get_mob_data();

        match self.target_selection_mode {
            TargetSelectionMode::MostDamage => {
                let best = combat_data.iter()
                    .max_by_key(|(_, td)| td.total_damage);
                match best {
                    Some((&id, _)) => {
                        let name = self.resolve_target_name(id);
                        (HashSet::from([id]), name, id)
                    }
                    None => (HashSet::new(), String::new(), 0),
                }
            }
            TargetSelectionMode::MostRecent => {
                let best = combat_data.iter()
                    .max_by_key(|(_, td)| td.last_damage_time);
                match best {
                    Some((&id, _)) => {
                        let name = self.resolve_target_name(id);
                        (HashSet::from([id]), name, id)
                    }
                    None => (HashSet::new(), String::new(), 0),
                }
            }
            TargetSelectionMode::BossTargets => {
                let boss_targets: Vec<_> = combat_data.keys()
                    .filter(|&&tid| {
                        if let Some(&mob_code) = mob_data.get(&tid) {
                            self.npc_lookup.is_boss(mob_code)
                        } else {
                            false
                        }
                    })
                    .cloned()
                    .collect();

                if let Some(&best) = boss_targets.iter()
                    .max_by_key(|&&tid| combat_data.get(&tid).map(|td| td.last_damage_time).unwrap_or(0))
                {
                    let name = self.resolve_target_name(best);
                    (HashSet::from([best]), name, best)
                } else if self.data_storage.current_dungeon_id() > 0 {
                    // No boss yet in a dungeon: show nothing. Every mob in an
                    // instance is on the way to a boss, so the fallback below
                    // put the first trash pull of each run on the meter.
                    (HashSet::new(), String::new(), 0)
                } else {
                    // No boss: the mob with the most damage. Once you are
                    // identified, only one you or your party hit. Any mob
                    // within range counts otherwise, and in the open world
                    // that put strangers fighting their own mobs on your
                    // meter (2026-10-04: one player, then another, each alone
                    // on a mob you never touched).
                    let ours = self.resolve_local_ids(summon_data).map(|mut ids| {
                        let party = self.data_storage.get_party_members();
                        ids.extend(nickname_data.iter()
                            .filter(|(_, name)| party.contains_key(name.as_str()))
                            .map(|(&id, _)| id));
                        ids
                    });
                    let best = combat_data.iter()
                        .filter(|(_, td)| ours.as_ref().is_none_or(|ids| td.actors.keys()
                            .any(|&a| ids.contains(&summon_resolver::resolve(a, summon_data)))))
                        .max_by_key(|(_, td)| td.total_damage);
                    match best {
                        Some((&id, _)) => {
                            let name = self.resolve_target_name(id);
                            (HashSet::from([id]), name, id)
                        }
                        None => (HashSet::new(), String::new(), 0),
                    }
                }
            }
            TargetSelectionMode::AllTargets => {
                let all: HashSet<i32> = combat_data.keys().cloned().collect();
                (all, "All Targets".to_string(), 0)
            }
            TargetSelectionMode::TrainTargets => {
                let trains: HashSet<i32> = combat_data.keys()
                    .filter(|&&tid| {
                        mob_data.get(&tid).is_some_and(|&code| self.npc_lookup.is_training_dummy(code))
                    })
                    .cloned()
                    .collect();
                (trains, "Train".to_string(), 0)
            }
            TargetSelectionMode::LastHitByMe => {
                let local_ids = self.resolve_local_ids(summon_data);
                if let Some(ref ids) = local_ids {
                    // Find the target most recently damaged by the local player
                    let mut best_target: Option<(i32, i64)> = None;
                    for (&target_id, target_data) in combat_data {
                        for (&actor_id, actor_data) in &target_data.actors {
                            let resolved = summon_resolver::resolve(actor_id, summon_data);
                            if ids.contains(&resolved) {
                                let ts = actor_data.last_damage_time;
                                if best_target.is_none() || ts > best_target.unwrap().1 {
                                    best_target = Some((target_id, ts));
                                }
                            }
                        }
                    }
                    match best_target {
                        Some((id, _)) => {
                            let name = self.resolve_target_name(id);
                            (HashSet::from([id]), name, id)
                        }
                        None => (HashSet::new(), String::new(), 0),
                    }
                } else {
                    // Not identified — fall back to most recently damaged target
                    let best = combat_data.iter()
                        .max_by_key(|(_, td)| td.last_damage_time);
                    match best {
                        Some((&id, _)) => {
                            let name = self.resolve_target_name(id);
                            (HashSet::from([id]), name, id)
                        }
                        None => (HashSet::new(), String::new(), 0),
                    }
                }
            }
        }
    }

    /// In an instance the party roster names each member's class. For a class
    /// only one member has, every actor of that class in the fight is that
    /// member, under an older entity id (meters before 2.0.42 lost track of
    /// the local player's), or one of their summons whose owner the capture
    /// never named. Returns each such class with the actor to credit: the one
    /// with the most distinct skills (a player's rotation, against a summon's
    /// one or two), then the most damage.
    ///
    /// None outside an instance, when the roster leaves a member's class
    /// unknown, or when more actors run a rotation than the party has members:
    /// then another party is in the fight and a class says nothing.
    ///
    /// `dungeon_id`: the instance the fight was in.
    /// `actors`: (row id, class name, distinct skills, damage).
    fn instance_class_owners(&self, dungeon_id: i32, actors: &[(i32, String, usize, i64)]) -> Option<HashMap<String, i32>> {
        const ROTATION_SKILLS: usize = 5;
        if dungeon_id <= 0 {
            return None;
        }
        let party = self.data_storage.get_party_members();
        if party.len() < 2 {
            return None;
        }
        let mut members_of: HashMap<String, usize> = HashMap::new();
        for member in party.values() {
            *members_of.entry(member.job?.class_name().to_string()).or_default() += 1;
        }
        if actors.iter().filter(|a| a.2 >= ROTATION_SKILLS).count() > party.len() {
            return None;
        }
        let mut best: HashMap<String, (i32, usize, i64)> = HashMap::new();
        for (id, job, skills, damage) in actors {
            if members_of.get(job) != Some(&1) {
                continue;
            }
            if best.get(job).is_none_or(|&(_, s, d)| (*skills, *damage) > (s, d)) {
                best.insert(job.clone(), (*id, *skills, *damage));
            }
        }
        Some(best.into_iter().map(|(job, (id, _, _))| (job, id)).collect())
    }

    fn resolve_target_name(&self, target_id: i32) -> String {
        let mob_data = self.data_storage.get_mob_data();
        if let Some(&code) = mob_data.get(&target_id) {
            let name = self.npc_lookup.get_npc_name(code);
            if !name.is_empty() {
                return name;
            }
        }
        String::new()
    }

    fn resolve_local_ids(&self, summon_data: &HashMap<i32, i32>) -> Option<HashSet<i32>> {
        let local_id = self.data_storage.local_player_id()? as i32;
        let mut ids = HashSet::new();
        ids.insert(local_id);
        for (&summon, &owner) in summon_data {
            if summon_resolver::resolve(owner, summon_data) == local_id {
                ids.insert(summon);
            }
        }
        Some(ids)
    }

    fn cached_job(&self, nickname: &str) -> Option<String> {
        let key = nickname.trim().to_lowercase();
        if key.is_empty() || key.chars().all(|c| c.is_ascii_digit()) { return None; }
        self.nickname_job_cache.get(&key)
            .filter(|j| !j.is_empty() && *j != "Unknown")
            .cloned()
    }

    fn cache_job(&mut self, nickname: &str, job: &str) {
        if job.is_empty() || job == "Unknown" { return; }
        let key = nickname.trim().to_lowercase();
        if key.is_empty() || key.chars().all(|c| c.is_ascii_digit()) { return; }
        self.nickname_job_cache.insert(key, job.to_string());
    }

    /// Whether the local player, one of their summons or a party member hit
    /// this target. Every boss and training dummy in range used to be saved,
    /// whoever fought it, and a field boss fought only by two strangers was
    /// auto-uploaded under the local player's account (issue #19). Without
    /// either a local id or a party to go on, every fight counts, as before.
    fn is_our_fight(&self, target: &TargetCombatData) -> bool {
        let summon_data = self.data_storage.get_summon_data();
        let nicknames = self.data_storage.get_nicknames();
        let party = self.data_storage.get_party_members();
        let local = self.resolve_local_ids(&summon_data);
        if local.is_none() && party.is_empty() {
            return true;
        }
        target.actors.keys().any(|&actor| {
            let owner = summon_resolver::resolve(actor, &summon_data);
            local.as_ref().is_some_and(|ids| ids.contains(&actor) || ids.contains(&owner))
                || nicknames.get(&owner).is_some_and(|name| party.contains_key(name))
        })
    }

    pub fn snapshot_boss_fights(&mut self) -> Vec<FightRecord> {
        self.snapshot_boss_fights_inner(false)
    }

    pub fn snapshot_boss_fights_force(&mut self) -> Vec<FightRecord> {
        self.snapshot_boss_fights_inner(true)
    }

    fn snapshot_boss_fights_inner(&mut self, force: bool) -> Vec<FightRecord> {
        let mob_data = self.data_storage.get_mob_data();
        // Light snapshot: only used for target filtering + per-actor aggregate
        // stats here; the saved record's timestamps come from get_target_details.
        let combat_data = self.data_storage.get_combat_snapshot_light();
        // Once per snapshot rather than once per actor: the roster is a clone
        // behind a lock, and it does not change between targets here.
        let party_members = self.data_storage.get_party_members();
        let supporters = self.data_storage.supporters();
        let now_ms = crate::clock::now_ms();

        let mut records = Vec::new();

        let boss_target_ids: Vec<i32> = combat_data.keys()
            .filter(|&&tid| {
                // Saved as ended and not hit since: nothing new to save.
                let last_hit = combat_data.get(&tid).map(|td| td.last_damage_time).unwrap_or(0);
                if self.saved_boss_targets.get(&tid).is_some_and(|&saved| saved >= last_hit) {
                    return false;
                }
                if let Some(&code) = mob_data.get(&tid) {
                    self.npc_lookup.is_boss(code) || self.npc_lookup.is_training_dummy(code)
                } else {
                    false
                }
            })
            .cloned()
            .collect();

        if !boss_target_ids.is_empty() {
            tracing::trace!("snapshot_boss_fights: {} candidate targets", boss_target_ids.len());
        }

        for target_id in boss_target_ids {
            let target_data = match combat_data.get(&target_id) {
                Some(td) => td,
                None => continue,
            };

            let battle_time = (target_data.last_damage_time - target_data.first_damage_time).max(0);
            if battle_time < 5_000 || target_data.total_damage <= 0 {
                continue;
            }
            if !self.is_our_fight(target_data) {
                continue;
            }

            let idle_time = now_ms - target_data.last_damage_time;
            let is_ended = idle_time >= 10_000;
            let is_periodic = battle_time >= 15_000;
            if !force && !is_ended && !is_periodic {
                continue;
            }

            // Generate fight record
            let details = self.get_target_details(target_id, None);
            let nickname_data = self.data_storage.get_nicknames();
            let summon_data_snap = self.data_storage.get_summon_data();

            let mut record_actors: HashMap<i32, (String, String)> = HashMap::new();
            for skill in &details.skills {
                let uid = skill.actor_id;
                record_actors.entry(uid).or_insert_with(|| {
                    let nick = resolve_nickname(uid, &nickname_data, &summon_data_snap);
                    let job = if !skill.job.is_empty() { skill.job.clone() }
                        else { JobClass::convert_from_skill(skill.code).map(|j| j.class_name().to_string()).unwrap_or_default() };
                    (nick, job)
                });
                let entry = record_actors.get_mut(&uid).unwrap();
                if entry.1.is_empty() && !skill.job.is_empty() {
                    entry.1 = skill.job.clone();
                }
            }

            let local_id = self.data_storage.local_player_id().unwrap_or(-1) as i32;
            let actors: Vec<DetailsActorSummary> = record_actors.iter()
                .map(|(&id, (nick, job))| {
                    let display_nick = if id == local_id {
                        nick.clone()
                    } else {
                        crate::entity::fight_record::obscure_nickname(nick)
                    };
                    let job_class = JobClass::convert_from_skill(
                        details.skills.iter()
                            .find(|s| s.actor_id == id && !s.job.is_empty())
                            .map(|s| s.code)
                            .unwrap_or(0)
                    );
                    // Aggregate per-actor stats across all targets
                    let (mut party_heal, mut regen, mut dmg_recv, mut hits_recv) = (0i64, 0i64, 0i64, 0i32);
                    for td in combat_data.values() {
                        if let Some(ad) = td.actors.get(&id) {
                            party_heal += ad.party_heal;
                            regen += ad.regen;
                            dmg_recv += ad.damage_received;
                            hits_recv += ad.hits_received;
                        }
                    }
                    // Joined on the unobscured nickname: the roster is keyed by
                    // name, and `display_nick` above has already been masked for
                    // everyone but the local player.
                    let roster = party_members.get(nick.as_str());
                    DetailsActorSummary {
                        actor_id: id,
                        nickname: display_nick,
                        job: job.clone(),
                        job_id: job_class.map(|j| j.class_prefix()).unwrap_or(0),
                        party_heal,
                        regen,
                        damage_received: dmg_recv,
                        hits_received: hits_recv,
                        dbid: roster.map(|m| m.dbid).unwrap_or(0),
                        server_id: roster.map(|m| m.server_id).unwrap_or(0),
                        is_supporter: supporters
                            .contains(nick, roster.map(|m| m.dbid).unwrap_or(0)),
                        level: roster.map(|m| m.level).unwrap_or(0),
                        gear_score: roster.map(|m| m.gear_score).unwrap_or(0),
                        combat_power: roster.map(|m| m.combat_power).unwrap_or(0),
                    }
                })
                .collect();

            let mob_code = mob_data.get(&target_id).copied().unwrap_or(0);
            let boss_name = self.resolve_target_name(target_id);

            let job_ids: Vec<i32> = actors.iter()
                .filter(|a| a.job_id > 0)
                .map(|a| a.job_id)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            let jobs: Vec<String> = actors.iter()
                .filter(|a| !a.job.is_empty() && a.job != "Unknown")
                .map(|a| a.job.clone())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();

            let id = format!("auto_{}_{}", target_id, target_data.first_damage_time);

            let is_train = self.npc_lookup.is_training_dummy(mob_code);
            let record = FightRecord {
                id,
                boss_name,
                target_id,
                start_time_ms: target_data.first_damage_time,
                duration_ms: battle_time,
                total_damage: target_data.total_damage as i32,
                jobs,
                job_ids,
                details,
                actors,
                is_train,
                app_version: crate::entity::fight_record::APP_VERSION.to_string(),
                mob_code,
                dungeon_id: target_data.dungeon_id,
                server_id: self.data_storage.fight_server_id(),
            };

            if is_ended {
                self.saved_boss_targets.insert(target_id, target_data.last_damage_time);
            }
            records.push(record);
        }

        records
    }

    pub fn get_details_context(&self) -> DetailsContext {
        // Light snapshot: this builds per-target/per-actor summaries only — the
        // per-hit timeline is fetched separately via get_target_details.
        let combat_data = self.data_storage.get_combat_snapshot_light();
        let nickname_data = self.data_storage.get_nicknames();
        let summon_data = self.data_storage.get_summon_data();
        let supporters = self.data_storage.supporters();
        let mob_hp_data = self.data_storage.get_mob_hp_data();
        let mob_data = self.data_storage.get_mob_data();

        let mut actor_meta: HashMap<i32, (String, String)> = HashMap::new();
        let mut targets = Vec::new();

        for (&target_id, target_data) in &combat_data {
            let mut actor_damage: HashMap<i32, i32> = HashMap::new();
            let canonical = build_nickname_canonical_map_from_aggregates(
                &target_data.actors.iter().map(|(&id, ad)| (id, ad.total_damage)).collect(),
                &summon_data,
                &nickname_data,
                self.data_storage.local_player_id().map(|v| v as i32),
            );

            for (&actor_id, actor_data) in &target_data.actors {
                let raw_uid = summon_resolver::resolve(actor_id, &summon_data);
                if raw_uid <= 0 { continue; }
                let nickname = resolve_nickname(raw_uid, &nickname_data, &summon_data);
                let uid = *canonical.get(&nickname).unwrap_or(&raw_uid);
                *actor_damage.entry(uid).or_insert(0) += actor_data.total_damage as i32;

                actor_meta.entry(uid).or_insert_with(|| {
                    (resolve_nickname(uid, &nickname_data, &summon_data), String::new())
                });

                if actor_meta.get(&uid).unwrap().1.is_empty() {
                    if let Some(job) = actor_data.job {
                        actor_meta.get_mut(&uid).unwrap().1 = job.class_name().to_string();
                    }
                }
            }

            // Orphan summon inference: merge true orphans (not known players) into
            // the same-class player when there is exactly one — an unambiguous
            // owner even if several such orphans share the class.
            let target_actor_ids: HashSet<i32> = actor_damage.keys().copied().collect();
            let known_players = self.data_storage.get_known_player_ids();
            let mut orphan_merges: Vec<(i32, i32)> = Vec::new();
            for (&uid, (_, job)) in &actor_meta {
                if !target_actor_ids.contains(&uid) { continue; }
                if summon_data.contains_key(&uid) { continue; }
                if nickname_data.contains_key(&uid) { continue; }
                if known_players.contains(&uid) { continue; }
                if job.is_empty() { continue; }
                let same_job: Vec<i32> = actor_meta.iter()
                    .filter(|(oid, (_, oj))| **oid != uid && *oj == *job
                        && nickname_data.contains_key(oid)
                        && target_actor_ids.contains(oid))
                    .map(|(oid, _)| *oid)
                    .collect();
                if same_job.len() == 1 {
                    orphan_merges.push((uid, same_job[0]));
                }
            }
            for (orphan, owner) in &orphan_merges {
                if let Some(dmg) = actor_damage.remove(orphan) {
                    *actor_damage.entry(*owner).or_insert(0) += dmg;
                }
                actor_meta.remove(orphan);
            }
            // Remove actors with no job and no nickname
            let remove_ids: Vec<i32> = actor_damage.keys()
                .filter(|id| {
                    actor_meta.get(id).is_some_and(|(_, job)| job.is_empty() && !nickname_data.contains_key(id))
                })
                .copied().collect();
            for id in remove_ids {
                actor_damage.remove(&id);
                actor_meta.remove(&id);
            }

            let target_name = if let Some(&code) = mob_data.get(&target_id) {
                self.npc_lookup.get_npc_name(code)
            } else {
                String::new()
            };

            targets.push(DetailsTargetSummary {
                target_id,
                target_name,
                max_hp: mob_hp_data.get(&target_id).copied().unwrap_or(0),
                battle_time: (target_data.last_damage_time - target_data.first_damage_time).max(0),
                last_damage_time: target_data.last_damage_time,
                total_damage: target_data.total_damage as i32,
                actor_damage,
            });
        }

        let actors: Vec<DetailsActorSummary> = actor_meta.iter()
            .map(|(&id, (nick, job))| {
                let job_id = if let Some(jc) = JobClass::convert_from_skill(
                    // Find a skill code from this actor's aggregate data
                    combat_data.values()
                        .flat_map(|td| td.actors.get(&id))
                        .flat_map(|ad| ad.skills.keys())
                        .find(|&&(sc, _)| JobClass::convert_from_skill(sc).is_some())
                        .map(|&(sc, _)| sc)
                        .unwrap_or(0)
                ) { jc.class_prefix() } else { 0 };
                // Aggregate per-actor stats
                let (mut party_heal, mut regen, mut dmg_recv, mut hits_recv) = (0i64, 0i64, 0i64, 0i32);
                for td in combat_data.values() {
                    if let Some(ad) = td.actors.get(&id) {
                        party_heal += ad.party_heal;
                        regen += ad.regen;
                        dmg_recv += ad.damage_received;
                        hits_recv += ad.hits_received;
                    }
                }
                DetailsActorSummary {
                    actor_id: id,
                    nickname: nick.clone(),
                    job: job.clone(),
                    job_id,
                    party_heal,
                    regen,
                    damage_received: dmg_recv,
                    hits_received: hits_recv,
                    // The live view is never uploaded, and this runs on every
                    // refresh — not worth taking the roster lock for identity
                    // nothing here reads.
                    dbid: 0,
                    server_id: 0,
                    // By name only, for the same reason: a name-keyed roster
                    // needs no dbid, and a dbid-keyed one is a later state that
                    // will come with the party join it needs.
                    is_supporter: supporters.contains(nick, 0),
                    // Same reason again: the live rows already show combat
                    // power from the roster; the saved record is where it
                    // has to persist.
                    level: 0,
                    gear_score: 0,
                    combat_power: 0,
                }
            })
            .collect();

        DetailsContext {
            current_target_id: self.current_target,
            targets,
            actors,
        }
    }

    pub fn get_target_details(&self, target_id: i32, actor_ids: Option<&[i32]>) -> TargetDetailsResponse {
        let combat_data = self.data_storage.get_combat_snapshot();
        let target_data = match combat_data.get(&target_id) {
            Some(td) => td,
            None => return TargetDetailsResponse {
                target_id,
                max_hp: 0,
                total_target_damage: 0,
                battle_time: 0,
                start_time: 0,
                skills: Vec::new(),
                ping_history: Vec::new(),
                heal_skills: Vec::new(),
            },
        };

        let summon_data = self.data_storage.get_summon_data();
        let nickname_data = self.data_storage.get_nicknames();
        let mob_hp_data = self.data_storage.get_mob_hp_data();

        let actor_damage_map: HashMap<i32, i64> = target_data.actors.iter()
            .map(|(&id, ad)| (id, ad.total_damage))
            .collect();
        let canonical = build_nickname_canonical_map_from_aggregates(&actor_damage_map, &summon_data, &nickname_data, self.data_storage.local_player_id().map(|v| v as i32));

        // Build orphan summon map
        let mut orphan_to_owner: HashMap<i32, i32> = HashMap::new();
        {
            let known_players = self.data_storage.get_known_player_ids();
            let mut actor_jobs: HashMap<i32, String> = HashMap::new();
            for (&actor_id, actor_data) in &target_data.actors {
                let raw_uid = summon_resolver::resolve(actor_id, &summon_data);
                if raw_uid <= 0 { continue; }
                let uid = *canonical.get(&resolve_nickname(raw_uid, &nickname_data, &summon_data)).unwrap_or(&raw_uid);
                if actor_jobs.contains_key(&uid) { continue; }
                if let Some(job) = actor_data.job {
                    actor_jobs.insert(uid, job.class_name().to_string());
                }
            }
            let scalars = self.data_storage.get_power_scalars();
            // Players who hit this target, as the rows they end up on.
            let player_ids: HashSet<i32> = target_data.actors.keys()
                .map(|&id| summon_resolver::resolve(id, &summon_data))
                .filter(|id| known_players.contains(id))
                .map(|id| *canonical.get(&resolve_nickname(id, &nickname_data, &summon_data)).unwrap_or(&id))
                .collect();
            let mut seen = HashSet::new();
            for (&actor_id, actor_data) in &target_data.actors {
                let raw_uid = summon_resolver::resolve(actor_id, &summon_data);
                if raw_uid <= 0 { continue; }
                if summon_data.contains_key(&raw_uid) || nickname_data.contains_key(&raw_uid) { continue; }
                // Never merge known players — they have their own identity
                if known_players.contains(&raw_uid) { continue; }
                if !seen.insert(raw_uid) { continue; }
                // Use loose detection from any skill this actor used
                let job = actor_data.skills.keys()
                    .find_map(|&(sc, _)| JobClass::convert_from_skill_loose(sc))
                    .map(|j| j.class_name().to_string());
                if let Some(job) = &job {
                    let matching: Vec<i32> = actor_jobs.iter()
                        .filter(|(id, j)| **id != raw_uid && *j == job && nickname_data.contains_key(id))
                        .map(|(id, _)| *id)
                        .collect();
                    if matching.len() == 1 {
                        orphan_to_owner.insert(raw_uid, matching[0]);
                        continue;
                    }
                }
                // The power scalar, as in `get_dps`: with two players of a
                // class named, the class says nothing, and a spirit whose
                // skills name no class has none to go on. History showed
                // another Elementalist's spirits as their own rows, or folded
                // every spirit into one of the two (2026-10-03).
                let Some(mine) = scalars.get(&raw_uid).filter(|s| !s.is_empty()) else { continue };
                let owners: Vec<i32> = player_ids.iter()
                    .copied()
                    .filter(|&id| {
                        id != raw_uid
                            && job.as_ref().is_none_or(|j| actor_jobs.get(&id) == Some(j))
                            && scalars.get(&id).is_some_and(|s| !s.is_disjoint(mine))
                    })
                    .collect();
                if owners.len() == 1 {
                    orphan_to_owner.insert(raw_uid, owners[0]);
                }
            }

            // In an instance, an actor of a class only one party member has
            // is that member or their summon. Slices from older meters carry
            // no spawn packets, lost track of the local player's id, and tie
            // names to stale ids, so neither the names nor the scalar above can
            // place them: one Vakron log kept 14 rows of a Sorcerer's summons
            // and the uploader split in two (2026-10-05).
            let mut rows: HashMap<i32, (String, HashSet<i32>, i64)> = HashMap::new();
            for (&actor_id, actor_data) in &target_data.actors {
                let raw_uid = summon_resolver::resolve(actor_id, &summon_data);
                if raw_uid <= 0 || orphan_to_owner.contains_key(&raw_uid) { continue; }
                let job = actor_data.job
                    .or_else(|| actor_data.skills.keys()
                        .find_map(|&(sc, _)| JobClass::convert_from_skill_loose(sc)))
                    .map(|j| j.class_name().to_string())
                    .unwrap_or_default();
                let row = rows.entry(raw_uid).or_insert_with(|| (String::new(), HashSet::new(), 0));
                if row.0.is_empty() { row.0 = job; }
                row.1.extend(actor_data.skills.keys().map(|k| k.0));
                row.2 += actor_data.total_damage;
            }
            let rows: Vec<(i32, String, usize, i64)> = rows.into_iter()
                .map(|(id, (job, skills, damage))| (id, job, skills.len(), damage))
                .collect();
            if let Some(owners) = self.instance_class_owners(target_data.dungeon_id, &rows) {
                for (id, job, _, _) in &rows {
                    if let Some(&owner) = owners.get(job) {
                        if owner != *id {
                            orphan_to_owner.insert(*id, owner);
                        }
                    }
                }
            }
        }

        // Build expanded actor ID set for filtering
        let filter_uids: Option<HashSet<i32>> = actor_ids.map(|ids| {
            let canonical_ids: HashSet<i32> = ids.iter()
                .map(|&id| {
                    let nick = resolve_nickname(id, &nickname_data, &summon_data);
                    *canonical.get(&nick).unwrap_or(&id)
                })
                .collect();
            let mut expanded = HashSet::from_iter(ids.iter().copied());
            for (&actor_id, _) in &target_data.actors {
                let raw_uid = summon_resolver::resolve(actor_id, &summon_data);
                if raw_uid <= 0 { continue; }
                let remapped = *orphan_to_owner.get(&raw_uid).unwrap_or(&raw_uid);
                let nick = resolve_nickname(remapped, &nickname_data, &summon_data);
                let uid = *canonical.get(&nick).unwrap_or(&remapped);
                if canonical_ids.contains(&uid) {
                    expanded.insert(raw_uid);
                }
            }
            for (&orphan, &owner) in &orphan_to_owner {
                let nick = resolve_nickname(owner, &nickname_data, &summon_data);
                let uid = *canonical.get(&nick).unwrap_or(&owner);
                if canonical_ids.contains(&uid) {
                    expanded.insert(orphan);
                }
            }
            expanded
        });

        // Build skill entries from aggregates (no packet iteration!)
        let mut skill_map: HashMap<(i32, i32), DetailSkillEntry> = HashMap::new();
        let fight_start = target_data.first_damage_time;

        for (&actor_id, actor_data) in &target_data.actors {
            let raw_uid = summon_resolver::resolve(actor_id, &summon_data);
            if raw_uid <= 0 { continue; }

            if let Some(ref filter) = filter_uids {
                if !filter.contains(&raw_uid) { continue; }
            }

            let remapped = *orphan_to_owner.get(&raw_uid).unwrap_or(&raw_uid);
            let nickname = resolve_nickname(remapped, &nickname_data, &summon_data);
            let uid = *canonical.get(&nickname).unwrap_or(&remapped);

            for (&(raw_skill, is_dot), skill_data) in &actor_data.skills {
                // Normalize skill code
                let skill_code = {
                    let base = raw_skill - (raw_skill % 10000);
                    let base_name = self.skill_lookup.get_skill_name(base);
                    if !base_name.is_empty() {
                        let raw_name = self.skill_lookup.get_skill_name(raw_skill);
                        if raw_name.is_empty() || raw_name == base_name { base } else { raw_skill }
                    } else { raw_skill }
                };

                let dot_offset = if is_dot { 1_000_000_000 } else { 0 };
                let key = (uid, skill_code + dot_offset);
                let mut skill_name = self.skill_lookup.lookup_skill_name(skill_code);
                if is_dot && !skill_name.is_empty() {
                    skill_name = format!("{} - DOT", skill_name);
                }
                let job = JobClass::convert_from_skill(skill_code)
                    .map(|j| j.class_name().to_string())
                    .unwrap_or_default();

                let entry = skill_map.entry(key).or_insert_with(|| DetailSkillEntry {
                    actor_id: uid,
                    code: skill_code,
                    name: skill_name,
                    time: 0,
                    dmg: 0,
                    multi_hit_count: 0,
                    multi_hit_damage: 0,
                    multi_hit_hits: 0,
                    min_dmg: i32::MAX,
                    max_dmg: 0,
                    crit: 0,
                    parry: 0,
                    back: 0,
                    frontal: 0,
                    perfect: 0,
                    double: 0,
                    smite: 0,
                    powershard: 0,
                    regen: 0,
                    job,
                    is_dot,
                    hit_timestamps: Vec::new(),
                    specs: skill_data.spec_flags.to_vec(),
                });

                entry.time += skill_data.hit_count;
                // saturating: damage sums are i32 and can exceed i32::MAX across
                // a long fight / many actors — avoid overflow panic (debug) and
                // wrap-to-negative (release).
                entry.dmg = entry.dmg.saturating_add(skill_data.total_damage);
                entry.multi_hit_count += skill_data.multi_hit_count;
                entry.multi_hit_damage = entry.multi_hit_damage.saturating_add(skill_data.multi_hit_damage);
                entry.multi_hit_hits += skill_data.multi_hit_hits;
                if skill_data.min_damage < entry.min_dmg { entry.min_dmg = skill_data.min_damage; }
                if skill_data.max_damage > entry.max_dmg { entry.max_dmg = skill_data.max_damage; }
                entry.crit += skill_data.crit_count;
                entry.back += skill_data.back_count;
                entry.frontal += skill_data.frontal_count;
                entry.parry += skill_data.parry_count;
                entry.perfect += skill_data.perfect_count;
                entry.double += skill_data.double_count;
                entry.smite += skill_data.smite_count;
                entry.powershard += skill_data.powershard_count;
                entry.regen = entry.regen.saturating_add(skill_data.heal_amount);
                // Add timestamps relative to fight start
                for &ts in &skill_data.hit_timestamps {
                    entry.hit_timestamps.push(ts - fight_start);
                }
                // Merge spec flags
                for (i, &flag) in skill_data.spec_flags.iter().enumerate() {
                    if flag && i < entry.specs.len() { entry.specs[i] = true; }
                }
            }
        }

        // Fix min_dmg sentinel
        for entry in skill_map.values_mut() {
            if entry.min_dmg == i32::MAX { entry.min_dmg = 0; }
        }

        // Healing done this segment, per healer/skill. Keyed by the canonical actor
        // (same nickname/orphan resolution as damage). Reuses DetailSkillEntry:
        // dmg = heal amount, time = tick count, is_dot = HoT.
        let mut heal_map: HashMap<(i32, i32), DetailSkillEntry> = HashMap::new();
        for (&actor_id, skills) in &self.data_storage.get_heal_snapshot() {
            let raw_uid = summon_resolver::resolve(actor_id, &summon_data);
            if raw_uid <= 0 { continue; }
            let remapped = *orphan_to_owner.get(&raw_uid).unwrap_or(&raw_uid);
            let nickname = resolve_nickname(remapped, &nickname_data, &summon_data);
            let uid = *canonical.get(&nickname).unwrap_or(&remapped);
            if let Some(ref filter) = filter_uids {
                if !filter.contains(&uid) { continue; }
            }
            for (&(skill_code, is_hot), hd) in skills {
                let mut skill_name = self.skill_lookup.lookup_skill_name(skill_code);
                if is_hot && !skill_name.is_empty() {
                    skill_name = format!("{} - HoT", skill_name);
                }
                let job = JobClass::convert_from_skill(skill_code)
                    .map(|j| j.class_name().to_string())
                    .unwrap_or_default();
                let key = (uid, skill_code + if is_hot { 1_000_000_000 } else { 0 });
                let entry = heal_map.entry(key).or_insert_with(|| DetailSkillEntry {
                    actor_id: uid,
                    code: skill_code,
                    name: skill_name,
                    time: 0,
                    dmg: 0,
                    multi_hit_count: 0,
                    multi_hit_damage: 0,
                    multi_hit_hits: 0,
                    min_dmg: 0,
                    max_dmg: 0,
                    crit: 0,
                    parry: 0,
                    back: 0,
                    frontal: 0,
                    perfect: 0,
                    double: 0,
                    smite: 0,
                    powershard: 0,
                    regen: 0,
                    job,
                    is_dot: is_hot,
                    hit_timestamps: Vec::new(),
                    specs: Vec::new(),
                });
                entry.dmg = entry.dmg.saturating_add(hd.total_heal.min(i32::MAX as i64) as i32);
                entry.time += hd.tick_count;
            }
        }

        let battle_time = (target_data.last_damage_time - target_data.first_damage_time).max(0);

        let ping_history = self.ping_tracker.get_ping_history(
            target_data.first_damage_time, target_data.last_damage_time
        ).into_iter()
            .map(|(ts, ping)| PingPoint { ts_ms: ts - fight_start, ping_ms: ping })
            .collect();

        TargetDetailsResponse {
            target_id,
            max_hp: mob_hp_data.get(&target_id).copied().unwrap_or(0),
            total_target_damage: target_data.total_damage as i32,
            battle_time,
            start_time: target_data.first_damage_time,
            skills: skill_map.into_values().collect(),
            ping_history,
            heal_skills: heal_map.into_values().collect(),
        }
    }
}

fn resolve_nickname(uid: i32, nicknames: &HashMap<i32, String>, summon_data: &HashMap<i32, i32>) -> String {
    if let Some(name) = nicknames.get(&uid) {
        return name.clone();
    }
    let resolved = summon_resolver::resolve(uid, summon_data);
    if let Some(name) = nicknames.get(&resolved) {
        return name.clone();
    }
    uid.to_string()
}

fn build_nickname_canonical_map_from_aggregates(
    actor_damage: &HashMap<i32, i64>,
    summon_data: &HashMap<i32, i32>,
    nickname_data: &HashMap<i32, String>,
    local_player_id: Option<i32>,
) -> HashMap<String, i32> {
    let mut nickname_damage: HashMap<String, HashMap<i32, i64>> = HashMap::new();

    for (&actor_id, &damage) in actor_damage {
        let uid = summon_resolver::resolve(actor_id, summon_data);
        if uid <= 0 { continue; }
        let nickname = resolve_nickname(uid, nickname_data, summon_data);
        *nickname_damage.entry(nickname).or_default().entry(uid).or_insert(0) += damage;
    }

    let mut result = HashMap::new();
    for (nickname, id_damage) in &nickname_damage {
        // Pin the local player's row to their bound id so it doesn't oscillate
        // between co-existing self-ids as damage accumulates (which made the
        // frontend re-bind and thrash the meter).
        if let Some(lid) = local_player_id {
            if id_damage.contains_key(&lid) {
                result.insert(nickname.clone(), lid);
                continue;
            }
        }
        let direct_owner = id_damage.keys().find(|&&id| nickname_data.get(&id).is_some_and(|n| n == nickname));
        let canonical = direct_owner.copied()
            .or_else(|| id_damage.iter().max_by_key(|(_, d)| *d).map(|(id, _)| *id));
        if let Some(id) = canonical {
            result.insert(nickname.clone(), id);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::damage_packet::ParsedDamagePacket;
    use std::sync::Mutex;

    fn hit(actor: i32, target: i32, at: i64) -> ParsedDamagePacket {
        let mut p = ParsedDamagePacket::new();
        p.set_actor_id(actor);
        p.set_target_id(target);
        p.set_skill_code(11010000);
        p.set_damage(500);
        p.set_timestamp(at);
        p
    }

    fn meter(storage: &Arc<DataStorage>) -> DpsCalculator {
        DpsCalculator::new(storage.clone(), Arc::new(SkillLookup::new()),
            Arc::new(NpcLookup::new()), Arc::new(PingTracker::new()))
    }

    #[test]
    fn displayed_rows_keep_their_detail_targets_until_reset() {
        let storage = Arc::new(DataStorage::new());
        storage.set_local_player_id(Some(2259));
        storage.append_damage(hit(2259, 50_000, 1_000));
        let mut calc = meter(&storage);
        let shown = calc.get_dps();
        assert_eq!(shown.detail_target_ids, vec![50_000]);
        assert!(!calc.get_target_details(50_000, Some(&[2259])).skills.is_empty());

        calc.set_target_selection_mode("trainTargets");
        let retained = calc.get_dps();
        assert_eq!(retained.target_id, 0);
        assert!(!retained.map.is_empty());
        assert_eq!(retained.detail_target_ids, vec![50_000]);

        calc.restart_target_selection(true);
        let reset = calc.get_dps();
        assert!(reset.map.is_empty());
        assert!(reset.detail_target_ids.is_empty());
    }

    #[test]
    fn all_targets_exposes_each_target_behind_the_displayed_damage() {
        let storage = Arc::new(DataStorage::new());
        storage.set_local_player_id(Some(2259));
        storage.append_damage(hit(2259, 50_000, 1_000));
        storage.append_damage(hit(2259, 60_000, 2_000));
        let mut calc = meter(&storage);
        calc.set_target_selection_mode("allTargets");
        let shown = calc.get_dps();
        assert_eq!(shown.target_id, 0);
        assert!(!shown.map.is_empty());
        assert_eq!(shown.detail_target_ids, vec![50_000, 60_000]);
    }

    /// A party of a Spiritmaster, a Cleric and two Gladiators, by class.
    fn roster(storage: &DataStorage) {
        use crate::combat::data_storage::PartyMember;
        use crate::entity::job_class::JobClass;
        let member = |slot, job| PartyMember { slot, job: Some(job), ..Default::default() };
        storage.set_party_roster(vec![
            ("TieuPhung".into(), member(1, JobClass::Elementalist)),
            ("Bong".into(), member(2, JobClass::Cleric)),
            ("Glad1".into(), member(3, JobClass::Gladiator)),
            ("Glad2".into(), member(4, JobClass::Gladiator)),
        ], true);
    }

    /// `skills` distinct skills of a class (prefix: 16 Spiritmaster, 17 Cleric,
    /// 11 Gladiator) from `actor` on `target`.
    fn rotation(storage: &DataStorage, actor: i32, target: i32, prefix: i32, skills: i32) {
        for i in 0..skills {
            let mut p = hit(actor, target, 1_000 + i as i64);
            p.set_skill_code(prefix * 1_000_000 + (i + 1) * 10_000);
            storage.append_damage(p);
        }
    }

    #[test]
    fn in_an_instance_an_actor_goes_to_the_one_party_member_of_its_class() {
        let storage = Arc::new(DataStorage::new());
        storage.set_current_dungeon(600_072);
        roster(&storage);
        rotation(&storage, 1490, 50_000, 16, 8);           // the Spiritmaster
        rotation(&storage, 5886, 50_000, 17, 8);           // the Cleric, under a new id
        rotation(&storage, 5844, 50_000, 17, 1);           // ...and its old one
        rotation(&storage, 7001, 50_000, 11, 8);           // two Gladiators
        rotation(&storage, 7002, 50_000, 11, 8);
        let mut spirit = hit(34_784, 50_000, 1_500);        // a spirit, owner never seen
        spirit.set_skill_code(16_130_004);
        storage.append_damage(spirit);
        let mut aura = hit(17_001, 50_000, 1_600);          // a Divine Aura
        aura.set_skill_code(17_150_000);
        storage.append_damage(aura);
        let mut stray = hit(119, 50_000, 1_700);            // a Gladiator-class summon
        stray.set_skill_code(11_390_000);
        storage.append_damage(stray);

        let details = meter(&storage).get_target_details(50_000, None);
        let rows: HashSet<i32> = details.skills.iter().map(|s| s.actor_id).collect();
        assert_eq!(rows, HashSet::from([1490, 5886, 7001, 7002, 119]),
            "spirit to the Spiritmaster; the old id and the aura to the Cleric; two Gladiators: unknown");
    }

    #[test]
    fn the_class_rule_stays_off_outside_an_instance_and_with_another_party() {
        // Open world: an actor of the class may be a stranger.
        let open = Arc::new(DataStorage::new());
        roster(&open);
        rotation(&open, 1490, 50_000, 16, 8);
        rotation(&open, 34_784, 50_000, 16, 1);
        let rows: HashSet<i32> = meter(&open).get_target_details(50_000, None)
            .skills.iter().map(|s| s.actor_id).collect();
        assert!(rows.contains(&34_784));

        // More players running a rotation than the party has: another party.
        let raid = Arc::new(DataStorage::new());
        raid.set_current_dungeon(600_072);
        roster(&raid);
        for (i, id) in [1490, 1491, 5886, 7001, 7002].into_iter().enumerate() {
            rotation(&raid, id, 50_000, [16, 16, 17, 11, 11][i], 8);
        }
        let rows: HashSet<i32> = meter(&raid).get_target_details(50_000, None)
            .skills.iter().map(|s| s.actor_id).collect();
        assert!(rows.contains(&1490) && rows.contains(&1491), "two Spiritmasters: both kept");
    }

    const BOSS: i32 = 700;

    /// A meter whose NPC table knows one boss, and `id` spawned as it.
    fn meter_with_boss(storage: &Arc<DataStorage>, ids: &[i32]) -> DpsCalculator {
        let npcs = NpcLookup::new();
        npcs.load_from_json(r#"{"700":{"name":"Boss","isBoss":true}}"#);
        for &id in ids {
            storage.append_mob(id, BOSS);
            storage.register_boss(id);
        }
        DpsCalculator::new(storage.clone(), Arc::new(SkillLookup::new()),
            Arc::new(npcs), Arc::new(PingTracker::new()))
    }

    /// One hit a second from `actor` on `target`, `from` to `to` inclusive.
    fn hits(storage: &DataStorage, actor: i32, target: i32, from: i64, to: i64) {
        let mut at = from;
        while at <= to {
            crate::clock::set_override(Some(at));
            storage.append_damage(hit(actor, target, at));
            at += 1_000;
        }
    }

    fn dungeon_of(records: &[FightRecord], id: &str) -> i32 {
        records.iter().find(|r| r.id == id).map(|r| r.dungeon_id).expect(id)
    }

    #[test]
    fn a_fight_outside_after_leaving_an_instance_has_no_dungeon() {
        let s = Arc::new(DataStorage::new());
        s.set_local_player_id(Some(2259));
        let saved = Arc::new(Mutex::new(Vec::new()));
        let calc = Arc::new(Mutex::new(meter_with_boss(&s, &[800, 900])));
        {
            let (calc, saved) = (calc.clone(), saved.clone());
            s.set_before_reset(move || saved.lock().unwrap().extend(calc.lock().unwrap().snapshot_boss_fights_force()));
        }
        s.set_current_dungeon(600_002);
        hits(&s, 2259, 800, 1_000, 8_000);
        crate::clock::set_override(Some(20_000));
        assert!(s.note_zone_change(), "the load out of the instance");
        assert_eq!(s.current_dungeon_id(), 0);
        hits(&s, 2259, 900, 30_000, 40_000);
        crate::clock::set_override(Some(41_000));
        let mut saved = saved.lock().unwrap().clone();
        saved.extend(calc.lock().unwrap().snapshot_boss_fights_force());
        assert_eq!(dungeon_of(&saved, "auto_800_1000"), 600_002);
        assert_eq!(dungeon_of(&saved, "auto_900_30000"), 0);
        crate::clock::set_override(None);
    }

    #[test]
    fn a_fight_before_the_first_roster_still_gets_its_dungeon() {
        let s = Arc::new(DataStorage::new());
        s.set_local_player_id(Some(2259));
        let mut calc = meter_with_boss(&s, &[800]);
        hits(&s, 2259, 800, 1_000, 3_000);
        s.set_current_dungeon(600_002);
        hits(&s, 2259, 800, 4_000, 8_000);
        crate::clock::set_override(Some(9_000));
        assert_eq!(dungeon_of(&calc.snapshot_boss_fights_force(), "auto_800_1000"), 600_002);
        crate::clock::set_override(None);
    }

    #[test]
    fn a_teleport_during_a_fight_in_an_instance_keeps_the_dungeon() {
        let s = Arc::new(DataStorage::new());
        s.set_local_player_id(Some(2259));
        let mut calc = meter_with_boss(&s, &[800]);
        s.set_current_dungeon(600_002);
        hits(&s, 2259, 800, 1_000, 5_000);
        crate::clock::set_override(Some(5_500));
        assert!(!s.note_zone_change(), "mid-fight: no combat reset");
        assert_eq!(s.current_dungeon_id(), 0, "until the roster names it again");
        hits(&s, 2259, 800, 6_000, 9_000);
        crate::clock::set_override(Some(10_000));
        assert_eq!(dungeon_of(&calc.snapshot_boss_fights_force(), "auto_800_1000"), 600_002);
        crate::clock::set_override(None);
    }

    #[test]
    fn a_fight_only_strangers_had_is_not_ours() {
        let storage = Arc::new(DataStorage::new());
        storage.append_damage(hit(11_345, 60_000, 1_000));     // a stranger alone on a boss
        let calc = meter(&storage);
        let combat = storage.get_combat_snapshot_light();
        assert!(calc.is_our_fight(&combat[&60_000]), "not knowing who we are, everything counts");

        storage.set_local_player_id(Some(2259));
        storage.append_damage(hit(2259, 50_000, 1_000));        // ours
        let combat = storage.get_combat_snapshot_light();
        assert!(calc.is_our_fight(&combat[&50_000]));
        assert!(!calc.is_our_fight(&combat[&60_000]), "a stranger's boss is not saved or uploaded");
    }

    #[test]
    fn boss_mode_shows_your_trash_mob_in_the_open_world_only() {
        let open_world = Arc::new(DataStorage::new());
        open_world.set_local_player_id(Some(2259));
        open_world.append_damage(hit(2259, 50_000, 1_000));
        // A stranger alone on a bigger fight of their own stays off the meter.
        for t in 0..5 {
            open_world.append_damage(hit(11_345, 60_000, 1_000 + t));
        }
        let shown = meter(&open_world).get_dps();
        assert_eq!(shown.target_id, 50_000);
        assert_eq!(shown.map.keys().copied().collect::<Vec<_>>(), vec![2259]);

        // In a dungeon, a mob that is not a boss is not shown at all.
        let dungeon = Arc::new(DataStorage::new());
        dungeon.set_local_player_id(Some(2259));
        dungeon.set_current_dungeon(600_011);
        dungeon.append_damage(hit(2259, 50_000, 1_000));
        let shown = meter(&dungeon).get_dps();
        assert_eq!(shown.target_id, 0);
        assert!(shown.map.is_empty());
    }
}

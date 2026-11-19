//! A saved fight's summary for History: what its list, filters and personal
//! bests need, read once per fight file (`fight_history`'s index).

use std::collections::HashSet;
use std::sync::LazyLock;

use crate::entity::fight_record::{FightRecord, FightSummary};

/// The game's final bosses and raid instances (`src/data/dungeon_bosses.json`).
struct DungeonBosses {
    finals: HashSet<i32>,
    raids: HashSet<i32>,
    tiers: std::collections::HashMap<i32, u8>,
}

static DUNGEON_BOSSES: LazyLock<DungeonBosses> = LazyLock::new(|| {
    let data: serde_json::Value =
        serde_json::from_str(include_str!("../../../src/data/dungeon_bosses.json")).unwrap_or_default();
    let ids = |key: &str| -> HashSet<i32> {
        data[key].as_array().into_iter().flatten().filter_map(|v| v.as_i64()).map(|v| v as i32).collect()
    };
    let tiers = data["tiers"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(id, tier)| Some((id.parse().ok()?, tier.as_u64()? as u8)))
        .collect();
    DungeonBosses { finals: ids("finalBosses"), raids: ids("raids"), tiers }
});

/// Above this, a max HP that came from live readings (not the spawn record)
/// may be short of the boss's real HP: readings past it are refused (#70).
const READING_CAP: i64 = 100_000_000;

pub fn of(record: &FightRecord) -> FightSummary {
    let me = you(record);
    let my_damage = me
        .map(|a| record.details.skills.iter().filter(|s| s.actor_id == a.actor_id).map(|s| s.dmg as i64).sum())
        .unwrap_or(0);
    FightSummary {
        id: record.id.clone(),
        boss_name: record.boss_name.clone(),
        target_id: record.target_id,
        start_time_ms: record.start_time_ms,
        duration_ms: record.duration_ms,
        total_damage: record.total_damage,
        world_boss_observed_damage: record.world_boss_observed_damage,
        jobs: record.jobs.clone(),
        job_ids: record.job_ids.clone(),
        is_train: record.is_train,
        is_live: false,
        app_version: record.app_version.clone(),
        mob_code: record.mob_code,
        dungeon_id: record.dungeon_id,
        member_jobs: record.member_jobs(),
        my_name: me.map(|a| a.nickname.clone()).unwrap_or_default(),
        my_job: me.map(|a| a.job.clone()).unwrap_or_default(),
        my_server: me.map(|a| a.server_id).filter(|&s| s != 0).unwrap_or(record.server_id),
        my_damage,
        my_certain: record.local_actor_id.is_some() || record.dungeon_id != 0,
        outcome: outcome(record).to_string(),
        final_boss: DUNGEON_BOSSES.finals.contains(&record.mob_code),
        raid: DUNGEON_BOSSES.raids.contains(&record.dungeon_id),
        dungeon_tier: DUNGEON_BOSSES.tiers.get(&record.dungeon_id).copied().unwrap_or(0),
    }
}

/// You in the fight: the actor it recorded as you, else (fights saved before
/// 2.0.87) the one name kept unmasked, as `obscure_nickname` masks everyone
/// else's.
fn you(record: &FightRecord) -> Option<&crate::entity::details_context::DetailsActorSummary> {
    if let Some(id) = record.local_actor_id {
        return record.actors.iter().find(|a| a.actor_id == id);
    }
    let mut unmasked = record.actors.iter().filter(|a| {
        !a.nickname.is_empty() && !a.nickname.contains('*') && !a.nickname.chars().all(|c| c.is_ascii_digit() || c == '#')
    });
    let first = unmasked.next()?;
    unmasked.next().is_none().then_some(first)
}

/// "kill", "wipe" or "unconfirmed". A fight that recorded its kill says so.
/// Before that (2.0.87), from the damage dealt against the boss's max HP:
/// all of it, where that HP can be trusted (under `READING_CAP`), is a kill;
/// under `NEAR` of it a wipe; between, unconfirmed (kills read 96.7% and
/// 98.9%, a few hits missed: Ultimate Berk, Furious Feruk).
fn outcome(record: &FightRecord) -> &'static str {
    match record.killed {
        Some(true) => "kill",
        Some(false) => "wipe",
        None => {
            let max_hp = record.details.max_hp as i64;
            let dealt = (record.total_damage as i64).max(record.details.total_target_damage as i64);
            if max_hp <= 0 {
                "unconfirmed"
            } else if dealt * 100 < max_hp * NEAR {
                "wipe"
            } else if dealt >= max_hp && max_hp < READING_CAP {
                "kill"
            } else {
                "unconfirmed"
            }
        }
    }
}

/// Percent of the boss's HP under which a fight without its kill recorded
/// is taken for a wipe.
const NEAR: i64 = 95;

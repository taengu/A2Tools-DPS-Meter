use serde::{Deserialize, Serialize};

use super::details_context::{DetailsActorSummary, TargetDetailsResponse};

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FightRecord {
    pub id: String,
    /// Display name (for backward compat). New files also have mob_code for i18n resolution.
    pub boss_name: String,
    pub target_id: i32,
    pub start_time_ms: i64,
    pub duration_ms: i64,
    pub total_damage: i32,
    /// Job class prefix IDs (e.g. [11, 14, 17]) for language-independent storage.
    pub jobs: Vec<String>,
    /// Job class prefix IDs for i18n resolution (new field).
    #[serde(default)]
    pub job_ids: Vec<i32>,
    pub details: TargetDetailsResponse,
    pub actors: Vec<DetailsActorSummary>,
    #[serde(default)]
    pub is_train: bool,
    #[serde(default)]
    pub app_version: String,
    /// NPC mob type code for i18n boss name resolution (new field).
    #[serde(default)]
    pub mob_code: i32,
    /// The instance this was fought in, identifying both the dungeon and its
    /// difficulty tier (Ferocious Horn Den is 600091/600092/600093 for
    /// Exploration / Conquest [Normal] / Conquest [Hard]). 0 in the open world.
    ///
    /// Already parsed from the party roster packet and kept in `DataStorage`;
    /// recorded here so a shared fight can say which tier it was, and so
    /// leaderboards do not rank a Normal clear against a Hard one.
    #[serde(default)]
    pub dungeon_id: i32,
    /// The recording player's home server (`1304` = Europe, Kaisinel), else
    /// their party's; 0 when the capture never said. Its digits name the
    /// region, which a2tools.app groups uploaded logs by.
    #[serde(default)]
    pub server_id: u16,
    /// The buffs and debuffs on the fight's actors and its target
    /// (`combat::fight_buffs`). `None` in a fight saved before the meter
    /// recorded them, and in one derived from an Evidence Slice (which holds
    /// no abnormal records), so neither the files of older fights nor the
    /// log service's output change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffs: Option<Vec<crate::combat::fight_buffs::BuffTrack>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FightSummary {
    pub id: String,
    pub boss_name: String,
    pub target_id: i32,
    pub start_time_ms: i64,
    pub duration_ms: i64,
    pub total_damage: i32,
    pub jobs: Vec<String>,
    #[serde(default)]
    pub job_ids: Vec<i32>,
    #[serde(default)]
    pub is_train: bool,
    #[serde(default)]
    pub is_live: bool,
    #[serde(default)]
    pub app_version: String,
    #[serde(default)]
    pub mob_code: i32,
    /// The instance it was fought in (0 in the open world), so History can
    /// group fights by dungeon.
    #[serde(default)]
    pub dungeon_id: i32,
    /// One class per party member who fought, so History shows two icons
    /// for two Clerics where `jobs` has one.
    #[serde(default)]
    pub member_jobs: Vec<String>,
    /// Whether it may be uploaded (FightRecord::is_uploadable), so History
    /// offers the button only where an upload would be accepted.
    #[serde(default = "uploadable_default")]
    pub uploadable: bool,
}

fn uploadable_default() -> bool {
    true
}

/// An open-world boss below this much HP is a quest boss: the NPC table marks
/// them as bosses, but they die in seconds to one player, and a2tools.app
/// does not take them as logs. Instances are not held to it: Nightmare's and
/// the Ascension Trials' bosses can be smaller and are real fights.
pub const OPEN_WORLD_MIN_HP: i64 = 5_000_000;

impl FightRecord {
    /// Whether this fight may be uploaded: not a training dummy, and not an
    /// open-world quest boss (dungeon 0, max HP known and under
    /// OPEN_WORLD_MIN_HP). A fight with no HP reading is let through.
    pub fn is_uploadable(&self) -> bool {
        let hp = self.details.max_hp as i64;
        !self.is_train && !(self.dungeon_id == 0 && hp > 0 && hp < OPEN_WORLD_MIN_HP)
    }

    /// Each player's class, one entry per player. With a party roster, only
    /// its members: a summon or aura that was never tied to its owner stays
    /// in `actors` with its owner's class and no roster identity. Without
    /// one, every classed actor not named by a bare id.
    pub fn member_jobs(&self) -> Vec<String> {
        let classed = self.actors.iter().filter(|a| !a.job.is_empty());
        let mut jobs: Vec<String> = if self.actors.iter().any(|a| a.dbid != 0) {
            classed.filter(|a| a.dbid != 0).map(|a| a.job.clone()).collect()
        } else {
            classed
                .filter(|a| {
                    let id_only = a.nickname.chars().all(|c| c.is_ascii_digit() || c == '*' || c == '#');
                    a.nickname.is_empty() || !id_only
                })
                .map(|a| a.job.clone())
                .collect()
        };
        jobs.sort();
        jobs
    }
}

/// Obscure a nickname for privacy: keep first char and last char, mask the middle.
/// For CJK names (2-3 chars), keep first char, mask rest.
/// The local player's name is NOT obscured.
pub fn obscure_nickname(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= 1 {
        return name.to_string();
    }
    if chars.len() == 2 {
        return format!("{}*", chars[0]);
    }
    if chars.len() == 3 {
        return format!("{}*{}", chars[0], chars[2]);
    }
    // For longer names: first 2 chars + asterisks + last char
    let mask_len = (chars.len() - 3).min(4);
    let mask: String = std::iter::repeat_n('*', mask_len).collect();
    format!("{}{}{}{}", chars[0], chars[1], mask, chars[chars.len() - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_world_quest_bosses_are_not_uploadable() {
        let fight = |dungeon: i32, hp: i32, train: bool| {
            let mut v = old_record();
            v["dungeonId"] = dungeon.into();
            v["details"]["maxHp"] = hp.into();
            v["isTrain"] = train.into();
            serde_json::from_value::<FightRecord>(v).unwrap()
        };
        assert!(!fight(0, 230_000, false).is_uploadable(), "an open-world quest boss");
        assert!(fight(0, 20_100_000, false).is_uploadable(), "a field boss");
        assert!(fight(0, 0, false).is_uploadable(), "no HP reading: let through");
        assert!(fight(200003, 230_000, false).is_uploadable(), "a small boss in an instance (Nightmare)");
        assert!(!fight(600072, 50_000_000, true).is_uploadable(), "a training dummy");
    }

    fn old_record() -> serde_json::Value {
        serde_json::json!({
            "id": "auto_1_2", "bossName": "Some Boss", "targetId": 1, "startTimeMs": 1_700_000_000_000i64,
            "durationMs": 60_000, "totalDamage": 1000, "jobs": [], "jobIds": [],
            "details": {"targetId": 1, "maxHp": 0, "totalTargetDamage": 1000, "battleTime": 60_000,
                        "startTime": 0, "skills": [], "pingHistory": [], "healSkills": []},
            "actors": [], "isTrain": false, "appVersion": "2.0.54", "mobCode": 0, "dungeonId": 0, "serverId": 0
        })
    }

    #[test]
    fn a_fight_saved_before_buffs_loads_and_saves_unchanged() {
        let record: FightRecord = serde_json::from_value(old_record()).unwrap();
        assert!(record.buffs.is_none());
        assert_eq!(serde_json::to_value(&record).unwrap(), old_record());
    }

    #[test]
    fn buffs_round_trip() {
        let mut json = old_record();
        json["buffs"] = serde_json::json!([
            {"on": 7, "id": 161900001, "by": 8, "skill": 16190000, "segs": "1000,11000,1,1", "up": 10000},
            {"on": 7, "summon": true, "id": 20, "by": 7, "passive": true, "up": 60000}
        ]);
        let record: FightRecord = serde_json::from_value(json.clone()).unwrap();
        let buffs = record.buffs.as_ref().unwrap();
        assert_eq!(buffs.len(), 2);
        assert!(buffs[1].summon && buffs[1].passive && buffs[1].segs.is_empty());
        assert_eq!(serde_json::to_value(&record).unwrap(), json);
        // Recorded, none seen: an empty list, not an older fight.
        let mut none_seen = record.clone();
        none_seen.buffs = Some(Vec::new());
        assert_eq!(serde_json::to_value(&none_seen).unwrap()["buffs"], serde_json::json!([]));
    }
}

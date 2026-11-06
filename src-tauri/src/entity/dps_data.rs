use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::personal_data::PersonalData;

fn no_stagger() -> f64 {
    -1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DpsData {
    pub map: HashMap<i32, PersonalData>,
    pub target_name: String,
    pub target_mode: String,
    pub target_id: i32,
    /// Targets represented by the displayed rows, retained while no new target is selected.
    #[serde(default)]
    pub detail_target_ids: Vec<i32>,
    pub battle_time: i64,
    pub local_player_id: Option<i64>,
    /// Max HP of the current single boss target (0 = unknown / multi-target).
    pub target_max_hp: i64,
    /// Cumulative tracked damage dealt to the current target — the fallback HP-bar
    /// source (max_hp - dealt) when no live current-HP reading is available.
    pub target_total_damage: i64,
    /// Live current HP of the current target from the in-place HP feed, or -1 if
    /// none has been observed. When >= 0 the bar uses this real value directly.
    pub target_current_hp: i64,
    /// How far the current target's stagger gauge is worn down (0-100), or -1
    /// when it has none or none has been seen: shown on the boss bar while
    /// the meter shows stagger.
    #[serde(default = "no_stagger")]
    pub target_stagger_pct: f64,
    /// The current target is a boss (not a training dummy): the page shows the
    /// time-to-kill estimate only then.
    #[serde(default)]
    pub target_is_boss: bool,
    /// The Train time limit has run out: hits on dummies no longer count
    /// until the refresh (Settings → Advanced).
    #[serde(default)]
    pub train_limit_reached: bool,
    /// The current target's NPC code (0 = none or unknown). `target_id` is
    /// the entity; the page looks names and a boss's own level up by code
    /// (Nightmare's bosses have ten codes each, one per level).
    #[serde(default)]
    pub target_mob_code: i32,
    /// Instance id from the party roster (0 = not in a party instance). The
    /// frontend maps it to a dungeon name + difficulty.
    pub dungeon_id: i32,
    /// `DataStorage::party_generation`: changes when the party's members do.
    #[serde(default)]
    pub party_generation: u64,
    /// `DataStorage::zone_generations`: every map load, and those entering
    /// or leaving an instance.
    #[serde(default)]
    pub zone_generation: u64,
    #[serde(default)]
    pub instance_generation: u64,
}

impl DpsData {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            target_name: String::new(),
            target_mode: "bossTargets".to_string(),
            target_id: 0,
            detail_target_ids: Vec::new(),
            battle_time: 0,
            local_player_id: None,
            target_max_hp: 0,
            target_total_damage: 0,
            target_current_hp: -1,
            target_stagger_pct: -1.0,
            target_is_boss: false,
            train_limit_reached: false,
            target_mob_code: 0,
            dungeon_id: 0,
            party_generation: 0,
            zone_generation: 0,
            instance_generation: 0,
        }
    }
}

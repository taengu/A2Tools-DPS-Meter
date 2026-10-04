use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::personal_data::PersonalData;

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
    /// Instance id from the party roster (0 = not in a party instance). The
    /// frontend maps it to a dungeon name + difficulty.
    pub dungeon_id: i32,
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
            dungeon_id: 0,
        }
    }
}

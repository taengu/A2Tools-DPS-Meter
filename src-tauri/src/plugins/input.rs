//! Esc and clicks in the game for plugins (permission `events:input`, meter
//! 2.0.89 on).
//!
//! A plugin asks with `a2.watchInput(zones)`: parts of the game's window, as
//! fractions of its client area. While any plugin asks, the meter watches
//! (`platform::game_input`) and tells each one that asks an "input" event:
//! `{ key: "Escape" }` for Esc, `{ zone }` (its index) for a left click in
//! one of its zones. Nothing else: no other key, no click elsewhere, no
//! position. `a2.watchInput(null)` stops; so does the plugin's window going.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Deserialize;
use serde_json::Value;

use super::{emit_plugin, require, window_plugin};
use crate::platform::game_input::{self, GameInput};

/// The most zones one plugin may watch.
const MAX_ZONES: usize = 16;

/// A part of the game's window, as fractions (0 to 1) of its client area.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Zone {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl Zone {
    fn valid(&self) -> bool {
        [self.x, self.y, self.width, self.height].iter().all(|v| (0.0..=1.0).contains(v))
            && self.width > 0.0
            && self.height > 0.0
    }

    fn holds(&self, x: f64, y: f64) -> bool {
        (self.x..self.x + self.width).contains(&x) && (self.y..self.y + self.height).contains(&y)
    }
}

/// The plugins that asked, with their zones.
static WATCHERS: Mutex<Option<HashMap<String, Vec<Zone>>>> = Mutex::new(None);

/// What a plugin with `zones` hears of `input`: Esc, or which of its zones
/// a click is in; nothing of a click outside them.
fn heard(zones: &[Zone], input: GameInput) -> Option<Value> {
    match input {
        GameInput::Escape => Some(serde_json::json!({ "key": "Escape" })),
        GameInput::Click { x, y, area } => {
            let (x, y) = (f64::from(x) / f64::from(area.width), f64::from(y) / f64::from(area.height));
            zones.iter().position(|z| z.holds(x, y)).map(|zone| serde_json::json!({ "zone": zone }))
        }
    }
}

fn tell(app: &tauri::AppHandle, input: GameInput) {
    let watchers = WATCHERS.lock().unwrap_or_else(|p| p.into_inner()).clone().unwrap_or_default();
    for (id, zones) in watchers {
        if let Some(data) = heard(&zones, input) {
            emit_plugin(app, &id, serde_json::json!({ "plugin": id, "event": "input", "data": data }));
        }
    }
}

/// Start watching with `zones` (replacing any before), or stop (`None`).
#[tauri::command]
pub fn plugin_input_watch(app: tauri::AppHandle, window: tauri::WebviewWindow, plugin: Option<String>, zones: Option<Vec<Zone>>) -> Result<(), String> {
    let id = window_plugin(&window, plugin.as_deref())?;
    require(&app, &id, "events:input")?;
    if zones.as_ref().is_some_and(|z| z.len() > MAX_ZONES || !z.iter().all(Zone::valid)) {
        return Err(format!("up to {MAX_ZONES} zones, each with x, y, width and height from 0 to 1"));
    }
    let handle = app.clone();
    game_input::start(move |input| tell(&handle, input));
    let mut watchers = WATCHERS.lock().unwrap_or_else(|p| p.into_inner());
    let set = watchers.get_or_insert_with(HashMap::new);
    match zones {
        Some(zones) => {
            set.insert(id, zones);
        }
        None => {
            set.remove(&id);
        }
    }
    game_input::set_on(!set.is_empty());
    Ok(())
}

/// A plugin's window has gone, or it was turned off: it watches no more.
pub fn forget_plugin(id: &str) {
    if let Some(set) = WATCHERS.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
        set.remove(id);
        game_input::set_on(!set.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::game_input::Area;

    #[test]
    fn a_plugin_hears_esc_and_clicks_in_its_zones_only() {
        let zones = [Zone { x: 0.95, y: 0.0, width: 0.05, height: 0.075 }, Zone { x: 0.05, y: 0.0, width: 0.1, height: 0.06 }];
        let area = Area { x: 1920, y: 0, width: 2560, height: 1440 };
        assert_eq!(heard(&zones, GameInput::Escape), Some(serde_json::json!({ "key": "Escape" })));
        assert_eq!(heard(&zones, GameInput::Click { x: 2510, y: 50, area }), Some(serde_json::json!({ "zone": 0 })));
        assert_eq!(heard(&zones, GameInput::Click { x: 300, y: 40, area }), Some(serde_json::json!({ "zone": 1 })));
        assert_eq!(heard(&zones, GameInput::Click { x: 1280, y: 50, area }), None);
        assert_eq!(heard(&[], GameInput::Click { x: 2510, y: 50, area }), None);
        assert!(!Zone { x: 0.5, y: 0.5, width: 0.0, height: 0.1 }.valid());
        assert!(!Zone { x: f64::NAN, y: 0.0, width: 0.1, height: 0.1 }.valid());
        assert!(!Zone { x: 0.0, y: 0.0, width: 1.5, height: 0.1 }.valid());
    }
}

//! No game window to find or watch here: no area, and no Esc or clicks.
use crate::platform::game_input::{Area, GameInput};

pub fn game_area() -> Option<Area> {
    None
}

pub fn watch_while(_on_input: &dyn Fn(GameInput), _on: &dyn Fn() -> bool) -> Result<(), String> {
    Err("not on this system".into())
}

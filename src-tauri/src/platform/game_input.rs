//! Esc pressed and left clicks in AION 2, for `events:input` (plugins/input.rs):
//! only while the game is in front, only while a plugin asks, and nothing is
//! taken from the game: it gets every key and click as before. Shops and
//! windows the game closes without telling the server (the Black Cloud
//! Merchant's) can be followed this way.
//!
//! Watching is the OS implementation's (`watch_while`), and so is where the
//! game's window is (`game_area`). Here, the thread it runs on: started the
//! first time a plugin asks, and waiting, the OS watch stopped (no polling,
//! no X connection), while none does.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

pub use super::os::game_input::game_area;

/// The game window's client area on screen, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameInput {
    /// Esc went down.
    Escape,
    /// The left (primary) button went down at `x`, `y` in the game's client
    /// area, which is `area`.
    Click { x: i32, y: i32, area: Area },
}

/// Whether a plugin asks.
static ON: AtomicBool = AtomicBool::new(false);
/// The watch's thread, once started (None: it could not start).
static THREAD: OnceLock<Option<std::thread::Thread>> = OnceLock::new();

fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Watch while `set_on(true)`, handing each Esc and click to `on_input`.
/// Starts the thread the first time; later calls keep the first `on_input`.
pub fn start(on_input: impl Fn(GameInput) + Send + 'static) {
    THREAD.get_or_init(|| {
        let spawned = std::thread::Builder::new().name("game-input".into()).spawn(move || {
            loop {
                while !on() {
                    std::thread::park();
                }
                if let Err(e) = super::os::game_input::watch_while(&on_input, &on) {
                    tracing::info!("Game input: none from the game: {e}");
                    return;
                }
            }
        });
        match spawned {
            Ok(handle) => Some(handle.thread().clone()),
            Err(e) => {
                tracing::warn!("Game input: cannot start its thread: {e}");
                None
            }
        }
    });
}

/// Whether some plugin asks: the watch runs only then.
pub fn set_on(on: bool) {
    ON.store(on, Ordering::Relaxed);
    if let (true, Some(Some(thread))) = (on, THREAD.get()) {
        thread.unpark();
    }
}

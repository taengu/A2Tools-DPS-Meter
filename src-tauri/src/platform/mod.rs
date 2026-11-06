//! Everything that depends on the operating system, and nothing else.
//!
//! **The rule:** `#[cfg(windows)]` / `#[cfg(target_os = ...)]` appear in this
//! module and in `Cargo.toml`, and nowhere else in `src/`. CI enforces it. The
//! rest of the meter calls `platform::...` and never asks which OS it is on.
//!
//! Exactly one OS implementation is compiled in, chosen below. Each lives in its
//! own folder and exposes the **same modules, functions and signatures**:
//!
//! - `win32/` — Windows, the platform the meter ships on.
//! - `linux/` — Linux, for players running the game under Proton. Partly
//!   ported; what is not ported yet is borrowed from `unsupported/`.
//! - `unsupported/` — every other target. It compiles, and does the safe
//!   nothing: no capture library, no screenshots, no hotkeys, no stored token.
//!
//! OS-neutral helpers that only *support* platform code (the screenshot rect
//! maths and PNG encoder, hotkey-label parsing, the meter anchor's placement
//! maths) live beside this file and re-export the OS half, so callers have one
//! path either way.
//!
//! The folder is `win32`, not `windows`: a local module named `windows` would
//! shadow the `windows` crate the Windows code is written against.

#[cfg(windows)]
#[path = "win32/mod.rs"]
mod os;

#[cfg(target_os = "linux")]
#[path = "linux/mod.rs"]
mod os;

#[cfg(not(any(windows, target_os = "linux")))]
#[path = "unsupported/mod.rs"]
mod os;

pub mod game_input;
pub mod hotkeys;
pub mod kwin_rules;
pub mod meter_anchor;
pub mod procfs;
pub mod screenshot;

pub use os::{
    admin, clock, dialog, discord_ipc, fps, pcap, process, secret, shell, updater, window, window_detector, window_rules, window_startup,
};

pub use os::atomic_file;

/// Which web process a window's page shares, where the OS gives each window
/// its own unless told otherwise (Linux: `window::share_process`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessGroup {
    /// The meter and its own windows: Settings, History, Details.
    Meter,
    /// The plugin dock, plugin windows and their widgets, apart from the
    /// meter's, so a busy plugin never stalls the meter.
    Plugins,
}

/// The game's frame rate, as `fps::game_fps` reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FpsReading {
    /// Frames per second (0 while the game presents none, minimised say).
    Fps(u32),
    /// The game isn't open.
    NoGame,
    /// Counting needs the meter to run as administrator.
    NeedsAdmin,
    /// Not on this OS.
    Unsupported,
}

/// Why `secret::unprotect` could not give a stored secret back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsealError {
    /// The store did not answer, stayed locked, or does not show the item
    /// now. The same data may unseal later.
    Unavailable,
    /// The data can never be unsealed: not ours, or sealed for another user.
    Invalid,
}

/// The installer the update manifest names for each kind of install: the MSI
/// for Windows, and on Linux one package per package manager. Each platform's
/// `updater::package_url` picks the one it can install ("" where the manifest
/// has none).
#[derive(Debug, Default, Clone, Copy)]
pub struct UpdatePackages<'a> {
    pub msi: &'a str,
    pub arch: &'a str,
    pub deb: &'a str,
    pub rpm: &'a str,
}

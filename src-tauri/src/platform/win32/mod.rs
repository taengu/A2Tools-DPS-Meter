//! The Windows implementation. Every module here has a twin with the same
//! signatures in `../unsupported/` (and, later, `../linux/`).

#[cfg(test)]
mod atomic_file_tests;

pub mod admin;
pub mod atomic_file;
pub mod clock;
pub mod dialog;
pub mod discord_ipc;
pub mod fps;
pub mod hotkeys;
mod installer;
pub mod pcap;
pub mod process;
pub mod screen;
pub mod secret;
mod system_paths;
pub mod shell;
pub mod updater;
pub mod window;
pub mod window_detector;
#[path = "../unsupported/window_rules.rs"]
pub mod window_rules;
#[path = "../unsupported/window_startup.rs"]
pub mod window_startup;

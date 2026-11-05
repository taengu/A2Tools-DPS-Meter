//! The Linux implementation, for players running AION 2 under Proton. Same
//! modules and signatures as `../win32/`.
//!
//! Ported so far: packet capture (libpcap), finding the game (/proc), the
//! capture-permission check, the account token (the desktop keyring), dialogs
//! and the folder picker (GTK), screenshots (WebKit's own snapshot), updates
//! (pacman, apt, dnf, zypper), the clock Wine's QueryPerformanceCounter runs
//! on, and keeping a saved file's owner, mode and ACL. The rest still comes
//! from `../unsupported/` and does the safe nothing until it is ported here.

pub mod admin;
pub mod atomic_file;
pub mod clock;
pub mod dialog;
pub mod discord_ipc;
/// No frame record to read: under Proton the frames go through Vulkan.
#[path = "../unsupported/fps.rs"]
pub mod fps;
pub mod game_input;
mod file_permissions;
mod focus;
pub mod pcap;
pub mod process;
pub mod screen;
pub mod secret;
pub mod updater;
pub mod window;
pub mod window_detector;
pub mod window_rules;
pub mod window_startup;

pub mod hotkeys;
#[path = "../unsupported/shell.rs"]
pub mod shell;

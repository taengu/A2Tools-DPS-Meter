//! Which packet-capture library to load. The API is libpcap's on every OS;
//! on Windows it comes from Npcap.

use std::path::Path;

/// The library `capture::pcap_capturer` loads at runtime (the first of these
/// that loads).
pub const LIBRARIES: &[&str] = &["wpcap.dll"];

/// What to tell the player when it will not load.
pub const MISSING_HELP: &str = "Is Npcap installed? Download from https://npcap.com";

/// The meter can fetch and run Npcap's installer for the player.
pub const OFFERS_INSTALL: bool = true;

/// Whether a read returns at its timeout on a device with no traffic: Npcap's
/// does.
pub const READ_TIMES_OUT_WHEN_IDLE: bool = true;

/// Whether a filter can change while capturing without losing what the
/// kernel has queued. Not here: Npcap empties its buffer on a new filter
/// (BIOCSETF), so a lock that narrowed the filter would cut the game's
/// stream right where it was found. The filter is set once, when the device
/// opens, and kept (`pcap_capturer`).
pub const FILTER_CHANGE_KEEPS_QUEUE: bool = false;

/// Load only a system Npcap installation, including its dependencies. Neither
/// the application's directory, the current directory nor PATH is searched.
pub fn load_library() -> Result<libloading::Library, String> {
    use libloading::os::windows::{Library, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32};
    let system = super::system_paths::system_directory()?;
    let mut errors = Vec::new();
    for path in [system.join("Npcap").join("wpcap.dll"), system.join("wpcap.dll")] {
        // SAFETY: an absolute OS-owned path, with dependency search restricted
        // to that DLL's own directory and the Windows system directory.
        match unsafe { Library::load_with_flags(&path, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32) } {
            Ok(library) => return Ok(library.into()),
            Err(error) => errors.push(format!("{}: {error}", path.display())),
        }
    }
    Err(errors.join("; "))
}

/// Whether the capture library can be loaded at all.
pub fn library_available() -> bool {
    load_library().is_ok()
}

/// Run Npcap's installer and wait until it closes. It needs administrator
/// rights, so Windows is asked to elevate the trusted Windows helper. That
/// helper checks the stored copy's digest before starting verified Npcap.
pub fn run_installer(installer: &Path) -> Result<(), String> {
    let guarded = super::installer::LockedFile::open(installer)?;
    super::installer::verify_npcap(&guarded)?;
    let hash = guarded.sha256()?;
    super::updater::run_npcap_installer(guarded.path(), &hash)
}

/// Devices not worth opening on this OS. None here.
pub fn skip_device(_name: &str) -> bool {
    false
}

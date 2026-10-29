//! libpcap, as nearly every distribution ships it.

/// Tried in order. Distributions disagree on the soname: Arch and Fedora ship
/// `libpcap.so.1`, while Debian and Ubuntu keep the old `libpcap.so.0.8` and
/// have no `.so.1` at all (only the -dev package adds a bare `libpcap.so`).
/// Loading `.so.1` alone left the meter unable to capture on every
/// Debian-family system.
pub const LIBRARIES: &[&str] = &["libpcap.so.1", "libpcap.so.0.8", "libpcap.so"];

pub const MISSING_HELP: &str = "Install libpcap (e.g. `sudo apt install libpcap0.8`), then let the meter \
capture without root: `sudo setcap cap_net_raw=ep <path to the meter>`";

/// The capture library comes from the distribution's packages, not the meter.
pub const OFFERS_INSTALL: bool = false;

/// Whether a read returns at its timeout on a device with no traffic. Not
/// here: Linux's packet ring (TPACKET_V3) starts the timeout only once a
/// packet has arrived, so a quiet device's read blocks until one does, and
/// its capture filter could not change meanwhile (`pcap_capturer`).
pub const READ_TIMES_OUT_WHEN_IDLE: bool = false;

/// Whether a filter can change while capturing without losing what the
/// kernel has queued: libpcap runs a new filter over the packets already in
/// the ring rather than dropping them.
pub const FILTER_CHANGE_KEEPS_QUEUE: bool = true;

/// Nothing to set up before loading libpcap.
pub fn load_library() -> Result<libloading::Library, String> {
    let mut errors = Vec::new();
    for name in LIBRARIES {
        // SAFETY: libpcap is loaded from the system loader's library paths.
        match unsafe { libloading::Library::new(name) } {
            Ok(library) => return Ok(library),
            Err(error) => errors.push(format!("{name}: {error}")),
        }
    }
    Err(errors.join("; "))
}

pub fn run_installer(_installer: &std::path::Path) -> Result<(), String> {
    Err("the meter does not install libpcap; the distribution's packages do".into())
}

pub fn library_available() -> bool {
    load_library().is_ok()
}

/// Skip libpcap's pseudo-devices. "any" sees every packet a second time on top
/// of the real interface it arrived on; the rest carry no IP traffic.
///
/// Container networking too: Docker and Podman give every container a `veth`
/// pair and every network a `docker0` / `br-<id>` / `podman` bridge. The game,
/// under Proton, talks through the real interface, so these only add capture
/// threads (one player had 22 of them).
pub fn skip_device(name: &str) -> bool {
    name == "any"
        || ["nflog", "nfqueue", "usbmon", "dbus", "bluetooth", "veth", "docker", "br-", "podman"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

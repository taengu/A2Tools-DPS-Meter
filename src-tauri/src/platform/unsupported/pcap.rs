/// The conventional libpcap soname, so a port has a starting point; capture is
/// not supported here yet.
pub const LIBRARIES: &[&str] = &["libpcap.so.1"];

pub const MISSING_HELP: &str = "Packet capture is not supported on this platform yet.";

/// Capture is not supported here, so there is nothing to install.
pub const OFFERS_INSTALL: bool = false;

pub const READ_TIMES_OUT_WHEN_IDLE: bool = false;

pub const FILTER_CHANGE_KEEPS_QUEUE: bool = false;

pub fn load_library() -> Result<libloading::Library, String> {
    Err(MISSING_HELP.into())
}

pub fn run_installer(_installer: &std::path::Path) -> Result<(), String> {
    Err("packet capture is not supported on this platform yet".into())
}

pub fn library_available() -> bool {
    false
}

/// Devices not worth opening on this OS. None here.
pub fn skip_device(_name: &str) -> bool {
    false
}

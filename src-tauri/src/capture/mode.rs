//! An explicit opt-in for a dedicated machine receiving mirrored game traffic.
//! The normal mode remains non-promiscuous and requires a local game window.

pub const SETTING_KEY: &str = "dpsMeter.mirroredCapture";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptureMode {
    #[default]
    Local,
    Mirrored,
}

impl CaptureMode {
    pub fn from_setting(value: Option<&str>) -> Self {
        if value == Some("true") { Self::Mirrored } else { Self::Local }
    }

    pub fn is_mirrored(self) -> bool { self == Self::Mirrored }

    pub fn promiscuous(self) -> i32 { i32::from(self.is_mirrored()) }

    pub fn game_available(self, local_window: bool) -> bool {
        self.is_mirrored() || local_window
    }

    pub fn lists_device(self, has_addresses: bool) -> bool {
        self.is_mirrored() || has_addresses
    }

    /// Promiscuous capture is limited to the adapter deliberately selected by
    /// the player. A missing selection never enables it on every interface.
    pub fn captures_device(self, has_addresses: bool, label: &str, selected: Option<&str>) -> bool {
        if !self.is_mirrored() { return has_addresses; }
        selected.is_some_and(|device| !device.trim().is_empty() && device.trim() == label.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_capture_keeps_its_window_and_address_requirements() {
        for setting in [None, Some("false"), Some("TRUE"), Some("invalid")] {
            let mode = CaptureMode::from_setting(setting);
            assert_eq!(mode.promiscuous(), 0);
            assert!(!mode.game_available(false));
            assert!(mode.game_available(true));
            assert!(!mode.lists_device(false));
            assert!(!mode.captures_device(false, "SPAN", Some("SPAN")));
            assert!(mode.captures_device(true, "Ethernet", None));
        }
    }

    #[test]
    fn mirrored_capture_needs_only_the_explicit_adapter_not_a_local_game_or_ip() {
        let mode = CaptureMode::from_setting(Some("true"));
        assert_eq!(mode.promiscuous(), 1);
        assert!(mode.game_available(false));
        assert!(mode.lists_device(false));
        assert!(mode.captures_device(false, "SPAN", Some("SPAN")));
        assert!(!mode.captures_device(true, "Ethernet", Some("SPAN")));
        assert!(!mode.captures_device(false, "Ethernet [NIC-A]", Some("Ethernet [nic-a]")));
        for selection in [None, Some(""), Some("  ")] {
            assert!(!mode.captures_device(false, "SPAN", selection));
            assert!(!mode.captures_device(true, "Ethernet", selection));
        }
    }
}

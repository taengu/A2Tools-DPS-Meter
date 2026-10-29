//! Focus visibility policy, independent of native window calls.

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Action {
    Keep,
    Hide,
    Show,
}

pub(super) struct MainFocusVisibility {
    hidden_by_focus: bool,
    hide_delay: u8,
}

impl MainFocusVisibility {
    pub const fn new() -> Self {
        Self {
            hidden_by_focus: false,
            hide_delay: 0,
        }
    }

    pub fn clear(&mut self) {
        self.hidden_by_focus = false;
        self.hide_delay = 0;
    }

    /// Another hide rule ended while focus still requires the meter hidden.
    pub fn hold_hidden(&mut self) {
        self.hidden_by_focus = true;
        self.hide_delay = 0;
    }

    pub fn delay(&self) -> u8 {
        self.hide_delay
    }

    pub fn update(
        &mut self,
        enabled: bool,
        eligible: bool,
        foreground: bool,
        hidden: bool,
        wait_ticks: u8,
    ) -> Action {
        if !eligible {
            // Manual hiding and the closed/solo/combat rules own this hide.
            self.clear();
            return Action::Keep;
        }
        if !enabled || foreground {
            self.hide_delay = 0;
            let owned = std::mem::replace(&mut self.hidden_by_focus, false);
            return if owned || (enabled && hidden) {
                Action::Show
            } else {
                Action::Keep
            };
        }
        if hidden {
            self.hide_delay = 0;
            return Action::Keep;
        }
        self.hide_delay = self.hide_delay.saturating_add(1);
        if self.hide_delay >= wait_ticks {
            self.hidden_by_focus = true;
            Action::Hide
        } else {
            Action::Keep
        }
    }
}

pub(super) fn hides_surface(master: bool, covered_surface: bool, plugin_opted: bool) -> bool {
    (master && covered_surface) || plugin_opted
}

pub(super) fn may_change_cursor_style(visible: bool, minimized: bool) -> bool {
    visible && !minimized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_hides_after_the_delay_and_returns_with_the_game_or_meter() {
        let mut state = MainFocusVisibility::new();
        assert_eq!(state.update(true, true, true, false, 3), Action::Keep);
        assert_eq!(state.update(true, true, false, false, 3), Action::Keep);
        assert_eq!(state.update(true, true, false, false, 3), Action::Keep);
        assert_eq!(state.update(true, true, false, false, 3), Action::Hide);
        assert_eq!(state.update(true, true, false, true, 3), Action::Keep);
        assert_eq!(state.update(true, true, true, true, 3), Action::Show);
        assert_eq!(state.update(true, true, true, false, 3), Action::Keep);
    }

    #[test]
    fn turning_off_restores_only_a_focus_owned_hide() {
        let mut state = MainFocusVisibility::new();
        assert_eq!(state.update(true, true, false, false, 1), Action::Hide);
        assert_eq!(state.update(false, true, false, true, 3), Action::Show);
        assert_eq!(state.update(false, true, false, true, 3), Action::Keep);
        // A pre-existing hide was not caused by focus.
        assert_eq!(state.update(true, true, false, true, 3), Action::Keep);
        assert_eq!(state.update(false, true, false, true, 3), Action::Keep);
    }

    #[test]
    fn manual_and_rule_hides_are_not_restored_by_focus_or_turning_off() {
        for foreground in [false, true] {
            let mut state = MainFocusVisibility::new();
            state.hold_hidden();
            assert_eq!(
                state.update(false, false, foreground, true, 3),
                Action::Keep
            );
            assert_eq!(state.update(true, false, foreground, true, 3), Action::Keep);
            assert_eq!(state.update(false, true, foreground, true, 3), Action::Keep);
        }
        // When another rule ends while unfocused, focus takes over its hide.
        let mut state = MainFocusVisibility::new();
        state.hold_hidden();
        assert_eq!(state.update(false, true, false, true, 3), Action::Show);
    }

    #[test]
    fn a_quick_focus_return_or_disabling_resets_the_hide_delay() {
        let mut state = MainFocusVisibility::new();
        assert_eq!(state.update(true, true, false, false, 3), Action::Keep);
        assert_eq!(state.update(true, true, true, false, 3), Action::Keep);
        assert_eq!(state.delay(), 0);
        assert_eq!(state.update(true, true, false, false, 3), Action::Keep);
        assert_eq!(state.update(false, true, false, false, 3), Action::Keep);
        assert_eq!(state.delay(), 0);
        assert_eq!(state.update(true, true, false, false, 3), Action::Keep);
    }

    #[test]
    fn master_covers_all_plugin_surfaces_and_individual_options_still_work() {
        assert!(hides_surface(true, true, false));
        assert!(!hides_surface(false, true, false));
        assert!(hides_surface(false, true, true));
        assert!(!hides_surface(true, false, false));
    }

    #[test]
    fn hidden_or_minimized_windows_defer_cursor_style_changes() {
        assert!(may_change_cursor_style(true, false));
        assert!(!may_change_cursor_style(false, false));
        assert!(!may_change_cursor_style(true, true));
        assert!(!may_change_cursor_style(false, true));
    }
}

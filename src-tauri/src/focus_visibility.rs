//! Focus visibility policy, independent of native window calls.

/// Queue the observation and its commit together. A queued request carries no
/// earlier decision: settings and window state are read on the executor that
/// applies the native action, even if an older request arrives late.
pub(super) fn dispatch_latest<S: Send + 'static, E>(
    schedule: impl FnOnce(Box<dyn FnOnce() + Send>) -> Result<(), E>,
    read_latest: impl FnOnce() -> Option<S> + Send + 'static,
    commit: impl FnOnce(S) + Send + 'static,
) -> Result<(), E> {
    schedule(Box::new(move || {
        if let Some(snapshot) = read_latest() {
            commit(snapshot);
        }
    }))
}

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

    #[cfg(test)]
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
    fn queued_native_work_does_not_hold_a_worker_lock_needed_by_the_main_thread() {
        use std::sync::{Arc, Barrier, Mutex, mpsc};
        let applied = Arc::new(Mutex::new(false));
        let queued = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let (tx, rx) = mpsc::channel::<Box<dyn FnOnce() + Send>>();
        let worker = {
            let (applied, queued, release) = (applied.clone(), queued.clone(), release.clone());
            std::thread::spawn(move || {
                dispatch_latest(
                    move |task| {
                        tx.send(task).unwrap();
                        queued.wait();
                        release.wait();
                        Ok::<_, ()>(())
                    },
                    || Some(()),
                    move |()| *applied.lock().unwrap() = true,
                ).unwrap();
            })
        };
        queued.wait();
        // A focus Show can take this lock while native work is queued. Holding
        // it on the worker before dispatching a GTK operation would deadlock.
        let available = applied.try_lock().is_ok();
        release.wait();
        worker.join().unwrap();
        assert!(available, "a worker held the input lock while waiting for the main thread");
        rx.recv().unwrap()();
        assert!(*applied.lock().unwrap());
    }

    #[test]
    fn a_delayed_focus_request_cannot_commit_a_hide_after_disabling() {
        use std::sync::{Arc, Barrier, Mutex, atomic::{AtomicBool, Ordering}};
        type Task = Box<dyn FnOnce() + Send>;
        let enabled = Arc::new(AtomicBool::new(true));
        let hidden = Arc::new(AtomicBool::new(false));
        let policy = Arc::new(Mutex::new(MainFocusVisibility::new()));
        let queue = Arc::new(Mutex::new(Vec::<Task>::new()));
        let delayed = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let request = |wait: Option<(Arc<Barrier>, Arc<Barrier>)>| {
            let (enabled, hidden, policy, queue) = (enabled.clone(), hidden.clone(), policy.clone(), queue.clone());
            let window = hidden.clone();
            dispatch_latest(
                move |task| {
                    if let Some((delayed, release)) = wait {
                        delayed.wait();
                        release.wait();
                    }
                    queue.lock().unwrap().push(task);
                    Ok::<_, ()>(())
                },
                move || Some((enabled.load(Ordering::SeqCst), hidden.load(Ordering::SeqCst))),
                move |(enabled, hidden)| {
                    let action = policy.lock().unwrap().update(enabled, true, false, hidden, 1);
                    match action {
                        Action::Hide => window.store(true, Ordering::SeqCst),
                        Action::Show => window.store(false, Ordering::SeqCst),
                        Action::Keep => {}
                    }
                },
            ).unwrap();
        };
        // Hold the older callback before it reaches the main-thread queue.
        std::thread::scope(|scope| {
            scope.spawn(|| request(Some((delayed.clone(), release.clone()))));
            delayed.wait();
            enabled.store(false, Ordering::SeqCst);
            request(None);
            queue.lock().unwrap().remove(0)();
            release.wait();
        });
        queue.lock().unwrap().remove(0)();
        assert!(!hidden.load(Ordering::SeqCst), "an old request applied a stale Hide");

        // The same delayed request is harmless after a genuine option-off Show.
        enabled.store(true, Ordering::SeqCst);
        request(None);
        queue.lock().unwrap().remove(0)();
        assert!(hidden.load(Ordering::SeqCst));
        request(None);
        enabled.store(false, Ordering::SeqCst);
        request(None);
        queue.lock().unwrap().remove(1)();
        assert!(!hidden.load(Ordering::SeqCst), "disabling did not restore a focus hide");
        queue.lock().unwrap().remove(0)();
        assert!(!hidden.load(Ordering::SeqCst), "late callback hid the restored window");
    }

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

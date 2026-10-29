//! One installation warning per process, shared by every plugin and update.
//! Existing plugins do not consult this gate when they load or open.

use std::sync::{Condvar, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    Unasked,
    Prompting,
    Approved,
    Declined,
}

pub(super) struct InstallWarning {
    decision: Mutex<Decision>,
    decided: Condvar,
}

impl InstallWarning {
    pub(super) const fn new() -> Self {
        Self { decision: Mutex::new(Decision::Unasked), decided: Condvar::new() }
    }

    /// Run the native question once, without holding the state lock while it
    /// is open. Concurrent installers share its answer. A failed question is
    /// a decline too, so a waiter cannot be left blocked by a panicking host.
    pub(super) fn confirm(&self, prompt: impl FnOnce() -> bool) -> bool {
        let mut decision = self.decision.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            match *decision {
                Decision::Approved => return true,
                Decision::Declined => return false,
                Decision::Prompting => {
                    decision = self.decided.wait(decision).unwrap_or_else(|p| p.into_inner());
                }
                Decision::Unasked => {
                    *decision = Decision::Prompting;
                    break;
                }
            }
        }
        drop(decision);
        let approved = std::panic::catch_unwind(std::panic::AssertUnwindSafe(prompt)).unwrap_or(false);
        let mut decision = self.decision.lock().unwrap_or_else(|p| p.into_inner());
        *decision = if approved { Decision::Approved } else { Decision::Declined };
        self.decided.notify_all();
        approved
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier, atomic::{AtomicUsize, Ordering}};

    #[test]
    fn different_plugins_and_updates_share_one_acknowledgement() {
        let warning = InstallWarning::new();
        let prompts = AtomicUsize::new(0);
        for _operation in ["install timers", "install cooldowns", "update timers"] {
            assert!(warning.confirm(|| { prompts.fetch_add(1, Ordering::SeqCst); true }));
        }
        assert_eq!(prompts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn declining_blocks_further_installations_without_another_question() {
        let warning = InstallWarning::new();
        assert!(!warning.confirm(|| false));
        assert!(!warning.confirm(|| panic!("a declined session must not ask again")));
        assert!(!warning.confirm(|| true));
    }

    #[test]
    fn a_new_process_session_starts_with_a_new_decision() {
        let first_launch = InstallWarning::new();
        assert!(!first_launch.confirm(|| false));
        let next_launch = InstallWarning::new();
        let prompts = AtomicUsize::new(0);
        assert!(next_launch.confirm(|| { prompts.fetch_add(1, Ordering::SeqCst); true }));
        assert_eq!(prompts.load(Ordering::SeqCst), 1);
    }

    fn concurrent_decision(approved: bool) {
        let warning = Arc::new(InstallWarning::new());
        let start = Arc::new(Barrier::new(17));
        let prompts = Arc::new(AtomicUsize::new(0));
        let workers: Vec<_> = (0..16).map(|_| {
            let (warning, start, prompts) = (warning.clone(), start.clone(), prompts.clone());
            std::thread::spawn(move || {
                start.wait();
                warning.confirm(|| {
                    prompts.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    approved
                })
            })
        }).collect();
        start.wait();
        for worker in workers { assert_eq!(worker.join().unwrap(), approved); }
        assert_eq!(prompts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn concurrent_installations_share_the_pending_approval() { concurrent_decision(true); }

    #[test]
    fn concurrent_installations_share_the_pending_decline() { concurrent_decision(false); }

    #[test]
    fn a_failed_question_declines_and_does_not_prompt_again() {
        let warning = InstallWarning::new();
        assert!(!warning.confirm(|| panic!("native question failed")));
        assert!(!warning.confirm(|| true));
    }
}

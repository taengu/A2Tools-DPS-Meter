//! Keep driver loading and discovery off the desktop setup thread.
//!
//! An application exit cancels work still waiting on the availability probe.
//! Already-admitted driver operations are not interrupted.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

#[derive(Default)]
pub struct Startup {
    cancelled: AtomicBool,
}

impl Startup {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn spawn(
        self: &Arc<Self>,
        probe: impl FnOnce() -> bool + Send + 'static,
        finish: impl FnOnce(bool) + Send + 'static,
    ) -> io::Result<JoinHandle<()>> {
        let startup = self.clone();
        std::thread::Builder::new()
            .name("capture-startup".into())
            .spawn(move || {
                if startup.cancelled.load(Ordering::Acquire) {
                    return;
                }
                let available = probe();
                if !startup.cancelled.load(Ordering::Acquire) {
                    finish(available);
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn a_slow_probe_does_not_hold_the_callers_setup_thread() {
        let startup = Arc::new(Startup::default());
        let setup_thread = std::thread::current().id();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let worker = startup
            .spawn(
                move || {
                    entered_tx.send(std::thread::current().id()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                    true
                },
                move |available| finished_tx.send(available).unwrap(),
            )
            .unwrap();
        let probe_thread = entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_ne!(probe_thread, setup_thread);
        assert!(finished_rx.try_recv().is_err());
        // Reaching this caller-side send while the probe waits demonstrates
        // that setup can continue to initialize windows and service native IPC.
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        assert!(finished_rx.recv().unwrap());
    }

    #[test]
    fn an_unavailable_library_uses_the_missing_driver_path_on_the_worker() {
        let startup = Arc::new(Startup::default());
        let setup_thread = std::thread::current().id();
        let (finished_tx, finished_rx) = mpsc::channel();
        startup
            .spawn(
                || false,
                move |available| {
                    finished_tx
                        .send((available, std::thread::current().id()))
                        .unwrap();
                },
            )
            .unwrap()
            .join()
            .unwrap();
        let (available, thread) = finished_rx.recv().unwrap();
        assert!(!available);
        assert_ne!(thread, setup_thread);
    }

    #[test]
    fn cancelling_before_scheduling_does_not_probe_or_start() {
        let startup = Arc::new(Startup::default());
        startup.cancel();
        startup
            .spawn(
                || panic!("cancelled probe ran"),
                |_| panic!("cancelled startup ran"),
            )
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn cancelling_a_slow_probe_does_not_wait_or_start_capture_after_it_finishes() {
        cancelled_probe(true);
    }

    #[test]
    fn cancelling_a_slow_missing_driver_probe_does_not_offer_an_installer() {
        cancelled_probe(false);
    }

    fn cancelled_probe(available: bool) {
        let startup = Arc::new(Startup::default());
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = startup
            .spawn(
                move || {
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                    available
                },
                |_| panic!("probe result started work after exit"),
            )
            .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        startup.cancel();
        release_tx.send(()).unwrap();
        worker.join().unwrap();
    }
}

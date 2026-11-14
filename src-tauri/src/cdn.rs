//! Reading from a2tools' CDN (cdn.a2tools.app) on a poor connection.
//!
//! A connection that drops once often works a moment later: a player in Egypt
//! on an unreliable ISP got no answer, then a reset, then the file
//! (2026-11-15). So each fetch is tried a few times before it counts as
//! failed, and fetches that still fail in a row are counted: past
//! `HINT_AFTER`, the meter shows the player once what usually fixes it
//! (Cloudflare's 1.1.1.1 app; the troubleshooter on a2tools.app).

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

/// Waits before the second and third tries.
#[cfg(not(test))]
const WAITS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(3)];
#[cfg(test)]
const WAITS: [Duration; 2] = [Duration::ZERO, Duration::ZERO];
/// Fetches failed in a row (each after its tries) before the hint shows.
const HINT_AFTER: u32 = 3;

static FAILED_IN_A_ROW: AtomicU32 = AtomicU32::new(0);
static HINTED: AtomicBool = AtomicBool::new(false);

/// `op`, a whole fetch (the request and its body), tried up to three times.
/// Its own answers stay its own: a 404 it reads as "not there" is a success.
pub async fn retry<T, F, Fut>(mut op: F) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let mut last = String::new();
    for attempt in 0..=WAITS.len() {
        if attempt > 0 {
            tokio::time::sleep(WAITS[attempt - 1]).await;
        }
        match op().await {
            Ok(value) => {
                note(true);
                return Ok(value);
            }
            Err(e) => last = e,
        }
    }
    note(false);
    Err(last)
}

/// A fetch's outcome, for fetches that retry in their own way (the pairings'
/// slower schedule, the update download's resume).
pub fn note(ok: bool) {
    if ok {
        FAILED_IN_A_ROW.store(0, Ordering::Relaxed);
    } else {
        FAILED_IN_A_ROW.fetch_add(1, Ordering::Relaxed);
    }
}

/// True once a run, when `HINT_AFTER` fetches have failed in a row.
pub fn hint_due() -> bool {
    FAILED_IN_A_ROW.load(Ordering::Relaxed) >= HINT_AFTER && !HINTED.swap(true, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    // One test: the count is the whole run's, so two in parallel would mix.
    #[tokio::test(flavor = "current_thread")]
    async fn retries_until_it_works_and_hints_once_after_three_failures() {
        let mut tries = 0;
        let got = retry(|| {
            tries += 1;
            let n = tries;
            async move { if n < 3 { Err("reset".to_string()) } else { Ok(n) } }
        })
        .await;
        assert_eq!(got, Ok(3), "a fetch that fails twice then works is a success");
        assert!(!hint_due());
        for _ in 0..HINT_AFTER {
            let _: Result<(), String> = retry(|| async { Err("no answer".to_string()) }).await;
        }
        assert!(hint_due());
        assert!(!hint_due(), "once a run");
    }
}

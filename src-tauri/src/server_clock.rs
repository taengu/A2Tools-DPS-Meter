//! How far this PC's clock is from real time.
//!
//! The game server's times (a field boss's next spawn, a map's up-since) are
//! absolute, so a countdown against the PC's clock is off by however far that
//! clock has drifted: a PC 14 s slow saw bosses spawn 12 s before their
//! countdown ended. `offset_ms` is real minus local, 0 until known.
//!
//! No request of its own: the boss timer feed the meter already reads from
//! cdn.a2tools.app (`plugins::feed_file`) comes with the CDN's `Date` header,
//! the edge's clock when it answered, cached file or not. A `Date` is whole
//! seconds, so one answer only says the offset lies in a window a second plus
//! the round trip wide; the windows of the answers of the last `KEEP`
//! overlap in a much narrower one, whose middle is the offset.
//!
//! A clock changed under the meter (a sync, or the player setting it) shows
//! as the wall clock moving apart from the monotonic one: the offset and the
//! kept windows follow the jump at once.
//!
//! Plugins get it with no permission (`a2.clockOffset()`, `a2.now()`, the
//! "clock" event), as the meter's hellos carry `clock` and a change of more
//! than `TELL_MS` is told to every plugin window and widget.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use serde_json::Value;

static OFFSET_MS: AtomicI64 = AtomicI64::new(0);
static KNOWN: AtomicBool = AtomicBool::new(false);
/// The offset plugins were last told.
static TOLD_MS: AtomicI64 = AtomicI64::new(0);

/// A change smaller than this isn't worth telling plugins about.
const TELL_MS: i64 = 250;
/// Answers older than this no longer count (the PC's clock drifts).
const KEEP: Duration = Duration::from_secs(600);
/// An answer whose round trip took longer says too little.
const MAX_RTT: Duration = Duration::from_secs(2);

/// Real minus local, in ms (0 until known).
pub fn offset_ms() -> i64 {
    OFFSET_MS.load(Ordering::Relaxed)
}

/// Whether the offset is known this run.
pub fn known() -> bool {
    KNOWN.load(Ordering::Relaxed)
}

/// What plugins are told: `{ offset, known }`.
pub fn event() -> Value {
    serde_json::json!({ "offset": offset_ms(), "known": known() })
}

/// The offsets one answer allows, `[lo, hi]` in ms: the server's second
/// `date_ms` began somewhere between sending (`sent_ms`) and receiving
/// (`got_ms`), local time, and the server read its clock within that second.
fn window(date_ms: i64, sent_ms: i64, got_ms: i64) -> (i64, i64) {
    (date_ms - got_ms, date_ms + 999 - sent_ms)
}

/// The offset the windows agree on, newest first: their overlap's middle.
/// An older window that doesn't overlap the newer ones (the clock moved
/// between) is left out, with all older than it.
fn agreed(windows: impl Iterator<Item = (i64, i64)>) -> Option<i64> {
    let mut both: Option<(i64, i64)> = None;
    for (lo, hi) in windows {
        both = Some(match both {
            None => (lo, hi),
            Some((a, b)) if lo.max(a) <= hi.min(b) => (lo.max(a), hi.min(b)),
            Some(_) => break,
        });
    }
    both.map(|(lo, hi)| (lo + hi) / 2)
}

struct Answer {
    at: Instant,
    lo: i64,
    hi: i64,
}

fn answers() -> &'static Mutex<VecDeque<Answer>> {
    static ANSWERS: Mutex<VecDeque<Answer>> = Mutex::new(VecDeque::new());
    &ANSWERS
}

/// Tell every plugin window and widget the offset.
fn announce(app: &tauri::AppHandle) {
    use tauri::{Emitter, Manager};
    let data = event();
    crate::plugins::broadcast(app, "clock", data.clone());
    // A widget hears its own plugin's events only.
    for (label, window) in app.webview_windows() {
        if let Some((id, _)) = crate::plugins::widgets::parse_label(&label) {
            let msg = serde_json::json!({ "plugin": id, "event": "clock", "data": data });
            let _ = window.emit("plugin-event", msg);
        }
    }
}

/// Keep a new offset, and tell plugins when it moved by more than `TELL_MS`
/// (or is the first).
fn set(app: &tauri::AppHandle, offset: i64, why: &str) {
    let first = !KNOWN.swap(true, Ordering::Relaxed);
    OFFSET_MS.store(offset, Ordering::Relaxed);
    if first || (offset - TOLD_MS.load(Ordering::Relaxed)).abs() > TELL_MS {
        TOLD_MS.store(offset, Ordering::Relaxed);
        let side = if offset >= 0 { "slow" } else { "fast" };
        tracing::info!("Server clock: this PC is {:.1} s {side} ({why})", offset.abs() as f64 / 1000.0);
        announce(app);
    }
}

/// An answer from the CDN: its `Date` header, with the PC's clock (unix ms)
/// just before sending and just after the answer came, and the round trip.
pub fn note_answer(app: &tauri::AppHandle, date: Option<&str>, sent_ms: i64, got_ms: i64, rtt: Duration) {
    let Some(date_ms) = date.and_then(|d| chrono::DateTime::parse_from_rfc2822(d).ok()).map(|d| d.timestamp_millis()) else {
        return;
    };
    // The wall clock moved apart from the round trip: changed meanwhile.
    if rtt > MAX_RTT || ((got_ms - sent_ms) - rtt.as_millis() as i64).abs() > 50 {
        return;
    }
    let (lo, hi) = window(date_ms, sent_ms, got_ms);
    let offset = {
        let mut kept = answers().lock().unwrap_or_else(|p| p.into_inner());
        kept.retain(|a| a.at.elapsed() < KEEP);
        kept.push_front(Answer { at: Instant::now(), lo, hi });
        kept.truncate(64);
        agreed(kept.iter().map(|a| (a.lo, a.hi)))
    };
    if let Some(offset) = offset {
        set(app, offset, &format!("the CDN's clock, round trip {} ms", rtt.as_millis()));
    }
}

/// Follow the PC's clock being changed (a sync, or set by hand): the offset
/// and the kept answers shift by the jump at once.
pub fn spawn(app: tauri::AppHandle) {
    const WATCH: Duration = Duration::from_secs(5);
    const JUMP_MS: i64 = 1000;
    tauri::async_runtime::spawn(async move {
        let mut wall = crate::clock::now_ms();
        let mut mono = Instant::now();
        loop {
            tokio::time::sleep(WATCH).await;
            let (now_wall, now_mono) = (crate::clock::now_ms(), Instant::now());
            let jump = (now_wall - wall) - now_mono.duration_since(mono).as_millis() as i64;
            (wall, mono) = (now_wall, now_mono);
            if jump.abs() > JUMP_MS {
                tracing::info!("Server clock: the PC's clock moved {:+.1} s", jump as f64 / 1000.0);
                for a in answers().lock().unwrap_or_else(|p| p.into_inner()).iter_mut() {
                    (a.lo, a.hi) = (a.lo - jump, a.hi - jump);
                }
                if known() {
                    set(&app, offset_ms() - jump, "the PC's clock changed");
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_answer_is_a_second_wide() {
        // Sent at 1000 local, back at 1100; the CDN said second 15000: 13.9 to 14.999 s slow.
        assert_eq!(window(15_000, 1_000, 1_100), (13_900, 14_999));
    }

    #[test]
    fn answers_narrow_the_offset() {
        // Real offset 14 400: one answer just after second 15 000 began (local
        // 600), one just before it ended (local 1 599).
        let w1 = window(15_000, 600, 640);
        let w2 = window(15_000, 1_590, 1_620);
        let both = agreed([w2, w1].into_iter()).unwrap();
        assert!((both - 14_400).abs() <= 40, "{both}");
    }

    #[test]
    fn an_answer_from_before_a_clock_change_is_left_out() {
        let new = (14_000, 14_500);
        let old = (2_000, 2_900);
        assert_eq!(agreed([new, old].into_iter()), Some(14_250));
        assert_eq!(agreed(std::iter::empty()), None);
    }
}

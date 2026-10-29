use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::capture::captured_payload::CapturedPayload;

pub fn init_logging() {
    // Console layer: respects RUST_LOG env var, defaults to "info"
    let console_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_filter(console_filter);

    // File layer: captures ALL events (debug+) when enabled — no filtering
    // The DebugFileLayer checks DEBUG_ENABLED internally
    let file_filter = EnvFilter::new("debug");

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(DebugFileLayer.with_filter(file_filter))
        .init();
}

// ===== Debug File Logger (tracing layer) =====

static DEBUG_ENABLED: AtomicBool = AtomicBool::new(false);
static DEBUG_WRITER: Mutex<Option<DebugFileWriter>> = Mutex::new(None);

struct DebugFileWriter {
    writer: std::io::BufWriter<std::fs::File>,
    bytes_written: u64,
    path: PathBuf,
}

const MAX_DEBUG_LOG_SIZE: u64 = 5 * 1024 * 1024; // 5 MB

/// The log before the current one, kept when debug.log fills up.
const PREVIOUS_DEBUG_LOG: &str = "debug.old.log";

/// Opens debug.log for appending. A full one is moved to debug.old.log first
/// (replacing the one before), so the log always has room: a full log used to
/// stop it for good, across restarts too.
fn open_debug_log(path: &std::path::Path) -> Option<DebugFileWriter> {
    let full = std::fs::metadata(path).is_ok_and(|m| m.len() >= MAX_DEBUG_LOG_SIZE);
    if full {
        let previous = path.with_file_name(PREVIOUS_DEBUG_LOG);
        let _ = std::fs::remove_file(&previous);
        let _ = std::fs::rename(path, &previous);
    }
    let file = crate::platform::atomic_file::private(std::fs::OpenOptions::new().create(true).append(true)).open(path).ok()?;
    let bytes_written = file.metadata().map(|m| m.len()).unwrap_or(0);
    Some(DebugFileWriter { writer: std::io::BufWriter::new(file), bytes_written, path: path.to_path_buf() })
}

/// Custom tracing layer that writes debug+ events to debug.log when enabled.
struct DebugFileLayer;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for DebugFileLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if !DEBUG_ENABLED.load(Ordering::Relaxed) {
            return;
        }
        // Use try_lock to avoid deadlock if tracing is called while we hold the lock
        let mut guard = match DEBUG_WRITER.try_lock() {
            Some(g) => g,
            None => return,
        };
        if guard.as_ref().is_some_and(|l| l.bytes_written >= MAX_DEBUG_LOG_SIZE) {
            let path = guard.as_ref().map(|l| l.path.clone()).unwrap_or_default();
            *guard = None; // closed before it is moved
            *guard = open_debug_log(&path);
        }
        let logger = match guard.as_mut() {
            Some(l) => l,
            None => return,
        };

        let now = chrono::Local::now().format("%H:%M:%S%.3f");
        let level = match *event.metadata().level() {
            tracing::Level::ERROR => "ERROR",
            tracing::Level::WARN => "WARN",
            tracing::Level::INFO => "INFO",
            tracing::Level::DEBUG => "DEBUG",
            tracing::Level::TRACE => "TRACE",
        };
        let module = event.metadata().module_path().unwrap_or("");
        // Extract short module name (last segment)
        let short_module = module.rsplit("::").next().unwrap_or(module);

        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);

        let msg = shorten(visitor.0);

        let line = format!("{} {} {} - {}\n", now, level, short_module, msg);
        let len = line.len() as u64;
        if logger.writer.write_all(line.as_bytes()).is_ok() {
            logger.bytes_written += len;
            let _ = logger.writer.flush();
        }
    }
}

struct MessageVisitor(String);

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{:?}", value);
        } else if !self.0.is_empty() {
            self.0.push_str(&format!(" {}={:?}", field.name(), value));
        } else {
            self.0 = format!("{}={:?}", field.name(), value);
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_string();
        } else if !self.0.is_empty() {
            self.0.push_str(&format!(" {}={}", field.name(), value));
        } else {
            self.0 = format!("{}={}", field.name(), value);
        }
    }
}

pub fn set_debug_enabled(enabled: bool, log_dir: &std::path::Path) {
    let prev = DEBUG_ENABLED.swap(enabled, Ordering::SeqCst);
    if enabled && !prev {
        let path = log_dir.join("debug.log");
        if let Some(writer) = open_debug_log(&path) {
            {
                let mut guard = DEBUG_WRITER.lock();
                *guard = Some(writer);
            } // guard dropped before tracing
            tracing::info!("Debug file logging started: {}", path.display());
        }
    } else if !enabled && prev {
        {
            let mut guard = DEBUG_WRITER.lock();
            *guard = None;
        } // guard dropped before tracing
        tracing::info!("Debug file logging stopped");
    }
}

pub fn is_debug_enabled() -> bool {
    DEBUG_ENABLED.load(Ordering::Relaxed)
}

// ===== Raw Packet Logger =====

static PACKET_LOG_ENABLED: AtomicBool = AtomicBool::new(false);
static PACKET_LOGGER: Mutex<Option<PacketFileLogger>> = Mutex::new(None);

/// Roll over to a new capture file past this size.
///
/// Captures run about 14 MB an hour, so this is a couple of hours of play in one
/// file — enough that a fight and its lead-in are never split, and small enough
/// to attach to a bug report.
const MAX_PACKET_FILE_SIZE: u64 = 32 * 1024 * 1024;

/// How many capture files to keep. Everything older is deleted when a new
/// capture starts.
///
/// There used to be no limit at all in either direction: no cap on the file and
/// no cap on how many accumulated. A dev machine that had been running the meter
/// for a few months held 71 files and 153 MB of them, none of which anything
/// would ever have removed. Five files at 32 MB bounds that at 160 MB, which is
/// a lot to leave on someone's disk but is at least a number.
const KEEP_PACKET_FILES: usize = 5;

struct PacketFileLogger {
    writer: std::io::BufWriter<std::fs::File>,
    path: PathBuf,
    bytes_written: u64,
    log_dir: PathBuf,
}

/// Open a fresh capture file and write its header.
fn open_packet_file(log_dir: &std::path::Path) -> Option<(std::io::BufWriter<std::fs::File>, PathBuf)> {
    let now = chrono::Local::now();
    let stamp = now.format("%Y%m%d_%H%M%S");
    // A rollover can land in the same second as the file it replaces.
    let mut path = log_dir.join(format!("packets_{}.txt", stamp));
    let mut nth = 1;
    while path.exists() && nth < 100 {
        path = log_dir.join(format!("packets_{}_{}.txt", stamp, nth));
        nth += 1;
    }
    let file = crate::platform::atomic_file::private(std::fs::OpenOptions::new().create(true).write(true).truncate(true))
        .open(&path)
        .ok()?;
    let mut writer = std::io::BufWriter::new(file);
    let header = format!(
        "# Packet capture started at {}\n# Format: TIMESTAMP|STREAMKEY|HEX_DATA\n\n",
        now.format("%+")
    );
    writer.write_all(header.as_bytes()).ok()?;
    let _ = writer.flush();
    Some((writer, path))
}

/// Delete all but the newest `KEEP_PACKET_FILES` captures.
///
/// Only touches `packets_*.txt` in the app's own data directory — files this
/// module wrote. It runs when a capture *starts*, not on shutdown, so a capture
/// you meant to keep survives until you deliberately turn logging on again;
/// copy it out before then if it matters.
fn prune_packet_logs(log_dir: &std::path::Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(log_dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("packets_") && n.ends_with(".txt"))
        })
        .collect();
    if files.len() <= keep {
        return;
    }
    // Names are timestamped, so lexicographic order is chronological.
    files.sort();
    let doomed = files.len() - keep;
    for path in files.into_iter().take(doomed) {
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        match std::fs::remove_file(&path) {
            Ok(()) => tracing::info!(
                "Removed old packet capture {} ({} bytes)",
                path.display(),
                bytes
            ),
            Err(e) => tracing::warn!("Could not remove {}: {e}", path.display()),
        }
    }
}

pub fn set_packet_log_enabled(enabled: bool, log_dir: &std::path::Path) {
    let prev = PACKET_LOG_ENABLED.swap(enabled, Ordering::SeqCst);
    if enabled && !prev {
        // Before opening a new one, so the new capture is never the thing pruned.
        prune_packet_logs(log_dir, KEEP_PACKET_FILES.saturating_sub(1));
        if let Some((writer, path)) = open_packet_file(log_dir) {
            {
                let mut guard = PACKET_LOGGER.lock();
                *guard = Some(PacketFileLogger {
                    writer,
                    path: path.clone(),
                    bytes_written: 0,
                    log_dir: log_dir.to_path_buf(),
                });
            }
            tracing::info!("Raw packet logging started: {}", path.display());
        }
    } else if !enabled && prev {
        let stopped_path = {
            let mut guard = PACKET_LOGGER.lock();
            let p = guard.as_ref().map(|l| l.path.display().to_string());
            *guard = None;
            p
        };
        if let Some(p) = stopped_path {
            tracing::info!("Raw packet logging stopped: {}", p);
        }
    }
}

pub fn is_packet_log_enabled() -> bool {
    PACKET_LOG_ENABLED.load(Ordering::Relaxed)
}

pub fn log_packet(cap: &CapturedPayload) {
    if !PACKET_LOG_ENABLED.load(Ordering::Relaxed) { return; }
    let mut guard = PACKET_LOGGER.lock();
    if let Some(ref mut logger) = *guard {
        // The packet's capture time, as the share ring stamps it: libpcap
        // hands packets over in 100 ms batches, so the time of writing falls
        // on that grid and hides the real spacing between packets.
        use chrono::TimeZone;
        let captured = cap.capture_time_ms().and_then(|ms| chrono::Local.timestamp_millis_opt(ms).single());
        let ts = captured.unwrap_or_else(chrono::Local::now).format("%+");
        let key = cap.stream_key();
        let hex: String = cap.data.iter().map(|b| format!("{:02X}", b)).collect();
        let line = format!("{}|{}|{}\n", ts, key, hex);
        if logger.writer.write_all(line.as_bytes()).is_ok() {
            // Flushed per packet on purpose: a capture is usually being taken
            // because something is going wrong, and the tail is the part that
            // matters when it does.
            let _ = logger.writer.flush();
            logger.bytes_written += line.len() as u64;
        }

        if logger.bytes_written >= MAX_PACKET_FILE_SIZE {
            let log_dir = logger.log_dir.clone();
            let finished = logger.path.clone();
            if let Some((writer, path)) = open_packet_file(&log_dir) {
                *logger = PacketFileLogger {
                    writer,
                    path: path.clone(),
                    bytes_written: 0,
                    log_dir,
                };
                tracing::info!(
                    "Packet capture rolled over: {} -> {}",
                    finished.display(),
                    path.display()
                );
            } else {
                // Could not open a replacement — stop rather than grow without
                // limit, which is the bug this whole path exists to fix.
                tracing::warn!("Could not roll over packet capture; stopping capture");
                PACKET_LOG_ENABLED.store(false, Ordering::SeqCst);
                *guard = None;
            }
        }
    }
}

/// Cut a long message to 240 bytes like the Kotlin version, on a character
/// boundary: names and chat in Korean are three bytes a character.
fn shorten(msg: String) -> String {
    if msg.len() <= 240 {
        return msg;
    }
    let mut cut = 237;
    while !msg.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}...", &msg[..cut])
}

#[cfg(test)]
mod packet_log_tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("a2tools-logtest-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(dir: &std::path::Path, name: &str) {
        std::fs::write(dir.join(name), b"x").unwrap();
    }

    /// A full debug.log used to stop the debug log for good, across restarts
    /// (a player's stayed silent from 2026-10-13). Now it moves to
    /// debug.old.log and a new one starts.
    #[test]
    fn a_full_debug_log_rolls_over_instead_of_going_silent() {
        let dir = temp_dir("debug-roll");
        let path = dir.join("debug.log");
        std::fs::write(&path, vec![b'a'; MAX_DEBUG_LOG_SIZE as usize]).unwrap();
        std::fs::write(dir.join(PREVIOUS_DEBUG_LOG), b"older").unwrap();

        let writer = open_debug_log(&path).expect("opened");
        assert_eq!(writer.bytes_written, 0);
        assert_eq!(std::fs::metadata(dir.join(PREVIOUS_DEBUG_LOG)).unwrap().len(), MAX_DEBUG_LOG_SIZE);
        drop(writer);

        // One with room is appended to, as before.
        std::fs::write(&path, b"some lines\n").unwrap();
        assert_eq!(open_debug_log(&path).expect("opened").bytes_written, 11);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_keeps_the_newest_and_removes_the_rest() {
        let dir = temp_dir("prune");
        for stamp in [
            "packets_20260101_000000.txt",
            "packets_20260102_000000.txt",
            "packets_20260103_000000.txt",
            "packets_20260104_000000.txt",
        ] {
            touch(&dir, stamp);
        }
        prune_packet_logs(&dir, 2);

        assert!(!dir.join("packets_20260101_000000.txt").exists());
        assert!(!dir.join("packets_20260102_000000.txt").exists());
        assert!(dir.join("packets_20260103_000000.txt").exists());
        assert!(dir.join("packets_20260104_000000.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_leaves_files_it_did_not_write() {
        let dir = temp_dir("prune-foreign");
        touch(&dir, "packets_20260101_000000.txt");
        touch(&dir, "packets_20260102_000000.txt");
        touch(&dir, "packets_20260103_000000.txt");
        // Not ours: the raw diagnostic dump, a user's notes, the debug log.
        touch(&dir, "rawpackets_20260101_000000.txt");
        touch(&dir, "my-important-capture.txt");
        touch(&dir, "debug.log");

        prune_packet_logs(&dir, 1);

        assert!(dir.join("rawpackets_20260101_000000.txt").exists());
        assert!(dir.join("my-important-capture.txt").exists());
        assert!(dir.join("debug.log").exists());
        assert!(dir.join("packets_20260103_000000.txt").exists());
        assert!(!dir.join("packets_20260101_000000.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_does_nothing_when_under_the_limit() {
        let dir = temp_dir("prune-under");
        touch(&dir, "packets_20260101_000000.txt");
        prune_packet_logs(&dir, 5);
        assert!(dir.join("packets_20260101_000000.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_capture_file_never_collides_with_an_existing_one() {
        let dir = temp_dir("collide");
        let (_w1, p1) = open_packet_file(&dir).expect("first");
        let (_w2, p2) = open_packet_file(&dir).expect("second in the same second");
        assert_ne!(p1, p2, "rolling over within one second must not reuse a path");
        assert!(p1.exists() && p2.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_long_korean_message_is_cut_between_characters() {
        let msg = format!("a{}", "가".repeat(100));
        let short = shorten(msg.clone());
        assert!(short.len() <= 240 && short.ends_with("..."));
        assert!(msg.starts_with(short.trim_end_matches("...")));
        assert_eq!(shorten("short".into()), "short");
    }
}

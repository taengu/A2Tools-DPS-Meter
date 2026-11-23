use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use tracing::info;

use crate::entity::fight_record::{FightRecord, FightSummary};

/// How many fights to keep on disk.
///
/// Records average around 73 KB — they carry a timestamp per hit — so this is
/// roughly 35 MB of history, and it is the first limit this directory has ever
/// had. Generous on purpose: someone who has been playing for months should not
/// find their history quietly truncated because a number was chosen tightly.
pub const MAX_HISTORY_FIGHTS: usize = 500;

/// The limits Settings → Advanced offers (0: no limit), and the setting.
pub const HISTORY_LIMITS: [usize; 5] = [500, 1000, 2000, 5000, 0];
pub const HISTORY_LIMIT_KEY: &str = "dpsMeter.historyLimit";

/// The limit a stored setting names: one of `HISTORY_LIMITS`, else the default.
pub fn history_limit(setting: Option<&str>) -> usize {
    setting
        .and_then(|s| if s == "unlimited" { Some(0) } else { s.parse().ok() })
        .filter(|n| HISTORY_LIMITS.contains(n))
        .unwrap_or(MAX_HISTORY_FIGHTS)
}

/// Whether `name` is a single file name: no path separator, no "..", no
/// drive prefix. Fight ids and icon keys come from the webview and become
/// file names under the app data directory.
pub fn is_plain_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\', ':', '\0']) && !name.contains("..") && !windows_reserved(name)
}

/// A name Windows keeps for a device (`CON`, `NUL`, `COM1`...), with or
/// without an extension, or one ending in a dot or space, which Windows
/// trims: neither is the file it looks like (security audit L9).
fn windows_reserved(name: &str) -> bool {
    if name.ends_with(['.', ' ']) {
        return true;
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
}

/// The file of the fight `id` in `dir`, or None for an id that isn't a plain
/// file name. The prune takes ids from inside the history files, not from
/// their names, so a file holding "id": "..\x" must not reach outside.
fn fight_file(dir: &std::path::Path, id: &str) -> Option<PathBuf> {
    is_plain_name(id).then(|| dir.join(format!("{id}.json")))
}

fn check_id(id: &str) -> Result<(), String> {
    if is_plain_name(id) {
        Ok(())
    } else {
        Err(format!("Invalid fight id: {id:?}"))
    }
}

/// Cheap fingerprint of the history directory: how many fight files there are
/// and the newest write among them. Comparing this costs a directory scan;
/// rebuilding the summaries costs reading and parsing every file.
#[derive(PartialEq, Eq)]
struct DirStamp {
    count: usize,
    latest: Option<SystemTime>,
}

/// The summaries of the fight files, kept beside the folder (`history-index.json`)
/// so a start reads only the files that are new or changed since: 500 fights
/// are some 150 MB of JSON, seconds to parse in full. Each entry stands while
/// its file's size and modified time do.
#[derive(Default, Serialize, Deserialize)]
struct SummaryIndex {
    version: u32,
    files: HashMap<String, IndexEntry>,
}

#[derive(Clone, Serialize, Deserialize)]
struct IndexEntry {
    size: u64,
    modified_ms: i64,
    summary: FightSummary,
}

/// A new version drops every entry: their summaries lack what it adds.
const INDEX_VERSION: u32 = 4;

/// Manages saving and loading fight records as JSON files.
pub struct FightHistoryManager {
    history_dir: PathBuf,
    index_path: PathBuf,
    /// The index as last read or written, so a rebuild doesn't read it again.
    index: Mutex<Option<SummaryIndex>>,
    /// Summaries are distilled from every file in `history_dir` — around 13MB
    /// of JSON parsed in full to keep twelve fields per fight, which measured
    /// at ~350ms. The frontend asks for this once per window at startup and
    /// again every 10s in each window, so the answer is cached and rebuilt only
    /// when the directory actually changes.
    cache: Mutex<Option<(DirStamp, Vec<FightSummary>)>>,
    /// Personal bests (`personal_bests.json`), kept up as the summaries change.
    bests_path: PathBuf,
    bests: Mutex<super::personal_bests::PersonalBests>,
    /// How many fights are kept (0: all), from the setting (`set_limit`).
    limit: AtomicUsize,
}

impl FightHistoryManager {
    pub fn new(app_data_dir: PathBuf) -> Self {
        let history_dir = app_data_dir.join("history");
        let _ = std::fs::create_dir_all(&history_dir);
        Self {
            history_dir,
            index_path: app_data_dir.join("history-index.json"),
            index: Mutex::new(None),
            cache: Mutex::new(None),
            bests: Mutex::new(super::personal_bests::PersonalBests::load(&app_data_dir.join("personal_bests.json"))),
            bests_path: app_data_dir.join("personal_bests.json"),
            limit: AtomicUsize::new(MAX_HISTORY_FIGHTS),
        }
    }

    /// How many fights to keep from now on (0: all of them).
    pub fn set_limit(&self, keep: usize) {
        self.limit.store(keep, Ordering::Relaxed);
    }

    fn stamp(&self) -> DirStamp {
        let mut count = 0usize;
        let mut latest: Option<SystemTime> = None;
        if let Ok(entries) = std::fs::read_dir(&self.history_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "json") {
                    count += 1;
                    if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
                        latest = Some(match latest {
                            Some(current) if current >= modified => current,
                            _ => modified,
                        });
                    }
                }
            }
        }
        DirStamp { count, latest }
    }

    fn invalidate(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            *cache = None;
        }
    }

    pub fn save_fight(&self, record: &FightRecord) -> Result<(), String> {
        check_id(&record.id)?;
        let file_path = self.history_dir.join(format!("{}.json", record.id));
        let json = serde_json::to_string_pretty(record)
            .map_err(|e| format!("Serialization error: {}", e))?;
        crate::atomic_file::write(&file_path, json.as_bytes())
            .map_err(|e| format!("Write error: {}", e))?;
        self.invalidate();
        info!("Fight saved: {}", record.id);
        // Settings → Advanced: 500 by default, up to 5000 or no limit.
        let keep = self.limit.load(Ordering::Relaxed);
        if keep > 0 {
            self.prune(keep);
        }
        Ok(())
    }

    /// Delete the oldest fights beyond `keep`.
    ///
    /// Fights are auto-saved and, until this existed, never removed: a dev
    /// machine had 327 of them and 24 MB. Records average ~73 KB because they
    /// carry a timestamp per hit, so this grows without limit for anyone who
    /// plays regularly and never opens the delete mode.
    ///
    /// Oldest by fight start time rather than file mtime, so re-saving an
    /// in-progress fight (the auto-save runs every 30s while a boss is alive)
    /// cannot make an old fight look new and push out a recent one.
    fn prune(&self, keep: usize) {
        // Cheap guard first. This runs on every auto-save, which fires every 30s
        // while a boss is alive, and the scan below reads and parses every file
        // — the ~350ms cost the summary cache exists to avoid paying twice.
        // Counting directory entries costs a stat each; parsing costs the file.
        if self.stamp().count <= keep {
            return;
        }
        // Start times from the index: parsing every file here cost seconds on
        // each save once the history was full. A fight holding a personal
        // best is kept: its "open" link stays good. The others' outcomes stay
        // counted in the bests' totals.
        let mut fights = self.list_fights();
        let over = self.stamp().count.saturating_sub(keep);
        if over == 0 {
            return;
        }
        let mut bests = self.bests.lock().unwrap_or_else(|e| e.into_inner());
        let spared = bests.fight_ids();
        fights.retain(|f| !spared.contains(&f.id));
        fights.sort_by_key(|f| f.start_time_ms);
        for f in fights.into_iter().take(over) {
            let Some(path) = fight_file(&self.history_dir, &f.id) else {
                tracing::warn!("Not pruning a fight with an invalid id: {:?}", f.id);
                continue;
            };
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    info!("Pruned old fight {}", path.display());
                    bests.archive(&f);
                }
                Err(e) => tracing::warn!("Could not prune {}: {e}", path.display()),
            }
        }
        bests.save(&self.bests_path);
        drop(bests);
        self.invalidate();
    }

    pub fn load_fight(&self, id: &str) -> Result<FightRecord, String> {
        check_id(id)?;
        let file_path = self.history_dir.join(format!("{}.json", id));
        let json = std::fs::read_to_string(&file_path)
            .map_err(|e| format!("Read error: {}", e))?;
        serde_json::from_str(&json)
            .map_err(|e| format!("Parse error: {}", e))
    }

    pub fn delete_fight(&self, id: &str) -> Result<(), String> {
        check_id(id)?;
        let file_path = self.history_dir.join(format!("{}.json", id));
        std::fs::remove_file(&file_path)
            .map_err(|e| format!("Delete error: {}", e))?;
        self.invalidate();
        let mut bests = self.bests.lock().unwrap_or_else(|e| e.into_inner());
        if bests.forget(id) {
            bests.save(&self.bests_path);
        }
        drop(bests);
        info!("Fight deleted: {}", id);
        Ok(())
    }

    pub fn list_fights(&self) -> Vec<FightSummary> {
        let stamp = self.stamp();
        if let Ok(cache) = self.cache.lock() {
            if let Some((cached, summaries)) = cache.as_ref() {
                if *cached == stamp {
                    return summaries.clone();
                }
            }
        }
        let summaries = self.read_all_summaries();
        if let Ok(mut cache) = self.cache.lock() {
            *cache = Some((stamp, summaries.clone()));
        }
        let mut bests = self.bests.lock().unwrap_or_else(|e| e.into_inner());
        if bests.update(&summaries) {
            bests.save(&self.bests_path);
        }
        summaries
    }

    /// Every personal best, the history's newest fights taken in first.
    pub fn personal_bests(&self) -> Vec<super::personal_bests::Encounter> {
        self.list_fights();
        let bests = self.bests.lock().unwrap_or_else(|e| e.into_inner());
        bests.encounters.values().cloned().collect()
    }

    /// Every fight's summary: from the index while its file is unchanged,
    /// else read from the file, and the index written back when it changed.
    fn read_all_summaries(&self) -> Vec<FightSummary> {
        let mut guard = self.index.lock().unwrap_or_else(|e| e.into_inner());
        let index = guard.get_or_insert_with(|| {
            std::fs::read(&self.index_path)
                .ok()
                .and_then(|b| serde_json::from_slice::<SummaryIndex>(&b).ok())
                .filter(|i| i.version == INDEX_VERSION)
                .unwrap_or(SummaryIndex { version: INDEX_VERSION, files: HashMap::new() })
        });
        let mut files = HashMap::new();
        let mut changed = false;
        if let Ok(entries) = std::fs::read_dir(&self.history_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.extension().is_some_and(|e| e == "json") {
                    continue;
                }
                let Some(name) = path.file_stem().and_then(|n| n.to_str()).map(str::to_string) else { continue };
                let Ok(meta) = entry.metadata() else { continue };
                let modified_ms = meta
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_millis() as i64);
                if let Some(kept) = index.files.get(&name).filter(|e| e.size == meta.len() && e.modified_ms == modified_ms) {
                    files.insert(name, kept.clone());
                    continue;
                }
                let Ok(json) = std::fs::read_to_string(&path) else { continue };
                let Ok(record) = serde_json::from_str::<FightRecord>(&json) else { continue };
                files.insert(name, IndexEntry { size: meta.len(), modified_ms, summary: super::summary::of(&record) });
                changed = true;
            }
        }
        changed |= files.len() != index.files.len();
        index.files = files;
        if changed {
            if let Ok(bytes) = serde_json::to_vec(&*index) {
                if let Err(e) = crate::atomic_file::write(&self.index_path, &bytes) {
                    tracing::warn!("History index not written: {e}");
                }
            }
        }
        let mut summaries: Vec<FightSummary> = index.files.values().map(|e| e.summary.clone()).collect();
        summaries.sort_by(|a, b| b.start_time_ms.cmp(&a.start_time_ms));
        summaries
    }

    pub fn export_fight_json(&self, record: &FightRecord) -> Result<String, String> {
        serde_json::to_string(record)
            .map_err(|e| format!("Serialization error: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_history_limit_setting_names_one_of_the_offered_limits() {
        assert_eq!(history_limit(None), 500);
        assert_eq!(history_limit(Some("5000")), 5000);
        assert_eq!(history_limit(Some("unlimited")), 0);
        // Anything else is the default: a hand-edited 3 must not delete history.
        assert_eq!(history_limit(Some("3")), 500);
        assert_eq!(history_limit(Some("lots")), 500);
    }

    #[test]
    fn ids_that_leave_the_directory_are_refused() {
        for bad in ["", "..", "../settings", "a/b", "a\\b", "C:x", "auto..1"] {
            assert!(!is_plain_name(bad), "{bad:?}");
        }
        assert!(is_plain_name("auto_36734_1759578598645"));
        assert!(is_plain_name("Skill_Icon_01.png"));

        let dir = std::env::temp_dir().join(format!("a2t-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let history = FightHistoryManager::new(dir.clone());
        std::fs::write(dir.join("keep.json"), b"{}").unwrap();
        assert!(history.delete_fight("../keep").is_err());
        assert!(dir.join("keep.json").exists());
        assert!(history.load_fight("../keep").is_err());
        // The prune's ids come from inside the files: one naming a path
        // outside the folder is never turned into a file to delete.
        for bad in [r"..\keep", "../keep", "a/b", ""] {
            assert!(fight_file(&dir, bad).is_none(), "{bad:?}");
        }
        assert_eq!(fight_file(&dir, "auto_1_2"), Some(dir.join("auto_1_2.json")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A copy of a real app data folder (A2_HISTORY_COPY, with its
    /// `history`): the first list reads every file, the next one (a new
    /// start, the index on disk) none, and both give the same summaries.
    #[test]
    fn the_index_spares_a_start_reading_every_fight() {
        let Some(dir) = std::env::var_os("A2_HISTORY_COPY").map(PathBuf::from) else { return };
        let _ = std::fs::remove_file(dir.join("history-index.json"));
        let t = std::time::Instant::now();
        let first = FightHistoryManager::new(dir.clone()).list_fights();
        let cold = t.elapsed();
        let t = std::time::Instant::now();
        let again = FightHistoryManager::new(dir.clone()).list_fights();
        let warm = t.elapsed();
        eprintln!("{} fights: {cold:?} without the index, {warm:?} with it", first.len());
        assert_eq!(serde_json::to_string(&first).unwrap(), serde_json::to_string(&again).unwrap());
        assert!(warm * 5 < cold);
    }
}

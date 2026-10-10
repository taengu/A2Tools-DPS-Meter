use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

use tracing::info;

use crate::entity::fight_record::{FightRecord, FightSummary};

/// How many fights to keep on disk.
///
/// Records average around 73 KB — they carry a timestamp per hit — so this is
/// roughly 35 MB of history, and it is the first limit this directory has ever
/// had. Generous on purpose: someone who has been playing for months should not
/// find their history quietly truncated because a number was chosen tightly.
const MAX_HISTORY_FIGHTS: usize = 500;

/// Whether `name` is a single file name: no path separator, no "..", no
/// drive prefix. Fight ids and icon keys come from the webview and become
/// file names under the app data directory.
pub fn is_plain_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\', ':', '\0']) && !name.contains("..")
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

/// Manages saving and loading fight records as JSON files.
pub struct FightHistoryManager {
    history_dir: PathBuf,
    /// Summaries are distilled from every file in `history_dir` — around 13MB
    /// of JSON parsed in full to keep twelve fields per fight, which measured
    /// at ~350ms. The frontend asks for this once per window at startup and
    /// again every 10s in each window, so the answer is cached and rebuilt only
    /// when the directory actually changes.
    cache: Mutex<Option<(DirStamp, Vec<FightSummary>)>>,
}

impl FightHistoryManager {
    pub fn new(app_data_dir: PathBuf) -> Self {
        let history_dir = app_data_dir.join("history");
        let _ = std::fs::create_dir_all(&history_dir);
        Self {
            history_dir,
            cache: Mutex::new(None),
        }
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
        self.prune(MAX_HISTORY_FIGHTS);
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
        let Ok(entries) = std::fs::read_dir(&self.history_dir) else {
            return;
        };
        let mut fights: Vec<(i64, std::path::PathBuf)> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .filter_map(|p| {
                let text = std::fs::read_to_string(&p).ok()?;
                let summary: FightSummary = serde_json::from_str(&text).ok()?;
                Some((summary.start_time_ms, p))
            })
            .collect();
        if fights.len() <= keep {
            return;
        }
        fights.sort_by_key(|(start, _)| *start);
        let doomed = fights.len() - keep;
        for (_, path) in fights.into_iter().take(doomed) {
            match std::fs::remove_file(&path) {
                Ok(()) => info!("Pruned old fight {}", path.display()),
                Err(e) => tracing::warn!("Could not prune {}: {e}", path.display()),
            }
        }
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
        summaries
    }

    fn read_all_summaries(&self) -> Vec<FightSummary> {
        let mut summaries = Vec::new();
        let entries = match std::fs::read_dir(&self.history_dir) {
            Ok(e) => e,
            Err(_) => return summaries,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                if let Ok(json) = std::fs::read_to_string(&path) {
                    if let Ok(record) = serde_json::from_str::<FightRecord>(&json) {
                        let member_jobs = record.member_jobs();
                        summaries.push(FightSummary {
                            id: record.id,
                            boss_name: record.boss_name,
                            target_id: record.target_id,
                            start_time_ms: record.start_time_ms,
                            duration_ms: record.duration_ms,
                            total_damage: record.total_damage,
                            jobs: record.jobs,
                            job_ids: record.job_ids,
                            is_train: record.is_train,
                            is_live: false,
                            app_version: record.app_version,
                            mob_code: record.mob_code,
                            dungeon_id: record.dungeon_id,
                            member_jobs,
                        });
                    }
                }
            }
        }

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
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! "Send logs to dev": the meter's debug log and the newest packet captures,
//! to a2tools.app, for debugging.
//!
//! Unlike a fight upload this sends raw packet logs: everything the game sent
//! while packet logging was on, names included. The UI asks first, and the
//! site keeps them private to the developer and deletes them after 30 days.
//!
//! Two steps, because a capture is up to 32 MB: register the report (small
//! JSON), then PUT each file gzipped. The site streams those straight to R2.
//! An account is used when the meter is signed in, but is not required.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

/// How many captures to send: the newest, which is where a problem just was.
pub const SEND_FILES: usize = 3;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendResult {
    /// What the player gives the developer to find this report.
    pub code: String,
    pub files: usize,
}

/// The newest `SEND_FILES` packet captures, newest first.
///
/// Capture names are timestamped (`packets_YYYYMMDD_HHMMSS[_N].txt`), so name
/// order is time order, which is also how the logger prunes them.
/// What a report sends: the debug log, when there is one (what the meter did:
/// windows shown and hidden, widgets opened and closed, which captures alone
/// don't say), then the newest captures, `SEND_FILES` in all (the site takes
/// three, and `debug.log` by that name).
pub fn files_to_send(app_data_dir: &Path) -> Vec<PathBuf> {
    let debug = app_data_dir.join("debug.log");
    let mut files: Vec<PathBuf> =
        std::fs::metadata(&debug).is_ok_and(|m| m.is_file() && m.len() > 0).then_some(debug).into_iter().collect();
    let room = SEND_FILES - files.len();
    files.extend(newest_captures(app_data_dir).into_iter().take(room));
    files
}

/// The most of the debug log a report sends: its newest part, under the
/// site's 40 MB a file.
const DEBUG_LOG_MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_RESPONSE: usize = 64 * 1024;

pub fn newest_captures(app_data_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(app_data_dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("packets_") && n.ends_with(".txt"))
                && std::fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false)
        })
        .collect();
    files.sort();
    files.reverse();
    files.truncate(SEND_FILES);
    files
}

pub async fn send(client: &reqwest::Client, app_data_dir: &Path) -> Result<SendResult, String> {
    let paths = files_to_send(app_data_dir);
    if paths.is_empty() {
        return Err("There are no logs to send. Turn on debug logging or packet logging, play until \
                    the problem happens, then send."
            .to_string());
    }

    // Read and compress off the async runtime: up to ~100 MB of text.
    let packed = tokio::task::spawn_blocking(move || {
        paths
            .iter()
            .map(|p| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
                let mut raw = std::fs::read(p).map_err(|e| format!("Could not read {name}: {e}"))?;
                if name == "debug.log" && raw.len() > DEBUG_LOG_MAX_BYTES {
                    raw.drain(..raw.len() - DEBUG_LOG_MAX_BYTES);
                }
                let size = raw.len();
                Ok((name, size, super::gzip(&raw)?))
            })
            .collect::<Result<Vec<(String, usize, Vec<u8>)>, String>>()
    })
    .await
    .map_err(|e| format!("Could not prepare the logs: {e}"))??;

    let token = crate::account::secret::load(app_data_dir);
    let base = crate::account::base_url();
    let files: Vec<_> = packed
        .iter()
        .map(|(name, size, _)| serde_json::json!({ "name": name, "size": size }))
        .collect();
    let mut register = client
        .post(format!("{base}/api/dev-logs"))
        .timeout(Duration::from_secs(15))
        .header("content-type", "application/json")
        .body(
            serde_json::json!({
                "appVersion": crate::entity::fight_record::APP_VERSION,
                "files": files,
            })
            .to_string(),
        );
    if let Some(token) = &token {
        register = register.header("authorization", format!("Bearer {token}"));
    }
    let response = register
        .send()
        .await
        .map_err(|e| format!("Could not reach a2tools.app: {e}"))?;
    let status = response.status();
    let reply: serde_json::Value =
        serde_json::from_str(&crate::bounded_http::text(response, MAX_RESPONSE).await?).unwrap_or_default();
    if !status.is_success() {
        return Err(server_message(&reply, status));
    }
    let code = reply["code"].as_str().unwrap_or_default().to_string();
    let upload_path = reply["uploadPath"].as_str().unwrap_or_default().to_string();
    if code.is_empty() || !is_upload_path(&upload_path) {
        return Err("Unexpected reply from a2tools.app.".to_string());
    }

    let mut sent = 0;
    for (name, _, body) in packed {
        let response = client
            .put(format!("{base}{upload_path}{name}"))
            // Up to ~10 MB per file, on whatever upload speed the player has.
            .timeout(Duration::from_secs(600))
            .header("content-type", "application/gzip")
            .body(body)
            .send()
            .await
            .map_err(|e| format!("Could not send {name}: {e}"))?;
        let status = response.status();
        if !status.is_success() {
            let reply: serde_json::Value =
                serde_json::from_str(&crate::bounded_http::text(response, MAX_RESPONSE).await?).unwrap_or_default();
            return Err(format!("{name}: {}", server_message(&reply, status)));
        }
        sent += 1;
    }
    tracing::info!("Sent {sent} packet logs to the developer as report {code}");
    Ok(SendResult { code, files: sent })
}

/// The server names where to PUT the files, appended to the API base. Only a
/// path on that host: it starts with '/', has no ".." (plain or escaped), and
/// nothing that could make the joined URL name another host.
fn is_upload_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains("..")
        && path.bytes().all(|b| b.is_ascii_graphic() && !matches!(b, b'\\' | b'@' | b'?' | b'#' | b'%'))
}

fn server_message(reply: &serde_json::Value, status: reqwest::StatusCode) -> String {
    reply["message"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| format!("a2tools.app answered {status}."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn picks_the_newest_three_non_empty_captures() {
        let dir = std::env::temp_dir().join(format!("a2t-devlogs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        touch(&dir, "packets_20261001_000000.txt", "a");
        touch(&dir, "packets_20261002_000000.txt", "b");
        touch(&dir, "packets_20261003_000000.txt", "c");
        touch(&dir, "packets_20261003_000000_1.txt", "d");
        touch(&dir, "packets_20261004_000000.txt", "");
        touch(&dir, "debug.log", "not a capture");
        touch(&dir, "credentials.dat", "never");
        let names: Vec<String> = newest_captures(&dir)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            names,
            vec![
                "packets_20261003_000000_1.txt",
                "packets_20261003_000000.txt",
                "packets_20261002_000000.txt",
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_debug_log_goes_first_then_the_newest_captures() {
        let dir = std::env::temp_dir().join(format!("a2t-devlogs-dbg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        touch(&dir, "packets_20261001_000000.txt", "a");
        touch(&dir, "packets_20261002_000000.txt", "b");
        touch(&dir, "packets_20261003_000000.txt", "c");
        let names = |dir: &Path| -> Vec<String> {
            files_to_send(dir).iter().map(|p| p.file_name().unwrap().to_string_lossy().to_string()).collect()
        };
        assert_eq!(names(&dir), vec!["packets_20261003_000000.txt", "packets_20261002_000000.txt", "packets_20261001_000000.txt"], "no debug log: captures only");
        touch(&dir, "debug.log", "what the meter did");
        assert_eq!(names(&dir), vec!["debug.log", "packets_20261003_000000.txt", "packets_20261002_000000.txt"]);
        for f in ["packets_20261001_000000.txt", "packets_20261002_000000.txt", "packets_20261003_000000.txt"] {
            std::fs::remove_file(dir.join(f)).unwrap();
        }
        assert_eq!(names(&dir), vec!["debug.log"], "debug logging alone is enough");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_path_on_the_api_host_is_used_for_uploads() {
        assert!(is_upload_path("/api/dev-logs/ABC123/"));
        for bad in [
            "",
            "api/dev-logs/",
            "//example.com/",
            "/api/../admin/",
            "/api/%2e%2e/admin/",
            "@example.com/",
            "https://example.com/",
            "/api\\dev-logs/",
            "/api/dev logs/",
        ] {
            assert!(!is_upload_path(bad), "{bad:?}");
        }
    }
}

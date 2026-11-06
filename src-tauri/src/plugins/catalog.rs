//! Reviewed plugins: the signed catalog and installing from it.
//!
//! The plugins repo (taengu/A2Tools-Plugins) publishes, on each merge, every
//! plugin's files and an `index.json` listing them with their SHA-256 hashes,
//! signed with an Ed25519 key that only the repo's release workflow holds.
//! The meter checks that signature against the key built in below, and
//! installs a file only if its hash matches the signed index, so nothing
//! from anywhere else installs.
//!
//!   https://cdn.a2tools.app/plugins/index.json       the catalog
//!   https://cdn.a2tools.app/plugins/index.json.sig   its signature, hex
//!   https://cdn.a2tools.app/plugins/<id>/<version>/<file>
//!   https://cdn.a2tools.app/plugins/<id>/<version>.a2pk  all of them, packed
//!
//! A version's pack (2.0.90 on) is one download instead of a request per
//! file; its hash is the entry's `pack`, and each file in it is checked
//! against its own hash as a file downloaded alone would be. Without a pack,
//! or when it fails, the files come down one by one as before.
//!
//! Test plugins (meter 2.0.78 on) are in a second catalog, signed the same
//! way, that older meters never read: `plugins/test/index.json` (+ `.sig`).
//! Its plugins are installed by id (Settings → Plugins, "Install a test
//! plugin"), aren't listed until installed, and update like any other.
//!
//! An install keeps the signed catalog it came from (`.proof/<id>.json` in
//! the plugins folder), and every start checks the plugin's files against
//! it again (`verify_installed`): the folder is the player's to write, so
//! anything else running as them could change a reviewed plugin, or drop in
//! one, and have it run in the meter (audit M11).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const BASE_URL: &str = "https://cdn.a2tools.app/plugins";
/// The catalog signing key's public half (hex). Its private half is the
/// `PLUGIN_SIGNING_KEY` secret of the plugins repo, and nowhere else.
const PUBLIC_KEY_HEX: &str = "d71e888cd5530b57254a4f87bf8f1861d8ddd8f7ddf45ecdba0771bb8f7d54ae";
/// Where the catalog lives, and the key that signs it. A debug build takes
/// both from `A2_PLUGIN_CATALOG` (a base URL) and `A2_PLUGIN_KEY` (hex) when
/// set, to test publishing against a local catalog; a release build never does.
fn base_url() -> String {
    #[cfg(debug_assertions)]
    if let Ok(url) = std::env::var("A2_PLUGIN_CATALOG") {
        return url.trim_end_matches('/').to_string();
    }
    BASE_URL.to_string()
}

fn public_key() -> String {
    #[cfg(debug_assertions)]
    if let Ok(key) = std::env::var("A2_PLUGIN_KEY") {
        return key;
    }
    PUBLIC_KEY_HEX.to_string()
}

/// Largest catalog, and largest plugin (all its files), the meter downloads.
const MAX_CATALOG: usize = 1024 * 1024;
const MAX_PLUGIN: usize = 8 * 1024 * 1024;
/// A plugin's files downloaded at once: one after another, each of its dozen
/// or two waited out its own round trip first (2026-10-31).
const PARALLEL_DOWNLOADS: usize = 4;
/// How long one catalog download may take, the whole body included.
const DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub format: u32,
    /// When it was built (unix seconds); none before 2026-10-30. A catalog
    /// older than one already read is refused (`check_serial`).
    #[serde(default)]
    pub serial: u64,
    pub plugins: Vec<Entry>,
}

/// A signed catalog as downloaded, its text and signature: what an install
/// keeps to check its files against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signed {
    pub index: String,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api_version: u32,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Where its source can be read.
    #[serde(default)]
    pub source: String,
    /// Each file's path in the plugin folder → its SHA-256 (hex).
    pub files: BTreeMap<String, String>,
    /// The SHA-256 (hex) of the version's pack, `<version>.a2pk`; none in
    /// catalogs before 2026-11-06.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack: Option<String>,
    /// From the test catalog (set here, not in the file).
    #[serde(default, skip_deserializing)]
    pub test: bool,
    /// What its recent versions added, newest first (its manifest's
    /// `changes`, checked by the catalog's builder). A meter before this
    /// field reads the catalog as before.
    #[serde(default)]
    pub changes: Vec<super::Change>,
    /// The signed catalog it was read from (set here, not in the file).
    #[serde(skip)]
    pub signed: Option<Arc<Signed>>,
}

/// The notes of `entry`'s versions after `from`, up to and with `to`.
pub fn changes_between(entry: &Entry, from: &str, to: &str) -> Vec<super::Change> {
    entry.changes.iter().filter(|c| newer(&c.version, from) && !newer(&c.version, to)).cloned().collect()
}



fn hex_bytes(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Check the catalog's signature with `key` (hex) and read it.
fn verify_with(key_hex: &str, body: &[u8], signature_hex: &str) -> Result<Catalog, String> {
    let key: [u8; 32] = hex_bytes(key_hex)
        .and_then(|k| k.try_into().ok())
        .ok_or("this build has no plugin catalog key")?;
    let key = VerifyingKey::from_bytes(&key).map_err(|_| "the plugin catalog key is not valid")?;
    let signature: [u8; 64] = hex_bytes(signature_hex)
        .and_then(|s| s.try_into().ok())
        .ok_or("the plugin catalog's signature is malformed")?;
    key.verify_strict(body, &Signature::from_bytes(&signature))
        .map_err(|_| "the plugin catalog's signature does not match: not installing from it")?;
    let catalog: Catalog = serde_json::from_slice(body).map_err(|e| format!("plugin catalog: {e}"))?;
    if catalog.format != 1 {
        return Err(format!("plugin catalog format {} (this meter reads 1): update the meter", catalog.format));
    }
    Ok(catalog)
}

/// A path inside a plugin folder: plain names separated by `/`, nothing that
/// could climb out of it.
fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 128
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        })
}

async fn get(http: &reqwest::Client, url: &str, cap: usize) -> Result<Vec<u8>, String> {
    // Its own time limit: the client's 30 s cut off Cooldowns' 5 MB voice
    // file on a slow connection ("error decoding response body", 2026-10-31).
    let response = http.get(url).timeout(DOWNLOAD_TIMEOUT).send().await.map_err(|e| format!("{url}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("{url}: {}", response.status()));
    }
    crate::update_check::read_capped(response, cap).await.map_err(|e| format!("{url}: {e}"))
}

/// Check a downloaded catalog and keep it, signed, with each of its entries.
fn read_signed(body: Vec<u8>, signature: Vec<u8>) -> Result<Catalog, String> {
    let signed = Signed {
        index: String::from_utf8(body).map_err(|_| "the plugin catalog is not UTF-8")?,
        signature: String::from_utf8_lossy(&signature).trim().to_string(),
    };
    let mut catalog = verify_with(&public_key(), signed.index.as_bytes(), &signed.signature)?;
    let signed = Arc::new(signed);
    for entry in &mut catalog.plugins {
        entry.signed = Some(Arc::clone(&signed));
    }
    Ok(catalog)
}

/// Where the newest catalog serials read are kept (`.proof/serials.json`),
/// set when the plugins load.
fn serials_file() -> &'static std::sync::Mutex<Option<PathBuf>> {
    static FILE: std::sync::OnceLock<std::sync::Mutex<Option<PathBuf>>> = std::sync::OnceLock::new();
    FILE.get_or_init(Default::default)
}

pub fn keep_serials_in(plugins_dir: &Path) {
    *serials_file().lock().unwrap_or_else(|p| p.into_inner()) = Some(plugins_dir.join(PROOF_DIR).join("serials.json"));
}

/// Refuse a catalog (`which`: "main" or "test") older than one read before,
/// or without a serial once one had it: an old signed index served again
/// would hold its plugins back (audit L2). A newer one is remembered.
fn check_serial(which: &str, serial: u64) -> Result<(), String> {
    let Some(file) = serials_file().lock().unwrap_or_else(|p| p.into_inner()).clone() else { return Ok(()) };
    let mut seen: BTreeMap<String, u64> =
        std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let last = seen.get(which).copied().unwrap_or(0);
    if serial < last {
        return Err(format!("the plugin catalog is older than one already read ({serial} < {last}): not using it"));
    }
    if serial > last {
        seen.insert(which.to_string(), serial);
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&file, serde_json::to_vec(&seen).unwrap_or_default());
    }
    Ok(())
}

/// Download the catalog and check its signature. The list and its signature
/// come down side by side: one after the other, they were most of the 0.8 s
/// Settings waited for its plugin list (2026-10-18).
pub async fn fetch(http: &reqwest::Client) -> Result<Catalog, String> {
    let base = base_url();
    let (list_url, signature_url) = (format!("{base}/index.json"), format!("{base}/index.json.sig"));
    let (body, signature) = tokio::join!(get(http, &list_url, MAX_CATALOG), get(http, &signature_url, 1024));
    let mut catalog = read_signed(body?, signature?)?;
    check_serial("main", catalog.serial)?;
    // The test catalog, when there is one: its plugins added (a reviewed
    // plugin of the same id wins). Missing or bad, it's left out quietly.
    let (test_list_url, test_signature_url) = (format!("{base}/test/index.json"), format!("{base}/test/index.json.sig"));
    let (test_body, test_signature) =
        tokio::join!(get(http, &test_list_url, MAX_CATALOG), get(http, &test_signature_url, 1024));
    if let (Ok(body), Ok(signature)) = (test_body, test_signature) {
        match read_signed(body, signature).and_then(|test| check_serial("test", test.serial).map(|()| test)) {
            Ok(test) => {
                for mut entry in test.plugins {
                    if catalog.plugins.iter().all(|e| e.id != entry.id) {
                        entry.test = true;
                        catalog.plugins.push(entry);
                    }
                }
            }
            Err(e) => tracing::info!("Plugins: the test catalog didn't verify: {e}"),
        }
    }
    *last_fetched().lock().unwrap_or_else(|p| p.into_inner()) = Some((std::time::Instant::now(), catalog.clone()));
    Ok(catalog)
}

/// The catalog as fetched within `max_age`, else a fresh one. For the list
/// Settings shows on every open; checking for updates and installing always
/// fetch, and what they fetch is what this hands out next.
pub async fn recent(http: &reqwest::Client, max_age: std::time::Duration) -> Result<Catalog, String> {
    let kept = last_fetched().lock().unwrap_or_else(|p| p.into_inner()).clone();
    match kept {
        Some((at, catalog)) if at.elapsed() < max_age => Ok(catalog),
        _ => fetch(http).await,
    }
}

fn last_fetched() -> &'static std::sync::Mutex<Option<(std::time::Instant, Catalog)>> {
    static LAST: std::sync::OnceLock<std::sync::Mutex<Option<(std::time::Instant, Catalog)>>> = std::sync::OnceLock::new();
    LAST.get_or_init(Default::default)
}

/// Download a catalog entry's files into `plugins_dir/<id>`, replacing what
/// was there. Every file must match its hash in the signed catalog; nothing
/// is replaced unless all of them do. `downloaded` is called once they all
/// have, as the install itself starts.
pub async fn install(http: &reqwest::Client, entry: &Entry, plugins_dir: &Path, downloaded: impl Fn()) -> Result<PathBuf, String> {
    if !super::valid_id(&entry.id) {
        return Err(format!("plugin id {:?} is not valid", entry.id));
    }
    if entry.api_version == 0 || entry.api_version > super::API_VERSION {
        return Err(format!("{} needs a newer meter (plugin API {})", entry.name, entry.api_version));
    }
    if !entry.files.contains_key("manifest.json") || !entry.files.contains_key("index.html") {
        return Err(format!("{} is missing its manifest or page", entry.name));
    }
    if let Some(path) = entry.files.keys().find(|path| !safe_path(path)) {
        return Err(format!("{}: file {path:?} is not allowed", entry.name));
    }
    let files = match download_pack(http, entry).await {
        Some(Ok(files)) => files,
        Some(Err(e)) => {
            tracing::info!("Plugins: {e}; downloading its files one by one");
            download_files(http, entry).await?
        }
        None => download_files(http, entry).await?,
    };
    downloaded();

    let staging = plugins_dir.join(format!(".staging-{}", entry.id));
    let target = plugins_dir.join(&entry.id);
    let _ = std::fs::remove_dir_all(&staging);
    for (path, bytes) in &files {
        let file = staging.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&file, bytes).map_err(|e| e.to_string())?;
    }
    let loaded = super::load_folder(&staging)?;
    // What the consent dialog showed (the catalog's permissions) is what the
    // manifest grants (audit L2).
    let mut asked = entry.permissions.clone();
    let mut granted = loaded.manifest.permissions.clone();
    asked.sort();
    granted.sort();
    if loaded.manifest.id != entry.id || loaded.manifest.version != entry.version || asked != granted {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("{}: its manifest does not match the catalog", entry.name));
    }
    // Never back to an older version (an old catalog served again, L2).
    if let Ok(old) = super::load_folder(&target) {
        if newer(&old.manifest.version, &entry.version) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(format!("{} {} is installed, newer than the catalog's {}", entry.name, old.manifest.version, entry.version));
        }
    }
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(&staging, &target).map_err(|e| e.to_string())?;
    write_proof(entry, plugins_dir)?;
    Ok(target)
}

/// `entry`'s pack, unpacked and checked; `None` when the catalog has none.
async fn download_pack(http: &reqwest::Client, entry: &Entry) -> Option<Result<Vec<(String, Vec<u8>)>, String>> {
    let hash = entry.pack.as_ref()?;
    let url = format!("{}/{}/{}.a2pk", base_url(), entry.id, entry.version);
    Some(async {
        let bytes = match get(http, &url, MAX_PACK).await {
            Ok(bytes) => bytes,
            Err(_) => get(http, &url, MAX_PACK).await?,
        };
        if !sha256_hex(&bytes).eq_ignore_ascii_case(hash) {
            return Err(format!("{}: its pack does not match the catalog", entry.name));
        }
        unpack(entry, &bytes)
    }
    .await)
}

/// The largest pack downloaded: gzip can grow what it can't shrink, a little.
const MAX_PACK: usize = MAX_PLUGIN + 64 * 1024;
const PACK_MAGIC: &[u8] = b"A2PK1\n";

/// The next `n` bytes of `rest`, taken off it.
fn take<'a>(rest: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    if rest.len() < n {
        return None;
    }
    let (head, tail) = rest.split_at(n);
    *rest = tail;
    Some(head)
}

/// A pack's files (see the plugins repo's tools/build_catalog.py): gzip of
/// `A2PK1\n`, then per file a u16 LE path length, the path, a u32 LE size
/// and the bytes. Exactly `entry`'s files, each matching its hash, at most
/// MAX_PLUGIN in all.
fn unpack(entry: &Entry, packed: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    use std::io::Read;
    let bad = |why: &str| format!("{}: its pack {why}", entry.name);
    // The files, and each one's path and sizes.
    let limit = MAX_PLUGIN + PACK_MAGIC.len() + entry.files.len() * (2 + 128 + 4);
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(packed)
        .take(limit as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(|_| bad("is not readable"))?;
    if raw.len() > limit {
        return Err(format!("{} is larger than {MAX_PLUGIN} bytes", entry.name));
    }
    let mut rest = raw.strip_prefix(PACK_MAGIC).ok_or_else(|| bad("is not one this meter reads"))?;
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut total = 0;
    while !rest.is_empty() {
        let short = || bad("is cut short");
        let len = take(&mut rest, 2).ok_or_else(short)?;
        let path = take(&mut rest, u16::from_le_bytes([len[0], len[1]]) as usize).ok_or_else(short)?;
        let path = String::from_utf8(path.to_vec()).map_err(|_| bad("has a bad file name"))?;
        let size = take(&mut rest, 4).ok_or_else(short)?;
        let bytes = take(&mut rest, u32::from_le_bytes([size[0], size[1], size[2], size[3]]) as usize).ok_or_else(short)?;
        total += bytes.len();
        if total > MAX_PLUGIN {
            return Err(format!("{} is larger than {MAX_PLUGIN} bytes", entry.name));
        }
        let Some(hash) = entry.files.get(&path) else { return Err(bad(&format!("has {path:?}, not one of its files"))) };
        if files.iter().any(|(p, _)| *p == path) {
            return Err(bad(&format!("has {path} twice")));
        }
        if !sha256_hex(bytes).eq_ignore_ascii_case(hash) {
            return Err(format!("{}: {path} does not match the catalog, not installing", entry.name));
        }
        files.push((path, bytes.to_vec()));
    }
    if let Some(missing) = entry.files.keys().find(|f| files.iter().all(|(p, _)| p != *f)) {
        return Err(bad(&format!("is missing {missing}")));
    }
    Ok(files)
}

/// `entry`'s files one by one, each checked against its hash.
async fn download_files(http: &reqwest::Client, entry: &Entry) -> Result<Vec<(String, Vec<u8>)>, String> {
    // PARALLEL_DOWNLOADS at a time, each tried a second time if the first
    // fails (a dropped request no longer fails the update).
    use futures_util::StreamExt;
    // Each download owns what it uses (a borrow held across the stream's
    // awaits doesn't satisfy the spawned task's lifetimes).
    let jobs: Vec<(String, String, String)> = entry
        .files
        .iter()
        .map(|(path, hash)| (path.clone(), hash.clone(), format!("{}/{}/{}/{path}", base_url(), entry.id, entry.version)))
        .collect();
    let results: Vec<Result<(String, Vec<u8>), String>> = futures_util::stream::iter(jobs)
        .map(|(path, hash, url)| {
            let (http, name) = (http.clone(), entry.name.clone());
            async move {
                let bytes = match get(&http, &url, MAX_PLUGIN).await {
                    Ok(bytes) => bytes,
                    Err(_) => get(&http, &url, MAX_PLUGIN).await?,
                };
                if !sha256_hex(&bytes).eq_ignore_ascii_case(&hash) {
                    return Err(format!("{name}: {path} does not match the catalog, not installing"));
                }
                Ok((path, bytes))
            }
        })
        .buffer_unordered(PARALLEL_DOWNLOADS)
        .collect()
        .await;
    let mut files = Vec::new();
    let mut total = 0;
    for result in results {
        let (path, bytes) = result?;
        total += bytes.len();
        if total > MAX_PLUGIN {
            return Err(format!("{} is larger than {MAX_PLUGIN} bytes", entry.name));
        }
        files.push((path, bytes));
    }
    Ok(files)
}

/// The folder, in the plugins folder, of each install's signed catalog.
pub const PROOF_DIR: &str = ".proof";

fn proof_path(plugins_dir: &Path, id: &str) -> PathBuf {
    plugins_dir.join(PROOF_DIR).join(format!("{id}.json"))
}

/// Keep the signed catalog `entry` came from with its install.
fn write_proof(entry: &Entry, plugins_dir: &Path) -> Result<(), String> {
    let signed = entry.signed.as_ref().ok_or("the catalog entry has no signature to keep")?;
    let path = proof_path(plugins_dir, &entry.id);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, serde_json::to_vec(&**signed).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// Forget an uninstalled plugin's signed catalog.
pub fn remove_proof(plugins_dir: &Path, id: &str) {
    let _ = std::fs::remove_file(proof_path(plugins_dir, id));
}

/// Why an installed plugin doesn't run.
#[derive(Debug, Clone, PartialEq)]
pub enum Unverified {
    /// Installed before installs kept their catalog (2.0.87), or it's gone.
    NoProof,
    /// Its files aren't the signed catalog's.
    Changed(String),
}

/// Whether `dir` holds exactly the files of `entry`, each with its hash.
fn check_files(entry: &Entry, dir: &Path) -> Result<(), String> {
    let mut found = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    let mut total = 0usize;
    while let Some((folder, prefix)) = stack.pop() {
        for item in std::fs::read_dir(&folder).map_err(|e| e.to_string())?.flatten() {
            let name = item.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
            let kind = item.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() {
                stack.push((item.path(), rel));
                continue;
            }
            let Some(hash) = entry.files.get(&rel) else { return Err(format!("{rel} is not one of its files")) };
            // Only plain files: a link could point anywhere.
            if !kind.is_file() {
                return Err(format!("{rel} is not a plain file"));
            }
            let bytes = std::fs::read(item.path()).map_err(|e| format!("{rel}: {e}"))?;
            total += bytes.len();
            if total > MAX_PLUGIN {
                return Err(format!("larger than {MAX_PLUGIN} bytes"));
            }
            if !sha256_hex(&bytes).eq_ignore_ascii_case(hash) {
                return Err(format!("{rel} differs from the catalog's"));
            }
            found.push(rel);
        }
    }
    match entry.files.keys().find(|f| !found.contains(f)) {
        Some(missing) => Err(format!("{missing} is missing")),
        None => Ok(()),
    }
}

/// Check an installed plugin (`id` at `version`, in `dir`) against the
/// signed catalog its install kept.
pub fn verify_installed(plugins_dir: &Path, dir: &Path, id: &str, version: &str) -> Result<(), Unverified> {
    verify_installed_with(&public_key(), plugins_dir, dir, id, version)
}

fn verify_installed_with(key: &str, plugins_dir: &Path, dir: &Path, id: &str, version: &str) -> Result<(), Unverified> {
    let Ok(bytes) = std::fs::read(proof_path(plugins_dir, id)) else { return Err(Unverified::NoProof) };
    let Ok(signed) = serde_json::from_slice::<Signed>(&bytes) else { return Err(Unverified::NoProof) };
    let catalog = verify_with(key, signed.index.as_bytes(), &signed.signature).map_err(Unverified::Changed)?;
    let entry = catalog
        .plugins
        .iter()
        .find(|e| e.id == id && e.version == version)
        .ok_or_else(|| Unverified::Changed(format!("its catalog has no {id} {version}")))?;
    check_files(entry, dir).map_err(Unverified::Changed)
}

/// Take an install without a kept catalog for `entry`'s, when its files are
/// exactly that version's: it keeps the catalog from now on.
pub fn adopt(entry: &Entry, plugins_dir: &Path) -> Result<(), String> {
    check_files(entry, &plugins_dir.join(&entry.id))?;
    write_proof(entry, plugins_dir)
}

/// Whether `candidate` is a later version than `current` ("1.2.10" > "1.2.9").
pub fn newer(candidate: &str, current: &str) -> bool {
    let parts = |v: &str| v.split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parts(candidate) > parts(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    /// A version's notes come with its entry; an entry without them (every
    /// catalog before) reads as before.
    #[test]
    fn whats_new_comes_with_an_entry() {
        let json = r#"{"format":1,"plugins":[
            {"id":"a","name":"A","version":"1.2.0","apiVersion":2,"files":{},
             "changes":[{"version":"1.2.0","en":"Rows line up","de":"Zeilen"},{"version":"1.1.0","en":"A voice"},{"version":"1.0.0","en":"First"}]},
            {"id":"b","name":"B","version":"1.0.0","apiVersion":2,"files":{}}]}"#;
        let catalog: Catalog = serde_json::from_str(json).unwrap();
        let a = &catalog.plugins[0];
        let since = changes_between(a, "1.0.0", "1.2.0");
        assert_eq!(since.iter().map(|c| c.version.as_str()).collect::<Vec<_>>(), ["1.2.0", "1.1.0"]);
        assert_eq!(super::super::change_text(&since[0], "de"), "Zeilen");
        assert_eq!(super::super::change_text(&since[1], "de"), "A voice", "English when there's none in the language");
        assert!(catalog.plugins[1].changes.is_empty());
    }

    fn signed(catalog: &str) -> (String, String) {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let public: String = key.verifying_key().to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        let signature: String = key.sign(catalog.as_bytes()).to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        (public, signature)
    }

    const CATALOG: &str = r#"{"format":1,"plugins":[{"id":"pet-souls","name":"Pet Soul Tracker","version":"0.2.0","apiVersion":1,"files":{"manifest.json":"00","index.html":"00"}}]}"#;

    #[test]
    fn a_signed_catalog_reads() {
        let (key, signature) = signed(CATALOG);
        let catalog = verify_with(&key, CATALOG.as_bytes(), &signature).unwrap();
        assert_eq!(catalog.plugins[0].id, "pet-souls");
    }

    #[test]
    fn a_changed_catalog_is_refused() {
        let (key, signature) = signed(CATALOG);
        let tampered = CATALOG.replace("0.2.0", "0.2.1");
        assert!(verify_with(&key, tampered.as_bytes(), &signature).is_err());
        assert!(verify_with("", CATALOG.as_bytes(), &signature).is_err());
        assert!(verify_with(&key, CATALOG.as_bytes(), "abcd").is_err());
    }

    #[test]
    fn paths_stay_inside_the_folder() {
        assert!(safe_path("index.html"));
        assert!(safe_path("img/pet-1.png"));
        for bad in ["", "../x", "a/../b", "/abs", "a//b", "a\\b", "C:x", "./a"] {
            assert!(!safe_path(bad), "{bad}");
        }
    }

    /// A plugin folder `p` in `dir` with `files`, and its entry in a catalog
    /// signed with the test key.
    fn plugin_with(dir: &Path, files: &[(&str, &str)]) -> (String, Entry) {
        let mut hashes = BTreeMap::new();
        for (path, text) in files {
            let file = dir.join("p").join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, text).unwrap();
            hashes.insert(path.to_string(), sha256_hex(text.as_bytes()));
        }
        let index = serde_json::json!({ "format": 1, "plugins": [{
            "id": "p", "name": "P", "version": "1.0.0", "apiVersion": 2, "files": hashes }] }).to_string();
        let (key, signature) = signed(&index);
        let mut catalog = verify_with(&key, index.as_bytes(), &signature).unwrap();
        let mut entry = catalog.plugins.remove(0);
        entry.signed = Some(Arc::new(Signed { index, signature }));
        (key, entry)
    }

    /// A kept catalog's files are checked again: a changed, added or missing
    /// file stops the plugin, and with no catalog kept it waits to be adopted.
    #[test]
    fn an_install_is_checked_against_the_catalog_it_kept() {
        let dir = std::env::temp_dir().join(format!("a2-proof-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (key, entry) = plugin_with(&dir, &[("manifest.json", "{}"), ("index.html", "<p>"), ("img/a.png", "png")]);
        let check = || verify_installed_with(&key, &dir, &dir.join("p"), "p", "1.0.0");
        assert_eq!(check(), Err(Unverified::NoProof));
        adopt(&entry, &dir).unwrap();
        assert_eq!(check(), Ok(()));
        assert!(matches!(verify_installed_with(&key, &dir, &dir.join("p"), "p", "1.0.1"), Err(Unverified::Changed(_))));
        std::fs::write(dir.join("p/index.html"), "<script>").unwrap();
        assert!(matches!(check(), Err(Unverified::Changed(_))));
        // Changed files can't be adopted either.
        assert!(adopt(&entry, &dir).is_err());
        std::fs::write(dir.join("p/index.html"), "<p>").unwrap();
        std::fs::write(dir.join("p/extra.js"), "x").unwrap();
        assert!(matches!(check(), Err(Unverified::Changed(_))));
        std::fs::remove_file(dir.join("p/extra.js")).unwrap();
        std::fs::remove_file(dir.join("p/img/a.png")).unwrap();
        assert!(matches!(check(), Err(Unverified::Changed(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pack as the plugins repo's build_catalog.py writes one: manifest.json
    /// `{}`, index.html `<p>`, img/a.png `png`.
    const PACK: &str = "1f8b080000000000020a73340af036e4e264c8cc4dd74fd42bc84b6766606000525c0c997929a9157a1925b93920219b023b5e86dcc4bcccb4d4e212bdace2fc3c26a068752d00724c839b40000000";

    fn pack(files: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut body = PACK_MAGIC.to_vec();
        for (path, bytes) in files {
            body.extend((path.len() as u16).to_le_bytes());
            body.extend(path.as_bytes());
            body.extend((bytes.len() as u32).to_le_bytes());
            body.extend(*bytes);
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&body).unwrap();
        gz.finish().unwrap()
    }

    /// A pack unpacks to exactly its entry's files; one with a changed, an
    /// extra, a missing or a repeated file, or cut short, is refused.
    #[test]
    fn a_pack_holds_exactly_its_files() {
        let dir = std::env::temp_dir().join(format!("a2-pack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (_, entry) = plugin_with(&dir, &[("manifest.json", "{}"), ("index.html", "<p>"), ("img/a.png", "png")]);
        let _ = std::fs::remove_dir_all(&dir);
        let mut files = unpack(&entry, &hex_bytes(PACK).unwrap()).unwrap();
        files.sort();
        assert_eq!(files, [("img/a.png".into(), b"png".to_vec()), ("index.html".into(), b"<p>".to_vec()), ("manifest.json".into(), b"{}".to_vec())]);
        let good: [(&str, &[u8]); 3] = [("manifest.json", b"{}"), ("index.html", b"<p>"), ("img/a.png", b"png")];
        assert!(unpack(&entry, &pack(&good)).is_ok());
        assert!(unpack(&entry, &pack(&[good[0], ("index.html", b"<script>"), good[2]])).is_err(), "changed");
        assert!(unpack(&entry, &pack(&[good[0], good[1], good[2], ("x.js", b"x")])).is_err(), "extra");
        assert!(unpack(&entry, &pack(&good[..2])).is_err(), "missing");
        assert!(unpack(&entry, &pack(&[good[0], good[1], good[2], good[1]])).is_err(), "twice");
        let whole = pack(&good);
        let mut short = flate2::read::GzDecoder::new(&whole[..]);
        let mut raw = Vec::new();
        std::io::Read::read_to_end(&mut short, &mut raw).unwrap();
        raw.pop();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, &raw).unwrap();
        assert!(unpack(&entry, &gz.finish().unwrap()).is_err(), "cut short");
        assert!(unpack(&entry, b"not a pack").is_err());
    }

    #[test]
    fn an_older_catalog_is_refused() {
        let dir = std::env::temp_dir().join(format!("a2-serial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        keep_serials_in(&dir);
        assert!(check_serial("main", 0).is_ok(), "none read yet");
        assert!(check_serial("main", 100).is_ok());
        assert!(check_serial("main", 100).is_ok(), "the same one again");
        assert!(check_serial("main", 99).is_err());
        assert!(check_serial("main", 0).is_err(), "one without a serial once one had it");
        assert!(check_serial("test", 50).is_ok(), "each catalog its own");
        *serials_file().lock().unwrap() = None;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("0.2.10", "0.2.9"));
        assert!(newer("1.0", "0.9.9"));
        assert!(!newer("0.2.0", "0.2.0"));
        assert!(!newer("0.1.9", "0.2.0"));
    }
}

//! The update check, in the backend (2.0.80 on).
//!
//! The page used to fetch the manifest, read it and hand the package URL and
//! hash to `show_update_window`, so whatever ran in the page chose what got
//! installed (security audit H1). Now the meter fetches the manifest itself
//! from a fixed address, checks its Ed25519 signature
//! (`latest-v2.json.sig`, made by `tools/sign-manifest.mjs` with a key that
//! is not the plugin catalog's), refuses anything not newer than the running
//! version, and only then offers it.
//!
//! 2.0.80 still accepted a manifest without a signature (logged), so the
//! release that started signing couldn't strand anyone. Every manifest has
//! been signed since, so from 2.0.81 one without is refused, as is a wrong
//! signature (`REQUIRE_SIGNATURE`).

use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use std::time::Duration;

pub const MANIFEST_URL: &str = "https://cdn.a2tools.app/latest-v2.json";
pub const SIGNATURE_URL: &str = "https://cdn.a2tools.app/latest-v2.json.sig";
/// The last releases' notes for Settings → What's new, made from the GitHub
/// releases by `tools/changelog.mjs` and signed with the same key.
pub const CHANGELOG_URL: &str = "https://cdn.a2tools.app/changelog-v2.json";
pub const CHANGELOG_SIGNATURE_URL: &str = "https://cdn.a2tools.app/changelog-v2.json.sig";

/// The update key's public half (Ed25519), made 2026-10-27. The private
/// half is kept off the repository, on the publisher's PC.
const UPDATE_PUBLIC_KEY_HEX: &str = "dab40e70aa5e31e705c0b4b178d17c8a40859bec4aa2dcc9cff088b958b5c020";

/// Whether a manifest without a signature is refused. Off in 2.0.80, which
/// started the signing; on from 2.0.81 (security audit H1).
const REQUIRE_SIGNATURE: bool = true;

const TIMEOUT: Duration = Duration::from_secs(15);
/// The manifest is a few hundred bytes; anything far bigger isn't it.
const MAX_MANIFEST: usize = 64 * 1024;
/// Five releases' notes in four languages are some 40 KB.
const MAX_CHANGELOG: usize = 1024 * 1024;
const MAX_SIGNATURE: usize = 1024;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub msi_url: String,
    #[serde(default)]
    pub arch_url: String,
    #[serde(default)]
    pub deb_url: String,
    #[serde(default)]
    pub rpm_url: String,
    #[serde(default)]
    pub msi_sha256: String,
    #[serde(default)]
    pub arch_sha256: String,
    #[serde(default)]
    pub deb_sha256: String,
    #[serde(default)]
    pub rpm_sha256: String,
}

/// Fetch the manifest and check it: its signature (when there is one, or
/// always once `REQUIRE_SIGNATURE`), then read it.
pub async fn fetch(client: &reqwest::Client) -> Result<Manifest, String> {
    let body = get(client, MANIFEST_URL, MAX_MANIFEST).await?.ok_or("no update manifest")?;
    match get(client, SIGNATURE_URL, MAX_SIGNATURE).await? {
        Some(sig) => verify(UPDATE_PUBLIC_KEY_HEX, &body, String::from_utf8_lossy(&sig).trim())?,
        None if REQUIRE_SIGNATURE => return Err("the update manifest is not signed".into()),
        None => tracing::warn!("Update: the manifest has no signature (accepted in this version)"),
    }
    serde_json::from_slice(&body).map_err(|e| format!("update manifest: {e}"))
}

/// The release notes for Settings, as the page shows them. Signed like the
/// manifest, always: what they say about updating is read as ours.
pub async fn fetch_changelog(client: &reqwest::Client) -> Result<serde_json::Value, String> {
    // Debug builds can read a local copy first (A2T_CHANGELOG_URL, e.g. a
    // test server's), still signed: release builds have only the CDN.
    #[cfg(debug_assertions)]
    let url = std::env::var("A2T_CHANGELOG_URL").ok();
    #[cfg(not(debug_assertions))]
    let url: Option<String> = None;
    let (url, sig_url) = match &url {
        Some(u) => (u.as_str(), format!("{u}.sig")),
        None => (CHANGELOG_URL, CHANGELOG_SIGNATURE_URL.to_string()),
    };
    let body = get(client, url, MAX_CHANGELOG).await?.ok_or("no release notes")?;
    let sig = get(client, &sig_url, MAX_SIGNATURE).await?.ok_or("the release notes are not signed")?;
    verify(UPDATE_PUBLIC_KEY_HEX, &body, String::from_utf8_lossy(&sig).trim())?;
    serde_json::from_slice(&body).map_err(|e| format!("release notes: {e}"))
}

/// A file's bytes, or None when it isn't there (404).
async fn get(client: &reqwest::Client, url: &str, cap: usize) -> Result<Option<Vec<u8>>, String> {
    let response = client.get(url).timeout(TIMEOUT).send().await.map_err(|e| e.to_string())?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    crate::bounded_http::bytes(response, cap).await.map(Some)
}

/// Check `body`'s Ed25519 signature (hex) with `key_hex`.
fn verify(key_hex: &str, body: &[u8], signature_hex: &str) -> Result<(), String> {
    let key: [u8; 32] = hex(key_hex).and_then(|k| k.try_into().ok()).ok_or("this build has no update key")?;
    let key = VerifyingKey::from_bytes(&key).map_err(|_| "the update key is not valid")?;
    let signature: [u8; 64] = hex(signature_hex)
        .and_then(|s| s.try_into().ok())
        .ok_or("the update manifest's signature is malformed")?;
    key.verify_strict(body, &Signature::from_bytes(&signature))
        .map_err(|_| "the update manifest's signature does not match: not updating from it".to_string())
}

fn hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

/// Whether `latest` is newer than `current` ("2.0.80" against "2.0.79"; a
/// leading "v" and a "-pre" part are allowed, a release beats its
/// pre-release). Anything unreadable is not newer: never a downgrade.
pub fn newer(latest: &str, current: &str) -> bool {
    fn parse(v: &str) -> Option<([u64; 3], bool)> {
        let v = v.trim().trim_start_matches(['v', 'V']);
        let (base, pre) = match v.split_once('-') {
            Some((b, _)) => (b, true),
            None => (v, false),
        };
        let mut parts = [0u64; 3];
        let mut n = 0;
        for (i, part) in base.split('.').enumerate() {
            if i >= 3 {
                return None;
            }
            parts[i] = part.parse().ok()?;
            n += 1;
        }
        (n > 0).then_some((parts, pre))
    }
    match (parse(latest), parse(current)) {
        (Some((l, lpre)), Some((c, cpre))) => l > c || (l == c && cpre && !lpre),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn only_a_matching_signature_passes() {
        let key = SigningKey::from_bytes(&[9u8; 32]);
        let public: String = key.verifying_key().to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        let body = br#"{"version":"2.0.80"}"#;
        let sig: String = key.sign(body).to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        assert!(verify(&public, body, &sig).is_ok());
        assert!(verify(&public, br#"{"version":"9.9.9"}"#, &sig).is_err());
        assert!(verify(UPDATE_PUBLIC_KEY_HEX, body, &sig).is_err());
        assert!(verify(&public, body, "zz").is_err());
    }

    #[test]
    fn only_newer_versions_are_offered() {
        assert!(newer("2.0.80", "2.0.79"));
        assert!(newer("v2.1.0", "2.0.99"));
        assert!(newer("2.0.80", "2.0.80-beta"));
        assert!(!newer("2.0.79", "2.0.80"));
        assert!(!newer("2.0.80", "2.0.80"));
        assert!(!newer("2.0.80-beta", "2.0.80"));
        assert!(!newer("latest", "2.0.80"));
        assert!(!newer("2.0.80\n\nClick Yes", "2.0.79"));
    }

    #[test]
    fn the_built_in_key_is_a_key() {
        let key: [u8; 32] = hex(UPDATE_PUBLIC_KEY_HEX).unwrap().try_into().unwrap();
        assert!(VerifyingKey::from_bytes(&key).is_ok());
    }
}

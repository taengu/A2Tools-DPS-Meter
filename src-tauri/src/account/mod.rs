//! Signing the meter in to an A2 Tools account.
//!
//! The website owns identity — Discord, Google, email, passkeys all land on one
//! account (`A2-Tools/src/accounts.py`). The meter does not reimplement any of
//! that. It has no browser and cannot hold a client secret, so it uses the
//! Device Authorization Grant the server already exposes: ask for a code, show
//! the short half to the player, poll until they approve it in a browser.
//!
//! ```text
//! POST /api/device/code   {client, device_label}
//!   -> {device_code, user_code, expires_in, interval,
//!       verification_uri, verification_uri_complete}
//!
//! POST /api/device/token  {device_code}
//!   -> {access_token, ...}                      once approved
//!   -> 428 {error: "authorization_pending"}     keep polling
//!   -> 400 {error: "slow_down"|"expired_token"|"access_denied"}
//! ```
//!
//! The error vocabulary is RFC 8628's, so the rules here are the standard ones
//! rather than anything invented: honour the server's `interval`, back off on
//! `slow_down`, stop on `expired_token` and `access_denied`.
//!
//! The token is a bearer credential and is stored encrypted — see [`secret`].

pub mod guest;
pub mod secret;

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Identifies this client to the server. It is not a secret and does not
/// authenticate anything; it only scopes what the issued token may do
/// (`METER_SCOPES` on the server: profile, sync, characters).
pub const CLIENT_ID: &str = "dps-meter";

const PRODUCTION_URL: &str = "https://a2tools.app";

/// Every account request is small; a server that has not answered by then
/// will not.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const MAX_RESPONSE: usize = 256 * 1024;

/// Where the account lives. `A2TOOLS_API` points the meter at another server
/// without a rebuild.
pub fn base_url() -> String {
    api_base(std::env::var("A2TOOLS_API").ok().as_deref())
}

/// The token goes to this server in a header, so an override must be https.
/// Anything else falls back to production.
fn api_base(override_url: Option<&str>) -> String {
    let Some(value) = override_url.map(str::trim).filter(|v| !v.is_empty()) else {
        return PRODUCTION_URL.to_string();
    };
    match reqwest::Url::parse(value) {
        Ok(url) if url.scheme() == "https" && url.host_str().is_some() => {
            value.trim_end_matches('/').to_string()
        }
        _ => {
            tracing::warn!("A2TOOLS_API is not an https URL; using {PRODUCTION_URL}");
            PRODUCTION_URL.to_string()
        }
    }
}

/// Whether a link the server sent may be opened in the browser: https on
/// a2tools.app or a subdomain, default port, no user info.
pub fn is_site_url(url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else { return false };
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url
            .host_str()
            .is_some_and(|h| h == "a2tools.app" || h.ends_with(".a2tools.app"))
}

/// A grant in progress: what to show the player, and what to poll with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceGrant {
    /// Polling credential. Never shown to the user and never logged.
    #[serde(skip_serializing)]
    pub device_code: String,
    /// The short code the player types or confirms in the browser.
    pub user_code: String,
    pub expires_in: i64,
    /// Seconds between polls, chosen by the server.
    pub interval: i64,
    pub verification_uri: String,
    /// The same page with the code already filled in — the difference between
    /// a player pasting nine characters and a player giving up.
    pub verification_uri_complete: String,
}

/// What the UI needs to know, without ever handing it a credential.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkPrompt {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: i64,
}

impl From<&DeviceGrant> for LinkPrompt {
    fn from(g: &DeviceGrant) -> Self {
        Self {
            user_code: g.user_code.clone(),
            verification_uri: g.verification_uri.clone(),
            verification_uri_complete: g.verification_uri_complete.clone(),
            expires_in: g.expires_in,
        }
    }
}

/// How a poll ended.
#[derive(Debug, Clone, PartialEq)]
pub enum PollOutcome {
    /// Approved. Carries the bearer token.
    Approved(String),
    /// Nobody has decided yet; poll again after the interval.
    Pending,
    /// The server wants a longer gap. RFC 8628 says add 5s and continue.
    SlowDown,
    /// The code ran out, or the player said no. Both end the attempt.
    Expired,
    Denied,
    /// Network or server trouble. Worth retrying; not worth giving up over.
    Unreachable(String),
}

/// Classify one poll response. Split out so the RFC's vocabulary is testable
/// without a server — getting `slow_down` wrong produces a client that hammers
/// an endpoint until it is rate-limited.
pub fn classify(status: u16, body: &str) -> PollOutcome {
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    if let Some(token) = parsed.get("access_token").and_then(|v| v.as_str()) {
        if !token.is_empty() {
            return PollOutcome::Approved(token.to_string());
        }
    }
    match parsed.get("error").and_then(|v| v.as_str()) {
        Some("authorization_pending") => PollOutcome::Pending,
        Some("slow_down") => PollOutcome::SlowDown,
        Some("expired_token") => PollOutcome::Expired,
        Some("access_denied") => PollOutcome::Denied,
        Some(other) => PollOutcome::Unreachable(other.to_string()),
        // 428 is the server's "keep polling" even if the body was unreadable.
        None if status == 428 => PollOutcome::Pending,
        None => PollOutcome::Unreachable(format!("HTTP {status}")),
    }
}

/// Ask the server to begin a device login.
pub async fn start(client: &reqwest::Client, device_label: &str) -> Result<DeviceGrant, String> {
    let body = serde_json::json!({ "client": CLIENT_ID, "device_label": device_label });
    let response = client
        .post(format!("{}/api/device/code", base_url()))
        .timeout(REQUEST_TIMEOUT)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| format!("could not reach a2tools.app: {e}"))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("a2tools.app refused the request ({status})"));
    }
    let text = crate::bounded_http::text(response, MAX_RESPONSE).await?;
    serde_json::from_str::<DeviceGrant>(&text)
        .map_err(|e| format!("could not read the response from a2tools.app: {e}"))
}

/// Poll once.
pub async fn poll_once(client: &reqwest::Client, device_code: &str) -> PollOutcome {
    let body = serde_json::json!({ "device_code": device_code });
    match client
        .post(format!("{}/api/device/token", base_url()))
        .timeout(REQUEST_TIMEOUT)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status().as_u16();
            match crate::bounded_http::text(response, MAX_RESPONSE).await {
                Ok(text) => classify(status, &text),
                Err(why) => PollOutcome::Unreachable(why),
            }
        }
        Err(e) => PollOutcome::Unreachable(e.to_string()),
    }
}

/// Poll until the player decides, the code expires, or `deadline` passes.
///
/// Honours the server's interval and RFC 8628's `slow_down`. Network errors do
/// not end the attempt — someone alt-tabbing to a browser on a flaky connection
/// should not have to start over.
pub async fn poll_until_decided(
    client: &reqwest::Client,
    grant: &DeviceGrant,
    app_data_dir: &Path,
) -> Result<(), String> {
    let mut interval = Duration::from_secs(grant.interval.clamp(1, 60) as u64);
    let deadline =
        std::time::Instant::now() + Duration::from_secs(grant.expires_in.clamp(60, 3600) as u64);

    loop {
        if std::time::Instant::now() >= deadline {
            return Err("the code expired before it was approved".into());
        }
        tokio::time::sleep(interval).await;

        match poll_once(client, &grant.device_code).await {
            PollOutcome::Approved(token) => {
                if let Err(why) = secret::save(app_data_dir, &token) {
                    return Err(format!("signed in, but the token could not be stored securely: {why}"));
                }
                tracing::info!("Account connected");
                return Ok(());
            }
            PollOutcome::Pending => {}
            PollOutcome::SlowDown => {
                interval += Duration::from_secs(5);
                tracing::debug!("Device poll backing off to {}s", interval.as_secs());
            }
            PollOutcome::Expired => return Err("the code expired before it was approved".into()),
            PollOutcome::Denied => return Err("the request was declined".into()),
            PollOutcome::Unreachable(why) => {
                // Keep trying: the deadline is what ends this, not one bad poll.
                tracing::debug!("Device poll could not complete: {why}");
            }
        }
    }
}

/// The account behind the stored token, as the website describes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSummary {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub supporter: bool,
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Everything else the server chose to send, passed through untouched so
    /// the UI can show new fields without a meter release.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// What `whoami` found.
#[derive(Debug, Clone)]
pub enum AccountState {
    SignedIn(AccountSummary),
    /// No token, or the server said the token is no longer valid.
    SignedOut,
    /// A token is stored but could not be checked now: the keyring is locked
    /// or the server did not answer. Not a reason to ask for a new sign-in.
    Unavailable(String),
}

/// Ask who we are.
pub async fn whoami(client: &reqwest::Client, app_data_dir: &Path) -> AccountState {
    let token = match secret::load_stored(app_data_dir) {
        secret::Stored::Token(token) => token,
        secret::Stored::Missing => return AccountState::SignedOut,
        secret::Stored::Locked => {
            return AccountState::Unavailable(
                "The desktop keyring is locked, so the account cannot be checked. \
                 Unlock the keyring and open Settings again."
                    .into(),
            )
        }
    };
    let response = match client
        .get(format!("{}/api/me", base_url()))
        .timeout(REQUEST_TIMEOUT)
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
    {
        Ok(response) => response,
        Err(_) => return AccountState::Unavailable("Could not reach a2tools.app to check the account.".into()),
    };

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        // Revoked from the website, or the account is gone. Drop it rather than
        // showing a connected account that cannot do anything.
        tracing::info!("Account token is no longer valid; signing out");
        secret::clear(app_data_dir);
        return AccountState::SignedOut;
    }
    if !response.status().is_success() {
        // A server hiccup is not a reason to sign someone out.
        return AccountState::Unavailable(format!(
            "a2tools.app could not check the account ({}).",
            response.status()
        ));
    }
    match crate::bounded_http::text(response, MAX_RESPONSE).await.ok().and_then(|t| serde_json::from_str(&t).ok()) {
        Some(summary) => AccountState::SignedIn(summary),
        None => AccountState::Unavailable("Unexpected reply from a2tools.app.".into()),
    }
}

/// A name for this install, so the approval page says what is being approved.
/// How a meter names itself to a2tools.app (an account's list of devices, a
/// guest): by its system only. It said the PC's name, which can be a
/// person's name, and the site doesn't need it (security audit L6).
pub fn device_label() -> String {
    let system = match std::env::consts::OS {
        "windows" => "Windows",
        "linux" => "Linux",
        "macos" => "macOS",
        other => other,
    };
    format!("A2Tools Meter on {system}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_approved_poll_yields_the_token() {
        let out = classify(200, r#"{"access_token":"tok_live_123","token_type":"bearer"}"#);
        assert_eq!(out, PollOutcome::Approved("tok_live_123".into()));
    }

    #[test]
    fn the_rfc_error_vocabulary_is_handled() {
        assert_eq!(
            classify(428, r#"{"error":"authorization_pending"}"#),
            PollOutcome::Pending
        );
        assert_eq!(classify(400, r#"{"error":"slow_down"}"#), PollOutcome::SlowDown);
        assert_eq!(classify(400, r#"{"error":"expired_token"}"#), PollOutcome::Expired);
        assert_eq!(classify(400, r#"{"error":"access_denied"}"#), PollOutcome::Denied);
    }

    #[test]
    fn a_428_with_an_unreadable_body_still_means_keep_polling() {
        // The server uses 428 for pending; trust the status when the body is not
        // JSON rather than treating a proxy's HTML error page as a refusal.
        assert_eq!(classify(428, "<html>gateway</html>"), PollOutcome::Pending);
    }

    #[test]
    fn an_empty_token_is_not_an_approval() {
        // Otherwise a malformed success would "sign in" with a useless token and
        // the account would look connected while every call 401s.
        assert!(matches!(
            classify(200, r#"{"access_token":""}"#),
            PollOutcome::Unreachable(_)
        ));
    }

    #[test]
    fn an_unknown_error_is_retryable_rather_than_fatal() {
        assert!(matches!(
            classify(500, r#"{"error":"no database bound"}"#),
            PollOutcome::Unreachable(_)
        ));
        assert!(matches!(classify(502, ""), PollOutcome::Unreachable(_)));
    }

    #[test]
    fn the_grant_never_serialises_its_polling_credential() {
        // The prompt goes to the webview; the device code must not ride along.
        let grant = DeviceGrant {
            device_code: "SECRET-DEVICE-CODE".into(),
            user_code: "ABCD-1234".into(),
            expires_in: 600,
            interval: 5,
            verification_uri: "https://a2tools.app/link".into(),
            verification_uri_complete: "https://a2tools.app/link?code=ABCD-1234".into(),
        };
        let json = serde_json::to_string(&grant).unwrap();
        assert!(!json.contains("SECRET-DEVICE-CODE"));

        let prompt = serde_json::to_string(&LinkPrompt::from(&grant)).unwrap();
        assert!(!prompt.contains("SECRET-DEVICE-CODE"));
        assert!(prompt.contains("ABCD-1234"), "the user code is what gets shown");
    }

    #[test]
    fn the_api_base_accepts_only_an_https_override() {
        assert_eq!(api_base(None), "https://a2tools.app");
        assert_eq!(api_base(Some("")), "https://a2tools.app");
        assert_eq!(api_base(Some("https://staging.example.org/")), "https://staging.example.org");
        assert_eq!(api_base(Some("http://localhost:8787")), "https://a2tools.app");
        assert_eq!(api_base(Some("ftp://a2tools.app")), "https://a2tools.app");
        assert_eq!(api_base(Some("a2tools.app")), "https://a2tools.app");
    }

    #[test]
    fn only_https_a2tools_links_are_opened() {
        assert!(is_site_url("https://a2tools.app/link?code=ABCD-1234"));
        assert!(is_site_url("https://logs.a2tools.app/abc"));
        assert!(is_site_url("https://A2Tools.app/logs/abc"));
        assert!(!is_site_url("http://a2tools.app/link"));
        assert!(!is_site_url("https://a2tools.app.example.com/"));
        assert!(!is_site_url("https://evila2tools.app/"));
        assert!(!is_site_url("https://a2tools.app@example.com/"));
        assert!(!is_site_url("https://user@a2tools.app/"));
        assert!(!is_site_url("https://a2tools.app:8443/"));
        assert!(!is_site_url("file:///etc/passwd"));
        assert!(!is_site_url("javascript:alert(1)"));
        assert!(!is_site_url(""));
    }
}

//! Uploading fights without an account, and claiming them later.
//!
//! A player who turns on "Upload fights without an account"
//! (`dpsMeter.guestUpload`) gets a guest on a2tools.app: an account with no
//! sign-in of its own, whose upload key this meter keeps like the account
//! token ([`secret::save_guest`], `guest.dat`). Its fights upload as any other,
//! public and on the leaderboards, under "Guest upload".
//!
//! ```text
//! POST /api/guest              {deviceLabel}         -> 201 {token, accountId}
//! POST /api/guest/merge        Bearer <account>      {guestToken} -> {claimed}
//! POST /api/guest/claim-code   Bearer <guest>        -> {code, expiresAt}
//! ```
//!
//! Claiming: once the player signs in on this meter, the guest is merged into
//! the account (its fights become theirs) and the key forgotten. On another
//! PC, a claim code from Settings is entered at a2tools.app/claim. Holding the
//! key is what owns the fights: nothing in a fight says who uploaded it, so a
//! key lost before claiming leaves them guest uploads.

use std::path::Path;
use std::time::Duration;

use super::{base_url, secret};

/// Settings' "Share boss fights anonymously" (uploading without an account).
pub const GUEST_UPLOAD_KEY: &str = "dpsMeter.guestUpload";

/// Set once the player has been asked about sharing (2.0.80 on): the first
/// run's choice answered, or the notice to those already sharing shown.
/// Until then nothing uploads without an account, whatever the switch says.
pub const SHARING_ASKED_KEY: &str = "dpsMeter.sharingAsked";

/// Whether fights upload without an account now: chosen, and asked.
pub fn uploads(settings: &crate::config::settings::Settings) -> bool {
    settings.get(GUEST_UPLOAD_KEY).as_deref() == Some("true")
        && settings.get(SHARING_ASKED_KEY).as_deref() == Some("true")
}

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Why the guest key couldn't be had now, and whether trying later could help.
#[derive(Debug)]
pub struct GuestError {
    pub message: String,
    pub retryable: bool,
}

/// The guest key: the one kept, or a new guest made on a2tools.app.
pub async fn token(client: &reqwest::Client, app_data_dir: &Path) -> Result<String, GuestError> {
    match secret::load_guest(app_data_dir) {
        secret::Stored::Token(token) => return Ok(token),
        secret::Stored::Locked => {
            return Err(GuestError {
                message: "The desktop keyring is locked. Unlock the keyring to upload fights.".into(),
                retryable: true,
            })
        }
        secret::Stored::Missing => {}
    }
    let body = serde_json::json!({ "deviceLabel": super::device_label() });
    let response = client
        .post(format!("{}/api/guest", base_url()))
        .timeout(REQUEST_TIMEOUT)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| GuestError { message: format!("Could not reach a2tools.app: {e}"), retryable: true })?;
    let status = response.status();
    if !status.is_success() {
        return Err(GuestError {
            message: if status.as_u16() == 429 {
                "Too many guest uploaders from this connection today. Try again tomorrow, or sign in.".into()
            } else {
                format!("a2tools.app could not set up guest uploads ({status}).")
            },
            retryable: status.as_u16() == 429 || status.is_server_error(),
        });
    }
    let text = crate::bounded_http::text(response, super::MAX_RESPONSE).await
        .map_err(|message| GuestError { message, retryable: true })?;
    let token = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v["token"].as_str().map(str::to_string))
        .filter(|t| !t.is_empty())
        .ok_or_else(|| GuestError { message: "Unexpected reply from a2tools.app.".into(), retryable: true })?;
    secret::save_guest(app_data_dir, &token)
        .map_err(|why| GuestError { message: format!("The guest key could not be stored securely: {why}"), retryable: false })?;
    tracing::info!("Guest uploads set up");
    Ok(token)
}

/// Whether this meter holds a guest key (fights uploaded without an account,
/// not claimed yet).
pub fn has_guest(app_data_dir: &Path) -> bool {
    secret::guest_path(app_data_dir).exists()
}

/// Set while this PC's guest uploads are still to be deleted from
/// a2tools.app: the player stopped sharing (2.0.80 on). Tried again at each
/// start until the site says they're gone.
pub const DELETE_PENDING_KEY: &str = "dpsMeter.guestDeletePending";

/// Delete every fight this PC uploaded as a guest, and the guest with them
/// (`DELETE /api/guest/logs`), then forget the guest key. Some(n) when it is
/// done (n deleted; 0 when there was nothing, or the guest was already gone
/// or claimed by an account), None to try again later (no network, the site
/// busy).
pub async fn delete_uploads(client: &reqwest::Client, app_data_dir: &Path) -> Option<u64> {
    let secret::Stored::Token(guest) = secret::load_guest(app_data_dir) else { return Some(0) };
    let response = client
        .delete(format!("{}/api/guest/logs", base_url()))
        .timeout(REQUEST_TIMEOUT)
        .header("authorization", format!("Bearer {guest}"))
        .send()
        .await
        .ok()?;
    let status = response.status().as_u16();
    let text = crate::bounded_http::text(response, super::MAX_RESPONSE).await.ok()?;
    match status {
        200..=299 => {
            let deleted = serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| v["deleted"].as_u64()).unwrap_or(0);
            secret::clear_guest(app_data_dir);
            tracing::info!("Guest uploads deleted from a2tools.app: {deleted} fights");
            Some(deleted)
        }
        // Gone already (an earlier try got through), claimed by an account,
        // or not a guest: nothing of this guest's is left to delete.
        401 | 403 => {
            secret::clear_guest(app_data_dir);
            tracing::info!("Guest uploads: nothing left to delete ({status})");
            Some(0)
        }
        _ => {
            tracing::info!("Could not delete the guest uploads now ({status}); trying again later");
            None
        }
    }
}

/// Make this meter's guest fights the signed-in account's, and forget the
/// guest key. Some(n) fights claimed; None when it couldn't be done now (it's
/// tried again at the next start).
pub async fn merge_into_account(client: &reqwest::Client, app_data_dir: &Path) -> Option<u64> {
    let secret::Stored::Token(guest) = secret::load_guest(app_data_dir) else { return None };
    let secret::Stored::Token(account) = secret::load_stored(app_data_dir) else { return None };
    let response = client
        .post(format!("{}/api/guest/merge", base_url()))
        .timeout(REQUEST_TIMEOUT)
        .header("authorization", format!("Bearer {account}"))
        .header("content-type", "application/json")
        // With the real names of this PC's uploads: a guest's fights went up
        // masked, and the site names them again once they are the account's.
        .body(serde_json::json!({ "guestToken": guest, "names": crate::share::uploaded_names(app_data_dir, 2000) }).to_string())
        .send()
        .await
        .ok()?;
    let status = response.status();
    let text = crate::bounded_http::text(response, super::MAX_RESPONSE).await.ok()?;
    match status.as_u16() {
        200..=299 => {
            let claimed = serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| v["claimed"].as_u64()).unwrap_or(0);
            secret::clear_guest(app_data_dir);
            tracing::info!("Guest uploads claimed by the account: {claimed} fights");
            Some(claimed)
        }
        // Already merged (elsewhere, by a claim code), or the guest is gone:
        // nothing left to claim with this key.
        404 => {
            secret::clear_guest(app_data_dir);
            tracing::info!("Guest key no longer known to a2tools.app; forgotten");
            Some(0)
        }
        _ => {
            tracing::info!("Could not claim the guest uploads now ({status})");
            None
        }
    }
}

/// A short code that claims this meter's guest fights for whoever enters it
/// at a2tools.app/claim, signed in. (code, expires at as the site says it).
pub async fn claim_code(client: &reqwest::Client, app_data_dir: &Path) -> Result<(String, String), String> {
    let secret::Stored::Token(guest) = secret::load_guest(app_data_dir) else {
        return Err("No fights uploaded without an account on this PC.".into());
    };
    let response = client
        .post(format!("{}/api/guest/claim-code", base_url()))
        .timeout(REQUEST_TIMEOUT)
        .header("authorization", format!("Bearer {guest}"))
        .send()
        .await
        .map_err(|e| format!("Could not reach a2tools.app: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("a2tools.app could not make a claim code ({status})."));
    }
    let text = crate::bounded_http::text(response, super::MAX_RESPONSE).await?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|_| "Unexpected reply from a2tools.app.".to_string())?;
    match (v["code"].as_str(), v["expiresAt"].as_str()) {
        (Some(code), Some(at)) if !code.is_empty() => Ok((code.to_string(), at.to_string())),
        _ => Err("Unexpected reply from a2tools.app.".into()),
    }
}

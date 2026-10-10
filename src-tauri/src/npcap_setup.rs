//! When the capture library will not load and the meter can install it
//! (Windows: Npcap), offer to: fetch Npcap's current installer from
//! npcap.com, run it, and start capturing as soon as it loads, with no
//! restart. Elsewhere the page is told, as before.

#[cfg(feature = "online")]
use std::path::{Path, PathBuf};

use tauri::Emitter;
#[cfg(feature = "online")]
use tauri::Manager;

#[cfg(feature = "online")]
use crate::app::AppState;
use crate::capture::pcap_capturer::PcapCapturer;
#[cfg(feature = "online")]
use crate::platform;

#[cfg(feature = "online")]
const SITE: &str = "https://npcap.com/";

#[cfg(feature = "online")]
/// The prompt's words, in the meter's language.
struct Texts {
    title: String,
    ask: String,
    download_failed: String,
    still_missing: String,
}

#[cfg(feature = "online")]
const LANGUAGES: [&str; 10] = ["de", "en", "es", "fr", "ja", "ko", "pt", "ru", "zh-Hans", "zh-Hant"];

#[cfg(feature = "online")]
fn texts(data_dir: Option<&Path>, lang: &str) -> Texts {
    let load = |lang: &str| -> Option<serde_json::Value> {
        let path = data_dir?.join("i18n").join("ui").join(format!("{lang}.json"));
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    };
    let lang = LANGUAGES.iter().find(|known| known.eq_ignore_ascii_case(lang)).copied().unwrap_or("en");
    let (ui, en) = (load(lang), load("en"));
    let text = |key: &str, fallback: &str| -> String {
        [ui.as_ref(), en.as_ref()]
            .into_iter()
            .flatten()
            .find_map(|v| v["npcapPrompt"][key].as_str().map(str::to_string))
            .unwrap_or_else(|| fallback.to_string())
    };
    Texts {
        title: "A2Tools DPS Meter".into(),
        ask: text(
            "ask",
            "A2Tools DPS Meter needs Npcap to read the game's network traffic, and Npcap is missing or not working.\n\n\
             Install it now? The meter downloads Npcap's installer from npcap.com and opens it. Once Npcap is \
             installed, the meter starts capturing by itself.\n\n\
             Choose No to install it yourself from https://npcap.com/#download",
        ),
        download_failed: text(
            "downloadFailed",
            "Npcap's installer could not be downloaded ({error}).\n\n\
             Install it yourself from https://npcap.com/#download, then open the meter again.",
        ),
        still_missing: text(
            "stillMissing",
            "Npcap is still not working. If its installer was closed before it finished, install Npcap from \
             https://npcap.com/#download, then open the meter again.",
        ),
    }
}

#[cfg(feature = "online")]
/// The installer's file name in npcap.com's page (`dist/npcap-1.89.exe`),
/// the newest if it lists more than one.
fn installer_name(page: &str) -> Option<String> {
    let version = |name: &str| -> Vec<u32> {
        name.trim_start_matches("npcap-").trim_end_matches(".exe").split('.').filter_map(|p| p.parse().ok()).collect()
    };
    page.match_indices("dist/npcap-")
        .filter_map(|(at, _)| {
            let rest = &page[at + "dist/".len()..];
            let end = rest.find(".exe")?;
            let name = &rest[..end + ".exe".len()];
            let digits = &name["npcap-".len()..end];
            (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit() || c == '.')).then(|| name.to_string())
        })
        .max_by_key(|name| version(name))
}

#[cfg(feature = "online")]
async fn fetch_installer(http: &reqwest::Client) -> Result<PathBuf, String> {
    let page = http.get(SITE).send().await.and_then(|r| r.error_for_status()).map_err(|e| e.to_string())?;
    let page = page.text().await.map_err(|e| e.to_string())?;
    let name = installer_name(&page).ok_or("npcap.com lists no installer")?;
    let response = http
        .get(format!("{SITE}dist/{name}"))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?;
    let bytes = response.bytes().await.map_err(|e| e.to_string())?;
    // A Windows program, and about the size of one (1.89 is 1.3 MB).
    if bytes.len() < 200_000 || !bytes.starts_with(b"MZ") {
        return Err("the download is not an installer".into());
    }
    let path = std::env::temp_dir().join(format!("a2tools-{name}"));
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Called at startup when the capture library did not load.
///
/// Where the meter can install Npcap (Windows, online build) it offers to
/// download the installer. Otherwise the page shows the help: on Linux libpcap
/// comes from the packages, and the private build downloads nothing, so it
/// says where to get Npcap and leaves the installing to the player.
pub fn offer(app: tauri::AppHandle, capturer: PcapCapturer) {
    #[cfg(feature = "online")]
    if platform::pcap::OFFERS_INSTALL {
        offer_download(app, capturer);
        return;
    }
    #[cfg(not(feature = "online"))]
    drop(capturer);
    tauri::async_runtime::spawn(async move {
        // Small delay so the page is listening.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let _ = app.emit("npcap-missing", ());
    });
}

/// Ask, download Npcap's installer from npcap.com, run it and start capturing.
#[cfg(feature = "online")]
fn offer_download(app: tauri::AppHandle, capturer: PcapCapturer) {
    tauri::async_runtime::spawn(async move {
        let Some(state) = app.try_state::<AppState>() else { return };
        let lang = state.settings.get("dpsMeter.language").unwrap_or_else(|| "en".into());
        let t = texts(state.i18n_data_dir.as_deref(), &lang);
        let http = state.http.clone();

        let (title, ask) = (t.title.clone(), t.ask.clone());
        let yes = tauri::async_runtime::spawn_blocking(move || platform::dialog::ask_yes_no(&title, &ask))
            .await
            .unwrap_or(false);
        if !yes {
            tracing::info!("Npcap missing; the player will install it themselves");
            return;
        }

        let installer = match fetch_installer(&http).await {
            Ok(path) => path,
            Err(e) => {
                tracing::warn!("Npcap installer download failed: {e}");
                let (title, message) = (t.title.clone(), t.download_failed.replace("{error}", &e));
                let _ = tauri::async_runtime::spawn_blocking(move || platform::dialog::show_error(&title, &message)).await;
                return;
            }
        };
        tracing::info!("Running Npcap's installer: {}", installer.display());
        let ran = {
            let installer = installer.clone();
            tauri::async_runtime::spawn_blocking(move || platform::pcap::run_installer(&installer))
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
        };
        let _ = std::fs::remove_file(&installer);
        if let Err(e) = &ran {
            tracing::warn!("Npcap's installer did not run: {e}");
        }

        if platform::pcap::library_available() {
            tracing::info!("Npcap installed; starting capture");
            capturer.start();
            let _ = app.emit("npcap-installed", ());
        } else {
            let (title, message) = (t.title.clone(), t.still_missing.clone());
            let _ = tauri::async_runtime::spawn_blocking(move || platform::dialog::show_error(&title, &message)).await;
        }
    });
}

#[cfg(feature = "online")]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_installer_link_is_read_from_the_download_page() {
        let page = r#"<a href="dist/npcap-1.89.exe">Npcap 1.89 installer</a> ... <a href="dist/npcap-1.89.exe">"#;
        assert_eq!(installer_name(page).as_deref(), Some("npcap-1.89.exe"));
        let two = r#"href="dist/npcap-1.9.exe" href="dist/npcap-1.10.exe" href="dist/npcap-1.10-oem.exe""#;
        assert_eq!(installer_name(two).as_deref(), Some("npcap-1.10.exe"), "newest by version, not by text");
        assert_eq!(installer_name("no installer here"), None);
        assert_eq!(installer_name(r#"href="dist/npcap-sdk.exe""#), None);
    }

    #[test]
    fn the_prompt_has_words_in_every_language() {
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/data");
        let en = texts(Some(&data), "en");
        assert!(en.ask.contains("npcap.com"));
        assert!(en.download_failed.contains("{error}"));
        for lang in LANGUAGES {
            let t = texts(Some(&data), lang);
            assert!(t.ask.contains("https://npcap.com/#download"), "{lang}");
            assert!(t.download_failed.contains("{error}"), "{lang}");
            if lang != "en" {
                assert_ne!(t.ask, en.ask, "{lang} is translated");
            }
        }
        assert_eq!(texts(None, "ko").ask, en.ask, "no data: the built-in English");
    }
}

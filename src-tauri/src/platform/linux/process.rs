//! Setup for the whole process, before the UI starts.
//!
//! WebKitGTK's DMA-BUF renderer hands its frames to the compositor as GPU
//! buffers, and some setups reject them: on NVIDIA under Wayland the window
//! never opens ("Error 71 (Protocol error) dispatching to Wayland display",
//! issue #7), and under XWayland GBM cannot allocate them ("Failed to create
//! GBM buffer"). 2.0.38 turned the renderer off for that. From WebKitGTK 2.54
//! a window drawn without it is mostly blank: only parts of the transparent
//! overlay paint (issue #8). Keeping the renderer and having it hand frames
//! over in shared memory avoids both. Tested on WebKitGTK 2.52.6 and 2.54.1,
//! AMD and NVIDIA, Wayland and XWayland. A value the player set themselves
//! for either variable is left alone.

const DISABLE: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";
const FORCE_SHM: &str = "WEBKIT_DMABUF_RENDERER_FORCE_SHM";

fn desktop_is_gnome(desktop: &str) -> bool {
    desktop.split(':').any(|name| name.eq_ignore_ascii_case("gnome"))
}

pub(crate) fn is_gnome() -> bool {
    desktop_is_gnome(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default())
}

/// System76's COSMIC (Pop!_OS 24.04): its windows go on the overlay layer
/// (`layer`).
fn desktop_is_cosmic(desktop: &str) -> bool {
    desktop.split(':').any(|name| name.eq_ignore_ascii_case("cosmic"))
}

pub(crate) fn is_cosmic() -> bool {
    desktop_is_cosmic(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default())
}

/// The overlay mode asked for (`A2TOOLS_COSMIC_OVERLAY=1`): native Wayland and
/// layer surfaces (`layer`), not XWayland and sticky windows.
pub(crate) fn cosmic_overlay_requested() -> bool {
    std::env::var("A2TOOLS_COSMIC_OVERLAY").is_ok_and(|v| v.trim() == "1")
}

pub(crate) fn is_kde() -> bool {
    crate::platform::kwin_rules::is_kde(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default())
}

/// Hyprland, which sets this for every program it starts.
pub(crate) fn is_hyprland() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
}

/// Native Wayland asked for (`A2TOOLS_NATIVE_WAYLAND=1`) where the meter
/// would otherwise go through XWayland for KWin's stale frames.
fn native_wayland_requested() -> bool {
    std::env::var("A2TOOLS_NATIVE_WAYLAND").is_ok_and(|v| v.trim() == "1")
}

/// Whether this KWin (`kwin_wayland --version`: "kwin 6.8.3") can show a
/// WebKitGTK window's previous frame for a moment after its content changes:
/// from 6.8.3 (KDE bug 528950; 6.8.2 did not). The overlay flickered as its
/// tooltips opened and closed under the pointer, part of it cut off for a
/// frame or two (#198, CachyOS, Plasma 6.8.3); the same meter through
/// XWayland did not, nor on 6.8.0. No fixed version is known yet.
fn kwin_shows_stale_frames(version: &str) -> bool {
    let Some(number) = version.split_whitespace().last() else { return false };
    let mut parts = number.split('.').map(|p| p.parse::<u32>().ok());
    match (parts.next().flatten(), parts.next().flatten(), parts.next().flatten().unwrap_or(0)) {
        (Some(major), Some(minor), patch) => (major, minor, patch) >= (6, 8, 3),
        _ => false,
    }
}

/// The running KWin shows stale frames (`kwin_shows_stale_frames`): asked
/// only on KDE, once at start.
fn kde_kwin_shows_stale_frames(desktop: &str) -> bool {
    if !crate::platform::kwin_rules::is_kde(desktop) {
        return false;
    }
    std::process::Command::new("kwin_wayland")
        .arg("--version")
        .output()
        .ok()
        .is_some_and(|out| kwin_shows_stale_frames(&String::from_utf8_lossy(&out.stdout)))
}

/// GNOME, for keep-above; COSMIC too (its windows are made sticky either way,
/// `cosmic_sticky`; under XWayland the meter places them itself), unless its
/// overlay mode is asked for, which needs native Wayland; and KDE where KWin
/// shows stale frames (`stale_kwin`), unless native Wayland is asked for.
fn prefer_xwayland(backend_set: bool, desktop: &str, wayland: bool, x11_available: bool, stale_kwin: bool) -> bool {
    !backend_set
        && wayland
        && x11_available
        && (desktop_is_gnome(desktop)
            || (desktop_is_cosmic(desktop) && !cosmic_overlay_requested())
            || (stale_kwin && !native_wayland_requested()))
}

/// Returns a note for the log when it changed anything.
pub fn prepare() -> Option<String> {
    let mut notes = Vec::new();
    let nonempty = |name| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let wayland = nonempty("WAYLAND_DISPLAY")
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|value| value.eq_ignore_ascii_case("wayland"));
    // GTK's Wayland backend cannot request keep-above on GNOME, nor sticky on
    // COSMIC, and KWin 6.8.3 shows the overlay's stale frames. Prefer
    // XWayland for the meter, with native Wayland as an availability fallback.
    let backend_set = std::env::var_os("GDK_BACKEND").is_some();
    let stale_kwin = !backend_set && wayland && kde_kwin_shows_stale_frames(&desktop);
    if prefer_xwayland(backend_set, &desktop, wayland, nonempty("DISPLAY"), stale_kwin) {
        // SAFETY: prepare runs before the UI or any worker thread starts.
        unsafe { std::env::set_var("GDK_BACKEND", "x11,wayland") };
        let why = if stale_kwin && !desktop_is_gnome(&desktop) && !desktop_is_cosmic(&desktop) {
            "KWin 6.8.3+ shows stale frames, KDE bug 528950; A2TOOLS_NATIVE_WAYLAND=1 keeps native Wayland"
        } else {
            "GNOME/COSMIC overlay"
        };
        notes.push(format!("GDK_BACKEND=x11,wayland ({why}; explicit GDK_BACKEND overrides this)"));
    }
    if std::env::var_os(DISABLE).is_none() && std::env::var_os(FORCE_SHM).is_none() {
        // SAFETY: prepare runs before the UI or any worker thread starts.
        unsafe { std::env::set_var(FORCE_SHM, "1") };
        notes.push(format!("{FORCE_SHM}=1 (set by the meter; set {FORCE_SHM}=0 to hand frames over as GPU buffers)"));
    }
    notes.push(keep_allocator_lean());
    (!notes.is_empty()).then(|| notes.join("; "))
}

/// How often freed memory goes back to the system.
const TRIM_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

/// glibc's allocator keeps what the meter frees for itself, and with a pool
/// per busy thread it seldom reuses it: a meter using 60-125 MB held 650-790
/// MB more, freed but never returned, after an hour of real play (packet
/// bursts on loading a zone, the work at a fight's end), so it climbed past
/// 1 GB and on for players. Two pools, and what is free handed back every
/// TRIM_EVERY. Windows' heap returns freed memory by itself.
fn keep_allocator_lean() -> String {
    // SAFETY: prepare runs before any other thread starts; mallopt and
    // malloc_trim are thread-safe glibc calls with no Rust-side invariants.
    unsafe { libc::mallopt(libc::M_ARENA_MAX, 2) };
    let _ = std::thread::Builder::new().name("malloc-trim".into()).spawn(|| loop {
        std::thread::sleep(TRIM_EVERY);
        // SAFETY: as above.
        unsafe { libc::malloc_trim(0) };
    });
    "allocator: 2 arenas, freed memory returned every 30 s".to_string()
}

#[cfg(test)]
mod tests {
    use super::{desktop_is_cosmic, desktop_is_gnome, kwin_shows_stale_frames, prefer_xwayland};

    #[test]
    fn gnome_detection_matches_desktop_components_only() {
        for desktop in ["GNOME", "gnome", "ubuntu:GNOME", "GNOME-Classic:GNOME"] {
            assert!(desktop_is_gnome(desktop), "{desktop}");
        }
        for desktop in ["", "KDE", "Hyprland", "sway", "i3", "X-Cinnamon", "not-gnome"] {
            assert!(!desktop_is_gnome(desktop), "{desktop}");
            assert!(!prefer_xwayland(false, desktop, true, true, false), "{desktop}");
        }
    }

    #[test]
    fn backend_choice_respects_desktop_session_and_overrides() {
        assert!(prefer_xwayland(false, "ubuntu:GNOME", true, true, false));
        assert!(prefer_xwayland(false, "gnome", true, true, false));
        assert!(!prefer_xwayland(true, "GNOME", true, true, false));
        assert!(!prefer_xwayland(false, "KDE", true, true, false));
        assert!(!prefer_xwayland(false, "sway", true, true, false));
        assert!(!prefer_xwayland(false, "", true, true, false));
        assert!(!prefer_xwayland(false, "GNOME", false, true, false));
        assert!(!prefer_xwayland(false, "GNOME", true, false, false));
    }

    /// Pop!_OS 24.04 says `COSMIC` (a player's neofetch, 2026-11-14): through
    /// XWayland (the overlay mode is not asked for here).
    #[test]
    fn cosmic_is_told_apart_and_runs_through_xwayland() {
        assert!(desktop_is_cosmic("COSMIC"));
        assert!(desktop_is_cosmic("pop:COSMIC"));
        assert!(!desktop_is_cosmic("pop:GNOME"));
        assert!(prefer_xwayland(false, "COSMIC", true, true, false));
        assert!(!prefer_xwayland(true, "COSMIC", true, true, false));
        assert!(!prefer_xwayland(false, "COSMIC", true, false, false));
    }

    /// KWin 6.8.3 on: through XWayland (#198, KDE bug 528950); 6.8.2 and
    /// earlier, or a GDK_BACKEND the player set, stay as they are.
    #[test]
    fn kwin_683_and_later_runs_through_xwayland() {
        assert!(kwin_shows_stale_frames("kwin 6.8.3"));
        assert!(kwin_shows_stale_frames("kwin 6.8.10\n"));
        assert!(kwin_shows_stale_frames("kwin 6.9.0"));
        assert!(kwin_shows_stale_frames("kwin 7.0"));
        assert!(!kwin_shows_stale_frames("kwin 6.8.2"));
        assert!(!kwin_shows_stale_frames("kwin 6.8.0"));
        assert!(!kwin_shows_stale_frames("kwin 5.27.11"));
        assert!(!kwin_shows_stale_frames(""));
        assert!(!kwin_shows_stale_frames("kwin_wayland: unknown"));
        assert!(prefer_xwayland(false, "KDE", true, true, true));
        assert!(!prefer_xwayland(false, "KDE", true, true, false));
        assert!(!prefer_xwayland(true, "KDE", true, true, true));
        assert!(!prefer_xwayland(false, "KDE", false, true, true));
        assert!(!prefer_xwayland(false, "KDE", true, false, true));
    }
}

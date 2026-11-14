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

pub(crate) fn is_kde() -> bool {
    crate::platform::kwin_rules::is_kde(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default())
}

/// Hyprland, which sets this for every program it starts.
pub(crate) fn is_hyprland() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
}

fn prefer_xwayland(backend_set: bool, desktop: &str, wayland: bool, x11_available: bool) -> bool {
    !backend_set && wayland && x11_available && desktop_is_gnome(desktop)
}

/// Returns a note for the log when it changed anything.
pub fn prepare() -> Option<String> {
    let mut notes = Vec::new();
    let nonempty = |name| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let wayland = nonempty("WAYLAND_DISPLAY")
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|value| value.eq_ignore_ascii_case("wayland"));
    // GTK's Wayland backend cannot request keep-above on GNOME. Prefer
    // XWayland for the meter, with native Wayland as an availability fallback.
    if prefer_xwayland(std::env::var_os("GDK_BACKEND").is_some(), &desktop, wayland, nonempty("DISPLAY")) {
        // SAFETY: prepare runs before the UI or any worker thread starts.
        unsafe { std::env::set_var("GDK_BACKEND", "x11,wayland") };
        notes.push("GDK_BACKEND=x11,wayland (GNOME overlay; explicit GDK_BACKEND overrides this)".to_string());
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
    use super::{desktop_is_gnome, prefer_xwayland};

    #[test]
    fn gnome_detection_matches_desktop_components_only() {
        for desktop in ["GNOME", "gnome", "ubuntu:GNOME", "GNOME-Classic:GNOME"] {
            assert!(desktop_is_gnome(desktop), "{desktop}");
        }
        for desktop in ["", "KDE", "Hyprland", "sway", "i3", "X-Cinnamon", "not-gnome"] {
            assert!(!desktop_is_gnome(desktop), "{desktop}");
            assert!(!prefer_xwayland(false, desktop, true, true), "{desktop}");
        }
    }

    #[test]
    fn backend_choice_respects_desktop_session_and_overrides() {
        assert!(prefer_xwayland(false, "ubuntu:GNOME", true, true));
        assert!(prefer_xwayland(false, "gnome", true, true));
        assert!(!prefer_xwayland(true, "GNOME", true, true));
        assert!(!prefer_xwayland(false, "KDE", true, true));
        assert!(!prefer_xwayland(false, "sway", true, true));
        assert!(!prefer_xwayland(false, "", true, true));
        assert!(!prefer_xwayland(false, "GNOME", false, true));
        assert!(!prefer_xwayland(false, "GNOME", true, false));
    }
}

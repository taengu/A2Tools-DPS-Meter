//! COSMIC (Pop!_OS 24.04), opt-in (`A2TOOLS_COSMIC_OVERLAY=1`): the meter's
//! windows as layer-shell surfaces on the overlay layer, as COSMIC's own panel
//! and dock are. Otherwise they are made sticky (`cosmic_sticky`).
//!
//! Opt-in while unproven: in a VirtualBox VM (no 3D) WebKit painted nothing in
//! a layer surface, in any of its renderers, though plain GTK did (2026-11-14);
//! a player with a real GPU tries it first.
//!
//! COSMIC keeps no ordinary window above the others on request (natively or
//! through XWayland): a windowed game took the meter under it at each click
//! (a Pop!_OS player, 2026-11-14). Its "sticky" windows stay on top but must
//! be made so by hand, one by one, and wear the focus outline. An overlay
//! surface stays above every window, a fullscreen game's too, with no outline
//! and nothing to set up (tried on Pop!_OS 24.04, COSMIC of 2026-11).
//!
//! libgtk-layer-shell is loaded at run time, not linked: where it is missing
//! (or the compositor has no layer shell) the windows stay ordinary ones, as
//! before. An overlay surface is placed by its margins from the screen's top
//! left, not by the compositor: the meter drags it itself (`start_drag`) and
//! keeps where it was left in the store the KWin script fills
//! (`focus::save_place_at`).

use std::collections::HashMap;
use std::ffi::{c_char, c_int};
use std::sync::{Mutex, OnceLock};

use gtk::glib::translate::ToGlibPtr;
use gtk::prelude::*;

type Win = *mut gtk::ffi::GtkWindow;

// gtk-layer-shell's enums.
const LAYER_OVERLAY: c_int = 3;
const EDGE_LEFT: c_int = 0;
const EDGE_TOP: c_int = 2;
const KEYBOARD_NONE: c_int = 0;
const KEYBOARD_ON_DEMAND: c_int = 2;

struct Lib {
    _lib: libloading::Library,
    is_supported: Option<unsafe extern "C" fn() -> c_int>,
    init: unsafe extern "C" fn(Win),
    set_layer: unsafe extern "C" fn(Win, c_int),
    set_anchor: unsafe extern "C" fn(Win, c_int, c_int),
    set_margin: unsafe extern "C" fn(Win, c_int, c_int),
    set_namespace: unsafe extern "C" fn(Win, *const c_char),
    /// 0.6 on; before it only "interactivity", on or off.
    set_keyboard_mode: Option<unsafe extern "C" fn(Win, c_int)>,
    set_keyboard_interactivity: Option<unsafe extern "C" fn(Win, c_int)>,
}

// SAFETY: the function pointers are plain C symbols; they are only called on
// the GTK thread (`dialog::on_gtk_thread` / GDK's event handler).
unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

fn lib() -> Option<&'static Lib> {
    static LIB: OnceLock<Option<Lib>> = OnceLock::new();
    LIB.get_or_init(|| {
        // SAFETY: loading a system library whose initialiser has no
        // requirements; each symbol is looked up with its C signature.
        unsafe {
            let lib = libloading::Library::new("libgtk-layer-shell.so.0").ok()?;
            let sym = |name: &[u8]| lib.get::<*const ()>(name).ok().map(|s| *s);
            let need = |name: &[u8]| sym(name);
            let l = Lib {
                is_supported: sym(b"gtk_layer_is_supported\0").map(|p| std::mem::transmute(p)),
                init: std::mem::transmute(need(b"gtk_layer_init_for_window\0")?),
                set_layer: std::mem::transmute(need(b"gtk_layer_set_layer\0")?),
                set_anchor: std::mem::transmute(need(b"gtk_layer_set_anchor\0")?),
                set_margin: std::mem::transmute(need(b"gtk_layer_set_margin\0")?),
                set_namespace: std::mem::transmute(need(b"gtk_layer_set_namespace\0")?),
                set_keyboard_mode: sym(b"gtk_layer_set_keyboard_mode\0").map(|p| std::mem::transmute(p)),
                set_keyboard_interactivity: sym(b"gtk_layer_set_keyboard_interactivity\0").map(|p| std::mem::transmute(p)),
                _lib: lib,
            };
            Some(l)
        }
    })
    .as_ref()
}

/// Whether the meter's windows become overlay surfaces: COSMIC on native
/// Wayland, the library present. (Its compositor's support is asked as each
/// window is made one.)
pub fn wanted() -> bool {
    super::process::is_cosmic()
        && super::process::cosmic_overlay_requested()
        && std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
        && !std::env::var("GDK_BACKEND").is_ok_and(|b| b.trim_start().starts_with("x11"))
        && lib().is_some()
}

/// The overlay surfaces made, by window label: their margins (x from the
/// left, y from the top) and title.
fn surfaces() -> &'static Mutex<HashMap<String, Surface>> {
    static S: OnceLock<Mutex<HashMap<String, Surface>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

#[derive(Clone)]
struct Surface {
    title: String,
    margin: (i32, i32),
}

/// Whether `window` is one of the overlay surfaces.
pub fn is_layer(window: &tauri::WebviewWindow) -> bool {
    surfaces().lock().unwrap_or_else(|p| p.into_inner()).contains_key(window.label())
}

/// The window's margins, for the meter's own idea of where it is.
pub fn position(window: &tauri::WebviewWindow) -> Option<(i32, i32)> {
    surfaces().lock().unwrap_or_else(|p| p.into_inner()).get(window.label()).map(|s| s.margin)
}

/// Windows that must not take the keyboard from the game when clicked: the
/// overlay, the pins (plugin widgets) and the FP pin.
fn keeps_keyboard_off(label: &str) -> bool {
    label == "main" || label == crate::fp_pin::LABEL || crate::plugins::widgets::parse_label(label).is_some()
}

/// Make `window` an overlay surface, where `wanted`. One already shown is
/// hidden while it is set up, and shown again as it was.
pub fn apply(window: &tauri::WebviewWindow) {
    if !wanted() || is_layer(window) {
        return;
    }
    let target = window.clone();
    let label = window.label().to_string();
    let title = window.title().unwrap_or_default();
    let title_here = title.clone();
    let made = super::dialog::on_gtk_thread(move || {
        let lib = lib()?;
        // SAFETY: a C call with no arguments.
        if let Some(supported) = lib.is_supported {
            if unsafe { supported() } == 0 {
                return None;
            }
        }
        let gtk_window: gtk::Window = target.gtk_window().ok()?.upcast();
        let shown = gtk_window.is_visible();
        // Unmapped while it is set up; never unrealized: WebKit's view does
        // not come back from that (its page stopped loading, the window was
        // never shown). gtk-layer-shell 0.8 takes a realized window and maps
        // it as a layer surface the next time it shows.
        if shown {
            gtk_window.hide();
        }
        let ptr: Win = gtk_window.to_glib_none().0;
        let margin = start_margin(&title_here, &gtk_window);
        // SAFETY: `ptr` is the live GtkWindow, unrealized, on the GTK thread.
        unsafe {
            (lib.init)(ptr);
            (lib.set_layer)(ptr, LAYER_OVERLAY);
            (lib.set_anchor)(ptr, EDGE_LEFT, 1);
            (lib.set_anchor)(ptr, EDGE_TOP, 1);
            (lib.set_margin)(ptr, EDGE_LEFT, margin.0);
            (lib.set_margin)(ptr, EDGE_TOP, margin.1);
            (lib.set_namespace)(ptr, c"a2tools-dps-meter".as_ptr());
            let keyboard = if keeps_keyboard_off(target.label()) { KEYBOARD_NONE } else { KEYBOARD_ON_DEMAND };
            if let Some(f) = lib.set_keyboard_mode {
                f(ptr, keyboard);
            } else if let Some(f) = lib.set_keyboard_interactivity {
                f(ptr, (keyboard != KEYBOARD_NONE) as c_int);
            }
        }
        install_drag_handler();
        if shown {
            gtk_window.show();
        }
        Some(margin)
    })
    .flatten();
    if let Some(margin) = made {
        tracing::info!("Window {label}: an overlay surface (COSMIC) at {margin:?}");
        surfaces().lock().unwrap_or_else(|p| p.into_inner()).insert(label, Surface { title, margin });
    }
}

/// Where a new surface goes: where its window was left, or centred near the
/// top of the screen. Kept on the screen.
fn start_margin(title: &str, gtk_window: &gtk::Window) -> (i32, i32) {
    let (w, h) = gtk_window.size();
    let screen = gtk::gdk::Display::default()
        .and_then(|d| d.primary_monitor().or_else(|| d.monitor(0)))
        .map(|m| m.geometry())
        .map(|g| (g.width(), g.height()))
        .unwrap_or((1920, 1080));
    let (x, y) = super::focus::place_of_title(title).unwrap_or(((screen.0 - w) / 2, 80));
    clamp((x, y), (w, h), screen)
}

fn clamp((x, y): (i32, i32), (w, h): (i32, i32), (sw, sh): (i32, i32)) -> (i32, i32) {
    (x.clamp(0, (sw - w.min(sw)).max(0)), y.clamp(0, (sh - h.min(sh)).max(0)))
}

/// A drag under way: the window's label, the pointer's place in it when the
/// drag began (taken from the first motion), and its margins.
struct Drag {
    label: String,
    window: tauri::WebviewWindow,
    grab: Option<(f64, f64)>,
    margin: (i32, i32),
}

fn drag() -> &'static Mutex<Option<Drag>> {
    static D: OnceLock<Mutex<Option<Drag>>> = OnceLock::new();
    D.get_or_init(Default::default)
}

/// Start moving an overlay surface with the pointer, the button held: the
/// compositor moves no layer surface, so its margins follow the pointer here
/// (`install_drag_handler`). False for an ordinary window.
pub fn start_drag(window: &tauri::WebviewWindow) -> bool {
    let Some(margin) = position(window) else { return false };
    *drag().lock().unwrap_or_else(|p| p.into_inner()) =
        Some(Drag { label: window.label().to_string(), window: window.clone(), grab: None, margin });
    true
}

/// GDK hands every event to this before GTK: during a drag, the pointer's
/// motion moves the surface by how far the pointer is from where it held the
/// window (it holds the same place as the surface follows), and the release
/// ends it. Everything else goes on to GTK unchanged.
fn install_drag_handler() {
    static INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if INSTALLED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    gtk::gdk::Event::set_handler(Some(|event: &mut gtk::gdk::Event| {
        if on_drag_event(event) {
            return;
        }
        gtk::main_do_event(event);
    }));
}

/// True when the event was the drag's and GTK should not see it.
fn on_drag_event(event: &gtk::gdk::Event) -> bool {
    use gtk::gdk::EventType;
    let kind = event.event_type();
    if !matches!(kind, EventType::MotionNotify | EventType::ButtonRelease) {
        return false;
    }
    let mut guard = drag().lock().unwrap_or_else(|p| p.into_inner());
    let Some(d) = guard.as_mut() else { return false };
    let held = event.state().is_some_and(|s| s.contains(gtk::gdk::ModifierType::BUTTON1_MASK));
    if kind == EventType::ButtonRelease || !held {
        let done = guard.take();
        drop(guard);
        if let Some(d) = done {
            finish(&d);
        }
        return false;
    }
    let Some((x, y)) = event.root_coords() else { return false };
    let Some((gx, gy)) = d.grab else {
        d.grab = Some((x, y));
        return true;
    };
    let (dx, dy) = ((x - gx).round() as i32, (y - gy).round() as i32);
    if dx == 0 && dy == 0 {
        return true;
    }
    d.margin = (d.margin.0 + dx, d.margin.1 + dy);
    if let Ok(gtk_window) = d.window.gtk_window().map(|w| w.upcast::<gtk::Window>()) {
        let screen = gtk::gdk::Display::default()
            .and_then(|disp| disp.primary_monitor().or_else(|| disp.monitor(0)))
            .map(|m| (m.geometry().width(), m.geometry().height()))
            .unwrap_or((1920, 1080));
        d.margin = clamp(d.margin, gtk_window.size(), screen);
        set_margins(&gtk_window, d.margin);
    }
    true
}

fn set_margins(gtk_window: &gtk::Window, (x, y): (i32, i32)) {
    let Some(lib) = lib() else { return };
    let ptr: Win = gtk_window.to_glib_none().0;
    // SAFETY: the live GtkWindow, a layer surface, on the GTK thread.
    unsafe {
        (lib.set_margin)(ptr, EDGE_LEFT, x);
        (lib.set_margin)(ptr, EDGE_TOP, y);
    }
}

/// A drag over: its place kept, for this run and the next.
fn finish(d: &Drag) {
    let mut all = surfaces().lock().unwrap_or_else(|p| p.into_inner());
    let Some(s) = all.get_mut(&d.label) else { return };
    s.margin = d.margin;
    let (w, h) = d.window.outer_size().map(|s| (s.width as i32, s.height as i32)).unwrap_or((0, 0));
    super::focus::save_place_at(&s.title, (d.margin.0, d.margin.1, w, h));
}

#[cfg(test)]
mod tests {
    use super::clamp;

    #[test]
    fn a_surface_stays_on_the_screen() {
        assert_eq!(clamp((-40, 50), (400, 70), (1280, 800)), (0, 50));
        assert_eq!(clamp((1200, 790), (400, 70), (1280, 800)), (880, 730));
        // Wider than the screen: at its left edge.
        assert_eq!(clamp((100, 0), (2000, 70), (1280, 800)), (0, 0));
    }
}

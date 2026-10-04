//! Window helpers on Linux. As in `../unsupported/window.rs`, except that the
//! pointer position can be read when the meter runs on GDK's X11 backend.
//!
//! The click-through lock needs the pointer's position outside the meter's
//! windows (`platform::window::cursor_position`). A native Wayland window
//! cannot read that, but the meter often runs under XWayland (`GDK_BACKEND=x11`),
//! and so does a Proton game unless `PROTON_ENABLE_WAYLAND` is set; an X11
//! client can read the global pointer while it is over any X11 window. With
//! a position, the lock as it is works: a player tested it on KDE Plasma
//! (issue #14). libX11 is loaded at run time, so nothing new is linked.

use x11_dl::xlib;

/// Detect GDK's actual backend, including XWayland and backend fallbacks.
pub fn native_wayland(window: &tauri::WebviewWindow) -> bool {
    use gtk::prelude::*;
    let window = window.clone();
    super::dialog::on_gtk_thread(move || {
        window.gtk_window().ok().is_some_and(|w| {
            w.display().type_().name() == "GdkWaylandDisplay"
        })
    }).unwrap_or(false)
}

/// Compositor grabs clear client pointer focus. Reject synthetic WebKit
/// hover events until the pointer returns to this window.
pub fn native_pointer_down(window: &tauri::WebviewWindow) -> Option<bool> {
    use gtk::{gdk, prelude::*};
    let window = window.clone();
    super::dialog::on_gtk_thread(move || {
        let gtk_window = window.gtk_window().ok()?;
        let surface = gtk_window.window()?;
        let pointer = gtk_window.display().default_seat()?.pointer()?;
        let hit = pointer.window_at_position().0?;
        if hit.toplevel() != surface.toplevel() {
            return None;
        }
        Some(surface.device_position(&pointer).3.contains(gdk::ModifierType::BUTTON1_MASK))
    }).flatten()
}

/// Commit unpinned size hints before requesting a compositor resize.
/// WebKit animation frames do not guarantee a GTK surface commit.
pub async fn prepare_native_resize(
    window: &tauri::WebviewWindow,
    min: tauri::LogicalSize<f64>,
) -> Result<(), String> {
    use gtk::{gdk, glib, prelude::*};
    let window = window.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (clock, signal) = super::dialog::on_gtk_thread(move || {
        let gtk_window = window.gtk_window().map_err(|e| e.to_string())?;
        let clock = gtk_window.frame_clock().ok_or("Window has no frame clock")?;
        let geometry = gdk::Geometry::new(
            min.width.ceil() as i32, min.height.ceil() as i32,
            0, 0, 0, 0, 0, 0, 0.0, 0.0, gdk::Gravity::NorthWest,
        );
        gtk_window.set_geometry_hints(None::<&gtk::Widget>, Some(&geometry), gdk::WindowHints::MIN_SIZE);
        let sender = std::cell::RefCell::new(Some(tx));
        let signal = clock.connect_local("after-paint", true, move |_| {
            if let Some(tx) = sender.borrow_mut().take() {
                let _ = tx.send(());
            }
            None
        });
        gtk_window.queue_resize();
        gtk_window.queue_draw();
        clock.request_phase(gdk::FrameClockPhase::LAYOUT | gdk::FrameClockPhase::PAINT | gdk::FrameClockPhase::AFTER_PAINT);
        Ok::<_, String>((glib::SendWeakRef::from(clock.downgrade()), signal))
    }).ok_or("GTK thread unavailable")??;
    // Hidden windows may never paint; detach the handler even on timeout.
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), rx).await;
    super::dialog::on_gtk_thread(move || {
        if let Some(clock) = clock.upgrade() {
            clock.disconnect(signal);
        }
    });
    result.map_err(|_| "Timed out applying resize hints".to_string())?
        .map_err(|_| "Window closed before resize".to_string())
}

/// One X connection per thread that asks, closed when the thread ends. The
/// pointer watch runs on its own thread; gdk's pointer calls would need the
/// GTK main thread.
struct Conn {
    x: xlib::Xlib,
    display: *mut xlib::Display,
}

impl Drop for Conn {
    fn drop(&mut self) {
        unsafe { (self.x.XCloseDisplay)(self.display) };
    }
}

thread_local! {
    static CONN: Option<Conn> = {
        xlib::Xlib::open().ok().and_then(|x| {
            let display = unsafe { (x.XOpenDisplay)(std::ptr::null()) };
            (!display.is_null()).then_some(Conn { x, display })
        })
    };
}

/// Whether the meter's windows are on GDK's X11 backend (XWayland or X11).
fn on_x11() -> bool {
    let backend = std::env::var("GDK_BACKEND").unwrap_or_default();
    match backend.split(',').next().map(str::trim) {
        Some("x11") => true,
        Some("wayland") => false,
        _ => std::env::var_os("WAYLAND_DISPLAY").is_none_or(|v| v.is_empty()),
    }
}

/// Where the mouse pointer is, in screen pixels: on the X11 backend only. A
/// native Wayland window cannot read it, so there the lock is not offered.
/// The root coordinates are the space tao's `inner_position()` uses on X11,
/// so the lock button's hit test lines up.
pub fn cursor_position() -> Option<(i32, i32)> {
    query_pointer().map(|(x, y, _)| (x, y))
}

/// Whether the left mouse button is held: on the X11 backend only. Ends a
/// window-manager resize of a tool window (see `release_size`).
pub fn primary_button_down() -> Option<bool> {
    query_pointer().map(|(_, _, mask)| mask & xlib::Button1Mask != 0)
}

fn query_pointer() -> Option<(i32, i32, u32)> {
    if !on_x11() {
        return None;
    }
    CONN.with(|conn| {
        let c = conn.as_ref()?;
        let (mut root_ret, mut child) = (0, 0);
        let (mut rx, mut ry, mut wx, mut wy, mut mask) = (0, 0, 0, 0, 0);
        let ok = unsafe {
            let root = (c.x.XDefaultRootWindow)(c.display);
            (c.x.XQueryPointer)(
                c.display, root, &mut root_ret, &mut child, &mut rx, &mut ry, &mut wx, &mut wy, &mut mask,
            )
        };
        (ok != 0).then_some((rx, ry, mask))
    })
}

pub fn start_drag(window: &tauri::WebviewWindow) {
    let _ = window.start_dragging();
}

pub fn show_on_top_without_focus(window: &tauri::WebviewWindow) {
    let _ = window.show();
    let _ = window.set_always_on_top(true);
}

pub fn minimize_off_top(window: &tauri::WebviewWindow) {
    let _ = window.set_always_on_top(false);
    let _ = window.minimize();
}

/// Size a meter window and pin its size hints to that size. Window managers
/// only edge-tile or maximize a window whose minimum size is below its maximum
/// (KWin: `X11Window::isResizable`), so pinned hints keep KDE, GNOME and the
/// rest from snapping the overlay or its tool windows into a tile. The meter
/// still sizes its windows itself: every size change goes through here. Min
/// and max go in one call; set one at a time, the window manager sees a
/// minimum above the maximum in between and the window flickers.
pub fn set_size(window: &tauri::WebviewWindow, size: tauri::Size) {
    let (w, h) = match size {
        tauri::Size::Physical(s) => (
            tauri::PixelUnit::Physical(tauri::PhysicalUnit::new(s.width as i32)),
            tauri::PixelUnit::Physical(tauri::PhysicalUnit::new(s.height as i32)),
        ),
        tauri::Size::Logical(s) => (
            tauri::PixelUnit::Logical(tauri::LogicalUnit::new(s.width)),
            tauri::PixelUnit::Logical(tauri::LogicalUnit::new(s.height)),
        ),
    };
    let _ = window.set_size_constraints(tauri::WindowSizeConstraints {
        min_width: Some(w),
        min_height: Some(h),
        max_width: Some(w),
        max_height: Some(h),
    });
    let _ = window.set_size(size);
}

/// Unpin a tool window's size so the window manager can resize it, down to
/// `min` logical pixels. Window managers tile on a move, not on a resize, so
/// this is safe for the length of a resize; `set_size` pins it again after.
pub fn release_size(window: &tauri::WebviewWindow, min: tauri::LogicalSize<f64>) {
    let _ = window.set_size_constraints(tauri::WindowSizeConstraints {
        min_width: Some(tauri::PixelUnit::Logical(tauri::LogicalUnit::new(min.width))),
        min_height: Some(tauri::PixelUnit::Logical(tauri::LogicalUnit::new(min.height))),
        max_width: None,
        max_height: None,
    });
}

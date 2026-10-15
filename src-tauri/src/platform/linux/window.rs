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

/// GNOME uses compositor resizing on both native Wayland and X11.
pub fn compositor_resize_supported(window: &tauri::WebviewWindow) -> bool {
    if !super::process::is_gnome() {
        return false;
    }
    use gtk::prelude::*;
    let window = window.clone();
    super::dialog::on_gtk_thread(move || {
        window.gtk_window().ok().is_some_and(|w| {
            matches!(w.display().type_().name(), "GdkWaylandDisplay" | "GdkX11Display")
        })
    }).unwrap_or(false)
}

/// Compositor grabs clear client pointer focus. Reject synthetic WebKit
/// hover events until the pointer returns to this window.
pub fn resize_pointer_down(window: &tauri::WebviewWindow) -> Option<bool> {
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
pub async fn prepare_resize(
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

/// The click-through lock as an input region: the window takes the mouse
/// only over `keep` (the lock button: CSS-pixel x, y, width, height and the
/// page's scale), or nowhere but its top-left pixel without one, as tao's
/// own `set_ignore_cursor_events` does. `ignore` false takes it everywhere.
///
/// Under XWayland the pointer can be read only while it is over an X11
/// window, so the pointer watch the lock uses elsewhere lost it over the
/// desktop or a native Wayland window and the button could not be clicked.
/// An input region needs no pointer. Returns whether it was applied.
pub fn click_through_except(window: &tauri::WebviewWindow, ignore: bool, keep: Option<(f64, f64, f64, f64, f64)>) -> bool {
    use gtk::{cairo, prelude::*};
    let window = window.clone();
    super::dialog::on_gtk_thread(move || {
        let gtk_window = window.gtk_window().ok()?;
        if !ignore {
            gtk_window.input_shape_combine_region(None);
            return Some(());
        }
        let surface = gtk_window.window()?;
        // CSS pixels to GDK's: the page's scale over GDK's own.
        let rect = keep.and_then(|(x, y, w, h, page_scale)| {
            let k = page_scale / f64::from(gtk_window.scale_factor().max(1));
            let (left, top) = ((x * k).floor() as i32, (y * k).floor() as i32);
            let (right, bottom) = (((x + w) * k).ceil() as i32, ((y + h) * k).ceil() as i32);
            (right > left && bottom > top).then(|| cairo::RectangleInt::new(left, top, right - left, bottom - top))
        });
        let region = cairo::Region::create_rectangle(&rect.unwrap_or_else(|| cairo::RectangleInt::new(0, 0, 1, 1)));
        surface.input_shape_combine_region(&region, 0, 0);
        Some(())
    })
    .flatten()
    .is_some()
}

pub fn start_drag(window: &tauri::WebviewWindow) {
    let _ = window.start_dragging();
}

/// Show a window on top without taking focus from the game. A plain show
/// maps the window and the window manager activates it as a new window
/// (KWin did, under XWayland, for each plugin alert), so GTK is told first
/// not to take focus on map (`_NET_WM_USER_TIME` 0 on X11). Clicking the
/// window still focuses it.
pub fn show_on_top_without_focus(window: &tauri::WebviewWindow) {
    use gtk::prelude::*;
    let target = window.clone();
    super::dialog::on_gtk_thread(move || {
        if let Ok(gtk_window) = target.gtk_window() {
            gtk_window.set_focus_on_map(false);
        }
    });
    let _ = window.show();
    let _ = window.set_always_on_top(true);
}

/// Hide a window `show_on_top_without_focus` shows (see the Windows one).
pub fn hide_without_focus(window: &tauri::WebviewWindow) {
    let _ = window.hide();
}

/// Here a window can be built hidden and shown later as it is.
pub const HIDE_AFTER_BUILD: bool = false;

/// The windows `hide_in_place` has hidden, by label.
fn hidden_in_place() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static HIDDEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new();
    HIDDEN.get_or_init(Default::default)
}

/// Hide a window without unmapping it: fully transparent, click-through and
/// out of the taskbar. A compositor places a window anew each time it is
/// mapped (KWin under the pointer, as a CachyOS player saw an alert bring a
/// plugin window up, 2026-10-16), and Wayland lets no client move its own
/// window back, so a window that stays mapped is the only one that stays where
/// it was left. Without a compositor nothing can be transparent, and it is
/// hidden as before. A window not showing (a plugin started hidden has no GDK
/// surface yet) is left hidden: tao's `set_ignore_cursor_events` unwraps that
/// surface and aborts the meter, so the input region is set here instead.
pub fn hide_in_place(window: &tauri::WebviewWindow) {
    use gtk::{cairo, prelude::*};
    let target = window.clone();
    let concealed = super::dialog::on_gtk_thread(move || {
        let gtk_window = target.gtk_window().ok()?;
        if !gtk_window.is_visible() || !WidgetExt::screen(&gtk_window)?.is_composited() {
            return None;
        }
        let surface = gtk_window.window()?;
        gtk_window.set_opacity(0.0);
        surface.input_shape_combine_region(&cairo::Region::create_rectangle(&cairo::RectangleInt::new(0, 0, 1, 1)), 0, 0);
        Some(())
    })
    .flatten()
    .is_some();
    if !concealed {
        hide_without_focus(window);
        return;
    }
    let _ = window.set_skip_taskbar(true);
    hidden_in_place().lock().unwrap_or_else(|p| p.into_inner()).insert(window.label().to_string());
}

/// Show a window `hide_in_place` hid, where it is; false if it was not hidden
/// that way (then it is shown as usual).
pub fn reveal_in_place(window: &tauri::WebviewWindow) -> bool {
    let was = hidden_in_place().lock().unwrap_or_else(|p| p.into_inner()).remove(window.label());
    if !was {
        return false;
    }
    let _ = window.set_ignore_cursor_events(false);
    let _ = window.set_skip_taskbar(false);
    let target = window.clone();
    super::dialog::on_gtk_thread(move || {
        use gtk::prelude::*;
        if let Ok(gtk_window) = target.gtk_window() {
            gtk_window.set_opacity(1.0);
        }
    });
    let _ = window.set_always_on_top(true);
    true
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
    if window.label() == "main" && super::process::is_gnome() {
        let window = window.clone();
        super::dialog::on_gtk_thread(move || {
            use gtk::prelude::*;
            let Ok(window) = window.gtk_window() else { return };
            let context = window.style_context();
            if context.has_class("a2tools-overlay") { return; }
            let provider = gtk::CssProvider::new();
            if provider.load_from_data(b"window.a2tools-overlay decoration { box-shadow: none; margin: 0; border: 0; }").is_ok() {
                gtk::StyleContext::add_provider_for_screen(
                    &window.display().default_screen(), &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                );
                context.add_class("a2tools-overlay");
            }
        });
    }
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

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

/// GNOME and KDE resize the overlay through the compositor, on native
/// Wayland and X11. Elsewhere the page grows the window to the space right
/// and below it and resizes inside that; on KWin that space, measured from a
/// position Wayland does not give, made the window huge and KWin moved it:
/// the resize "glitched all over" (2026-10-17). KWin already resized the
/// meter's other windows this way.
pub fn compositor_resize_supported(window: &tauri::WebviewWindow) -> bool {
    // Hyprland too: resized by the app instead, a floating window keeps its
    // centre, so the meter jumped left as its corner was pressed (the window
    // grew for the drag) and back on release (#102). Its own resize keeps
    // the edge that isn't dragged.
    if !super::process::is_gnome() && !super::process::is_kde() && !super::process::is_hyprland() {
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

/// A tool window resizable from its outer edge, as other windows are. For a
/// window without a title bar tao takes the outer 5 px for the window
/// manager's resize: it shows the resize cursor there and starts the resize
/// on the press itself, and neither the page nor a motion handler hears of
/// it. The meter keeps
/// tool windows' size pinned (`set_size`, so KWin cannot tile one that is
/// dragged), and KWin refused the resize: the cursor offered one that never
/// came (CachyOS, 2026-10-28). So the size is unpinned, down to `min`, while
/// the pointer is in that band, with a frame to send the new hints (GTK sends
/// them only on a commit), and pinned again once it is back inside with no
/// button held: in a KDE Wayland VM, Battle History went from 1117 to 1231 px
/// wide, then KWin had it fixed at that size again.
pub fn resizable_edges(window: &tauri::WebviewWindow, min: tauri::LogicalSize<f64>) {
    use gtk::{gdk, glib, prelude::*};
    let target = window.clone();
    super::dialog::on_gtk_thread(move || {
        let Ok(gtk_window) = target.gtk_window() else { return };
        let unpinned = std::rc::Rc::new(std::cell::Cell::new(false));
        let set = {
            let unpinned = unpinned.clone();
            let target = target.clone();
            move |w: &gtk::ApplicationWindow, edge: bool| {
                if edge == unpinned.get() {
                    return;
                }
                unpinned.set(edge);
                if edge {
                    // Through tao, which keeps the limits itself and would put
                    // its own back over GTK hints set here.
                    release_size(&target, min);
                    w.queue_resize();
                    w.queue_draw();
                    if let Some(clock) = w.frame_clock() {
                        clock.request_phase(gdk::FrameClockPhase::LAYOUT | gdk::FrameClockPhase::PAINT);
                    }
                } else if let Ok(size) = target.inner_size() {
                    set_size(&target, tauri::Size::Physical(size));
                }
            }
        };
        // Where the pointer is, read ten times a second: tao's band passes no
        // motion on to other handlers. Pinned again only once the pointer is
        // back inside, not on its leaving: the window manager's resize takes
        // the pointer, and pinning then ended the resize as it began.
        let weak = gtk_window.downgrade();
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            let Some(w) = weak.upgrade() else { return glib::ControlFlow::Break };
            let Some(surface) = w.window() else { return glib::ControlFlow::Continue };
            let Some(pointer) = w.display().default_seat().and_then(|seat| seat.pointer()) else {
                return glib::ControlFlow::Continue;
            };
            let (_, x, y, held) = surface.device_position(&pointer);
            // A button held: a resize (or a drag) under way, the position
            // stale while the window manager has the pointer. It ends pinned
            // at its new size, on the next look after the release.
            if held.contains(gdk::ModifierType::BUTTON1_MASK) {
                return glib::ControlFlow::Continue;
            }
            let (width, height) = (surface.width(), surface.height());
            let inside = x >= 0 && y >= 0 && x < width && y < height;
            if !inside {
                return glib::ControlFlow::Continue;
            }
            let band = surface.scale_factor() * 5 + 1;
            let edge = !w.is_maximized() && (x < band || y < band || x >= width - band || y >= height - band);
            set(&w, edge);
            glib::ControlFlow::Continue
        });
    });
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
/// Whether the click-through lock works by an input region, needing no
/// pointer: here it does (`click_through_except`), on native Wayland too.
pub fn click_through_by_region() -> bool {
    true
}

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
///
/// A window `hide_in_place` hid never takes the mouse, whatever asks: it is
/// invisible but still there, and took every click over it once a screenshot
/// tool left the front (`screenshot_tool` gives every window the mouse back).
pub fn click_through_except(window: &tauri::WebviewWindow, ignore: bool, keep: Option<(f64, f64, f64, f64, f64)>) -> bool {
    input_region(window, ignore, keep.map(|(x, y, w, h, scale)| (vec![(x, y, w, h)], scale)))
}

/// A plugin widget's click-through: as `click_through_except`, with any
/// number of rectangles taking the mouse (the parts it draws), so the gaps
/// between them and the room it keeps for more let clicks through.
pub fn widget_click_through(window: &tauri::WebviewWindow, ignore: bool, keep: Option<(&[(f64, f64, f64, f64)], f64)>) -> bool {
    input_region(window, ignore, keep.map(|(rects, scale)| (rects.to_vec(), scale)))
}

/// Rectangles in CSS px, and the page's scale.
type Keep = (Vec<(f64, f64, f64, f64)>, f64);

/// The window's input region: everywhere (`ignore` false), the union of
/// `keep`'s rectangles, or a single pixel (nothing to keep).
fn input_region(window: &tauri::WebviewWindow, ignore: bool, keep: Option<Keep>) -> bool {
    use gtk::{cairo, prelude::*};
    click_through_asked().lock().unwrap_or_else(|p| p.into_inner()).insert(window.label().to_string(), (ignore, keep.clone()));
    let (ignore, keep) = if is_hidden_in_place(window) { (true, None) } else { (ignore, keep) };
    let window = window.clone();
    super::dialog::on_gtk_thread(move || {
        let gtk_window = window.gtk_window().ok()?;
        if !ignore {
            gtk_window.input_shape_combine_region(None);
            return Some(());
        }
        let surface = gtk_window.window()?;
        let region = cairo::Region::create();
        if let Some((rects, page_scale)) = keep {
            // CSS pixels to GDK's: the page's scale over GDK's own.
            let k = page_scale / f64::from(gtk_window.scale_factor().max(1));
            for (x, y, w, h) in rects {
                let (left, top) = ((x * k).floor() as i32, (y * k).floor() as i32);
                let (right, bottom) = (((x + w) * k).ceil() as i32, ((y + h) * k).ceil() as i32);
                if right > left && bottom > top {
                    let _ = region.union_rectangle(&cairo::RectangleInt::new(left, top, right - left, bottom - top));
                }
            }
        }
        if region.is_empty() {
            let _ = region.union_rectangle(&cairo::RectangleInt::new(0, 0, 1, 1));
        }
        surface.input_shape_combine_region(&region, 0, 0);
        Some(())
    })
    .flatten()
    .is_some()
}

/// Reject a delayed Open when the desktop reports another app in front.
/// Unknown focus tracking keeps the compositor's existing decision.
pub fn user_focus_unchanged(_before: Option<isize>) -> bool {
    front_is_ours_or_game()
}

/// A recent explicit configuration Open asks the compositor for focus.
/// There is no portable foreground handle here; its focus policy decides.
pub fn activate_from_user(window: &tauri::WebviewWindow, _before: Option<isize>) {
    let _ = window.set_focus();
}

pub fn start_drag(window: &tauri::WebviewWindow) {
    let _ = window.start_dragging();
}

/// A tool window behind others, brought in front (see the Windows one). The
/// compositor decides whether it takes focus.
pub fn bring_forward(window: &tauri::WebviewWindow) {
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_always_on_top(true);
    let _ = window.set_focus();
}

/// Show a window on top without taking focus from the game. A plain show
/// maps the window and the window manager activates it as a new window
/// (KWin did, under XWayland, for each plugin alert), so GTK is told first
/// not to take focus on map (`_NET_WM_USER_TIME` 0 on X11). Clicking the
/// window still focuses it.
///
/// On KDE a window the meter hid is unmapped (`hide_in_place`), and shown
/// again it is a new surface to KWin: it would take the focus (on Wayland
/// GTK's hint does nothing), so the KWin script hands it back
/// (`focus::note_quiet_show`). And the input region its click-through set
/// went with the old surface, so a locked overlay or icon took the mouse
/// again until something set it anew (Hirai, 2026-10-24): it is set again
/// once the window is back.
pub fn show_on_top_without_focus(window: &tauri::WebviewWindow) {
    use gtk::prelude::*;
    if let Ok(title) = window.title() {
        super::focus::note_quiet_show(&title);
    }
    let target = window.clone();
    super::dialog::on_gtk_thread(move || {
        if let Ok(gtk_window) = target.gtk_window() {
            gtk_window.set_focus_on_map(false);
        }
    });
    let _ = window.show();
    let _ = window.set_always_on_top(true);
    reapply_click_through(window);
}

/// The click-through each window last asked for (`click_through_except`):
/// what `reapply_click_through` sets again.
fn click_through_asked() -> &'static std::sync::Mutex<std::collections::HashMap<String, (bool, Option<Keep>)>> {
    static ASKED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, (bool, Option<Keep>)>>> =
        std::sync::OnceLock::new();
    ASKED.get_or_init(Default::default)
}

/// Set a window's last click-through again once it is mapped: shortly after
/// it is shown, and once more a little later in case the compositor took
/// its time.
fn reapply_click_through(window: &tauri::WebviewWindow) {
    let label = window.label().to_string();
    let window = window.clone();
    tauri::async_runtime::spawn(async move {
        for wait in [80, 400] {
            tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
            if !window.is_visible().unwrap_or(false) {
                return;
            }
            // What it asks for now, not when it was shown: a widget grown
            // by its page reports its new area in between, and the old one
            // put back left the new part of the window click-through.
            let asked = click_through_asked().lock().unwrap_or_else(|p| p.into_inner()).get(&label).cloned();
            let Some((ignore, keep)) = asked else { return };
            input_region(&window, ignore, keep);
        }
    });
}

/// Hide a window `show_on_top_without_focus` shows (see the Windows one).
pub fn hide_without_focus(window: &tauri::WebviewWindow) {
    let _ = window.hide();
}

/// Here a window can be built hidden and shown later as it is.
pub const HIDE_AFTER_BUILD: bool = false;

/// The windows `hide_in_place` has hidden, by label, each with whether it
/// stayed out of the taskbar before.
fn hidden_in_place() -> &'static std::sync::Mutex<std::collections::HashMap<String, bool>> {
    static HIDDEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, bool>>> = std::sync::OnceLock::new();
    HIDDEN.get_or_init(Default::default)
}

/// Hide a window without unmapping it: fully transparent, click-through and
/// out of the taskbar. A compositor places a window anew each time it is
/// mapped (KWin under the pointer, as a CachyOS player saw an alert bring a
/// plugin window up, 2026-10-16), and Wayland lets no client move its own
/// window back, so a window that stays mapped is the only one that stays where
/// it was left. Without a compositor nothing can be transparent, and it is
/// hidden as before.
///
/// Where KWin puts the meter's windows back as they appear (KDE on native
/// Wayland, `focus::kwin_places_windows`) a window is hidden for good: there
/// the invisible window, kept mapped and above everything (the plugin windows
/// rule), still took every click over it (#100).
pub fn hide_in_place(window: &tauri::WebviewWindow) {
    use gtk::prelude::*;
    if super::focus::kwin_places_windows() {
        hide_without_focus(window);
        return;
    }
    let target = window.clone();
    let concealed = super::dialog::on_gtk_thread(move || {
        let gtk_window = target.gtk_window().ok()?;
        // A window never shown has no GDK window yet, and tao unwraps it when
        // told to ignore the mouse: a panic in a GTK callback, so an abort (a
        // CachyOS player's meter, at a plugin's hide at start, 2026-10-16).
        // It has no place to keep either, so it is hidden as usual.
        if !gtk_window.is_visible() || gtk_window.window().is_none() {
            return None;
        }
        if !WidgetExt::screen(&gtk_window)?.is_composited() {
            return None;
        }
        gtk_window.set_opacity(0.0);
        Some(gtk_window.skips_taskbar_hint())
    })
    .flatten();
    let Some(skipped_taskbar) = concealed else {
        hide_without_focus(window);
        return;
    };
    hidden_in_place().lock().unwrap_or_else(|p| p.into_inner()).insert(window.label().to_string(), skipped_taskbar);
    click_through_except(window, true, None);
    let _ = window.set_skip_taskbar(true);
}

/// Show a window `hide_in_place` hid, where it is; false if it was not hidden
/// that way (then it is shown as usual).
pub fn reveal_in_place(window: &tauri::WebviewWindow) -> bool {
    let Some(skipped_taskbar) = hidden_in_place().lock().unwrap_or_else(|p| p.into_inner()).remove(window.label()) else {
        return false;
    };
    click_through_except(window, false, None);
    // Back as it was: a widget went into the taskbar and Alt+Tab once shown
    // again, and Alt+Tab then gave it the focus, the game's cursor gone and
    // its clicks not going through (Hirai, KDE, 2026-10-31).
    let _ = window.set_skip_taskbar(skipped_taskbar);
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

/// Keep the meter's windows where they were left (#76): on Wayland through
/// KWin, which may place windows where an application may not
/// (`focus::keep_places`). `save` gets a window's title and its place (x, y,
/// width, height); `load` answers where a window of that title belongs.
pub fn keep_places(
    save: impl Fn(&str, (i32, i32, i32, i32)) + Send + Sync + 'static,
    load: impl Fn(&str) -> Option<(i32, i32, i32, i32)> + Send + Sync + 'static,
) {
    super::focus::keep_places(super::focus::Places { save: Box::new(save), load: Box::new(load) });
}

/// Whether the meter places its own windows and KWin only its widgets
/// (`focus::places_own_windows`).
pub fn places_own_windows() -> bool {
    super::focus::places_own_windows()
}

/// The edge the overlay keeps as it fits its rows (Settings → Meter stays
/// fixed at), for the KWin script, which keeps it on native Wayland.
pub fn set_main_anchor(anchor: crate::platform::meter_anchor::Anchor) {
    super::focus::set_main_anchor(anchor);
}

/// Whether the overlay's next resize keeps its edge, or grows it for a moment
/// (a panel, the promo): the KWin script has it before the resize.
pub async fn anchor_next_resize(anchored: bool) {
    super::focus::anchor_next_resize(anchored).await;
}

/// Whether `hide_in_place` has this window hidden.
pub fn is_hidden_in_place(window: &tauri::WebviewWindow) -> bool {
    hidden_in_place().lock().unwrap_or_else(|p| p.into_inner()).contains_key(window.label())
}

/// Hide the meter while the game is not in front. Kept mapped (see
/// `hide_in_place`) rather than minimized: on Wayland an application cannot
/// bring its own minimized window back.
pub fn minimize_off_top(window: &tauri::WebviewWindow) {
    hide_in_place(window);
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

/// Size and move a window (`meter_anchor::resize`). Two requests here: Tauri
/// has no call for both.
pub fn set_size_and_position(window: &tauri::WebviewWindow, size: tauri::Size, pos: tauri::PhysicalPosition<i32>) {
    set_size(window, size);
    let _ = window.set_position(tauri::Position::Physical(pos));
}

/// Whether Settings offers "Meter stays fixed at": where the meter places its
/// own windows (XWayland), or KWin keeps the edge (KDE Wayland, the script
/// running). Not in an X11 session, nor on native Wayland elsewhere.
pub fn meter_anchor_available() -> bool {
    places_own_windows() || super::focus::kwin_places_windows()
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

/// Windows only (see the Windows version): here the window manager decides
/// what takes focus.
pub fn front_window() -> Option<isize> {
    None
}

/// The game or the meter in front, where the desktop says (KDE); else
/// assumed, so tool windows stay on top as before.
pub fn front_is_ours_or_game() -> bool {
    crate::platform::window_detector::is_aion2_foreground()
}

/// Windows only (see the Windows version).
pub fn never_activate(_window: &tauri::WebviewWindow, _before: Option<isize>) {}

/// Windows only (see the Windows version).
pub fn give_front_back(_window: &tauri::WebviewWindow, _before: Option<isize>) {}

/// Whether a tray icon can be shown. GTK's tray is an AppIndicator, whose
/// library (libayatana-appindicator, or the older libappindicator) is loaded
/// as the icon is built, and building one without it aborts the meter. Not
/// every desktop has it, so it is looked for first.
pub fn tray_supported() -> bool {
    ["libayatana-appindicator3.so.1", "libappindicator3.so.1"]
        .iter()
        // SAFETY: loading runs the library's initialisers only, as the
        // tray's own loading would.
        .any(|name| unsafe { libloading::Library::new(name) }.is_ok())
}

/// The open windows of each process group (`ProcessGroup`) and their WebKit
/// views: a new window of the group is related to one of them.
struct GroupView(String, webkit2gtk::WebView);
// SAFETY: as Tauri's own `WebviewAttributes` holding a related view: the
// view only travels in a builder to the GTK thread, which uses it there;
// elsewhere it is only cloned and dropped (GObject reference counting is
// thread-safe), and the list's reference goes on the GTK thread (`joined`).
unsafe impl Send for GroupView {}

static GROUP_VIEWS: std::sync::Mutex<[Vec<GroupView>; 2]> = std::sync::Mutex::new([Vec::new(), Vec::new()]);

fn group_slot(group: crate::platform::ProcessGroup) -> usize {
    match group {
        crate::platform::ProcessGroup::Meter => 0,
        crate::platform::ProcessGroup::Plugins => 1,
    }
}

/// A new window's page in its group's web process. WebKitGTK gives every
/// view its own process, each holding some 70-110 MB of its own on top of
/// the libraries every process maps, so each plugin widget cost a process:
/// a player with five plugins saw 1.1 GiB (2.0.81). Related views share one.
pub fn share_process<'a, R: tauri::Runtime, M: tauri::Manager<R>>(
    builder: tauri::WebviewWindowBuilder<'a, R, M>,
    group: crate::platform::ProcessGroup,
) -> tauri::WebviewWindowBuilder<'a, R, M> {
    let view = GROUP_VIEWS.lock().unwrap_or_else(|p| p.into_inner())[group_slot(group)].first().map(|v| v.1.clone());
    match view {
        Some(view) => builder.with_related_view(view),
        None => builder,
    }
}

/// A window of the group built: later windows of the group may share its
/// process, until it closes.
pub fn joined(window: &tauri::WebviewWindow, group: crate::platform::ProcessGroup) {
    let slot = group_slot(group);
    let label = window.label().to_string();
    let _ = window.with_webview(move |webview| {
        let view = webview.inner();
        let mut views = GROUP_VIEWS.lock().unwrap_or_else(|p| p.into_inner());
        if !views[slot].iter().any(|v| v.0 == label) {
            views[slot].push(GroupView(label, view));
        }
    });
    let label = window.label().to_string();
    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            GROUP_VIEWS.lock().unwrap_or_else(|p| p.into_inner())[slot].retain(|v| v.0 != label);
        }
    });
}

//! Esc and left clicks in AION 2 (`platform::game_input`) under Proton.
//!
//! The game is an X11 window (XWayland, or an X11 desktop), and X hands any
//! client a copy of the key presses and clicks the focused X11 window gets
//! (XInput2 raw events): nothing is grabbed, the game gets every key and click
//! as before. Only Esc and the first button are looked at, and only while the
//! game's window has the focus. Proton's native Wayland driver makes no X11
//! window: then there is no area, and nothing comes.
//!
//! The game's window is the X11 window titled "AION2…", as on Windows.

use std::os::raw::{c_int, c_uint, c_ulong};
use std::sync::{Mutex, Once, OnceLock};

use x11_dl::{keysym, xinput2, xlib};

use crate::platform::game_input::{Area, GameInput};

/// An X connection: the plugin thread's, asking where the game is (`CONN`,
/// closed when the thread ends), and the watch's while a plugin asks.
struct X {
    x: xlib::Xlib,
    display: *mut xlib::Display,
    root: c_ulong,
}

thread_local! {
    static CONN: Option<X> = X::open();
}

type ErrorHandler = Option<unsafe extern "C" fn(*mut xlib::Display, *mut xlib::XErrorEvent) -> c_int>;

/// Our connections: their X errors are ignored (a window gone between two
/// calls), where Xlib's default handler would end the meter.
fn ours() -> &'static Mutex<Vec<usize>> {
    static OURS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
    &OURS
}

/// The handler before ours (GDK's on X11, else Xlib's): every other
/// connection's errors still go to it.
static PREVIOUS: OnceLock<ErrorHandler> = OnceLock::new();

unsafe extern "C" fn on_error(display: *mut xlib::Display, event: *mut xlib::XErrorEvent) -> c_int {
    if ours().lock().unwrap_or_else(|p| p.into_inner()).contains(&(display as usize)) {
        return 0;
    }
    match PREVIOUS.get().copied().flatten() {
        Some(previous) => unsafe { previous(display, event) },
        None => 0,
    }
}

impl X {
    fn open() -> Option<X> {
        let x = xlib::Xlib::open().ok()?;
        let display = unsafe { (x.XOpenDisplay)(std::ptr::null()) };
        if display.is_null() {
            return None;
        }
        ours().lock().unwrap_or_else(|p| p.into_inner()).push(display as usize);
        static HANDLER: Once = Once::new();
        HANDLER.call_once(|| {
            let _ = PREVIOUS.set(unsafe { (x.XSetErrorHandler)(Some(on_error)) });
        });
        let root = unsafe { (x.XDefaultRootWindow)(display) };
        Some(X { x, display, root })
    }

    fn name(&self, window: c_ulong) -> Option<String> {
        let mut name = std::ptr::null_mut();
        if unsafe { (self.x.XFetchName)(self.display, window, &mut name) } == 0 || name.is_null() {
            return None;
        }
        let text = unsafe { std::ffi::CStr::from_ptr(name) }.to_string_lossy().into_owned();
        unsafe { (self.x.XFree)(name.cast()) };
        Some(text)
    }

    /// A window's parent and children.
    fn tree(&self, window: c_ulong) -> (c_ulong, Vec<c_ulong>) {
        let (mut root, mut parent, mut children, mut count) = (0, 0, std::ptr::null_mut(), 0 as c_uint);
        if unsafe { (self.x.XQueryTree)(self.display, window, &mut root, &mut parent, &mut children, &mut count) } == 0 {
            return (0, Vec::new());
        }
        let list = if children.is_null() {
            Vec::new()
        } else {
            let list = unsafe { std::slice::from_raw_parts(children, count as usize) }.to_vec();
            unsafe { (self.x.XFree)(children.cast()) };
            list
        };
        (parent, list)
    }

    fn is_game(&self, window: c_ulong) -> bool {
        self.name(window).is_some_and(|name| name.starts_with("AION2"))
    }

    /// The windows the window manager says it manages (_NET_CLIENT_LIST:
    /// KWin, Mutter, wlroots' and Hyprland's XWayland, Xfwm, Openbox, i3...),
    /// however deep in its frames it keeps them.
    fn clients(&self) -> Vec<c_ulong> {
        let atom = unsafe { (self.x.XInternAtom)(self.display, c"_NET_CLIENT_LIST".as_ptr(), xlib::True) };
        let (mut kind, mut format, mut count, mut left, mut data) = (0, 0, 0, 0, std::ptr::null_mut());
        if atom == 0
            || unsafe {
                (self.x.XGetWindowProperty)(self.display, self.root, atom, 0, 4096, xlib::False, xlib::XA_WINDOW,
                    &mut kind, &mut format, &mut count, &mut left, &mut data)
            } != 0
            || data.is_null()
        {
            return Vec::new();
        }
        // Format 32 comes as C longs, as many as `count`.
        let list = if format == 32 { unsafe { std::slice::from_raw_parts(data as *const c_ulong, count as usize) }.to_vec() } else { Vec::new() };
        unsafe { (self.x.XFree)(data.cast()) };
        list
    }

    /// The game's window: in the window manager's list or, with no such list,
    /// a top-level window or one just inside it (a frame's).
    fn game(&self) -> Option<c_ulong> {
        let clients = self.clients();
        if !clients.is_empty() {
            return clients.into_iter().find(|&w| self.is_game(w));
        }
        self.tree(self.root).1.into_iter().find_map(|top| {
            std::iter::once(top).chain(self.tree(top).1).find(|&w| self.is_game(w))
        })
    }

    fn area(&self, window: c_ulong) -> Option<Area> {
        let mut attributes: xlib::XWindowAttributes = unsafe { std::mem::zeroed() };
        if unsafe { (self.x.XGetWindowAttributes)(self.display, window, &mut attributes) } == 0 {
            return None;
        }
        let (mut x, mut y, mut child) = (0, 0, 0);
        unsafe { (self.x.XTranslateCoordinates)(self.display, window, self.root, 0, 0, &mut x, &mut y, &mut child) };
        let (width, height) = (attributes.width, attributes.height);
        (width > 0 && height > 0).then_some(Area { x, y, width, height })
    }

    /// The game's window when it has the focus: the focused window, or the
    /// first titled one above it, is the game's.
    fn focused_game(&self) -> Option<c_ulong> {
        let (mut window, mut revert) = (0, 0);
        unsafe { (self.x.XGetInputFocus)(self.display, &mut window, &mut revert) };
        for _ in 0..4 {
            // None (0), PointerRoot (1) or the root: not on a window.
            if window <= 1 || window == self.root {
                return None;
            }
            if let Some(name) = self.name(window) {
                return name.starts_with("AION2").then_some(window);
            }
            window = self.tree(window).0;
        }
        None
    }

    /// Where the pointer is on the root window.
    fn pointer(&self) -> Option<(i32, i32)> {
        let (mut root, mut child) = (0, 0);
        let (mut x, mut y, mut wx, mut wy, mut mask) = (0, 0, 0, 0, 0);
        let on_screen = unsafe {
            (self.x.XQueryPointer)(self.display, self.root, &mut root, &mut child, &mut x, &mut y, &mut wx, &mut wy, &mut mask)
        };
        (on_screen != 0).then_some((x, y))
    }

    /// Hand `on_input` the game's Esc presses and clicks while `on()`, seen at
    /// each key or click (the connection closes then). The X server gone,
    /// Xlib ends the meter as it would for GDK's own connection on X11.
    fn listen(&self, on_input: &dyn Fn(GameInput), on: &dyn Fn() -> bool) -> Result<(), String> {
        let xi = xinput2::XInput2::open().map_err(|e| format!("no XInput2 library ({e})"))?;
        let (mut opcode, mut first_event, mut first_error) = (0, 0, 0);
        let found = unsafe {
            (self.x.XQueryExtension)(self.display, c"XInputExtension".as_ptr(), &mut opcode, &mut first_event, &mut first_error)
        };
        let (mut major, mut minor) = (2, 0);
        if found == 0 || unsafe { (xi.XIQueryVersion)(self.display, &mut major, &mut minor) } != 0 {
            return Err("the X server has no XInput 2".into());
        }
        let mut mask = [0u8; 4];
        for event in [xinput2::XI_RawKeyPress, xinput2::XI_RawButtonPress] {
            mask[(event / 8) as usize] |= 1 << (event % 8);
        }
        let mut select = xinput2::XIEventMask {
            deviceid: xinput2::XIAllMasterDevices,
            mask_len: mask.len() as c_int,
            mask: mask.as_mut_ptr(),
        };
        unsafe {
            (xi.XISelectEvents)(self.display, self.root, &mut select, 1);
            (self.x.XFlush)(self.display);
        }
        let escape = c_int::from(unsafe { (self.x.XKeysymToKeycode)(self.display, c_ulong::from(keysym::XK_Escape)) });
        tracing::debug!("Game input: watching Esc and clicks in the game (XInput2)");
        let mut event: xlib::XEvent = unsafe { std::mem::zeroed() };
        while on() {
            unsafe { (self.x.XNextEvent)(self.display, &mut event) };
            let cookie = unsafe { &mut event.generic_event_cookie };
            if cookie.type_ != xlib::GenericEvent
                || cookie.extension != opcode
                || unsafe { (self.x.XGetEventData)(self.display, cookie) } == 0
            {
                continue;
            }
            let (kind, detail) = {
                let raw = unsafe { &*(cookie.data as *const xinput2::XIRawEvent) };
                (raw.evtype, raw.detail)
            };
            unsafe { (self.x.XFreeEventData)(self.display, cookie) };
            let escape_down = kind == xinput2::XI_RawKeyPress && detail == escape;
            let click = kind == xinput2::XI_RawButtonPress && detail == 1;
            if !(escape_down || click) {
                continue;
            }
            let Some(game) = self.focused_game() else { continue };
            if !on() {
                break;
            }
            if escape_down {
                on_input(GameInput::Escape);
            } else if let (Some(area), Some((x, y))) = (self.area(game), self.pointer()) {
                on_input(GameInput::Click { x: x - area.x, y: y - area.y, area });
            }
        }
        tracing::debug!("Game input: no plugin asks, the watch stops");
        Ok(())
    }
}

impl Drop for X {
    fn drop(&mut self) {
        ours().lock().unwrap_or_else(|p| p.into_inner()).retain(|&d| d != self.display as usize);
        unsafe { (self.x.XCloseDisplay)(self.display) };
    }
}

pub fn game_area() -> Option<Area> {
    CONN.with(|conn| {
        let x = conn.as_ref()?;
        x.area(x.game()?)
    })
}

/// Hand `on_input` Esc and clicks in the game while `on()`, on a connection
/// of its own, closed when it returns.
pub fn watch_while(on_input: &dyn Fn(GameInput), on: &dyn Fn() -> bool) -> Result<(), String> {
    X::open().ok_or("no X display")?.listen(on_input, on)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use x11_dl::xtest::Xf86vmode as XTest;

    /// Esc and a click come only while a plugin asks and the game's window
    /// has the focus, even deep in a window manager's frames (Openbox keeps a
    /// window in a plate in a frame). Run on an X server of its own, never
    /// the desktop's:
    /// `xvfb-run -a cargo test --no-default-features --features desktop game_input -- --ignored`
    #[test]
    #[ignore = "needs an X server of its own (xvfb-run)"]
    fn esc_and_clicks_in_the_game_only() {
        let x = X::open().expect("an X display");
        let t = XTest::open().expect("libXtst");
        let d = x.display;
        let window = |parent, at: (i32, i32), size: (u32, u32)| unsafe {
            let w = (x.x.XCreateSimpleWindow)(d, parent, at.0, at.1, size.0, size.1, 0, 0, 0);
            (x.x.XMapWindow)(d, w);
            w
        };
        let frame = window(x.root, (100, 50), (660, 400));
        let plate = window(frame, (0, 0), (660, 400));
        let game = window(plate, (5, 20), (640, 360));
        let other = window(x.root, (900, 50), (200, 200));
        unsafe {
            (x.x.XStoreName)(d, game, c"AION2  ".as_ptr());
            (x.x.XStoreName)(d, other, c"Terminal".as_ptr());
            // What the window manager says it manages.
            let atom = (x.x.XInternAtom)(d, c"_NET_CLIENT_LIST".as_ptr(), xlib::False);
            let clients = [other, game];
            (x.x.XChangeProperty)(d, x.root, atom, xlib::XA_WINDOW, 32, xlib::PropModeReplace, clients.as_ptr().cast(), 2);
            (x.x.XSync)(d, xlib::False);
        }
        let area = Area { x: 105, y: 70, width: 640, height: 360 };
        assert_eq!(game_area(), Some(area));

        let (tx, rx) = std::sync::mpsc::channel();
        crate::platform::game_input::start(move |input| {
            let _ = tx.send(input);
        });
        let ask = |on| {
            crate::platform::game_input::set_on(on);
            std::thread::sleep(Duration::from_millis(300)); // listening, or not
        };
        ask(true);
        let escape = c_uint::from(unsafe { (x.x.XKeysymToKeycode)(d, c_ulong::from(keysym::XK_Escape)) });
        let press_escape = || unsafe {
            (t.XTestFakeKeyEvent)(d, escape, xlib::True, 0);
            (t.XTestFakeKeyEvent)(d, escape, xlib::False, 0);
            (x.x.XSync)(d, xlib::False);
        };
        let click_at = |px, py| unsafe {
            (t.XTestFakeMotionEvent)(d, -1, px, py, 0);
            (t.XTestFakeButtonEvent)(d, 1, xlib::True, 0);
            (t.XTestFakeButtonEvent)(d, 1, xlib::False, 0);
            (x.x.XSync)(d, xlib::False);
        };
        let focus = |w| unsafe {
            (x.x.XSetInputFocus)(d, w, xlib::RevertToParent, xlib::CurrentTime);
            (x.x.XSync)(d, xlib::False);
        };
        let next = || rx.recv_timeout(Duration::from_millis(500)).ok();

        focus(game);
        press_escape();
        assert_eq!(next(), Some(GameInput::Escape));
        click_at(135, 110);
        assert_eq!(next(), Some(GameInput::Click { x: 30, y: 40, area }));
        // Another window in front: neither its Esc nor its clicks.
        focus(other);
        press_escape();
        click_at(950, 100);
        assert_eq!(next(), None);
        // No plugin asking: nothing, and again once one asks.
        focus(game);
        ask(false);
        press_escape();
        assert_eq!(next(), None);
        ask(true);
        press_escape();
        assert_eq!(next(), Some(GameInput::Escape));
    }
}

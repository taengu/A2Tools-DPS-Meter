//! Which window is active, on KDE. Wayland tells no application about another
//! application's windows, but KWin runs scripts and a script can call D-Bus:
//! the one loaded here tells the meter each time the active window changes,
//! for "Hide when AION2 isn't focused". Other desktops are not covered.
//!
//! On Wayland it also keeps the meter's windows where they were left (#76):
//! no application may place its own windows there, but a KWin script may.
//! The script reports where each of the meter's windows was moved or resized
//! to, and puts each back there when it appears again. It also keeps the
//! overlay's top, centre or bottom in place as it fits its rows (Settings →
//! Meter stays fixed at).

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::platform::kwin_rules::OVERLAY_TITLE;
use crate::platform::meter_anchor::Anchor;

const SERVICE: &str = "app.a2tools.DpsMeter";
const PATH: &str = "/Focus";
/// The script's name in KWin, so a script left by an earlier run is replaced.
const PLUGIN: &str = "a2tools-dps-meter-focus";

/// Values are passed as strings: KWin hands JavaScript numbers to D-Bus as
/// doubles, which an integer argument would refuse. KWin 6 has
/// `windowActivated` and `windowAdded`, KWin 5 `clientActivated` and
/// `clientAdded`. `__PID__` is the meter's process id: only its windows are
/// placed. `__OVERLAY_TITLE__` is the overlay's title and `__ANCHOR__` the
/// edge it keeps (`set_main_anchor`).
const SCRIPT: &str = r#"// A2Tools DPS Meter: tells the meter which window is active, so it can hide
// while the game is not in front, and keeps the meter's own windows where
// they were left. Loaded by the meter at each start.
var SERVICE = "app.a2tools.DpsMeter", PATH = "/Focus", IFACE = "app.a2tools.DpsMeter.Focus";
var MY_PID = __PID__;
function report(w) {
    callDBus(SERVICE, PATH, IFACE, "Active",
             w ? String(w.pid) : "0", w ? String(w.resourceClass) : "", w ? String(w.caption) : "");
}
function mine(w) {
    return !!w && Number(w.pid) === MY_PID;
}
// A plugin's widget (an alert's icon), titled "<plugin> · <name>".
function widget(w) {
    return mine(w) && String(w.caption).indexOf(" · ") >= 0;
}
// The last window not ours to be active, and our widgets that just appeared:
// Wayland activates a new window, so each icon an alert opened took the
// focus from the game (a tester, mid-PvP, 2026-10-17). An icon activated
// because it appeared hands the focus straight back; one clicked keeps it.
var lastOther = null;
var fresh = [];
// The active window and the one before it. KWin activates a new window
// before it announces it (windowActivated, then windowAdded: seen on KDE
// Plasma 6, 2026-10-19), so an icon is already active when follow() meets it.
var current = null, previous = null;
function activate(w) {
    if (workspace.windowActivated) workspace.activeWindow = w;
    else workspace.activeClient = w;
}
function onActivated(w) {
    if (w !== current) {
        previous = current;
        current = w;
    }
    if (w && !mine(w)) lastOther = w;
    var i = fresh.indexOf(w);
    if (i >= 0) {
        fresh.splice(i, 1);
        if (lastOther) {
            activate(lastOther);
            return;
        }
    }
    report(w);
}
// The meter's overlay, which sizes itself to its rows.
function overlay(w) {
    return String(w.caption) === "__OVERLAY_TITLE__";
}
// The height the overlay last kept its edge at (`place`): what it is saved
// with, so a panel open as it hides does not come back as its height.
var overlayHeight = 0;
function remember(w) {
    var g = w.frameGeometry;
    var h = overlay(w) && overlayHeight > 0 ? overlayHeight : Math.round(g.height);
    callDBus(SERVICE, PATH, IFACE, "Geometry", String(w.caption), String(Math.round(g.x)), String(Math.round(g.y)),
             String(Math.round(g.width)), String(h));
}
function onScreen(r) {
    var area = workspace.virtualScreenGeometry;
    if (!area) return true;
    // Enough of its top edge on screen to be seen and dragged back.
    return r.x + 48 <= area.x + area.width && r.x + r.width >= area.x + 48
        && r.y >= area.y - 8 && r.y + 32 <= area.y + area.height;
}
// How long a window is held at its place: the meter sizes its windows to
// their content just after they appear, and KWin keeps a resized window's
// centre where it was, so the overlay, 300 px tall at first, came down
// 120 px once it shrank to its rows (a CachyOS player, 2026-10-19).
var HOLD_MS = 4000;
// The edge the overlay keeps as it fits its rows (Settings → Meter stays
// fixed at): "top", "middle" or "bottom", written in at load, and whether a
// panel or the promo has it grown for a moment. The meter answers "Anchor"
// only once that is not what the script has, so a change is here at once and
// no resize waits on D-Bus for it.
var mainAnchor = "__ANCHOR__", mainGrown = false;
function followAnchor(known) {
    callDBus(SERVICE, PATH, IFACE, "Anchor", known, function (answer) {
        var told = String(answer).split(" ");
        mainAnchor = told[0];
        mainGrown = told[1] === "grown";
        followAnchor(String(answer));
    });
}
function place(w) {
    callDBus(SERVICE, PATH, IFACE, "Placement", String(w.caption), function (answer) {
        var p = String(answer).split(",").map(Number);
        if (p.length !== 4 || p.some(isNaN)) return;
        var g = w.frameGeometry;
        // The overlay sizes itself: its height here is the one it last kept
        // its edge at (`remember`), not a size to give it.
        var main = overlay(w);
        // A width or height of 0: the window keeps its own size.
        var r = { x: p[0], y: p[1], width: p[2] > 0 ? p[2] : g.width, height: p[3] > 0 && !main ? p[3] : g.height };
        // The overlay's bottom where it was left. It appears 300 px tall and
        // then fits its rows, so with Bottom it goes where its bottom was, not
        // 300 px below its top: each start moved it down by the difference.
        var seen = Math.round(r.height);
        var bottom = r.y + (main && p[3] > 0 ? p[3] : seen);
        if (main && mainAnchor === "bottom" && !mainGrown) r.y = bottom - seen;
        // Its top, and the height it last kept its edge at: a panel or the
        // promo grows it for a moment from its top, and it goes back from there.
        var top = r.y, keptHeight = bottom - r.y;
        var placed = onScreen(r);
        if (main) overlayHeight = placed ? keptHeight : 0;
        if (!placed) return;
        w.frameGeometry = r;
        // A widget's size as last seen, for a resize to keep its top left.
        var widgetSize = Math.round(w.frameGeometry.width) + "x" + Math.round(w.frameGeometry.height), resizedAt = 0;
        // Moved by a resize while held: back to its place, at its new size.
        // Never against the player's own move or resize.
        var until = Date.now() + HOLD_MS;
        var held = false;
        var userMove = false;
        var start = w.interactiveMoveResizeStarted || w.clientStartUserMovedResized;
        if (start) {
            start.connect(function () {
                userMove = true;
                until = 0;
            });
        }
        var done = w.interactiveMoveResizeFinished || w.clientFinishUserMovedResized;
        if (done) {
            done.connect(function () {
                userMove = false;
                var now = w.frameGeometry;
                seen = Math.round(now.height);
                top = Math.round(now.y);
                // The player sized it: its edge is kept from this height.
                if (main && !mainGrown) keptHeight = overlayHeight = seen;
                // Or moved it: its top left is kept from here.
                if (!main) {
                    r = { x: Math.round(now.x), y: top };
                    widgetSize = Math.round(now.width) + "x" + seen;
                }
            });
        }
        w.frameGeometryChanged.connect(function () {
            if (held || userMove || w.move || w.resize) return;
            var now = w.frameGeometry;
            if (!main) {
                var nx = Math.round(now.x), ny = Math.round(now.y);
                var size = Math.round(now.width) + "x" + Math.round(now.height);
                // Held, it stays at its place. After, a move by KWin alone is
                // its place now, but a resize keeps its top left: KWin keeps
                // a resized window's centre, and a plugin sizes its widget
                // again as the game tells it more (Cooldowns once the class
                // and its skills come, long after the hold when the game was
                // just started): the buffs slid onto the skills (#177).
                // (The move can come as a change of its own just after.)
                if (size !== widgetSize) resizedAt = Date.now();
                widgetSize = size;
                if (nx === r.x && ny === r.y) return;
                if (Date.now() > until && Date.now() - resizedAt > 500) {
                    r = { x: nx, y: ny };
                    return;
                }
                held = true;
                w.frameGeometry = { x: r.x, y: r.y, width: now.width, height: now.height };
                held = false;
                return;
            }
            var x = Math.round(now.x), y = Math.round(now.y), h = Math.round(now.height);
            var holding = Date.now() <= until;
            var resized = h !== seen;
            seen = h;
            // Held, it stays at its place as any window does; after, a move by
            // KWin is its place now. A resize keeps the top, or with Bottom the
            // bottom it had at keptHeight; grown for a moment, it grows from
            // its top. Centre: KWin keeps the centre itself once not held.
            var wantX = holding ? r.x : x;
            var wantY = holding ? top : y;
            if (resized) {
                if (mainAnchor === "bottom" && !mainGrown) wantY = top + keptHeight - h;
                else if (mainAnchor !== "middle" || holding) wantY = top;
                if (!mainGrown) keptHeight = overlayHeight = h;
            }
            top = wantY;
            // Scale rounding can report +/- 1 px between events.
            if (Math.abs(x - wantX) <= 1 && Math.abs(y - wantY) <= 1) return;
            held = true;
            w.frameGeometry = { x: wantX, y: wantY, width: now.width, height: now.height };
            held = false;
        });
    });
}
function follow(w) {
    if (!mine(w)) return;
    // An icon already active as it is added: KWin activated it first. The
    // focus goes back to the window active before it. (The KWin rule for
    // icons keeps them from taking it at all; this is for when the rule is
    // missing, as KWin can lose it.)
    var aw = workspace.activeWindow || workspace.activeClient;
    if (widget(w) && aw === w && previous && previous !== w && !previous.deleted) activate(previous);
    else if (widget(w)) {
        fresh.push(w);
        if (fresh.length > 32) fresh.shift();
    } else {
        // Another of the meter's windows the meter showed on its own (back
        // with the game after hiding, a plugin showing itself): KWin takes it
        // for a new window and activates it, so it took the focus from the
        // game (2.0.76, now that hidden windows are unmapped). It hands the
        // focus back, as an icon does; one the player opened keeps it.
        callDBus(SERVICE, PATH, IFACE, "Quiet", String(w.caption), function (answer) {
            if (String(answer) !== "true") return;
            var now = workspace.activeWindow || workspace.activeClient;
            if (now === w && lastOther && !lastOther.deleted) activate(lastOther);
            else {
                fresh.push(w);
                if (fresh.length > 32) fresh.shift();
            }
        });
    }
    var done = w.interactiveMoveResizeFinished || w.clientFinishUserMovedResized;
    if (done) done.connect(function () { remember(w); });
    place(w);
}
// A window of the meter going away (hidden: on KDE the meter unmaps the
// windows it hides) leaves its place behind, so it comes back there.
function gone(w) {
    if (mine(w) && onScreen(w.frameGeometry)) remember(w);
}
if (workspace.windowRemoved) workspace.windowRemoved.connect(gone);
else if (workspace.clientRemoved) workspace.clientRemoved.connect(gone);
followAnchor(mainAnchor);
if (workspace.windowActivated) {
    workspace.windowActivated.connect(onActivated);
    report(workspace.activeWindow);
    workspace.windowAdded.connect(follow);
    workspace.windowList().forEach(follow);
} else {
    workspace.clientActivated.connect(onActivated);
    report(workspace.activeClient);
    workspace.clientAdded.connect(follow);
    workspace.clientList().forEach(follow);
}
"#;

/// A window's place and size, as KWin gives them: x, y, width, height.
pub type Rect = (i32, i32, i32, i32);

/// Where the meter keeps the places KWin reports, by window title.
pub struct Places {
    pub save: Box<dyn Fn(&str, Rect) + Send + Sync>,
    pub load: Box<dyn Fn(&str) -> Option<Rect> + Send + Sync>,
}

static PLACES: OnceLock<Places> = OnceLock::new();

/// The meter runs on X11 (`GDK_BACKEND=x11`, XWayland) in a Wayland session.
/// It places and remembers its own windows then, but KWin keeps a new X11
/// window inside the X11 work area: one rectangle for every monitor, its
/// bottom the top of the lowest panel on any of them. So a widget left below
/// that line on another monitor came back above it at each start (Ping on a
/// 1440 px monitor, a panel on a 1080 px one: back at the 1232 line). KWin
/// places only widgets then, where the meter left them (`load`).
static X11_IN_WAYLAND: AtomicBool = AtomicBool::new(false);

/// The script is running, so KWin puts each of the meter's windows back where
/// it was each time it appears (`place` in the script).
static PLACING: AtomicBool = AtomicBool::new(false);
/// The edge the overlay keeps as it fits its rows (Settings → Meter stays
/// fixed at): 0 top, 1 middle, 2 bottom. The script has it from the start and
/// is told when it changes (`Focus::anchor`).
static MAIN_ANCHOR: AtomicU8 = AtomicU8::new(1);
/// Wakes a held `Focus::anchor` when the setting changes.
static ANCHOR_CHANGED: std::sync::LazyLock<tokio::sync::Notify> = std::sync::LazyLock::new(tokio::sync::Notify::new);
/// How long `Focus::anchor` holds the script's call before it answers with
/// the edge unchanged: KWin gives up on a D-Bus call after 25 s and then never
/// calls the script back, which would not hear of a change again.
const ANCHOR_HOLD: std::time::Duration = std::time::Duration::from_secs(20);
/// The overlay is grown for a moment (a panel, the promo): it grows from its
/// top, and its edge waits for it to shrink back, as where the meter moves it
/// itself (`meter_anchor::resize`).
static MAIN_GROWN: AtomicBool = AtomicBool::new(false);
/// What the script last said it has (`Focus::anchor`), for `tell_grown`.
static HEARD: std::sync::LazyLock<tokio::sync::watch::Sender<String>> =
    std::sync::LazyLock::new(|| tokio::sync::watch::Sender::new(String::new()));
/// How long `tell_grown` waits for the script: a D-Bus round trip, a frame or
/// two of KWin's.
const HEARD_WAIT: std::time::Duration = std::time::Duration::from_millis(200);

/// Keep the meter's windows where they were left, through KWin (#76). Only
/// in a Wayland session: under X11 the meter places its own windows, as on
/// Windows (in a Wayland session too, but for its widgets: X11_IN_WAYLAND).
pub fn keep_places(places: Places) {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        X11_IN_WAYLAND.store(places_own_windows(), Ordering::Relaxed);
        let _ = PLACES.set(places);
    }
}

/// Whether the meter places and remembers its own windows, KWin placing only
/// its widgets (see X11_IN_WAYLAND). Read from the environment, which is set
/// before any window opens, so it is right from the start and not only once
/// `keep_places` has run on its thread.
pub fn places_own_windows() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() && std::env::var("GDK_BACKEND").is_ok_and(|b| b.starts_with("x11"))
}

/// Whether KWin puts the meter's windows back where they were whenever they
/// appear: on native Wayland in KDE, with the script running. A window can
/// then be hidden for good and shown again at its place.
pub fn kwin_places_windows() -> bool {
    PLACING.load(Ordering::Relaxed) && PLACES.get().is_some() && !X11_IN_WAYLAND.load(Ordering::Relaxed)
}

/// Whether the meter's windows can be hidden for good on Hyprland. It places a
/// window again each time one is mapped, and under XWayland the meter places
/// its own again (`places_own_windows`), so `hide_in_place` hides for real
/// there. Concealing it instead leaves it on screen: Hyprland keeps drawing a
/// window left mapped at `_NET_WM_WINDOW_OPACITY` 0.
pub fn hyprland_places_windows() -> bool {
    super::process::is_hyprland()
}

/// Keep where a window is now, for one hidden for good that the meter has to
/// put back itself: through the store KWin's script fills (#76), so a window
/// is placed the same way on both (KWin says where a window was left, Hyprland
/// is asked by `hide_in_place` before the window goes).
pub fn save_place(window: &tauri::WebviewWindow) {
    let (Ok(title), Ok(at), Ok(size)) = (window.title(), window.outer_position(), window.outer_size()) else {
        return;
    };
    // A window mapped but not placed yet has no size (and sits at 0,0): kept,
    // that place would move it to the corner on the show.
    if size.width == 0 || size.height == 0 {
        return;
    }
    save_place_at(&title, (at.x, at.y, size.width as i32, size.height as i32));
}

/// `save_place` without the window, for the store alone.
fn save_place_at(title: &str, rect: Rect) {
    if let Some(places) = PLACES.get() {
        (places.save)(title, rect);
    }
}

/// Where a window belongs, as the meter last kept it (#76).
pub fn place_of(window: &tauri::WebviewWindow) -> Option<(i32, i32)> {
    place_of_title(&window.title().ok()?)
}

/// `place_of` without the window, for the store alone.
fn place_of_title(title: &str) -> Option<(i32, i32)> {
    let (x, y, _, _) = PLACES.get().and_then(|places| (places.load)(title))?;
    Some((x, y))
}

/// The edge the overlay keeps on KDE Wayland, where the script keeps it. A
/// held `Focus::anchor` answers at once.
pub fn set_main_anchor(anchor: Anchor) {
    let v = match anchor {
        Anchor::Top => 0,
        Anchor::Middle => 1,
        Anchor::Bottom => 2,
    };
    MAIN_ANCHOR.store(v, Ordering::Relaxed);
    ANCHOR_CHANGED.notify_waiters();
}

fn main_anchor() -> Anchor {
    match MAIN_ANCHOR.load(Ordering::Relaxed) {
        0 => Anchor::Top,
        2 => Anchor::Bottom,
        _ => Anchor::Middle,
    }
}

/// What the script is told: the edge, and " grown" while the overlay is.
fn main_state() -> String {
    let edge = main_anchor().name();
    if MAIN_GROWN.load(Ordering::SeqCst) { format!("{edge} grown") } else { edge.to_string() }
}

/// Whether the overlay's next resize keeps its edge, or grows it for a moment
/// (a panel, the promo). On KDE Wayland the script is told before the resize.
pub async fn anchor_next_resize(anchored: bool) {
    if kwin_places_windows() {
        tell_grown(!anchored).await;
    }
}

/// The script must have it before the resize reaches KWin, or with Bottom a
/// panel would open upwards and close a round trip late: the meter waits until
/// the script asks again with it.
async fn tell_grown(grown: bool) {
    if MAIN_GROWN.swap(grown, Ordering::SeqCst) == grown {
        return;
    }
    ANCHOR_CHANGED.notify_waiters();
    // Only Bottom tells them apart: Top keeps the top either way, and KWin
    // keeps Centre itself.
    if main_anchor() != Anchor::Bottom {
        return;
    }
    let want = main_state();
    let mut heard = HEARD.subscribe();
    let _ = tokio::time::timeout(HEARD_WAIT, heard.wait_for(|heard| *heard == want)).await;
}

/// 0: not known (no report yet, or not KDE). 1: the game or one of the
/// meter's own windows is active. 2: something else is.
static ACTIVE: AtomicU8 = AtomicU8::new(0);

/// The game's window has been reported active at least once. Until then the
/// reports are not trusted: the setting is on by default, and a game window
/// this does not recognise must not leave the meter hidden for good.
static SEEN_GAME: AtomicBool = AtomicBool::new(false);

/// The meter's windows it is showing on its own, by title, and since when:
/// they hand the focus back as they appear (`Focus::quiet`).
static QUIET: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>> =
    std::sync::LazyLock::new(Default::default);
/// How long a quiet show waits for its window to appear.
const QUIET_FOR: std::time::Duration = std::time::Duration::from_secs(3);

/// The meter is showing the window titled `title` on its own, not because the
/// player asked: it is not to take the focus (`Focus::quiet`).
pub fn note_quiet_show(title: &str) {
    if PLACING.load(Ordering::Relaxed) {
        QUIET.lock().unwrap_or_else(|p| p.into_inner()).insert(title.to_string(), std::time::Instant::now());
    }
}

/// The connection that serves `Focus`; dropped, the service would go away.
static CONNECTION: OnceLock<zbus::blocking::Connection> = OnceLock::new();

/// Told when the game or the meter gains or loses the front.
static ON_CHANGE: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// Whether a screenshot tool (Spectacle) is the active window, and who is
/// told when that changes (`watch_screenshot_tool`).
static SCREENSHOT: AtomicBool = AtomicBool::new(false);
static ON_SCREENSHOT: OnceLock<Box<dyn Fn(bool) + Send + Sync>> = OnceLock::new();

/// Be told when a screenshot tool becomes, or stops being, the active window.
pub fn watch_screenshot_tool(on_change: impl Fn(bool) + Send + Sync + 'static) {
    let _ = ON_SCREENSHOT.set(Box::new(on_change));
}

/// KDE's Spectacle, by its window class (Plasma 6: `org.kde.spectacle`,
/// Plasma 5: `spectacle`). Its area selection is drawn under the meter's
/// windows (the overlay layer, see `kwin_rules`), so they took the drag
/// (Zoe, CachyOS, 2026-10-20).
fn is_screenshot_tool(class: &str) -> bool {
    let class = class.trim().to_ascii_lowercase();
    class == "org.kde.spectacle" || class == "spectacle"
}

/// Whether the game (or the meter) is the active window, if KWin has said
/// and the game has been recognised.
pub fn game_or_meter_active() -> Option<bool> {
    if !SEEN_GAME.load(Ordering::Relaxed) {
        return None;
    }
    match ACTIVE.load(Ordering::Relaxed) {
        0 => None,
        1 => Some(true),
        _ => Some(false),
    }
}

struct Focus;

#[zbus::interface(name = "app.a2tools.DpsMeter.Focus")]
impl Focus {
    fn active(&self, pid: String, class: String, caption: String) {
        let shot = is_screenshot_tool(&class);
        if SCREENSHOT.swap(shot, Ordering::Relaxed) != shot {
            tracing::info!("KWin: screenshot tool {}", if shot { "in front: the meter lets clicks through" } else { "gone" });
            if let Some(on_screenshot) = ON_SCREENSHOT.get() {
                on_screenshot(shot);
            }
        }
        let who = classify(pid.parse().unwrap_or(0), &class, &caption);
        if who == Who::Game && !SEEN_GAME.swap(true, Ordering::Relaxed) {
            tracing::info!("KWin: the game's window is active (class {class:?})");
        }
        let was = ACTIVE.swap(if who == Who::Other { 2 } else { 1 }, Ordering::Relaxed);
        if was != ACTIVE.load(Ordering::Relaxed) && SEEN_GAME.load(Ordering::Relaxed) {
            if let Some(on_change) = ON_CHANGE.get() {
                on_change();
            }
        }
    }

    /// One of the meter's windows was moved or resized by the player.
    fn geometry(&self, caption: String, x: String, y: String, width: String, height: String) {
        // On X11 the meter saves its own windows' places as they move.
        if places_own_windows() {
            return;
        }
        let Some(places) = PLACES.get() else { return };
        let n = |v: &str| v.trim().parse::<i32>().ok();
        if let (Some(x), Some(y), Some(w), Some(h)) = (n(&x), n(&y), n(&width), n(&height))
            && w > 0
            && h > 0
        {
            (places.save)(&caption, (x, y, w, h));
        }
    }

    /// Whether the window titled `caption`, just appeared, was shown by the
    /// meter on its own (`note_quiet_show`), so it gives the focus back.
    fn quiet(&self, caption: String) -> bool {
        let mut quiet = QUIET.lock().unwrap_or_else(|p| p.into_inner());
        quiet.retain(|_, at| at.elapsed() < QUIET_FOR);
        quiet.remove(&caption).is_some()
    }

    /// Where one of the meter's windows belongs, `"x,y,width,height"`, or ""
    /// to leave it where KWin put it. The overlay's height is the one it had
    /// there, for the edge it keeps; it sizes itself.
    fn placement(&self, caption: String) -> String {
        PLACES
            .get()
            .and_then(|places| (places.load)(&caption))
            .map(|(x, y, w, h)| format!("{x},{y},{w},{h}"))
            .unwrap_or_default()
    }

    /// The edge the overlay keeps (Settings → Meter stays fixed at), and
    /// whether it is grown for a moment, once that is not `known`, what the
    /// script has: at once, when it changes (`set_main_anchor`,
    /// `anchor_next_resize`), or unchanged after ANCHOR_HOLD, for the script
    /// to ask again. So the script hears of a change as it is made, rather
    /// than asking at each resize.
    async fn anchor(&self, known: String) -> String {
        HEARD.send_replace(known.clone());
        let changed = ANCHOR_CHANGED.notified();
        if main_state() == known {
            // zbus runs this on its own executor, which has no timer.
            let _ = tauri::async_runtime::spawn(async move {
                let _ = tokio::time::timeout(ANCHOR_HOLD, changed).await;
            })
            .await;
        }
        main_state()
    }
}

#[derive(Debug, PartialEq)]
enum Who {
    Game,
    Meter,
    Other,
}

/// The game by title or class, as on Windows, or by its process; the meter by
/// its own process id, so opening Settings does not hide the overlay.
fn classify(pid: u32, class: &str, caption: &str) -> Who {
    if caption.starts_with("AION2") || class.eq_ignore_ascii_case("aion2.exe") {
        return Who::Game;
    }
    if pid == 0 {
        return Who::Other;
    }
    if pid == std::process::id() {
        return Who::Meter;
    }
    let dir = std::path::PathBuf::from(format!("/proc/{pid}"));
    let comm = std::fs::read_to_string(dir.join("comm")).unwrap_or_default();
    let cmdline = std::fs::read(dir.join("cmdline")).unwrap_or_default();
    if crate::platform::procfs::is_aion2_process(&comm, &cmdline) { Who::Game } else { Who::Other }
}

/// Serve `Focus` on the session bus and load the script into KWin. Does
/// nothing outside KDE: there the game is taken as always focused.
pub fn watch(on_change: impl Fn() + Send + Sync + 'static) -> Result<(), String> {
    if !super::process::is_kde() || CONNECTION.get().is_some() {
        return Ok(());
    }
    let connection = zbus::blocking::connection::Builder::session()
        .and_then(|b| b.name(SERVICE))
        .and_then(|b| b.serve_at(PATH, Focus))
        .and_then(|b| b.build())
        .map_err(|e| format!("D-Bus service: {e}"))?;

    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let path = dir.join("a2tools-dps-meter-focus.js");
    let script = SCRIPT
        .replace("__PID__", &std::process::id().to_string())
        .replace("__OVERLAY_TITLE__", OVERLAY_TITLE)
        .replace("__ANCHOR__", main_anchor().name());
    std::fs::write(&path, script).map_err(|e| format!("writing the script: {e}"))?;

    let kwin = zbus::blocking::Proxy::new(&connection, "org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting")
        .map_err(|e| format!("KWin scripting: {e}"))?;
    let _ = kwin.call_method("unloadScript", &(PLUGIN,));
    let id: i32 = kwin
        .call("loadScript", &(path.to_string_lossy().as_ref(), PLUGIN))
        .map_err(|e| format!("loading the script: {e}"))?;
    if id < 0 {
        return Err("KWin did not load the script".into());
    }
    kwin.call_method("start", &()).map_err(|e| format!("starting the script: {e}"))?;
    let _ = ON_CHANGE.set(Box::new(on_change));
    let _ = CONNECTION.set(connection);
    PLACING.store(true, Ordering::Relaxed);
    tracing::info!("KWin reports the active window (script {id})");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_place_kept_for_a_window_comes_back_from_the_store() {
        // hide_in_place keeps where a Hyprland window was in the store KWin's
        // script fills (#76), and the show after asks it back: the same store,
        // so a window is placed alike on both.
        static KEPT: OnceLock<std::sync::Mutex<std::collections::HashMap<String, Rect>>> = OnceLock::new();
        let kept = KEPT.get_or_init(Default::default);
        if PLACES.get().is_none() {
            let _ = PLACES.set(Places {
                save: Box::new(|title, rect| {
                    kept.lock().unwrap_or_else(|p| p.into_inner()).insert(title.to_string(), rect);
                }),
                load: Box::new(|title| kept.lock().unwrap_or_else(|p| p.into_inner()).get(title).copied()),
            });
        }
        save_place_at("A2Tools DPS Meter", (400, 300, 398, 55));
        assert_eq!(place_of_title("A2Tools DPS Meter"), Some((400, 300)));
        // A window the store never heard of is left where the compositor put it.
        assert_eq!(place_of_title("Settings"), None);
    }

    #[test]
    fn spectacle_is_known_by_its_class() {
        assert!(is_screenshot_tool("org.kde.spectacle"));
        assert!(is_screenshot_tool("spectacle"));
        assert!(!is_screenshot_tool("org.kde.konsole"));
        assert!(!is_screenshot_tool(""));
    }

    #[test]
    fn the_game_is_known_by_title_or_class_and_the_meter_by_itself() {
        assert_eq!(classify(0, "", "AION2"), Who::Game);
        assert_eq!(classify(0, "aion2.exe", ""), Who::Game);
        assert_eq!(classify(std::process::id(), "a2tools-dps-meter", "A2Tools DPS Meter"), Who::Meter);
        assert_eq!(classify(0, "firefox", "Mozilla Firefox"), Who::Other);
    }

    #[test]
    fn the_script_hears_of_a_change_at_once() {
        use tauri::async_runtime::block_on;
        set_main_anchor(Anchor::Top);
        MAIN_GROWN.store(false, Ordering::SeqCst);
        // Not what the script has: answered at once.
        assert_eq!(block_on(Focus.anchor("middle".into())), "top");
        // What it has: held until the setting changes.
        let change = std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            set_main_anchor(Anchor::Bottom);
        });
        let asked = std::time::Instant::now();
        assert_eq!(block_on(Focus.anchor("top".into())), "bottom");
        assert!(asked.elapsed() < ANCHOR_HOLD);
        change.join().unwrap();
        // A panel opening with Bottom: the meter goes on once the script asks
        // again with it, not after the wait, so the resize is taken for one.
        std::thread::spawn(|| {
            let mut known = "bottom".to_string();
            loop {
                known = block_on(Focus.anchor(known));
            }
        });
        let told = std::time::Instant::now();
        block_on(tell_grown(true));
        assert!(told.elapsed() < HEARD_WAIT);
        assert_eq!(*HEARD.borrow(), "bottom grown");
        let told = std::time::Instant::now();
        block_on(tell_grown(false));
        assert!(told.elapsed() < HEARD_WAIT);
        assert_eq!(*HEARD.borrow(), "bottom");
    }
}

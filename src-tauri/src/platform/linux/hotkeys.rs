//! Global hotkeys on Linux, through the desktop's GlobalShortcuts portal
//! (`org.freedesktop.portal.GlobalShortcuts`).
//!
//! Wayland lets no app grab keys for itself: the desktop owns them, and an app
//! asks the portal for named shortcuts. KDE Plasma 6 (Bazzite's desktop) shows
//! the meter's shortcuts with the keys it suggests and lets the user keep or
//! change them; GNOME 48 and later do the same. Hyprland's portal ignores the
//! suggested keys: the user binds each shortcut in their config with the
//! `global` dispatcher, by the id `hyprctl globalshortcuts` lists
//! (`com.a2tools.dps-meter:reset`), as docs/linux.md shows. With no portal, or
//! one without GlobalShortcuts, there are no hotkeys, as before.
//!
//! The keys are only suggestions here, so the meter's own Keybinds settings
//! decide what is suggested; the desktop decides what is bound.
//!
//! The portal work runs in a helper process, `a2tools-hotkeys`, never in the
//! meter. The portal reads a caller's /proc/<pid>/root, and Linux refuses that
//! to a program lacking the capabilities its target holds: the packaged meter
//! holds cap_net_raw (for capture) and the portal does not, so every call the
//! meter made was refused ("Unable to open /proc/<pid>/root", #42), dumpable
//! or not. The helper is a separate executable without file capabilities, and
//! a program the meter starts does not inherit the meter's, so the portal
//! reads it like any app. It reports back on stdout: `status <id> ok|taken`
//! and `fire <id>`.

use std::collections::HashMap;
use std::sync::Mutex;

use tracing::{info, warn};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";
/// The app id the shortcuts are filed under: Tauri's identifier. The .deb and
/// .rpm name their desktop file after the product ("A2Tools DPS Meter"),
/// which is no valid app id, so the identifier is registered instead.
const APP_ID: &str = "com.a2tools.dps-meter";

/// The shortcut ids, stable: Hyprland users bind them by name.
const RELOAD: &str = "reset";
const TOGGLE: &str = "toggle";
const LOCK: &str = "lock";
const PLUGINS: &str = "plugins";
const LAYOUT: &str = "layout";

/// A binding's shortcut id and the description the desktop shows.
fn shortcut_of(name: &str) -> Option<(&'static str, &'static str)> {
    Some(match name {
        "reload" => (RELOAD, "Reset the meter"),
        "toggle" => (TOGGLE, "Show or hide the meter"),
        "lock" => (LOCK, "Turn the click-through lock on or off"),
        "plugins" => (PLUGINS, "Bring back the open plugin windows"),
        "layout" => (LAYOUT, "Edit the layout of the meter and plugin widgets"),
        _ => return None,
    })
}

/// Off until the user turns them on (Settings → Keybinds): the desktop asks
/// to approve the shortcuts, which should not appear unasked.
pub const DEFAULT_ON: bool = false;

/// The helper's file name, next to the meter's own executable.
const HELPER: &str = "a2tools-hotkeys";

pub struct HotkeyManager {
    /// The running helper, if any.
    child: Mutex<Option<std::process::Child>>,
}

impl HotkeyManager {
    pub fn new() -> Self {
        Self {
            child: Mutex::new(None),
        }
    }

    /// Start the helper with the shortcuts and act on what it reports.
    /// The numbers are the Win32 modifier flags and virtual-key codes settings
    /// store (`parse_hotkey_label`); a key of 0 leaves that shortcut out.
    pub fn start(
        &self,
        bindings: Vec<crate::platform::hotkeys::Binding>,
        on_status: impl Fn(&'static str, &'static str, Option<String>) + Send + 'static,
    ) {
        use std::io::BufRead;
        use std::process::{Command, Stdio};

        let mut slot = self.child.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(child) = slot.as_mut() {
            if matches!(child.try_wait(), Ok(None)) {
                return;
            }
        }
        let mut args = Vec::new();
        for binding in &bindings {
            let Some((id, description)) = shortcut_of(binding.name) else { continue };
            if binding.vk != 0 {
                args.push("--shortcut".to_string());
                args.push(format!("{id}\t{description}\t{}", trigger(binding.mods, binding.vk).unwrap_or_default()));
            }
        }
        let path = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join(HELPER)))
            .filter(|path| path.exists())
            // Where the packages put it; never looked up on PATH (audit I1).
            .unwrap_or_else(|| std::path::Path::new("/usr/bin").join(HELPER));
        let mut child = match Command::new(&path)
            .args(&args)
            // Its stdin is how it learns the meter is gone: it exits at EOF.
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                warn!("Global hotkeys unavailable: cannot start {}: {e}", path.display());
                return;
            }
        };
        let Some(stdout) = child.stdout.take() else { return };
        *slot = Some(child);
        drop(slot);

        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                let mut parts = line.splitn(3, ' ');
                match (parts.next(), parts.next(), parts.next()) {
                    (Some("fire"), Some(id), _) => {
                        let fired = bindings.iter().find(|b| shortcut_of(b.name).is_some_and(|(s, _)| s == id));
                        if let Some(binding) = fired {
                            (binding.on_fire)();
                        }
                    }
                    (Some("status"), Some(id), Some(rest)) => {
                        // "<state>" or "<state> <the key the desktop gave>".
                        let (state, assigned) = rest.split_once(' ').unwrap_or((rest, ""));
                        let state = match state {
                            "ok" => "ok",
                            "unset" => "unset",
                            _ => "taken",
                        };
                        let assigned = Some(assigned.trim().to_string()).filter(|a| !a.is_empty());
                        if let Some(key) = status_key(id) {
                            info!("Hotkey {key}: {state}{}", assigned.as_deref().map(|a| format!(" ({a})")).unwrap_or_default());
                            on_status(key, state, assigned);
                        }
                    }
                    (Some("error"), _, _) => warn!("Global hotkeys unavailable: {}", &line[6.min(line.len())..]),
                    _ => info!("Hotkeys helper: {line}"),
                }
            }
        });
    }

    /// Open the desktop's own shortcut settings, where the player gives the
    /// meter's shortcuts their keys: KDE keeps the key it stored the first
    /// time and takes no new suggestion after, so a key changed in the meter's
    /// Settings changes nothing there (and a dismissed first dialog leaves
    /// them all without a key, as a CachyOS player's were, 2026-10-19).
    /// KDE's and GNOME's settings by name; elsewhere the portal's own
    /// ConfigureShortcuts, through the helper.
    pub fn open_desktop_settings(&self) -> Result<(), String> {
        use std::io::Write;
        use std::process::Command;
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_uppercase();
        let tries: &[&[&str]] = if desktop.contains("KDE") {
            &[&["systemsettings", "kcm_keys"], &["kcmshell6", "kcm_keys"], &["kcmshell5", "kcm_keys"]]
        } else if desktop.contains("GNOME") {
            &[&["gnome-control-center", "keyboard"]]
        } else {
            &[]
        };
        for args in tries {
            if Command::new(args[0]).args(&args[1..]).spawn().is_ok() {
                return Ok(());
            }
        }
        let mut slot = self.child.lock().unwrap_or_else(|p| p.into_inner());
        let stdin = slot.as_mut().and_then(|c| c.stdin.as_mut()).ok_or("keybinds are off")?;
        writeln!(stdin, "configure").map_err(|e| e.to_string())
    }

    /// Stop the helper. Its portal session ends with it, so the desktop
    /// drops the shortcuts.
    pub fn stop(&self) {
        let child = self.child.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The helper's whole life: register the shortcuts given as
/// `--shortcut "<id>\t<description>\t<trigger>"`, print what happens, and
/// exit when the meter closes our stdin (or dies). Returns the exit code.
pub fn run_helper() -> i32 {
    use std::io::Write;

    let mut wanted: Vec<(&'static str, &'static str, Option<String>)> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg != "--shortcut" {
            continue;
        }
        let Some(spec) = args.next() else { break };
        let mut fields = spec.splitn(3, '\t');
        let (Some(id), Some(description)) = (fields.next(), fields.next()) else { continue };
        let trigger = fields.next().filter(|t| !t.is_empty()).map(str::to_string);
        // Known ids only: they are what the portal and Hyprland are told.
        let Some(id) = [RELOAD, TOGGLE, LOCK, PLUGINS].into_iter().find(|known| *known == id) else { continue };
        let description: &'static str = Box::leak(description.to_string().into_boxed_str());
        wanted.push((id, description, trigger));
    }
    if wanted.is_empty() {
        println!("error no shortcuts given");
        return 2;
    }

    // The meter closing our stdin, or exiting, ends us too. A "configure"
    // line asks the portal to show its shortcut settings for our session.
    std::thread::spawn(|| {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim() == "configure" {
                if let Err(e) = configure() {
                    let mut out = std::io::stdout().lock();
                    let _ = writeln!(out, "error ConfigureShortcuts: {e}");
                    let _ = out.flush();
                }
            }
        }
        std::process::exit(0);
    });

    let say = |line: String| {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    };
    let slot = Mutex::new(None);
    let status = |id: &str, state: &str, assigned: Option<&str>| {
        say(match assigned {
            Some(a) => format!("status {id} {state} {a}"),
            None => format!("status {id} {state}"),
        })
    };
    match listen(&wanted, &slot, &status, |id| say(format!("fire {id}"))) {
        Ok(()) => 0,
        Err(e) => {
            say(format!("error {e} (no GlobalShortcuts portal?)"));
            1
        }
    }
}

/// The portal's trigger for a Win32 modifier set and key (`CTRL+ALT+r`), in
/// the shortcuts spec's form: modifiers by name, the key by its XKB name.
/// `None` for a key it has no name for; the shortcut is still bound, without
/// a suggestion.
fn trigger(mods: u32, vk: u32) -> Option<String> {
    let key = match vk {
        0x30..=0x39 => ((vk as u8) as char).to_string(),
        0x41..=0x5A => ((vk as u8).to_ascii_lowercase() as char).to_string(),
        0x70..=0x7B => format!("F{}", vk - 0x6F),
        0x08 => "BackSpace".into(),
        0x09 => "Tab".into(),
        0x0D => "Return".into(),
        0x1B => "Escape".into(),
        0x20 => "space".into(),
        0x21 => "Page_Up".into(),
        0x22 => "Page_Down".into(),
        0x23 => "End".into(),
        0x24 => "Home".into(),
        0x25 => "Left".into(),
        0x26 => "Up".into(),
        0x27 => "Right".into(),
        0x28 => "Down".into(),
        0x2D => "Insert".into(),
        0x2E => "Delete".into(),
        _ => return None,
    };
    let mut parts: Vec<&str> = Vec::new();
    if mods & 0x0002 != 0 {
        parts.push("CTRL");
    }
    if mods & 0x0001 != 0 {
        parts.push("ALT");
    }
    if mods & 0x0004 != 0 {
        parts.push("SHIFT");
    }
    if mods & 0x0008 != 0 {
        parts.push("LOGO");
    }
    let mut out = parts.join("+");
    if !out.is_empty() {
        out.push('+');
    }
    out.push_str(&key);
    Some(out)
}

/// The settings' name for a shortcut id (`reset` is the reload key).
fn status_key(id: &str) -> Option<&'static str> {
    match id {
        RELOAD => Some("reload"),
        TOGGLE => Some("toggle"),
        LOCK => Some("lock"),
        PLUGINS => Some("plugins"),
        _ => None,
    }
}

/// Which shortcuts the desktop gave a key, from BindShortcuts' `shortcuts`
/// result (`a(sa{sv})`, each with its `trigger_description`). A shortcut the
/// desktop bound to nothing, because its suggested keys were taken, comes
/// back with an empty description. `None` when the desktop says nothing.
fn assigned(results: &HashMap<String, OwnedValue>) -> Option<Vec<(String, Option<String>)>> {
    let list = results.get("shortcuts")?;
    let array = list.downcast_ref::<zbus::zvariant::Array>().ok()?;
    let mut out = Vec::new();
    for item in array.iter() {
        let Value::Structure(entry) = item else { continue };
        let fields = entry.fields();
        let Some(Value::Str(id)) = fields.first() else { continue };
        let Some(Value::Dict(props)) = fields.get(1) else { continue };
        let described = props
            .get::<&str, Value>(&"trigger_description")
            .ok()
            .flatten()
            .and_then(|v| text_of(&v));
        out.push((id.as_str().to_string(), described));
    }
    Some(out)
}

/// The text in it, through any `v` wrapping, if any.
fn text_of(value: &Value) -> Option<String> {
    match value {
        Value::Str(s) => Some(s.as_str().trim().to_string()).filter(|t| !t.is_empty()),
        Value::Value(inner) => text_of(inner),
        _ => None,
    }
}

/// Register, open a session, bind the shortcuts and call `fire` with each
/// activated shortcut's id until the connection closes. `status(id, state,
/// key)` reports, per shortcut, the key the desktop gave it, or `unset`.
fn listen(
    wanted: &[(&'static str, &'static str, Option<String>)],
    slot: &Mutex<Option<Connection>>,
    status: &dyn Fn(&str, &str, Option<&str>),
    fire: impl Fn(&str),
) -> zbus::Result<()> {
    let connection = Connection::session()?;

    // A host (unsandboxed) app says who it is before any other portal call,
    // so the desktop files its shortcuts under the meter rather than an
    // unnamed app. Portals before 1.19 have no registry: carry on without.
    let registry = proxy(&connection, PORTAL_PATH, "org.freedesktop.host.portal.Registry")?;
    let no_options: HashMap<&str, Value> = HashMap::new();
    if let Err(e) = registry.call::<_, _, ()>("Register", &(APP_ID, no_options)) {
        info!("Portal registry unavailable ({e}); binding shortcuts unregistered");
    }

    let portal = proxy(&connection, PORTAL_PATH, SHORTCUTS)?;
    // Listen before binding, so no activation is missed.
    let activations = portal.receive_signal("Activated")?;

    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("handle_token", Value::from("a2t_session_req"));
    options.insert("session_handle_token", Value::from("a2t_session"));
    let (code, results) =
        request(&connection, &portal, "a2t_session_req", "CreateSession", &(options,), || {})?;
    if code != 0 {
        return Err(zbus::Error::Failure(format!("CreateSession refused ({code})")));
    }
    let session: String = results
        .get("session_handle")
        .and_then(|v| v.downcast_ref::<&str>().ok().map(str::to_string))
        .ok_or_else(|| zbus::Error::Failure("CreateSession gave no session".into()))?;
    let session_path = ObjectPath::try_from(session.as_str())?;

    let shortcuts: Vec<(&str, HashMap<&str, Value>)> = wanted
        .iter()
        .map(|(id, description, trigger)| {
            let mut props: HashMap<&str, Value> = HashMap::new();
            props.insert("description", Value::from(*description));
            if let Some(t) = trigger {
                props.insert("preferred_trigger", Value::from(t.as_str()));
            }
            (*id, props)
        })
        .collect();
    let mut bind_options: HashMap<&str, Value> = HashMap::new();
    bind_options.insert("handle_token", Value::from("a2t_bind_req"));
    let (code, bound) = request(
        &connection,
        &portal,
        "a2t_bind_req",
        "BindShortcuts",
        &(&session_path, shortcuts, "", bind_options),
        || {},
    )?;
    if code != 0 {
        // 1: the user dismissed the desktop's dialog. Nothing is bound.
        return Err(zbus::Error::Failure(format!("BindShortcuts refused ({code})")));
    }
    info!(
        "Global hotkeys bound through the portal: {}",
        wanted
            .iter()
            .map(|(id, _, t)| format!("{APP_ID}:{id}={}", t.as_deref().unwrap_or("-")))
            .collect::<Vec<_>>()
            .join(", ")
    );

    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        // Hyprland's portal assigns no keys; the meter binds them itself and
        // knows which it could.
        hyprland_bind(wanted, status);
    } else if let Some(assigned) = assigned(&bound) {
        // The key the desktop gave each, or none: on KDE the first one stays,
        // and a dismissed first dialog left them all without one. Not
        // "taken": the desktop says no more than that it has no key.
        for (id, key) in assigned {
            match key {
                Some(key) => status(&id, "ok", Some(&key)),
                None => status(&id, "unset", None),
            }
        }
    }

    *slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(connection.clone());
    *session_slot().lock().unwrap_or_else(|p| p.into_inner()) = Some((connection.clone(), session.clone()));
    for message in activations {
        let body = message.body();
        let Ok((from, id, _time, _options)) =
            body.deserialize::<(ObjectPath<'_>, &str, u64, HashMap<&str, OwnedValue>)>()
        else {
            continue;
        };
        if from.as_str() == session_path.as_str() {
            fire(id);
        }
    }
    Ok(())
}

/// The portal session, for `configure`.
fn session_slot() -> &'static Mutex<Option<(Connection, String)>> {
    static SLOT: std::sync::OnceLock<Mutex<Option<(Connection, String)>>> = std::sync::OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Ask the portal to show its settings for our shortcuts (GlobalShortcuts
/// version 2, ConfigureShortcuts).
fn configure() -> zbus::Result<()> {
    let Some((connection, session)) = session_slot().lock().unwrap_or_else(|p| p.into_inner()).clone() else {
        return Err(zbus::Error::Failure("no portal session".into()));
    };
    let portal = proxy(&connection, PORTAL_PATH, SHORTCUTS)?;
    let session_path = ObjectPath::try_from(session.as_str())?;
    let options: HashMap<&str, Value> = HashMap::new();
    portal.call::<_, _, ()>("ConfigureShortcuts", &(&session_path, "", options))
}

/// Hyprland binds nothing on its own: its portal takes no suggested keys, and
/// the user would have to add a `global` bind for each shortcut. So on
/// Hyprland the meter adds them itself, at run time through `hyprctl`, for
/// this session only (a config reload drops them; the next start adds them
/// again). A key combination that is already bound, by the user or by an
/// earlier start, is left alone, so nothing of theirs is shadowed and nothing
/// fires twice.
fn hyprland_bind(wanted: &[(&'static str, &'static str, Option<String>)], status: &dyn Fn(&str, &str, Option<&str>)) {
    use std::process::Command;
    let run = |args: &[&str]| -> Option<String> {
        let out = Command::new("hyprctl").args(args).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    // The ids as Hyprland filed them (`appid:shortcut`): the app id part is
    // the portal's to choose, so read it rather than assume it.
    let Some(listed) = run(&["globalshortcuts", "-j"]) else {
        warn!("Hyprland: hyprctl globalshortcuts failed; bind the meter's shortcuts by hand");
        return;
    };
    let listed: Vec<serde_json::Value> = serde_json::from_str(&listed).unwrap_or_default();
    // Each bind's keys, and what it runs (`global` and its shortcut's name).
    let existing: Vec<(u64, String, String)> = run(&["binds", "-j"])
        .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(&s).ok())
        .unwrap_or_default()
        .iter()
        .map(|b| {
            let runs = format!(
                "{} {}",
                b.get("dispatcher").and_then(|d| d.as_str()).unwrap_or(""),
                b.get("arg").and_then(|a| a.as_str()).unwrap_or("")
            );
            (
                b.get("modmask").and_then(|m| m.as_u64()).unwrap_or(0),
                b.get("key").and_then(|k| k.as_str()).unwrap_or("").to_lowercase(),
                runs,
            )
        })
        .collect();

    for (id, description, trigger) in wanted {
        let Some(trigger) = trigger else { continue };
        let Some(name) = listed.iter().find_map(|s| {
            let name = s.get("name").and_then(|n| n.as_str())?;
            let ours = name.ends_with(&format!(":{id}"))
                && s.get("description").and_then(|d| d.as_str()) == Some(*description);
            ours.then(|| name.to_string())
        }) else {
            continue;
        };
        let Some((mods, key)) = hyprland_keys(trigger) else { continue };
        if let Some((_, _, runs)) = existing.iter().find(|(m, k, _)| *m == mods.mask && *k == key.to_lowercase()) {
            // Ours, from an earlier start this session: it works.
            if runs.trim() == format!("global {name}") {
                status(id, "ok", Some(trigger.as_str()));
            } else {
                info!("Hyprland: {} is already bound; left for {name}", trigger);
                status(id, "taken", None);
            }
            continue;
        }
        // Lua config (current Hyprland) first, then the hyprlang `keyword`.
        let lua = format!(
            "hl.bind(\"{}\", hl.dsp.global(\"{}\"))",
            mods.plus_joined(&key),
            name
        );
        let ok = run(&["eval", &lua]).is_some_and(|s| s.trim() == "ok")
            || run(&["keyword", "bind", &format!("{}, {}, global, {}", mods.space_joined(), key, name)])
                .is_some_and(|s| s.trim() == "ok");
        status(id, if ok { "ok" } else { "taken" }, ok.then_some(trigger.as_str()));
        if ok {
            info!("Hyprland: bound {trigger} to {name}");
        } else {
            warn!("Hyprland: could not bind {trigger} to {name}; bind it by hand");
        }
    }
}

/// A trigger's modifiers, as Hyprland names and masks them.
struct HyprMods {
    names: Vec<&'static str>,
    mask: u64,
}

impl HyprMods {
    fn plus_joined(&self, key: &str) -> String {
        let mut parts: Vec<&str> = self.names.clone();
        parts.push(key);
        parts.join(" + ")
    }
    fn space_joined(&self) -> String {
        self.names.join(" ")
    }
}

/// `CTRL+ALT+r` as Hyprland binds it: its modifier names and mask (SHIFT 1,
/// CTRL 4, ALT 8, SUPER 64) and the key.
fn hyprland_keys(trigger: &str) -> Option<(HyprMods, String)> {
    let mut mods = HyprMods { names: Vec::new(), mask: 0 };
    let mut key = None;
    for part in trigger.split('+') {
        match part {
            "CTRL" => {
                mods.names.push("CTRL");
                mods.mask |= 4;
            }
            "ALT" => {
                mods.names.push("ALT");
                mods.mask |= 8;
            }
            "SHIFT" => {
                mods.names.push("SHIFT");
                mods.mask |= 1;
            }
            "LOGO" => {
                mods.names.push("SUPER");
                mods.mask |= 64;
            }
            k => key = Some(k.to_string()),
        }
    }
    Some((mods, key?))
}

/// A proxy on the portal that never reads properties: none are used, and a
/// Request object does not exist until the call that makes it.
fn proxy<'a>(connection: &Connection, path: &'a str, interface: &'a str) -> zbus::Result<Proxy<'a>> {
    zbus::blocking::proxy::Builder::new(connection)
        .destination(PORTAL)?
        .path(path)?
        .interface(interface)?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
}

/// Call a portal method that answers through a Request object, and wait for
/// its Response. The Request's path is known before the call (the sender's
/// unique name and the token), so its signal is subscribed first and cannot
/// be missed.
fn request<B>(
    connection: &Connection,
    portal: &Proxy<'_>,
    token: &str,
    method: &str,
    body: &B,
    sent: impl FnOnce(),
) -> zbus::Result<(u32, HashMap<String, OwnedValue>)>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    let sender = connection
        .unique_name()
        .map(|n| n.as_str().trim_start_matches(':').replace('.', "_"))
        .ok_or_else(|| zbus::Error::Failure("no unique bus name".into()))?;
    let path = format!("{PORTAL_PATH}/request/{sender}/{token}");
    let request = proxy(connection, &path, "org.freedesktop.portal.Request")?;
    let mut responses = request.receive_signal("Response")?;
    let _handle: zbus::zvariant::OwnedObjectPath = portal.call(method, body)?;
    // The portal has checked the caller by now; only the answer is awaited.
    sent();
    let message = responses
        .next()
        .ok_or_else(|| zbus::Error::Failure(format!("{method}: no response")))?;
    message.body().deserialize::<(u32, HashMap<String, OwnedValue>)>()
}

#[cfg(test)]
mod tests {
    use super::{hyprland_keys, trigger};

    #[test]
    fn hyprland_reads_a_trigger_as_its_modifiers_and_key() {
        let (mods, key) = hyprland_keys("CTRL+ALT+r").unwrap();
        assert_eq!((mods.mask, key.as_str()), (12, "r"));
        assert_eq!(mods.plus_joined(&key), "CTRL + ALT + r");
        assert_eq!(mods.space_joined(), "CTRL ALT");
        let (mods, key) = hyprland_keys("LOGO+SHIFT+F5").unwrap();
        assert_eq!((mods.mask, key.as_str()), (65, "F5"));
        assert!(hyprland_keys("CTRL+ALT").is_none());
    }

    #[test]
    fn triggers_read_as_the_shortcuts_spec_writes_them() {
        assert_eq!(trigger(0x0002 | 0x0001, 0x52).as_deref(), Some("CTRL+ALT+r"));
        assert_eq!(trigger(0x0002 | 0x0001, 0x26).as_deref(), Some("CTRL+ALT+Up"));
        assert_eq!(trigger(0x0002 | 0x0004, 0x74).as_deref(), Some("CTRL+SHIFT+F5"));
        assert_eq!(trigger(0x0008, 0x31).as_deref(), Some("LOGO+1"));
        assert_eq!(trigger(0x0002, 0xBA), None);
    }
}

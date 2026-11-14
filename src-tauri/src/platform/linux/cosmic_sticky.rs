//! COSMIC (Pop!_OS 24.04): every one of the meter's windows made sticky as it
//! appears, through COSMIC's own toplevel management protocol.
//!
//! COSMIC keeps no window above the others on request, natively or through
//! XWayland: a windowed game took the meter under it at each click (a Pop!_OS
//! player, 2026-11-14). Its sticky windows stay on top, a fullscreen game's
//! too, but had to be made so by hand, one window at a time, with a shortcut
//! COSMIC leaves unbound. GTK's own request (`_NET_WM_STATE_STICKY` through
//! XWayland) went unheard. `zcosmic_toplevel_manager_v1.set_sticky` (version
//! 3 on) is offered to every client, and works on a window shown (tried on
//! Pop!_OS 24.04, cosmic-comp of 2026-11).
//!
//! A thread of its own holds a second Wayland connection, watches the
//! toplevels (`ext_foreign_toplevel_list_v1`) and sticks each of the meter's,
//! known by its app id, once: one the player unsticks stays so. A window the
//! meter unmaps comes back as a new toplevel and is stuck again.

#![allow(non_upper_case_globals, non_camel_case_types, unused_imports, dead_code, clippy::all)]

use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};

mod workspace {
    use wayland_client;
    use wayland_client::protocol::*;
    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/cosmic/cosmic-workspace-unstable-v1.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_client_code!("protocols/cosmic/cosmic-workspace-unstable-v1.xml");
}

mod info {
    use super::workspace::*;
    use wayland_client;
    use wayland_client::protocol::*;
    use wayland_protocols::ext::foreign_toplevel_list::v1::client::*;
    use wayland_protocols::ext::workspace::v1::client::*;
    pub mod __interfaces {
        use super::super::workspace::__interfaces::*;
        use wayland_client::protocol::__interfaces::*;
        use wayland_protocols::ext::foreign_toplevel_list::v1::client::__interfaces::*;
        use wayland_protocols::ext::workspace::v1::client::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/cosmic/cosmic-toplevel-info-unstable-v1.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_client_code!("protocols/cosmic/cosmic-toplevel-info-unstable-v1.xml");
}

mod manage {
    use super::info::*;
    use super::workspace::*;
    use wayland_client;
    use wayland_client::protocol::*;
    use wayland_protocols::ext::workspace::v1::client::*;
    pub mod __interfaces {
        use super::super::info::__interfaces::*;
        use super::super::workspace::__interfaces::*;
        use wayland_client::protocol::__interfaces::*;
        use wayland_protocols::ext::workspace::v1::client::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/cosmic/cosmic-toplevel-management-unstable-v1.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_client_code!("protocols/cosmic/cosmic-toplevel-management-unstable-v1.xml");
}

use info::zcosmic_toplevel_handle_v1::{self, ZcosmicToplevelHandleV1};
use info::zcosmic_toplevel_info_v1::{self, ZcosmicToplevelInfoV1};
use manage::zcosmic_toplevel_manager_v1::{self, ZcosmicToplevelManagerV1};

/// The meter's app id: its X11 class through XWayland ("A2tools-dps-meter"),
/// its Wayland app id natively.
const APP_ID: &str = "a2tools-dps-meter";
/// `set_sticky` came with this version of the manager.
const STICKY_SINCE: u32 = 3;

fn ours(app_id: &str) -> bool {
    app_id.eq_ignore_ascii_case(APP_ID)
}

struct Toplevel {
    handle: ExtForeignToplevelHandleV1,
    app_id: String,
    cosmic: Option<ZcosmicToplevelHandleV1>,
}

#[derive(Default)]
struct Watch {
    info: Option<ZcosmicToplevelInfoV1>,
    manager: Option<ZcosmicToplevelManagerV1>,
    toplevels: Vec<Toplevel>,
}

impl Watch {
    /// A toplevel's description complete: one of the meter's gets its COSMIC
    /// handle and is stuck, once. COSMIC tells no state of a window opened
    /// without focus (Settings), so this waits for none. A window hidden and
    /// shown again (Settings reopened) comes back still marked sticky but
    /// under the game: unsticking it first puts it back on top.
    fn described(&mut self, handle: &ExtForeignToplevelHandleV1, qh: &QueueHandle<Self>) {
        let (Some(info), Some(manager)) = (self.info.clone(), self.manager.clone()) else { return };
        let Some(t) = self.toplevels.iter_mut().find(|t| &t.handle == handle) else { return };
        if t.cosmic.is_none() && ours(&t.app_id) {
            let cosmic = info.get_cosmic_toplevel(&t.handle, qh, ());
            manager.unset_sticky(&cosmic);
            manager.set_sticky(&cosmic);
            t.cosmic = Some(cosmic);
            tracing::info!("COSMIC: a meter window made sticky");
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Watch {
    fn event(w: &mut Self, reg: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "ext_foreign_toplevel_list_v1" => {
                    reg.bind::<ExtForeignToplevelListV1, _, _>(name, 1, qh, ());
                }
                "zcosmic_toplevel_info_v1" if version >= 2 => w.info = Some(reg.bind(name, version.min(3), qh, ())),
                "zcosmic_toplevel_manager_v1" if version >= STICKY_SINCE => w.manager = Some(reg.bind(name, version.min(4), qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for Watch {
    fn event(w: &mut Self, _: &ExtForeignToplevelListV1, event: ext_foreign_toplevel_list_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            w.toplevels.push(Toplevel { handle: toplevel, app_id: String::new(), cosmic: None });
        }
    }
    wayland_client::event_created_child!(Watch, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for Watch {
    fn event(w: &mut Self, h: &ExtForeignToplevelHandleV1, event: ext_foreign_toplevel_handle_v1::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        match event {
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                if let Some(t) = w.toplevels.iter_mut().find(|t| &t.handle == h) {
                    t.app_id = app_id;
                }
            }
            ext_foreign_toplevel_handle_v1::Event::Done => w.described(h, qh),
            ext_foreign_toplevel_handle_v1::Event::Closed => {
                if let Some(i) = w.toplevels.iter().position(|t| &t.handle == h) {
                    let t = w.toplevels.swap_remove(i);
                    if let Some(c) = t.cosmic {
                        c.destroy();
                    }
                    t.handle.destroy();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZcosmicToplevelInfoV1, ()> for Watch {
    fn event(_: &mut Self, _: &ZcosmicToplevelInfoV1, _: zcosmic_toplevel_info_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    wayland_client::event_created_child!(Watch, ZcosmicToplevelInfoV1, [
        zcosmic_toplevel_info_v1::EVT_TOPLEVEL_OPCODE => (ZcosmicToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZcosmicToplevelHandleV1, ()> for Watch {
    fn event(_: &mut Self, _: &ZcosmicToplevelHandleV1, _: zcosmic_toplevel_handle_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZcosmicToplevelManagerV1, ()> for Watch {
    fn event(_: &mut Self, _: &ZcosmicToplevelManagerV1, _: zcosmic_toplevel_manager_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

/// Start sticking the meter's windows, once, on COSMIC. Quietly nothing where
/// the compositor offers no such protocol.
pub fn start() {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !super::process::is_cosmic() || STARTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let spawned = std::thread::Builder::new().name("cosmic-sticky".into()).spawn(|| {
        if let Err(e) = run() {
            tracing::warn!("COSMIC: the meter's windows cannot be made sticky: {e}");
        }
    });
    if let Err(e) = spawned {
        tracing::warn!("COSMIC: no sticky watcher: {e}");
    }
}

fn run() -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| e.to_string())?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut watch = Watch::default();
    conn.display().get_registry(&qh, ());
    queue.roundtrip(&mut watch).map_err(|e| e.to_string())?;
    if watch.info.is_none() || watch.manager.is_none() {
        return Err("COSMIC offers no toplevel management with set_sticky".into());
    }
    tracing::info!("COSMIC: the meter's windows will be made sticky as they appear");
    loop {
        queue.blocking_dispatch(&mut watch).map_err(|e| e.to_string())?;
    }
}

#[cfg(test)]
mod tests {
    use super::ours;

    #[test]
    fn the_meters_windows_are_known_by_their_app_id() {
        assert!(ours("A2tools-dps-meter"));
        assert!(ours("a2tools-dps-meter"));
        assert!(!ours("clipgame.exe"));
        assert!(!ours("steam_app_1234"));
    }
}

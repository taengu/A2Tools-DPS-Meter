use std::ffi::{c_char, c_int, c_long, c_uint, CStr, CString};
use std::ptr;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use libloading::{Library, Symbol};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use super::captured_payload::CapturedPayload;
use super::combat_port_detector::CombatPortDetector;
use super::mode::CaptureMode;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

// ===== Raw pcap FFI types =====

type PcapT = *mut std::ffi::c_void;
type PcapIfT = *mut PcapIf;

#[repr(C)]
struct PcapIf {
    next: *mut PcapIf,
    name: *const c_char,
    description: *const c_char,
    addresses: *mut PcapAddr,
    flags: c_uint,
}

#[repr(C)]
struct PcapAddr {
    next: *mut PcapAddr,
    addr: *mut SockAddr,
    netmask: *mut SockAddr,
    broadaddr: *mut SockAddr,
    dstaddr: *mut SockAddr,
}

#[repr(C)]
struct SockAddr {
    sa_family: u16,
    sa_data: [u8; 14],
}

/// `struct pcap_pkthdr`. The timestamp is a C `struct timeval`, two `long`s:
/// 32-bit on Windows, 64-bit on 64-bit Linux. Declaring them `i32` (as this
/// once did) reads `caplen` from the middle of the timestamp on Linux.
#[repr(C)]
struct PcapPkthdr {
    ts_sec: c_long,
    ts_usec: c_long,
    caplen: c_uint,
    len: c_uint,
}

/// `struct bpf_program`: a compiled capture filter.
#[repr(C)]
struct BpfProgram {
    bf_len: c_uint,
    bf_insns: *mut std::ffi::c_void,
}

const PCAP_IF_LOOPBACK: c_uint = 0x00000001;
/// `PCAP_NETMASK_UNKNOWN`: only broadcast tests need the netmask, and the
/// filters here have none.
const PCAP_NETMASK_UNKNOWN: u32 = 0xffff_ffff;

// ===== Device info =====

#[derive(Clone)]
struct DeviceInfo {
    name: String,
    description: String,
    has_addresses: bool,
    is_loopback: bool,
}

impl DeviceInfo {
    fn capture_label(&self, mode: CaptureMode) -> String {
        if mode.is_mirrored() { format!("{} [{}]", self.label(), self.name) } else { self.label().to_string() }
    }

    fn label(&self) -> &str {
        if self.description.is_empty() {
            &self.name
        } else {
            &self.description
        }
    }

    fn is_virtual(&self) -> bool {
        let label = self.label().to_lowercase();
        self.is_loopback
            || self.name.to_lowercase().contains("loopback")
            || label.contains("loopback")
            || label.contains("tap-windows")
            || label.contains("tap")
            || label.contains("wintun")
            || label.contains("wireguard")
    }
}

// ===== Pcap library wrapper =====

struct PcapLib {
    _lib: Library,
    findalldevs: unsafe extern "C" fn(*mut PcapIfT, *mut c_char) -> c_int,
    freealldevs: unsafe extern "C" fn(PcapIfT),
    open_live: unsafe extern "C" fn(*const c_char, c_int, c_int, c_int, *mut c_char) -> PcapT,
    close: unsafe extern "C" fn(PcapT),
    next_ex: unsafe extern "C" fn(PcapT, *mut *mut PcapPkthdr, *mut *const u8) -> c_int,
    datalink: unsafe extern "C" fn(PcapT) -> c_int,
    filter: Option<FilterFns>,
}

/// libpcap's filter calls. Every libpcap and Npcap has them; without them
/// the capture runs unfiltered, as it did before filters.
struct FilterFns {
    compile: unsafe extern "C" fn(PcapT, *mut BpfProgram, *const c_char, c_int, u32) -> c_int,
    setfilter: unsafe extern "C" fn(PcapT, *mut BpfProgram) -> c_int,
    freecode: unsafe extern "C" fn(*mut BpfProgram),
    geterr: unsafe extern "C" fn(PcapT) -> *const c_char,
    setnonblock: unsafe extern "C" fn(PcapT, c_int, *mut c_char) -> c_int,
    /// libpcap's filter compiler was not thread-safe before 1.8, and every
    /// device has its own thread.
    compiling: Mutex<()>,
}

impl PcapLib {
    fn load() -> Result<Self, String> {
        let lib = crate::platform::pcap::load_library()
            .map_err(|e| format!("{}. {e}", crate::platform::pcap::MISSING_HELP))?;

        unsafe {
            let findalldevs: Symbol<unsafe extern "C" fn(*mut PcapIfT, *mut c_char) -> c_int> =
                lib.get(b"pcap_findalldevs").map_err(|e| format!("pcap_findalldevs: {}", e))?;
            let freealldevs: Symbol<unsafe extern "C" fn(PcapIfT)> =
                lib.get(b"pcap_freealldevs").map_err(|e| format!("pcap_freealldevs: {}", e))?;
            let open_live: Symbol<
                unsafe extern "C" fn(*const c_char, c_int, c_int, c_int, *mut c_char) -> PcapT,
            > = lib.get(b"pcap_open_live").map_err(|e| format!("pcap_open_live: {}", e))?;
            let close: Symbol<unsafe extern "C" fn(PcapT)> =
                lib.get(b"pcap_close").map_err(|e| format!("pcap_close: {}", e))?;
            let next_ex: Symbol<
                unsafe extern "C" fn(PcapT, *mut *mut PcapPkthdr, *mut *const u8) -> c_int,
            > = lib.get(b"pcap_next_ex").map_err(|e| format!("pcap_next_ex: {}", e))?;
            let datalink: Symbol<unsafe extern "C" fn(PcapT) -> c_int> =
                lib.get(b"pcap_datalink").map_err(|e| format!("pcap_datalink: {}", e))?;
            let filter = (|| {
                Some(FilterFns {
                    compile: *lib.get(b"pcap_compile").ok()?,
                    setfilter: *lib.get(b"pcap_setfilter").ok()?,
                    freecode: *lib.get(b"pcap_freecode").ok()?,
                    geterr: *lib.get(b"pcap_geterr").ok()?,
                    setnonblock: *lib.get(b"pcap_setnonblock").ok()?,
                    compiling: Mutex::new(()),
                })
            })();
            if filter.is_none() {
                warn!("libpcap has no pcap_setfilter: capturing without a filter");
            }

            Ok(Self {
                findalldevs: *findalldevs,
                freealldevs: *freealldevs,
                open_live: *open_live,
                close: *close,
                next_ex: *next_ex,
                datalink: *datalink,
                filter,
                _lib: lib,
            })
        }
    }

    fn find_all_devs(&self) -> Result<Vec<DeviceInfo>, String> {
        let mut alldevs: PcapIfT = ptr::null_mut();
        let mut errbuf = [0u8; 256];

        let ret =
            unsafe { (self.findalldevs)(&mut alldevs, errbuf.as_mut_ptr() as *mut c_char) };

        if ret != 0 || alldevs.is_null() {
            let err = unsafe { CStr::from_ptr(errbuf.as_ptr() as *const c_char) }
                .to_string_lossy()
                .to_string();
            return Err(format!("pcap_findalldevs failed: {}", err));
        }

        let mut devices = Vec::new();
        let mut dev = alldevs;
        while !dev.is_null() {
            let d = unsafe { &*dev };
            let name = if d.name.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(d.name) }
                    .to_string_lossy()
                    .to_string()
            };
            let description = if d.description.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(d.description) }
                    .to_string_lossy()
                    .to_string()
            };
            let has_addresses = !d.addresses.is_null();
            let is_loopback = (d.flags & PCAP_IF_LOOPBACK) != 0;

            devices.push(DeviceInfo {
                name,
                description,
                has_addresses,
                is_loopback,
            });
            dev = d.next;
        }

        unsafe { (self.freealldevs)(alldevs) };
        Ok(devices)
    }

    fn open_live_handle(&self, name: &str, mode: CaptureMode) -> Result<PcapT, String> {
        let c_name = CString::new(name).map_err(|e| format!("Invalid device name: {}", e))?;
        let mut errbuf = [0u8; 256];

        let handle = unsafe {
            (self.open_live)(
                c_name.as_ptr(),
                65535,  // snaplen
                mode.promiscuous(), // Only the explicitly selected mirror adapter may be promiscuous.
                100,    // timeout ms
                errbuf.as_mut_ptr() as *mut c_char,
            )
        };

        if handle.is_null() {
            let err = unsafe { CStr::from_ptr(errbuf.as_ptr() as *const c_char) }
                .to_string_lossy()
                .to_string();
            return Err(format!("pcap_open_live failed: {}", err));
        }

        Ok(handle)
    }

    /// Has the kernel pass this handle only what `filter` matches.
    fn set_filter(&self, handle: PcapT, filter: &str) -> Result<(), String> {
        let f = self.filter.as_ref().ok_or("no pcap_setfilter")?;
        let text = CString::new(filter).map_err(|e| e.to_string())?;
        let mut program = BpfProgram { bf_len: 0, bf_insns: ptr::null_mut() };
        let error = || unsafe { CStr::from_ptr((f.geterr)(handle)) }.to_string_lossy().into_owned();
        {
            let _one_at_a_time = f.compiling.lock().unwrap_or_else(|p| p.into_inner());
            // SAFETY: a live handle, and a program for libpcap to fill.
            if unsafe { (f.compile)(handle, &mut program, text.as_ptr(), 1, PCAP_NETMASK_UNKNOWN) } != 0 {
                return Err(error());
            }
        }
        // SAFETY: the program compiled above; libpcap copies it, so it is
        // freed whatever setfilter says.
        let set = unsafe { (f.setfilter)(handle, &mut program) };
        let result = if set == 0 { Ok(()) } else { Err(error()) };
        unsafe { (f.freecode)(&mut program) };
        result
    }
}

/// With AION2 open this long and no game stream found, the filter may be what
/// hides it (a relay or accelerator tunnelling the game over port 80 or 443,
/// say): capture goes on unfiltered, as it did before filters.
const SAFETY_NET_MS: i64 = 60_000;
/// How often a capture thread looks for the game's window, for the net.
const GAME_CHECK_MS: i64 = 5_000;

/// The safety net's clock: when the game was first seen open with nothing
/// locked. True once that has lasted SAFETY_NET_MS.
#[derive(Default)]
struct SafetyNet {
    unlocked_since: Option<i64>,
}

impl SafetyNet {
    fn note(&mut self, game_open: bool, locked: bool, now: i64) -> bool {
        if !game_open || locked {
            self.unlocked_since = None;
            return false;
        }
        now - *self.unlocked_since.get_or_insert(now) >= SAFETY_NET_MS
    }
}

/// The port to filter for: the locked one, unless it has carried nothing the
/// dispatcher could parse for its stale time. Then the filter widens, so other
/// traffic reaches the dispatcher again and its stale check (which runs only
/// when a packet comes) can clear the lock: a relay that reconnected on a new
/// local port would otherwise leave every device filtered for the old one,
/// with nothing ever arriving to notice.
///
/// Where a filter change empties the kernel's queue (Npcap), the device the
/// game is locked on keeps the wide filter (its queue holds the game's
/// packets), and only the others narrow: none of the game's packets wait on
/// them, so emptying their queue loses nothing. They still see the game's
/// port, so a relay taking it over or the game moving to them is noticed. Left
/// wide, every device read all TCP but the web's for the whole session:
/// Npcap's loopback, every program on the PC talking to another, read at
/// 7.8 MB/s against 0.5 MB/s off the network (2026-10-29).
fn filter_port(detector: &CombatPortDetector, now: i64, device: &str) -> Option<u16> {
    let port = detector.locked_port_now()?;
    if !crate::platform::pcap::FILTER_CHANGE_KEEPS_QUEUE
        && detector.current_device().map_or(true, |locked| locked.trim() == device.trim())
    {
        return None;
    }
    let last = detector.last_parsed_at_ms();
    let stale = last > 0 && now - last > crate::combat::capture_dispatcher::STALE_CONNECTION_MS;
    // The locked port gone quiet: the game may be on a new connection (a
    // server switch), which the dispatcher can only follow if it sees it.
    let quiet = detector.port_quiet_ms(now).is_some_and(|q| q >= crate::combat::capture_dispatcher::PORT_MOVE_QUIET_MS);
    (!stale && !quiet).then_some(port)
}

/// The capture filter, as libpcap filter text: the game's port once it is
/// locked (both directions, as the dispatcher reads it), and before that all
/// IPv4 TCP but the web's ports (the game has never used them; what comes on
/// them is HTTPS and HTTP from browsers and launchers). The first form also
/// matches frames with one or two VLAN tags, which some network cards add
/// (see `link_header_len`); the second is for link types where libpcap has no
/// VLAN test.
fn capture_filters(port: Option<u16>) -> [String; 2] {
    let plain = match port {
        Some(port) => format!("ip and tcp port {port}"),
        None => "ip and tcp and not port 80 and not port 443".to_string(),
    };
    // Nested: each `vlan` moves the offsets for everything after it in the
    // expression, so side-by-side alternatives would add their tags up.
    [format!("({plain}) or (vlan and (({plain}) or (vlan and {plain})))"), plain]
}

// Safety: PcapLib function pointers are thread-safe (each thread gets its own pcap handle)
unsafe impl Send for PcapLib {}
unsafe impl Sync for PcapLib {}

/// Manages pcap device handles and captures TCP traffic from network interfaces.
/// Loads the OS's pcap library at runtime (`platform::pcap::LIBRARIES`) — no SDK
/// needed at compile time.
pub struct PcapCapturer {
    running: Arc<AtomicBool>,
    sender: mpsc::Sender<CapturedPayload>,
    shared: Arc<Shared>,
}

/// What the capture threads and the device watcher share.
struct Shared {
    mode: CaptureMode,
    /// Devices with a capture thread (or one about to start), by name. A
    /// thread takes its device out again when it ends, so a device that goes
    /// away and comes back (a VPN reconnecting) is captured again.
    active: Mutex<HashSet<String>>,
    /// Devices that could not be opened: not tried again, so the log does not
    /// repeat the failure every scan.
    failed: Mutex<HashSet<String>>,
    /// Packets lost because the parser had not taken the earlier ones yet.
    dropped: AtomicU64,
    /// Which port the game is on, once found: each capture thread filters
    /// for it.
    port_detector: Arc<CombatPortDetector>,
    /// Set by the safety net (`SafetyNet`): every device captures without a
    /// filter for the rest of the session.
    unfiltered: AtomicBool,
}

/// How often to look for network devices that appeared after the start: a
/// VPN or ping reducer (Mudfish's tap0, a WireGuard tunnel) connected while
/// the meter runs carries the game from then on.
const DEVICE_SCAN_MS: u64 = 15_000;

/// Without blocking reads: how long to wait after finding nothing. Packets
/// wait in the kernel meanwhile and keep their capture times (the pcap
/// header's, which ping uses); only an empty read waits, so a busy device is
/// read straight through.
const IDLE_WAIT_MS: u64 = 20;

impl PcapCapturer {
    pub fn new(sender: mpsc::Sender<CapturedPayload>, port_detector: Arc<CombatPortDetector>) -> Self {
        Self::with_mode(sender, port_detector, CaptureMode::Local)
    }

    pub fn with_mode(sender: mpsc::Sender<CapturedPayload>, port_detector: Arc<CombatPortDetector>, mode: CaptureMode) -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            sender,
            shared: Arc::new(Shared {
                mode,
                active: Mutex::default(),
                failed: Mutex::default(),
                dropped: AtomicU64::new(0),
                port_detector,
                unfiltered: AtomicBool::new(false),
            }),
        }
    }

    pub fn start(&self) {
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }

        let pcap = match PcapLib::load() {
            Ok(p) => Arc::new(p),
            Err(e) => {
                error!("{}", e);
                self.running.store(false, Ordering::SeqCst);
                return;
            }
        };

        let devices = match pcap.find_all_devs() {
            Ok(d) => d,
            Err(e) => {
                error!("Failed to list devices: {}", e);
                self.running.store(false, Ordering::SeqCst);
                return;
            }
        };

        // Mirrored capture can use an unnumbered interface, but only the one
        // selected explicitly. OS pseudo-devices still stay out.
        let selected = self.shared.port_detector.preferred_device();
        let devices: Vec<_> = devices
            .into_iter()
            .filter(|d| self.shared.mode.captures_device(d.has_addresses, &d.capture_label(self.shared.mode), selected.as_deref())
                && !crate::platform::pcap::skip_device(&d.name))
            .collect();

        if devices.is_empty() {
            if self.shared.mode.is_mirrored() {
                error!("No mirror adapter available: select the dedicated adapter in Settings, then restart the meter");
            } else {
                error!("No capture devices found with addresses");
            }
            self.running.store(false, Ordering::SeqCst);
            return;
        }

        info!("Found {} capture devices", devices.len());
        for (i, dev) in devices.iter().enumerate() {
            info!(
                "  [{}] {} (loopback={}, addresses={})",
                i,
                dev.label(),
                dev.is_loopback,
                dev.has_addresses
            );
        }

        let virtual_devices: Vec<_> = devices.iter().filter(|d| d.is_virtual()).cloned().collect();
        let physical_devices: Vec<_> = devices.iter().filter(|d| !d.is_virtual()).cloned().collect();

        // Every device is claimed now, so the watcher below does not start a
        // second thread on a physical device still waiting for its delay.
        {
            let mut active = self.shared.active.lock().unwrap_or_else(|p| p.into_inner());
            active.extend(devices.iter().map(|d| d.name.clone()));
        }

        // Start virtual/loopback devices first
        for device in &virtual_devices {
            start_capture_thread(
                device.clone(),
                pcap.clone(),
                self.sender.clone(),
                self.running.clone(),
                self.shared.clone(),
            );
        }

        if virtual_devices.is_empty() {
            // No virtual devices — start physical immediately
            for device in &physical_devices {
                start_capture_thread(
                    device.clone(),
                    pcap.clone(),
                    self.sender.clone(),
                    self.running.clone(),
                    self.shared.clone(),
                );
            }
        } else {
            // Start physical after delay as fallback
            let running = self.running.clone();
            let sender = self.sender.clone();
            let pcap2 = pcap.clone();
            let shared = self.shared.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(1500));
                if !running.load(Ordering::SeqCst) {
                    return;
                }
                for device in &physical_devices {
                    info!("Starting capture on physical device: {}", device.label());
                    start_capture_thread(
                        device.clone(),
                        pcap2.clone(),
                        sender.clone(),
                        running.clone(),
                        shared.clone(),
                    );
                }
            });
        }

        // Devices that appear later, and lost packets.
        let running = self.running.clone();
        let sender = self.sender.clone();
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            let mut reported_drops = 0;
            while running.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(DEVICE_SCAN_MS));
                let dropped = shared.dropped.load(Ordering::Relaxed);
                if dropped > reported_drops {
                    warn!(
                        "{} packets lost in the last {} s: the parser fell behind the capture",
                        dropped - reported_drops,
                        DEVICE_SCAN_MS / 1000
                    );
                    reported_drops = dropped;
                }
                let Ok(devices) = pcap.find_all_devs() else { continue };
                for device in devices {
                    let selected = shared.port_detector.preferred_device();
                    if !shared.mode.captures_device(device.has_addresses, &device.capture_label(shared.mode), selected.as_deref())
                        || crate::platform::pcap::skip_device(&device.name) {
                        continue;
                    }
                    if shared.failed.lock().unwrap_or_else(|p| p.into_inner()).contains(&device.name) {
                        continue;
                    }
                    if !shared.active.lock().unwrap_or_else(|p| p.into_inner()).insert(device.name.clone()) {
                        continue;
                    }
                    info!("New capture device: {} (loopback={})", device.label(), device.is_loopback);
                    start_capture_thread(device, pcap.clone(), sender.clone(), running.clone(), shared.clone());
                }
            }
        });
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

/// List available capture device labels (for the settings UI dropdown).
pub fn list_device_labels() -> Result<Vec<String>, String> {
    list_device_labels_for_mode(CaptureMode::Local)
}

pub fn list_device_labels_for_mode(mode: CaptureMode) -> Result<Vec<String>, String> {
    let pcap = PcapLib::load().map_err(|e| e.to_string())?;
    let devices = pcap.find_all_devs().map_err(|e| e.to_string())?;
    Ok(devices
        .into_iter()
        .filter(|d| mode.lists_device(d.has_addresses) && !crate::platform::pcap::skip_device(&d.name))
        .map(|d| d.capture_label(mode))
        .collect())
}

fn start_capture_thread(
    device: DeviceInfo,
    pcap: Arc<PcapLib>,
    sender: mpsc::Sender<CapturedPayload>,
    running: Arc<AtomicBool>,
    shared: Arc<Shared>,
) {
    let label = device.capture_label(shared.mode);
    let dev_name = device.name.clone();

    std::thread::spawn(move || {
        let release = |failed: bool| {
            shared.active.lock().unwrap_or_else(|p| p.into_inner()).remove(&dev_name);
            if failed {
                shared.failed.lock().unwrap_or_else(|p| p.into_inner()).insert(dev_name.clone());
            }
        };
        let handle = match pcap.open_live_handle(&dev_name, shared.mode) {
            Ok(h) => h,
            Err(e) => {
                warn!("Failed to open capture on {}: {}", label, e);
                release(true);
                return;
            }
        };

        let link_type = unsafe { (pcap.datalink)(handle) };
        info!("Capture active on {} (link type {})", label, link_type);

        // The filter in place: the port it is for (None: not locked yet), or
        // nothing tried yet. Checked before every read, so a lock narrows it
        // and a reset widens it at once. Where a read on a quiet device would
        // block until a packet comes, reads don't block: a filter for the old
        // port would otherwise wait forever once the game moved to another.
        let mut filtered_for: Option<Option<u16>> = None;
        let mut filter_failed = false;
        let mut cleared = false;
        let mut net = SafetyNet::default();
        let mut game_open = false;
        let mut last_game_check: i64 = 0;
        let mut nonblocking = false;
        if let (false, Some(f)) = (crate::platform::pcap::READ_TIMES_OUT_WHEN_IDLE, &pcap.filter) {
            let mut errbuf = [0u8; 256];
            // SAFETY: a live handle and an error buffer of libpcap's size.
            nonblocking = unsafe { (f.setnonblock)(handle, 1, errbuf.as_mut_ptr() as *mut c_char) } == 0;
        }
        while running.load(Ordering::SeqCst) {
            if shared.mode.is_mirrored() && !shared.mode.captures_device(
                device.has_addresses, &label, shared.port_detector.preferred_device().as_deref(),
            ) {
                break;
            }
            let now = now_ms();
            if pcap.filter.is_some() && !filter_failed && !shared.unfiltered.load(Ordering::Relaxed) {
                if now - last_game_check >= GAME_CHECK_MS {
                    last_game_check = now;
                    game_open = shared.mode.game_available(crate::platform::window_detector::find_aion2_window_title().is_some());
                }
                let locked = shared.port_detector.locked_port_now().is_some();
                if net.note(game_open, locked, now) && !shared.unfiltered.swap(true, Ordering::Relaxed) {
                    warn!(
                        "AION2 has been open {} s and no game stream was found: capturing without a filter for this session",
                        SAFETY_NET_MS / 1000
                    );
                }
            }
            if shared.unfiltered.load(Ordering::Relaxed) {
                if !cleared && filtered_for.is_some() && !filter_failed {
                    cleared = true;
                    match pcap.set_filter(handle, "") {
                        Ok(()) => info!("Capture filter on {}: none", label),
                        Err(e) => warn!("Could not clear the capture filter on {}: {}", label, e),
                    }
                }
            }
            let port = filter_port(&shared.port_detector, now, &label);
            if !shared.unfiltered.load(Ordering::Relaxed) && filtered_for != Some(port) && !filter_failed {
                filtered_for = Some(port);
                let [full, plain] = capture_filters(port);
                let result = pcap.set_filter(handle, &full).or_else(|_| pcap.set_filter(handle, &plain));
                match result {
                    Ok(()) => info!("Capture filter on {}: {}", label, match port {
                        Some(p) => format!("game port {p} only"),
                        None => "IPv4 TCP, not ports 80/443".to_string(),
                    }),
                    // Unfiltered, as before filters: the dispatcher still
                    // keeps only the game's packets. An empty filter clears a
                    // narrower one still in place, so a failed widening never
                    // leaves the capture on the old port.
                    Err(e) => {
                        warn!("No capture filter on {}: {}", label, e);
                        if let Err(e) = pcap.set_filter(handle, "") {
                            warn!("Could not clear the capture filter on {}: {}", label, e);
                        }
                        filter_failed = true;
                    }
                }
            }

            let mut header: *mut PcapPkthdr = ptr::null_mut();
            let mut data: *const u8 = ptr::null();

            let ret = unsafe { (pcap.next_ex)(handle, &mut header, &mut data) };

            match ret {
                1 => {
                    let hdr = unsafe { &*header };
                    let len = hdr.caplen as usize;
                    // Use pcap hardware timestamp (seconds + microseconds since epoch)
                    let pcap_ts_ms = (hdr.ts_sec as i64) * 1000 + (hdr.ts_usec as i64) / 1000;
                    // Sanity check: if pcap timestamp looks bogus, fall back to wall clock
                    let ts = if pcap_ts_ms > 1_000_000_000_000 && pcap_ts_ms < 2_000_000_000_000 {
                        pcap_ts_ms
                    } else {
                        now_ms()
                    };
                    let frame = unsafe { std::slice::from_raw_parts(data, len) };
                    if let Some(mut payload) = parse_tcp_payload(frame, link_type, &label) {
                        payload.captured_at_ms = ts;
                        if sender.try_send(payload).is_err() {
                            shared.dropped.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                // Nothing yet (a timeout, or nothing waiting).
                0 => {
                    if nonblocking {
                        std::thread::sleep(std::time::Duration::from_millis(IDLE_WAIT_MS));
                    }
                    continue;
                }
                -2 => break,   // EOF (savefile)
                _ => {
                    warn!("Capture error on {} (ret={})", label, ret);
                    break;
                }
            }
        }

        unsafe { (pcap.close)(handle) };
        info!("Capture stopped on {}", label);
        release(false);
    });
}

/// Where IPv4 starts in a frame of pcap link type `link_type`, or `None` if this
/// frame is not IPv4 under that type. Link types are libpcap's (DLT_*/LINKTYPE_*),
/// the same on every OS.
fn link_header_len(link_type: c_int, frame: &[u8]) -> Option<usize> {
    let be16 = |at: usize| frame.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    match link_type {
        // Ethernet, with up to two 802.1Q / 802.1ad VLAN tags (some NICs, e.g.
        // Realtek with "Priority & VLAN" on, tag every frame).
        1 => {
            let mut at = 12;
            for _ in 0..2 {
                match be16(at)? {
                    0x8100 | 0x88A8 => at += 4,
                    _ => break,
                }
            }
            (be16(at)? == 0x0800).then_some(at + 2)
        }
        // BSD loopback / Npcap loopback: 4-byte address family, AF_INET = 2.
        0 => (frame.get(..4)? == [2, 0, 0, 0]).then_some(4),
        // Raw IP (VPN and tunnel adapters, WireGuard).
        12 | 14 | 101 | 228 => Some(0),
        // Linux "cooked" capture v1: protocol at bytes 14-15, 16-byte header.
        113 => (be16(14)? == 0x0800).then_some(16),
        // Linux "cooked" capture v2: protocol at bytes 0-1, 20-byte header.
        276 => (be16(0)? == 0x0800).then_some(20),
        _ => None,
    }
}

/// The original guess at the link layer, from the frame alone. Kept as the
/// fallback so nothing that parsed before the link type was consulted can stop
/// parsing now.
fn guess_link_header_len(frame: &[u8]) -> Option<usize> {
    if frame.len() >= 14 {
        let ether_type = u16::from_be_bytes([frame[12], frame[13]]);
        if ether_type == 0x0800 {
            Some(14) // Standard Ethernet
        } else if frame[0] == 2 && frame[1] == 0 && frame[2] == 0 && frame[3] == 0 {
            Some(4) // NULL/Loopback: AF_INET (little-endian 2) = IPv4
        } else if (frame[0] >> 4) == 4 {
            Some(0) // Raw IPv4
        } else {
            None
        }
    } else if (frame[0] >> 4) == 4 {
        Some(0) // Raw IPv4 (short frame)
    } else {
        None
    }
}

/// Parse raw captured frame to extract TCP payload. The link type says where
/// IPv4 starts; when it does not fit, the frame-only guess gets a chance.
fn parse_tcp_payload(frame: &[u8], link_type: c_int, device_name: &str) -> Option<CapturedPayload> {
    if frame.len() < 4 {
        return None;
    }
    let ip_offset = link_header_len(link_type, frame)
        .filter(|&at| frame.get(at).is_some_and(|b| b >> 4 == 4))
        .or_else(|| guess_link_header_len(frame))?;

    if frame.len() < ip_offset + 20 {
        return None;
    }
    let ip_header = &frame[ip_offset..];
    if ip_header[0] >> 4 != 4 {
        return None;
    }
    let ip_header_len = ((ip_header[0] & 0x0F) as usize) * 4;
    if ip_header_len < 20 || ip_header_len > ip_header.len() || ip_header[9] != 6 {
        return None; // Invalid IPv4 header, or not TCP.
    }
    // This reader does not reassemble IP fragments. Neither a first fragment
    // nor a later one is a complete TCP segment; interpreting either as one
    // can put IP/TCP header bytes or unrelated fragment data into the stream.
    // Only the Don't Fragment flag may be set (the reserved flag is invalid).
    let fragment = u16::from_be_bytes([ip_header[6], ip_header[7]]);
    if fragment & 0xbfff != 0 {
        return None;
    }

    let ip_total_len = u16::from_be_bytes([ip_header[2], ip_header[3]]) as usize;
    if ip_total_len != 0 && ip_total_len < ip_header_len + 20 {
        return None;
    }
    let ip_end = ip_offset + ip_total_len;
    let end = if ip_total_len != 0 && ip_end <= frame.len() { ip_end } else { frame.len() };

    // Kept as numbers: formatting them as text cost two allocations for
    // every packet on every device, and nothing reads them as text.
    let src_ip = std::net::Ipv4Addr::new(ip_header[12], ip_header[13], ip_header[14], ip_header[15]);
    let dst_ip = std::net::Ipv4Addr::new(ip_header[16], ip_header[17], ip_header[18], ip_header[19]);

    let tcp_offset = ip_offset + ip_header_len;
    if end < tcp_offset + 20 {
        return None;
    }
    let tcp_header = &frame[tcp_offset..end];
    let src_port = u16::from_be_bytes([tcp_header[0], tcp_header[1]]);
    let dst_port = u16::from_be_bytes([tcp_header[2], tcp_header[3]]);
    let tcp_seq = u32::from_be_bytes([tcp_header[4], tcp_header[5], tcp_header[6], tcp_header[7]]);
    let tcp_ack =
        u32::from_be_bytes([tcp_header[8], tcp_header[9], tcp_header[10], tcp_header[11]]);
    let tcp_header_len = ((tcp_header[12] >> 4) as usize) * 4;
    if tcp_header_len < 20 || tcp_header_len > tcp_header.len() {
        return None;
    }

    // The payload ends where the IP packet says it does, not at the end of the
    // frame: Ethernet pads short frames to 60 bytes, so a bare ACK carries 6
    // padding bytes after its TCP header. Most network cards pad with zeros,
    // which the framing skips; some pad with whatever was in the buffer, and
    // those bytes landed in the game stream and threw the framing off. One
    // player's meter read 77% of a boss's damage while their party member's,
    // in the same fight, read all of it (2026-10-06). A total length of 0
    // (segmentation offload) or past the frame keeps the frame's end.
    let payload_offset = tcp_offset + tcp_header_len;
    if payload_offset >= end {
        return None;
    }
    let payload = &frame[payload_offset..end];
    if payload.is_empty() {
        return None;
    }

    Some(CapturedPayload {
        src_port,
        dst_port,
        data: payload.to_vec(),
        device_name: Some(device_name.to_string()),
        captured_at_ms: now_ms(),
        src_ip: Some(src_ip),
        dst_ip: Some(dst_ip),
        tcp_seq,
        tcp_ack,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrored_adapter_selection_distinguishes_identical_descriptions() {
        let first = DeviceInfo { name: "nic-a".into(), description: "Ethernet".into(), has_addresses: false, is_loopback: false };
        let second = DeviceInfo { name: "nic-b".into(), ..first.clone() };
        assert_eq!(first.capture_label(CaptureMode::Local), second.capture_label(CaptureMode::Local));
        let selected = first.capture_label(CaptureMode::Mirrored);
        assert_ne!(selected, second.capture_label(CaptureMode::Mirrored));
        assert!(CaptureMode::Mirrored.captures_device(false, &selected, Some(&selected)));
        assert!(!CaptureMode::Mirrored.captures_device(false, &second.capture_label(CaptureMode::Mirrored), Some(&selected)));
        let case_only = DeviceInfo { name: "NIC-A".into(), ..first.clone() };
        assert!(!CaptureMode::Mirrored.captures_device(false, &case_only.capture_label(CaptureMode::Mirrored), Some(&selected)));
    }

    /// IPv4 + TCP from 10.0.0.2:51000 to 193.202.112.99:13328, payload "hi".
    fn ipv4_tcp() -> Vec<u8> {
        let mut ip = vec![0x45, 0, 0, 42, 0, 0, 0, 0, 64, 6, 0, 0, 10, 0, 0, 2, 193, 202, 112, 99];
        let tcp = [0xC7, 0x38, 0x34, 0x10, 0, 0, 0, 1, 0, 0, 0, 0, 0x50, 0x18, 0, 0, 0, 0, 0, 0];
        ip.extend_from_slice(&tcp);
        ip.extend_from_slice(b"hi");
        ip
    }

    fn check(link_type: c_int, header: &[u8]) {
        let frame = [header, &ipv4_tcp()].concat();
        let p = parse_tcp_payload(&frame, link_type, "dev").expect("parsed");
        assert_eq!((p.src_port, p.dst_port), (51000, 13328), "link type {link_type}");
        assert_eq!(p.dst_ip, Some(std::net::Ipv4Addr::new(193, 202, 112, 99)));
        assert_eq!(p.data, b"hi");
    }

    #[test]
    fn reads_every_link_layer_the_meter_meets() {
        let ether = [[0u8; 12].as_slice(), &[0x08, 0x00]].concat();
        check(1, &ether); // Ethernet / Wi-Fi
        let vlan = [[0u8; 12].as_slice(), &[0x81, 0x00, 0x00, 0x05, 0x08, 0x00]].concat();
        check(1, &vlan); // Ethernet with an 802.1Q tag
        check(0, &[2, 0, 0, 0]); // Npcap / BSD loopback
        check(12, &[]); // raw IP (VPN, WireGuard)
        check(101, &[]);
        let sll = [[0u8; 14].as_slice(), &[0x08, 0x00]].concat();
        check(113, &sll); // Linux cooked v1
        let sll2 = [[0x08u8, 0x00].as_slice(), &[0u8; 18]].concat();
        check(276, &sll2); // Linux cooked v2
    }

    /// The filters, compiled by the system's libpcap and run on packets
    /// (`pcap_offline_filter`), for every link type the meter reads (raw IP
    /// as live captures report it, 12 and 228: 101 is its number in saved
    /// files only). Skipped where no libpcap is installed.
    #[test]
    fn the_capture_filter_passes_the_game_and_nothing_else() {
        let Ok(lib) = crate::platform::pcap::load_library() else {
            eprintln!("no libpcap: skipped");
            return;
        };
        type Compile = unsafe extern "C" fn(PcapT, *mut BpfProgram, *const c_char, c_int, u32) -> c_int;
        type Filter = unsafe extern "C" fn(*const BpfProgram, *const PcapPkthdr, *const u8) -> c_int;
        let (open_dead, compile, run, freecode, close) = unsafe {
            (
                *lib.get::<unsafe extern "C" fn(c_int, c_int) -> PcapT>(b"pcap_open_dead").unwrap(),
                *lib.get::<Compile>(b"pcap_compile").unwrap(),
                *lib.get::<Filter>(b"pcap_offline_filter").unwrap(),
                *lib.get::<unsafe extern "C" fn(*mut BpfProgram)>(b"pcap_freecode").unwrap(),
                *lib.get::<unsafe extern "C" fn(PcapT)>(b"pcap_close").unwrap(),
            )
        };
        let to_port = |port: u16| {
            let mut ip = ipv4_tcp();
            ip[22..24].copy_from_slice(&port.to_be_bytes());
            ip
        };
        let ether = [[0u8; 12].as_slice(), &[0x08, 0x00]].concat();
        let vlan = [[0u8; 12].as_slice(), &[0x81, 0x00, 0x00, 0x05, 0x08, 0x00]].concat();
        let qinq = [[0u8; 12].as_slice(), &[0x88, 0xA8, 0x00, 0x05, 0x81, 0x00, 0x00, 0x06, 0x08, 0x00]].concat();
        let sll = [[0u8; 14].as_slice(), &[0x08, 0x00]].concat();
        let sll2 = [[0x08u8, 0x00].as_slice(), &[0u8; 18]].concat();
        let links: [(c_int, &[u8]); 8] =
            [(1, &ether), (1, &vlan), (1, &qinq), (0, &[2, 0, 0, 0]), (12, &[]), (228, &[]), (113, &sll), (276, &sll2)];
        for (link, header) in links {
            let dead = unsafe { open_dead(link, 65535) };
            assert!(!dead.is_null());
            for (locked, cases) in [
                (None, [(13328, true), (5000, true), (443, false), (80, false)]),
                (Some(13328), [(13328, true), (5000, false), (443, false), (80, false)]),
            ] {
                // The first form where it compiles, as the capture does.
                let mut program = BpfProgram { bf_len: 0, bf_insns: ptr::null_mut() };
                let compiled = capture_filters(locked).iter().any(|text| {
                    let text = CString::new(text.as_str()).unwrap();
                    unsafe { compile(dead, &mut program, text.as_ptr(), 1, PCAP_NETMASK_UNKNOWN) == 0 }
                });
                assert!(compiled, "link type {link}: no filter compiles");
                for (port, passes) in cases {
                    let frame = [header, &to_port(port)].concat();
                    let hdr = PcapPkthdr { ts_sec: 0, ts_usec: 0, caplen: frame.len() as c_uint, len: frame.len() as c_uint };
                    let matched = unsafe { run(&program, &hdr, frame.as_ptr()) } != 0;
                    assert_eq!(matched, passes, "link type {link}, header {header:02x?}, locked {locked:?}, port {port}");
                }
                unsafe { freecode(&mut program) };
            }
            unsafe { close(dead) };
        }
    }

    #[test]
    fn the_safety_net_trips_after_a_minute_of_the_game_open_and_nothing_found() {
        let mut net = SafetyNet::default();
        assert!(!net.note(false, false, 0), "game closed: never");
        assert!(!net.note(true, false, 1_000));
        assert!(!net.note(true, false, 1_000 + SAFETY_NET_MS - 1));
        assert!(net.note(true, false, 1_000 + SAFETY_NET_MS));
        // A lock (or the game closing) starts the minute over.
        let mut net = SafetyNet::default();
        assert!(!net.note(true, false, 0));
        assert!(!net.note(true, true, SAFETY_NET_MS));
        assert!(!net.note(true, false, SAFETY_NET_MS + 1));
        assert!(!net.note(false, false, 2 * SAFETY_NET_MS));
        assert!(!net.note(true, false, 2 * SAFETY_NET_MS + 1));
    }

    #[test]
    fn the_filter_narrows_to_a_live_lock_and_widens_once_it_goes_quiet() {
        let detector = CombatPortDetector::new();
        assert_eq!(filter_port(&detector, now_ms(), "NPF_Loopback"), None, "nothing locked");
        let loopback = Some("NPF_Loopback");
        detector.register_candidate(13328, (13328, 50000), loopback);
        detector.confirm_candidate(13328, 50000, loopback);
        detector.mark_packet_parsed();
        let now = now_ms();
        // The locked device narrows only where that keeps its queue; any
        // other device narrows everywhere (none of the game's packets wait
        // there).
        let live = if crate::platform::pcap::FILTER_CHANGE_KEEPS_QUEUE { Some(13328) } else { None };
        assert_eq!(filter_port(&detector, now, "NPF_Loopback"), live);
        assert_eq!(filter_port(&detector, now, "Wi-Fi"), Some(13328));
        // A relay that reconnected elsewhere: nothing parsed for the stale
        // time, so the filter lets other traffic through to notice it.
        let quiet = now + crate::combat::capture_dispatcher::STALE_CONNECTION_MS + 1;
        assert_eq!(filter_port(&detector, quiet, "NPF_Loopback"), None);
        assert_eq!(filter_port(&detector, quiet, "Wi-Fi"), None);
    }

    #[test]
    fn an_unknown_or_mismatched_link_type_still_gets_the_old_guess() {
        // Ethernet framing reported as some other link type: guessed as before.
        let ether = [[0u8; 12].as_slice(), &[0x08, 0x00]].concat();
        check(999, &ether);
        // Raw IP on a device that claims Ethernet: the exact read fails, the
        // guess finds IPv4 at 0, as it always did.
        check(1, &[]);
    }

    #[test]
    fn ethernet_padding_is_not_payload() {
        // A bare ACK (IP total length 40) padded to 60 bytes with non-zero
        // bytes, as one player's network card sends them (2026-10-06).
        let ether = [[0u8; 12].as_slice(), &[0x08, 0x00]].concat();
        let mut ack = ipv4_tcp();
        ack.truncate(40);
        ack[3] = 40;
        let frame = [ether.as_slice(), &ack, &[0xbe, 0xde, 0x00, 0x03, 0x66, 0xfd]].concat();
        assert!(parse_tcp_payload(&frame, 1, "dev").is_none());

        // Data followed by padding keeps only the data.
        let mut short = ipv4_tcp();
        short.truncate(41);
        short[3] = 41;
        let frame = [ether.as_slice(), &short, &[0xbe, 0xde, 0x00, 0x03, 0x66]].concat();
        assert_eq!(parse_tcp_payload(&frame, 1, "dev").unwrap().data, b"h");

        // A total length of 0 (segmentation offload) keeps the frame's end.
        let mut tso = ipv4_tcp();
        tso[2] = 0;
        tso[3] = 0;
        let frame = [ether.as_slice(), &tso].concat();
        assert_eq!(parse_tcp_payload(&frame, 1, "dev").unwrap().data, b"hi");
    }

    #[test]
    fn impossible_header_lengths_and_short_declared_packets_are_rejected() {
        for ihl in 0..5 {
            let mut frame = ipv4_tcp();
            frame[0] = 0x40 | ihl;
            assert!(parse_tcp_payload(&frame, 12, "dev").is_none(), "IP IHL {ihl}");
        }
        let mut truncated_options = ipv4_tcp();
        truncated_options[0] = 0x4f;
        assert!(parse_tcp_payload(&truncated_options, 12, "dev").is_none());
        for offset in [0, 1, 2, 3, 4, 15] {
            let mut frame = ipv4_tcp();
            frame[32] = offset << 4;
            assert!(parse_tcp_payload(&frame, 12, "dev").is_none(), "TCP offset {offset}");
        }
        for total in [1u16, 19, 20, 39] {
            let mut frame = ipv4_tcp();
            frame[2..4].copy_from_slice(&total.to_be_bytes());
            // Bytes after the declared IP packet cannot supply its headers.
            frame.extend_from_slice(&[0x55; 40]);
            assert!(parse_tcp_payload(&frame, 12, "dev").is_none(), "IP length {total}");
        }
        let mut short_tcp_options = ipv4_tcp();
        short_tcp_options[32] = 0x60;
        short_tcp_options.extend_from_slice(&[0; 20]);
        assert!(parse_tcp_payload(&short_tcp_options, 12, "dev").is_none(), "options end past IP length");
    }

    #[test]
    fn fragmented_ip_datagrams_do_not_enter_the_tcp_stream() {
        for fragment in [0x2000u16, 0x0001, 0x2001, 0x8000] {
            let mut frame = ipv4_tcp();
            frame[6..8].copy_from_slice(&fragment.to_be_bytes());
            assert!(parse_tcp_payload(&frame, 12, "dev").is_none(), "fragment {fragment:#x}");
        }
        let mut dont_fragment = ipv4_tcp();
        dont_fragment[6..8].copy_from_slice(&0x4000u16.to_be_bytes());
        assert_eq!(parse_tcp_payload(&dont_fragment, 12, "dev").unwrap().data, b"hi");
    }

    #[test]
    fn valid_options_and_offloaded_lengths_keep_the_captured_payload() {
        let mut ip_options = ipv4_tcp();
        ip_options.splice(20..20, [0u8; 4]);
        ip_options[0] = 0x46;
        ip_options[2..4].copy_from_slice(&46u16.to_be_bytes());
        assert_eq!(parse_tcp_payload(&ip_options, 12, "dev").unwrap().data, b"hi");

        let mut tcp_options = ipv4_tcp();
        tcp_options.splice(40..40, [0u8; 4]);
        tcp_options[32] = 0x60;
        tcp_options[2..4].copy_from_slice(&46u16.to_be_bytes());
        assert_eq!(parse_tcp_payload(&tcp_options, 12, "dev").unwrap().data, b"hi");

        for total in [0u16, 1000] {
            let mut offloaded = ipv4_tcp();
            offloaded[2..4].copy_from_slice(&total.to_be_bytes());
            assert_eq!(parse_tcp_payload(&offloaded, 12, "dev").unwrap().data, b"hi");
            offloaded.truncate(39);
            assert!(parse_tcp_payload(&offloaded, 12, "dev").is_none(), "a whole TCP header is still required");
        }
    }

    #[test]
    fn non_ipv4_frames_are_ignored() {
        let arp = [[0u8; 12].as_slice(), &[0x08, 0x06], &[0u8; 28]].concat();
        assert!(parse_tcp_payload(&arp, 1, "dev").is_none());
    }
}

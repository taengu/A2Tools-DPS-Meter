use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use tracing::info;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// How long to hold off locking a physical/external port after the first combat
/// candidate, giving a loopback relay tunnel (ping-reducer) time to surface and
/// take precedence. A loopback flow locks immediately and ignores this window;
/// direct-connection users simply wait this long once before the first lock.
const LOOPBACK_GRACE_MS: i64 = 2500;
// Candidate assemblers are bounded too; discarded ports must not accumulate
// here while discovery runs. A recently active port keeps its place.
const MAX_CANDIDATE_PORTS: usize = 16;

/// Global combat port detector — identifies which network device and port
/// carry AION 2 game traffic by looking for the combat signature bytes.
pub struct CombatPortDetector {
    inner: Mutex<Inner>,
    last_parsed_at_ms: AtomicI64,
    /// The locked port (0: none), readable without the lock: every capture
    /// thread checks it for every packet, to filter for it (`pcap_capturer`).
    locked_port_now: AtomicU32,
    /// When the locked port last sent anything (unix ms; 0: not since the
    /// lock). Quiet for `PORT_MOVE_QUIET_MS`, the game may have moved to
    /// another connection (`capture_dispatcher`).
    last_from_port_ms: AtomicI64,
}

struct Inner {
    locked_port: Option<u16>,
    locked_device: Option<String>,
    candidates: HashMap<u16, (Option<String>, Instant)>,
    preferred_device: Option<String>,
    /// Wall-clock of the first combat candidate seen since the last reset; used
    /// to time the loopback-preference grace window.
    first_candidate_ms: i64,
}

impl CombatPortDetector {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                locked_port: None,
                locked_device: None,
                candidates: HashMap::new(),
                preferred_device: None,
                first_candidate_ms: 0,
            }),
            last_parsed_at_ms: AtomicI64::new(0),
            locked_port_now: AtomicU32::new(0),
            last_from_port_ms: AtomicI64::new(0),
        }
    }

    pub fn current_port(&self) -> Option<u16> {
        self.inner.lock().locked_port
    }

    /// The locked port, without taking the lock.
    pub fn locked_port_now(&self) -> Option<u16> {
        match self.locked_port_now.load(Ordering::Relaxed) {
            0 => None,
            port => Some(port as u16),
        }
    }

    pub fn current_device(&self) -> Option<String> {
        self.inner.lock().locked_device.clone()
    }

    pub fn last_parsed_at_ms(&self) -> i64 {
        self.last_parsed_at_ms.load(Ordering::Relaxed)
    }

    pub fn preferred_device(&self) -> Option<String> {
        self.inner.lock().preferred_device.clone()
    }

    pub fn set_preferred_device(&self, device: Option<String>) {
        self.inner.lock().preferred_device = device.map(|d| d.trim().to_string()).filter(|d| !d.is_empty());
    }

    pub fn register_candidate(&self, port: u16, _flow_key: (u16, u16), device_name: Option<&str>) {
        let mut inner = self.inner.lock();
        let trimmed = device_name.map(|d| d.trim().to_string()).filter(|d| !d.is_empty());

        if inner.locked_port.is_some() {
            // Promote to loopback if applicable
            if let Some(ref dev) = trimmed {
                if inner.locked_port == Some(port) && is_loopback(dev) && !is_loopback_opt(&inner.locked_device) {
                    info!("Switching combat device to loopback: {:?} -> {}", inner.locked_device, dev);
                    inner.locked_device = Some(dev.clone());
                }
            }
            return;
        }

        if inner.first_candidate_ms == 0 {
            inner.first_candidate_ms = now_ms();
        }

        if !inner.candidates.contains_key(&port) && inner.candidates.len() >= MAX_CANDIDATE_PORTS {
            if let Some(oldest) = inner.candidates.iter().min_by_key(|(_, (_, seen))| *seen).map(|(&p, _)| p) {
                inner.candidates.remove(&oldest);
            }
        }

        // Note: loopback flows are no longer locked here on sight. Locking is
        // gated on the flow actually producing combat (see confirm_candidate),
        // so a coincidental loopback service can't hijack the lock.
        let seen = Instant::now();
        inner.candidates.entry(port).and_modify(|c| c.1 = seen).or_insert((trimmed, seen));
    }

    pub fn confirm_candidate(&self, port_a: u16, port_b: u16, device_name: Option<&str>) {
        let mut inner = self.inner.lock();
        if inner.locked_port.is_some() {
            return;
        }

        let port = if inner.candidates.contains_key(&port_a) {
            Some(port_a)
        } else if inner.candidates.contains_key(&port_b) {
            Some(port_b)
        } else {
            None
        };

        let port = match port {
            Some(p) => p,
            None => return,
        };

        let trimmed = device_name.map(|d| d.trim().to_string()).filter(|d| !d.is_empty());
        let candidate_device = inner.candidates.get(&port).and_then(|c| c.0.clone());
        let device_for_lock = trimmed.or(candidate_device);

        // A loopback flow is the relay/ping-reducer's local tunnel — the exact
        // stream the game client consumes — so lock it the instant it produces
        // combat, ahead of any external session.
        if is_loopback_opt(&device_for_lock) {
            Self::lock_inner(&mut inner, port, device_for_lock);
            self.locked_port_now.store(port as u32, Ordering::Relaxed);
            self.last_from_port_ms.store(now_ms(), Ordering::Relaxed);
            return;
        }

        // Physical/external flow: hold off briefly. With a ping-reducer the same
        // combat arrives on several external sessions and is relayed to a
        // loopback tunnel a moment later; this grace lets that loopback flow
        // appear and win. If none shows up, commit to the physical port (direct
        // connection / no relay).
        let waited = now_ms() - inner.first_candidate_ms;
        if inner.first_candidate_ms != 0 && waited < LOOPBACK_GRACE_MS {
            return;
        }

        Self::lock_inner(&mut inner, port, device_for_lock);
        self.locked_port_now.store(port as u32, Ordering::Relaxed);
        self.last_from_port_ms.store(now_ms(), Ordering::Relaxed);
    }

    /// The locked port sent something.
    pub fn mark_from_port(&self) {
        self.last_from_port_ms.store(now_ms(), Ordering::Relaxed);
    }

    /// How long the locked port has been quiet, if a port is locked.
    pub fn port_quiet_ms(&self, now: i64) -> Option<i64> {
        self.locked_port_now()?;
        let last = self.last_from_port_ms.load(Ordering::Relaxed);
        Some(if last > 0 { now - last } else { 0 })
    }

    /// The game moved to another connection on another port (a server
    /// switch): keep the device, follow the port.
    pub fn move_port(&self, port: u16) {
        let mut inner = self.inner.lock();
        if inner.locked_port.is_some() && inner.locked_port != Some(port) {
            info!("Combat port moved: {:?} -> {}", inner.locked_port, port);
            inner.locked_port = Some(port);
            self.locked_port_now.store(port as u32, Ordering::Relaxed);
            self.last_from_port_ms.store(now_ms(), Ordering::Relaxed);
        }
    }

    fn lock_inner(inner: &mut Inner, port: u16, device: Option<String>) {
        if inner.locked_port.is_none() {
            inner.locked_port = Some(port);
            inner.locked_device = device.clone();
            info!("Combat port locked: {}", port);
            inner.candidates.clear();
        }
    }

    /// The game's traffic now arrives on another device, on the same port
    /// (a VPN connected or dropped, or the route for an instance server
    /// differs): keep the port, follow the device.
    pub fn move_device(&self, device: &str) {
        let mut inner = self.inner.lock();
        if inner.locked_port.is_some() {
            info!("Combat device moved: {:?} -> {}", inner.locked_device, device);
            inner.locked_device = Some(device.trim().to_string());
        }
    }

    pub fn mark_packet_parsed(&self) {
        self.last_parsed_at_ms.store(now_ms(), Ordering::Relaxed);
    }

    pub fn reset(&self) {
        let mut inner = self.inner.lock();
        let was_locked = inner.locked_port.is_some();
        inner.locked_port = None;
        inner.locked_device = None;
        inner.candidates.clear();
        inner.first_candidate_ms = 0;
        self.last_parsed_at_ms.store(0, Ordering::Relaxed);
        self.locked_port_now.store(0, Ordering::Relaxed);
        self.last_from_port_ms.store(0, Ordering::Relaxed);
        if was_locked {
            info!("Combat port lock cleared");
        }
    }
}

fn is_loopback(name: &str) -> bool {
    name.to_lowercase().contains("loopback")
}

fn is_loopback_opt(name: &Option<String>) -> bool {
    name.as_ref().is_some_and(|n| is_loopback(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_churn_keeps_recent_ports_and_the_first_device() {
        let detector = CombatPortDetector::new();
        for port in 1..=MAX_CANDIDATE_PORTS as u16 {
            detector.register_candidate(port, (port, 50_000), Some("eth0"));
        }
        // Make one old port unambiguously least recent without sleeping.
        detector.inner.lock().candidates.get_mut(&2).unwrap().1 -= std::time::Duration::from_secs(1);
        detector.register_candidate(1, (1, 50_001), Some("eth1"));
        detector.register_candidate(100, (100, 50_000), Some("eth2"));
        let inner = detector.inner.lock();
        assert_eq!(inner.candidates.len(), MAX_CANDIDATE_PORTS);
        assert!(!inner.candidates.contains_key(&2));
        assert_eq!(inner.candidates.get(&1).unwrap().0.as_deref(), Some("eth0"));
        assert!(inner.candidates.contains_key(&100));
    }

    #[test]
    fn new_loopback_candidate_can_lock_after_candidate_churn() {
        let detector = CombatPortDetector::new();
        for port in 1..=100 {
            detector.register_candidate(port, (port, 50_000), Some("eth0"));
            assert!(detector.inner.lock().candidates.len() <= MAX_CANDIDATE_PORTS);
        }
        let first = detector.inner.lock().first_candidate_ms;
        detector.register_candidate(13328, (13328, 50_000), Some("Npcap Loopback Adapter"));
        assert_eq!(detector.inner.lock().first_candidate_ms, first, "churn does not restart the grace period");
        detector.confirm_candidate(13328, 50_000, None);
        assert_eq!(detector.current_port(), Some(13328));
        assert_eq!(detector.current_device().as_deref(), Some("Npcap Loopback Adapter"));
        assert!(detector.inner.lock().candidates.is_empty());
    }
}

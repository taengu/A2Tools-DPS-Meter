/// Raw TCP payload captured from pcap.
#[derive(Debug, Clone)]
pub struct CapturedPayload {
    pub src_port: u16,
    pub dst_port: u16,
    pub data: Vec<u8>,
    pub device_name: Option<String>,
    pub captured_at_ms: i64,
    pub src_ip: Option<std::net::Ipv4Addr>,
    pub dst_ip: Option<std::net::Ipv4Addr>,
    pub tcp_seq: u32,
    pub tcp_ack: u32,
}

/// Both TCP endpoints and the capture device identify a connection. Ports
/// alone collide between remote hosts, and a relay can repeat the same bytes
/// on another device. The endpoints are sorted so replies have the same key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FlowKey {
    a: (Option<std::net::Ipv4Addr>, u16),
    b: (Option<std::net::Ipv4Addr>, u16),
    device: String,
}

impl CapturedPayload {
    /// Live IPv4 capture always records both endpoints, so sequence zero is a
    /// real sequence number there. Text-file replays have neither address nor
    /// sequence metadata, even when their timestamps look like live captures.
    pub fn has_tcp_sequence(&self) -> bool {
        self.tcp_seq != 0 || (self.src_ip.is_some() && self.dst_ip.is_some())
    }

    /// A process-local opaque connection tag keeps different endpoints out of
    /// the same saved stream without putting their network addresses into an
    /// Evidence Slice. Old captures without addresses retain their old names.
    pub fn stream_key(&self) -> String {
        use std::hash::{BuildHasher, Hash, Hasher};
        static HASHER: std::sync::OnceLock<std::collections::hash_map::RandomState> = std::sync::OnceLock::new();
        if self.src_ip.is_none() || self.dst_ip.is_none() {
            return stream_key(self.src_port, self.dst_port);
        }
        let mut hash = HASHER.get_or_init(std::collections::hash_map::RandomState::new).build_hasher();
        self.direction_key().hash(&mut hash);
        format!("Client:{:016x}:{}:{}", hash.finish(), self.dst_port, self.src_port)
    }

    /// One direction of the connection, for a TCP byte stream and its rate
    /// tracker. Bytes from the opposite direction must never be concatenated.
    pub fn direction_key(&self) -> FlowKey {
        FlowKey {
            a: (self.src_ip, self.src_port),
            b: (self.dst_ip, self.dst_port),
            device: self.device_name.as_deref().unwrap_or("").trim().to_ascii_lowercase(),
        }
    }

    pub fn flow_key(&self) -> FlowKey {
        let src = (self.src_ip, self.src_port);
        let dst = (self.dst_ip, self.dst_port);
        let (a, b) = if src <= dst { (src, dst) } else { (dst, src) };
        FlowKey {
            a,
            b,
            device: self.device_name.as_deref().unwrap_or("").trim().to_ascii_lowercase(),
        }
    }

    /// When libpcap captured the segment. A payload that never came from
    /// libpcap (a test, a replay) has no capture time worth using.
    pub fn capture_time_ms(&self) -> Option<i64> {
        (1_000_000_000_000..2_000_000_000_000).contains(&self.captured_at_ms).then_some(self.captured_at_ms)
    }
}

/// The stream name a server-to-client segment is logged and sliced under:
/// `Client:<client port>:<server port>`. One name per connection, so a second
/// connection from the same server port (the game server also talks TLS from
/// it, and a reconnect reuses it) never runs into the game's bytes. Readers
/// take the server port from after the last `:`, as they did from the older
/// `Client:<server port>`.
pub fn stream_key(server_port: u16, client_port: u16) -> String {
    format!("Client:{client_port}:{server_port}")
}

// Parser core — compiled for wasm32 as well as the desktop app.
pub mod abnormal;
pub mod abyss_artifacts;
pub mod supply_requests;
pub mod abyss_cores;
pub mod black_market;
pub mod boss_map;
pub mod rankings;
pub mod flight_power;
pub mod loot_rolls;
pub mod field_bosses;
pub mod captured_payload;
pub mod evidence_slice;
pub mod frame_tap;
pub mod framing;
pub mod packet_accumulator;
pub mod stream_assembler;
pub mod stream_processor;
pub mod servers;
pub mod tcp_order;
pub mod tunnel;

// Live capture. pcap needs libloading, the port detector reads the wall clock,
// and the file replay drives them both — none of which exist on wasm32.
#[cfg(feature = "desktop")]
pub mod combat_port_detector;
#[cfg(feature = "desktop")]
pub mod file_replay;
#[cfg(feature = "desktop")]
pub mod pcap_capturer;

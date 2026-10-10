//! A2Tools DPS Meter.
//!
//! The crate is split in two by the `desktop` feature, and the line is load
//! bearing rather than tidy-mindedness. Everything outside the feature — the
//! packet parser, the combat aggregation, the Evidence Slice builder — must
//! compile to `wasm32-unknown-unknown`, because the log service re-derives an
//! uploaded fight by running *this* code rather than trusting the numbers a
//! client sent. Anything that reaches for Tauri, pcap, HTTP or Windows lives
//! behind the feature.
//!
//! CI builds both. If a `use` of the desktop half creeps into the parser half,
//! the wasm build fails rather than the divergence being discovered later in a
//! log whose numbers nobody can reproduce.

// ── parser core: must stay wasm-clean ──────────────────────────────────────
pub mod capture;
pub mod clock;
pub mod combat;
pub mod entity;
pub mod i18n;
pub mod rederive;
pub mod supporters;

// ── desktop only ───────────────────────────────────────────────────────────
#[cfg(feature = "online")]
pub mod account;
#[cfg(feature = "desktop")]
pub mod config;
#[cfg(feature = "desktop")]
pub mod history;
#[cfg(feature = "desktop")]
pub mod logging;
#[cfg(feature = "desktop")]
pub mod platform;
#[cfg(feature = "online")]
mod presence;
#[cfg(feature = "desktop")]
pub mod share;
#[cfg(feature = "online")]
pub mod stream_overlay;
#[cfg(feature = "desktop")]
mod npcap_setup;

#[cfg(feature = "desktop")]
mod app;

#[cfg(feature = "desktop")]
mod atomic_file;
#[cfg(feature = "desktop")]
mod blocking;

#[cfg(feature = "desktop")]
pub use app::run;

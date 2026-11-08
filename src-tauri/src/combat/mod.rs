pub mod data_storage;
pub mod death_recap;
pub mod dps_calculator;
pub mod fight_buffs;
pub mod kda;
pub mod ping_tracker;
pub mod stagger;

// Owns the capture threads and the tokio channel they feed.
#[cfg(feature = "desktop")]
pub mod capture_dispatcher;

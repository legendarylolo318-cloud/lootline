//! Lootline core: events, odds math, loot tables, storage, themes, DSP.
//! Pure Rust, no platform-specific code.

pub mod dsp;
pub mod event;
pub mod loot;
pub mod odds;
pub mod settings;
pub mod stats;
pub mod storage;
pub mod theme;

pub use event::{Event, EventLog, PlayerId, PlayerInfo, Rarity};

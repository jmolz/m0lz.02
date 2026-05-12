//! Phase 7: event-bus + manifest-saver subsystem.
//!
//! Two concerns, two files:
//!
//! - [`bus`] — the [`EventBus`]. `broadcast::Sender<ManifestEventPayload>`
//!   fan-out: one per-feature channel + one wildcard channel. All
//!   orchestrator event emission lives on the bus; `manifest/subscribe`
//!   subscribers (Task 6) consume it.
//! - [`saver`] — the [`ManifestSaver`] trait + its production
//!   [`EventEmittingSaver`] impl. Every manifest state transition goes
//!   through `save_and_emit(..., intent)` so the save + bus-publish pair
//!   is coupled at every call site. The orchestrator never calls
//!   `VerificationManifest::save` directly (pinned by Task 9's
//!   grep-assertion coverage test).
//!
//! The two files are independent: `bus` has no `saver` dependency, and
//! `saver` owns only a `&EventBus` reference. Downstream code assembles
//! them inside the daemon's `DaemonContext`.

pub mod bus;
pub mod saver;

pub use bus::EventBus;
pub use saver::{
    terminal_save_intent_for_manifest, EventEmittingSaver, EventEmittingSaverHooks, ManifestSaver,
    NullSaver, SaveIntent,
};

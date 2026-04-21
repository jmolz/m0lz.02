//! Phase 7: captured-provider-session log store.
//!
//! Tasks 4/5/6/13 pipeline:
//! - The orchestrator captures chunks from provider sessions and
//!   forwards them to [`LogStore::append_chunk`] as they arrive
//!   (Task 9 wires the forwarding).
//! - Task 6's `logs/stream` subscribe handler reads via
//!   [`LogStore::snapshot`] + [`LogStore::subscribe`].
//! - Task 13's `pice logs` CLI command lands the one-shot snapshot
//!   path.
//!
//! See [`store`] for the concrete implementation and its invariants.

pub mod store;

pub use store::{LogStore, BUFFER_BYTES_CAP, CHANNEL_CAPACITY};

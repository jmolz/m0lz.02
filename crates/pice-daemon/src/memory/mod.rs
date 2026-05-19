//! Daemon-owned PICE summary memory.
//!
//! Storage, recall, redaction, and lifecycle recording live here so prompt
//! builders can remain pure presentation functions that receive precomputed
//! `MemoryBrief` values.

pub mod recall;
pub mod recorder;
pub mod redaction;
pub mod store;

pub use recorder::{MemoryWriteOutcome, SessionMemoryRecorder, SessionRunContext};
pub use store::{MemoryPaths, MemoryStoreStats};

//! PICE-owned summary memory types and policy.
//!
//! This module is deliberately pure: no filesystem, SQLite, provider calls,
//! or process environment reads. The daemon owns storage and lifecycle; core
//! owns the serializable shapes and default-deny policy shared by handlers.

pub mod policy;
pub mod types;

pub use policy::MemoryPolicy;
pub use types::{
    estimate_tokens, stable_record_id, MemoryBrief, MemoryBriefRecord, MemoryConsumer,
    MemoryRecord, MemoryStore, MemoryWriter, RedactionStatus,
};

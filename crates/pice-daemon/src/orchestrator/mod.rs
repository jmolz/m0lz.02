//! Provider orchestrator — manages the AI provider lifecycle and session
//! execution. Moved here from `pice-cli/src/engine/` in T12.
//!
//! Owns the `StreamSink` trait that severs the legacy dependency on
//! `pice-cli::engine::output`, allowing the orchestrator to emit output
//! events to any sink (the CLI's terminal renderer, the daemon's socket
//! notification relay, a test buffer, etc.).

pub mod stream;

pub use stream::{NoticeLevel, NullSink, SharedSink, StreamEvent, StreamSink};

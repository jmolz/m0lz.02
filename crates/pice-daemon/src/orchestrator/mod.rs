//! Provider orchestrator — manages the AI provider lifecycle and session
//! execution. Moved here from `pice-cli/src/engine/` in T12.
//!
//! Owns the [`StreamSink`] trait that severs the legacy dependency on
//! `pice-cli::engine::output`, allowing the orchestrator to emit output
//! events to any sink (the CLI's terminal renderer, the daemon's socket
//! notification relay, a test buffer, etc.).
//!
//! ## Module layout
//!
//! - [`core`] — `ProviderOrchestrator` struct + evaluation lifecycle
//! - [`session`] — `run_session` / `run_session_and_capture` helpers that
//!   drive the `session/create → session/send → session/destroy` RPC trio
//! - [`stream`] — `StreamSink` trait, `StreamEvent`, `NullSink`, and the
//!   `SharedSink` type alias used across the orchestrator boundary
//!
//! `ProviderOrchestrator` is re-exported at the module root so callers use
//! `pice_daemon::orchestrator::ProviderOrchestrator` rather than the nested
//! `pice_daemon::orchestrator::core::ProviderOrchestrator` path.

pub mod core;
pub mod session;
pub mod stack_loops;
pub mod stream;

pub use core::ProviderOrchestrator;
pub use stream::{NoticeLevel, NullSink, SharedSink, StreamEvent, StreamSink};

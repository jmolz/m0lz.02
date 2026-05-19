//! # pice-daemon
//!
//! Long-lived orchestration daemon for PICE. Listens on a Unix domain socket
//! (`~/.pice/daemon.sock`) or Windows named pipe (`\\.\pipe\pice-daemon`) and
//! dispatches `CommandRequest` RPCs from the CLI adapter to in-process command
//! handlers. Owns the provider host, the metrics database writes, the
//! verification manifest state (v0.2 Phase 1+), and the Stack Loops orchestrator
//! (v0.2 Phase 2+).
//!
//! ## Crate shape
//!
//! This crate is both a library (`pice_daemon`) AND a binary (`pice-daemon`):
//! - `pice-cli` imports `pice_daemon::inline::run_command` for the
//!   `PICE_DAEMON_INLINE=1` escape hatch.
//! - The `pice-daemon` binary entry point (`main.rs`) simply calls
//!   `lifecycle::run().await` to start the event loop.
//!
//! ## Module map
//!
//! | Module | Purpose | Populated by task |
//! |--------|---------|-------------------|
//! | [`server`] | Socket listener, transport impls, auth, RPC router | T15–T18 |
//! | [`orchestrator`] | Provider lifecycle + `run_session` + `StreamSink` trait | T12 |
//! | [`provider`] | Async `ProviderHost` (tokio process spawner) | T12 |
//! | [`metrics`] | SQLite writes + telemetry (non-fatal recording) | T14 |
//! | [`prompt`] | Context-assembly prompt builders | T13 |
//! | [`handlers`] | Per-command async handlers (init/prime/plan/…) | T19 |
//! | [`inline`] | `run_command` entry point for inline mode | T20 |
//! | [`lifecycle`] | Startup, signal handling, graceful shutdown | T21 |
//! | [`logging`] | Tracing setup with rolling file appender | T11 (stub), T21 (full) |
//!
//! See `.claude/rules/daemon.md` for the architectural invariants this crate
//! enforces (auth token handling, manifest-as-source-of-truth, single-daemon
//! prevention, graceful shutdown budget, etc.).

pub mod clock;
pub mod events;
pub mod handlers;
pub mod inline;
pub mod jobs;
pub mod lifecycle;
pub mod logging;
pub mod logs;
pub mod memory;
pub mod metrics;
pub mod orchestrator;
pub mod prompt;
pub mod provider;
pub mod server;
pub mod templates;
pub mod test_support;

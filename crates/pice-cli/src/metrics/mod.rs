//! Metrics module — CLI-side facade.
//!
//! In T14, the write side (`db`, `store`, `telemetry`) moved to
//! `pice-daemon::metrics`. Read-only aggregation (`aggregator`) stays in
//! `pice-cli` because `pice metrics` and `pice benchmark` are CLI commands
//! that only query the database.
//!
//! The `pub use` re-exports below let existing callers keep using
//! `crate::metrics::{db, store, telemetry, open_metrics_db, ...}` unchanged
//! while the underlying code lives in pice-daemon. This is a path alias, not
//! duplication — the rules in `.claude/rules/daemon.md` prohibit duplicated
//! logic between the two crates, not shared-via-reexport APIs.
//!
//! When T19 converts the CLI writer commands (evaluate, plan, execute,
//! commit) to daemon RPC calls, the re-exports for `db`/`store`/`telemetry`
//! will be removed and the CLI will stop linking them directly.

pub mod aggregator;

pub use pice_daemon::metrics::{
    db, normalize_plan_path, open_metrics_db, resolve_metrics_db_path, store, telemetry,
};

//! SQLite metrics writer + telemetry HTTP sender.
//!
//! Moved here from `pice-cli/src/metrics/` in T14. The daemon owns ALL writes
//! to the metrics database. The CLI reads from the same database for reporting
//! (`pice metrics`, `pice status`, `pice benchmark`) via its own
//! `metrics::aggregator` module, but never writes directly.
//!
//! Non-fatal recording pattern per `.claude/rules/metrics.md` — write failures
//! log at `warn` and continue. See also `.claude/rules/daemon.md` for the
//! crate-boundary invariants this module participates in.
//!
//! ## Layout
//!
//! - [`db`] — `MetricsDb` connection wrapper + schema migrations.
//! - [`store`] — insert/query functions for evaluations, loop events, and
//!   the telemetry queue.
//! - [`telemetry`] — opt-in anonymized telemetry client + HTTP `send_batch`.
//!
//! `pice-cli::metrics::aggregator` (read-only aggregation queries) lives in
//! pice-cli and imports the `MetricsDb` type from here.

pub mod db;
pub mod store;
pub mod telemetry;

use anyhow::Result;
use std::path::{Path, PathBuf};

use pice_core::config::PiceConfig;

/// Re-exported from `pice_core::paths` so callers can use
/// `pice_daemon::metrics::normalize_plan_path(..)` alongside the other
/// helpers in this module. The implementation lives in pice-core and is
/// shared between both the CLI read path and the daemon write path.
pub use pice_core::paths::normalize_plan_path;

/// Open the metrics database for a project.
///
/// Returns `Ok(None)` when the DB file doesn't exist yet (uninitialized
/// project) so callers using the non-fatal recording pattern can skip
/// recording without erroring. Returns `Err(..)` only when the file exists
/// but cannot be opened (corrupt DB, permission error, etc.).
pub fn open_metrics_db(project_root: &Path) -> Result<Option<db::MetricsDb>> {
    let config_path = project_root.join(".pice/config.toml");
    let config = PiceConfig::load(&config_path).unwrap_or_else(|_| PiceConfig::default());
    let db_path = project_root.join(&config.metrics.db_path);
    if !db_path.exists() {
        return Ok(None);
    }
    Ok(Some(db::MetricsDb::open(&db_path)?))
}

/// Resolve the configured metrics DB path for a project.
///
/// Used by `pice init` to know where to create the DB file even when one
/// doesn't exist yet. Does not touch the filesystem beyond reading the
/// project's `.pice/config.toml`.
pub fn resolve_metrics_db_path(project_root: &Path) -> PathBuf {
    let config_path = project_root.join(".pice/config.toml");
    let config = PiceConfig::load(&config_path).unwrap_or_else(|_| PiceConfig::default());
    project_root.join(&config.metrics.db_path)
}

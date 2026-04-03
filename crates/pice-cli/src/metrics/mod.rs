pub mod aggregator;
pub mod db;
pub mod store;
pub mod telemetry;

use anyhow::Result;
use std::path::Path;

use crate::config::PiceConfig;

/// Open the metrics database for a project.
/// Returns None if the DB file doesn't exist (project not initialized).
pub fn open_metrics_db(project_root: &Path) -> Result<Option<db::MetricsDb>> {
    let config_path = project_root.join(".pice/config.toml");
    let config = PiceConfig::load(&config_path).unwrap_or_else(|_| PiceConfig::default());
    let db_path = project_root.join(&config.metrics.db_path);
    if !db_path.exists() {
        return Ok(None);
    }
    Ok(Some(db::MetricsDb::open(&db_path)?))
}

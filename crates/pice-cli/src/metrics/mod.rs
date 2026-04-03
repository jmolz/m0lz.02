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

/// Resolve the configured metrics DB path for a project (for init).
pub fn resolve_metrics_db_path(project_root: &Path) -> std::path::PathBuf {
    let config_path = project_root.join(".pice/config.toml");
    let config = PiceConfig::load(&config_path).unwrap_or_else(|_| PiceConfig::default());
    project_root.join(&config.metrics.db_path)
}

/// Normalize a plan path to a project-relative canonical form.
/// Converts absolute paths and various relative spellings to `.claude/plans/<filename>`.
/// This ensures consistent keys in the metrics DB regardless of how the user invoked the command.
pub fn normalize_plan_path(plan_path: &str, project_root: &Path) -> String {
    let path = std::path::Path::new(plan_path);

    // Try to extract the filename
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(plan_path);

    // If the path contains ".claude/plans/", extract the suffix after it
    if let Some(idx) = plan_path.find(".claude/plans/") {
        return plan_path[idx..].to_string();
    }

    // If it's an absolute path, try to make it relative to project_root
    if path.is_absolute() {
        if let Ok(rel) = path.strip_prefix(project_root) {
            return rel.to_string_lossy().to_string();
        }
    }

    // Default: normalize to .claude/plans/<filename>
    format!(".claude/plans/{filename}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn normalize_relative_path() {
        let root = PathBuf::from("/project");
        assert_eq!(
            normalize_plan_path(".claude/plans/test.md", &root),
            ".claude/plans/test.md"
        );
    }

    #[test]
    fn normalize_absolute_path_with_project_root() {
        let root = PathBuf::from("/project");
        assert_eq!(
            normalize_plan_path("/project/.claude/plans/test.md", &root),
            ".claude/plans/test.md"
        );
    }

    #[test]
    fn normalize_dotslash_path() {
        let root = PathBuf::from("/project");
        assert_eq!(
            normalize_plan_path("./.claude/plans/test.md", &root),
            ".claude/plans/test.md"
        );
    }

    #[test]
    fn normalize_bare_filename() {
        let root = PathBuf::from("/project");
        assert_eq!(
            normalize_plan_path("test.md", &root),
            ".claude/plans/test.md"
        );
    }

    #[test]
    fn normalize_absolute_outside_project() {
        let root = PathBuf::from("/project");
        // Absolute path outside project root falls back to filename
        assert_eq!(
            normalize_plan_path("/other/place/test.md", &root),
            ".claude/plans/test.md"
        );
    }
}

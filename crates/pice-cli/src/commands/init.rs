use anyhow::Result;
use clap::Args;
use std::path::Path;
use tracing::info;

use crate::config::PiceConfig;
use crate::templates::extract_templates;

#[derive(Args, Debug)]
pub struct InitArgs {
    /// Overwrite existing files instead of skipping them
    #[arg(long)]
    pub force: bool,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: &InitArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    run_in(&cwd, args.force, args.json)
}

/// Core init logic, testable with an explicit base directory.
pub fn run_in(base: &Path, force: bool, json: bool) -> Result<()> {
    let claude_dir = base.join(".claude");
    let pice_dir = base.join(".pice");

    if !json {
        println!("Scaffolding .claude/ directory...");
    }
    let claude_result = extract_templates(&claude_dir, "claude/", force)?;

    if !json {
        println!("Scaffolding .pice/ directory...");
    }
    let pice_result = extract_templates(&pice_dir, "pice/", force)?;

    // Verify the scaffolded config is valid by loading it
    let config_path = pice_dir.join("config.toml");
    if config_path.exists() {
        match PiceConfig::load(&config_path) {
            Ok(config) => {
                info!(
                    provider = %config.provider.name,
                    eval_model = %config.evaluation.primary.model,
                    "loaded config"
                );
            }
            Err(e) => {
                tracing::warn!("config.toml exists but failed to parse: {e}");
            }
        }
    }

    // Initialize metrics database with schema (or run migrations on existing DB).
    // Resolve path from config (supports non-default db_path).
    // Never delete an existing DB — it contains evaluation history.
    let metrics_db = crate::metrics::resolve_metrics_db_path(base);
    if !metrics_db.exists() {
        if let Some(parent) = metrics_db.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::metrics::db::MetricsDb::open(&metrics_db)?;
        info!(path = %metrics_db.display(), "initialized metrics database");
    } else if force {
        // On --force, open the existing DB and run any pending migrations
        // instead of destroying evaluation history.
        crate::metrics::db::MetricsDb::open(&metrics_db)?;
        info!(path = %metrics_db.display(), "migrated existing metrics database");
    }

    let total_created = claude_result.created.len() + pice_result.created.len();
    let total_skipped = claude_result.skipped.len() + pice_result.skipped.len();

    if json {
        let created: Vec<String> = claude_result
            .created
            .iter()
            .map(|f| format!(".claude/{f}"))
            .chain(pice_result.created.iter().map(|f| format!(".pice/{f}")))
            .collect();
        let skipped: Vec<String> = claude_result
            .skipped
            .iter()
            .map(|f| format!(".claude/{f}"))
            .chain(pice_result.skipped.iter().map(|f| format!(".pice/{f}")))
            .collect();
        let output = serde_json::json!({
            "created": created,
            "skipped": skipped,
            "totalCreated": total_created,
            "totalSkipped": total_skipped,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!();
        if total_created > 0 {
            println!("Created {} files:", total_created);
            for f in &claude_result.created {
                println!("  .claude/{f}");
            }
            for f in &pice_result.created {
                println!("  .pice/{f}");
            }
        }

        if total_skipped > 0 {
            println!(
                "Skipped {} existing files (use --force to overwrite)",
                total_skipped
            );
        }

        println!();
        println!("PICE initialized. Run `pice prime` to orient on your codebase.");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_creates_claude_directory() {
        let dir = tempfile::tempdir().unwrap();

        run_in(dir.path(), false, false).unwrap();

        assert!(dir.path().join(".claude/commands/plan-feature.md").exists());
        assert!(dir
            .path()
            .join(".claude/templates/plan-template.md")
            .exists());
        assert!(dir.path().join(".claude/docs/PLAYBOOK.md").exists());
        assert!(dir.path().join(".pice/config.toml").exists());
        assert!(dir.path().join(".pice/metrics.db").exists());
    }

    #[test]
    fn init_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();

        run_in(dir.path(), false, false).unwrap();

        // Modify a file
        let plan_path = dir.path().join(".claude/commands/plan-feature.md");
        std::fs::write(&plan_path, "custom content").unwrap();

        // Run again — should not overwrite
        run_in(dir.path(), false, false).unwrap();

        let content = std::fs::read_to_string(&plan_path).unwrap();
        assert_eq!(content, "custom content");
    }

    #[test]
    fn init_force_overwrites() {
        let dir = tempfile::tempdir().unwrap();

        run_in(dir.path(), false, false).unwrap();

        // Modify a file
        let plan_path = dir.path().join(".claude/commands/plan-feature.md");
        std::fs::write(&plan_path, "custom content").unwrap();

        // Force init should overwrite
        run_in(dir.path(), true, false).unwrap();

        let content = std::fs::read_to_string(&plan_path).unwrap();
        assert_ne!(content, "custom content");
    }

    #[test]
    fn init_json_output() {
        let dir = tempfile::tempdir().unwrap();

        // Capture would require redirecting stdout; just verify it doesn't error
        run_in(dir.path(), false, true).unwrap();

        assert!(dir.path().join(".claude/commands/plan-feature.md").exists());
        assert!(dir.path().join(".pice/config.toml").exists());
    }
}

//! Embedded template extraction for `pice init`.
//!
//! Mirrors `pice-cli/src/templates/mod.rs` — the daemon owns the actual
//! scaffolding logic; the CLI adapter delegates via the daemon RPC.
//!
//! Templates are embedded at build time from `../../templates/` (the
//! workspace-root `templates/` directory) using `rust-embed`.

use anyhow::{Context, Result};
use rust_embed::Embed;
use std::path::Path;
use tracing::info;

#[derive(Embed)]
#[folder = "../../templates/"]
struct TemplateAssets;

/// Extraction result tracking what was created vs skipped.
pub struct ExtractionResult {
    pub created: Vec<String>,
    pub skipped: Vec<String>,
}

/// Extract embedded template files matching `prefix` to `target_dir`.
///
/// Files that already exist at the target path are skipped (idempotent)
/// unless `force` is true. Directories are created as needed.
pub fn extract_templates(target_dir: &Path, prefix: &str, force: bool) -> Result<ExtractionResult> {
    let mut result = ExtractionResult {
        created: Vec::new(),
        skipped: Vec::new(),
    };

    for file_path in TemplateAssets::iter() {
        let file_path_str = file_path.as_ref();
        if !file_path_str.starts_with(prefix) {
            continue;
        }

        // Strip the prefix to get the relative path under target_dir.
        // The starts_with check above guarantees this succeeds.
        // Phase 4.1 Pass-6 C13: the `expect` is preceded by an explicit
        // `starts_with(prefix)` guard 6 lines above, making the strip
        // provably infallible. Grandfathered under
        // `-D clippy::expect_used`.
        #[allow(clippy::expect_used)]
        let relative = file_path_str
            .strip_prefix(prefix)
            .expect("prefix was verified by starts_with above");
        let target_path = target_dir.join(relative);

        if target_path.exists() && !force {
            info!(path = %target_path.display(), "skipping (already exists)");
            result.skipped.push(relative.to_string());
            continue;
        }

        if let Some(parent) = target_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory {}", parent.display()))?;
        }

        let asset = TemplateAssets::get(file_path_str)
            .with_context(|| format!("embedded asset not found: {file_path_str}"))?;
        std::fs::write(&target_path, asset.data.as_ref())
            .with_context(|| format!("failed to write {}", target_path.display()))?;

        info!(path = %target_path.display(), "created");
        result.created.push(relative.to_string());
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_templates_are_available() {
        let files: Vec<String> = TemplateAssets::iter().map(|f| f.to_string()).collect();
        assert!(!files.is_empty(), "no templates embedded");
        assert!(
            files.iter().any(|f| f.starts_with("claude/")),
            "missing claude/ templates"
        );
        assert!(
            files.iter().any(|f| f.starts_with("pice/")),
            "missing pice/ templates"
        );
    }

    #[test]
    fn extract_claude_templates_to_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(".claude");

        let result = extract_templates(&target, "claude/", false).unwrap();

        assert!(!result.created.is_empty(), "nothing was extracted");
        assert!(target.join("commands/plan-feature.md").exists());
        assert!(target.join("templates/plan-template.md").exists());
        assert!(target.join("docs/PLAYBOOK.md").exists());
    }

    #[test]
    fn extract_pice_templates_to_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(".pice");

        let result = extract_templates(&target, "pice/", false).unwrap();

        assert!(!result.created.is_empty());
        assert!(target.join("config.toml").exists());
    }

    #[test]
    fn extract_is_idempotent_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(".claude");

        // First extraction
        let first = extract_templates(&target, "claude/", false).unwrap();
        assert!(!first.created.is_empty());
        assert!(first.skipped.is_empty());

        // Second extraction — everything should be skipped
        let second = extract_templates(&target, "claude/", false).unwrap();
        assert!(second.created.is_empty());
        assert_eq!(second.skipped.len(), first.created.len());
    }

    #[test]
    fn extract_with_force_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(".claude");

        // First extraction
        extract_templates(&target, "claude/", false).unwrap();

        // Modify a file
        let plan_path = target.join("commands/plan-feature.md");
        std::fs::write(&plan_path, "custom content").unwrap();

        // Force extraction should overwrite
        let result = extract_templates(&target, "claude/", true).unwrap();
        assert!(!result.created.is_empty());

        let content = std::fs::read_to_string(&plan_path).unwrap();
        assert_ne!(content, "custom content");
    }
}

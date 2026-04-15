//! Category 12 — Resource exhaustion at boundaries.
//!
//! Detects pool/connection/thread counts that exceed a safe threshold
//! without downstream capacity checks. Emits `Failed` when the configured
//! value is extreme; otherwise `Warning` when mildly high.

use crate::seam::types::{LayerBoundary, SeamCheck, SeamContext, SeamFinding, SeamResult};

const WARN_THRESHOLD: u32 = 50;
const FAIL_THRESHOLD: u32 = 500;

pub struct ResourceExhaustionCheck;

impl SeamCheck for ResourceExhaustionCheck {
    fn id(&self) -> &str {
        "resource_exhaustion"
    }
    fn category(&self) -> u8 {
        12
    }
    fn applies_to(&self, _boundary: &LayerBoundary) -> bool {
        true
    }
    fn run(&self, ctx: &SeamContext<'_>) -> SeamResult {
        let mut warnings: Vec<SeamFinding> = Vec::new();
        let mut failures: Vec<SeamFinding> = Vec::new();
        for rel in ctx.boundary_files {
            let full = ctx.repo_root.join(rel);
            let Ok(content) = std::fs::read_to_string(&full) else {
                continue;
            };
            for (key, value) in scan_numeric(
                &content,
                &[
                    "pool_size",
                    "max_connections",
                    "worker_threads",
                    "max_pool_size",
                    "connection_pool_size",
                ],
            ) {
                if value >= FAIL_THRESHOLD {
                    failures.push(
                        SeamFinding::new(format!(
                            "{key} = {value} in {} — exceeds safe upper bound ({FAIL_THRESHOLD})",
                            rel.display()
                        ))
                        .with_file(rel.clone()),
                    );
                } else if value >= WARN_THRESHOLD {
                    warnings.push(
                        SeamFinding::new(format!(
                            "{key} = {value} in {} — above typical ({WARN_THRESHOLD}); \
                             verify downstream capacity",
                            rel.display()
                        ))
                        .with_file(rel.clone()),
                    );
                }
            }
        }
        if !failures.is_empty() {
            SeamResult::Failed(failures)
        } else if !warnings.is_empty() {
            SeamResult::Warning(warnings)
        } else {
            SeamResult::Passed
        }
    }
}

fn scan_numeric(content: &str, keys: &[&str]) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    for line in content.lines() {
        let t = line.trim();
        for key in keys {
            if let Some(rest) = t.strip_prefix(*key) {
                let tail = rest.trim_start();
                if let Some(rest) = tail.strip_prefix([':', '=']) {
                    let digits: String = rest
                        .trim()
                        .chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect();
                    if let Ok(v) = digits.parse::<u32>() {
                        out.push(((*key).to_string(), v));
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(files: &[(&str, &str)]) -> (tempfile::TempDir, Vec<PathBuf>) {
        let dir = tempfile::tempdir().unwrap();
        let mut rels = Vec::new();
        for (rel, content) in files {
            let full = dir.path().join(rel);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&full, content).unwrap();
            rels.push(PathBuf::from(rel));
        }
        (dir, rels)
    }

    #[test]
    fn passes_at_safe_values() {
        let (dir, rels) = fixture(&[("db.toml", "pool_size = 10\n")]);
        let b = LayerBoundary::new("backend", "database");
        let ctx = SeamContext {
            boundary: &b,
            filtered_diff: "",
            repo_root: dir.path(),
            boundary_files: &rels,
            args: None,
        };
        assert_eq!(ResourceExhaustionCheck.run(&ctx), SeamResult::Passed);
    }

    #[test]
    fn warns_above_warn_threshold() {
        let (dir, rels) = fixture(&[("db.toml", "pool_size = 100\n")]);
        let b = LayerBoundary::new("backend", "database");
        let ctx = SeamContext {
            boundary: &b,
            filtered_diff: "",
            repo_root: dir.path(),
            boundary_files: &rels,
            args: None,
        };
        assert!(ResourceExhaustionCheck.run(&ctx).is_warning());
    }

    #[test]
    fn fails_above_fail_threshold() {
        let (dir, rels) = fixture(&[("db.toml", "pool_size = 1000\n")]);
        let b = LayerBoundary::new("backend", "database");
        let ctx = SeamContext {
            boundary: &b,
            filtered_diff: "",
            repo_root: dir.path(),
            boundary_files: &rels,
            args: None,
        };
        assert!(ResourceExhaustionCheck.run(&ctx).is_failed());
    }

    #[test]
    fn always_applies() {
        assert!(ResourceExhaustionCheck.applies_to(&LayerBoundary::new("x", "y")));
    }
}

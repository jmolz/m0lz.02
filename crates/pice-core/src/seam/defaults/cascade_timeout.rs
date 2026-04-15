//! Category 5 — Cascading failures from dependencies.
//!
//! **v0.2 static heuristic**: scans config files for `retries × timeout`
//! products and warns when the total exceeds a parent-timeout bound. Full
//! semantics require runtime trace analysis (v0.4 implicit contract
//! inference). This check ALWAYS emits `Warning`, never `Failed`.

use crate::seam::types::{LayerBoundary, SeamCheck, SeamContext, SeamFinding, SeamResult};

pub struct CascadeTimeoutCheck;

impl SeamCheck for CascadeTimeoutCheck {
    fn id(&self) -> &str {
        "cascade_timeout"
    }
    fn category(&self) -> u8 {
        5
    }
    fn applies_to(&self, _boundary: &LayerBoundary) -> bool {
        true
    }
    fn run(&self, ctx: &SeamContext<'_>) -> SeamResult {
        let mut findings: Vec<SeamFinding> = Vec::new();
        for rel in ctx.boundary_files {
            let full = ctx.repo_root.join(rel);
            let Ok(content) = std::fs::read_to_string(&full) else {
                continue;
            };
            // Collect `retries: N` and `timeout: T` proximally — if both appear
            // in the same file and their product is large, warn.
            let retries = find_numeric_key(&content, "retries").unwrap_or(0);
            let timeout = find_numeric_key(&content, "timeout").unwrap_or(0);
            if retries > 0 && timeout > 0 && retries * timeout >= 30 {
                findings.push(
                    SeamFinding::new(format!(
                        "retries × timeout = {} in {} — cascade budget may exceed upstream patience",
                        retries * timeout,
                        rel.display()
                    ))
                    .with_file(rel.clone()),
                );
            }
        }
        if findings.is_empty() {
            SeamResult::Passed
        } else {
            SeamResult::Warning(findings)
        }
    }
}

fn find_numeric_key(content: &str, key: &str) -> Option<u32> {
    for line in content.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix(key) {
            let tail = rest.trim_start();
            if let Some(rest) = tail.strip_prefix([':', '=']) {
                let digits: String = rest
                    .trim()
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(v) = digits.parse::<u32>() {
                    return Some(v);
                }
            }
        }
    }
    None
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
    fn passes_under_threshold() {
        let (dir, rels) = fixture(&[("config.yaml", "retries: 2\ntimeout: 5\n")]);
        let b = LayerBoundary::new("a", "b");
        let ctx = SeamContext {
            boundary: &b,
            filtered_diff: "",
            repo_root: dir.path(),
            boundary_files: &rels,
            args: None,
        };
        assert_eq!(CascadeTimeoutCheck.run(&ctx), SeamResult::Passed);
    }

    #[test]
    fn warns_over_threshold() {
        let (dir, rels) = fixture(&[("config.yaml", "retries: 10\ntimeout: 5\n")]);
        let b = LayerBoundary::new("a", "b");
        let ctx = SeamContext {
            boundary: &b,
            filtered_diff: "",
            repo_root: dir.path(),
            boundary_files: &rels,
            args: None,
        };
        let result = CascadeTimeoutCheck.run(&ctx);
        assert!(result.is_warning(), "expected Warning, got {result:?}");
    }

    #[test]
    fn always_applies() {
        assert!(CascadeTimeoutCheck.applies_to(&LayerBoundary::new("x", "y")));
    }

    #[test]
    fn never_returns_failed_v0_2_heuristic() {
        // Sanity: even an egregious config only warns.
        let (dir, rels) = fixture(&[("config.yaml", "retries: 100\ntimeout: 100\n")]);
        let b = LayerBoundary::new("a", "b");
        let ctx = SeamContext {
            boundary: &b,
            filtered_diff: "",
            repo_root: dir.path(),
            boundary_files: &rels,
            args: None,
        };
        assert!(!CascadeTimeoutCheck.run(&ctx).is_failed());
    }
}

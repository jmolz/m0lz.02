//! Category 6 — Retry storm / timeout conflicts.
//!
//! **v0.2 static heuristic**: flags retry counts above a threshold without
//! considering downstream capacity. Full retry-storm semantics are runtime
//! (v0.4 implicit contract inference). Always emits `Warning`, never `Failed`.

use crate::seam::types::{LayerBoundary, SeamCheck, SeamContext, SeamFinding, SeamResult};

const MAX_SAFE_RETRIES: u32 = 5;

pub struct RetryStormCheck;

impl SeamCheck for RetryStormCheck {
    fn id(&self) -> &str {
        "retry_storm"
    }
    fn category(&self) -> u8 {
        6
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
            for (key, value) in scan_numeric(&content, &["retries", "max_attempts", "retry_count"]) {
                if value > MAX_SAFE_RETRIES {
                    findings.push(
                        SeamFinding::new(format!(
                            "{key} = {value} exceeds safe threshold ({MAX_SAFE_RETRIES}) — \
                             check downstream capacity to avoid retry storm"
                        ))
                        .with_file(rel.clone()),
                    );
                }
            }
        }
        if findings.is_empty() {
            SeamResult::Passed
        } else {
            SeamResult::Warning(findings)
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
    fn passes_under_threshold() {
        let (dir, rels) = fixture(&[("config.yaml", "retries: 3\n")]);
        let b = LayerBoundary::new("a", "b");
        let ctx = SeamContext {
            boundary: &b,
            filtered_diff: "",
            repo_root: dir.path(),
            boundary_files: &rels,
            args: None,
        };
        assert_eq!(RetryStormCheck.run(&ctx), SeamResult::Passed);
    }

    #[test]
    fn warns_over_threshold() {
        let (dir, rels) = fixture(&[("config.yaml", "retries: 20\n")]);
        let b = LayerBoundary::new("a", "b");
        let ctx = SeamContext {
            boundary: &b,
            filtered_diff: "",
            repo_root: dir.path(),
            boundary_files: &rels,
            args: None,
        };
        assert!(RetryStormCheck.run(&ctx).is_warning());
    }

    #[test]
    fn always_applies() {
        assert!(RetryStormCheck.applies_to(&LayerBoundary::new("x", "y")));
    }
}

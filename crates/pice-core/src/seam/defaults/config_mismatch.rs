//! Category 1 — Configuration/deployment mismatches (Google SRE: 31% of triggers).
//!
//! Detects env vars declared on one side of the boundary but not consumed on
//! the other. Runs at any boundary that touches `infrastructure` or
//! `deployment`.
//!
//! Deterministic, <100ms, no I/O beyond `boundary_files`.

use super::env_scan::{is_infra_manifest, parse_consumed, parse_declared};
use crate::seam::types::{LayerBoundary, SeamCheck, SeamContext, SeamFinding, SeamResult};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub struct ConfigMismatchCheck;

impl SeamCheck for ConfigMismatchCheck {
    fn id(&self) -> &str {
        "config_mismatch"
    }
    fn category(&self) -> u8 {
        1
    }
    fn applies_to(&self, boundary: &LayerBoundary) -> bool {
        boundary.touches("infrastructure") || boundary.touches("deployment")
    }
    fn run(&self, ctx: &SeamContext<'_>) -> SeamResult {
        let mut declared: std::collections::BTreeSet<String> = Default::default();
        let mut consumed: std::collections::BTreeSet<String> = Default::default();
        let mut declared_file: BTreeMap<String, PathBuf> = Default::default();
        let mut consumed_file: BTreeMap<String, PathBuf> = Default::default();

        for rel in ctx.boundary_files {
            let full = ctx.repo_root.join(rel);
            let Ok(content) = std::fs::read_to_string(&full) else {
                continue;
            };
            if is_infra_manifest(rel) {
                for name in parse_declared(&content) {
                    declared_file.entry(name.clone()).or_insert(rel.clone());
                    declared.insert(name);
                }
            } else {
                for name in parse_consumed(&content) {
                    consumed_file.entry(name.clone()).or_insert(rel.clone());
                    consumed.insert(name);
                }
            }
        }

        let mut findings: Vec<SeamFinding> = Vec::new();
        for name in declared.difference(&consumed) {
            let mut f = SeamFinding::new(format!(
                "env var '{name}' declared in infrastructure but not consumed by the app"
            ));
            if let Some(p) = declared_file.get(name).cloned() {
                f = f.with_file(p);
            }
            findings.push(f);
        }
        for name in consumed.difference(&declared) {
            let mut f = SeamFinding::new(format!(
                "env var '{name}' read by app but not declared in infrastructure"
            ));
            if let Some(p) = consumed_file.get(name).cloned() {
                f = f.with_file(p);
            }
            findings.push(f);
        }

        if findings.is_empty() {
            SeamResult::Passed
        } else {
            SeamResult::Failed(findings)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn ctx<'a>(
        dir: &'a tempfile::TempDir,
        boundary: &'a LayerBoundary,
        rels: &'a [PathBuf],
    ) -> SeamContext<'a> {
        SeamContext {
            boundary,
            filtered_diff: "",
            repo_root: dir.path(),
            boundary_files: rels,
            args: None,
        }
    }

    #[test]
    fn passes_when_env_declared_and_consumed() {
        let (dir, rels) = fixture(&[
            ("Dockerfile", "FROM alpine\nENV DATABASE_URL=postgres://x\n"),
            ("src/main.rs", r#"fn main() { env::var("DATABASE_URL"); }"#),
        ]);
        let b = LayerBoundary::new("backend", "infrastructure");
        assert_eq!(ConfigMismatchCheck.run(&ctx(&dir, &b, &rels)), SeamResult::Passed);
    }

    #[test]
    fn fails_when_env_declared_but_not_consumed() {
        let (dir, rels) = fixture(&[
            ("Dockerfile", "FROM alpine\nENV FOO=1\n"),
            ("src/main.rs", "fn main() {}\n"),
        ]);
        let b = LayerBoundary::new("backend", "infrastructure");
        let result = ConfigMismatchCheck.run(&ctx(&dir, &b, &rels));
        assert!(result.is_failed(), "expected Failed, got {result:?}");
        assert!(result.findings()[0].message.contains("FOO"));
    }

    #[test]
    fn fails_when_env_consumed_but_not_declared() {
        let (dir, rels) = fixture(&[
            ("Dockerfile", "FROM alpine\n"),
            ("src/main.rs", r#"fn main() { env::var("BAR").unwrap(); }"#),
        ]);
        let b = LayerBoundary::new("backend", "infrastructure");
        let result = ConfigMismatchCheck.run(&ctx(&dir, &b, &rels));
        assert!(result.is_failed());
        assert!(result.findings()[0].message.contains("BAR"));
    }

    #[test]
    fn out_of_scope_when_neither_side_is_infra_or_deploy() {
        assert!(!ConfigMismatchCheck.applies_to(&LayerBoundary::new("api", "frontend")));
    }

    #[test]
    fn applies_to_infrastructure_and_deployment() {
        assert!(ConfigMismatchCheck.applies_to(&LayerBoundary::new("backend", "infrastructure")));
        assert!(ConfigMismatchCheck.applies_to(&LayerBoundary::new("deployment", "backend")));
    }

    #[test]
    fn compose_environment_block_recognized() {
        let (dir, rels) = fixture(&[
            (
                "docker-compose.yml",
                "services:\n  app:\n    environment:\n      - MY_VAR=x\n",
            ),
            ("src/main.rs", "fn main() {}\n"),
        ]);
        let b = LayerBoundary::new("backend", "infrastructure");
        let result = ConfigMismatchCheck.run(&ctx(&dir, &b, &rels));
        assert!(result.is_failed());
        assert!(result.findings()[0].message.contains("MY_VAR"));
    }
}

//! Category 8 — Health check blind spots.
//!
//! Detects health endpoints that only report "process is alive" without
//! checking upstream dependencies. Warns when a `/health*` handler is found
//! but contains no references to database / queue / cache probes.

use crate::seam::types::{LayerBoundary, SeamCheck, SeamContext, SeamFinding, SeamResult};

pub struct HealthCheckCheck;

impl SeamCheck for HealthCheckCheck {
    fn id(&self) -> &str {
        "health_check"
    }
    fn category(&self) -> u8 {
        8
    }
    fn applies_to(&self, boundary: &LayerBoundary) -> bool {
        boundary.touches("api") || boundary.touches("backend") || boundary.touches("observability")
    }
    fn run(&self, ctx: &SeamContext<'_>) -> SeamResult {
        let mut findings: Vec<SeamFinding> = Vec::new();
        for rel in ctx.boundary_files {
            let full = ctx.repo_root.join(rel);
            let Ok(content) = std::fs::read_to_string(&full) else {
                continue;
            };
            // Look for anything that looks like a health endpoint definition.
            let declares_health = content.contains("/health")
                || content.contains("/healthz")
                || content.contains("/ready")
                || content.contains("/livez");
            if !declares_health {
                continue;
            }
            let references_upstream = content.contains("db.")
                || content.contains("database")
                || content.contains("redis")
                || content.contains("queue")
                || content.contains("sql")
                || content.contains("ping")
                || content.contains("probe");
            if !references_upstream {
                findings.push(
                    SeamFinding::new(format!(
                        "health endpoint declared in {} but does not probe any upstream dependency",
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
    fn passes_when_health_probes_upstream() {
        let (dir, rels) = fixture(&[(
            "src/health.rs",
            "fn health() { /* ping db.check(); */ }\n// route: /healthz\n",
        )]);
        let b = LayerBoundary::new("api", "backend");
        let ctx = SeamContext {
            boundary: &b,
            filtered_diff: "",
            repo_root: dir.path(),
            boundary_files: &rels,
            args: None,
        };
        assert_eq!(HealthCheckCheck.run(&ctx), SeamResult::Passed);
    }

    #[test]
    fn warns_when_health_endpoint_lacks_probes() {
        let (dir, rels) = fixture(&[(
            "src/health.rs",
            "fn health() { return 200; }\n// route: /healthz\n",
        )]);
        let b = LayerBoundary::new("api", "backend");
        let ctx = SeamContext {
            boundary: &b,
            filtered_diff: "",
            repo_root: dir.path(),
            boundary_files: &rels,
            args: None,
        };
        assert!(HealthCheckCheck.run(&ctx).is_warning());
    }

    #[test]
    fn out_of_scope_on_infra_deploy_boundary() {
        assert!(!HealthCheckCheck.applies_to(&LayerBoundary::new("deployment", "infrastructure")));
    }
}

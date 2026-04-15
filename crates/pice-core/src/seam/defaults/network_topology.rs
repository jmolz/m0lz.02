//! Category 11 — Network topology assumptions.
//!
//! **v0.2 static heuristic**: flags hardcoded AZ/region strings in app code
//! (e.g., `"us-east-1"`) that should come from config. Runtime-only semantics
//! (true topology awareness) are deferred to v0.4. Always emits `Warning`.

use crate::seam::types::{LayerBoundary, SeamCheck, SeamContext, SeamFinding, SeamResult};

const HARDCODED_REGIONS: &[&str] = &[
    "us-east-1",
    "us-east-2",
    "us-west-1",
    "us-west-2",
    "eu-west-1",
    "eu-west-2",
    "eu-central-1",
    "ap-south-1",
    "ap-southeast-1",
    "ap-northeast-1",
];

pub struct NetworkTopologyCheck;

impl SeamCheck for NetworkTopologyCheck {
    fn id(&self) -> &str {
        "network_topology"
    }
    fn category(&self) -> u8 {
        11
    }
    fn applies_to(&self, boundary: &LayerBoundary) -> bool {
        boundary.touches("backend") || boundary.touches("api") || boundary.touches("deployment")
    }
    fn run(&self, ctx: &SeamContext<'_>) -> SeamResult {
        let mut findings: Vec<SeamFinding> = Vec::new();
        for rel in ctx.boundary_files {
            let full = ctx.repo_root.join(rel);
            let Ok(content) = std::fs::read_to_string(&full) else {
                continue;
            };
            for region in HARDCODED_REGIONS {
                if content.contains(region) {
                    findings.push(
                        SeamFinding::new(format!(
                            "hardcoded region '{region}' found in {} — move to config/env",
                            rel.display()
                        ))
                        .with_file(rel.clone()),
                    );
                    break; // one warning per file is plenty
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
    fn passes_when_region_from_env() {
        let (dir, rels) = fixture(&[(
            "src/aws.rs",
            r#"let region = env::var("AWS_REGION").unwrap();"#,
        )]);
        let b = LayerBoundary::new("api", "backend");
        let ctx = SeamContext {
            boundary: &b,
            filtered_diff: "",
            repo_root: dir.path(),
            boundary_files: &rels,
            args: None,
        };
        assert_eq!(NetworkTopologyCheck.run(&ctx), SeamResult::Passed);
    }

    #[test]
    fn warns_on_hardcoded_region() {
        let (dir, rels) = fixture(&[(
            "src/aws.rs",
            r#"let region = "us-east-1";"#,
        )]);
        let b = LayerBoundary::new("api", "backend");
        let ctx = SeamContext {
            boundary: &b,
            filtered_diff: "",
            repo_root: dir.path(),
            boundary_files: &rels,
            args: None,
        };
        assert!(NetworkTopologyCheck.run(&ctx).is_warning());
    }

    #[test]
    fn out_of_scope_on_database_infra_only() {
        assert!(!NetworkTopologyCheck.applies_to(&LayerBoundary::new("database", "infrastructure")));
    }
}

//! Category 10 — Cold start / ordering dependencies.
//!
//! **v0.2 static heuristic**: scans docker-compose for services that
//! reference a database or cache URL but lack a corresponding `depends_on`
//! clause. Full cold-start semantics require runtime trace analysis (v0.4).
//! Always emits `Warning`, never `Failed`.

use crate::seam::types::{LayerBoundary, SeamCheck, SeamContext, SeamFinding, SeamResult};

pub struct ColdStartOrderCheck;

impl SeamCheck for ColdStartOrderCheck {
    fn id(&self) -> &str {
        "cold_start_order"
    }
    fn category(&self) -> u8 {
        10
    }
    fn applies_to(&self, boundary: &LayerBoundary) -> bool {
        boundary.touches("infrastructure") || boundary.touches("deployment")
    }
    fn run(&self, ctx: &SeamContext<'_>) -> SeamResult {
        let mut findings: Vec<SeamFinding> = Vec::new();
        for rel in ctx.boundary_files {
            let full = ctx.repo_root.join(rel);
            let Ok(content) = std::fs::read_to_string(&full) else {
                continue;
            };
            let fname = rel
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !(fname == "docker-compose.yml" || fname == "docker-compose.yaml") {
                continue;
            }
            // Brute-force: if the file mentions DATABASE_URL / REDIS_URL but has
            // no `depends_on:` declaration anywhere, flag it.
            let mentions_upstream =
                content.contains("DATABASE_URL") || content.contains("REDIS_URL");
            let has_depends_on = content.contains("depends_on:");
            if mentions_upstream && !has_depends_on {
                findings.push(
                    SeamFinding::new(format!(
                        "{} references upstream services but has no depends_on — \
                         cold-start ordering may race on boot",
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
    fn passes_when_depends_on_declared() {
        let (dir, rels) = fixture(&[(
            "docker-compose.yml",
            "services:\n  app:\n    environment:\n      - DATABASE_URL=postgres://db\n    depends_on:\n      - db\n  db:\n    image: postgres\n",
        )]);
        let b = LayerBoundary::new("backend", "infrastructure");
        let ctx = SeamContext {
            boundary: &b,
            filtered_diff: "",
            repo_root: dir.path(),
            boundary_files: &rels,
            args: None,
        };
        assert_eq!(ColdStartOrderCheck.run(&ctx), SeamResult::Passed);
    }

    #[test]
    fn warns_when_depends_on_missing() {
        let (dir, rels) = fixture(&[(
            "docker-compose.yml",
            "services:\n  app:\n    environment:\n      - DATABASE_URL=postgres://db\n  db:\n    image: postgres\n",
        )]);
        let b = LayerBoundary::new("backend", "infrastructure");
        let ctx = SeamContext {
            boundary: &b,
            filtered_diff: "",
            repo_root: dir.path(),
            boundary_files: &rels,
            args: None,
        };
        assert!(ColdStartOrderCheck.run(&ctx).is_warning());
    }

    #[test]
    fn out_of_scope_on_pure_app_boundary() {
        assert!(!ColdStartOrderCheck.applies_to(&LayerBoundary::new("api", "frontend")));
    }
}

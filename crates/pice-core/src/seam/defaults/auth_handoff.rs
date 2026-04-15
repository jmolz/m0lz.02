//! Category 4 — Authentication handoff failures.
//!
//! Verifies that auth-shaped env vars (anything starting with `JWT_`, `AUTH_`,
//! `SESSION_`, `OAUTH_`, or named `*_SECRET` / `*_TOKEN`) declared on one side
//! of the boundary are consumed on the other.

use crate::seam::types::{LayerBoundary, SeamCheck, SeamContext, SeamFinding, SeamResult};
use std::collections::BTreeSet;

pub struct AuthHandoffCheck;

impl SeamCheck for AuthHandoffCheck {
    fn id(&self) -> &str {
        "auth_handoff"
    }
    fn category(&self) -> u8 {
        4
    }
    fn applies_to(&self, boundary: &LayerBoundary) -> bool {
        boundary.touches("infrastructure")
            || boundary.touches("deployment")
            || boundary.touches("backend")
            || boundary.touches("api")
    }
    fn run(&self, ctx: &SeamContext<'_>) -> SeamResult {
        let mut declared: BTreeSet<String> = Default::default();
        let mut consumed: BTreeSet<String> = Default::default();

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
            let is_infra = fname == "dockerfile"
                || fname.starts_with("dockerfile.")
                || fname == "docker-compose.yml"
                || fname == "docker-compose.yaml"
                || fname.starts_with(".env");
            if is_infra {
                for name in super::env_scan::parse_declared(&content) {
                    if is_auth(&name) {
                        declared.insert(name);
                    }
                }
            } else {
                for name in super::env_scan::parse_consumed(&content) {
                    if is_auth(&name) {
                        consumed.insert(name);
                    }
                }
            }
        }

        let mut findings: Vec<SeamFinding> = Vec::new();
        for name in declared.difference(&consumed) {
            findings.push(SeamFinding::new(format!(
                "auth env '{name}' declared in infra but not consumed by app"
            )));
        }
        for name in consumed.difference(&declared) {
            findings.push(SeamFinding::new(format!(
                "auth env '{name}' consumed by app but not declared in infra"
            )));
        }
        if findings.is_empty() {
            SeamResult::Passed
        } else {
            SeamResult::Failed(findings)
        }
    }
}

fn is_auth(name: &str) -> bool {
    name.starts_with("JWT_")
        || name.starts_with("AUTH_")
        || name.starts_with("SESSION_")
        || name.starts_with("OAUTH_")
        || name.ends_with("_SECRET")
        || name.ends_with("_TOKEN")
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
    fn passes_when_auth_env_present_on_both_sides() {
        let (dir, rels) = fixture(&[
            ("Dockerfile", "ENV JWT_SECRET=xyz\n"),
            ("src/main.rs", r#"fn main() { env::var("JWT_SECRET"); }"#),
        ]);
        let b = LayerBoundary::new("backend", "infrastructure");
        let ctx = SeamContext {
            boundary: &b,
            filtered_diff: "",
            repo_root: dir.path(),
            boundary_files: &rels,
            args: None,
        };
        assert_eq!(AuthHandoffCheck.run(&ctx), SeamResult::Passed);
    }

    #[test]
    fn fails_when_auth_env_missing_from_app() {
        let (dir, rels) = fixture(&[
            ("Dockerfile", "ENV JWT_SECRET=xyz\n"),
            ("src/main.rs", "fn main() {}\n"),
        ]);
        let b = LayerBoundary::new("backend", "infrastructure");
        let ctx = SeamContext {
            boundary: &b,
            filtered_diff: "",
            repo_root: dir.path(),
            boundary_files: &rels,
            args: None,
        };
        let result = AuthHandoffCheck.run(&ctx);
        assert!(result.is_failed());
        assert!(result.findings()[0].message.contains("JWT_SECRET"));
    }

    #[test]
    fn out_of_scope_when_boundary_is_pure_frontend_data() {
        let b = LayerBoundary::new("database", "frontend");
        assert!(!AuthHandoffCheck.applies_to(&b));
    }

    #[test]
    fn is_auth_matches_expected_prefixes() {
        assert!(is_auth("JWT_SECRET"));
        assert!(is_auth("AUTH_URL"));
        assert!(is_auth("SESSION_KEY"));
        assert!(is_auth("OAUTH_CLIENT_ID"));
        assert!(is_auth("DB_TOKEN"));
        assert!(is_auth("COOKIE_SECRET"));
        assert!(!is_auth("DATABASE_URL"));
    }
}

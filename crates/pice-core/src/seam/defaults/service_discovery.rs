//! Category 7 — Service discovery failures.
//!
//! Cross-references docker-compose service names against app connection
//! strings that reference host names. Reports services referenced by the app
//! that don't exist in the compose file.

use crate::seam::types::{LayerBoundary, SeamCheck, SeamContext, SeamFinding, SeamResult};
use std::collections::BTreeSet;

pub struct ServiceDiscoveryCheck;

impl SeamCheck for ServiceDiscoveryCheck {
    fn id(&self) -> &str {
        "service_discovery"
    }
    fn category(&self) -> u8 {
        7
    }
    fn applies_to(&self, boundary: &LayerBoundary) -> bool {
        boundary.touches("infrastructure")
            || boundary.touches("deployment")
            || boundary.touches("backend")
    }
    fn run(&self, ctx: &SeamContext<'_>) -> SeamResult {
        let mut declared_services: BTreeSet<String> = Default::default();
        let mut referenced_hosts: BTreeSet<String> = Default::default();

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
            if fname == "docker-compose.yml" || fname == "docker-compose.yaml" {
                declared_services.extend(parse_compose_services(&content));
            } else {
                referenced_hosts.extend(parse_referenced_hosts(&content));
            }
        }

        // Nothing to compare against.
        if declared_services.is_empty() || referenced_hosts.is_empty() {
            return SeamResult::Passed;
        }

        let mut findings: Vec<SeamFinding> = Vec::new();
        for host in referenced_hosts.difference(&declared_services) {
            findings.push(SeamFinding::new(format!(
                "app references host '{host}' which is not a declared docker-compose service"
            )));
        }
        if findings.is_empty() {
            SeamResult::Passed
        } else {
            SeamResult::Failed(findings)
        }
    }
}

/// Capture immediate children of the top-level `services:` key.
fn parse_compose_services(content: &str) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = Default::default();
    let mut in_services = false;
    let mut services_indent: usize = 0;
    for raw in content.lines() {
        if raw.trim().is_empty() || raw.trim_start().starts_with('#') {
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        let line = raw.trim();
        if line.starts_with("services:") {
            in_services = true;
            services_indent = indent;
            continue;
        }
        if in_services && indent <= services_indent {
            in_services = false;
        }
        if in_services && indent == services_indent + 2 {
            if let Some(cut) = line.find(':') {
                let name = line[..cut].trim().to_string();
                if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
                    out.insert(name);
                }
            }
        }
    }
    out
}

/// Capture hostnames from connection strings like `postgres://db/...`,
/// `redis://redis:6379`, `http://api:3000`, etc.
fn parse_referenced_hosts(content: &str) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = Default::default();
    for scheme in ["postgres://", "postgresql://", "mysql://", "redis://", "http://", "https://"] {
        let mut rest = content;
        while let Some(idx) = rest.find(scheme) {
            let after = &rest[idx + scheme.len()..];
            // Strip optional user:pass@ prefix.
            let host_start = after.find('@').map(|i| i + 1).unwrap_or(0);
            let host_slice = &after[host_start..];
            let host_end = host_slice
                .find(|c: char| c == '/' || c == ':' || c == '?' || c == '"' || c == '\'' || c.is_whitespace())
                .unwrap_or(host_slice.len());
            let host = &host_slice[..host_end];
            // Skip IP addresses and localhost.
            if !host.is_empty()
                && host != "localhost"
                && !host.chars().next().is_some_and(|c| c.is_ascii_digit())
                && host.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
                && !host.contains('.')
            {
                out.insert(host.to_string());
            }
            rest = &after[host_start + host_end..];
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
    fn passes_when_referenced_host_exists_in_compose() {
        let (dir, rels) = fixture(&[
            (
                "docker-compose.yml",
                "services:\n  db:\n    image: postgres\n  app:\n    image: myapp\n",
            ),
            ("src/main.rs", r#"const URL: &str = "postgres://db/mydb";"#),
        ]);
        let b = LayerBoundary::new("backend", "infrastructure");
        let ctx = SeamContext {
            boundary: &b,
            filtered_diff: "",
            repo_root: dir.path(),
            boundary_files: &rels,
            args: None,
        };
        assert_eq!(ServiceDiscoveryCheck.run(&ctx), SeamResult::Passed);
    }

    #[test]
    fn fails_when_host_not_in_compose() {
        let (dir, rels) = fixture(&[
            (
                "docker-compose.yml",
                "services:\n  db:\n    image: postgres\n",
            ),
            ("src/main.rs", r#"const URL: &str = "postgres://cache";"#),
        ]);
        let b = LayerBoundary::new("backend", "infrastructure");
        let ctx = SeamContext {
            boundary: &b,
            filtered_diff: "",
            repo_root: dir.path(),
            boundary_files: &rels,
            args: None,
        };
        let result = ServiceDiscoveryCheck.run(&ctx);
        assert!(result.is_failed());
        assert!(result.findings()[0].message.contains("cache"));
    }

    #[test]
    fn out_of_scope_when_boundary_excludes_infra_deploy_backend() {
        assert!(!ServiceDiscoveryCheck.applies_to(&LayerBoundary::new("api", "frontend")));
    }
}

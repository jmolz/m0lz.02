//! Category 2 — Binary / version incompatibilities.
//!
//! Compares declared versions across manifest files at the boundary. A single
//! package declared with conflicting versions across two sides of the
//! boundary is a finding.

use crate::seam::types::{LayerBoundary, SeamCheck, SeamContext, SeamFinding, SeamResult};
use std::collections::BTreeMap;

pub struct VersionSkewCheck;

impl SeamCheck for VersionSkewCheck {
    fn id(&self) -> &str {
        "version_skew"
    }
    fn category(&self) -> u8 {
        2
    }
    fn applies_to(&self, _boundary: &LayerBoundary) -> bool {
        true
    }
    fn run(&self, ctx: &SeamContext<'_>) -> SeamResult {
        // Map: package name → observed versions across files.
        let mut versions: BTreeMap<String, BTreeMap<String, Vec<std::path::PathBuf>>> =
            BTreeMap::new();

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
            let pairs: Vec<(String, String)> = if fname == "cargo.toml" {
                extract_cargo_deps(&content)
            } else if fname == "package.json" {
                extract_npm_deps(&content)
            } else {
                continue;
            };
            for (name, version) in pairs {
                versions
                    .entry(name)
                    .or_default()
                    .entry(version)
                    .or_default()
                    .push(rel.clone());
            }
        }

        let mut findings: Vec<SeamFinding> = Vec::new();
        for (name, by_version) in &versions {
            if by_version.len() > 1 {
                let list = by_version
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                findings.push(SeamFinding::new(format!(
                    "package '{name}' has conflicting versions across boundary: {list}"
                )));
            }
        }

        if findings.is_empty() {
            SeamResult::Passed
        } else {
            SeamResult::Failed(findings)
        }
    }
}

fn extract_cargo_deps(content: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    // Match lines like `foo = "1.2.3"` under a `[dependencies]`-ish section.
    let mut in_deps = false;
    for raw in content.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            in_deps = line.contains("dependencies") || line.contains("dev-dependencies");
            continue;
        }
        if !in_deps || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(eq) = line.find('=') {
            let name = line[..eq].trim().to_string();
            let value = line[eq + 1..].trim();
            if value.starts_with('"') {
                let end = value[1..].find('"').unwrap_or(0);
                let version = value[1..1 + end].to_string();
                if !name.is_empty() && !version.is_empty() {
                    out.push((name, version));
                }
            }
        }
    }
    out
}

fn extract_npm_deps(content: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    // Primitive JSON scan — look for "dependencies": { ... } and
    // "devDependencies": { ... } blocks, then extract `"name": "version"`.
    for block_key in ["\"dependencies\"", "\"devDependencies\""] {
        if let Some(start) = content.find(block_key) {
            if let Some(open) = content[start..].find('{') {
                let abs_open = start + open;
                if let Some(close_rel) = find_matching(&content[abs_open + 1..], '{', '}') {
                    let block = &content[abs_open + 1..abs_open + 1 + close_rel];
                    out.extend(extract_json_kv(block));
                }
            }
        }
    }
    out
}

fn extract_json_kv(block: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = block;
    while let Some(start) = rest.find('"') {
        let after = &rest[start + 1..];
        let Some(key_end) = after.find('"') else { break };
        let key = after[..key_end].to_string();
        let after_key = &after[key_end + 1..];
        let Some(colon) = after_key.find(':') else { break };
        let tail = &after_key[colon + 1..];
        let Some(val_start) = tail.find('"') else { break };
        let after_val = &tail[val_start + 1..];
        let Some(val_end) = after_val.find('"') else { break };
        let value = after_val[..val_end].to_string();
        out.push((key, value));
        rest = &after_val[val_end + 1..];
    }
    out
}

fn find_matching(s: &str, open: char, close: char) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 1i32;
    for (i, &b) in bytes.iter().enumerate() {
        if b == open as u8 {
            depth += 1;
        } else if b == close as u8 {
            depth -= 1;
            if depth == 0 {
                return Some(i);
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
    fn passes_when_versions_align() {
        let (dir, rels) = fixture(&[
            (
                "crates/a/Cargo.toml",
                "[dependencies]\nserde = \"1.0\"\n",
            ),
            (
                "crates/b/Cargo.toml",
                "[dependencies]\nserde = \"1.0\"\n",
            ),
        ]);
        let b = LayerBoundary::new("api", "backend");
        let ctx = SeamContext {
            boundary: &b,
            filtered_diff: "",
            repo_root: dir.path(),
            boundary_files: &rels,
            args: None,
        };
        assert_eq!(VersionSkewCheck.run(&ctx), SeamResult::Passed);
    }

    #[test]
    fn fails_when_versions_diverge() {
        let (dir, rels) = fixture(&[
            (
                "crates/a/Cargo.toml",
                "[dependencies]\nserde = \"1.0\"\n",
            ),
            (
                "crates/b/Cargo.toml",
                "[dependencies]\nserde = \"2.0\"\n",
            ),
        ]);
        let b = LayerBoundary::new("api", "backend");
        let ctx = SeamContext {
            boundary: &b,
            filtered_diff: "",
            repo_root: dir.path(),
            boundary_files: &rels,
            args: None,
        };
        let result = VersionSkewCheck.run(&ctx);
        assert!(result.is_failed());
        assert!(result.findings()[0].message.contains("serde"));
    }

    #[test]
    fn always_applies() {
        // Version skew is boundary-agnostic.
        assert!(VersionSkewCheck.applies_to(&LayerBoundary::new("a", "b")));
    }
}

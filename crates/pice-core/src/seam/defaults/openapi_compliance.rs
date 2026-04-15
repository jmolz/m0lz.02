//! Category 3 — Protocol / API contract violations.
//!
//! Compares declared OpenAPI response shapes against handler return types.
//! Best-effort — uses simple structural matching on `.yaml`/`.yml`/`.json`
//! spec files and regex-style property extraction on TS/Rust/Python handlers.

use crate::seam::types::{LayerBoundary, SeamCheck, SeamContext, SeamFinding, SeamResult};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

pub struct OpenApiComplianceCheck;

impl SeamCheck for OpenApiComplianceCheck {
    fn id(&self) -> &str {
        "openapi_compliance"
    }
    fn category(&self) -> u8 {
        3
    }
    fn applies_to(&self, boundary: &LayerBoundary) -> bool {
        boundary.touches("api") || boundary.touches("frontend") || boundary.touches("backend")
    }
    fn run(&self, ctx: &SeamContext<'_>) -> SeamResult {
        // Collect declared response-property NAMES from openapi files, and
        // property NAMES present in handler return shapes. Mismatches are
        // findings. We deliberately do not match by TYPE — handler source
        // rarely exposes types in a machine-parseable form — but we DO
        // extract the type literal when possible so we can compare.
        let mut spec_props: BTreeMap<String, String> = BTreeMap::new(); // name -> type label
        let mut spec_file: BTreeMap<String, PathBuf> = BTreeMap::new();
        let mut handler_props: BTreeMap<String, String> = BTreeMap::new();
        let mut handler_file: BTreeMap<String, PathBuf> = BTreeMap::new();

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
            let is_spec = fname.starts_with("openapi.")
                || fname.starts_with("swagger.")
                || rel
                    .components()
                    .any(|c| c.as_os_str().eq_ignore_ascii_case("openapi"));
            if is_spec {
                for (name, ty) in parse_openapi_properties(&content) {
                    spec_props.entry(name.clone()).or_insert(ty);
                    spec_file.entry(name).or_insert(rel.clone());
                }
            } else {
                for (name, ty) in parse_handler_returns(&content) {
                    handler_props.entry(name.clone()).or_insert(ty);
                    handler_file.entry(name).or_insert(rel.clone());
                }
            }
        }

        let mut findings: Vec<SeamFinding> = Vec::new();
        if spec_props.is_empty() || handler_props.is_empty() {
            // Nothing to compare — no finding.
            return SeamResult::Passed;
        }

        let spec_keys: BTreeSet<&String> = spec_props.keys().collect();
        let handler_keys: BTreeSet<&String> = handler_props.keys().collect();

        // Fields in spec but not in handler → likely missing in response.
        for name in spec_keys.difference(&handler_keys) {
            let mut f = SeamFinding::new(format!(
                "OpenAPI spec declares response field '{name}' but no handler returns it"
            ));
            if let Some(p) = spec_file.get(*name).cloned() {
                f = f.with_file(p);
            }
            findings.push(f);
        }
        // Fields in handler but not in spec → undocumented response.
        for name in handler_keys.difference(&spec_keys) {
            let mut f = SeamFinding::new(format!(
                "handler returns field '{name}' but OpenAPI spec does not declare it"
            ));
            if let Some(p) = handler_file.get(*name).cloned() {
                f = f.with_file(p);
            }
            findings.push(f);
        }
        // Type mismatches for shared fields — only when both sides declared a type.
        for name in spec_keys.intersection(&handler_keys) {
            let spec_ty = spec_props.get(*name).map(|s| s.to_lowercase()).unwrap_or_default();
            let handler_ty = handler_props
                .get(*name)
                .map(|s| s.to_lowercase())
                .unwrap_or_default();
            if !spec_ty.is_empty() && !handler_ty.is_empty() && !types_compatible(&spec_ty, &handler_ty) {
                let mut f = SeamFinding::new(format!(
                    "field '{name}' type mismatch: OpenAPI spec says '{spec_ty}', handler returns '{handler_ty}'"
                ));
                if let Some(p) = handler_file.get(*name).cloned() {
                    f = f.with_file(p);
                }
                findings.push(f);
            }
        }

        if findings.is_empty() {
            SeamResult::Passed
        } else {
            SeamResult::Failed(findings)
        }
    }
}

/// Parse OpenAPI YAML/JSON for response property names and types. Skips over
/// `#/components/schemas` section-level keys and only captures fields under
/// a `properties:` or `"properties":` block.
fn parse_openapi_properties(content: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut in_properties = false;
    let mut props_indent: usize = 0;
    let mut last_name: Option<String> = None;
    let mut name_indent: usize = 0;

    for raw in content.lines() {
        if raw.trim().is_empty() {
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        let trimmed = raw.trim();

        if trimmed.starts_with("properties:") || trimmed.starts_with("\"properties\":") {
            in_properties = true;
            props_indent = indent;
            last_name = None;
            continue;
        }

        if in_properties && indent <= props_indent {
            // Exited the properties block.
            in_properties = false;
            last_name = None;
        }

        if !in_properties {
            continue;
        }

        // Immediate children of properties:
        if indent == props_indent + 2 {
            // YAML two-space style, typical.
            if let Some(name) = yaml_key(trimmed) {
                last_name = Some(name);
                name_indent = indent;
                continue;
            }
        } else if indent > props_indent && last_name.is_none() {
            // First indented key — treat it as property name.
            if let Some(name) = yaml_key(trimmed) {
                last_name = Some(name);
                name_indent = indent;
                continue;
            }
        } else if indent > name_indent {
            // Inside a property body; capture its `type:` line.
            if let Some(rest) = trimmed.strip_prefix("type:") {
                if let Some(name) = last_name.clone() {
                    let ty = rest.trim().trim_matches('"').to_string();
                    out.push((name, ty));
                }
            }
        } else if indent <= name_indent {
            // Sibling property — close previous.
            last_name = None;
            if let Some(name) = yaml_key(trimmed) {
                last_name = Some(name);
                name_indent = indent;
            }
        }
    }

    out
}

fn yaml_key(line: &str) -> Option<String> {
    // `foo:` or `"foo":` — stop at first `:`.
    let cut = line.find(':')?;
    let key = line[..cut].trim().trim_matches('"').trim_matches('\'');
    if key.is_empty() || key.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    Some(key.to_string())
}

/// Parse handler source files for `return { field: value }` style shapes.
/// Covers TS/JS object literals and Rust struct literals in a handler body.
fn parse_handler_returns(content: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    // Object literal — `return { foo: "x", bar: 1 }`.
    if let Some(start) = content.find("return {") {
        if let Some(len) = find_matching(&content[start + 7..], '{', '}') {
            let inner = &content[start + 8..start + 7 + len];
            for part in inner.split(',') {
                if let Some(colon) = part.find(':') {
                    let name = part[..colon]
                        .trim()
                        .trim_matches('"')
                        .trim_matches('\'')
                        .to_string();
                    let val = part[colon + 1..].trim();
                    let ty = infer_literal_type(val);
                    if !name.is_empty() && is_ident(&name) {
                        out.push((name, ty));
                    }
                }
            }
        }
    }
    // JSON-like response: `res.json({ ... })`, `c.json({ ... })`.
    for marker in [".json({", ".json({ "].iter() {
        let mut rest = content;
        while let Some(idx) = rest.find(marker) {
            let after = &rest[idx + marker.len()..];
            if let Some(close) = after.find('}') {
                let body = &after[..close];
                for part in body.split(',') {
                    if let Some(colon) = part.find(':') {
                        let name = part[..colon]
                            .trim()
                            .trim_matches('"')
                            .trim_matches('\'')
                            .to_string();
                        let val = part[colon + 1..].trim();
                        let ty = infer_literal_type(val);
                        if !name.is_empty() && is_ident(&name) {
                            out.push((name, ty));
                        }
                    }
                }
                rest = &after[close + 1..];
            } else {
                break;
            }
        }
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

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

fn infer_literal_type(val: &str) -> String {
    let v = val.trim().trim_end_matches(',').trim_end_matches('}');
    if v.starts_with('"') || v.starts_with('\'') {
        return "string".into();
    }
    if v.starts_with('[') {
        return "array".into();
    }
    if v.starts_with('{') {
        return "object".into();
    }
    if v == "true" || v == "false" {
        return "boolean".into();
    }
    if v.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '-') {
        return if v.contains('.') { "number".into() } else { "integer".into() };
    }
    String::new()
}

fn types_compatible(spec: &str, handler: &str) -> bool {
    if spec == handler {
        return true;
    }
    matches!(
        (spec, handler),
        ("number" | "integer", "number" | "integer")
    )
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
    fn passes_when_handler_matches_spec() {
        let (dir, rels) = fixture(&[
            (
                "openapi.yaml",
                "paths:\n  /x:\n    get:\n      responses:\n        '200':\n          content:\n            application/json:\n              schema:\n                properties:\n                  id:\n                    type: integer\n                  name:\n                    type: string\n",
            ),
            (
                "src/handlers.ts",
                "export function h() { return { id: 1, name: 'a' }; }\n",
            ),
        ]);
        let boundary = LayerBoundary::new("api", "frontend");
        let result = OpenApiComplianceCheck.run(&ctx(&dir, &boundary, &rels));
        assert_eq!(result, SeamResult::Passed);
    }

    #[test]
    fn fails_when_types_diverge() {
        let (dir, rels) = fixture(&[
            (
                "openapi.yaml",
                "paths:\n  /x:\n    get:\n      responses:\n        '200':\n          content:\n            application/json:\n              schema:\n                properties:\n                  id:\n                    type: integer\n",
            ),
            (
                "src/handlers.ts",
                "export function h() { return { id: \"abc\" }; }\n",
            ),
        ]);
        let boundary = LayerBoundary::new("api", "frontend");
        let result = OpenApiComplianceCheck.run(&ctx(&dir, &boundary, &rels));
        assert!(result.is_failed(), "expected type-mismatch finding");
        assert!(result.findings()[0].message.contains("id"));
    }

    #[test]
    fn fails_when_handler_returns_undeclared_field() {
        let (dir, rels) = fixture(&[
            (
                "openapi.yaml",
                "paths:\n  /x:\n    get:\n      responses:\n        '200':\n          content:\n            application/json:\n              schema:\n                properties:\n                  id:\n                    type: integer\n",
            ),
            (
                "src/handlers.ts",
                "export function h() { return { id: 1, email: 'x' }; }\n",
            ),
        ]);
        let boundary = LayerBoundary::new("api", "frontend");
        let result = OpenApiComplianceCheck.run(&ctx(&dir, &boundary, &rels));
        assert!(result.is_failed());
        let msgs: Vec<&str> = result.findings().iter().map(|f| f.message.as_str()).collect();
        assert!(msgs.iter().any(|m| m.contains("email")));
    }

    #[test]
    fn out_of_scope_when_boundary_is_infra_only() {
        let b = LayerBoundary::new("infrastructure", "deployment");
        assert!(!OpenApiComplianceCheck.applies_to(&b));
    }
}

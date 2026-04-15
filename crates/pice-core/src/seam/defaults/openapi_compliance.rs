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
        let mut saw_spec_file = false;
        let mut saw_handler_file = false;
        let mut spec_file_paths: Vec<PathBuf> = Vec::new();
        let mut handler_file_paths: Vec<PathBuf> = Vec::new();

        // Track plausible-but-unrecognized artifacts for the double-miss
        // silent-bypass guard (Phase 3 third-round review fix). These are
        // files that LOOK like they could be spec or handler shapes
        // (`.yaml`/`.yml`/`.json` for specs; recognized source extensions
        // for handlers) but neither narrow heuristic could classify. If
        // EITHER bucket is non-empty when both saw_* flags are false, we
        // refuse to claim "Passed" — we can't compare, but artifacts exist.
        let mut plausible_spec_artifacts: Vec<PathBuf> = Vec::new();
        let mut plausible_handler_artifacts: Vec<PathBuf> = Vec::new();

        for rel in ctx.boundary_files {
            let full = ctx.repo_root.join(rel);
            let Ok(content) = std::fs::read_to_string(&full) else {
                continue;
            };
            if looks_like_spec_file(rel) {
                saw_spec_file = true;
                spec_file_paths.push(rel.clone());
                for (name, ty) in parse_openapi_properties(&content) {
                    spec_props.entry(name.clone()).or_insert(ty);
                    spec_file.entry(name).or_insert(rel.clone());
                }
            } else if looks_like_handler_file(rel, &content) {
                saw_handler_file = true;
                handler_file_paths.push(rel.clone());
                for (name, ty) in parse_handler_returns(&content) {
                    handler_props.entry(name.clone()).or_insert(ty);
                    handler_file.entry(name).or_insert(rel.clone());
                }
            } else {
                // Catch the double-miss case: file is plausibly a spec or
                // handler but didn't match the narrow heuristic.
                if file_looks_plausibly_spec_like(rel, &content) {
                    plausible_spec_artifacts.push(rel.clone());
                } else if file_looks_plausibly_handler_like(rel) {
                    plausible_handler_artifacts.push(rel.clone());
                }
            }
        }

        let mut findings: Vec<SeamFinding> = Vec::new();

        // Asymmetry cases — we could see one side but not the other. Emit
        // Warning so operators investigate rather than silently Passed.
        // (Phase 3 adversarial review: a renamed/moved spec OR a handler the
        // parser could not recognize would otherwise produce a clean boundary.)
        match (saw_spec_file, saw_handler_file) {
            (false, false) => {
                // Double-miss guard (Phase 3 third-round review fix). If
                // there are plausible-but-unclassified artifacts on BOTH
                // sides, refuse to silently Pass — emit a Warning so the
                // operator widens the heuristic or renames their files.
                // Requiring BOTH sides keeps the guard from over-triggering
                // on a single utility source file with no comparable spec.
                if !plausible_spec_artifacts.is_empty() && !plausible_handler_artifacts.is_empty() {
                    let mut summary = String::from(
                        "OpenAPI compliance: boundary contains plausible spec/handler \
                         artifacts that neither heuristic could classify; cannot compare. ",
                    );
                    if !plausible_spec_artifacts.is_empty() {
                        summary.push_str(&format!(
                            "Possible spec files: {}. ",
                            plausible_spec_artifacts
                                .iter()
                                .map(|p| p.display().to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                    if !plausible_handler_artifacts.is_empty() {
                        summary.push_str(&format!(
                            "Possible handler files: {}. ",
                            plausible_handler_artifacts
                                .iter()
                                .map(|p| p.display().to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                    summary.push_str(
                        "Rename spec files to `openapi.*` / `swagger.*` (or place under \
                         an `openapi` path component) and add `return {` / `.json(` \
                         response shapes to handler files so the parser can recognize them.",
                    );
                    let mut f = SeamFinding::new(summary);
                    if let Some(p) = plausible_spec_artifacts
                        .first()
                        .or_else(|| plausible_handler_artifacts.first())
                    {
                        f = f.with_file(p.clone());
                    }
                    return SeamResult::Warning(vec![f]);
                }
                return SeamResult::Passed;
            }
            (true, false) => {
                let mut f = SeamFinding::new(
                    "OpenAPI spec file detected on this boundary but no recognizable \
                     handler response found — boundary could not be compared. If handler \
                     sources live outside the layer globs, widen them; otherwise migrate \
                     the spec-side parser."
                        .to_string(),
                );
                if let Some(p) = spec_file_paths.first() {
                    f = f.with_file(p.clone());
                }
                return SeamResult::Warning(vec![f]);
            }
            (false, true) => {
                let mut f = SeamFinding::new(
                    "Handler response code detected on this boundary but no OpenAPI/Swagger \
                     spec file found — boundary could not be compared. Add the spec to the \
                     boundary layer globs, or rename it to match `openapi.*` / `swagger.*` / \
                     an `openapi` path component."
                        .to_string(),
                );
                if let Some(p) = handler_file_paths.first() {
                    f = f.with_file(p.clone());
                }
                return SeamResult::Warning(vec![f]);
            }
            (true, true) => {}
        }

        // Both sides present but one parsed zero properties — the parser
        // couldn't recognize the shape; report a Warning so it's visible.
        if spec_props.is_empty() {
            let mut f = SeamFinding::new(
                "OpenAPI spec file present but parser extracted zero properties — \
                 boundary could not be compared. Spec may use $ref-only schemas or \
                 a structure this parser does not recognize."
                    .to_string(),
            );
            if let Some(p) = spec_file_paths.first() {
                f = f.with_file(p.clone());
            }
            return SeamResult::Warning(vec![f]);
        }
        if handler_props.is_empty() {
            let mut f = SeamFinding::new(
                "Handler file(s) present but parser extracted zero response properties — \
                 boundary could not be compared. Handler may use a response helper or \
                 shape this parser does not recognize."
                    .to_string(),
            );
            if let Some(p) = handler_file_paths.first() {
                f = f.with_file(p.clone());
            }
            return SeamResult::Warning(vec![f]);
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
            let spec_ty = spec_props
                .get(*name)
                .map(|s| s.to_lowercase())
                .unwrap_or_default();
            let handler_ty = handler_props
                .get(*name)
                .map(|s| s.to_lowercase())
                .unwrap_or_default();
            if !spec_ty.is_empty()
                && !handler_ty.is_empty()
                && !types_compatible(&spec_ty, &handler_ty)
            {
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

/// Spec-file detection: filename-based (`openapi.*`, `swagger.*`) OR path
/// component containing `openapi`. Filename heuristic is narrow by design —
/// handler files rarely match it, and renamed specs fall into the asymmetry
/// warning path rather than silently being treated as handlers.
fn looks_like_spec_file(rel: &std::path::Path) -> bool {
    let fname = rel
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    fname.starts_with("openapi.")
        || fname.starts_with("swagger.")
        || rel
            .components()
            .any(|c| c.as_os_str().eq_ignore_ascii_case("openapi"))
}

/// Handler-file detection: a recognized source extension AND content that
/// contains a shape the parser can evaluate. If neither marker is present,
/// the file is ignored (a stray TOML or README won't count as a "handler
/// file present" on the asymmetry path).
fn looks_like_handler_file(rel: &std::path::Path, content: &str) -> bool {
    let ext = rel
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let is_source = matches!(
        ext.as_str(),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "rs" | "py" | "go" | "java" | "kt"
    );
    is_source && (content.contains("return {") || content.contains(".json("))
}

/// Plausibility check for "this file COULD be a spec but the narrow
/// `looks_like_spec_file` heuristic missed it." Used by the double-miss
/// silent-bypass guard. Catches renamed YAML/JSON specs (`api-spec.yaml`,
/// `schema.json`) by checking the file extension AND content markers
/// `openapi:` / `swagger:` / `"openapi"` / `"swagger"`.
fn file_looks_plausibly_spec_like(rel: &std::path::Path, content: &str) -> bool {
    let ext = rel
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(ext.as_str(), "yaml" | "yml" | "json") {
        return false;
    }
    let lower = content.to_ascii_lowercase();
    lower.contains("openapi:")
        || lower.contains("swagger:")
        || lower.contains("\"openapi\"")
        || lower.contains("\"swagger\"")
        || lower.contains("paths:")
}

/// Plausibility check for "this file COULD be a handler but the narrow
/// `looks_like_handler_file` heuristic missed it." Catches handlers that
/// return via response helpers, typed objects, or framework-specific
/// patterns the literal-string parser doesn't recognize. Conservative —
/// only triggers on recognized source extensions.
fn file_looks_plausibly_handler_like(rel: &std::path::Path) -> bool {
    let ext = rel
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "rs" | "py" | "go" | "java" | "kt"
    )
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
        && s.chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
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
    if v.chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit() || c == '-')
    {
        return if v.contains('.') {
            "number".into()
        } else {
            "integer".into()
        };
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
        let msgs: Vec<&str> = result
            .findings()
            .iter()
            .map(|f| f.message.as_str())
            .collect();
        assert!(msgs.iter().any(|m| m.contains("email")));
    }

    #[test]
    fn out_of_scope_when_boundary_is_infra_only() {
        let b = LayerBoundary::new("infrastructure", "deployment");
        assert!(!OpenApiComplianceCheck.applies_to(&b));
    }

    #[test]
    fn warns_when_handler_present_but_spec_missing() {
        // Handler in the boundary but no OpenAPI/Swagger file — previously
        // silently Passed; now Warning so operators surface the gap.
        let (dir, rels) = fixture(&[(
            "src/handlers.ts",
            "export function h() { return { id: 1, name: 'a' }; }\n",
        )]);
        let boundary = LayerBoundary::new("api", "frontend");
        let result = OpenApiComplianceCheck.run(&ctx(&dir, &boundary, &rels));
        assert!(
            matches!(result, SeamResult::Warning(_)),
            "handler-without-spec must Warning, got {result:?}"
        );
        assert!(result.findings()[0]
            .message
            .contains("no OpenAPI/Swagger spec file found"));
    }

    #[test]
    fn warns_when_spec_present_but_handler_missing() {
        let (dir, rels) = fixture(&[(
            "openapi.yaml",
            "paths:\n  /x:\n    get:\n      responses:\n        '200':\n          content:\n            application/json:\n              schema:\n                properties:\n                  id:\n                    type: integer\n",
        )]);
        let boundary = LayerBoundary::new("api", "frontend");
        let result = OpenApiComplianceCheck.run(&ctx(&dir, &boundary, &rels));
        assert!(
            matches!(result, SeamResult::Warning(_)),
            "spec-without-handler must Warning, got {result:?}"
        );
        assert!(result.findings()[0]
            .message
            .contains("no recognizable handler response found"));
    }

    #[test]
    fn warns_when_spec_renamed_to_non_heuristic_path() {
        // Codex finding: a renamed/moved spec (e.g., `api-spec.yaml`) with a
        // real handler previously looked clean. Now it raises Warning because
        // the spec is not recognized → asymmetric → boundary uncompared.
        let (dir, rels) = fixture(&[
            (
                "specs/api-spec.yaml",
                "paths:\n  /x:\n    get:\n      responses:\n        '200':\n          content:\n            application/json:\n              schema:\n                properties:\n                  id:\n                    type: integer\n",
            ),
            (
                "src/handlers.ts",
                "export function h() { return { id: 1 }; }\n",
            ),
        ]);
        let boundary = LayerBoundary::new("api", "frontend");
        let result = OpenApiComplianceCheck.run(&ctx(&dir, &boundary, &rels));
        assert!(
            matches!(result, SeamResult::Warning(_)),
            "renamed spec must surface as Warning, got {result:?}"
        );
    }

    #[test]
    fn warns_when_double_miss_with_plausible_spec_and_handler() {
        // Phase 3 third-round adversarial finding: a renamed spec
        // (`api-spec.yaml`) AND a Rust handler that uses a typed response
        // (no `return {` / `.json(` literal) would previously fall through
        // both heuristics and silently Pass. The double-miss guard now
        // catches plausible-but-unclassified artifacts and emits Warning.
        let (dir, rels) = fixture(&[
            (
                "specs/api-spec.yaml",
                "openapi: 3.0.0\npaths:\n  /x:\n    get: {}\n",
            ),
            (
                "src/handlers.rs",
                "fn h() -> Json<Resp> { Json(Resp { id: 1 }) }\n",
            ),
        ]);
        let boundary = LayerBoundary::new("api", "frontend");
        let result = OpenApiComplianceCheck.run(&ctx(&dir, &boundary, &rels));
        assert!(
            matches!(result, SeamResult::Warning(_)),
            "double-miss with plausible artifacts must Warning, got {result:?}"
        );
        let msg = &result.findings()[0].message;
        assert!(
            msg.contains("plausible spec/handler artifacts")
                && msg.contains("api-spec.yaml")
                && msg.contains("handlers.rs"),
            "warning must name both plausible artifacts: {msg}"
        );
    }

    #[test]
    fn passes_when_only_plausible_spec_and_no_source() {
        // A renamed YAML spec ALONE on the boundary (no source files at
        // all) doesn't fire the double-miss guard — there's nothing on the
        // handler side to compare. The asymmetry is real but unactionable;
        // single-side plausibles do not trigger Warning to keep noise low.
        let (dir, rels) = fixture(&[(
            "config/api.yaml",
            "openapi: 3.0.0\npaths:\n  /x:\n    get: {}\n",
        )]);
        let boundary = LayerBoundary::new("api", "frontend");
        let result = OpenApiComplianceCheck.run(&ctx(&dir, &boundary, &rels));
        assert_eq!(
            result,
            SeamResult::Passed,
            "single-side plausible artifact must NOT over-trigger Warning, got {result:?}"
        );
    }

    #[test]
    fn passes_when_no_artifacts_on_boundary() {
        // Neither spec nor handler on this boundary — nothing to evaluate,
        // no silent bypass concern (no parsable artifacts exist).
        let (dir, rels) = fixture(&[("src/util.ts", "export const K = 1;\n")]);
        let boundary = LayerBoundary::new("api", "frontend");
        let result = OpenApiComplianceCheck.run(&ctx(&dir, &boundary, &rels));
        assert_eq!(result, SeamResult::Passed);
    }
}

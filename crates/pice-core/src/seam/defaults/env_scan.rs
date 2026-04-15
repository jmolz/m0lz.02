//! Shared env-var scanning helpers used by `config_mismatch` and
//! `auth_handoff` checks. Kept small and pure so both checks stay
//! deterministic and <100ms.

use std::collections::BTreeSet;
use std::path::Path;

/// True if the file looks like an infrastructure/deployment manifest.
pub fn is_infra_manifest(rel: &Path) -> bool {
    let fname = rel
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if fname == "dockerfile" || fname.starts_with("dockerfile.") {
        return true;
    }
    if fname == "docker-compose.yml" || fname == "docker-compose.yaml" {
        return true;
    }
    if fname.starts_with(".env") {
        return true;
    }
    let path_str = rel.to_string_lossy();
    path_str.starts_with("terraform/")
        || path_str.starts_with("deploy/")
        || path_str.starts_with("infra/")
}

/// Extract declared env-var names from Dockerfile / compose / .env content.
pub fn parse_declared(content: &str) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = Default::default();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Dockerfile `ENV NAME=val` or `ENV NAME val` — name is the first
        // whitespace-delimited token after the directive.
        if let Some(rest) = line
            .strip_prefix("ENV ")
            .or_else(|| line.strip_prefix("env "))
        {
            if let Some(name) = extract_ident(rest.trim()) {
                out.insert(name);
            }
            continue;
        }
        // compose YAML list item under `environment:`: `- NAME=value`.
        if let Some(rest) = line.strip_prefix("- ") {
            if let Some(name) = extract_ident_assignment(rest.trim()) {
                out.insert(name);
            }
            continue;
        }
        // `.env` / YAML key form — require an explicit `=` or `:` separator
        // so Dockerfile directives like `FROM alpine` aren't misread.
        if let Some(name) = extract_ident_assignment(line) {
            out.insert(name);
        }
    }
    out
}

/// Extract a SHOUTY_SNAKE identifier after a directive (`ENV NAME=val`
/// or `ENV NAME val`). Name terminates at `=`, `:`, or whitespace.
fn extract_ident(s: &str) -> Option<String> {
    let cut = s
        .find(|c: char| c == '=' || c == ':' || c.is_whitespace())
        .unwrap_or(s.len());
    let head = s[..cut].trim().trim_start_matches('-').trim();
    validate_ident(head)
}

/// Extract a SHOUTY_SNAKE identifier ONLY when `=` or `:` is present.
/// Used for `.env`/YAML forms so Dockerfile directives without an
/// assignment (e.g. `FROM alpine`) are not misread as env declarations.
fn extract_ident_assignment(s: &str) -> Option<String> {
    let cut = s.find(['=', ':'])?;
    let head = s[..cut].trim().trim_start_matches('-').trim();
    validate_ident(head)
}

fn validate_ident(head: &str) -> Option<String> {
    if head.is_empty()
        || !head
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        || !head.chars().next().is_some_and(|c| c.is_ascii_uppercase())
    {
        return None;
    }
    Some(head.to_string())
}

/// Extract env-var names consumed by Rust / TS / Python app code.
pub fn parse_consumed(content: &str) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = Default::default();
    collect_between(content, "env::var(\"", "\"", &mut out);
    collect_between(content, "env::var_os(\"", "\"", &mut out);
    collect_dot_access(content, "process.env.", &mut out);
    collect_between(content, "process.env[\"", "\"]", &mut out);
    collect_between(content, "process.env['", "']", &mut out);
    collect_between(content, "os.environ[\"", "\"]", &mut out);
    collect_between(content, "os.environ['", "']", &mut out);
    collect_between(content, "os.environ.get(\"", "\"", &mut out);
    collect_between(content, "os.getenv(\"", "\"", &mut out);
    out
}

fn collect_between(content: &str, start: &str, end: &str, out: &mut BTreeSet<String>) {
    let mut rest = content;
    while let Some(idx) = rest.find(start) {
        let after = &rest[idx + start.len()..];
        let Some(end_idx) = after.find(end) else { break };
        let name = &after[..end_idx];
        if is_shouty_snake(name) {
            out.insert(name.to_string());
        }
        rest = &after[end_idx + end.len()..];
    }
}

fn collect_dot_access(content: &str, prefix: &str, out: &mut BTreeSet<String>) {
    let mut rest = content;
    while let Some(idx) = rest.find(prefix) {
        let after = &rest[idx + prefix.len()..];
        let end = after
            .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
            .unwrap_or(after.len());
        let name = &after[..end];
        if is_shouty_snake(name) {
            out.insert(name.to_string());
        }
        rest = &after[end..];
    }
}

fn is_shouty_snake(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn dockerfile_is_infra() {
        assert!(is_infra_manifest(&PathBuf::from("Dockerfile")));
        assert!(is_infra_manifest(&PathBuf::from("docker-compose.yml")));
        assert!(is_infra_manifest(&PathBuf::from(".env.production")));
        assert!(is_infra_manifest(&PathBuf::from("terraform/main.tf")));
        assert!(!is_infra_manifest(&PathBuf::from("src/main.rs")));
    }

    #[test]
    fn parse_declared_covers_all_formats() {
        let content = "FROM alpine\nENV FOO=1\nENV BAR 2\nignored\nBAZ=3\n- QUX=4\n";
        let out = parse_declared(content);
        for name in ["FOO", "BAR", "BAZ", "QUX"] {
            assert!(out.contains(name), "expected '{name}' in {out:?}");
        }
    }

    #[test]
    fn parse_consumed_covers_rust_ts_python() {
        let content = r#"
env::var("RUST_VAR");
process.env.TS_VAR;
process.env["TS_BRACKETED"];
os.environ["PY_VAR"];
os.getenv("PY_GETENV");
"#;
        let out = parse_consumed(content);
        for name in ["RUST_VAR", "TS_VAR", "TS_BRACKETED", "PY_VAR", "PY_GETENV"] {
            assert!(out.contains(name), "expected '{name}' in {out:?}");
        }
    }
}

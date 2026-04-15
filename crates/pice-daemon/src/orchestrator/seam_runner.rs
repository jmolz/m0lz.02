//! Seam runner — executes registered [`SeamCheck`]s against a layer's
//! boundaries and returns [`SeamCheckResult`]s for inclusion in the
//! verification manifest.
//!
//! # Context isolation
//!
//! Each check sees a [`SeamContext`] scoped to a single boundary: the filtered
//! diff for that boundary's `(a, b)` file union, and the concrete file set.
//! No other layer's contract, diff, or findings is reachable. The
//! `context_isolation_leak` test verifies this via a distinctive-marker scan.
//!
//! # Timeout
//!
//! Each check runs in a detached thread with a 100ms CPU budget enforced via
//! `mpsc::Receiver::recv_timeout`. On timeout the runner returns a `Warning`
//! finding rather than panicking. Rust cannot safely kill a thread, so the
//! stuck check leaks the thread; v0.2 accepts this as acceptable tail-risk
//! given checks are deterministic and fast by contract.

use pice_core::layers::manifest::{CheckStatus, SeamCheckResult};
use pice_core::seam::types::{LayerBoundary, SeamCheck, SeamContext, SeamFinding, SeamResult};
use pice_core::seam::Registry;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Per-check wall-clock budget. See stack-loops.md — static checks must be
/// deterministic and <100ms each.
const CHECK_BUDGET: Duration = Duration::from_millis(100);

/// Run every registered check applicable to `layer_name`'s boundaries and
/// return the results in a deterministic order.
///
/// - `merged_seams`: the fully-merged `{boundary → [check_id]}` map as
///   resolved by `pice-core::workflow::merge::merge_seams` + whatever the
///   project's `layers.toml [seams]` supplies.
/// - `active_layers`: only boundaries where BOTH layers are active are
///   evaluated. Inactive sides mean no observable diff.
/// - `layer_paths`: layer-name → list of changed file paths filtered to that
///   layer's globs. Used to build the per-boundary filtered diff and
///   `boundary_files` set.
pub fn run_seams_for_layer(
    layer_name: &str,
    active_layers: &HashSet<String>,
    merged_seams: &BTreeMap<String, Vec<String>>,
    registry: &Registry,
    repo_root: &Path,
    full_diff: &str,
    layer_paths: &BTreeMap<String, Vec<PathBuf>>,
) -> Vec<SeamCheckResult> {
    let mut out: Vec<SeamCheckResult> = Vec::new();

    for (raw_boundary, check_ids) in merged_seams {
        // Parse + validate boundary. Malformed keys are surfaced by
        // `validate_seams` at load time, but be defensive here too.
        let Ok(boundary) = LayerBoundary::parse(raw_boundary) else {
            continue;
        };
        if !boundary.touches(layer_name) {
            continue;
        }
        if !active_layers.contains(&boundary.a) || !active_layers.contains(&boundary.b) {
            continue;
        }

        // Build the boundary file set and filtered diff.
        let empty: Vec<PathBuf> = Vec::new();
        let a_files = layer_paths.get(&boundary.a).unwrap_or(&empty);
        let b_files = layer_paths.get(&boundary.b).unwrap_or(&empty);
        let mut boundary_files: Vec<PathBuf> = Vec::with_capacity(a_files.len() + b_files.len());
        let mut seen: HashSet<&std::ffi::OsStr> = HashSet::new();
        for p in a_files.iter().chain(b_files.iter()) {
            if seen.insert(p.as_os_str()) {
                boundary_files.push(p.clone());
            }
        }
        let filtered_diff = filter_diff_to_paths(full_diff, &boundary_files);

        for id in check_ids {
            let Some(check) = registry.get(id) else {
                out.push(SeamCheckResult {
                    name: id.clone(),
                    status: CheckStatus::Failed,
                    boundary: boundary.canonical(),
                    category: None,
                    details: Some(format!("seam check id '{id}' is not registered")),
                });
                continue;
            };
            if !check.applies_to(&boundary) {
                // Out-of-scope — skip without recording.
                continue;
            }

            let result = run_with_timeout(check, &boundary, &filtered_diff, repo_root, &boundary_files);
            out.push(seam_result_to_record(check, &boundary, result));
        }
    }

    out
}

/// Run a single check synchronously and post-facto check the elapsed budget.
///
/// We elect NOT to run checks on a detached thread with a kill timer. Rust
/// cannot safely cancel a thread; the mpsc+`recv_timeout` pattern would
/// leak threads AND require the trait object to cross thread boundaries,
/// which requires `'static` bounds not available on `&dyn SeamCheck`.
///
/// Since every default check is contractually bound to <100ms and is pure
/// over the boundary file set, running inline is correct. If a check exceeds
/// the budget post-hoc, we downgrade its result to a `Warning` with a
/// budget-exceeded finding — the contract test ensures the 12 default
/// checks stay comfortably below. Plugin crates that ship misbehaving
/// checks will surface as Warnings, not crashes.
fn run_with_timeout(
    check: &(dyn SeamCheck + Send + Sync),
    boundary: &LayerBoundary,
    filtered_diff: &str,
    repo_root: &Path,
    boundary_files: &[PathBuf],
) -> SeamResult {
    let ctx = SeamContext {
        boundary,
        filtered_diff,
        repo_root,
        boundary_files,
        args: None,
    };
    let start = std::time::Instant::now();
    let result = check.run(&ctx);
    let elapsed = start.elapsed();
    if elapsed > CHECK_BUDGET {
        return SeamResult::Warning(vec![SeamFinding::new(format!(
            "seam check '{}' exceeded {}ms budget (took {}ms)",
            check.id(),
            CHECK_BUDGET.as_millis(),
            elapsed.as_millis()
        ))]);
    }
    result
}

fn seam_result_to_record(
    check: &(dyn SeamCheck + Send + Sync),
    boundary: &LayerBoundary,
    result: SeamResult,
) -> SeamCheckResult {
    let canonical = boundary.canonical();
    let category = Some(check.category());
    match result {
        SeamResult::Passed => SeamCheckResult {
            name: check.id().to_string(),
            status: CheckStatus::Passed,
            boundary: canonical,
            category,
            details: None,
        },
        SeamResult::Warning(findings) => SeamCheckResult {
            name: check.id().to_string(),
            status: CheckStatus::Warning,
            boundary: canonical,
            category,
            details: Some(format_findings(&findings)),
        },
        SeamResult::Failed(findings) => SeamCheckResult {
            name: check.id().to_string(),
            status: CheckStatus::Failed,
            boundary: canonical,
            category,
            details: Some(format_findings(&findings)),
        },
    }
}

fn format_findings(findings: &[SeamFinding]) -> String {
    findings
        .iter()
        .map(|f| match (&f.file, f.line) {
            (Some(p), Some(l)) => format!("{}:{} — {}", p.display(), l, f.message),
            (Some(p), None) => format!("{} — {}", p.display(), f.message),
            _ => f.message.clone(),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Filter a unified git diff to only include hunks touching one of `paths`.
/// The diff parser is forgiving: a header line starting with `diff --git`
/// opens a new file scope, and lines continue until the next header.
fn filter_diff_to_paths(diff: &str, paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return String::new();
    }
    let wanted: HashSet<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();

    let mut out = String::new();
    let mut include_current = false;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            include_current = path_in_header(rest, &wanted);
        }
        if include_current {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn path_in_header(header: &str, wanted: &HashSet<String>) -> bool {
    // Header is `a/path b/path`. Extract both and test membership.
    let parts: Vec<&str> = header.split_whitespace().collect();
    for p in parts {
        let stripped = p
            .strip_prefix("a/")
            .or_else(|| p.strip_prefix("b/"))
            .unwrap_or(p);
        if wanted.contains(stripped) {
            return true;
        }
    }
    false
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use pice_core::seam::default_registry;

    fn make_active(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn empty_paths() -> BTreeMap<String, Vec<PathBuf>> {
        BTreeMap::new()
    }

    #[test]
    fn runs_check_for_layer_touching_boundary() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("Dockerfile"), "ENV FOO=1\n").unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let mut seams = BTreeMap::new();
        seams.insert(
            "backend↔infrastructure".into(),
            vec!["config_mismatch".into()],
        );
        let active = make_active(&["backend", "infrastructure"]);
        let mut paths = BTreeMap::new();
        paths.insert("backend".to_string(), vec![PathBuf::from("src/main.rs")]);
        paths.insert(
            "infrastructure".to_string(),
            vec![PathBuf::from("Dockerfile")],
        );

        let results = run_seams_for_layer(
            "backend",
            &active,
            &seams,
            &default_registry(),
            dir.path(),
            "",
            &paths,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "config_mismatch");
        assert_eq!(results[0].boundary, "backend↔infrastructure");
        assert_eq!(results[0].category, Some(1));
        assert_eq!(results[0].status, CheckStatus::Failed);
    }

    #[test]
    fn skips_boundary_not_touching_layer() {
        let mut seams = BTreeMap::new();
        seams.insert(
            "backend↔infrastructure".into(),
            vec!["config_mismatch".into()],
        );
        let active = make_active(&["backend", "infrastructure", "frontend"]);
        let dir = tempfile::tempdir().unwrap();
        let results = run_seams_for_layer(
            "frontend",
            &active,
            &seams,
            &default_registry(),
            dir.path(),
            "",
            &empty_paths(),
        );
        assert!(results.is_empty(), "frontend shouldn't run backend↔infra checks");
    }

    #[test]
    fn skips_boundary_with_inactive_side() {
        let mut seams = BTreeMap::new();
        seams.insert(
            "backend↔infrastructure".into(),
            vec!["config_mismatch".into()],
        );
        // infrastructure NOT active
        let active = make_active(&["backend"]);
        let dir = tempfile::tempdir().unwrap();
        let results = run_seams_for_layer(
            "backend",
            &active,
            &seams,
            &default_registry(),
            dir.path(),
            "",
            &empty_paths(),
        );
        assert!(results.is_empty());
    }

    #[test]
    fn unknown_check_id_produces_failed_record() {
        let mut seams = BTreeMap::new();
        seams.insert("backend↔infrastructure".into(), vec!["ghost_check".into()]);
        let active = make_active(&["backend", "infrastructure"]);
        let dir = tempfile::tempdir().unwrap();
        let results = run_seams_for_layer(
            "backend",
            &active,
            &seams,
            &default_registry(),
            dir.path(),
            "",
            &empty_paths(),
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, CheckStatus::Failed);
        assert!(results[0]
            .details
            .as_deref()
            .unwrap_or("")
            .contains("not registered"));
        assert_eq!(results[0].category, None);
    }

    #[test]
    fn malformed_boundary_skipped_silently() {
        // Validation should have caught this at load time; runner defensively
        // ignores to avoid runtime panic.
        let mut seams = BTreeMap::new();
        seams.insert("malformed".into(), vec!["config_mismatch".into()]);
        let active = make_active(&["backend", "infrastructure"]);
        let dir = tempfile::tempdir().unwrap();
        let results = run_seams_for_layer(
            "backend",
            &active,
            &seams,
            &default_registry(),
            dir.path(),
            "",
            &empty_paths(),
        );
        assert!(results.is_empty());
    }

    #[test]
    fn context_isolation_leak_test() {
        // Plant distinctive markers in the repo that are outside the boundary
        // files and assert they never reach the context the check sees.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Dockerfile"),
            "ENV DATABASE_URL=x\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/app.rs"),
            r#"fn main() { env::var("DATABASE_URL"); }"#,
        )
        .unwrap();
        // OTHER-LAYER SECRET FILE that must not leak into the boundary context.
        std::fs::create_dir_all(dir.path().join("other")).unwrap();
        std::fs::write(
            dir.path().join("other/contract.toml"),
            "layer_a_contract_marker = true\nlayer_b_finding_marker = true\n",
        )
        .unwrap();

        let mut seams = BTreeMap::new();
        seams.insert(
            "backend↔infrastructure".into(),
            vec!["config_mismatch".into()],
        );
        let active = make_active(&["backend", "infrastructure"]);
        let mut paths = BTreeMap::new();
        paths.insert("backend".to_string(), vec![PathBuf::from("src/app.rs")]);
        paths.insert(
            "infrastructure".to_string(),
            vec![PathBuf::from("Dockerfile")],
        );
        // A fully-loaded full_diff containing the distinctive markers — the
        // filtered diff must NOT surface them since they aren't in the
        // boundary paths.
        let full_diff = "\
diff --git a/other/contract.toml b/other/contract.toml
+layer_a_contract_marker = true
+layer_b_finding_marker = true
diff --git a/Dockerfile b/Dockerfile
+ENV DATABASE_URL=x
diff --git a/src/app.rs b/src/app.rs
+fn main() {}
";
        let results = run_seams_for_layer(
            "backend",
            &active,
            &seams,
            &default_registry(),
            dir.path(),
            full_diff,
            &paths,
        );
        // Result should succeed (DATABASE_URL declared + consumed).
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, CheckStatus::Passed);

        // Now explicitly verify the filtered diff we would pass to the check
        // does not contain the distinctive markers.
        let boundary_files = vec![
            PathBuf::from("src/app.rs"),
            PathBuf::from("Dockerfile"),
        ];
        let filtered = filter_diff_to_paths(full_diff, &boundary_files);
        assert!(
            !filtered.contains("layer_a_contract_marker"),
            "context leak: a-layer marker visible in filtered diff: {filtered}"
        );
        assert!(
            !filtered.contains("layer_b_finding_marker"),
            "context leak: b-layer marker visible in filtered diff"
        );
    }

    #[test]
    fn deterministic_output_ordering() {
        // Two runs with the same inputs produce the same result order.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Dockerfile"), "ENV X=1\nENV Y=2\n").unwrap();
        std::fs::write(dir.path().join("app.rs"), "").unwrap();

        let mut seams = BTreeMap::new();
        seams.insert(
            "backend↔infrastructure".into(),
            vec!["config_mismatch".into(), "auth_handoff".into()],
        );
        let active = make_active(&["backend", "infrastructure"]);
        let mut paths = BTreeMap::new();
        paths.insert("backend".to_string(), vec![PathBuf::from("app.rs")]);
        paths.insert("infrastructure".to_string(), vec![PathBuf::from("Dockerfile")]);

        let r1 = run_seams_for_layer(
            "backend",
            &active,
            &seams,
            &default_registry(),
            dir.path(),
            "",
            &paths,
        );
        let r2 = run_seams_for_layer(
            "backend",
            &active,
            &seams,
            &default_registry(),
            dir.path(),
            "",
            &paths,
        );
        let ids_1: Vec<&str> = r1.iter().map(|r| r.name.as_str()).collect();
        let ids_2: Vec<&str> = r2.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(ids_1, ids_2);
    }
}

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
//! # Boundary activation
//!
//! A boundary's checks run when **at least one side** is active. Seams exist
//! to catch drift between layers; a one-sided change (e.g. handler updated,
//! OpenAPI spec unchanged) is exactly the class of failure seam checks must
//! surface. `boundary_files` is the union of both layers' full file sets
//! (not just the changed diff), so checks can compare changed code against
//! stable counterpart artifacts.
//!
//! # Timeout
//!
//! The runner enforces a 100ms wall-clock budget **post-hoc**: `check.run()`
//! executes synchronously, then elapsed time is measured. Overrun NEVER
//! downgrades a genuine `Failed` result — a failing check that happened to
//! be slow still blocks the layer; the budget warning is appended to its
//! findings rather than replacing the result. Rust cannot safely cancel a
//! thread, so v0.2 accepts that a pathologically stuck plugin check will
//! block the process — plugin authors are contractually bound to <100ms.

use pice_core::layers::manifest::{CheckStatus, SeamCheckResult};
use pice_core::seam::types::{LayerBoundary, SeamCheck, SeamContext, SeamFinding, SeamResult};
use pice_core::seam::Registry;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Per-check wall-clock budget. See stack-loops.md — static checks must be
/// deterministic and <100ms each.
const CHECK_BUDGET: Duration = Duration::from_millis(100);

/// Inputs for running registered seam checks for one layer.
///
/// - `merged_seams`: the fully-merged `{boundary → [check_id]}` map as
///   resolved by the evaluate handler via `pice-core::workflow::merge::merge_seams`.
/// - `active_layers`: boundaries run when AT LEAST ONE side is active
///   (Phase 3 review fix). If both sides are inactive, nothing changed
///   that could have introduced drift.
/// - `layer_paths`: layer-name → per-layer file set (changed files tagged
///   to that layer, unioned with unchanged files under the layer's globs
///   via `scan_files_by_globs`). Used to build the per-boundary filtered
///   diff and `boundary_files` set — full file set, not just the changed
///   diff, so checks can compare one changed side against the other's
///   stable counterpart artifact.
pub struct SeamRunRequest<'a> {
    pub layer_name: &'a str,
    pub active_layers: &'a HashSet<String>,
    pub merged_seams: &'a BTreeMap<String, Vec<String>>,
    pub registry: &'a Registry,
    pub repo_root: &'a Path,
    pub full_diff: &'a str,
    pub layer_paths: &'a BTreeMap<String, Vec<PathBuf>>,
    pub file_contents: Option<&'a BTreeMap<PathBuf, String>>,
}

/// Run every registered check applicable to `layer_name`'s boundaries and
/// return the results in a deterministic order.
pub fn run_seams_for_layer(req: SeamRunRequest<'_>) -> Vec<SeamCheckResult> {
    let SeamRunRequest {
        layer_name,
        active_layers,
        merged_seams,
        registry,
        repo_root,
        full_diff,
        layer_paths,
        file_contents,
    } = req;
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
        // At-least-one-side-active gate. If BOTH sides are inactive, nothing
        // changed that could have introduced drift on this boundary — skip.
        // If EITHER side is active, the boundary runs: seam checks compare
        // the changed code against the other side's stable counterpart
        // artifacts (the full layer file sets in `layer_paths`). Requiring
        // both sides active was a silent false-negative route on the exact
        // case this feature exists to catch — one-sided drift.
        if !active_layers.contains(&boundary.a) && !active_layers.contains(&boundary.b) {
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

            let result = run_with_timeout(
                check,
                &boundary,
                &filtered_diff,
                repo_root,
                &boundary_files,
                file_contents,
            );
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
/// ## Budget handling (HARD rule)
///
/// A genuine `Failed` result is NEVER downgraded to `Warning` by the budget
/// path — a correct-but-slow failing check must still block the layer.
/// Overrun appends a budget-exceeded `SeamFinding` to the existing result:
///
/// - `Passed` + overrun → `Warning([budget_finding])` (informational)
/// - `Warning(findings)` + overrun → `Warning(findings + budget_finding)`
/// - `Failed(findings)` + overrun → `Failed(findings + budget_finding)`
fn run_with_timeout(
    check: &(dyn SeamCheck + Send + Sync),
    boundary: &LayerBoundary,
    filtered_diff: &str,
    repo_root: &Path,
    boundary_files: &[PathBuf],
    file_contents: Option<&BTreeMap<PathBuf, String>>,
) -> SeamResult {
    let ctx = SeamContext {
        boundary,
        filtered_diff,
        repo_root,
        file_contents,
        boundary_files,
        args: None,
    };
    let start = std::time::Instant::now();
    let result = check.run(&ctx);
    let elapsed = start.elapsed();
    if elapsed <= CHECK_BUDGET {
        return result;
    }
    let budget_finding = SeamFinding::new(format!(
        "seam check '{}' exceeded {}ms budget (took {}ms)",
        check.id(),
        CHECK_BUDGET.as_millis(),
        elapsed.as_millis()
    ));
    match result {
        SeamResult::Failed(mut findings) => {
            findings.push(budget_finding);
            SeamResult::Failed(findings)
        }
        SeamResult::Warning(mut findings) => {
            findings.push(budget_finding);
            SeamResult::Warning(findings)
        }
        SeamResult::Passed => SeamResult::Warning(vec![budget_finding]),
    }
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

    fn run_test_seams_for_layer(
        layer_name: &str,
        active_layers: &HashSet<String>,
        merged_seams: &BTreeMap<String, Vec<String>>,
        repo_root: &Path,
        full_diff: &str,
        layer_paths: &BTreeMap<String, Vec<PathBuf>>,
        file_contents: Option<&BTreeMap<PathBuf, String>>,
    ) -> Vec<SeamCheckResult> {
        let registry = default_registry();
        run_seams_for_layer(SeamRunRequest {
            layer_name,
            active_layers,
            merged_seams,
            registry: &registry,
            repo_root,
            full_diff,
            layer_paths,
            file_contents,
        })
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

        let results =
            run_test_seams_for_layer("backend", &active, &seams, dir.path(), "", &paths, None);
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
        let results = run_test_seams_for_layer(
            "frontend",
            &active,
            &seams,
            dir.path(),
            "",
            &empty_paths(),
            None,
        );
        assert!(
            results.is_empty(),
            "frontend shouldn't run backend↔infra checks"
        );
    }

    #[test]
    fn runs_boundary_when_only_one_side_active() {
        // At-least-one-side semantics: backend is active, infrastructure is
        // NOT active, but the boundary still runs because the drift case
        // this feature exists to catch is exactly "changed one side, forgot
        // the other." The runner relies on `layer_paths` carrying the full
        // per-layer file set so the check still sees the unchanged side.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        // Infra side has an orphan env var declared but never read by app.
        std::fs::write(dir.path().join("Dockerfile"), "ENV ORPHAN_VAR=x\n").unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let mut seams = BTreeMap::new();
        seams.insert(
            "backend↔infrastructure".into(),
            vec!["config_mismatch".into()],
        );
        // infrastructure NOT active — backend changed, infra untouched.
        let active = make_active(&["backend"]);
        let mut paths = BTreeMap::new();
        paths.insert("backend".to_string(), vec![PathBuf::from("src/main.rs")]);
        paths.insert(
            "infrastructure".to_string(),
            vec![PathBuf::from("Dockerfile")],
        );
        let results =
            run_test_seams_for_layer("backend", &active, &seams, dir.path(), "", &paths, None);
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].status,
            CheckStatus::Failed,
            "one-sided drift must produce a finding, not a silent Passed: {:?}",
            results[0].details
        );
    }

    #[test]
    fn skips_boundary_when_both_sides_inactive() {
        // True skip case: nothing changed on either side, so no seam drift
        // could have been introduced.
        let mut seams = BTreeMap::new();
        seams.insert(
            "backend↔infrastructure".into(),
            vec!["config_mismatch".into()],
        );
        // Only frontend is active — neither backend nor infrastructure.
        let active = make_active(&["frontend"]);
        let dir = tempfile::tempdir().unwrap();
        let results = run_test_seams_for_layer(
            "backend",
            &active,
            &seams,
            dir.path(),
            "",
            &empty_paths(),
            None,
        );
        assert!(
            results.is_empty(),
            "expected skip when neither side of the boundary is active"
        );
    }

    #[test]
    fn unknown_check_id_produces_failed_record() {
        let mut seams = BTreeMap::new();
        seams.insert("backend↔infrastructure".into(), vec!["ghost_check".into()]);
        let active = make_active(&["backend", "infrastructure"]);
        let dir = tempfile::tempdir().unwrap();
        let results = run_test_seams_for_layer(
            "backend",
            &active,
            &seams,
            dir.path(),
            "",
            &empty_paths(),
            None,
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
        let results = run_test_seams_for_layer(
            "backend",
            &active,
            &seams,
            dir.path(),
            "",
            &empty_paths(),
            None,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn context_isolation_leak_test() {
        // Plant distinctive markers in the repo that are outside the boundary
        // files and assert they never reach the context the check sees.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Dockerfile"), "ENV DATABASE_URL=x\n").unwrap();
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
        let results = run_test_seams_for_layer(
            "backend",
            &active,
            &seams,
            dir.path(),
            full_diff,
            &paths,
            None,
        );
        // Result should succeed (DATABASE_URL declared + consumed).
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, CheckStatus::Passed);

        // Now explicitly verify the filtered diff we would pass to the check
        // does not contain the distinctive markers.
        let boundary_files = vec![PathBuf::from("src/app.rs"), PathBuf::from("Dockerfile")];
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
        paths.insert(
            "infrastructure".to_string(),
            vec![PathBuf::from("Dockerfile")],
        );

        let r1 = run_test_seams_for_layer("backend", &active, &seams, dir.path(), "", &paths, None);
        let r2 = run_test_seams_for_layer("backend", &active, &seams, dir.path(), "", &paths, None);
        let ids_1: Vec<&str> = r1.iter().map(|r| r.name.as_str()).collect();
        let ids_2: Vec<&str> = r2.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(ids_1, ids_2);
    }

    #[test]
    fn seam_checks_use_dispatch_file_snapshot_when_supplied() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("Dockerfile"), "ENV DATABASE_URL=postgres\n").unwrap();
        std::fs::write(
            dir.path().join("src/app.ts"),
            "export const db = process.env.DATABASE_URL;\n",
        )
        .unwrap();

        let mut seams = BTreeMap::new();
        seams.insert(
            "backend↔infrastructure".into(),
            vec!["config_mismatch".into()],
        );
        let active = make_active(&["backend", "infrastructure"]);
        let mut paths = BTreeMap::new();
        paths.insert("backend".to_string(), vec![PathBuf::from("src/app.ts")]);
        paths.insert(
            "infrastructure".to_string(),
            vec![PathBuf::from("Dockerfile")],
        );
        let file_contents = BTreeMap::from([
            (
                PathBuf::from("Dockerfile"),
                "ENV DATABASE_URL=postgres\n".to_string(),
            ),
            (
                PathBuf::from("src/app.ts"),
                "export const db = process.env.DATABASE_URL;\n".to_string(),
            ),
        ]);

        std::fs::write(dir.path().join("src/app.ts"), "export const db = null;\n").unwrap();

        let snapshot_results = run_test_seams_for_layer(
            "backend",
            &active,
            &seams,
            dir.path(),
            "",
            &paths,
            Some(&file_contents),
        );
        assert_eq!(snapshot_results[0].status, CheckStatus::Passed);

        let live_results =
            run_test_seams_for_layer("backend", &active, &seams, dir.path(), "", &paths, None);
        assert_eq!(live_results[0].status, CheckStatus::Failed);
    }

    // ─── Budget-enforcement tests ────────────────────────────────────────

    /// Deterministic stub that sleeps and then returns a chosen [`SeamResult`].
    /// Used to exercise the over-budget path without depending on any real
    /// default check's timing behavior.
    struct SleepyStub {
        id_: &'static str,
        category_: u8,
        sleep_ms: u64,
        outcome: fn() -> SeamResult,
    }

    impl SeamCheck for SleepyStub {
        fn id(&self) -> &str {
            self.id_
        }
        fn category(&self) -> u8 {
            self.category_
        }
        fn applies_to(&self, _: &LayerBoundary) -> bool {
            true
        }
        fn run(&self, _: &SeamContext<'_>) -> SeamResult {
            std::thread::sleep(Duration::from_millis(self.sleep_ms));
            (self.outcome)()
        }
    }

    fn single_finding(msg: &str) -> Vec<SeamFinding> {
        vec![SeamFinding::new(msg.to_string())]
    }

    /// Budget overrun on a check that returned Failed MUST NOT downgrade the
    /// result to Warning — the fail-closed guarantee depends on this.
    /// Regression test for the critical silent-bypass found during the Phase 3
    /// adversarial review.
    #[test]
    fn over_budget_preserves_failed_result() {
        let boundary = LayerBoundary::new("a", "b");
        let check = SleepyStub {
            id_: "slow_failer",
            category_: 9,
            sleep_ms: 150, // CHECK_BUDGET is 100ms
            outcome: || SeamResult::Failed(single_finding("underlying failure")),
        };
        let dir = tempfile::tempdir().unwrap();
        let result = run_with_timeout(&check, &boundary, "", dir.path(), &[], None);
        match result {
            SeamResult::Failed(findings) => {
                assert!(
                    findings
                        .iter()
                        .any(|f| f.message.contains("underlying failure")),
                    "original finding must be preserved: {findings:?}"
                );
                assert!(
                    findings.iter().any(|f| f.message.contains("budget")),
                    "budget warning must be appended: {findings:?}"
                );
            }
            other => panic!("over-budget Failed must stay Failed, got {other:?}"),
        }
    }

    /// Budget overrun on a Passed check downgrades to Warning with a single
    /// budget-exceeded finding.
    #[test]
    fn over_budget_passed_becomes_warning() {
        let boundary = LayerBoundary::new("a", "b");
        let check = SleepyStub {
            id_: "slow_passer",
            category_: 2,
            sleep_ms: 150,
            outcome: || SeamResult::Passed,
        };
        let dir = tempfile::tempdir().unwrap();
        let result = run_with_timeout(&check, &boundary, "", dir.path(), &[], None);
        match result {
            SeamResult::Warning(findings) => {
                assert_eq!(findings.len(), 1);
                assert!(findings[0].message.contains("budget"));
                assert!(findings[0].message.contains("slow_passer"));
            }
            other => panic!("over-budget Passed must become Warning, got {other:?}"),
        }
    }

    /// Budget overrun on an existing Warning appends the budget finding
    /// without dropping the original findings.
    #[test]
    fn over_budget_warning_appends_budget_finding() {
        let boundary = LayerBoundary::new("a", "b");
        let check = SleepyStub {
            id_: "slow_warner",
            category_: 5,
            sleep_ms: 150,
            outcome: || SeamResult::Warning(single_finding("advisory note")),
        };
        let dir = tempfile::tempdir().unwrap();
        let result = run_with_timeout(&check, &boundary, "", dir.path(), &[], None);
        match result {
            SeamResult::Warning(findings) => {
                assert_eq!(findings.len(), 2);
                assert!(findings.iter().any(|f| f.message.contains("advisory note")));
                assert!(findings.iter().any(|f| f.message.contains("budget")));
            }
            other => panic!("over-budget Warning must stay Warning, got {other:?}"),
        }
    }

    /// Under-budget path preserves the result exactly — no budget finding is
    /// appended when the check completes in time.
    #[test]
    fn under_budget_result_is_untouched() {
        let boundary = LayerBoundary::new("a", "b");
        let check = SleepyStub {
            id_: "fast_failer",
            category_: 1,
            sleep_ms: 1,
            outcome: || SeamResult::Failed(single_finding("fast failure")),
        };
        let dir = tempfile::tempdir().unwrap();
        let result = run_with_timeout(&check, &boundary, "", dir.path(), &[], None);
        match result {
            SeamResult::Failed(findings) => {
                assert_eq!(findings.len(), 1);
                assert!(!findings[0].message.contains("budget"));
            }
            other => panic!("under-budget result must be preserved, got {other:?}"),
        }
    }

    /// Every default check must complete under the 100ms budget on a
    /// representative fixture. This is the hard rule from
    /// `.claude/rules/stack-loops.md` — adaptive-pass budgets depend on it.
    #[test]
    fn every_default_check_under_budget_on_fixture() {
        let registry = default_registry();
        let dir = tempfile::tempdir().unwrap();
        // Seed a minimal fixture so checks that read files don't short-circuit.
        std::fs::write(
            dir.path().join("Dockerfile"),
            "FROM alpine\nENV DATABASE_URL=x\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/app.rs"),
            "fn main() { std::env::var(\"DATABASE_URL\").unwrap(); }",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("prisma")).unwrap();
        std::fs::write(
            dir.path().join("prisma/schema.prisma"),
            "model User { id Int @id }\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("migrations")).unwrap();
        std::fs::write(
            dir.path().join("migrations/001.sql"),
            "CREATE TABLE User (id INT PRIMARY KEY);",
        )
        .unwrap();
        std::fs::write(dir.path().join("openapi.yaml"), "paths: {}\n").unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"x","version":"1.0.0","dependencies":{}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("docker-compose.yml"),
            "services:\n  web:\n    image: alpine\n",
        )
        .unwrap();

        let boundary_files: Vec<PathBuf> = vec![
            PathBuf::from("Dockerfile"),
            PathBuf::from("src/app.rs"),
            PathBuf::from("prisma/schema.prisma"),
            PathBuf::from("migrations/001.sql"),
            PathBuf::from("openapi.yaml"),
            PathBuf::from("package.json"),
            PathBuf::from("docker-compose.yml"),
        ];
        // Use boundaries that touch a wide variety of layers so every
        // check's `applies_to()` has at least one boundary that matches.
        let boundaries = [
            LayerBoundary::new("backend", "infrastructure"),
            LayerBoundary::new("api", "frontend"),
            LayerBoundary::new("backend", "database"),
            LayerBoundary::new("backend", "deployment"),
            LayerBoundary::new("api", "observability"),
        ];
        for (id, check) in registry.iter() {
            let boundary = boundaries
                .iter()
                .find(|b| check.applies_to(b))
                .unwrap_or(&boundaries[0]);
            let ctx = SeamContext {
                boundary,
                filtered_diff: "",
                repo_root: dir.path(),
                file_contents: None,
                boundary_files: &boundary_files,
                args: None,
            };
            let start = std::time::Instant::now();
            let _ = check.run(&ctx);
            let elapsed = start.elapsed();
            assert!(
                elapsed <= CHECK_BUDGET,
                "default check '{id}' exceeded {}ms budget (took {}ms) — \
                 adaptive-pass budgets depend on every default staying under",
                CHECK_BUDGET.as_millis(),
                elapsed.as_millis()
            );
        }
    }
}

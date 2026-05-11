//! Phase 7 Task 8: startup reconciliation for interrupted background
//! dispatches.
//!
//! When the daemon starts (cold start or post-crash restart), the state
//! directory can contain manifests left over from the previous process
//! lifetime. Three disposition rules apply, keyed on `overall_status`:
//!
//! | Prior status | Action |
//! |--------------|--------|
//! | `Queued` | DELETE the manifest file. A `Queued` manifest represents a dispatch that never acquired a global permit and never ran orchestrator logic. Rewriting it to `Failed` would mislead the user into thinking their evaluation ran and failed; deletion correctly represents "the dispatch didn't produce work." |
//! | `InProgress` | Rewrite as `Failed` with `overall_status = Failed`; every `InProgress` / `Pending` / `PendingReview` layer becomes `Failed` with `halted_by = "failed-interrupted"`. Preserves any already-completed layer results (Passed / Failed / Skipped) for audit. |
//! | Terminal (`Passed` / `Failed` / `FailedInterrupted` / `Cancelled`) | Untouched. |
//! | `PendingReview` with fully-decided gates | Untouched (terminal from gate POV). |
//!
//! The reconciler runs BEFORE the daemon accepts its first RPC —
//! clients never observe a zombie `InProgress` manifest.
//!
//! See plan Task 8 for the rationale + integration test expectations.

use std::path::{Path, PathBuf};

use anyhow::Result;
use pice_core::cli::ExitJsonStatus;
use pice_core::layers::manifest::{LayerResult, LayerStatus, ManifestStatus, VerificationManifest};

/// Marker written to `LayerResult.halted_by` (and accessible in the
/// manifest's top-level `halted_by` field via per-layer aggregation) on
/// every layer rewritten by the reconciler. CLI renderers pattern-match
/// on this exact prefix to explain why the feature ended up Failed.
pub const FAILED_INTERRUPTED: &str = ExitJsonStatus::FAILED_INTERRUPTED_HALT;

/// Summary of what the reconciler did during a single startup pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconciliationReport {
    /// Feature ids whose `Queued` manifests were deleted.
    pub discarded_queued: Vec<String>,
    /// Feature ids whose `InProgress` manifests were rewritten to
    /// `Failed` with `halted_by = "failed-interrupted"`.
    pub reconciled_interrupted: Vec<String>,
    /// Paths that could not be read/parsed. Logged at `warn` but do not
    /// block startup — a corrupt manifest is surfaced to the user via
    /// subsequent `pice status` which will show it as unloadable.
    pub unreadable: Vec<PathBuf>,
}

/// Reconcile all manifests under `state_dir`. Scans the directory tree
/// depth-2: `state_dir/{project_hash}/{feature_id}.manifest.json`.
///
/// Returns a [`ReconciliationReport`] summarizing the actions taken.
/// Must run BEFORE the daemon accepts its first RPC — callers in
/// [`crate::lifecycle::run_with_paths`] invoke this synchronously
/// during startup (before `UnixSocketListener::bind` returns Ready for
/// accepting).
pub fn reconcile_on_startup(state_dir: &Path) -> Result<ReconciliationReport> {
    let mut report = ReconciliationReport::default();

    if !state_dir.exists() {
        // No state dir → nothing to reconcile. Daemon's first dispatch
        // creates the directory tree on demand.
        return Ok(report);
    }

    let namespaces = match std::fs::read_dir(state_dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                dir = %state_dir.display(),
                error = %e,
                "reconcile_on_startup: unable to list state dir"
            );
            return Ok(report);
        }
    };

    for ns_entry in namespaces.flatten() {
        let ns_path = ns_entry.path();
        if !ns_path.is_dir() {
            continue;
        }
        let manifests = match std::fs::read_dir(&ns_path) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    dir = %ns_path.display(),
                    error = %e,
                    "reconcile_on_startup: unable to list namespace dir"
                );
                continue;
            }
        };

        for entry in manifests.flatten() {
            let path = entry.path();
            let file_name = match path.file_name().and_then(|s| s.to_str()) {
                Some(s) if s.ends_with(".manifest.json") => s.to_string(),
                _ => continue,
            };
            let feature_id = file_name.trim_end_matches(".manifest.json").to_string();

            reconcile_one(&path, &feature_id, &mut report);
        }

        // If we deleted every manifest in this namespace dir, remove
        // the namespace dir too so `pice status --list` doesn't walk
        // empty shards forever.
        if let Ok(mut it) = std::fs::read_dir(&ns_path) {
            if it.next().is_none() {
                let _ = std::fs::remove_dir(&ns_path);
            }
        }
    }

    if !report.discarded_queued.is_empty() || !report.reconciled_interrupted.is_empty() {
        tracing::info!(
            discarded = report.discarded_queued.len(),
            reconciled = report.reconciled_interrupted.len(),
            unreadable = report.unreadable.len(),
            "reconciled startup state"
        );
    }

    Ok(report)
}

fn reconcile_one(path: &Path, feature_id: &str, report: &mut ReconciliationReport) {
    let manifest = match VerificationManifest::load(path) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "reconcile_on_startup: skipping unreadable manifest"
            );
            report.unreadable.push(path.to_path_buf());
            return;
        }
    };

    match manifest.overall_status {
        ManifestStatus::Queued => {
            if let Err(e) = std::fs::remove_file(path) {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "reconcile_on_startup: failed to delete Queued manifest"
                );
                report.unreadable.push(path.to_path_buf());
                return;
            }
            report.discarded_queued.push(feature_id.to_string());
        }
        ManifestStatus::InProgress => {
            let rewritten = rewrite_interrupted(manifest);
            let saver = crate::events::NullSaver;
            if let Err(e) = crate::events::ManifestSaver::save_and_emit(
                &saver,
                &rewritten,
                path,
                crate::events::SaveIntent::Cancelled {
                    reason: FAILED_INTERRUPTED.to_string(),
                },
            ) {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "reconcile_on_startup: failed to save rewritten manifest"
                );
                report.unreadable.push(path.to_path_buf());
                return;
            }
            report.reconciled_interrupted.push(feature_id.to_string());
        }
        // Terminal states: untouched.
        ManifestStatus::Passed
        | ManifestStatus::Failed
        | ManifestStatus::FailedInterrupted
        | ManifestStatus::Pending
        | ManifestStatus::PendingReview => {
            // `Pending` here means a pre-Phase-7 manifest that the old
            // daemon left in its initial state but never dispatched.
            // Leaving it alone is consistent with previous behavior;
            // the user can `pice evaluate` to advance it.
            // `PendingReview` preserves the in-flight gate state so a
            // reviewer's decision still applies post-restart.
        }
    }
}

/// Rewrite an `InProgress` manifest to `Failed` with `halted_by =
/// "failed-interrupted"` on every non-terminal layer. Preserves
/// completed layer results (Passed / Failed / Skipped) and any gate
/// history for audit.
fn rewrite_interrupted(mut manifest: VerificationManifest) -> VerificationManifest {
    for layer in manifest.layers.iter_mut() {
        rewrite_layer_if_open(layer);
    }
    manifest.overall_status = ManifestStatus::Failed;
    manifest
}

fn rewrite_layer_if_open(layer: &mut LayerResult) {
    match layer.status {
        LayerStatus::InProgress | LayerStatus::Pending | LayerStatus::PendingReview => {
            layer.status = LayerStatus::Failed;
            layer.halted_by = Some(FAILED_INTERRUPTED.to_string());
        }
        LayerStatus::Passed | LayerStatus::Failed | LayerStatus::Skipped => {
            // Preserved as-is.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pice_core::layers::manifest::{LayerResult, PassResult, VerificationManifest};

    fn make_manifest(feature_id: &str, overall: ManifestStatus) -> VerificationManifest {
        let mut m = VerificationManifest::new(feature_id, Path::new("/tmp/project"));
        m.overall_status = overall;
        m
    }

    fn seeded_layer(name: &str, status: LayerStatus) -> LayerResult {
        LayerResult {
            name: name.to_string(),
            status,
            passes: vec![],
            seam_checks: vec![],
            halted_by: None,
            final_confidence: None,
            total_cost_usd: None,
            escalation_events: None,
        }
    }

    // ─── Unit tests ─────────────────────────────────────────────────────

    #[test]
    fn queued_manifest_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let ns = dir.path().join("ns-12hex");
        std::fs::create_dir_all(&ns).unwrap();
        let path = ns.join("feat-q.manifest.json");
        make_manifest("feat-q", ManifestStatus::Queued)
            .save(&path)
            .unwrap();

        let report = reconcile_on_startup(dir.path()).unwrap();

        assert_eq!(report.discarded_queued, vec!["feat-q".to_string()]);
        assert!(report.reconciled_interrupted.is_empty());
        assert!(!path.exists(), "Queued manifest should be deleted");
    }

    #[test]
    fn in_progress_manifest_reconciled() {
        let dir = tempfile::tempdir().unwrap();
        let ns = dir.path().join("ns-12hex");
        std::fs::create_dir_all(&ns).unwrap();
        let path = ns.join("feat-ip.manifest.json");

        let mut m = make_manifest("feat-ip", ManifestStatus::InProgress);
        m.layers = vec![
            seeded_layer("a", LayerStatus::Passed),
            seeded_layer("b", LayerStatus::InProgress),
            seeded_layer("c", LayerStatus::Pending),
        ];
        m.save(&path).unwrap();

        let report = reconcile_on_startup(dir.path()).unwrap();

        assert_eq!(report.reconciled_interrupted, vec!["feat-ip".to_string()]);
        assert!(report.discarded_queued.is_empty());

        let reloaded = VerificationManifest::load(&path).unwrap();
        assert_eq!(reloaded.overall_status, ManifestStatus::Failed);
        assert_eq!(reloaded.layers[0].status, LayerStatus::Passed); // preserved
        assert_eq!(reloaded.layers[1].status, LayerStatus::Failed);
        assert_eq!(
            reloaded.layers[1].halted_by.as_deref(),
            Some(FAILED_INTERRUPTED)
        );
        assert_eq!(reloaded.layers[2].status, LayerStatus::Failed);
        assert_eq!(
            reloaded.layers[2].halted_by.as_deref(),
            Some(FAILED_INTERRUPTED)
        );
    }

    #[test]
    fn pending_review_layer_rewritten_on_in_progress_parent() {
        // A layer in PendingReview inside an InProgress manifest is a
        // fail-closed signal: the process was mid-gate-processing when
        // it died. The gate itself survives in `manifest.gates`, but
        // the layer result is marked Failed so a subsequent
        // `pice evaluate --resume` treats the layer as needing
        // re-evaluation from pass 1.
        let dir = tempfile::tempdir().unwrap();
        let ns = dir.path().join("ns-12hex");
        std::fs::create_dir_all(&ns).unwrap();
        let path = ns.join("feat-pr.manifest.json");
        let mut m = make_manifest("feat-pr", ManifestStatus::InProgress);
        m.layers = vec![seeded_layer("a", LayerStatus::PendingReview)];
        m.save(&path).unwrap();

        let _ = reconcile_on_startup(dir.path()).unwrap();

        let reloaded = VerificationManifest::load(&path).unwrap();
        assert_eq!(reloaded.layers[0].status, LayerStatus::Failed);
        assert_eq!(
            reloaded.layers[0].halted_by.as_deref(),
            Some(FAILED_INTERRUPTED)
        );
    }

    #[test]
    fn terminal_manifests_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let ns = dir.path().join("ns-12hex");
        std::fs::create_dir_all(&ns).unwrap();

        for (feat, status) in [
            ("feat-passed", ManifestStatus::Passed),
            ("feat-failed", ManifestStatus::Failed),
            ("feat-fi", ManifestStatus::FailedInterrupted),
            ("feat-prev", ManifestStatus::PendingReview),
        ] {
            let path = ns.join(format!("{feat}.manifest.json"));
            make_manifest(feat, status).save(&path).unwrap();
        }

        let report = reconcile_on_startup(dir.path()).unwrap();
        assert!(report.discarded_queued.is_empty());
        assert!(report.reconciled_interrupted.is_empty());
        for feat in ["feat-passed", "feat-failed", "feat-fi", "feat-prev"] {
            let path = ns.join(format!("{feat}.manifest.json"));
            assert!(path.exists(), "{feat} should be preserved");
        }
    }

    #[test]
    fn mixed_state_dir_handles_each_case() {
        let dir = tempfile::tempdir().unwrap();
        let ns = dir.path().join("ns-abc123");
        std::fs::create_dir_all(&ns).unwrap();

        let queued = ns.join("q.manifest.json");
        make_manifest("q", ManifestStatus::Queued)
            .save(&queued)
            .unwrap();
        let in_progress = ns.join("ip.manifest.json");
        make_manifest("ip", ManifestStatus::InProgress)
            .save(&in_progress)
            .unwrap();
        let passed = ns.join("ok.manifest.json");
        make_manifest("ok", ManifestStatus::Passed)
            .save(&passed)
            .unwrap();

        let report = reconcile_on_startup(dir.path()).unwrap();

        assert_eq!(report.discarded_queued, vec!["q".to_string()]);
        assert_eq!(report.reconciled_interrupted, vec!["ip".to_string()]);
        assert!(!queued.exists());
        assert!(in_progress.exists()); // rewritten
        assert!(passed.exists()); // untouched

        // Reloading the InProgress one should now show Failed.
        let rewritten = VerificationManifest::load(&in_progress).unwrap();
        assert_eq!(rewritten.overall_status, ManifestStatus::Failed);
    }

    #[test]
    fn nonexistent_state_dir_reports_empty() {
        let report = reconcile_on_startup(Path::new("/tmp/absolutely-nonexistent-xyz")).unwrap();
        assert!(report.discarded_queued.is_empty());
        assert!(report.reconciled_interrupted.is_empty());
    }

    #[test]
    fn unreadable_manifest_is_logged_and_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let ns = dir.path().join("ns-12hex");
        std::fs::create_dir_all(&ns).unwrap();
        let path = ns.join("corrupt.manifest.json");
        std::fs::write(&path, b"not valid json {{{ }}}}").unwrap();

        let report = reconcile_on_startup(dir.path()).unwrap();
        assert_eq!(report.unreadable.len(), 1);
        assert!(
            path.exists(),
            "corrupt file not deleted — preserved for manual inspection"
        );
    }

    #[test]
    fn ignores_non_manifest_files() {
        // Stray `.log` / `.tmp` files in the state dir must not trip
        // the reconciler (it sees them, skips them, keeps going).
        let dir = tempfile::tempdir().unwrap();
        let ns = dir.path().join("ns-12hex");
        std::fs::create_dir_all(&ns).unwrap();
        std::fs::write(ns.join("daemon.log"), b"log contents").unwrap();
        std::fs::write(ns.join("unrelated.txt"), b"x").unwrap();

        let report = reconcile_on_startup(dir.path()).unwrap();
        assert!(report.discarded_queued.is_empty());
        assert!(report.reconciled_interrupted.is_empty());
        assert!(report.unreadable.is_empty());
    }

    #[test]
    fn rewrite_interrupted_preserves_passes_and_gates() {
        let mut m = make_manifest("feat", ManifestStatus::InProgress);
        let mut l = seeded_layer("a", LayerStatus::InProgress);
        l.passes = vec![PassResult {
            index: 1,
            model: "stub".into(),
            score: Some(9.0),
            cost_usd: None,
            timestamp: "2026-04-21T10:00:00Z".into(),
            findings: vec![],
        }];
        m.layers = vec![l];
        let out = rewrite_interrupted(m);
        assert_eq!(out.layers[0].status, LayerStatus::Failed);
        assert_eq!(
            out.layers[0].passes.len(),
            1,
            "prior passes preserved for audit"
        );
    }

    #[test]
    fn empty_namespace_dir_is_removed_after_reconcile() {
        let dir = tempfile::tempdir().unwrap();
        let ns = dir.path().join("ns-empty");
        std::fs::create_dir_all(&ns).unwrap();
        let path = ns.join("only-q.manifest.json");
        make_manifest("only-q", ManifestStatus::Queued)
            .save(&path)
            .unwrap();

        let _ = reconcile_on_startup(dir.path()).unwrap();
        assert!(!ns.exists(), "empty namespace dir should be cleaned up");
    }
}

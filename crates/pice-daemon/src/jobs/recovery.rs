//! Phase 7 Task 8: startup reconciliation for interrupted background
//! dispatches.
//!
//! When the daemon starts (cold start or post-crash restart), the state
//! directory can contain manifests left over from the previous process
//! lifetime. Three disposition rules apply, keyed on `overall_status`:
//!
//! | Prior status | Action |
//! |--------------|--------|
//! | `Queued` without layers/gates | DELETE the manifest file. A blank `Queued` manifest represents a dispatch that never acquired a global permit and never ran orchestrator logic. Rewriting it to `Failed` would mislead the user into thinking their evaluation ran and failed; deletion correctly represents "the dispatch didn't produce work." |
//! | `Queued` with layers/gates | Rewrite to `Pending`. This is a defensive repair for resume dispatches: a queued manifest with preserved work is already an audit source of truth and must not be deleted. |
//! | `InProgress` | Rewrite as `Failed` with `overall_status = Failed`; every `InProgress` / `Pending` / `PendingReview` layer becomes `Failed` with `halted_by = "failed-interrupted"`. Preserves any already-completed layer results (Passed / Failed / Skipped) for audit. |
//! | Terminal (`Passed` / `Failed` / `FailedInterrupted` / `Cancelled`) | Untouched. |
//! | `PendingReview` with fully-decided gates | Untouched (terminal from gate POV). |
//!
//! The reconciler runs BEFORE the daemon accepts its first RPC —
//! clients never observe a zombie `InProgress` manifest.
//!
//! See plan Task 8 for the rationale + integration test expectations.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
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
    /// Feature ids whose `Queued` manifests already contained layers/gates
    /// and were preserved as `Pending` instead of deleted.
    pub preserved_queued_resume: Vec<String>,
    /// Feature ids whose `InProgress` manifests were rewritten to
    /// `Failed` with `halted_by = "failed-interrupted"`.
    pub reconciled_interrupted: Vec<String>,
    /// Paths that could not be read/parsed. Startup reconciliation now fails
    /// closed on these errors so the daemon cannot accept RPCs while
    /// interrupted manifests may still be unreconciled.
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

    let namespaces = std::fs::read_dir(state_dir)
        .with_context(|| format!("unable to list state dir {}", state_dir.display()))?;

    for ns_entry in namespaces {
        let ns_entry = ns_entry.with_context(|| {
            format!("unable to read state dir entry in {}", state_dir.display())
        })?;
        let ns_path = ns_entry.path();
        if !ns_path.is_dir() {
            continue;
        }
        let manifests = std::fs::read_dir(&ns_path)
            .with_context(|| format!("unable to list namespace dir {}", ns_path.display()))?;

        for entry in manifests {
            let entry = entry.with_context(|| {
                format!("unable to read manifest entry in {}", ns_path.display())
            })?;
            let path = entry.path();
            let file_name = match path.file_name().and_then(|s| s.to_str()) {
                Some(s) if s.ends_with(".manifest.json") => s.to_string(),
                _ => continue,
            };
            let feature_id = file_name.trim_end_matches(".manifest.json").to_string();

            reconcile_one(&path, &feature_id, &mut report)?;
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

    if !report.discarded_queued.is_empty()
        || !report.preserved_queued_resume.is_empty()
        || !report.reconciled_interrupted.is_empty()
    {
        tracing::info!(
            discarded = report.discarded_queued.len(),
            preserved_queued_resume = report.preserved_queued_resume.len(),
            reconciled = report.reconciled_interrupted.len(),
            unreadable = report.unreadable.len(),
            "reconciled startup state"
        );
    }

    Ok(report)
}

fn reconcile_one(path: &Path, feature_id: &str, report: &mut ReconciliationReport) -> Result<()> {
    let manifest = VerificationManifest::load(path)
        .with_context(|| format!("unable to load manifest {}", path.display()))?;

    match manifest.overall_status {
        ManifestStatus::Queued => {
            if queued_manifest_has_resume_state(&manifest) {
                let mut preserved = manifest;
                preserved.overall_status = ManifestStatus::Pending;
                let saver = crate::events::NullSaver;
                crate::events::ManifestSaver::save_and_emit(
                    &saver,
                    &preserved,
                    path,
                    crate::events::SaveIntent::FeatureCompleted,
                )
                .with_context(|| {
                    format!(
                        "failed to preserve Queued resume manifest {}",
                        path.display()
                    )
                })?;
                report.preserved_queued_resume.push(feature_id.to_string());
            } else {
                std::fs::remove_file(path).with_context(|| {
                    format!("failed to delete Queued manifest {}", path.display())
                })?;
                report.discarded_queued.push(feature_id.to_string());
            }
        }
        ManifestStatus::InProgress => {
            let rewritten = rewrite_interrupted(manifest, feature_id);
            let saver = crate::events::NullSaver;
            crate::events::ManifestSaver::save_and_emit(
                &saver,
                &rewritten,
                path,
                crate::events::SaveIntent::Cancelled {
                    reason: FAILED_INTERRUPTED.to_string(),
                },
            )
            .with_context(|| format!("failed to save rewritten manifest {}", path.display()))?;
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
    Ok(())
}

fn queued_manifest_has_resume_state(manifest: &VerificationManifest) -> bool {
    !manifest.layers.is_empty() || !manifest.gates.is_empty()
}

/// Rewrite an `InProgress` manifest to `Failed` with `halted_by =
/// "failed-interrupted"` on every non-terminal layer. Preserves
/// completed layer results (Passed / Failed / Skipped) and any gate
/// history for audit.
fn rewrite_interrupted(
    mut manifest: VerificationManifest,
    feature_id: &str,
) -> VerificationManifest {
    for layer in manifest.layers.iter_mut() {
        rewrite_layer_if_open(layer);
    }
    if !manifest
        .layers
        .iter()
        .any(|layer| layer.halted_by.as_deref() == Some(FAILED_INTERRUPTED))
    {
        manifest.layers.push(LayerResult {
            name: feature_id.to_string(),
            status: LayerStatus::Failed,
            passes: Vec::new(),
            seam_checks: Vec::new(),
            halted_by: Some(FAILED_INTERRUPTED.to_string()),
            final_confidence: None,
            total_cost_usd: None,
            escalation_events: None,
        });
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
        assert!(report.preserved_queued_resume.is_empty());
        assert!(report.reconciled_interrupted.is_empty());
        assert!(!path.exists(), "Queued manifest should be deleted");
    }

    #[test]
    fn queued_resume_manifest_with_state_is_preserved_as_pending() {
        let dir = tempfile::tempdir().unwrap();
        let ns = dir.path().join("ns-12hex");
        std::fs::create_dir_all(&ns).unwrap();
        let path = ns.join("feat-resume.manifest.json");

        let mut m = make_manifest("feat-resume", ManifestStatus::Queued);
        m.layers = vec![seeded_layer("infrastructure", LayerStatus::Passed)];
        m.gates.push(pice_core::layers::manifest::GateEntry {
            id: "feat-resume:infrastructure:0001".to_string(),
            layer: "infrastructure".to_string(),
            status: pice_core::layers::manifest::GateStatus::Approved,
            trigger_expression: "layer == infrastructure".to_string(),
            requested_at: "2026-05-12T00:00:00Z".to_string(),
            timeout_at: "2026-05-13T00:00:00Z".to_string(),
            on_timeout_action: pice_core::workflow::schema::OnTimeout::Reject,
            reject_attempts_remaining: 0,
            decision: Some("approve".to_string()),
            decided_at: Some("2026-05-12T00:01:00Z".to_string()),
        });
        m.save(&path).unwrap();

        let report = reconcile_on_startup(dir.path()).unwrap();

        assert!(report.discarded_queued.is_empty());
        assert_eq!(
            report.preserved_queued_resume,
            vec!["feat-resume".to_string()]
        );
        assert!(path.exists(), "resume manifest should be preserved");

        let reloaded = VerificationManifest::load(&path).unwrap();
        assert_eq!(reloaded.overall_status, ManifestStatus::Pending);
        assert_eq!(reloaded.layers.len(), 1);
        assert_eq!(reloaded.gates.len(), 1);
        assert_eq!(
            reloaded.gates[0].status,
            pice_core::layers::manifest::GateStatus::Approved
        );
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
        assert!(report.preserved_queued_resume.is_empty());

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
        assert!(report.preserved_queued_resume.is_empty());
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
        assert!(report.preserved_queued_resume.is_empty());
        assert!(report.reconciled_interrupted.is_empty());
    }

    #[test]
    fn unreadable_manifest_fails_reconciliation() {
        let dir = tempfile::tempdir().unwrap();
        let ns = dir.path().join("ns-12hex");
        std::fs::create_dir_all(&ns).unwrap();
        let path = ns.join("corrupt.manifest.json");
        std::fs::write(&path, b"not valid json {{{ }}}}").unwrap();

        let err = reconcile_on_startup(dir.path()).unwrap_err();
        let rendered = format!("{err:#}");
        assert!(rendered.contains("unable to load manifest"));
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
        assert!(report.preserved_queued_resume.is_empty());
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
        let out = rewrite_interrupted(m, "feat");
        assert_eq!(out.layers[0].status, LayerStatus::Failed);
        assert_eq!(
            out.layers[0].passes.len(),
            1,
            "prior passes preserved for audit"
        );
    }

    #[test]
    fn empty_in_progress_manifest_gets_failed_interrupted_marker() {
        let out = rewrite_interrupted(
            make_manifest("feat-empty", ManifestStatus::InProgress),
            "feat-empty",
        );

        assert_eq!(out.overall_status, ManifestStatus::Failed);
        assert_eq!(out.layers.len(), 1);
        assert_eq!(out.layers[0].name, "feat-empty");
        assert_eq!(out.layers[0].status, LayerStatus::Failed);
        assert_eq!(out.layers[0].halted_by.as_deref(), Some(FAILED_INTERRUPTED));
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

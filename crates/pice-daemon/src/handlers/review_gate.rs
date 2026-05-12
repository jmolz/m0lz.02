//! `pice review-gate` handler — list pending gates or record a reviewer
//! decision.
//!
//! `List`: read-only cross-feature scan of `~/.pice/state/{project_hash}/`
//! manifests, returning every gate in `Pending` status. `PICE_STATE_DIR`
//! overrides the root for tests.
//!
//! `Decide`: acquires the per-manifest tokio + fs2 lock (the same lock
//! `evaluate` uses), loads the manifest fresh, applies audit-before-manifest
//! ordering, writes the SQLite `gate_decisions` row first, then mutates the
//! manifest and saves. The UNIQUE(gate_id) constraint on `gate_decisions`
//! gives concurrent double-decide a typed `ReviewGateConflict` error.
//!
//! See `.claude/rules/daemon.md` → "Channel ownership invariant" and
//! `.claude/plans/phase-6-review-gates.md` Task 10 for the full design.

use anyhow::Result;
use chrono::Utc;
use pice_core::cli::{
    CommandResponse, ExitJsonStatus, GateDecideResponse, GateListEntry, GateListResponse,
    ReviewGateRequest, ReviewGateSubcommand,
};
use pice_core::gate::{GateDecision, GateDecisionOutcome};
use pice_core::layers::manifest::{
    manifest_project_namespace, GateEntry, GateStatus, LayerStatus, VerificationManifest,
};
use serde_json::json;

use crate::metrics::db::MetricsDb;
use crate::metrics::store::{
    find_gate_decision_by_id, insert_gate_decision, GateDecisionRow, GateInsertError,
};
use crate::orchestrator::StreamSink;
use crate::server::router::DaemonContext;

pub async fn run(
    req: ReviewGateRequest,
    ctx: &DaemonContext,
    _sink: &dyn StreamSink,
) -> Result<CommandResponse> {
    match req.subcommand {
        ReviewGateSubcommand::List { feature_id } => run_list(ctx, feature_id, req.json).await,
        ReviewGateSubcommand::Decide {
            gate_id,
            decision,
            reviewer,
            reason,
        } => run_decide(ctx, gate_id, decision, reviewer, reason, req.json).await,
    }
}

// ─── List ──────────────────────────────────────────────────────────────────

/// Resolve the state directory for manifests. Delegates to
/// `pice_core::layers::manifest::state_dir` — the single source of
/// truth that honors `PICE_STATE_DIR` with an `~/.pice/state/`
/// fallback. Falls back to `./` on catastrophic home-dir lookup
/// failure to preserve the handler's prior behavior rather than
/// surfacing an error on list/decide.
fn resolve_state_dir() -> std::path::PathBuf {
    VerificationManifest::state_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

/// Resolve the project-scoped state subdirectory for the caller's repo.
/// List and Decide restrict their manifest walk to this directory so one
/// project cannot see or mutate gates from another project (prevents
/// split-brain where the audit row lands in repo A's `metrics.db` while
/// the manifest mutation lands in repo B). See `.claude/rules/daemon.md`
/// → "Verification manifest — source of truth" for the namespace rule.
fn resolve_project_scoped_state_dir(ctx: &DaemonContext) -> std::path::PathBuf {
    let namespace = manifest_project_namespace(ctx.project_root());
    resolve_state_dir().join(namespace)
}

async fn run_list(
    ctx: &DaemonContext,
    feature_id_filter: Option<String>,
    json_mode: bool,
) -> Result<CommandResponse> {
    let project_dir = resolve_project_scoped_state_dir(ctx);
    let mut gates: Vec<GateListEntry> = Vec::new();
    if project_dir.is_dir() {
        // project_dir = state_dir/{project_hash}/; scan only THIS project's
        // {feature_id}.manifest.json files.
        for file in std::fs::read_dir(&project_dir)
            .into_iter()
            .flatten()
            .flatten()
        {
            let path = file.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Ok(manifest) = VerificationManifest::load(&path) else {
                tracing::warn!(path = %path.display(), "skipping unreadable manifest");
                continue;
            };
            if let Some(filter) = &feature_id_filter {
                if &manifest.feature_id != filter {
                    continue;
                }
            }
            for gate in &manifest.gates {
                if gate.status != GateStatus::Pending {
                    continue;
                }
                gates.push(GateListEntry {
                    id: gate.id.clone(),
                    feature_id: manifest.feature_id.clone(),
                    layer: gate.layer.clone(),
                    trigger_expression: gate.trigger_expression.clone(),
                    requested_at: gate.requested_at.clone(),
                    timeout_at: gate.timeout_at.clone(),
                    reject_attempts_remaining: gate.reject_attempts_remaining,
                });
            }
        }
    }
    // Stable ordering for deterministic CLI output.
    gates.sort_by(|a, b| a.requested_at.cmp(&b.requested_at));

    let response = GateListResponse { gates };
    if json_mode {
        return Ok(CommandResponse::Json {
            value: serde_json::to_value(&response)?,
        });
    }
    if response.gates.is_empty() {
        return Ok(CommandResponse::Text {
            content: "No pending review gates.\n".to_string(),
        });
    }
    let mut out = String::from("Pending review gates:\n\n");
    for g in &response.gates {
        out.push_str(&format!(
            "  ⏸ {} (feature: {}, layer: {})\n      id: {}\n      trigger: {}\n      timeout_at: {}\n      reject budget: {}\n",
            g.layer, g.feature_id, g.layer, g.id, g.trigger_expression, g.timeout_at, g.reject_attempts_remaining,
        ));
    }
    Ok(CommandResponse::Text { content: out })
}

// ─── Decide ────────────────────────────────────────────────────────────────

/// Apply a manual approve / reject / skip decision to a pending gate.
/// Acquires the per-manifest lock (reusing `evaluate`'s helper), loads
/// the manifest fresh, writes the SQLite audit row FIRST, then mutates
/// and saves the manifest. Returns typed exit codes:
/// - 0 on approve / skip / reject-with-retry
/// - 2 on reject-no-retry (feature halts) — maps to `ReviewGateRejected`
/// - 1 on gate not found / already decided / CAS conflict
async fn run_decide(
    ctx: &DaemonContext,
    gate_id: String,
    decision: GateDecision,
    reviewer: String,
    reason: Option<String>,
    json_mode: bool,
) -> Result<CommandResponse> {
    // Phase 6: locate the manifest carrying this gate id. Scoped to the
    // caller's project namespace only — walking every project's state
    // would let repo A mutate repo B's manifest while writing the audit
    // row to repo A's metrics.db (split-brain). See `.claude/rules/
    // daemon.md` → "Verification manifest — source of truth".
    let project_dir = resolve_project_scoped_state_dir(ctx);
    let mut target: Option<(std::path::PathBuf, VerificationManifest)> = None;
    if project_dir.is_dir() {
        for file in std::fs::read_dir(&project_dir)
            .into_iter()
            .flatten()
            .flatten()
        {
            let path = file.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Ok(manifest) = VerificationManifest::load(&path) else {
                continue;
            };
            if manifest.gates.iter().any(|g| g.id == gate_id) {
                target = Some((path, manifest));
                break;
            }
        }
    }
    let Some((manifest_path, _manifest_preview)) = target else {
        return Ok(decide_failure_response(
            json_mode,
            ExitJsonStatus::ReviewGateConflict,
            format!("no pending gate found with id '{gate_id}'"),
            1,
        ));
    };

    // Acquire the per-manifest lock. `manifest_lock_for` keys on
    // (project_namespace, feature_id); reuse the evaluate handler's
    // invariant that lock identity must match the on-disk manifest
    // path. The 12-char project hash is the parent directory name.
    let feature_id = manifest_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        // `{feature_id}.manifest` — strip the trailing `.manifest` component.
        .trim_end_matches(".manifest")
        .to_string();
    let project_namespace = manifest_path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let lock = ctx.manifest_lock_for(&project_namespace, &feature_id);
    let _lock_guard = lock.lock().await;

    // fs2 file lock for cross-process serialization (mirrors evaluate.rs).
    let _file_lock = {
        let lock_path = manifest_path.with_extension("manifest.lock");
        let feature_id_clone = feature_id.clone();
        tokio::task::spawn_blocking(move || {
            use fs2::FileExt;
            use std::fs::OpenOptions;
            let lock_path_inner = lock_path;
            if let Some(parent) = lock_path_inner.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&lock_path_inner)?;
            file.lock_exclusive()?;
            tracing::debug!(feature_id = %feature_id_clone, "review-gate acquired manifest fs2 lock");
            anyhow::Ok(file)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking joined with error: {e}"))??
    };

    // Reload the manifest fresh from disk under both locks.
    let mut manifest = VerificationManifest::load(&manifest_path)?;

    // Find the gate by id.
    let gate_index = match manifest.gates.iter().position(|g| g.id == gate_id) {
        Some(idx) => idx,
        None => {
            return Ok(decide_failure_response(
                json_mode,
                ExitJsonStatus::ReviewGateConflict,
                format!("gate '{gate_id}' not present in manifest (race)"),
                1,
            ));
        }
    };
    if manifest.gates[gate_index].status != GateStatus::Pending {
        return Ok(decide_failure_response(
            json_mode,
            ExitJsonStatus::ReviewGateConflict,
            format!(
                "gate '{gate_id}' already decided (status: {:?})",
                manifest.gates[gate_index].status
            ),
            1,
        ));
    }

    // Crash-recovery idempotency (Phase 6 evaluation fix): if a prior
    // decide succeeded at the audit insert but crashed before the
    // manifest save landed, the next decide attempt will find the gate
    // still `Pending` on disk BUT an audit row already present in
    // SQLite. Detect that case up front and re-derive the outcome from
    // the durable audit row so we can complete the manifest mutation
    // with the SAME decision the user originally approved — the audit
    // row is the source of truth, not the caller's new request.
    //
    // Detection happens here (before timeout-prelude / manual-decision
    // computation) so we use the recovered outcome consistently in the
    // mutation block below.
    let now = Utc::now();
    let db_path = ctx.project_root().join(".pice").join("metrics.db");
    let prior_audit: Option<crate::metrics::store::GateDecisionRecord> = if db_path.is_file() {
        let db = MetricsDb::open(&db_path)?;
        find_gate_decision_by_id(&db, &gate_id)?
    } else {
        None
    };
    let (outcome, recovery_audit_id): (GateDecisionOutcome, Option<i64>) = if let Some(ref prior) =
        prior_audit
    {
        match GateDecisionOutcome::from_audit_decision_string(&prior.decision) {
            Some(rec) => {
                // Recovery is ONLY legal when the caller's requested
                // decision matches the durable audit row. A mismatch
                // means two actors raced with DIFFERENT intents —
                // silently overwriting the caller's intent would be
                // user-hostile. Surface the UNIQUE CAS as a conflict
                // (preserves contract criterion 10 semantics) while
                // still allowing idempotent crash-recovery when the
                // same actor retries with the same decision.
                let caller_manual = GateDecisionOutcome::manual(decision);
                if rec.audit_decision_string() != caller_manual.audit_decision_string() {
                    tracing::info!(
                        gate_id = %gate_id,
                        prior_decision = %prior.decision,
                        caller_decision = %caller_manual.audit_decision_string(),
                        "decision mismatch on re-decide; surfacing ReviewGateConflict"
                    );
                    return Ok(decide_failure_response(
                        json_mode,
                        ExitJsonStatus::ReviewGateConflict,
                        format!(
                            "gate '{gate_id}' already decided as '{}' by prior actor (requested: '{}')",
                            prior.decision,
                            caller_manual.audit_decision_string()
                        ),
                        1,
                    ));
                }
                tracing::info!(
                    gate_id = %gate_id,
                    prior_decision = %prior.decision,
                    audit_id = prior.id,
                    "recovering from prior audit row; completing manifest mutation idempotently"
                );
                (rec, Some(prior.id))
            }
            None => {
                tracing::error!(
                    gate_id = %gate_id,
                    prior_decision = %prior.decision,
                    "audit row has unrecognized decision string; refusing to recover"
                );
                return Ok(decide_failure_response(
                    json_mode,
                    ExitJsonStatus::ReviewGateConflict,
                    format!(
                        "gate '{gate_id}' has audit row with unrecognized decision '{}'",
                        prior.decision
                    ),
                    1,
                ));
            }
        }
    } else {
        // No prior audit row — normal path. Apply timeout prelude (if
        // expired) or the caller's manual decision.
        let gate = &mut manifest.gates[gate_index];
        let computed = if let Some(timeout_outcome) =
            pice_core::gate::apply_timeout_if_expired(gate, gate.on_timeout_action, now)
        {
            timeout_outcome
        } else {
            GateDecisionOutcome::manual(decision)
        };
        (computed, None)
    };

    // Pre-compute manifest mutations without writing. Snapshot gate
    // fields we'll need below — the gate reference is no longer alive
    // after the recovery-vs-normal branch above.
    let current_reject_budget = manifest.gates[gate_index].reject_attempts_remaining;
    let gate_layer = manifest.gates[gate_index].layer.clone();
    let gate_requested_at = manifest.gates[gate_index].requested_at.clone();
    let gate_trigger = manifest.gates[gate_index].trigger_expression.clone();
    let feature_id_for_gate = manifest.feature_id.clone();
    let elapsed_seconds = match chrono::DateTime::parse_from_rfc3339(&gate_requested_at) {
        Ok(dt) => (now.signed_duration_since(dt.with_timezone(&Utc)))
            .num_seconds()
            .max(0),
        Err(_) => 0,
    };

    let (new_gate_status, new_layer_status, new_reject_budget, new_halted_by): (
        GateStatus,
        LayerStatus,
        u32,
        Option<String>,
    ) = match outcome.decision {
        GateDecision::Approve => (
            GateStatus::Approved,
            LayerStatus::Passed,
            current_reject_budget,
            None,
        ),
        GateDecision::Skip => (
            // Skip keeps `LayerStatus::Passed` (contract criterion 6).
            GateStatus::Skipped,
            LayerStatus::Passed,
            current_reject_budget,
            None,
        ),
        GateDecision::Reject => {
            if current_reject_budget > 0 {
                (
                    GateStatus::Rejected,
                    LayerStatus::Pending,
                    current_reject_budget - 1,
                    None,
                )
            } else {
                (
                    GateStatus::Rejected,
                    LayerStatus::Failed,
                    0,
                    Some(ExitJsonStatus::HALTED_GATE_REJECTED.to_string()),
                )
            }
        }
    };

    // Step 5c: write audit row FIRST — unless we're in crash-recovery
    // mode (prior audit row already durable; reuse its id).
    let audit_id = if let Some(existing_id) = recovery_audit_id {
        existing_id
    } else if db_path.is_file() {
        let db = MetricsDb::open(&db_path)?;
        let decision_str = outcome.audit_decision_string();
        let row = GateDecisionRow {
            gate_id: &gate_id,
            feature_id: &feature_id_for_gate,
            layer: &gate_layer,
            trigger_expression: &gate_trigger,
            decision: decision_str,
            reviewer: Some(reviewer.as_str()),
            reason: reason.as_deref(),
            requested_at: &gate_requested_at,
            decided_at: &now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            elapsed_seconds,
        };
        match insert_gate_decision(&db, &row) {
            Ok(id) => id,
            Err(GateInsertError::DuplicateGateId { .. }) => {
                // Lost a concurrent race AND `find_gate_decision_by_id`
                // didn't see the row at the top of this handler (another
                // decide landed between our read and our write). If the
                // winner's decision matches ours → recover idempotently.
                // Otherwise → surface a conflict (contract criterion 10).
                let db2 = MetricsDb::open(&db_path)?;
                match find_gate_decision_by_id(&db2, &gate_id)? {
                    Some(prior) => {
                        if prior.decision != decision_str {
                            tracing::info!(
                                gate_id = %gate_id,
                                prior_decision = %prior.decision,
                                caller_decision = %decision_str,
                                "decision mismatch on UNIQUE race; surfacing ReviewGateConflict"
                            );
                            return Ok(decide_failure_response(
                                json_mode,
                                ExitJsonStatus::ReviewGateConflict,
                                format!(
                                    "gate '{gate_id}' already decided as '{}' by concurrent \
                                     actor (requested: '{decision_str}')",
                                    prior.decision
                                ),
                                1,
                            ));
                        }
                        tracing::info!(
                            gate_id = %gate_id,
                            prior_decision = %prior.decision,
                            audit_id = prior.id,
                            "race-loser on audit insert; recovering from winner's row (same decision)"
                        );
                        prior.id
                    }
                    None => {
                        return Ok(decide_failure_response(
                            json_mode,
                            ExitJsonStatus::ReviewGateConflict,
                            format!(
                                "gate '{gate_id}' UNIQUE violated but row not found on re-read"
                            ),
                            1,
                        ));
                    }
                }
            }
            Err(GateInsertError::Other(e)) => {
                return Ok(decide_failure_response(
                    json_mode,
                    ExitJsonStatus::MetricsPersistFailed,
                    format!("failed to write gate audit row: {e}"),
                    1,
                ));
            }
        }
    } else {
        // No metrics DB — projection allowed (e.g. `pice init` not run).
        0
    };

    // Step 5d: apply precomputed manifest mutations.
    let decided_at_string = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    {
        let gate_mut = &mut manifest.gates[gate_index];
        gate_mut.status = new_gate_status;
        gate_mut.decision = Some(outcome.audit_decision_string().to_string());
        gate_mut.decided_at = Some(decided_at_string.clone());
        gate_mut.reject_attempts_remaining = new_reject_budget;
    }
    if let Some(layer) = manifest.layers.iter_mut().find(|l| l.name == gate_layer) {
        layer.status = new_layer_status.clone();
        if let Some(halted) = &new_halted_by {
            layer.halted_by = Some(halted.clone());
        }
    }
    manifest.compute_overall_status();
    // Phase 7: route the post-audit manifest save through the
    // EventEmittingSaver so `manifest/subscribe` consumers see a
    // `GateDecided` event with `{gate_id, decision}` in the payload.
    // The audit row is already in SQLite (step 5c above) — the event
    // order mirrors the audit-before-manifest rule: audit write, then
    // the (save + emit) couple.
    let decide_saver = crate::events::EventEmittingSaver::new(ctx.events());
    crate::events::ManifestSaver::save_and_emit(
        &decide_saver,
        &manifest,
        &manifest_path,
        crate::events::SaveIntent::GateDecided {
            gate_id: gate_id.clone(),
            layer: gate_layer.clone(),
            decision: outcome.audit_decision_string().to_string(),
        },
    )?;

    // Compute remaining pending gates AFTER the mutation.
    let pending_gates: Vec<GateEntry> = manifest
        .gates
        .iter()
        .filter(|g| g.status == GateStatus::Pending)
        .cloned()
        .collect();

    // Response construction.
    let layer_status = new_layer_status.clone();
    let manifest_status = manifest.overall_status.clone();
    let response = GateDecideResponse {
        decision: outcome.audit_decision_string().to_string(),
        layer_status: layer_status.clone(),
        manifest_status: manifest_status.clone(),
        reject_attempts_remaining: new_reject_budget,
        pending_gates,
        audit_id,
    };

    // Exit code: reject-no-retry → 2 (feature halted); everything else → 0.
    let exit_code = if matches!(new_halted_by.as_deref(), Some(h) if ExitJsonStatus::is_gate_halt(h))
    {
        2
    } else {
        0
    };

    if json_mode {
        let mut value = serde_json::to_value(&response)?;
        if let Some(obj) = value.as_object_mut() {
            let status = if exit_code == 2 {
                ExitJsonStatus::ReviewGateRejected.as_str()
            } else {
                "ok"
            };
            obj.insert("status".to_string(), json!(status));
        }
        if exit_code == 0 {
            return Ok(CommandResponse::Json { value });
        }
        return Ok(CommandResponse::ExitJson {
            code: exit_code,
            value,
        });
    }

    let decision_str = outcome.audit_decision_string();
    let body = format!(
        "Decision '{decision_str}' recorded for gate {gate_id}\n\
         Layer: {gate_layer} → {layer_status:?}\n\
         Manifest: {manifest_status:?}\n\
         Reject budget remaining: {new_reject_budget}\n",
    );
    if exit_code == 2 {
        return Ok(CommandResponse::Exit {
            code: 2,
            message: body,
        });
    }
    Ok(CommandResponse::Text { content: body })
}

fn decide_failure_response(
    json_mode: bool,
    status: ExitJsonStatus,
    message: String,
    code: i32,
) -> CommandResponse {
    if json_mode {
        CommandResponse::ExitJson {
            code,
            value: json!({
                "status": status.as_str(),
                "error": message,
            }),
        }
    } else {
        CommandResponse::Exit {
            code,
            message: format!("{}: {message}\n", status.as_str()),
        }
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use pice_core::layers::manifest::{
        GateStatus as GS, LayerResult, LayerStatus as LS, ManifestStatus as MS,
    };
    use tempfile::TempDir;

    // Shared via `pice_daemon::test_support` so the struct definition
    // can't drift between this inline test and the integration-test
    // binary under `tests/review_gate_lifecycle_integration.rs`. Each
    // binary still gets its own static `Mutex<()>` — which is correct
    // since cargo-test binaries run in separate processes.
    use crate::test_support::StateDirGuard;

    /// Build a temp project root + state dir + populated manifest fixture
    /// with a single pending gate on layer "infrastructure".
    /// Caller holds the returned guard for the duration of the test to
    /// keep PICE_STATE_DIR pinned.
    fn seeded_fixture() -> (TempDir, DaemonContext, String, StateDirGuard<'static>) {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path().to_path_buf();
        std::fs::create_dir_all(project_root.join(".pice")).unwrap();
        // PICE_STATE_DIR override so the handler sees our manifest path.
        let state_dir = project_root.join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let guard = StateDirGuard::new(&state_dir);
        // Phase 6 eval fix: project-scoped list/decide keys on the hash
        // computed from project_root, NOT a hardcoded string. Seed the
        // manifest under the same hash the handler will look up.
        let namespace = pice_core::layers::manifest::manifest_project_namespace(&project_root);
        std::fs::create_dir_all(state_dir.join(&namespace)).unwrap();
        let manifest_path = state_dir.join(&namespace).join("feat.manifest.json");

        let now = Utc::now();
        let gate = GateEntry {
            id: "feat:infrastructure:0001".to_string(),
            layer: "infrastructure".to_string(),
            status: GS::Pending,
            trigger_expression: "layer == infrastructure".to_string(),
            requested_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            timeout_at: (now + chrono::Duration::hours(24))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            on_timeout_action: pice_core::workflow::schema::OnTimeout::Reject,
            reject_attempts_remaining: 1,
            decision: None,
            decided_at: None,
        };
        let m = VerificationManifest {
            schema_version: pice_core::layers::manifest::SCHEMA_VERSION.to_string(),
            feature_id: "feat".to_string(),
            project_root_hash: namespace.clone(),
            layers: vec![LayerResult {
                name: "infrastructure".to_string(),
                status: LS::PendingReview,
                passes: Vec::new(),
                seam_checks: Vec::new(),
                halted_by: None,
                final_confidence: Some(0.95),
                total_cost_usd: Some(0.01),
                escalation_events: None,
            }],
            gates: vec![gate.clone()],
            overall_status: MS::PendingReview,
            run_id: None,
        };
        // Test fixture: route through `NullSaver` so this file contains
        // zero raw low-level save call sites (Task 9 grep-coverage
        // invariant).
        let saver = crate::events::NullSaver;
        crate::events::ManifestSaver::save_and_emit(
            &saver,
            &m,
            &manifest_path,
            crate::events::SaveIntent::FeatureCompleted,
        )
        .unwrap();

        let ctx = DaemonContext::new_for_test_with_root("tok", project_root);
        // Open + migrate the metrics DB so the audit insert path works.
        let db = MetricsDb::open(&ctx.project_root().join(".pice").join("metrics.db")).unwrap();
        drop(db);
        (tmp, ctx, gate.id, guard)
    }

    #[tokio::test]
    async fn decide_approve_records_audit_and_updates_manifest() {
        let (_tmp, ctx, gate_id, _state_guard) = seeded_fixture();
        let req = ReviewGateRequest {
            json: true,
            subcommand: ReviewGateSubcommand::Decide {
                gate_id: gate_id.clone(),
                decision: GateDecision::Approve,
                reviewer: "jacob".to_string(),
                reason: None,
            },
        };
        let resp = run(req, &ctx, &crate::orchestrator::NullSink)
            .await
            .unwrap();
        match resp {
            CommandResponse::Json { value } => {
                assert_eq!(value["decision"], "approve");
                assert_eq!(value["layer_status"], "passed");
            }
            other => panic!("expected Json, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn decide_skip_keeps_layer_passed_records_audit() {
        let (_tmp, ctx, gate_id, _state_guard) = seeded_fixture();
        let req = ReviewGateRequest {
            json: true,
            subcommand: ReviewGateSubcommand::Decide {
                gate_id,
                decision: GateDecision::Skip,
                reviewer: "jacob".to_string(),
                reason: Some("experimental layer".to_string()),
            },
        };
        let resp = run(req, &ctx, &crate::orchestrator::NullSink)
            .await
            .unwrap();
        if let CommandResponse::Json { value } = resp {
            // Contract criterion 6: skip keeps the LAYER status Passed.
            assert_eq!(value["decision"], "skip");
            assert_eq!(value["layer_status"], "passed");
        } else {
            panic!("expected Json response");
        }
    }

    #[tokio::test]
    async fn decide_reject_with_retry_decrements_counter_layer_returns_pending() {
        let (_tmp, ctx, gate_id, _state_guard) = seeded_fixture();
        let req = ReviewGateRequest {
            json: true,
            subcommand: ReviewGateSubcommand::Decide {
                gate_id,
                decision: GateDecision::Reject,
                reviewer: "jacob".to_string(),
                reason: Some("needs infra review".to_string()),
            },
        };
        let resp = run(req, &ctx, &crate::orchestrator::NullSink)
            .await
            .unwrap();
        if let CommandResponse::Json { value } = resp {
            assert_eq!(value["decision"], "reject");
            assert_eq!(value["layer_status"], "pending");
            // Seed counter was 1; reject with retry → 0.
            assert_eq!(value["reject_attempts_remaining"], 0);
        } else {
            panic!("expected Json response");
        }
    }

    #[tokio::test]
    async fn decide_reject_without_retry_halts_with_gate_rejected() {
        // Same fixture but we mutate the seeded gate to have 0 retries.
        let (_tmp, ctx, gate_id, _state_guard) = seeded_fixture();
        // Load and rewrite retry_count=0.
        let state_dir = std::path::PathBuf::from(std::env::var("PICE_STATE_DIR").unwrap());
        let namespace = pice_core::layers::manifest::manifest_project_namespace(ctx.project_root());
        let manifest_path = state_dir.join(&namespace).join("feat.manifest.json");
        let mut m = VerificationManifest::load(&manifest_path).unwrap();
        m.gates[0].reject_attempts_remaining = 0;
        // Test fixture: route through `NullSaver` (Task 9 grep-coverage
        // invariant).
        let saver = crate::events::NullSaver;
        crate::events::ManifestSaver::save_and_emit(
            &saver,
            &m,
            &manifest_path,
            crate::events::SaveIntent::FeatureCompleted,
        )
        .unwrap();

        let req = ReviewGateRequest {
            json: true,
            subcommand: ReviewGateSubcommand::Decide {
                gate_id,
                decision: GateDecision::Reject,
                reviewer: "jacob".to_string(),
                reason: Some("final".to_string()),
            },
        };
        let resp = run(req, &ctx, &crate::orchestrator::NullSink)
            .await
            .unwrap();
        if let CommandResponse::ExitJson { code, value } = resp {
            assert_eq!(code, 2, "reject-no-retry halts with exit 2");
            assert_eq!(value["decision"], "reject");
            assert_eq!(value["layer_status"], "failed");
            assert_eq!(value["status"], ExitJsonStatus::ReviewGateRejected.as_str());
        } else {
            panic!("expected ExitJson");
        }
    }

    #[tokio::test]
    async fn decide_on_already_decided_gate_returns_review_gate_conflict() {
        let (_tmp, ctx, gate_id, _state_guard) = seeded_fixture();
        // First decide: approve, succeeds.
        let first = ReviewGateRequest {
            json: true,
            subcommand: ReviewGateSubcommand::Decide {
                gate_id: gate_id.clone(),
                decision: GateDecision::Approve,
                reviewer: "jacob".to_string(),
                reason: None,
            },
        };
        let _ok = run(first, &ctx, &crate::orchestrator::NullSink)
            .await
            .unwrap();
        // Second decide: same gate_id, must collide.
        let second = ReviewGateRequest {
            json: true,
            subcommand: ReviewGateSubcommand::Decide {
                gate_id,
                decision: GateDecision::Approve,
                reviewer: "jacob".to_string(),
                reason: None,
            },
        };
        let resp = run(second, &ctx, &crate::orchestrator::NullSink)
            .await
            .unwrap();
        if let CommandResponse::ExitJson { code, value } = resp {
            assert_eq!(code, 1);
            assert_eq!(value["status"], ExitJsonStatus::ReviewGateConflict.as_str());
        } else {
            panic!("expected ExitJson, got {resp:?}");
        }
    }

    #[tokio::test]
    async fn decide_unique_violation_on_gate_id_surfaces_as_conflict() {
        // Contract criterion 10: the SQLite UNIQUE(gate_id) constraint
        // is the CAS primitive — a concurrent second decide with the
        // SAME gate_id but a DIFFERENT decision must receive a typed
        // ReviewGateConflict. Phase 6 eval refinement: matching
        // decisions trigger idempotent crash-recovery (a separate
        // test, `decide_same_decision_recovers_idempotently`);
        // mismatched decisions must still fail loudly.
        let (_tmp, ctx, gate_id, _state_guard) = seeded_fixture();
        // Pre-seed an audit row for this gate_id (simulating a concurrent
        // winner who decided "reject").
        let db = MetricsDb::open(&ctx.project_root().join(".pice").join("metrics.db")).unwrap();
        insert_gate_decision(
            &db,
            &GateDecisionRow {
                gate_id: &gate_id,
                feature_id: "feat",
                layer: "infrastructure",
                trigger_expression: "layer == infrastructure",
                decision: "reject", // winner rejected
                reviewer: Some("winner"),
                reason: None,
                requested_at: "2026-04-20T00:00:00Z",
                decided_at: "2026-04-20T00:00:01Z",
                elapsed_seconds: 1,
            },
        )
        .unwrap();
        drop(db);

        // Caller tries to approve — MISMATCH with durable audit.
        let req = ReviewGateRequest {
            json: true,
            subcommand: ReviewGateSubcommand::Decide {
                gate_id,
                decision: GateDecision::Approve,
                reviewer: "late-caller".to_string(),
                reason: None,
            },
        };
        let resp = run(req, &ctx, &crate::orchestrator::NullSink)
            .await
            .unwrap();
        if let CommandResponse::ExitJson { code, value } = resp {
            assert_eq!(code, 1);
            assert_eq!(
                value["status"],
                ExitJsonStatus::ReviewGateConflict.as_str(),
                "mismatched decision on UNIQUE CAS must route to ReviewGateConflict"
            );
        } else {
            panic!("expected ExitJson, got {resp:?}");
        }
    }

    /// Phase 6 eval idempotency coverage: a retry with the SAME
    /// decision as the durable audit row successfully completes the
    /// manifest mutation (crash-recovery path). The audit row is NOT
    /// duplicated — reuse via `find_gate_decision_by_id`.
    #[tokio::test]
    async fn decide_same_decision_recovers_idempotently() {
        let (_tmp, ctx, gate_id, _state_guard) = seeded_fixture();
        // Pre-seed an audit row for this gate_id (simulating a prior
        // decide that crashed between insert and manifest save).
        let db = MetricsDb::open(&ctx.project_root().join(".pice").join("metrics.db")).unwrap();
        let prior_id = insert_gate_decision(
            &db,
            &GateDecisionRow {
                gate_id: &gate_id,
                feature_id: "feat",
                layer: "infrastructure",
                trigger_expression: "layer == infrastructure",
                decision: "approve",
                reviewer: Some("original"),
                reason: None,
                requested_at: "2026-04-20T00:00:00Z",
                decided_at: "2026-04-20T00:00:01Z",
                elapsed_seconds: 1,
            },
        )
        .unwrap();
        drop(db);

        // Retry with the SAME decision — should recover idempotently.
        let req = ReviewGateRequest {
            json: true,
            subcommand: ReviewGateSubcommand::Decide {
                gate_id: gate_id.clone(),
                decision: GateDecision::Approve,
                reviewer: "retry".to_string(),
                reason: None,
            },
        };
        let resp = run(req, &ctx, &crate::orchestrator::NullSink)
            .await
            .unwrap();
        match resp {
            CommandResponse::Json { value } => {
                assert_eq!(value["decision"], "approve");
                assert_eq!(value["layer_status"], "passed");
                // The audit_id in the response matches the prior row
                // (no new audit row inserted).
                assert_eq!(
                    value["audit_id"].as_i64().unwrap(),
                    prior_id,
                    "idempotent recovery must reuse the prior audit_id"
                );
            }
            other => panic!("expected Json success on idempotent recovery, got {other:?}"),
        }

        // Assert only ONE audit row exists for this gate_id.
        let db = MetricsDb::open(&ctx.project_root().join(".pice").join("metrics.db")).unwrap();
        let rows = crate::metrics::store::query_gate_decisions(
            &db,
            &crate::metrics::store::GateDecisionsFilter {
                feature_id: Some("feat".to_string()),
                since: None,
                limit: None,
            },
        )
        .unwrap();
        assert_eq!(rows.len(), 1, "recovery must not duplicate audit rows");

        // Manifest mutation DID land (status != Pending).
        let state_dir = std::path::PathBuf::from(std::env::var("PICE_STATE_DIR").unwrap());
        let namespace = pice_core::layers::manifest::manifest_project_namespace(ctx.project_root());
        let manifest =
            VerificationManifest::load(&state_dir.join(&namespace).join("feat.manifest.json"))
                .unwrap();
        assert_ne!(
            manifest.gates[0].status,
            GS::Pending,
            "idempotent recovery must complete the manifest mutation"
        );
    }

    #[tokio::test]
    async fn decide_writes_audit_before_manifest_on_success() {
        // Contract criterion 8: audit row landed BEFORE manifest mutation.
        // We assert by querying the gate_decisions table after decide
        // and confirming one row exists, then reading the manifest back
        // and confirming gate.decided_at was written.
        let (_tmp, ctx, gate_id, _state_guard) = seeded_fixture();
        let req = ReviewGateRequest {
            json: true,
            subcommand: ReviewGateSubcommand::Decide {
                gate_id: gate_id.clone(),
                decision: GateDecision::Approve,
                reviewer: "jacob".to_string(),
                reason: None,
            },
        };
        let _ = run(req, &ctx, &crate::orchestrator::NullSink)
            .await
            .unwrap();

        // Audit: exactly one row, matching the gate_id.
        let db = MetricsDb::open(&ctx.project_root().join(".pice").join("metrics.db")).unwrap();
        let rows = crate::metrics::store::query_gate_decisions(
            &db,
            &crate::metrics::store::GateDecisionsFilter {
                feature_id: Some("feat".to_string()),
                since: None,
                limit: None,
            },
        )
        .unwrap();
        assert_eq!(rows.len(), 1, "exactly one audit row");
        assert_eq!(rows[0].gate_id, gate_id);

        // Manifest: decided_at populated, status moved off Pending.
        let state_dir = std::path::PathBuf::from(std::env::var("PICE_STATE_DIR").unwrap());
        let namespace = pice_core::layers::manifest::manifest_project_namespace(ctx.project_root());
        let manifest =
            VerificationManifest::load(&state_dir.join(&namespace).join("feat.manifest.json"))
                .unwrap();
        assert!(manifest.gates[0].decided_at.is_some());
        assert_ne!(manifest.gates[0].status, GS::Pending);
    }

    #[tokio::test]
    async fn decide_audit_failure_does_not_mutate_manifest() {
        // Contract criterion 8 inverse: when the SQLite insert fails
        // (simulated by deleting the DB after seed), the handler returns
        // MetricsPersistFailed and the manifest's gate stays Pending.
        let (_tmp, ctx, gate_id, _state_guard) = seeded_fixture();
        // Break the DB by truncating the file to non-SQLite bytes.
        let db_path = ctx.project_root().join(".pice").join("metrics.db");
        std::fs::write(&db_path, b"corrupt-not-sqlite").unwrap();

        let req = ReviewGateRequest {
            json: true,
            subcommand: ReviewGateSubcommand::Decide {
                gate_id: gate_id.clone(),
                decision: GateDecision::Approve,
                reviewer: "jacob".to_string(),
                reason: None,
            },
        };
        let resp = run(req, &ctx, &crate::orchestrator::NullSink).await;
        // Corrupt DB surfaces as an error from MetricsDb::open before
        // ever reaching the manifest mutation path.
        assert!(
            resp.is_err(),
            "corrupt metrics DB must fail-close the decide path"
        );

        // Manifest unchanged: gate still Pending, decided_at not set.
        let state_dir = std::path::PathBuf::from(std::env::var("PICE_STATE_DIR").unwrap());
        let namespace = pice_core::layers::manifest::manifest_project_namespace(ctx.project_root());
        let manifest =
            VerificationManifest::load(&state_dir.join(&namespace).join("feat.manifest.json"))
                .unwrap();
        assert_eq!(
            manifest.gates[0].status,
            GS::Pending,
            "gate must remain Pending when audit write fails"
        );
        assert!(manifest.gates[0].decided_at.is_none());
    }

    /// Contract criterion 8 sharpened coverage (Phase 6 eval fix): the
    /// PRIOR test covered DB-OPEN failure. This one covers INSERT failure
    /// AFTER a successful open — the narrower invariant the evaluator
    /// flagged. We make the DB file read-only after seeding so the v4
    /// migration + PRAGMA reads succeed but the INSERT statement fails
    /// with "readonly database".
    #[cfg(unix)]
    #[tokio::test]
    async fn decide_audit_insert_failure_preserves_manifest_state() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, ctx, gate_id, _state_guard) = seeded_fixture();
        let db_path = ctx.project_root().join(".pice").join("metrics.db");
        // Ensure the DB is fully initialized (migrations applied) before
        // we flip read-only. Seeded fixture already opened + closed it.
        let mut perms = std::fs::metadata(&db_path).unwrap().permissions();
        perms.set_mode(0o444);
        std::fs::set_permissions(&db_path, perms).unwrap();
        // Also need to flip the parent directory so SQLite can't create
        // a journal/WAL file that would otherwise let the write succeed.
        let pice_dir = ctx.project_root().join(".pice");
        let mut dir_perms = std::fs::metadata(&pice_dir).unwrap().permissions();
        dir_perms.set_mode(0o555);
        std::fs::set_permissions(&pice_dir, dir_perms).unwrap();

        let req = ReviewGateRequest {
            json: true,
            subcommand: ReviewGateSubcommand::Decide {
                gate_id: gate_id.clone(),
                decision: GateDecision::Approve,
                reviewer: "jacob".to_string(),
                reason: None,
            },
        };
        let resp = run(req, &ctx, &crate::orchestrator::NullSink).await;

        // Restore permissions so TempDir cleanup succeeds.
        let mut perms = std::fs::metadata(&db_path).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&db_path, perms).unwrap();
        let mut dir_perms = std::fs::metadata(&pice_dir).unwrap().permissions();
        dir_perms.set_mode(0o755);
        std::fs::set_permissions(&pice_dir, dir_perms).unwrap();

        // Either the insert path surfaces MetricsPersistFailed, or the
        // open path fails with a readonly-db error. Both are acceptable
        // — the invariant is "manifest unchanged on audit failure".
        match resp {
            Ok(CommandResponse::ExitJson { code: 1, value }) => {
                assert!(
                    value["status"] == "metrics-persist-failed"
                        || value["status"] == "review-gate-conflict",
                    "expected metrics-persist-failed or review-gate-conflict, got {value:?}"
                );
            }
            Err(_) => {
                // anyhow bubbled from MetricsDb::open on readonly parent —
                // also acceptable for the "audit failure" class.
            }
            other => panic!("expected failure response, got {other:?}"),
        }

        // Critical invariant: the manifest was NOT mutated by this
        // failed call — the gate is still Pending.
        let state_dir = std::path::PathBuf::from(std::env::var("PICE_STATE_DIR").unwrap());
        let namespace = pice_core::layers::manifest::manifest_project_namespace(ctx.project_root());
        let manifest =
            VerificationManifest::load(&state_dir.join(&namespace).join("feat.manifest.json"))
                .unwrap();
        assert_eq!(
            manifest.gates[0].status,
            GS::Pending,
            "gate must remain Pending when audit insert fails mid-transaction"
        );
        assert!(manifest.gates[0].decided_at.is_none());
    }

    #[tokio::test]
    async fn list_returns_pending_gates_across_features() {
        let (_tmp, ctx, _gate_id, _state_guard) = seeded_fixture();
        let req = ReviewGateRequest {
            json: true,
            subcommand: ReviewGateSubcommand::List { feature_id: None },
        };
        let resp = run(req, &ctx, &crate::orchestrator::NullSink)
            .await
            .unwrap();
        if let CommandResponse::Json { value } = resp {
            let gates = value["gates"].as_array().expect("gates array");
            assert!(!gates.is_empty(), "list must surface the seeded gate");
        } else {
            panic!("expected Json");
        }
    }
}

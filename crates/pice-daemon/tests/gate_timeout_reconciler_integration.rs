//! Phase 7 contract target for review-gate timeout reconciliation.
//!
//! The plan names this integration surface explicitly: an expired pending
//! gate must write the timeout decision to the SQLite audit trail and then
//! mutate the manifest state according to the pinned `on_timeout` action.

use chrono::{Duration, SecondsFormat, Utc};
use pice_core::cli::ExitJsonStatus;
use pice_core::layers::manifest::{
    GateEntry, GateStatus, LayerResult, LayerStatus, ManifestStatus, VerificationManifest,
};
use pice_core::workflow::schema::OnTimeout;
use pice_daemon::handlers::evaluate::reconcile_expired_gates_inline;
use pice_daemon::metrics::db::MetricsDb;
use pice_daemon::metrics::store::{query_gate_decisions, GateDecisionsFilter};

fn layer(name: &str, status: LayerStatus) -> LayerResult {
    LayerResult {
        name: name.to_string(),
        status,
        passes: Vec::new(),
        seam_checks: Vec::new(),
        halted_by: None,
        final_confidence: None,
        total_cost_usd: None,
        escalation_events: None,
    }
}

#[test]
fn expired_reject_gate_writes_timeout_audit_and_fails_layer() {
    let project = tempfile::tempdir().unwrap();
    let pice_dir = project.path().join(".pice");
    std::fs::create_dir_all(&pice_dir).unwrap();
    let db_path = pice_dir.join("metrics.db");
    let _db = MetricsDb::open(&db_path).unwrap();

    let now = Utc::now();
    let requested_at = (now - Duration::hours(2)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let timeout_at = (now - Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Secs, true);

    let mut manifest = VerificationManifest::new("timeout-feat", project.path());
    manifest.overall_status = ManifestStatus::PendingReview;
    manifest
        .layers
        .push(layer("backend", LayerStatus::PendingReview));
    manifest.gates.push(GateEntry {
        id: "timeout-feat:backend:gate-1".to_string(),
        layer: "backend".to_string(),
        status: GateStatus::Pending,
        trigger_expression: "confidence < 0.9".to_string(),
        requested_at: requested_at.clone(),
        timeout_at,
        on_timeout_action: OnTimeout::Reject,
        reject_attempts_remaining: 0,
        decision: None,
        decided_at: None,
    });

    let reconciled = reconcile_expired_gates_inline(&mut manifest, now, project.path());
    manifest.compute_overall_status();

    assert_eq!(reconciled, 1);
    let gate = &manifest.gates[0];
    assert_eq!(gate.status, GateStatus::Rejected);
    assert_eq!(gate.decision.as_deref(), Some("timeout_reject"));
    assert!(gate.decided_at.is_some());

    let layer = &manifest.layers[0];
    assert_eq!(layer.status, LayerStatus::Failed);
    assert_eq!(
        layer.halted_by.as_deref(),
        Some(ExitJsonStatus::HALTED_GATE_TIMEOUT_REJECT)
    );
    assert_eq!(manifest.overall_status, ManifestStatus::Failed);

    let db = MetricsDb::open(&db_path).unwrap();
    let rows = query_gate_decisions(
        &db,
        &GateDecisionsFilter {
            feature_id: Some("timeout-feat".to_string()),
            since: None,
            limit: None,
        },
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].gate_id, "timeout-feat:backend:gate-1");
    assert_eq!(rows[0].layer, "backend");
    assert_eq!(rows[0].trigger_expression, "confidence < 0.9");
    assert_eq!(rows[0].decision, "timeout_reject");
    assert_eq!(rows[0].reviewer.as_deref(), Some("system/timeout"));
    assert_eq!(rows[0].requested_at, requested_at);
    assert!(rows[0].elapsed_seconds >= 3600);
}

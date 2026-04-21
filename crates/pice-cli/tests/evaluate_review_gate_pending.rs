//! Phase 6 contract criterion #4: non-TTY / JSON-mode `pice evaluate`
//! returns exit 3 with `status: "review-gate-pending"` when the
//! orchestrator halted at a cohort boundary.
//!
//! Integration test at the binary boundary (matching the pattern in
//! `evaluate_integration.rs`). Seeds a manifest directly into
//! `PICE_STATE_DIR` so the evaluate handler's auto-resume detects a
//! pending-review feature without needing a full plan + provider setup.

use assert_cmd::Command;
use pice_core::cli::ExitJsonStatus;
use pice_core::layers::manifest::{
    GateEntry, GateStatus, LayerResult, LayerStatus, ManifestStatus, VerificationManifest,
    SCHEMA_VERSION,
};
use pice_core::workflow::schema::OnTimeout;
use std::fs;
use std::path::Path;

fn pice_cmd() -> Command {
    let mut cmd = Command::cargo_bin("pice").unwrap();
    cmd.env("PICE_DAEMON_INLINE", "1");
    cmd
}

fn git_init(dir: &Path) {
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@test.com",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ])
        .current_dir(dir)
        .output()
        .unwrap();
}

fn write_plan_with_contract(dir: &Path) -> std::path::PathBuf {
    let plan_path = dir.join("plan.md");
    fs::write(
        &plan_path,
        r#"# Feature: Test

## Contract

```json
{
  "feature": "test",
  "tier": 2,
  "pass_threshold": 8,
  "criteria": []
}
```
"#,
    )
    .unwrap();
    plan_path
}

fn write_layers_and_workflow(dir: &Path) {
    fs::create_dir_all(dir.join(".pice")).unwrap();
    fs::write(
        dir.join(".pice/layers.toml"),
        r#"[layers]
order = ["backend"]

[layers.backend]
paths = ["src/**/*.rs"]
"#,
    )
    .unwrap();
    fs::write(
        dir.join(".pice/workflow.yaml"),
        r#"schema_version: "0.2"
defaults:
  tier: 2
  min_confidence: 0.9
  max_passes: 5
  model: sonnet
  budget_usd: 0
review:
  enabled: true
  trigger: "layer == backend"
  timeout_hours: 24
  on_timeout: reject
  retry_on_reject: 1
"#,
    )
    .unwrap();
}

/// Contract criterion #4: `pice evaluate plan.md --json` returns exit 3
/// with `status: "review-gate-pending"` on a feature whose manifest is
/// already in PendingReview (the auto-resume short-circuit).
#[test]
fn evaluate_json_mode_returns_review_gate_pending_exit_three() {
    let tmp = tempfile::tempdir().unwrap();
    let project_root = tmp.path();
    git_init(project_root);
    write_layers_and_workflow(project_root);
    let plan_path = write_plan_with_contract(project_root);

    // Seed a pending-review manifest. `manifest_path_for` anchors at
    // `$HOME/.pice/state/{project_hash}/`, so we set HOME to tmpdir.
    // `project_root` must be the canonicalized path so the hash matches
    // what the daemon subprocess computes (macOS tmpdir is a symlink to
    // /private/var/folders/...).
    let home_dir = tmp.path().canonicalize().unwrap();
    let canonical_project_root = project_root.canonicalize().unwrap();
    let project_namespace =
        pice_core::layers::manifest::manifest_project_namespace(&canonical_project_root);
    let namespace_dir = home_dir
        .join(".pice")
        .join("state")
        .join(&project_namespace);
    fs::create_dir_all(&namespace_dir).unwrap();
    let manifest_path = namespace_dir.join("plan.manifest.json");
    eprintln!("test: seeding manifest at {}", manifest_path.display());
    let manifest = VerificationManifest {
        schema_version: SCHEMA_VERSION.to_string(),
        feature_id: "plan".to_string(),
        project_root_hash: project_namespace.clone(),
        layers: vec![LayerResult {
            name: "backend".to_string(),
            status: LayerStatus::PendingReview,
            passes: Vec::new(),
            seam_checks: Vec::new(),
            halted_by: None,
            final_confidence: Some(0.95),
            total_cost_usd: Some(0.01),
            escalation_events: None,
        }],
        // Use a timeout well in the future so the reconciler does NOT
        // auto-process this gate as expired. A hardcoded fixture date
        // would bit-rot; compute from `now + 24h` to stay
        // time-independent.
        gates: vec![GateEntry {
            id: "plan:backend:0001".to_string(),
            layer: "backend".to_string(),
            status: GateStatus::Pending,
            trigger_expression: "layer == backend".to_string(),
            requested_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            timeout_at: (chrono::Utc::now() + chrono::Duration::hours(24))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            on_timeout_action: OnTimeout::Reject,
            reject_attempts_remaining: 1,
            decision: None,
            decided_at: None,
        }],
        overall_status: ManifestStatus::PendingReview,
    };
    manifest.save(&manifest_path).unwrap();

    let output = pice_cmd()
        .current_dir(&canonical_project_root)
        .env("HOME", &home_dir)
        .args(["evaluate", plan_path.to_str().unwrap(), "--json"])
        .output()
        .unwrap();

    // Contract demands exit 3 exactly.
    assert_eq!(
        output.status.code(),
        Some(3),
        "exit code must be 3 (ReviewGatePending); stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap_or(serde_json::json!({}));
    assert_eq!(
        parsed["status"].as_str(),
        Some(ExitJsonStatus::ReviewGatePending.as_str()),
        "status field must be review-gate-pending; payload: {parsed}"
    );
    assert!(
        parsed["pending_gates"].is_array(),
        "payload must carry pending_gates array; payload: {parsed}"
    );
    // Full pinned-field assertions: the plan advertised `timeout_at`,
    // `reject_attempts_remaining`, and `trigger_expression` as pinned at
    // gate-request time. CI loops and dashboards read these fields to
    // surface remaining-budget + deadline UI, so regressions here would
    // silently change downstream shape.
    let gate = &parsed["pending_gates"][0];
    assert_eq!(gate["layer"].as_str(), Some("backend"));
    assert!(
        gate["id"].is_string(),
        "pending_gates[0].id must be a string; got {gate}"
    );
    assert!(
        gate["timeout_at"].is_string(),
        "pending_gates[0].timeout_at must be pinned as a string; got {gate}"
    );
    assert_eq!(
        gate["reject_attempts_remaining"].as_u64(),
        Some(1),
        "pending_gates[0].reject_attempts_remaining must match seeded fixture; got {gate}"
    );
    assert!(
        gate["trigger_expression"].is_string(),
        "pending_gates[0].trigger_expression must be pinned at request time; got {gate}"
    );
    // The timeout_at must be RFC3339 parseable (canonicalization
    // invariant from metrics/store::canonicalize_rfc3339).
    let timeout_raw = gate["timeout_at"].as_str().unwrap();
    assert!(
        chrono::DateTime::parse_from_rfc3339(timeout_raw).is_ok(),
        "pending_gates[0].timeout_at must parse as RFC3339; got {timeout_raw}"
    );
}

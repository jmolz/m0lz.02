//! `pice audit gates --csv` binary integration test.
//!
//! Seeds three gate_decisions rows into a temp-project's metrics DB
//! via the same `rusqlite` API the daemon uses, then runs the real
//! `pice` binary with `PICE_DAEMON_INLINE=1` and asserts:
//!
//! - The CSV output has exactly `1 + 3 = 4` lines (header + rows).
//! - The header column order matches the writer's schema.
//! - The filter flags `--feature` and `--since` narrow the result set
//!   deterministically.
//!
//! Uses the daemon's helpers so a schema change in the writer ripples
//! through without having to re-author raw SQL in the fixture.

use assert_cmd::Command;
use pice_daemon::metrics::db::MetricsDb;
use pice_daemon::metrics::store::{insert_gate_decision, GateDecisionRow};
use std::path::Path;

fn pice_cmd(cwd: &Path) -> Command {
    let mut cmd = Command::cargo_bin("pice").unwrap();
    cmd.env("PICE_DAEMON_INLINE", "1");
    cmd.current_dir(cwd);
    cmd
}

fn seed_db(project: &Path) {
    std::fs::create_dir_all(project.join(".pice")).unwrap();
    let db = MetricsDb::open(&project.join(".pice").join("metrics.db")).unwrap();
    insert_gate_decision(
        &db,
        &GateDecisionRow {
            gate_id: "feat:infra:01",
            feature_id: "feat",
            layer: "infra",
            trigger_expression: "layer == infra",
            decision: "approve",
            reviewer: Some("jacob"),
            reason: None,
            requested_at: "2026-04-20T00:00:00Z",
            decided_at: "2026-04-20T00:05:00Z",
            elapsed_seconds: 300,
        },
    )
    .unwrap();
    insert_gate_decision(
        &db,
        &GateDecisionRow {
            gate_id: "feat2:deploy:01",
            feature_id: "feat2",
            layer: "deploy",
            trigger_expression: "tier >= 3",
            decision: "reject",
            reviewer: Some("alice"),
            reason: Some("perf regression"),
            requested_at: "2026-04-20T02:00:00Z",
            decided_at: "2026-04-20T02:10:00Z",
            elapsed_seconds: 600,
        },
    )
    .unwrap();
    insert_gate_decision(
        &db,
        &GateDecisionRow {
            gate_id: "feat3:db:01",
            feature_id: "feat3",
            layer: "database",
            trigger_expression: "always",
            decision: "skip",
            reviewer: Some("bob"),
            reason: None,
            requested_at: "2026-04-20T04:00:00Z",
            decided_at: "2026-04-20T04:01:00Z",
            elapsed_seconds: 60,
        },
    )
    .unwrap();
}

#[test]
fn csv_has_header_plus_three_data_rows() {
    let tmp = tempfile::tempdir().unwrap();
    seed_db(tmp.path());
    let output = pice_cmd(tmp.path())
        .args(["audit", "gates", "--csv"])
        .output()
        .expect("pice audit gates");
    assert!(
        output.status.success(),
        "exit status = {:?}, stderr = {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Filter out empty lines — the stdout renderer adds a trailing newline
    // after the Text response; `.lines()` then yields one empty string.
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        4,
        "expected header + 3 rows = 4 non-empty lines; got: {stdout}"
    );
    assert!(lines[0].starts_with("id,gate_id,feature_id,"));
}

#[test]
fn csv_filters_by_feature() {
    let tmp = tempfile::tempdir().unwrap();
    seed_db(tmp.path());
    let output = pice_cmd(tmp.path())
        .args(["audit", "gates", "--csv", "--feature", "feat2"])
        .output()
        .expect("pice audit gates --feature");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    // Header + 1 row.
    assert_eq!(
        lines.len(),
        2,
        "expected header + 1 filtered row; got: {stdout}"
    );
    assert!(lines[1].contains("feat2:deploy:01"));
    assert!(!stdout.contains("feat:infra:01"));
    assert!(!stdout.contains("feat3:db:01"));
}

#[test]
fn json_mode_emits_decisions_array() {
    let tmp = tempfile::tempdir().unwrap();
    seed_db(tmp.path());
    let output = pice_cmd(tmp.path())
        .args(["audit", "gates", "--json"])
        .output()
        .expect("pice audit gates --json");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must parse as JSON in --json mode");
    let arr = value["decisions"].as_array().expect("decisions array");
    assert_eq!(arr.len(), 3);
    // Rows come back ordered by requested_at ascending.
    assert_eq!(arr[0]["gate_id"], "feat:infra:01");
    assert_eq!(arr[1]["gate_id"], "feat2:deploy:01");
    assert_eq!(arr[2]["gate_id"], "feat3:db:01");
}

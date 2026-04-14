//! Integration tests for `pice validate` — exercise the binary through the
//! adapter stack, not just the handler. Contract criterion #6 explicitly
//! requires these live in `crates/pice-cli/tests/`.
//!
//! Covers: valid workflow (exit 0), invalid trigger (exit 1), unknown layer
//! override (exit 1), `--json` shape well-formed. Each case runs via the
//! actual `pice` binary in inline-daemon mode so no API keys or socket
//! server are needed.

use assert_cmd::Command;
use std::fs;

fn pice_cmd() -> Command {
    let mut cmd = Command::cargo_bin("pice").unwrap();
    cmd.env("PICE_DAEMON_INLINE", "1");
    cmd
}

/// Write a minimal `.pice/layers.toml` that `validate_cross_references` can
/// consult — needs both `order` and `[layers.<name>]` defs for each name.
fn write_layers_toml(root: &std::path::Path) {
    let layers = r#"
[layers]
order = ["backend", "frontend"]

[layers.backend]
paths = ["src/**"]

[layers.frontend]
paths = ["web/**"]
"#;
    fs::create_dir_all(root.join(".pice")).unwrap();
    fs::write(root.join(".pice/layers.toml"), layers).unwrap();
}

fn write_workflow(root: &std::path::Path, yaml: &str) {
    fs::create_dir_all(root.join(".pice")).unwrap();
    fs::write(root.join(".pice/workflow.yaml"), yaml).unwrap();
}

#[test]
fn validate_valid_workflow_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    write_layers_toml(dir.path());
    write_workflow(
        dir.path(),
        r#"schema_version: "0.2"
defaults:
  tier: 2
  min_confidence: 0.90
  max_passes: 5
  model: sonnet
  budget_usd: 2.0
  cost_cap_behavior: halt
"#,
    );

    pice_cmd()
        .current_dir(dir.path())
        .arg("validate")
        .assert()
        .success();
}

#[test]
fn validate_bad_trigger_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    write_layers_toml(dir.path());
    write_workflow(
        dir.path(),
        r#"schema_version: "0.2"
defaults:
  tier: 2
  min_confidence: 0.90
  max_passes: 5
  model: sonnet
  budget_usd: 2.0
  cost_cap_behavior: halt
review:
  enabled: true
  trigger: "tier =="
"#,
    );

    pice_cmd()
        .current_dir(dir.path())
        .arg("validate")
        .assert()
        .failure();
}

#[test]
fn validate_unknown_layer_override_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    write_layers_toml(dir.path());
    write_workflow(
        dir.path(),
        r#"schema_version: "0.2"
defaults:
  tier: 2
  min_confidence: 0.90
  max_passes: 5
  model: sonnet
  budget_usd: 2.0
  cost_cap_behavior: halt
layer_overrides:
  ghost_layer:
    tier: 3
"#,
    );

    pice_cmd()
        .current_dir(dir.path())
        .arg("validate")
        .assert()
        .failure();
}

#[test]
fn validate_json_mode_emits_structured_report_on_success() {
    let dir = tempfile::tempdir().unwrap();
    write_layers_toml(dir.path());
    write_workflow(
        dir.path(),
        r#"schema_version: "0.2"
defaults:
  tier: 2
  min_confidence: 0.90
  max_passes: 5
  model: sonnet
  budget_usd: 2.0
  cost_cap_behavior: halt
"#,
    );

    let output = pice_cmd()
        .current_dir(dir.path())
        .args(["validate", "--json"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout is valid utf-8");
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("json parse failed: {e}\n{stdout}"));
    assert!(json.get("ok").is_some(), "missing 'ok': {json:?}");
    assert!(json.get("errors").is_some(), "missing 'errors': {json:?}");
    assert!(
        json.get("warnings").is_some(),
        "missing 'warnings': {json:?}"
    );
}

#[test]
fn validate_json_mode_emits_errors_array_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    write_layers_toml(dir.path());
    write_workflow(
        dir.path(),
        r#"schema_version: "0.2"
defaults:
  tier: 2
  min_confidence: 0.90
  max_passes: 5
  model: sonnet
  budget_usd: 2.0
  cost_cap_behavior: halt
layer_overrides:
  ghost_layer:
    tier: 3
"#,
    );

    let output = pice_cmd()
        .current_dir(dir.path())
        .args(["validate", "--json"])
        .output()
        .unwrap();

    // JSON-mode validation failure must EXIT 1 (so CI scripts like
    // `pice validate --json && deploy` fail closed) AND emit the
    // structured report on stdout (so machine callers can consume
    // ok/errors/warnings). The render layer routes JSON-shaped Exit
    // messages to stdout.
    assert!(
        !output.status.success(),
        "JSON-mode validate on invalid workflow must exit nonzero; exit: {:?}, stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("expected JSON on stdout; parse error: {e}\n{stdout}"));
    assert_eq!(
        json["ok"], false,
        "expected ok=false on invalid workflow; got {json:?}"
    );
    let errors = json["errors"]
        .as_array()
        .expect("errors should be an array");
    assert!(!errors.is_empty(), "expected ≥1 error entry: {json:?}");
    let err_msgs: String = errors
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        err_msgs.contains("ghost_layer"),
        "errors array should name the bad layer; got: {err_msgs}"
    );
}

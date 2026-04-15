//! Integration tests for `pice evaluate --json` — exercise the binary end
//! to end (CLI adapter → inline daemon → orchestrator → manifest → response
//! renderer). Adversarial-review finding #15 called for these explicitly:
//! unit tests in the daemon handler are necessary but not sufficient —
//! exit-code propagation and the ExitJson stdout-vs-stderr routing must be
//! covered at the binary boundary.
//!
//! Each case writes a tmpdir scaffold (`.pice/layers.toml`,
//! `.pice/workflow.yaml`, fixture files, plan file), then runs `pice
//! evaluate --plan <path> --json` with `PICE_DAEMON_INLINE=1` so no socket
//! server is needed.

use assert_cmd::Command;
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

fn write_file(dir: &Path, rel: &str, content: &str) {
    let full = dir.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&full, content).unwrap();
}

fn write_workflow_with_seam(root: &Path, boundary: &str, check_id: &str) {
    let yaml = format!(
        r#"schema_version: "0.2"
defaults:
  tier: 2
  min_confidence: 0.90
  max_passes: 5
  model: sonnet
  budget_usd: 2.0
  cost_cap_behavior: halt
seams:
  {boundary}:
    - {check_id}
"#
    );
    fs::create_dir_all(root.join(".pice")).unwrap();
    fs::write(root.join(".pice/workflow.yaml"), yaml).unwrap();
}

fn write_minimal_plan(root: &Path) -> std::path::PathBuf {
    let plan_dir = root.join(".claude/plans");
    fs::create_dir_all(&plan_dir).unwrap();
    let plan_path = plan_dir.join("p.md");
    fs::write(
        &plan_path,
        r#"# P

## Contract

```json
{ "feature": "seam smoke", "tier": 2, "pass_threshold": 8, "criteria": [] }
```
"#,
    )
    .unwrap();
    plan_path
}

/// Failing fixture: Dockerfile declares `ORPHAN_VAR` that backend never
/// reads. `config_mismatch` returns Failed; evaluate --json must emit the
/// manifest on STDOUT (not stderr) and exit 2.
#[test]
fn evaluate_json_failing_seam_exits_two_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());

    // layers.toml with two seam-connected layers
    let layers = r#"
[layers]
order = ["backend", "infrastructure"]

[layers.backend]
paths = ["src/**"]

[layers.infrastructure]
paths = ["Dockerfile"]
"#;
    fs::create_dir_all(dir.path().join(".pice")).unwrap();
    fs::write(dir.path().join(".pice/layers.toml"), layers).unwrap();

    write_workflow_with_seam(dir.path(), "\"backend↔infrastructure\"", "config_mismatch");

    // Failing fixture — orphan env var not consumed by app
    write_file(
        dir.path(),
        "src/main.rs",
        "fn main() { println!(\"hello\"); }",
    );
    write_file(dir.path(), "Dockerfile", "FROM alpine\nENV ORPHAN_VAR=1\n");

    let plan = write_minimal_plan(dir.path());

    let output = pice_cmd()
        .current_dir(dir.path())
        .args(["evaluate", plan.to_str().unwrap(), "--json"])
        .output()
        .unwrap();

    // Exit code MUST be 2 (per CLAUDE.md exit-code convention — evaluation failed).
    assert_eq!(
        output.status.code(),
        Some(2),
        "expected exit 2 on failing seam; exit: {:?}, stderr: {}, stdout: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );

    // JSON payload MUST be on stdout (ExitJson contract). Parse it.
    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "expected JSON manifest on stdout; parse error: {e}\n=== STDOUT ===\n{stdout}\n=== STDERR ===\n{}",
            String::from_utf8_lossy(&output.stderr)
        )
    });

    // Manifest shape: `layers[].seam_checks[]` must contain the failing finding.
    let layers_arr = json["layers"]
        .as_array()
        .unwrap_or_else(|| panic!("expected manifest.layers to be an array: {json}"));
    let any_failed_seam = layers_arr.iter().any(|l| {
        l["seam_checks"]
            .as_array()
            .map(|checks| {
                checks
                    .iter()
                    .any(|c| c["status"] == "failed" && c["name"] == "config_mismatch")
            })
            .unwrap_or(false)
    });
    assert!(
        any_failed_seam,
        "expected at least one Failed config_mismatch seam finding: {json}"
    );
}

/// Passing fixture: env var declared AND consumed on both sides → no drift.
/// evaluate --json must emit on stdout with exit code 0 and all seam
/// checks reporting `status=passed`.
#[test]
fn evaluate_json_clean_fixture_exits_zero_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());

    let layers = r#"
[layers]
order = ["backend", "infrastructure"]

[layers.backend]
paths = ["src/**"]

[layers.infrastructure]
paths = ["Dockerfile"]
"#;
    fs::create_dir_all(dir.path().join(".pice")).unwrap();
    fs::write(dir.path().join(".pice/layers.toml"), layers).unwrap();

    write_workflow_with_seam(dir.path(), "\"backend↔infrastructure\"", "config_mismatch");

    write_file(
        dir.path(),
        "src/main.rs",
        "fn main() { std::env::var(\"DATABASE_URL\").unwrap(); }",
    );
    write_file(
        dir.path(),
        "Dockerfile",
        "FROM alpine\nENV DATABASE_URL=postgres://x\n",
    );

    let plan = write_minimal_plan(dir.path());

    let output = pice_cmd()
        .current_dir(dir.path())
        .args(["evaluate", plan.to_str().unwrap(), "--json"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(0),
        "expected exit 0 on clean seam fixture; exit: {:?}, stderr: {}, stdout: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("expected JSON on stdout; parse error: {e}\n{stdout}"));
    let layers_arr = json["layers"].as_array().expect("layers array");
    // Every seam check across every layer should be status=passed.
    for layer in layers_arr {
        if let Some(checks) = layer["seam_checks"].as_array() {
            for c in checks {
                assert_ne!(
                    c["status"], "failed",
                    "clean fixture must not contain Failed seam findings: {c}"
                );
            }
        }
    }
}

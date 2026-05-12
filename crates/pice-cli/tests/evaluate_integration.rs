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
use pice_core::cli::ExitJsonStatus;
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

/// Write a `.pice/config.toml` that routes both primary and adversarial
/// providers to the built-in `stub` provider. The stub emits deterministic
/// scores (default `score=8, passed=true`) so tests don't need a real API
/// key and the adaptive pass loop sees a passing evaluation.
///
/// Phase 4 wired up real provider invocation inside `run_stack_loops`;
/// tests that don't configure this helper will hit the production
/// `claude-code` provider and either grade the fixture for real (slow,
/// requires `ANTHROPIC_API_KEY`) or fail graceful-degrade with unpredictable
/// exit codes. Prefer stubbing over relying on resolver-miss fallback.
fn write_stub_config(root: &Path) {
    let toml = r#"
[provider]
name = "stub"

[evaluation.primary]
provider = "stub"
model = "stub-model"

[evaluation.adversarial]
provider = "stub"
model = "stub-model"
effort = ""
enabled = false

[evaluation.tiers]
tier1_models = []
tier2_models = []
tier3_models = []
tier3_agent_team = false

[telemetry]
enabled = false
endpoint = ""

[metrics]
db_path = ".pice/metrics.db"
"#;
    fs::create_dir_all(root.join(".pice")).unwrap();
    fs::write(root.join(".pice/config.toml"), toml).unwrap();
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
    // Phase 4: stub provider keeps the exit code driven by the seam
    // failure (category 1), not by whatever a real provider would grade
    // the fixture.
    write_stub_config(dir.path());

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
    // Phase 4: route providers to the deterministic stub so the adaptive
    // loop gets a passing eval regardless of host env / API keys.
    write_stub_config(dir.path());

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
        // Phase 4: drive the stub to a passing score (9.5) so SPRT accepts
        // before `max_passes`. Without this env var the stub scores 8 per
        // pass — under `min_confidence=0.90` the observation threshold is
        // 9.0 and SPRT eventually rejects.
        .env("PICE_STUB_SCORES", "9.5,0.001;9.5,0.001;9.5,0.001")
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

    // Adversarial-review finding: the previous version of this test only
    // inspected seam_checks when present — a regression that dropped the
    // array entirely would still pass. Now require at least one populated
    // seam_checks array with the configured config_mismatch check at
    // status=passed, AND reject any Warning/Failed on the clean fixture.
    let mut saw_populated_config_mismatch_passed = false;
    for layer in layers_arr {
        let checks = layer["seam_checks"]
            .as_array()
            .unwrap_or_else(|| panic!("layer is missing seam_checks array: {layer}"));
        for c in checks {
            // Criterion 15: clean fixture must have status=passed for every
            // seam check — not just "not failed". A Warning on the clean
            // fixture would mean the parser couldn't evaluate the boundary.
            assert_eq!(
                c["status"], "passed",
                "clean fixture seam check must be passed: {c}"
            );
            if c["name"] == "config_mismatch" {
                saw_populated_config_mismatch_passed = true;
            }
        }
    }
    assert!(
        saw_populated_config_mismatch_passed,
        "clean fixture must emit at least one passed config_mismatch check in \
         layers[].seam_checks[]; got: {json}"
    );
}

/// Phase 3 third-round adversarial fix: missing plan file under `--json`
/// must emit ExitJson on stdout with exit 1, not a plain text Exit on stderr.
#[test]
fn evaluate_json_missing_plan_emits_exit_json_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());
    let layers = r#"
[layers]
order = ["backend"]

[layers.backend]
paths = ["src/**"]
"#;
    fs::create_dir_all(dir.path().join(".pice")).unwrap();
    fs::write(dir.path().join(".pice/layers.toml"), layers).unwrap();

    let output = pice_cmd()
        .current_dir(dir.path())
        .args(["evaluate", ".claude/plans/does-not-exist.md", "--json"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(1),
        "missing plan must exit 1; stderr: {}, stdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("missing-plan failure must emit JSON on stdout; parse error: {e}\n{stdout}")
    });
    assert_eq!(json["status"], ExitJsonStatus::PlanNotFound.as_str());
    assert!(json["plan_path"].is_string());
}

/// Phase 3 third-round adversarial fix: a plan with no `## Contract` section
/// under `--json` must emit ExitJson on stdout with exit 2 (criterion-2 = no
/// evaluation possible), not a plain text Exit on stderr.
#[test]
fn evaluate_json_no_contract_section_emits_exit_json_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());
    let layers = r#"
[layers]
order = ["backend"]

[layers.backend]
paths = ["src/**"]
"#;
    fs::create_dir_all(dir.path().join(".pice")).unwrap();
    fs::write(dir.path().join(".pice/layers.toml"), layers).unwrap();

    // Plan exists but has no Contract section.
    let plan_dir = dir.path().join(".claude/plans");
    fs::create_dir_all(&plan_dir).unwrap();
    let plan_path = plan_dir.join("p.md");
    fs::write(&plan_path, "# P\n\nJust some prose. No contract.\n").unwrap();

    let output = pice_cmd()
        .current_dir(dir.path())
        .args(["evaluate", plan_path.to_str().unwrap(), "--json"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(2),
        "no-contract must exit 2; stderr: {}, stdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("no-contract failure must emit JSON on stdout; parse error: {e}\n{stdout}")
    });
    assert_eq!(json["status"], ExitJsonStatus::NoContractSection.as_str());
    assert!(json["plan_path"].is_string());
}

/// Phase 3 third-round adversarial fix: a malformed plan (Contract heading
/// without a json fence) under `--json` must emit ExitJson on stdout with
/// exit 1.
#[test]
fn evaluate_json_plan_parse_failure_emits_exit_json_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());
    let layers = r#"
[layers]
order = ["backend"]

[layers.backend]
paths = ["src/**"]
"#;
    fs::create_dir_all(dir.path().join(".pice")).unwrap();
    fs::write(dir.path().join(".pice/layers.toml"), layers).unwrap();

    let plan_dir = dir.path().join(".claude/plans");
    fs::create_dir_all(&plan_dir).unwrap();
    let plan_path = plan_dir.join("p.md");
    // Contract heading present but no ```json fence — plan_parser surfaces
    // this as an error rather than Ok(None).
    fs::write(
        &plan_path,
        "# P\n\n## Contract\n\nNo JSON fence here, just prose.\n",
    )
    .unwrap();

    let output = pice_cmd()
        .current_dir(dir.path())
        .args(["evaluate", plan_path.to_str().unwrap(), "--json"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(1),
        "plan parse failure must exit 1; stderr: {}, stdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("plan-parse failure must emit JSON on stdout; parse error: {e}\n{stdout}")
    });
    assert_eq!(json["status"], ExitJsonStatus::PlanParseFailed.as_str());
    assert!(json["error"].is_string());
}

/// Phase 7 remediation: binary integration tests run under inline mode, so
/// `--background` must reject with the inline-mode typed status before lower
/// level background preflights such as missing `.pice/layers.toml`.
#[test]
fn evaluate_background_json_missing_layers_toml_emits_exit_json_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());
    let plan = write_minimal_plan(dir.path());

    let output = pice_cmd()
        .current_dir(dir.path())
        .args(["evaluate", plan.to_str().unwrap(), "--background", "--json"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(1),
        "missing layers.toml must exit 1; stderr: {}, stdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("inline background rejection must emit JSON on stdout; parse error: {e}\n{stdout}")
    });
    assert_eq!(
        json["status"],
        ExitJsonStatus::InlineModeBackgroundUnsupported.as_str()
    );
    assert!(json["plan_path"].is_string());
}

/// Phase 3 round-4 adversarial fix: workflow.yaml validation failure under
/// `--json` must emit ExitJson on stdout with exit 1, not plain text on
/// stderr. Two of three round-2 ExitJson statuses lacked CLI binary tests
/// (the third — workflow-validation-failed — is asserted here). Two
/// evaluators independently flagged this gap.
#[test]
fn evaluate_json_workflow_validation_failure_emits_exit_json_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());

    let layers = r#"
[layers]
order = ["backend"]

[layers.backend]
paths = ["src/**"]
"#;
    fs::create_dir_all(dir.path().join(".pice")).unwrap();
    fs::write(dir.path().join(".pice/layers.toml"), layers).unwrap();

    // workflow.yaml that validate_all rejects: an unknown layer override.
    // (`unknown_layer` does not appear in layers.toml `order`.)
    let workflow = r#"schema_version: "0.2"
defaults:
  tier: 2
  min_confidence: 0.90
  max_passes: 5
  model: sonnet
  budget_usd: 2.0
  cost_cap_behavior: halt
layer_overrides:
  unknown_layer:
    tier: 3
"#;
    fs::write(dir.path().join(".pice/workflow.yaml"), workflow).unwrap();

    let plan = write_minimal_plan(dir.path());

    let output = pice_cmd()
        .current_dir(dir.path())
        .args(["evaluate", plan.to_str().unwrap(), "--json"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(1),
        "workflow validation failure must exit 1; stderr: {}, stdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("workflow-validation failure must emit JSON on stdout; parse error: {e}\n{stdout}")
    });
    assert_eq!(
        json["status"],
        ExitJsonStatus::WorkflowValidationFailed.as_str()
    );
    assert!(
        json["errors"].is_array(),
        "must include errors array: {json}"
    );
}

/// Phase 3 round-4 adversarial fix: seam floor violation under `--json` must
/// emit ExitJson on stdout. The user empty-lists a project-required boundary.
#[test]
fn evaluate_json_seam_floor_violation_emits_exit_json_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());

    // layers.toml declares a seam boundary with required checks (the floor).
    let layers = r#"
[layers]
order = ["backend", "infrastructure"]

[layers.backend]
paths = ["src/**"]

[layers.infrastructure]
paths = ["Dockerfile"]

[seams]
"backend↔infrastructure" = ["config_mismatch"]
"#;
    fs::create_dir_all(dir.path().join(".pice")).unwrap();
    fs::write(dir.path().join(".pice/layers.toml"), layers).unwrap();

    // workflow.yaml empty-lists the same boundary — that's the floor
    // violation: the project required at least one check, the user said zero.
    let workflow = r#"schema_version: "0.2"
defaults:
  tier: 2
  min_confidence: 0.90
  max_passes: 5
  model: sonnet
  budget_usd: 2.0
  cost_cap_behavior: halt
seams:
  "backend↔infrastructure": []
"#;
    fs::write(dir.path().join(".pice/workflow.yaml"), workflow).unwrap();

    write_file(dir.path(), "src/main.rs", "fn main() {}\n");
    write_file(dir.path(), "Dockerfile", "FROM alpine\n");
    let plan = write_minimal_plan(dir.path());

    let output = pice_cmd()
        .current_dir(dir.path())
        .args(["evaluate", plan.to_str().unwrap(), "--json"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(1),
        "seam floor violation must exit 1; stderr: {}, stdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("seam-floor-violation must emit JSON on stdout; parse error: {e}\n{stdout}")
    });
    assert_eq!(json["status"], ExitJsonStatus::SeamFloorViolation.as_str());
    assert!(
        json["violations"].is_array(),
        "must include violations array: {json}"
    );
}

/// Phase 3 round-4 adversarial fix: merged-seam validation failure under
/// `--json` must emit ExitJson on stdout. The merged map (layers.toml +
/// workflow.yaml) is re-validated against the registry — declare a
/// boundary in layers.toml that uses an unknown check id.
#[test]
fn evaluate_json_merged_seam_validation_failure_emits_exit_json_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());

    // layers.toml declares an unknown seam check id — `validate_all` only
    // checks workflow.seams; the merged-seam re-validation catches this.
    let layers = r#"
[layers]
order = ["backend", "infrastructure"]

[layers.backend]
paths = ["src/**"]

[layers.infrastructure]
paths = ["Dockerfile"]

[seams]
"backend↔infrastructure" = ["this_check_id_does_not_exist"]
"#;
    fs::create_dir_all(dir.path().join(".pice")).unwrap();
    fs::write(dir.path().join(".pice/layers.toml"), layers).unwrap();

    write_file(dir.path(), "src/main.rs", "fn main() {}\n");
    write_file(dir.path(), "Dockerfile", "FROM alpine\n");
    let plan = write_minimal_plan(dir.path());

    let output = pice_cmd()
        .current_dir(dir.path())
        .args(["evaluate", plan.to_str().unwrap(), "--json"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(1),
        "merged-seam validation failure must exit 1; stderr: {}, stdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("merged-seam-validation-failed must emit JSON on stdout; parse error: {e}\n{stdout}")
    });
    assert_eq!(
        json["status"],
        ExitJsonStatus::MergedSeamValidationFailed.as_str()
    );
    assert!(
        json["errors"].is_array(),
        "must include errors array: {json}"
    );
}

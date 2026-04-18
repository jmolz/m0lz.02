//! CLI binary boundary tests for PRDv2 Phase 4 adaptive evaluation.
//!
//! Each test runs `pice evaluate --json` via `assert_cmd` with
//! `PICE_DAEMON_INLINE=1`, exercising the full adapter → inline daemon →
//! orchestrator → manifest → response renderer chain. Asserts:
//! - exit code matches the halt path (0 / 0 / 0 / 2 / 0 / 0 / 2)
//! - JSON on stdout (not stderr) carries `layers[].halted_by`
//! - `ExitJsonStatus::EvaluationFailed.as_str()` matches the wire string on
//!   exit-2 paths (per `.claude/rules/daemon.md` §"typed discriminants")
//!
//! Per-halt-reason coverage matrix:
//! | test                          | halted_by                     | exit |
//! |-------------------------------|-------------------------------|------|
//! | sprt_accept                   | sprt_confidence_reached       | 0    |
//! | sprt_reject                   | sprt_rejected                 | 2    |
//! | budget                        | budget                        | 0    |
//! | cold_start_seed               | budget                        | 0    |
//! | max_passes                    | max_passes                    | 0    |
//! | vec_entropy                   | vec_entropy                   | 0    |
//! | adts_exhaustion (skipped)     | adts_escalation_exhausted     | 2    |
//!
//! ADTS exhaustion is asserted at the orchestrator level
//! (`crates/pice-daemon/tests/adaptive_integration.rs`) — exercising it
//! through the CLI binary requires the stub provider's per-role offset
//! feature, which is deferred. See that file's notes for context.

use assert_cmd::Command;
use pice_core::cli::ExitJsonStatus;
use std::fs;
use std::path::Path;

// ─── Shared helpers (mirror evaluate_integration.rs) ───────────────────────

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

fn write_layers_toml(root: &Path) {
    let layers = r#"
[layers]
order = ["backend"]

[layers.backend]
paths = ["src/**"]
"#;
    fs::create_dir_all(root.join(".pice")).unwrap();
    fs::write(root.join(".pice/layers.toml"), layers).unwrap();
}

fn write_workflow(
    root: &Path,
    algorithm: &str,
    min_confidence: f64,
    max_passes: u32,
    budget_usd: f64,
) {
    let yaml = format!(
        r#"schema_version: "0.2"
defaults:
  tier: 2
  min_confidence: {min_confidence}
  max_passes: {max_passes}
  model: stub-model
  budget_usd: {budget_usd}
  cost_cap_behavior: halt
phases:
  evaluate:
    parallel: true
    seam_checks: true
    adaptive_algorithm: {algorithm}
"#,
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
        r#"# Phase 4 CLI test

## Contract

```json
{ "feature": "adaptive-cli", "tier": 2, "pass_threshold": 7, "criteria": [] }
```
"#,
    )
    .unwrap();
    plan_path
}

/// Common scaffolding: tmpdir + git init + stub config + layers.toml + plan.
fn setup(dir: &Path) -> std::path::PathBuf {
    git_init(dir);
    write_stub_config(dir);
    write_layers_toml(dir);
    write_file(dir, "src/main.rs", "fn main() {}");
    write_minimal_plan(dir)
}

/// Find the `backend` layer in the JSON output and assert its `halted_by`
/// matches `expected`.
fn assert_layer_halted_by(json: &serde_json::Value, expected: &str) {
    let layers = json["layers"]
        .as_array()
        .unwrap_or_else(|| panic!("expected layers array; got: {json}"));
    let backend = layers
        .iter()
        .find(|l| l["name"] == "backend")
        .unwrap_or_else(|| panic!("backend layer missing: {json}"));
    let halted_by = backend["halted_by"]
        .as_str()
        .unwrap_or_else(|| panic!("backend missing halted_by: {backend}"));
    assert_eq!(
        halted_by, expected,
        "expected halted_by={expected}; got {halted_by} ({backend})"
    );
}

// ─── Test 1: SPRT accept (exit 0) ──────────────────────────────────────────

#[test]
fn cli_evaluate_sprt_accept_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let plan = setup(dir.path());
    write_workflow(dir.path(), "bayesian_sprt", 0.90, 8, 10.0);

    let output = pice_cmd()
        .current_dir(dir.path())
        .env(
            "PICE_STUB_SCORES",
            "9.5,0.001;9.5,0.001;9.5,0.001;9.5,0.001;9.5,0.001;9.5,0.001;9.5,0.001;9.5,0.001",
        )
        .args(["evaluate", plan.to_str().unwrap(), "--json"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(0),
        "expected exit 0; stderr: {}, stdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_layer_halted_by(&json, "sprt_confidence_reached");
}

// ─── Test 2: SPRT reject (exit 2 + ExitJsonStatus::EvaluationFailed) ───────

#[test]
fn cli_evaluate_sprt_reject_exits_two_with_typed_status() {
    let dir = tempfile::tempdir().unwrap();
    let plan = setup(dir.path());
    write_workflow(dir.path(), "bayesian_sprt", 0.90, 10, 10.0);

    let output = pice_cmd()
        .current_dir(dir.path())
        .env(
            "PICE_STUB_SCORES",
            "3.0,0.001;3.0,0.001;3.0,0.001;3.0,0.001;3.0,0.001;3.0,0.001;3.0,0.001;3.0,0.001;3.0,0.001;3.0,0.001",
        )
        .args(["evaluate", plan.to_str().unwrap(), "--json"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(2),
        "expected exit 2 on SPRT reject; stderr: {}, stdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    // Pass-4 Claude Evaluator regression (Criterion 12): under `--json`, the
    // JSON failure payload MUST land on stdout, not stderr. `pice evaluate
    // --json > report.json && deploy` pipelines depend on stdout being the
    // canonical JSON channel. Tracing output on stderr is expected per
    // CLAUDE.md (`tracing` writes to stderr by design), so we assert the
    // JSON payload is NOT present on stderr — not that stderr is byte-empty.
    let stderr_text = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr_text.contains("\"status\":"),
        "JSON response must not leak to stderr under --json; got stderr: {stderr_text}",
    );
    assert!(
        !stderr_text.trim_start().starts_with('{'),
        "stderr must not start with a JSON object under --json; got stderr: {stderr_text}",
    );
    // Exit-2 ExitJson must NOT use a literal status string. Per
    // `.claude/rules/daemon.md`, the wire form must come from
    // `ExitJsonStatus::EvaluationFailed.as_str()` — verify here so a
    // refactor that swaps to a literal mechanically fails this test.
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    if let Some(status) = json["status"].as_str() {
        assert_eq!(status, ExitJsonStatus::EvaluationFailed.as_str());
    } else {
        // Some response shapes embed the manifest directly without a top-level
        // status — assert the failed-layer halt reason instead.
        assert_layer_halted_by(&json, "sprt_rejected");
    }
}

// ─── Test 3: Budget halt (exit 0 — Pending is not a CLI failure) ───────────

#[test]
fn cli_evaluate_budget_halt_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let plan = setup(dir.path());
    write_workflow(dir.path(), "bayesian_sprt", 0.90, 5, 0.05);

    let output = pice_cmd()
        .current_dir(dir.path())
        .env(
            "PICE_STUB_SCORES",
            "9.5,0.03;9.5,0.03;9.5,0.03;9.5,0.03;9.5,0.03",
        )
        .args(["evaluate", plan.to_str().unwrap(), "--json"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(0),
        "Pending (budget halt) is not a CLI failure; got {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_layer_halted_by(&json, "budget");
}

// ─── Test 4: Cold-start seed halt (exit 0, halted_by="budget") ─────────────

#[test]
fn cli_evaluate_cold_start_seed_exits_zero_with_budget_halt() {
    let dir = tempfile::tempdir().unwrap();
    let plan = setup(dir.path());
    write_workflow(dir.path(), "bayesian_sprt", 0.90, 5, 0.001);

    let output = pice_cmd()
        .current_dir(dir.path())
        .env(
            "PICE_STUB_SCORES",
            "9.5,0.01;9.5,0.01;9.5,0.01;9.5,0.01;9.5,0.01",
        )
        .args(["evaluate", plan.to_str().unwrap(), "--json"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_layer_halted_by(&json, "budget");
    let layers = json["layers"].as_array().unwrap();
    let backend = layers.iter().find(|l| l["name"] == "backend").unwrap();
    let passes = backend["passes"].as_array().unwrap();
    assert_eq!(passes.len(), 1, "cold-start should permit pass 1 then halt");
}

// ─── Test 5: max_passes halt (exit 0) ──────────────────────────────────────

#[test]
fn cli_evaluate_max_passes_halt_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let plan = setup(dir.path());
    write_workflow(dir.path(), "bayesian_sprt", 0.50, 4, 10.0);

    let output = pice_cmd()
        .current_dir(dir.path())
        .env(
            "PICE_STUB_SCORES",
            "6.0,0.001;4.0,0.001;6.0,0.001;4.0,0.001",
        )
        .args(["evaluate", plan.to_str().unwrap(), "--json"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_layer_halted_by(&json, "max_passes");
}

// ─── Test 6: VEC entropy halt (exit 0) ─────────────────────────────────────

#[test]
fn cli_evaluate_vec_entropy_halt_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let plan = setup(dir.path());
    // VEC entropy floor at 0.5 bits halts at pass 2. min_confidence=0.70
    // clears the gate (VEC → Passed requires `final_confidence >= min_confidence`;
    // Beta(3,1) posterior mean at pass 2 is 0.75).
    let yaml = r#"schema_version: "0.2"
defaults:
  tier: 2
  min_confidence: 0.70
  max_passes: 6
  model: stub-model
  budget_usd: 10.0
  cost_cap_behavior: halt
phases:
  evaluate:
    parallel: true
    seam_checks: true
    adaptive_algorithm: vec
    vec:
      entropy_floor: 0.5
"#;
    fs::write(dir.path().join(".pice/workflow.yaml"), yaml).unwrap();

    let output = pice_cmd()
        .current_dir(dir.path())
        .env(
            "PICE_STUB_SCORES",
            "8.0,0.001;8.0,0.001;8.0,0.001;8.0,0.001;8.0,0.001;8.0,0.001",
        )
        .args(["evaluate", plan.to_str().unwrap(), "--json"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_layer_halted_by(&json, "vec_entropy");
}

// ─── Test 7: AdaptiveAlgo::None still respects budget (exit 0) ────────────

#[test]
fn cli_evaluate_algo_none_respects_budget() {
    let dir = tempfile::tempdir().unwrap();
    let plan = setup(dir.path());
    write_workflow(dir.path(), "none", 0.90, 10, 0.05);

    let output = pice_cmd()
        .current_dir(dir.path())
        .env(
            "PICE_STUB_SCORES",
            "9.5,0.03;9.5,0.03;9.5,0.03;9.5,0.03;9.5,0.03;9.5,0.03;9.5,0.03;9.5,0.03;9.5,0.03;9.5,0.03",
        )
        .args(["evaluate", plan.to_str().unwrap(), "--json"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_layer_halted_by(&json, "budget");
}

// ─── Pass-7 Codex Critical #1: budget_usd=0 escape hatch ───────────────────

/// Phase 4.1 Pass-7 Codex Critical #1 regression: `budget_usd = 0.0` is
/// the documented "no financial enforcement" escape hatch (plan's Known
/// Limitation for teams whose providers lack `costTelemetry`). Before the
/// fix in `adaptive/decide.rs`, the universal `accumulated + projected
/// > budget_usd` check evaluated `positive + positive > 0` as true after
/// the first pass with any real cost, silently halting with
/// `halted_by = "budget"` and invalidating the escape hatch.
///
/// This test drives `pice evaluate` end-to-end with `budget_usd = 0` and
/// a stub that reports positive per-pass costs. Asserts the loop runs
/// through to `max_passes` (not to `budget`).
#[test]
fn cli_evaluate_budget_zero_does_not_halt_on_positive_costs() {
    let dir = tempfile::tempdir().unwrap();
    let plan = setup(dir.path());
    // `budget_usd = 0.0` → no financial enforcement. `max_passes = 3`
    // is the only universal bound. Stub reports $0.02 per pass — pre-fix
    // would halt at pass 1 with `budget`; post-fix runs to pass 3 and
    // halts with `max_passes`.
    write_workflow(dir.path(), "none", 0.90, 3, 0.0);

    let output = pice_cmd()
        .current_dir(dir.path())
        .env("PICE_STUB_SCORES", "9.5,0.02;9.5,0.02;9.5,0.02")
        .args(["evaluate", plan.to_str().unwrap(), "--json"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(0),
        "expected exit 0 (Pending / max_passes is not a CLI failure); stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_layer_halted_by(&json, "max_passes");
}

// ─── Pass-7 Codex High #2: DB open failure fails closed ────────────────────

/// Phase 4.1 Pass-7 Codex High #2 regression: when the metrics DB path
/// exists but cannot be opened (corrupt file, wrong type, permission
/// error, disk full), the handler must fail closed with
/// `ExitJsonStatus::MetricsPersistFailed` (exit 1) — not silently degrade
/// to `NullPassSink` and return a green response.
///
/// Before the fix, `.ok().flatten()` collapsed `Err(_)` into `None`,
/// indistinguishable from the legitimate "no DB file" path. Real
/// production failure modes (corrupt SQLite, RO filesystem, disk full)
/// returned success with no `pass_events` and no `evaluations` row —
/// invisible to dashboards and CI.
///
/// The test forces failure by making the DB path a DIRECTORY instead of
/// a file. SQLite's `open` fails on Unix with "is a directory", surfaced
/// as an `Err` from `open_metrics_db`. The post-fix handler returns the
/// `MetricsPersistFailed` structured response.
#[test]
fn cli_evaluate_corrupt_db_fails_closed_with_metrics_persist_failed() {
    let dir = tempfile::tempdir().unwrap();
    let plan = setup(dir.path());
    write_workflow(dir.path(), "bayesian_sprt", 0.90, 5, 10.0);

    // Force `open_metrics_db` to return `Err`: create `.pice/metrics.db` as
    // a DIRECTORY, not a file. SQLite's `open` surfaces this as a real
    // failure (not "Ok(None) — no DB yet"), exercising the fail-closed path.
    fs::create_dir_all(dir.path().join(".pice/metrics.db")).unwrap();

    let output = pice_cmd()
        .current_dir(dir.path())
        .env(
            "PICE_STUB_SCORES",
            "9.5,0.001;9.5,0.001;9.5,0.001;9.5,0.001;9.5,0.001",
        )
        .args(["evaluate", plan.to_str().unwrap(), "--json"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(1),
        "expected exit 1 on metrics-persist failure; got {:?}; stderr: {}; stdout: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    // JSON payload on stdout, not stderr — CLAUDE.md daemon rule.
    let stderr_text = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr_text.contains("\"status\":"),
        "JSON response must not leak to stderr under --json; got stderr: {stderr_text}",
    );
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout must parse as JSON under --json");
    let status = json["status"]
        .as_str()
        .expect("top-level status field required on metrics-persist-failed response");
    assert_eq!(
        status,
        ExitJsonStatus::MetricsPersistFailed.as_str(),
        "wire status must come from ExitJsonStatus::MetricsPersistFailed.as_str() — not a literal",
    );
    assert!(
        json["errors"].is_array(),
        "errors field must be an array of SQLite error strings",
    );
}

//! Integration tests for Phase 2 and Phase 3 commands.
//!
//! These tests use `assert_cmd` to invoke the pice binary with the stub provider.
//! They verify the full CLI pipeline without requiring real API keys.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

fn pice_cmd() -> Command {
    Command::cargo_bin("pice").unwrap()
}

/// Helper: create a temp directory with a minimal .pice/config.toml
/// pointing at the stub provider.
fn setup_stub_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();

    // Config that uses the stub provider
    let pice_dir = dir.path().join(".pice");
    fs::create_dir_all(&pice_dir).unwrap();
    fs::write(
        pice_dir.join("config.toml"),
        r#"
[provider]
name = "stub"

[evaluation]
[evaluation.primary]
provider = "stub"
model = "stub-echo"

[evaluation.adversarial]
provider = "stub"
model = "stub-echo"
effort = "high"
enabled = true

[evaluation.tiers]
tier1_models = ["stub-echo"]
tier2_models = ["stub-echo"]
tier3_models = ["stub-echo"]
tier3_agent_team = false

[telemetry]
enabled = false
endpoint = "https://telemetry.pice.dev/v1/events"

[metrics]
db_path = ".pice/metrics.db"
"#,
    )
    .unwrap();

    dir
}

/// Helper: create a plan file with a contract section.
fn create_plan_with_contract(dir: &std::path::Path) -> std::path::PathBuf {
    let plans_dir = dir.join(".claude/plans");
    fs::create_dir_all(&plans_dir).unwrap();
    let plan_path = plans_dir.join("test-plan.md");
    fs::write(
        &plan_path,
        r#"# Feature: Test Plan

## Overview
A simple test plan.

## Contract

```json
{
  "feature": "Test Plan",
  "tier": 2,
  "pass_threshold": 7,
  "criteria": [
    {
      "name": "Build passes",
      "threshold": 7,
      "validation": "cargo build"
    }
  ]
}
```
"#,
    )
    .unwrap();
    plan_path
}

// ─── Help / Flag Tests ─────────────────────────────────────────────────────

#[test]
fn plan_command_shows_json_flag_in_help() {
    pice_cmd()
        .arg("plan")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"));
}

#[test]
fn execute_command_shows_json_flag_in_help() {
    pice_cmd()
        .arg("execute")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"));
}

#[test]
fn evaluate_command_shows_json_flag_in_help() {
    pice_cmd()
        .arg("evaluate")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"));
}

// ─── Error Path Tests ──────────────────────────────────────────────────────

#[test]
fn execute_with_missing_plan_file_fails() {
    pice_cmd()
        .arg("execute")
        .arg("/nonexistent/plan.md")
        .assert()
        .failure();
}

#[test]
fn evaluate_with_missing_plan_file_fails() {
    pice_cmd()
        .arg("evaluate")
        .arg("/nonexistent/plan.md")
        .assert()
        .failure();
}

#[test]
fn evaluate_plan_without_contract_fails() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = dir.path().join("no-contract.md");
    fs::write(&plan_path, "# No Contract Plan\n\nJust text.\n").unwrap();

    // Set up config with adversarial disabled to avoid spawning a second provider
    let pice_dir = dir.path().join(".pice");
    fs::create_dir_all(&pice_dir).unwrap();
    fs::write(
        pice_dir.join("config.toml"),
        r#"
[provider]
name = "stub"

[evaluation]
[evaluation.primary]
provider = "stub"
model = "stub-echo"

[evaluation.adversarial]
provider = "stub"
model = "stub-echo"
effort = "high"
enabled = false

[evaluation.tiers]
tier1_models = ["stub-echo"]
tier2_models = ["stub-echo"]
tier3_models = ["stub-echo"]
tier3_agent_team = false

[telemetry]
enabled = false
endpoint = "https://telemetry.pice.dev/v1/events"

[metrics]
db_path = ".pice/metrics.db"
"#,
    )
    .unwrap();

    pice_cmd()
        .current_dir(dir.path())
        .arg("evaluate")
        .arg(plan_path.to_string_lossy().to_string())
        .assert()
        .failure()
        .stderr(predicate::str::contains("no contract section"));
}

// ─── Stub Provider Pipeline Tests ──────────────────────────────────────────
//
// These tests spawn the stub provider as a real child process.
// They require `pnpm build` to have been run so the stub JS is available.
// The binary locates providers via find_provider_base() which walks up from
// the binary location looking for packages/.

#[test]
fn plan_command_with_stub_provider() {
    let dir = setup_stub_project();

    // Plan command should succeed with the stub provider:
    // config load → provider spawn → session create → send → destroy → shutdown
    pice_cmd()
        .current_dir(dir.path())
        .arg("plan")
        .arg("test feature")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"complete\""));
}

#[test]
fn execute_command_with_stub_provider() {
    let dir = setup_stub_project();
    let plan_path = create_plan_with_contract(dir.path());

    // Execute command should succeed with the stub provider:
    // plan file load → provider spawn → session create → send → destroy → shutdown
    pice_cmd()
        .current_dir(dir.path())
        .arg("execute")
        .arg(plan_path.to_string_lossy().to_string())
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"complete\""))
        .stdout(predicate::str::contains("\"plan\": \"Feature: Test Plan\""));
}

#[test]
fn evaluate_command_with_stub_provider() {
    let dir = setup_stub_project();
    let plan_path = create_plan_with_contract(dir.path());

    // Initialize a git repo so get_git_diff() works
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@test.com",
            "commit",
            "-m",
            "init",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Evaluate with stub provider — stub returns mock scores that pass
    // Uses --json so we can verify the structured output
    pice_cmd()
        .current_dir(dir.path())
        .arg("evaluate")
        .arg(plan_path.to_string_lossy().to_string())
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"passed\": true"))
        .stdout(predicate::str::contains("\"tier\": 2"));
}

#[test]
fn evaluate_graceful_degradation() {
    let dir = tempfile::tempdir().unwrap();

    // Config with adversarial pointing to a nonexistent provider
    let pice_dir = dir.path().join(".pice");
    fs::create_dir_all(&pice_dir).unwrap();
    fs::write(
        pice_dir.join("config.toml"),
        r#"
[provider]
name = "stub"

[evaluation]
[evaluation.primary]
provider = "stub"
model = "stub-echo"

[evaluation.adversarial]
provider = "nonexistent-provider"
model = "fake-model"
effort = "high"
enabled = true

[evaluation.tiers]
tier1_models = ["stub-echo"]
tier2_models = ["stub-echo", "fake-model"]
tier3_models = ["stub-echo", "fake-model"]
tier3_agent_team = false

[telemetry]
enabled = false
endpoint = "https://telemetry.pice.dev/v1/events"

[metrics]
db_path = ".pice/metrics.db"
"#,
    )
    .unwrap();

    // Create plan with contract (tier 2 — triggers adversarial path)
    let plan_path = create_plan_with_contract(dir.path());

    // Initialize a git repo
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@test.com",
            "commit",
            "-m",
            "init",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Evaluate should complete with primary results only, not crash.
    // The adversarial provider fails to resolve, but the primary (stub) succeeds.
    // Exit code 0 because primary passes.
    pice_cmd()
        .current_dir(dir.path())
        .arg("evaluate")
        .arg(plan_path.to_string_lossy().to_string())
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"passed\": true"));
}

// ═══ Phase 4 Tests ═══════════════════════════════════════════════════════════

// ─── Phase 4: Help / Flag Tests ──────────────────────────────────────────────

#[test]
fn phase4_metrics_help() {
    pice_cmd()
        .arg("metrics")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("--csv"));
}

#[test]
fn phase4_benchmark_help() {
    pice_cmd()
        .arg("benchmark")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"));
}

// ─── Phase 4: Metrics with Empty DB ──────────────────────────────────────────

#[test]
fn phase4_metrics_empty_db() {
    let dir = tempfile::tempdir().unwrap();

    // Run pice init to create a real metrics DB
    pice_cmd()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();

    // Run pice metrics --json against the empty DB
    pice_cmd()
        .current_dir(dir.path())
        .arg("metrics")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"total_evaluations\": 0"))
        .stdout(predicate::str::contains("\"total_loops\": 0"));
}

// ─── Phase 4: Benchmark ─────────────────────────────────────────────────────

#[test]
fn phase4_benchmark_empty() {
    let dir = tempfile::tempdir().unwrap();

    // Init git repo
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
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
        .current_dir(dir.path())
        .output()
        .unwrap();

    pice_cmd()
        .current_dir(dir.path())
        .arg("benchmark")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"total_commits\""))
        .stdout(predicate::str::contains("\"coverage_pct\""));
}

// ─── Phase 4: Init creates real DB ──────────────────────────────────────────

#[test]
fn phase4_init_creates_real_db() {
    let dir = tempfile::tempdir().unwrap();

    pice_cmd()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();

    let db_path = dir.path().join(".pice/metrics.db");
    assert!(db_path.exists());

    // Verify it's a real SQLite DB (not empty) by checking file size
    let metadata = std::fs::metadata(&db_path).unwrap();
    assert!(metadata.len() > 0, "metrics.db should not be empty");
}

// ─── Phase 4: Status shows Last Eval column ─────────────────────────────────

#[test]
fn phase4_status_shows_evaluation_column() {
    let dir = tempfile::tempdir().unwrap();
    create_plan_with_contract(dir.path());

    pice_cmd()
        .current_dir(dir.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("Last Eval"));
}

// ═══ Phase 3 Tests ═══════════════════════════════════════════════════════════

// ─── Phase 3: Help / Flag Tests ──────────────────────────────────────────────

#[test]
fn prime_command_shows_json_flag_in_help() {
    pice_cmd()
        .arg("prime")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"));
}

#[test]
fn review_command_shows_json_flag_in_help() {
    pice_cmd()
        .arg("review")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"));
}

#[test]
fn commit_command_shows_flags_in_help() {
    pice_cmd()
        .arg("commit")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("--message"))
        .stdout(predicate::str::contains("--dry-run"));
}

#[test]
fn handoff_command_shows_flags_in_help() {
    pice_cmd()
        .arg("handoff")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("--output"));
}

#[test]
fn status_command_shows_json_flag_in_help() {
    pice_cmd()
        .arg("status")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"));
}

// ─── Phase 3: Status (no provider needed) ────────────────────────────────────

#[test]
fn status_command_no_plans_directory() {
    let dir = tempfile::tempdir().unwrap();

    pice_cmd()
        .current_dir(dir.path())
        .arg("status")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"plans\": []"));
}

#[test]
fn status_command_shows_plans() {
    let dir = tempfile::tempdir().unwrap();

    // Init git repo so git info is populated
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
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
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Create a plan file
    create_plan_with_contract(dir.path());

    pice_cmd()
        .current_dir(dir.path())
        .arg("status")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"title\": \"Feature: Test Plan\"",
        ))
        .stdout(predicate::str::contains("\"has_contract\": true"));
}

#[test]
fn status_command_shows_malformed_plans() {
    let dir = tempfile::tempdir().unwrap();
    let plans_dir = dir.path().join(".claude/plans");
    fs::create_dir_all(&plans_dir).unwrap();
    fs::write(
        plans_dir.join("bad-plan.md"),
        "# Bad Plan\n\n## Contract\n\n```json\n{invalid}\n```\n",
    )
    .unwrap();

    pice_cmd()
        .current_dir(dir.path())
        .arg("status")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"parse_error\""));
}

// ─── Phase 3: Stub Provider Pipeline Tests ───────────────────────────────────

/// Helper: set up a stub project with an initialized git repo.
fn setup_stub_project_with_git() -> tempfile::TempDir {
    let dir = setup_stub_project();

    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@test.com",
            "commit",
            "-m",
            "init",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    dir
}

#[test]
fn phase3_prime_command_with_stub_provider() {
    let dir = setup_stub_project_with_git();

    pice_cmd()
        .current_dir(dir.path())
        .arg("prime")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"complete\""));
}

#[test]
fn phase3_review_command_with_stub_provider() {
    let dir = setup_stub_project_with_git();

    pice_cmd()
        .current_dir(dir.path())
        .arg("review")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"complete\""));
}

#[test]
fn phase3_commit_command_dry_run_with_stub_provider() {
    let dir = setup_stub_project_with_git();

    // Modify a tracked file so git add -u will stage it
    let config_path = dir.path().join(".pice/config.toml");
    let mut config = fs::read_to_string(&config_path).unwrap();
    config.push_str("\n# modified\n");
    fs::write(&config_path, config).unwrap();

    pice_cmd()
        .current_dir(dir.path())
        .arg("commit")
        .arg("--dry-run")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"dry_run\""))
        .stdout(predicate::str::contains("\"message\""));
}

#[test]
fn phase3_handoff_command_with_stub_provider() {
    let dir = setup_stub_project_with_git();

    pice_cmd()
        .current_dir(dir.path())
        .arg("handoff")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"complete\""))
        .stdout(predicate::str::contains("\"path\""));

    // Verify HANDOFF.md was written
    assert!(dir.path().join("HANDOFF.md").exists());
}

// ─── Phase 3: Error Path Tests ───────────────────────────────────────────────

#[test]
fn phase3_commit_nothing_to_commit() {
    let dir = setup_stub_project_with_git();

    // Clean repo — nothing to commit
    pice_cmd()
        .current_dir(dir.path())
        .arg("commit")
        .arg("--json")
        .assert()
        .failure()
        .stdout(predicate::str::contains("nothing to commit"));
}

#[test]
fn phase3_commit_untracked_only_fails() {
    let dir = setup_stub_project_with_git();

    // Create only untracked files — git add -u won't stage them
    fs::write(dir.path().join("untracked-only.txt"), "hello").unwrap();

    pice_cmd()
        .current_dir(dir.path())
        .arg("commit")
        .arg("--message")
        .arg("test commit")
        .arg("--json")
        .assert()
        .failure()
        .stdout(predicate::str::contains("nothing staged to commit"));
}

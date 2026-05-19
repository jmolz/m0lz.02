//! Integration tests for Phase 2 and Phase 3 commands.
//!
//! These tests use `assert_cmd` to invoke the pice binary with the stub provider.
//! They verify the full CLI pipeline without requiring real API keys.

use assert_cmd::Command;
use pice_core::layers::manifest::manifest_project_namespace;
use predicates::prelude::*;
use std::fs;

const MEMORY_LEAK_SENTINEL: &str = "MEMORY_LEAK_SENTINEL_DO_NOT_INCLUDE";

fn pice_cmd() -> Command {
    let mut cmd = Command::cargo_bin("pice").unwrap();
    // v0.2 Phase 0: commands dispatch through the adapter. Use inline mode
    // so tests don't need a running daemon process.
    cmd.env("PICE_DAEMON_INLINE", "1");
    cmd
}

fn enable_project_memory_with_sentinel(dir: &std::path::Path) {
    let config_path = dir.join(".pice/config.toml");
    let mut config = fs::read_to_string(&config_path).unwrap();
    config.push_str(
        r#"

[memory]
enabled = true
store = "project_learnings"
max_recalled_items = 6
max_tokens = 1200
retention_days = 90
write_after = ["execute", "handoff"]
read_for = ["prime", "plan", "execute"]
"#,
    );
    fs::write(&config_path, config).unwrap();

    write_memory_record_with_body(
        dir,
        "mem_sentinel",
        "2026-05-19T00:00:00Z",
        &format!("Parsed sentinel body: {MEMORY_LEAK_SENTINEL}"),
    );
}

fn enable_private_memory_with_sentinel(dir: &std::path::Path, state_dir: &std::path::Path) {
    let config_path = dir.join(".pice/config.toml");
    let mut config = fs::read_to_string(&config_path).unwrap();
    config.push_str(
        r#"

[memory]
enabled = true
store = "private_state"
max_recalled_items = 6
max_tokens = 1200
retention_days = 90
write_after = ["execute", "handoff"]
read_for = ["prime", "plan", "execute"]
"#,
    );
    fs::write(&config_path, config).unwrap();

    let project_root = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let project_hash = manifest_project_namespace(&project_root);
    let memory_dir = state_dir.join(&project_hash).join("memory");
    fs::create_dir_all(&memory_dir).unwrap();
    fs::write(
        memory_dir.join("records.jsonl"),
        format!(
            "{{\"id\":\"mem_private_sentinel\",\"created_at\":\"2026-05-19T00:00:00Z\",\"source\":\"handoff_summary\",\"store\":\"private_state\",\"project_hash\":\"{project_hash}\",\"redaction_status\":\"clean\",\"title\":\"Private CLI memory test\",\"body\":\"Private sentinel body: {MEMORY_LEAK_SENTINEL}\",\"tags\":[\"durable\"]}}\n"
        ),
    )
    .unwrap();
}

fn enable_private_memory_with_corrupt_state(dir: &std::path::Path, state_dir: &std::path::Path) {
    let config_path = dir.join(".pice/config.toml");
    let mut config = fs::read_to_string(&config_path).unwrap();
    config.push_str(
        r#"

[memory]
enabled = true
store = "private_state"
max_recalled_items = 6
max_tokens = 1200
retention_days = 90
write_after = ["execute", "handoff"]
read_for = ["prime", "plan", "execute"]
"#,
    );
    fs::write(&config_path, config).unwrap();

    let project_root = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let project_hash = manifest_project_namespace(&project_root);
    let memory_dir = state_dir.join(&project_hash).join("memory");
    fs::create_dir_all(&memory_dir).unwrap();
    fs::write(memory_dir.join("records.jsonl"), "not-json\n").unwrap();
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

#[test]
fn daemon_subcommand_shows_actions_in_help() {
    pice_cmd()
        .arg("daemon")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("start"))
        .stdout(predicate::str::contains("stop"))
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("restart"))
        .stdout(predicate::str::contains("logs"));
}

#[test]
fn memory_command_shows_actions_in_help() {
    pice_cmd()
        .arg("memory")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("show"))
        .stdout(predicate::str::contains("prune"))
        .stdout(predicate::str::contains("delete"));
}

fn write_memory_record(dir: &std::path::Path, id: &str, created_at: &str) {
    write_memory_record_with_body(dir, id, created_at, &format!("Body for {id}."));
}

fn write_memory_record_with_body(dir: &std::path::Path, id: &str, created_at: &str, body: &str) {
    let pice_dir = dir.join(".pice");
    fs::create_dir_all(&pice_dir).unwrap();
    let project_root = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let project_hash = manifest_project_namespace(&project_root);
    fs::write(
        pice_dir.join("learnings.md"),
        format!(
            "<!-- pice-memory id=\"{id}\" created_at=\"{created_at}\" source=\"handoff_summary\" store=\"project_learnings\" project_hash=\"{project_hash}\" redaction_status=\"clean\" -->\n### CLI memory test\n\n{body}\n<!-- /pice-memory -->\n"
        ),
    )
    .unwrap();
}

#[test]
fn memory_status_json_works_inline() {
    let dir = setup_stub_project();

    pice_cmd()
        .current_dir(dir.path())
        .arg("memory")
        .arg("status")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains(r#""status": "complete""#))
        .stdout(predicate::str::contains(r#""store": "project_learnings""#));
}

#[test]
fn memory_list_show_delete_json_work_inline() {
    let dir = setup_stub_project();
    write_memory_record(dir.path(), "mem_cli", "2026-05-19T00:00:00Z");

    pice_cmd()
        .current_dir(dir.path())
        .arg("memory")
        .arg("list")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("mem_cli"));

    pice_cmd()
        .current_dir(dir.path())
        .arg("memory")
        .arg("show")
        .arg("mem_cli")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("Body for mem_cli."));

    pice_cmd()
        .current_dir(dir.path())
        .arg("memory")
        .arg("delete")
        .arg("mem_cli")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains(r#""removed": 1"#));
}

#[test]
fn memory_show_missing_json_is_structured_failure() {
    let dir = setup_stub_project();

    pice_cmd()
        .current_dir(dir.path())
        .arg("memory")
        .arg("show")
        .arg("mem_missing")
        .arg("--json")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains(r#""status": "not_found""#))
        .stdout(predicate::str::contains(r#""record_id": "mem_missing""#));
}

#[test]
fn memory_prune_json_uses_before_boundary_inline() {
    let dir = setup_stub_project();
    write_memory_record(dir.path(), "mem_old", "2026-05-18T00:00:00Z");

    pice_cmd()
        .current_dir(dir.path())
        .arg("memory")
        .arg("prune")
        .arg("--before")
        .arg("2026-05-19")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            r#""before": "2026-05-19T00:00:00Z""#,
        ))
        .stdout(predicate::str::contains(r#""removed": 1"#));
}

// ─── Error Path Tests ──────────────────────────────────────────────────────
// v0.2 Phase 0: commands dispatch through adapter → daemon stubs.
// These tests verify v0.1 behavior; re-enable when daemon handlers are ported.

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

#[test]
fn execute_plan_without_contract_fails_json() {
    let dir = setup_stub_project();
    let plans_dir = dir.path().join(".codex/plans");
    fs::create_dir_all(&plans_dir).unwrap();
    let plan_path = plans_dir.join("no-contract.md");
    fs::write(
        &plan_path,
        "# Feature: No Contract\n\n## Overview\nJust text.\n",
    )
    .unwrap();

    pice_cmd()
        .current_dir(dir.path())
        .arg("execute")
        .arg("--json")
        .arg(".codex/plans/no-contract.md")
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("plan-contract-required"))
        .stdout(predicate::str::contains(".codex/plans/no-contract.md"));
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
fn evaluate_create_payloads_do_not_include_enabled_memory() {
    let dir = setup_stub_project();
    enable_project_memory_with_sentinel(dir.path());
    let plan_path = create_plan_with_contract(dir.path());

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

    let log_path = dir.path().join("evaluate-requests.jsonl");
    pice_cmd()
        .current_dir(dir.path())
        .env("PICE_STUB_REQUEST_LOG", &log_path)
        .arg("evaluate")
        .arg(plan_path.to_string_lossy().to_string())
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"passed\": true"));

    let log = fs::read_to_string(&log_path).expect("stub request log");
    assert!(
        log.lines().filter(|line| !line.trim().is_empty()).count() >= 2,
        "tier 2 evaluation should record primary and adversarial evaluate/create payloads"
    );
    assert!(
        !log.contains(MEMORY_LEAK_SENTINEL),
        "evaluate/create payload leaked enabled memory: {log}"
    );
    assert!(
        !log.contains("Approved Project Memory"),
        "evaluate/create payload included memory section: {log}"
    );
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

// ─── Phase 4: Corrupt DB resilience ──────────────────────────────────────────

#[test]

fn phase4_metrics_with_corrupt_db() {
    let dir = tempfile::tempdir().unwrap();
    let pice_dir = dir.path().join(".pice");
    fs::create_dir_all(&pice_dir).unwrap();

    // Write garbage to metrics.db
    fs::write(pice_dir.join("metrics.db"), "THIS IS NOT SQLITE").unwrap();
    // Write a valid config so open_metrics_db resolves the path
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

    // pice metrics reports the error (exit 1) for corrupt DB — this is correct
    // for a reporting command. The non-fatal guarantee is for *workflow* commands.
    pice_cmd()
        .current_dir(dir.path())
        .arg("metrics")
        .arg("--json")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a database"));

    // But pice status (which uses the non-fatal pattern) should succeed
    pice_cmd()
        .current_dir(dir.path())
        .arg("status")
        .arg("--json")
        .assert()
        .success();
}

// ─── Phase 4: init --force preserves metrics data ───────────────────────────

#[test]

fn phase4_init_force_preserves_metrics_history() {
    let dir = tempfile::tempdir().unwrap();

    // First init creates DB
    pice_cmd()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();

    let db_path = dir.path().join(".pice/metrics.db");
    assert!(db_path.exists());

    // Insert a row directly into the DB
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "INSERT INTO evaluations (plan_path, feature_name, tier, passed, primary_provider, primary_model, summary, timestamp)
         VALUES ('test.md', 'Test', 1, 1, 'c', 'm', NULL, '2026-04-01T00:00:00Z')",
        [],
    )
    .unwrap();
    drop(conn);

    // Re-init with --force
    pice_cmd()
        .current_dir(dir.path())
        .arg("init")
        .arg("--force")
        .assert()
        .success();

    // Verify the data is still there
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM evaluations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1, "init --force should preserve metrics history");
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
fn status_command_shows_codex_plans() {
    let dir = tempfile::tempdir().unwrap();
    let plans_dir = dir.path().join(".codex/plans");
    fs::create_dir_all(&plans_dir).unwrap();
    fs::write(
        plans_dir.join("codex-plan.md"),
        r#"# Feature: Codex Plan

## Contract

```json
{
  "feature": "Codex Plan",
  "tier": 2,
  "pass_threshold": 8,
  "criteria": [
    { "name": "Tests pass", "threshold": 8, "validation": "cargo test" }
  ]
}
```
"#,
    )
    .unwrap();

    pice_cmd()
        .current_dir(dir.path())
        .arg("status")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains(".codex/plans/codex-plan.md"))
        .stdout(predicate::str::contains(
            "\"title\": \"Feature: Codex Plan\"",
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

fn setup_stub_project_with_git_and_memory() -> tempfile::TempDir {
    let dir = setup_stub_project();
    enable_project_memory_with_sentinel(dir.path());

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

fn assert_log_includes_enabled_memory(log: &str, command: &str) {
    assert!(log.contains(r#""method":"session/send""#));
    assert!(
        log.contains(MEMORY_LEAK_SENTINEL),
        "{command} prompt did not include enabled memory for an allowed consumer: {log}"
    );
    assert!(
        log.contains("Approved Project Memory"),
        "{command} prompt did not include the memory section for an allowed consumer: {log}"
    );
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
fn prime_prompt_includes_enabled_memory_for_allowed_consumer() {
    let dir = setup_stub_project_with_git_and_memory();

    let log_path = dir.path().join("prime-requests.jsonl");
    pice_cmd()
        .current_dir(dir.path())
        .env("PICE_STUB_REQUEST_LOG", &log_path)
        .arg("prime")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"complete\""));

    let log = fs::read_to_string(&log_path).expect("stub request log");
    assert_log_includes_enabled_memory(&log, "prime");
}

#[test]
fn plan_prompt_includes_enabled_memory_for_allowed_consumer() {
    let dir = setup_stub_project_with_git_and_memory();

    let log_path = dir.path().join("plan-requests.jsonl");
    pice_cmd()
        .current_dir(dir.path())
        .env("PICE_STUB_REQUEST_LOG", &log_path)
        .arg("plan")
        .arg("test feature")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"complete\""));

    let log = fs::read_to_string(&log_path).expect("stub request log");
    assert_log_includes_enabled_memory(&log, "plan");
}

#[test]
fn execute_prompt_includes_enabled_memory_for_allowed_consumer() {
    let dir = setup_stub_project_with_git_and_memory();
    let plan_path = create_plan_with_contract(dir.path());

    let log_path = dir.path().join("execute-requests.jsonl");
    pice_cmd()
        .current_dir(dir.path())
        .env("PICE_STUB_REQUEST_LOG", &log_path)
        .arg("execute")
        .arg(plan_path.to_string_lossy().to_string())
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"complete\""));

    let log = fs::read_to_string(&log_path).expect("stub request log");
    assert_log_includes_enabled_memory(&log, "execute");
}

#[test]
fn private_state_prime_prompt_uses_pice_state_dir_namespace() {
    let dir = setup_stub_project_with_git();
    let state_dir = tempfile::tempdir().unwrap();
    enable_private_memory_with_sentinel(dir.path(), state_dir.path());

    let project_root = dir.path().canonicalize().unwrap();
    let project_hash = manifest_project_namespace(&project_root);
    assert!(
        state_dir
            .path()
            .join(&project_hash)
            .join("memory")
            .join("records.jsonl")
            .exists(),
        "private sentinel fixture must live under PICE_STATE_DIR/<project_hash>/memory"
    );

    let log_path = dir.path().join("prime-private-requests.jsonl");
    pice_cmd()
        .current_dir(dir.path())
        .env("PICE_STATE_DIR", state_dir.path())
        .env("PICE_STUB_REQUEST_LOG", &log_path)
        .arg("prime")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"complete\""));

    let log = fs::read_to_string(&log_path).expect("stub request log");
    assert_log_includes_enabled_memory(&log, "prime private_state");
}

#[test]
fn corrupt_private_state_prime_uses_empty_memory_brief_instead_of_failing() {
    let dir = setup_stub_project_with_git();
    let state_dir = tempfile::tempdir().unwrap();
    enable_private_memory_with_corrupt_state(dir.path(), state_dir.path());

    let log_path = dir.path().join("prime-corrupt-private-requests.jsonl");
    pice_cmd()
        .current_dir(dir.path())
        .env("PICE_STATE_DIR", state_dir.path())
        .env("PICE_STUB_REQUEST_LOG", &log_path)
        .arg("prime")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"complete\""));

    let log = fs::read_to_string(&log_path).expect("stub request log");
    assert!(log.contains(r#""method":"session/send""#));
    assert!(
        !log.contains("Approved Project Memory"),
        "corrupt private-state recall should use an empty brief: {log}"
    );
}

#[test]
fn disabled_memory_execute_does_not_require_state_dir_home() {
    let dir = setup_stub_project_with_git();
    let plan_path = create_plan_with_contract(dir.path());

    pice_cmd()
        .current_dir(dir.path())
        .env_remove("PICE_STATE_DIR")
        .env_remove("HOME")
        .env_remove("USERPROFILE")
        .arg("execute")
        .arg(plan_path.to_string_lossy().to_string())
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"complete\""));
}

#[test]
fn disabled_memory_handoff_does_not_require_state_dir_home() {
    let dir = setup_stub_project_with_git();

    pice_cmd()
        .current_dir(dir.path())
        .env_remove("PICE_STATE_DIR")
        .env_remove("HOME")
        .env_remove("USERPROFILE")
        .arg("handoff")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"complete\""));
}

#[test]
fn review_prompt_does_not_include_enabled_memory() {
    let dir = setup_stub_project_with_git_and_memory();
    fs::write(dir.path().join("review-change.txt"), "review me").unwrap();

    let log_path = dir.path().join("review-requests.jsonl");
    pice_cmd()
        .current_dir(dir.path())
        .env("PICE_STUB_REQUEST_LOG", &log_path)
        .arg("review")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"complete\""));

    let log = fs::read_to_string(&log_path).expect("stub request log");
    assert!(log.contains(r#""method":"session/send""#));
    assert!(
        !log.contains(MEMORY_LEAK_SENTINEL),
        "review prompt leaked enabled memory: {log}"
    );
    assert!(
        !log.contains("Approved Project Memory"),
        "review prompt included memory section: {log}"
    );
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
fn commit_prompt_does_not_include_enabled_memory() {
    let dir = setup_stub_project_with_git_and_memory();

    let config_path = dir.path().join(".pice/config.toml");
    let mut config = fs::read_to_string(&config_path).unwrap();
    config.push_str("\n# commit isolation change\n");
    fs::write(&config_path, config).unwrap();

    let log_path = dir.path().join("commit-requests.jsonl");
    pice_cmd()
        .current_dir(dir.path())
        .env("PICE_STUB_REQUEST_LOG", &log_path)
        .arg("commit")
        .arg("--dry-run")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"dry_run\""));

    let log = fs::read_to_string(&log_path).expect("stub request log");
    assert!(log.contains(r#""method":"session/send""#));
    assert!(
        !log.contains(MEMORY_LEAK_SENTINEL),
        "commit prompt leaked enabled memory: {log}"
    );
    assert!(
        !log.contains("Approved Project Memory"),
        "commit prompt included memory section: {log}"
    );
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
    // Exit response prints message to stderr via render_response
    pice_cmd()
        .current_dir(dir.path())
        .arg("commit")
        .arg("--json")
        .assert()
        .failure()
        .stderr(predicate::str::contains("nothing staged to commit"));
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
        .stderr(predicate::str::contains("nothing staged to commit"));
}

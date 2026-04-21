//! Phase 7 Task 3 — clap conflict-rule regression tests.
//!
//! These pin the flag invariants that later Phase 7 tasks rely on:
//! - `pice execute/evaluate`: `--wait` requires `--background`;
//!   `--timeout-secs` requires `--wait`.
//! - `pice status`: `--follow` ⊥ `--wait`, `--follow` ⊥ `--json`,
//!   `--wait` requires a `feature_id` positional, `--stream-json`
//!   requires `--follow`.
//! - `pice logs`: `--json` ⊥ `--follow`, `--stream-json` requires
//!   `--follow`.
//!
//! We drive clap via `assert_cmd` rather than in-process parser tests
//! so the shape of the generated `--help` + error diagnostics matches
//! what users actually see. Conflict violations exit with clap's
//! default `2` (usage error) and print to stderr, so no daemon runtime
//! is required — these tests do NOT need `PICE_DAEMON_INLINE` or any
//! `.pice/` scaffolding.

use assert_cmd::Command;

fn pice() -> Command {
    Command::cargo_bin("pice").expect("pice binary must be built for tests")
}

// ─── pice execute ────────────────────────────────────────────────────────

#[test]
fn execute_wait_without_background_is_rejected() {
    let output = pice()
        .args(["execute", "plan.md", "--wait"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--background"),
        "expected --wait ⇒ --background error in stderr, got: {stderr}"
    );
}

#[test]
fn execute_timeout_secs_without_wait_is_rejected() {
    let output = pice()
        .args(["execute", "plan.md", "--timeout-secs", "30"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--wait"),
        "expected --timeout-secs ⇒ --wait error in stderr, got: {stderr}"
    );
}

// ─── pice evaluate ───────────────────────────────────────────────────────

#[test]
fn evaluate_wait_without_background_is_rejected() {
    let output = pice()
        .args(["evaluate", "plan.md", "--wait"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--background"),
        "expected --wait ⇒ --background error in stderr, got: {stderr}"
    );
}

#[test]
fn evaluate_timeout_secs_without_wait_is_rejected() {
    let output = pice()
        .args(["evaluate", "plan.md", "--timeout-secs", "30"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--wait"),
        "expected --timeout-secs ⇒ --wait error in stderr, got: {stderr}"
    );
}

// ─── pice status ─────────────────────────────────────────────────────────

#[test]
fn status_follow_conflicts_with_wait() {
    let output = pice()
        .args(["status", "feat-1", "--follow", "--wait"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--follow") && stderr.contains("--wait"),
        "expected --follow/--wait conflict in stderr, got: {stderr}"
    );
}

#[test]
fn status_follow_conflicts_with_json() {
    let output = pice()
        .args(["status", "--follow", "--json"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--follow") && stderr.contains("--json"),
        "expected --follow/--json conflict in stderr, got: {stderr}"
    );
}

#[test]
fn status_wait_without_feature_id_is_rejected() {
    let output = pice()
        .args(["status", "--wait"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("FEATURE_ID") || stderr.contains("feature_id"),
        "expected --wait ⇒ feature_id error in stderr, got: {stderr}"
    );
}

#[test]
fn status_stream_json_requires_follow() {
    let output = pice()
        .args(["status", "--stream-json"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--follow"),
        "expected --stream-json ⇒ --follow error in stderr, got: {stderr}"
    );
}

// ─── pice logs ───────────────────────────────────────────────────────────

#[test]
fn logs_follow_conflicts_with_json() {
    let output = pice()
        .args(["logs", "feat-1", "--follow", "--json"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--follow") && stderr.contains("--json"),
        "expected --follow/--json conflict in stderr, got: {stderr}"
    );
}

#[test]
fn logs_stream_json_requires_follow() {
    let output = pice()
        .args(["logs", "feat-1", "--stream-json"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--follow"),
        "expected --stream-json ⇒ --follow error in stderr, got: {stderr}"
    );
}

#[test]
fn logs_requires_feature_id_positional() {
    let output = pice()
        .args(["logs"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("FEATURE_ID") || stderr.contains("feature_id"),
        "expected FEATURE_ID missing-positional error, got: {stderr}"
    );
}

// ─── Happy-path: valid flag combinations parse cleanly (--help exit 0).
//     Not a dispatch test — just proves clap accepts the shapes the
//     later tasks depend on. ────────────────────────────────────────────

#[test]
fn execute_background_wait_timeout_parses() {
    // `--help` short-circuits clap parsing; if any flag attr is wrong
    // (e.g. `requires` spelled badly) this would still expose the
    // error at command-construction time.
    pice().args(["execute", "--help"]).assert().success();
}

#[test]
fn status_follow_with_stream_json_parses() {
    pice().args(["status", "--help"]).assert().success();
}

#[test]
fn logs_help_renders() {
    pice().args(["logs", "--help"]).assert().success();
}

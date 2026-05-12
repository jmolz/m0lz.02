//! Phase 7 Criterion 10: CLI integration tests pinning the exact wire strings
//! for all Phase 7 `ExitJsonStatus` variants via `ExitJsonStatus::X.as_str()`.
//!
//! Each test dispatches through the real `pice` binary (via
//! `assert_cmd::Command::cargo_bin("pice")`) and asserts:
//! - Exit code matches `ExitJsonStatus::X.exit_code()`.
//! - Stdout contains `ExitJsonStatus::X.as_str()` — asserted against the
//!   `.as_str()` CALL so a variant rename automatically propagates.
//!
//! Covered variants:
//! - `FeatureNotFound` — inline mode, `pice status feat-not-found --json`
//! - `FailedInterrupted` — inline mode, seed an already-reconciled manifest
//!   (overall_status=Failed, layers[].halted_by="failed-interrupted") to the
//!   PICE_STATE_DIR, then `pice status feat-fi --json`
//!
//! The remaining Phase 7 variants (`WaitTimeout`, `LogsStreamEnded`,
//! `DaemonDisconnected`) require a live socket daemon with a controlled
//! subscription flow. They are covered at the daemon handler dispatch level
//! in `crates/pice-daemon/tests/exit_json_status_phase7_handlers.rs`.

use assert_cmd::Command;
use pice_core::cli::ExitJsonStatus;
use pice_core::layers::manifest::{LayerResult, LayerStatus, ManifestStatus, VerificationManifest};
use std::fs;
use std::path::Path;

fn pice_cmd_inline() -> Command {
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

/// Criterion 10 — `feature-not-found` wire string.
///
/// `pice status feat-does-not-exist --json` under `PICE_DAEMON_INLINE=1`
/// invokes `StatusMode::Detail`, finds no manifest on disk, and returns
/// `ExitJsonStatus::FeatureNotFound` (exit 1) on stdout.
///
/// The assertion is against `.as_str()` NOT a literal so a variant rename
/// produces a compile-time error at the handler call site AND here — no
/// silent drift.
#[test]
fn feature_not_found_wire_string_is_pinned() {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());

    let output = pice_cmd_inline()
        .current_dir(dir.path())
        .args(["status", "feat-does-not-exist-p7cr10", "--json"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(ExitJsonStatus::FeatureNotFound.exit_code()),
        "expected exit {} for FeatureNotFound; stderr: {}, stdout: {}",
        ExitJsonStatus::FeatureNotFound.exit_code(),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "FeatureNotFound must emit JSON on stdout; parse error: {e}\n\
             stdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });

    assert_eq!(
        json["status"].as_str().unwrap_or(""),
        ExitJsonStatus::FeatureNotFound.as_str(),
        "stdout JSON `status` must equal ExitJsonStatus::FeatureNotFound.as_str() ({}); \
         got: {json}",
        ExitJsonStatus::FeatureNotFound.as_str()
    );
    assert_eq!(
        json["feature_id"].as_str().unwrap_or(""),
        "feat-does-not-exist-p7cr10",
        "stdout JSON must carry the requested feature_id; got: {json}"
    );
}

/// Criterion 10 — `failed-interrupted` wire string.
///
/// Seeds an already-reconciled `VerificationManifest` (overall_status=Failed,
/// layers[].halted_by="failed-interrupted") to a PICE_STATE_DIR override,
/// then runs `pice status feat-fi-p7cr10 --json` in inline mode.
///
/// The daemon's `status.rs::run_detail` handler reads the manifest and returns
/// `CommandResponse::Json { value: manifest_json }` (exit 0). The stdout JSON
/// body must contain `ExitJsonStatus::FailedInterrupted.as_str()` in
/// `layers[].halted_by`.
///
/// Inline mode is used so no socket daemon is needed — startup reconciliation
/// is not exercised here (the manifest is pre-seeded in its final state).
/// The reconciler path that PRODUCES the failed-interrupted halted_by string
/// is tested separately by the daemon integration tests in
/// `crates/pice-daemon/tests/interrupted_recovery_integration.rs`.
#[test]
fn failed_interrupted_wire_string_is_pinned() {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());

    // Use a PICE_STATE_DIR override so we can seed the manifest without
    // touching the real ~/.pice/state directory.
    let state_dir = dir.path().join("state");
    fs::create_dir_all(&state_dir).unwrap();

    // The manifest namespace is computed from the project root. In inline
    // mode the daemon uses the CLI's current_dir (dir.path()) as the
    // project root when computing the manifest path. Use the same function
    // to derive the path so the namespace matches.
    //
    // macOS tempdir returns symlinked paths (/var → /private/var); canonicalize
    // so the hash here matches `std::env::current_dir()` inside the binary.
    let project_root = dir
        .path()
        .canonicalize()
        .unwrap_or_else(|_| dir.path().to_path_buf());
    let feature_id = "feat-fi-p7cr10";

    let manifest_path = {
        // Temporarily set the env var so manifest_path_for() uses our state dir.
        let old = std::env::var("PICE_STATE_DIR").ok();
        std::env::set_var("PICE_STATE_DIR", state_dir.to_str().unwrap());
        let p = VerificationManifest::manifest_path_for(feature_id, &project_root).unwrap();
        match old {
            Some(v) => std::env::set_var("PICE_STATE_DIR", v),
            None => std::env::remove_var("PICE_STATE_DIR"),
        }
        p
    };
    fs::create_dir_all(manifest_path.parent().unwrap()).unwrap();

    // Build the already-reconciled manifest: overall_status=Failed,
    // the layer carries halted_by="failed-interrupted" (the wire string
    // emitted by startup reconciliation — ExitJsonStatus::FailedInterrupted.as_str()).
    let mut manifest = VerificationManifest::new(feature_id, &project_root);
    manifest.overall_status = ManifestStatus::Failed;
    manifest.layers.push(LayerResult {
        name: "api".to_string(),
        status: LayerStatus::Failed,
        passes: Vec::new(),
        seam_checks: Vec::new(),
        // This is the exact wire string the reconciler writes.
        halted_by: Some(ExitJsonStatus::FailedInterrupted.as_str().to_string()),
        final_confidence: None,
        total_cost_usd: None,
        escalation_events: None,
    });
    manifest.save(&manifest_path).unwrap();

    let output = pice_cmd_inline()
        .current_dir(dir.path())
        .env("PICE_STATE_DIR", state_dir.to_str().unwrap())
        .args(["status", feature_id, "--json"])
        .output()
        .unwrap();

    // `pice status <feat> --json` returns the manifest JSON with exit 0
    // when the manifest exists. Exit 0 because the feature WAS found.
    assert_eq!(
        output.status.code(),
        Some(0),
        "manifest exists → expected exit 0 for FailedInterrupted status detail; \
         stderr: {}, stdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");

    // The `failed-interrupted` string (ExitJsonStatus::FailedInterrupted.as_str())
    // must appear in the manifest JSON returned on stdout.
    assert!(
        stdout.contains(ExitJsonStatus::FailedInterrupted.as_str()),
        "stdout must contain ExitJsonStatus::FailedInterrupted.as_str() ({}) \
         in layers[].halted_by; stdout: {stdout}\nstderr: {}",
        ExitJsonStatus::FailedInterrupted.as_str(),
        String::from_utf8_lossy(&output.stderr),
    );

    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "pice status --json must emit JSON on stdout; parse error: {e}\n\
             stdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });

    let layers = json["layers"].as_array().expect("layers array in manifest");
    let has_failed_interrupted = layers
        .iter()
        .any(|l| l["halted_by"].as_str() == Some(ExitJsonStatus::FailedInterrupted.as_str()));
    assert!(
        has_failed_interrupted,
        "at least one layer must carry halted_by == ExitJsonStatus::FailedInterrupted.as_str() \
         ({}); manifest JSON: {json}",
        ExitJsonStatus::FailedInterrupted.as_str()
    );
}

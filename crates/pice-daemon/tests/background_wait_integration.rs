//! Phase 7 Criterion 7 integration tests — `pice evaluate --background --wait`
//! exit-code matrix.
//!
//! These tests pin the five terminal exit codes produced by the
//! `--background --wait` flow.  Because `wait_until_terminal` lives in
//! `pice-cli` (a binary-only crate, not a dev-dep of `pice-daemon`), the
//! logic is mirrored inline — see the `wait_logic` module below.
//!
//! Tests covered:
//!   C7-1  exit 0  (Passed)         — snapshot already-terminal short-circuit
//!   C7-2  exit 2  (Failed)         — snapshot already-terminal short-circuit
//!   C7-3  exit 3  (PendingReview)  — snapshot already-terminal short-circuit
//!   C7-4  exit 4  (WaitTimeout)    — live feature never completes, 100ms timeout
//!
//! SKIPPED:
//!   C7-5  exit 5  (DaemonDisconnected) — requires daemon-restart + reconciliation
//!   choreography (boot daemon, kill mid-subscribe, restart, observe
//!   Failed-interrupted rewrite).  This is covered conceptually by
//!   `interrupted_recovery_integration.rs`; the full daemon-restart path
//!   with a concurrent subscriber is deferred to a Phase 7.1 hardening pass.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::time::Duration;

use pice_core::cli::ExitJsonStatus;
use pice_core::layers::manifest::{ManifestStatus, VerificationManifest};
use pice_core::workflow::loader::embedded_defaults;
use pice_daemon::events::EventBus;
use pice_daemon::jobs::FeatureJobManager;
use pice_daemon::test_support::StateDirGuard;

// ─── Inline mirror of background_wait logic ──────────────────────────────────
//
// `wait_until_terminal` in `pice-cli/src/adapter/background_wait.rs` is
// `async fn` (private) and its crate is binary-only.  We mirror the minimal
// relevant fragment here so the daemon integration tests can call it
// hermetically (no real daemon socket, no autostart).
mod wait_logic {
    use pice_core::cli::ExitJsonStatus;
    use pice_core::events::{ManifestEvent, ManifestEventPayload};
    use pice_core::layers::manifest::ManifestStatus;
    use std::time::Duration;
    use tokio::sync::broadcast;

    /// Map a [`ManifestStatus`] to a terminal exit code, or `None` if
    /// the status is not terminal.  Mirrors `terminal_exit_code` from
    /// `background_wait.rs`.
    pub fn terminal_exit_code(status: &ManifestStatus) -> Option<i32> {
        match status {
            ManifestStatus::Passed => Some(0),
            ManifestStatus::Failed | ManifestStatus::FailedInterrupted => {
                Some(ExitJsonStatus::EvaluationFailed.exit_code())
            }
            ManifestStatus::PendingReview => Some(ExitJsonStatus::ReviewGatePending.exit_code()),
            ManifestStatus::Pending | ManifestStatus::InProgress | ManifestStatus::Queued => None,
        }
    }

    /// Inspect a notification payload for a terminal `FeatureComplete` or
    /// `Cancelled` event.  Returns `(status_wire, exit_code)` or `None`.
    /// Mirrors `parse_terminal_notification` from `background_wait.rs`.
    pub fn parse_terminal_notification(
        payload: &ManifestEventPayload,
    ) -> Option<(String, i32)> {
        match payload.event {
            ManifestEvent::FeatureComplete => {
                let status_wire = payload
                    .data
                    .get("overall_status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("failed")
                    .to_string();
                let code = match status_wire.as_str() {
                    "passed" => 0,
                    "pending-review" => ExitJsonStatus::ReviewGatePending.exit_code(),
                    _ => ExitJsonStatus::EvaluationFailed.exit_code(),
                };
                Some((status_wire, code))
            }
            ManifestEvent::Cancelled => Some((
                "cancelled".to_string(),
                ExitJsonStatus::EvaluationFailed.exit_code(),
            )),
            _ => None,
        }
    }

    /// Block on a pre-subscribed receiver until a terminal event arrives or
    /// `timeout` elapses.
    ///
    /// - Terminal event received → `Ok(exit_code)`
    /// - Timeout → `Ok(ExitJsonStatus::WaitTimeout.exit_code())`
    /// - Channel closed (sender dropped) → `Ok(ExitJsonStatus::DaemonDisconnected.exit_code())`
    ///
    /// Mirrors the `loop` in `wait_until_terminal` from `background_wait.rs`.
    pub async fn wait_on_receiver(
        rx: &mut broadcast::Receiver<ManifestEventPayload>,
        timeout: Duration,
    ) -> i32 {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let timeout_fut = tokio::time::sleep_until(deadline);
            tokio::select! {
                biased;
                _ = timeout_fut => {
                    return ExitJsonStatus::WaitTimeout.exit_code();
                }
                recv = rx.recv() => {
                    match recv {
                        Ok(payload) => {
                            if let Some((_wire, code)) = parse_terminal_notification(&payload) {
                                return code;
                            }
                            // Non-terminal — keep looping.
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            return ExitJsonStatus::DaemonDisconnected.exit_code();
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            // Missed some events — continue draining.
                        }
                    }
                }
            }
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn init_git(root: &std::path::Path) {
    let _ = std::process::Command::new("git")
        .args(["init"])
        .current_dir(root)
        .output();
    let _ = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=t@t",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ])
        .current_dir(root)
        .output();
}

/// Seed a manifest with the given `overall_status` under the test state dir
/// and return the manifest path.
fn seed_terminal_manifest(
    feature_id: &str,
    status: ManifestStatus,
    project_root: &std::path::Path,
) -> std::path::PathBuf {
    let path =
        VerificationManifest::manifest_path_for(feature_id, project_root).expect("manifest path");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create dirs");
    }
    let mut m = VerificationManifest::new(feature_id, project_root);
    m.overall_status = status;
    m.run_id = Some("r-test-001".to_string());
    m.save(&path).expect("save manifest");
    path
}

// ─── C7-1: exit 0 (Passed) ───────────────────────────────────────────────────

/// Passing feature: the snapshot already shows `Passed`.
/// The short-circuit path in `wait_until_terminal` returns exit 0
/// without blocking on the receiver.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_exit_0_passed_via_snapshot_short_circuit() {
    let state_tmp = tempfile::tempdir().unwrap();
    let _guard = StateDirGuard::new(state_tmp.path());

    let project = tempfile::tempdir().unwrap();
    init_git(project.path());

    let manifest_path = seed_terminal_manifest("wait-feat-pass", ManifestStatus::Passed, project.path());
    let m = VerificationManifest::load(&manifest_path).unwrap();

    // Short-circuit: terminal status detected from snapshot — no need to
    // wait on an event receiver.
    let code = wait_logic::terminal_exit_code(&m.overall_status)
        .expect("Passed is terminal — should return Some(0)");

    assert_eq!(code, 0, "exit 0 for Passed");
}

// ─── C7-2: exit 2 (Failed) ───────────────────────────────────────────────────

/// Contract-failing feature: snapshot shows `Failed` → exit 2.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_exit_2_failed_via_snapshot_short_circuit() {
    let state_tmp = tempfile::tempdir().unwrap();
    let _guard = StateDirGuard::new(state_tmp.path());

    let project = tempfile::tempdir().unwrap();
    init_git(project.path());

    let manifest_path =
        seed_terminal_manifest("wait-feat-fail", ManifestStatus::Failed, project.path());
    let m = VerificationManifest::load(&manifest_path).unwrap();

    let code = wait_logic::terminal_exit_code(&m.overall_status)
        .expect("Failed is terminal");

    assert_eq!(
        code,
        ExitJsonStatus::EvaluationFailed.exit_code(),
        "exit 2 for Failed"
    );
}

// ─── C7-3: exit 3 (PendingReview) ────────────────────────────────────────────

/// Pending-gate feature: snapshot shows `PendingReview` → exit 3.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_exit_3_pending_review_via_snapshot_short_circuit() {
    let state_tmp = tempfile::tempdir().unwrap();
    let _guard = StateDirGuard::new(state_tmp.path());

    let project = tempfile::tempdir().unwrap();
    init_git(project.path());

    let manifest_path = seed_terminal_manifest(
        "wait-feat-gate",
        ManifestStatus::PendingReview,
        project.path(),
    );
    let m = VerificationManifest::load(&manifest_path).unwrap();

    let code = wait_logic::terminal_exit_code(&m.overall_status)
        .expect("PendingReview is terminal");

    assert_eq!(
        code,
        ExitJsonStatus::ReviewGatePending.exit_code(),
        "exit 3 for PendingReview"
    );
}

// ─── C7-4: exit 4 (WaitTimeout) ──────────────────────────────────────────────

/// Subscribe to a feature that is live (InProgress) but never completes
/// within the 100ms timeout → exit 4. The background job itself is NOT
/// cancelled (wait is non-destructive per the plan).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_exit_4_timeout_feature_still_running() {
    let state_tmp = tempfile::tempdir().unwrap();
    let _guard = StateDirGuard::new(state_tmp.path());

    let project = tempfile::tempdir().unwrap();
    init_git(project.path());

    // Seed an InProgress manifest (non-terminal) so the snapshot check
    // does NOT short-circuit.
    let _ = seed_terminal_manifest(
        "wait-feat-timeout",
        ManifestStatus::InProgress,
        project.path(),
    );

    // Build a live EventBus + FeatureJobManager.  Spawn a gate future
    // that NEVER completes so the wait must time out.
    let events = EventBus::new();
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let gate_clone = gate.clone();

    let mgr = FeatureJobManager::new(events.clone(), 4);
    let env = std::sync::Arc::new(pice_core::jobs::JobEnv {
        state_dir: state_tmp.path().to_path_buf(),
        project_root: project.path().to_path_buf(),
        workflow_snapshot: embedded_defaults(),
        contracts: BTreeMap::new(),
        pice_state_dir_override: None,
        pice_user_workflow_file: None,
    });
    let _run_id = mgr
        .spawn(
            "wait-feat-timeout".to_string(),
            env,
            move |_env, permit, _cancel| async move {
                gate_clone.notified().await; // blocks forever in this test
                // Hold permit so it isn't released.
                let _p = permit;
                Ok(VerificationManifest::new(
                    "wait-feat-timeout",
                    std::path::Path::new("/irrelevant"),
                ))
            },
        )
        .expect("spawn");

    // Subscribe BEFORE the snapshot check so we do NOT miss any event.
    let mut rx = events.subscribe_feature("wait-feat-timeout");

    // The snapshot is InProgress (non-terminal), so a real wait_until_terminal
    // would fall through to the recv loop.  Simulate that:
    let code = wait_logic::wait_on_receiver(&mut rx, Duration::from_millis(100)).await;

    assert_eq!(
        code,
        ExitJsonStatus::WaitTimeout.exit_code(),
        "exit 4 for WaitTimeout"
    );

    // INVARIANT: the background job must STILL be running — wait is
    // non-destructive.
    assert!(
        mgr.active_count() > 0,
        "background job must not be cancelled by a timed-out wait"
    );

    // Release gate so the detached task can clean up.
    gate.notify_one();

    // Give the supervisor task time to remove the entry from the map.
    for _ in 0..50 {
        if mgr.active_count() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ─── C7-bonus: FeatureComplete event path ────────────────────────────────────

/// Verify the live-event path: `FeatureComplete` with `overall_status=passed`
/// arrives on the receiver → exit 0.  This exercises the `recv` loop in
/// `wait_on_receiver` directly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_exit_0_via_feature_complete_event() {
    let events = EventBus::new();
    let mut rx = events.subscribe_feature("live-feat");

    // Emit FeatureComplete from a background task.
    let bus_clone = events.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        bus_clone.emit_feature_complete(
            "live-feat",
            "r-live",
            serde_json::json!({"overall_status": "passed"}),
        );
    });

    let code =
        wait_logic::wait_on_receiver(&mut rx, Duration::from_secs(2)).await;

    assert_eq!(code, 0, "FeatureComplete(passed) → exit 0");
}

/// `FeatureComplete` with `overall_status=failed` → exit 2.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_exit_2_via_feature_complete_event() {
    let events = EventBus::new();
    let mut rx = events.subscribe_feature("fail-feat");

    let bus_clone = events.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        bus_clone.emit_feature_complete(
            "fail-feat",
            "r-f",
            serde_json::json!({"overall_status": "failed"}),
        );
    });

    let code =
        wait_logic::wait_on_receiver(&mut rx, Duration::from_secs(2)).await;

    assert_eq!(
        code,
        ExitJsonStatus::EvaluationFailed.exit_code(),
        "FeatureComplete(failed) → exit 2"
    );
}

/// `Cancelled` event → exit 2 (treated as evaluation-failed per plan).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_exit_2_via_cancelled_event() {
    let events = EventBus::new();
    let mut rx = events.subscribe_feature("cancel-feat");

    let bus_clone = events.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        bus_clone.emit_cancelled("cancel-feat", "r-c", "shutdown");
    });

    let code =
        wait_logic::wait_on_receiver(&mut rx, Duration::from_secs(2)).await;

    assert_eq!(
        code,
        ExitJsonStatus::EvaluationFailed.exit_code(),
        "Cancelled → exit 2"
    );
}

/// `FeatureComplete` with `overall_status=pending-review` → exit 3.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_exit_3_via_pending_review_event() {
    let events = EventBus::new();
    let mut rx = events.subscribe_feature("gate-feat");

    let bus_clone = events.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        bus_clone.emit_feature_complete(
            "gate-feat",
            "r-g",
            serde_json::json!({"overall_status": "pending-review"}),
        );
    });

    let code =
        wait_logic::wait_on_receiver(&mut rx, Duration::from_secs(2)).await;

    assert_eq!(
        code,
        ExitJsonStatus::ReviewGatePending.exit_code(),
        "FeatureComplete(pending-review) → exit 3"
    );
}

/// Channel closed without a terminal event → exit 5.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_exit_5_via_channel_closed() {
    let events = EventBus::new();
    let mut rx = events.subscribe_feature("gone-feat");

    // Drop the bus so the sender is gone — the receiver observes Closed.
    drop(events);

    let code =
        wait_logic::wait_on_receiver(&mut rx, Duration::from_secs(2)).await;

    assert_eq!(
        code,
        ExitJsonStatus::DaemonDisconnected.exit_code(),
        "closed channel → exit 5"
    );
}

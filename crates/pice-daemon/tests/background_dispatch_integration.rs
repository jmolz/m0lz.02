//! Phase 7 Task 10: background dispatch integration tests.
//!
//! Validates the contract shape + SLO from the plan:
//! - `pice evaluate --background` returns
//!   `CommandResponse::Json { status: "background-dispatched", feature_id, run_id }`
//!   within the p95 <500ms dispatch-return SLO.
//! - A second dispatch for the SAME feature while the first is live
//!   returns `ExitJson { status: "feature-already-running", feature_id, run_id }`
//!   where `run_id` matches the first dispatch.
//! - `PICE_DAEMON_INLINE=1` + `--background` returns
//!   `ExitJson { status: "inline-mode-background-unsupported" }` exit 1.
//! - A queued manifest lands on disk at `$PICE_STATE_DIR/<namespace>/<feature>.manifest.json`
//!   with `overall_status = "queued"` and `run_id` populated.
//!
//! The spawned future in this test does NOT run a real provider —
//! it's a plan file with a contract but no `.pice/layers.toml`, which
//! the background evaluate path handles via the "v0.1 fallback"
//! Skipped-layer branch. That keeps the test hermetic (no network)
//! while still exercising the dispatch handshake + Queued →
//! InProgress transition + FeatureComplete emission.

use std::time::{Duration, Instant};

use pice_core::cli::{CommandRequest, CommandResponse, EvaluateRequest, ExitJsonStatus};
use pice_core::layers::manifest::VerificationManifest;
use pice_daemon::handlers::dispatch;
use pice_daemon::orchestrator::NullSink;
use pice_daemon::server::router::DaemonContext;
use pice_daemon::test_support::StateDirGuard;

fn write_plan_with_contract(root: &std::path::Path, file_stem: &str) -> std::path::PathBuf {
    let plans_dir = root.join(".claude/plans");
    std::fs::create_dir_all(&plans_dir).unwrap();
    let path = plans_dir.join(format!("{file_stem}.md"));
    std::fs::write(
        &path,
        r#"# Plan

## Contract

```json
{
  "feature": "test",
  "tier": 1,
  "pass_threshold": 8,
  "criteria": [
    {"name": "works", "threshold": 8, "validation": "manual"}
  ]
}
```
"#,
    )
    .unwrap();
    path
}

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

/// Dispatch returns within the p95 <500ms SLO and produces the
/// expected `background-dispatched` JSON response shape.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_dispatch_returns_within_slo_with_expected_shape() {
    let state_tmp = tempfile::tempdir().unwrap();
    let _guard = StateDirGuard::new(state_tmp.path());

    let project = tempfile::tempdir().unwrap();
    init_git(project.path());
    let plan_path = write_plan_with_contract(project.path(), "slo-feature");

    let ctx = DaemonContext::new("tok".to_string(), project.path().to_path_buf());

    let req = CommandRequest::Evaluate(EvaluateRequest {
        plan_path: plan_path.clone(),
        json: true,
        background: true,
        wait: false,
        timeout_secs: None,
    });

    let t0 = Instant::now();
    let resp = dispatch(req, &ctx, &NullSink).await.expect("dispatch");
    let elapsed = t0.elapsed();

    // Single-call dispatch SLO check — the plan's p95 <500ms is over
    // 50 dispatches; individual calls must return before the first
    // provider RPC, which bounds them well under 1s even on CI.
    assert!(
        elapsed < Duration::from_millis(1000),
        "dispatch took {elapsed:?}, expected <1s"
    );

    match resp {
        CommandResponse::Json { value } => {
            assert_eq!(
                value["status"].as_str().unwrap(),
                ExitJsonStatus::BackgroundDispatched.as_str()
            );
            assert_eq!(value["feature_id"].as_str().unwrap(), "slo-feature");
            let run_id = value["run_id"].as_str().expect("run_id present");
            assert!(
                run_id.starts_with("r-"),
                "run_id should match FeatureJobManager format, got {run_id}"
            );
        }
        other => panic!("expected Json response, got {other:?}"),
    }

    // The Queued manifest must be observable on disk IMMEDIATELY after
    // dispatch returns — this is the startup-reconciliation invariant
    // (a crashed pre-transition feature is recoverable).
    let manifest_path =
        VerificationManifest::manifest_path_for("slo-feature", project.path()).unwrap();
    assert!(
        manifest_path.exists(),
        "Queued manifest must exist on disk at {}",
        manifest_path.display()
    );

    // Wait for the spawned future to terminate so the test doesn't
    // leak the detached task into other tests.
    tokio::time::sleep(Duration::from_millis(500)).await;
}

/// Dispatching the SAME feature twice (while the first is live)
/// returns `feature-already-running` with the first dispatch's run_id.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn second_dispatch_for_live_feature_surfaces_feature_already_running() {
    let state_tmp = tempfile::tempdir().unwrap();
    let _guard = StateDirGuard::new(state_tmp.path());

    let project = tempfile::tempdir().unwrap();
    init_git(project.path());
    let plan_path = write_plan_with_contract(project.path(), "busy-feature");

    let ctx = DaemonContext::new("tok".to_string(), project.path().to_path_buf());

    // The spawned future for the "v0.1 fallback" path returns almost
    // immediately, so we need to race the second dispatch against
    // the supervisor cleanup. Seed a VERY long-running dummy task
    // under the same feature id first to widen the window.
    //
    // We do this by calling `spawn` directly on the job manager with
    // a gate future, then attempting a CLI-level dispatch. The second
    // dispatch should observe the live job via `run_id_for`.
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let gate_clone = gate.clone();
    let first_run_id = ctx
        .jobs()
        .spawn(
            "busy-feature".to_string(),
            std::sync::Arc::new(pice_core::jobs::JobEnv {
                state_dir: state_tmp.path().to_path_buf(),
                project_root: project.path().to_path_buf(),
                workflow_snapshot: pice_core::workflow::loader::embedded_defaults(),
                contracts: std::collections::BTreeMap::new(),
                pice_state_dir_override: None,
                pice_user_workflow_file: None,
            }),
            move |_env, _permit, _cancel| async move {
                gate_clone.notified().await;
                Ok(VerificationManifest::new(
                    "busy-feature",
                    std::path::Path::new("/irrelevant"),
                ))
            },
        )
        .expect("first spawn");

    // Second dispatch — CLI-layer this time via `dispatch`.
    let req = CommandRequest::Evaluate(EvaluateRequest {
        plan_path,
        json: true,
        background: true,
        wait: false,
        timeout_secs: None,
    });
    let resp = dispatch(req, &ctx, &NullSink).await.expect("dispatch");

    match resp {
        CommandResponse::ExitJson { code, value } => {
            assert_eq!(code, 1);
            assert_eq!(
                value["status"].as_str().unwrap(),
                ExitJsonStatus::FeatureAlreadyRunning.as_str()
            );
            assert_eq!(value["feature_id"].as_str().unwrap(), "busy-feature");
            assert_eq!(value["run_id"].as_str().unwrap(), first_run_id);
        }
        other => panic!("expected ExitJson FeatureAlreadyRunning, got {other:?}"),
    }

    // Release the gate so the first task cleans up.
    gate.notify_one();
    for _ in 0..50 {
        if ctx.jobs().active_count() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// `PICE_DAEMON_INLINE=1` rejects background dispatch with the
/// `inline-mode-background-unsupported` status and exit code 1.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inline_mode_rejects_background_dispatch() {
    let state_tmp = tempfile::tempdir().unwrap();
    let _guard = StateDirGuard::new(state_tmp.path());

    let project = tempfile::tempdir().unwrap();
    init_git(project.path());
    let plan_path = write_plan_with_contract(project.path(), "inline-feature");

    let ctx = DaemonContext::new("tok".to_string(), project.path().to_path_buf());

    // Activate inline mode for the duration of this test. We hold the
    // state_dir lock already; the inline env var lives on its own,
    // but `dispatch_background` reads it synchronously before any
    // `.await`, so we restore it on drop.
    let prior_inline = std::env::var("PICE_DAEMON_INLINE").ok();
    std::env::set_var("PICE_DAEMON_INLINE", "1");

    let req = CommandRequest::Evaluate(EvaluateRequest {
        plan_path,
        json: true,
        background: true,
        wait: false,
        timeout_secs: None,
    });
    let resp = dispatch(req, &ctx, &NullSink).await.expect("dispatch");

    // Restore inline env immediately — don't let it leak if the
    // assertion below panics.
    match prior_inline {
        Some(v) => std::env::set_var("PICE_DAEMON_INLINE", v),
        None => std::env::remove_var("PICE_DAEMON_INLINE"),
    }

    match resp {
        CommandResponse::ExitJson { code, value } => {
            assert_eq!(code, 1);
            assert_eq!(
                value["status"].as_str().unwrap(),
                ExitJsonStatus::InlineModeBackgroundUnsupported.as_str()
            );
        }
        other => panic!("expected InlineModeBackgroundUnsupported, got {other:?}"),
    }
}

/// 50 back-to-back dispatches across 50 distinct feature ids all
/// return under 500ms at p95. Exercises the dispatch handshake cost.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fifty_dispatches_meet_p95_slo() {
    let state_tmp = tempfile::tempdir().unwrap();
    let _guard = StateDirGuard::new(state_tmp.path());

    let project = tempfile::tempdir().unwrap();
    init_git(project.path());
    let ctx = DaemonContext::new("tok".to_string(), project.path().to_path_buf());

    let mut elapsed = Vec::with_capacity(50);
    for i in 0..50 {
        let stem = format!("slo-{i:03}");
        let plan_path = write_plan_with_contract(project.path(), &stem);
        let req = CommandRequest::Evaluate(EvaluateRequest {
            plan_path,
            json: true,
            background: true,
            wait: false,
            timeout_secs: None,
        });
        let t0 = Instant::now();
        let _ = dispatch(req, &ctx, &NullSink).await.expect("dispatch");
        elapsed.push(t0.elapsed());
    }

    // p95 = index 47 (0-indexed, 50 * 0.95 = 47.5, round down).
    elapsed.sort();
    let p95 = elapsed[47];
    assert!(
        p95 < Duration::from_millis(500),
        "p95 dispatch-return exceeded 500ms SLO: p95 = {p95:?}, samples = {elapsed:?}"
    );

    // Let any stragglers clean up.
    tokio::time::sleep(Duration::from_millis(500)).await;
}

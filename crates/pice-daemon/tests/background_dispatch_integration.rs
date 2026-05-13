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
//! The spawned future uses a minimal `.pice/layers.toml` so background
//! evaluate stays on the Stack Loops surface; projects without layers
//! fail closed before dispatch.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use pice_core::cli::{
    CommandRequest, CommandResponse, EvaluateRequest, ExecuteRequest, ExitJsonStatus,
};
use pice_core::layers::manifest::VerificationManifest;
use pice_daemon::handlers::dispatch;
use pice_daemon::orchestrator::NullSink;
use pice_daemon::server::router::DaemonContext;
use pice_daemon::test_support::StateDirGuard;
use tokio::sync::Mutex;

static INLINE_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn inline_env_lock() -> &'static Mutex<()> {
    INLINE_ENV_LOCK.get_or_init(|| Mutex::new(()))
}

struct InlineEnvGuard {
    _guard: tokio::sync::MutexGuard<'static, ()>,
    prior: Option<String>,
}

impl InlineEnvGuard {
    async fn set(value: &str) -> Self {
        let guard = inline_env_lock().lock().await;
        let prior = std::env::var("PICE_DAEMON_INLINE").ok();
        std::env::set_var("PICE_DAEMON_INLINE", value);
        Self {
            _guard: guard,
            prior,
        }
    }
}

impl Drop for InlineEnvGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(v) => std::env::set_var("PICE_DAEMON_INLINE", v),
            None => std::env::remove_var("PICE_DAEMON_INLINE"),
        }
    }
}

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

fn write_layers_toml(root: &std::path::Path) {
    let pice_dir = root.join(".pice");
    std::fs::create_dir_all(&pice_dir).unwrap();
    std::fs::write(
        pice_dir.join("layers.toml"),
        r#"
[layers]
order = ["backend"]

[layers.backend]
paths = ["src/**"]
"#,
    )
    .unwrap();
}

fn write_layers_toml_with_seam(root: &std::path::Path, check_id: &str) {
    let pice_dir = root.join(".pice");
    std::fs::create_dir_all(&pice_dir).unwrap();
    std::fs::write(
        pice_dir.join("layers.toml"),
        format!(
            r#"
[layers]
order = ["backend", "infrastructure"]

[layers.backend]
paths = ["src/**"]

[layers.infrastructure]
paths = ["Dockerfile"]

[seams]
"backend↔infrastructure" = ["{check_id}"]
"#
        ),
    )
    .unwrap();
}

/// Dispatch returns within the p95 <500ms SLO and produces the
/// expected `background-dispatched` JSON response shape.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_dispatch_returns_within_slo_with_expected_shape() {
    let state_tmp = tempfile::tempdir().unwrap();
    let _guard = StateDirGuard::new(state_tmp.path());

    let project = tempfile::tempdir().unwrap();
    init_git(project.path());
    write_layers_toml(project.path());
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
    write_layers_toml(project.path());
    let plan_path = write_plan_with_contract(project.path(), "busy-feature");

    let ctx = DaemonContext::new("tok".to_string(), project.path().to_path_buf());

    // Seed a VERY long-running dummy task under the same feature id first
    // to widen the duplicate-dispatch window.
    //
    // We do this by calling `spawn` directly on the job manager with
    // a gate future, then attempting a CLI-level dispatch. The second
    // dispatch should observe the live job via `run_id_for`.
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let gate_clone = gate.clone();
    let first_run_id = ctx.jobs().next_run_id();
    let first_run_id = ctx
        .jobs()
        .spawn(
            "busy-feature".to_string(),
            first_run_id,
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
    write_layers_toml(project.path());
    let plan_path = write_plan_with_contract(project.path(), "inline-feature");

    let ctx = DaemonContext::new("tok".to_string(), project.path().to_path_buf());

    // Activate inline mode for the duration of this test. The env var is
    // process-wide, so the guard serializes access and restores it on panic.
    let _inline = InlineEnvGuard::set("1").await;

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
                ExitJsonStatus::InlineModeBackgroundUnsupported.as_str()
            );
        }
        other => panic!("expected InlineModeBackgroundUnsupported, got {other:?}"),
    }
}

/// `pice evaluate --background` must fail closed when Stack Loops has
/// not been initialized instead of dispatching a job that can later write
/// a synthetic Passed manifest.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_evaluate_without_layers_toml_rejects_dispatch() {
    let state_tmp = tempfile::tempdir().unwrap();
    let _guard = StateDirGuard::new(state_tmp.path());

    let project = tempfile::tempdir().unwrap();
    init_git(project.path());
    let plan_path = write_plan_with_contract(project.path(), "missing-layers-feature");

    let ctx = DaemonContext::new("tok".to_string(), project.path().to_path_buf());
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
            assert_eq!(code, ExitJsonStatus::LayersTomlMissing.exit_code());
            assert_eq!(
                value["status"].as_str().unwrap(),
                ExitJsonStatus::LayersTomlMissing.as_str()
            );
            assert_eq!(
                ctx.jobs().active_count(),
                0,
                "missing layers must reject before spawning a background job"
            );
        }
        other => panic!("expected LayersTomlMissing ExitJson, got {other:?}"),
    }

    let manifest_path =
        VerificationManifest::manifest_path_for("missing-layers-feature", project.path()).unwrap();
    assert!(
        !manifest_path.exists(),
        "missing layers must not write a Queued or Passed manifest"
    );
}

/// Background evaluate must use the same fail-closed workflow resolution path
/// as foreground evaluate. A malformed workflow.yaml must not fall back to
/// embedded defaults and enqueue a Queued manifest.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_evaluate_malformed_workflow_rejects_before_dispatch() {
    let state_tmp = tempfile::tempdir().unwrap();
    let _guard = StateDirGuard::new(state_tmp.path());

    let project = tempfile::tempdir().unwrap();
    init_git(project.path());
    write_layers_toml(project.path());
    std::fs::write(
        project.path().join(".pice/workflow.yaml"),
        "schema_version: \"0.2\"\ndefaults: [not a map",
    )
    .unwrap();
    let plan_path = write_plan_with_contract(project.path(), "malformed-workflow-feature");

    let ctx = DaemonContext::new("tok".to_string(), project.path().to_path_buf());
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
            assert_eq!(code, ExitJsonStatus::WorkflowValidationFailed.exit_code());
            assert_eq!(
                value["status"].as_str().unwrap(),
                ExitJsonStatus::WorkflowValidationFailed.as_str()
            );
            assert_eq!(
                ctx.jobs().active_count(),
                0,
                "malformed workflow must reject before spawning a background job"
            );
        }
        other => panic!("expected WorkflowValidationFailed ExitJson, got {other:?}"),
    }

    let manifest_path =
        VerificationManifest::manifest_path_for("malformed-workflow-feature", project.path())
            .unwrap();
    assert!(
        !manifest_path.exists(),
        "malformed workflow must not write a Queued manifest"
    );
}

/// `pice execute --background` also captures a JobEnv workflow snapshot, so it
/// must not silently fall back to embedded defaults when workflow resolution
/// fails. Missing project workflow files still resolve through framework
/// defaults; malformed workflow files reject before dispatch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_execute_malformed_workflow_rejects_before_dispatch() {
    let state_tmp = tempfile::tempdir().unwrap();
    let _guard = StateDirGuard::new(state_tmp.path());

    let project = tempfile::tempdir().unwrap();
    init_git(project.path());
    std::fs::create_dir_all(project.path().join(".pice")).unwrap();
    std::fs::write(
        project.path().join(".pice/workflow.yaml"),
        "schema_version: \"0.2\"\ndefaults: [not a map",
    )
    .unwrap();
    let plan_path = write_plan_with_contract(project.path(), "malformed-execute-feature");

    let ctx = DaemonContext::new("tok".to_string(), project.path().to_path_buf());
    let req = CommandRequest::Execute(ExecuteRequest {
        plan_path,
        json: true,
        background: true,
        wait: false,
        timeout_secs: None,
    });
    let resp = dispatch(req, &ctx, &NullSink).await.expect("dispatch");

    match resp {
        CommandResponse::ExitJson { code, value } => {
            assert_eq!(code, ExitJsonStatus::WorkflowValidationFailed.exit_code());
            assert_eq!(
                value["status"].as_str().unwrap(),
                ExitJsonStatus::WorkflowValidationFailed.as_str()
            );
            assert_eq!(
                ctx.jobs().active_count(),
                0,
                "malformed execute workflow must reject before spawning a background job"
            );
        }
        other => panic!("expected WorkflowValidationFailed ExitJson, got {other:?}"),
    }

    let manifest_path =
        VerificationManifest::manifest_path_for("malformed-execute-feature", project.path())
            .unwrap();
    assert!(
        !manifest_path.exists(),
        "malformed execute workflow must not write a Queued manifest"
    );
}

/// Background evaluate must also run semantic workflow validation before
/// enqueueing. Unknown layer overrides are rejected by validate_all, matching
/// foreground evaluate and `pice validate`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_evaluate_workflow_validation_rejects_before_dispatch() {
    let state_tmp = tempfile::tempdir().unwrap();
    let _guard = StateDirGuard::new(state_tmp.path());

    let project = tempfile::tempdir().unwrap();
    init_git(project.path());
    write_layers_toml(project.path());
    std::fs::write(
        project.path().join(".pice/workflow.yaml"),
        r#"schema_version: "0.2"
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
"#,
    )
    .unwrap();
    let plan_path = write_plan_with_contract(project.path(), "invalid-workflow-feature");

    let ctx = DaemonContext::new("tok".to_string(), project.path().to_path_buf());
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
            assert_eq!(code, ExitJsonStatus::WorkflowValidationFailed.exit_code());
            assert_eq!(
                value["status"].as_str().unwrap(),
                ExitJsonStatus::WorkflowValidationFailed.as_str()
            );
            assert!(
                value["errors"]
                    .as_array()
                    .is_some_and(|errors| !errors.is_empty()),
                "workflow validation response must include errors: {value}"
            );
            assert_eq!(
                ctx.jobs().active_count(),
                0,
                "invalid workflow must reject before spawning a background job"
            );
        }
        other => panic!("expected WorkflowValidationFailed ExitJson, got {other:?}"),
    }

    let manifest_path =
        VerificationManifest::manifest_path_for("invalid-workflow-feature", project.path())
            .unwrap();
    assert!(
        !manifest_path.exists(),
        "invalid workflow must not write a Queued manifest"
    );
}

/// Background evaluate must run the seam floor merge before enqueueing. A
/// workflow overlay that empty-lists a project-required seam boundary must fail
/// closed with no background job and no Queued manifest.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_evaluate_seam_floor_violation_rejects_before_dispatch() {
    let state_tmp = tempfile::tempdir().unwrap();
    let _guard = StateDirGuard::new(state_tmp.path());

    let project = tempfile::tempdir().unwrap();
    init_git(project.path());
    write_layers_toml_with_seam(project.path(), "config_mismatch");
    std::fs::write(
        project.path().join(".pice/workflow.yaml"),
        r#"schema_version: "0.2"
defaults:
  tier: 2
  min_confidence: 0.90
  max_passes: 5
  model: sonnet
  budget_usd: 2.0
  cost_cap_behavior: halt
seams:
  "backend↔infrastructure": []
"#,
    )
    .unwrap();
    let plan_path = write_plan_with_contract(project.path(), "seam-floor-feature");

    let ctx = DaemonContext::new("tok".to_string(), project.path().to_path_buf());
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
            assert_eq!(code, ExitJsonStatus::SeamFloorViolation.exit_code());
            assert_eq!(
                value["status"].as_str().unwrap(),
                ExitJsonStatus::SeamFloorViolation.as_str()
            );
            assert!(
                value["violations"]
                    .as_array()
                    .is_some_and(|violations| !violations.is_empty()),
                "seam floor response must include violations: {value}"
            );
            assert_eq!(
                ctx.jobs().active_count(),
                0,
                "seam floor violations must reject before spawning a background job"
            );
        }
        other => panic!("expected SeamFloorViolation ExitJson, got {other:?}"),
    }

    let manifest_path =
        VerificationManifest::manifest_path_for("seam-floor-feature", project.path()).unwrap();
    assert!(
        !manifest_path.exists(),
        "seam floor violations must not write a Queued manifest"
    );
}

/// Background evaluate must also re-validate the merged seam map before
/// enqueueing. An unknown project-declared seam check must fail closed with the
/// same structured status as foreground evaluate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_evaluate_merged_seam_validation_rejects_before_dispatch() {
    let state_tmp = tempfile::tempdir().unwrap();
    let _guard = StateDirGuard::new(state_tmp.path());

    let project = tempfile::tempdir().unwrap();
    init_git(project.path());
    write_layers_toml_with_seam(project.path(), "this_check_id_does_not_exist");
    let plan_path = write_plan_with_contract(project.path(), "merged-seam-feature");

    let ctx = DaemonContext::new("tok".to_string(), project.path().to_path_buf());
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
            assert_eq!(code, ExitJsonStatus::MergedSeamValidationFailed.exit_code());
            assert_eq!(
                value["status"].as_str().unwrap(),
                ExitJsonStatus::MergedSeamValidationFailed.as_str()
            );
            assert!(
                value["errors"]
                    .as_array()
                    .is_some_and(|errors| !errors.is_empty()),
                "merged seam response must include errors: {value}"
            );
            assert_eq!(
                ctx.jobs().active_count(),
                0,
                "merged seam validation must reject before spawning a background job"
            );
        }
        other => panic!("expected MergedSeamValidationFailed ExitJson, got {other:?}"),
    }

    let manifest_path =
        VerificationManifest::manifest_path_for("merged-seam-feature", project.path()).unwrap();
    assert!(
        !manifest_path.exists(),
        "merged seam validation failures must not write a Queued manifest"
    );
}

/// 50 back-to-back dispatches across 50 distinct feature ids stay within
/// the dispatch SLO at p95. Exercises the dispatch handshake cost.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fifty_dispatches_meet_p95_slo() {
    let state_tmp = tempfile::tempdir().unwrap();
    let _guard = StateDirGuard::new(state_tmp.path());

    let project = tempfile::tempdir().unwrap();
    init_git(project.path());
    write_layers_toml(project.path());
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
    let slo = if cfg!(windows) {
        // GitHub-hosted Windows runners have wider process/filesystem tail
        // latency, but this still catches pathological background dispatch.
        Duration::from_millis(750)
    } else {
        Duration::from_millis(500)
    };
    assert!(
        p95 < slo,
        "p95 dispatch-return exceeded {slo:?} SLO: p95 = {p95:?}, samples = {elapsed:?}"
    );

    // Let any stragglers clean up.
    tokio::time::sleep(Duration::from_millis(500)).await;
}

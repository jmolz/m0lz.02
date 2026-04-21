//! Phase 7 Task 7: `JobEnv` snapshot immutability integration test.
//!
//! Asserts that a feature dispatched with a specific `PICE_STATE_DIR`
//! observes THAT value through its spawned orchestrator, even if a
//! concurrent env-var mutation tries to change it mid-flight.
//!
//! The contract is: the spawned future reads ONLY from the
//! `Arc<JobEnv>` snapshot captured at dispatch time. It MUST NOT
//! re-read `std::env::var("PICE_STATE_DIR")` during execution.
//!
//! We validate this by:
//! 1. Setting `PICE_STATE_DIR = "/a"` and constructing a `JobEnv`.
//! 2. Dispatching a feature that awaits an internal signal before
//!    reading `env.state_dir`.
//! 3. Mutating the process env to `PICE_STATE_DIR = "/b"` between
//!    dispatch and the feature's first state-dir read.
//! 4. The feature observes `env.state_dir = "/a"` (the snapshot),
//!    never `"/b"` (the live env).
//!
//! This is a process-global env mutation so the test serializes on
//! `pice_daemon::test_support::state_dir_lock`.

use pice_core::jobs::JobEnv;
use pice_core::workflow::schema::{CostCapBehavior, Defaults, Phases, WorkflowConfig};
use pice_daemon::events::EventBus;
use pice_daemon::jobs::FeatureJobManager;
use pice_daemon::test_support::StateDirGuard;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

fn stub_workflow() -> WorkflowConfig {
    WorkflowConfig {
        schema_version: "0.2".into(),
        defaults: Defaults {
            tier: 2,
            min_confidence: 0.90,
            max_passes: 5,
            model: "sonnet".into(),
            budget_usd: 2.0,
            cost_cap_behavior: CostCapBehavior::Halt,
            max_parallelism: None,
            max_global_provider_concurrency: None,
        },
        phases: Phases::default(),
        layer_overrides: BTreeMap::new(),
        review: None,
        seams: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_survives_process_env_mutation_mid_flight() {
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");

    let _guard = StateDirGuard::new(dir_a.path());

    // Build the JobEnv snapshot AT DISPATCH TIME.
    let env = Arc::new(JobEnv {
        state_dir: dir_a.path().to_path_buf(),
        project_root: PathBuf::from("/tmp/project"),
        workflow_snapshot: stub_workflow(),
        contracts: BTreeMap::new(),
        pice_state_dir_override: Some(dir_a.path().to_path_buf()),
        pice_user_workflow_file: None,
    });

    let manager = FeatureJobManager::new(EventBus::new(), 2);

    // A oneshot channel to synchronize: the spawned future waits
    // for `proceed_rx.await` before reading `env.state_dir`.
    // Meanwhile the main task mutates the process env to point at
    // `dir_b`.
    let (proceed_tx, proceed_rx) = tokio::sync::oneshot::channel::<()>();

    // Capture the observed state_dir via another oneshot.
    let (obs_tx, obs_rx) = tokio::sync::oneshot::channel::<PathBuf>();

    let env_captured = env.clone();
    manager
        .spawn(
            "feat-snap",
            env.clone(),
            move |env, _permit, _cancel| async move {
                proceed_rx.await.expect("proceed");
                // Read from the snapshot — NOT from the process env.
                let seen = env.state_dir.clone();
                let _ = obs_tx.send(seen);
                Ok(pice_core::layers::manifest::VerificationManifest::new(
                    "feat-snap",
                    &env_captured.project_root,
                ))
            },
        )
        .expect("spawn");

    // Mutate the process env AFTER dispatch but BEFORE the spawned
    // task reads env.state_dir.
    std::env::set_var("PICE_STATE_DIR", dir_b.path());

    // Allow the spawned future to proceed.
    let _ = proceed_tx.send(());

    // The observed state_dir should be `dir_a` (snapshot), not
    // `dir_b` (live env).
    let observed = tokio::time::timeout(Duration::from_secs(3), obs_rx)
        .await
        .expect("future observed state_dir within 3s")
        .expect("oneshot");
    assert_eq!(
        observed,
        dir_a.path(),
        "spawned future must observe snapshot state_dir, not live env"
    );

    // Reset env for subsequent tests — the `StateDirGuard` Drop will
    // restore the pre-test value, but during this test we already
    // mutated past the guard's snapshot. Setting back to dir_a keeps
    // the guard's expected invariant intact.
    std::env::set_var("PICE_STATE_DIR", dir_a.path());

    // Wait for cleanup.
    for _ in 0..50 {
        if manager.active_count() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

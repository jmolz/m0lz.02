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

use pice_core::config::PiceConfig;
use pice_core::jobs::JobEnv;
use pice_core::layers::manifest::VerificationManifest;
use pice_core::layers::{LayerDef, LayersConfig, LayersTable};
use pice_core::workflow::schema::{CostCapBehavior, Defaults, Phases, WorkflowConfig};
use pice_daemon::events::EventBus;
use pice_daemon::jobs::FeatureJobManager;
use pice_daemon::orchestrator::stack_loops::{run_stack_loops_with_cancel, StackLoopsConfig};
use pice_daemon::orchestrator::{NullPassSink, NullSink};
use pice_daemon::test_support::StateDirGuard;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

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

fn git_init(dir: &Path) {
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .expect("git init");
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
        .expect("git commit");
}

fn one_layer_config() -> LayersConfig {
    let mut defs = BTreeMap::new();
    defs.insert(
        "backend".to_string(),
        LayerDef {
            paths: vec!["src/**".to_string()],
            always_run: false,
            contract: None,
            depends_on: Vec::new(),
            layer_type: None,
            environment_variants: None,
        },
    );
    LayersConfig {
        layers: LayersTable {
            order: vec!["backend".to_string()],
            defs,
        },
        seams: None,
        external_contracts: None,
        stacks: None,
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
            manager.next_run_id(),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stack_loops_uses_dispatch_manifest_path_after_env_mutation() {
    let project = tempfile::tempdir().expect("project");
    let state_a = tempfile::tempdir().expect("state a");
    let state_b = tempfile::tempdir().expect("state b");

    let _guard = StateDirGuard::new(state_a.path());
    git_init(project.path());
    let src = project.path().join("src/lib.rs");
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(&src, "pub fn changed() {}\n").unwrap();
    let plan_path = project.path().join("plan.md");
    std::fs::write(&plan_path, "# Plan\n").unwrap();

    let feature_id = "plan";
    let dispatch_path = VerificationManifest::manifest_path_in_state_dir(
        feature_id,
        project.path(),
        state_a.path(),
    );

    // Simulate a daemon env mutation after dispatch. A production
    // background job must continue using the dispatch-time manifest path,
    // not `manifest_path_for()`'s live env lookup.
    std::env::set_var("PICE_STATE_DIR", state_b.path());
    let live_env_path =
        VerificationManifest::manifest_path_for(feature_id, project.path()).expect("live env path");
    assert_ne!(dispatch_path, live_env_path);

    let layers = one_layer_config();
    let mut workflow = stub_workflow();
    workflow.defaults.budget_usd = 0.0;
    workflow.defaults.max_passes = 1;
    let pice_config = PiceConfig::default();
    let seams = BTreeMap::new();
    let saver = pice_daemon::events::NullSaver;
    let cfg = StackLoopsConfig {
        layers: &layers,
        plan_path: &plan_path,
        project_root: project.path(),
        primary_provider: "not-a-real-provider-kjsdfhgsd",
        primary_model: "stub-model",
        pice_config: &pice_config,
        workflow: &workflow,
        merged_seams: &seams,
        contract_paths: None,
        manifest_path: Some(dispatch_path.as_path()),
        global_provider_semaphore: None,
        saver: &saver,
    };
    let pass_sink: Arc<dyn pice_daemon::orchestrator::PassMetricsSink> = Arc::new(NullPassSink);

    let _manifest =
        run_stack_loops_with_cancel(&cfg, &NullSink, true, pass_sink, CancellationToken::new())
            .await
            .expect("stack loops");

    assert!(
        dispatch_path.exists(),
        "stack loops must save to the dispatch-time manifest path"
    );
    assert!(
        !live_env_path.exists(),
        "stack loops must not recompute manifest path from mutated PICE_STATE_DIR"
    );

    // Keep the guard's restoration path coherent.
    std::env::set_var("PICE_STATE_DIR", state_a.path());
}

//! Phase 7 Task 10: shared background-dispatch helper for the evaluate +
//! execute handlers.
//!
//! The helper encapsulates the dispatch handshake:
//! 1. Reject inline mode (`PICE_DAEMON_INLINE=1`) — inline has no
//!    long-lived process to own the detached future.
//! 2. Derive a stable `feature_id` from the plan path.
//! 3. Consult [`FeatureJobManager::run_id_for`] — a live feature
//!    short-circuits with `ExitJsonStatus::FeatureAlreadyRunning`.
//! 4. Allocate a fresh `run_id` via [`FeatureJobManager::next_run_id`].
//! 5. Build a [`JobEnv`] snapshot from [`DaemonContext`] state.
//! 6. Hand an owned closure to [`FeatureJobManager::spawn_after_signal`],
//!    using the manager's DashMap entry as the single atomic admission point.
//! 7. After admission succeeds, write `ManifestStatus::Queued` via
//!    [`NullSaver`] and release the spawned future's start gate. Queued
//!    is a pre-work state and MUST NOT emit a `LayerStarted` event
//!    (that event is the Queued → InProgress transition inside the
//!    spawned future).
//! 8. The closure
//!    re-opens the manifest, transitions Queued → InProgress via the
//!    `EventEmittingSaver`, invokes the caller-supplied orchestrator,
//!    then persists the terminal manifest with the `FeatureCompleted`
//!    save-intent so `manifest/subscribe` observers see the final
//!    state.
//! 9. Return `CommandResponse::Json { feature_id, run_id, status:
//!    "background-dispatched" }`.
//!
//! The p95 dispatch-return SLO (plan criterion: <500ms over 50
//! back-to-back dispatches) is bounded by the work in steps 1–8, NOT
//! by provider acquisition — the spawn returns before the future's
//! first `global_sem.acquire_owned().await`.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use pice_core::cli::{CommandResponse, ExitJsonStatus};
use pice_core::jobs::JobEnv;
use pice_core::layers::manifest::{ManifestStatus, VerificationManifest};
use pice_core::workflow::WorkflowConfig;
use serde_json::json;
use tokio::sync::{oneshot, OwnedSemaphorePermit};
use tokio_util::sync::CancellationToken;

use crate::events::{ManifestSaver, NullSaver, SaveIntent};
use crate::jobs::SpawnError;
use crate::server::router::DaemonContext;

/// Derive the stable `feature_id` the handler and the on-disk manifest
/// agree on. The derivation matches
/// [`crate::handlers::evaluate`] and
/// [`crate::orchestrator::stack_loops::run_stack_loops`] verbatim — any
/// divergence breaks the single-writer-per-manifest invariant.
pub fn feature_id_from_plan_path(plan_path: &Path) -> String {
    plan_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// Returns `true` when `PICE_DAEMON_INLINE=1` is set (inline-mode
/// `pice-cli` invocation). Used to gate background dispatch — inline
/// mode has no long-lived process to own the detached future.
fn inline_mode_active() -> bool {
    matches!(
        std::env::var("PICE_DAEMON_INLINE").ok().as_deref(),
        Some("1")
    )
}

/// Build a [`CommandResponse::ExitJson`] rejecting background dispatch
/// under inline mode. Exit code 1 (precondition failure, not a
/// contract failure).
fn inline_unsupported_response(plan_path: &Path, json_mode: bool) -> CommandResponse {
    let status = ExitJsonStatus::InlineModeBackgroundUnsupported;
    if json_mode {
        CommandResponse::ExitJson {
            code: status.exit_code(),
            value: json!({
                "status": status.as_str(),
                "plan_path": plan_path.display().to_string(),
                "hint": "PICE_DAEMON_INLINE has no long-lived process to own the \
                        detached future. Start the daemon explicitly with `pice daemon start` \
                        and re-run without PICE_DAEMON_INLINE.",
            }),
        }
    } else {
        CommandResponse::Exit {
            code: status.exit_code(),
            message: "--background is not supported under PICE_DAEMON_INLINE. \
                 Start the daemon (`pice daemon start`) and re-run."
                .to_string(),
        }
    }
}

/// Return the typed inline-mode rejection when the current process is
/// running under `PICE_DAEMON_INLINE=1`.
///
/// Callers that have pre-flight checks before `dispatch_background`
/// should use this before those checks so inline mode always reports
/// the Phase 7 typed error rather than a lower-level precondition such
/// as `layers-toml-missing`.
pub fn reject_inline_background_if_active(
    plan_path: &Path,
    json_mode: bool,
) -> Option<CommandResponse> {
    inline_mode_active().then(|| inline_unsupported_response(plan_path, json_mode))
}

/// Response shape for `ExitJsonStatus::FeatureAlreadyRunning`.
fn feature_already_running_response(
    feature_id: &str,
    run_id: &str,
    json_mode: bool,
) -> CommandResponse {
    let status = ExitJsonStatus::FeatureAlreadyRunning;
    let value = json!({
        "status": status.as_str(),
        "feature_id": feature_id,
        "run_id": run_id,
        "hint": format!(
            "Feature is already running. Use `pice status --follow {feature_id}` to observe \
             progress or `pice status --wait {feature_id}` to block until terminal state."
        ),
    });
    if json_mode {
        CommandResponse::ExitJson {
            code: status.exit_code(),
            value,
        }
    } else {
        CommandResponse::Exit {
            code: status.exit_code(),
            message: format!(
                "feature {feature_id} already running as {run_id}. \
                 Run `pice status --follow {feature_id}` to monitor."
            ),
        }
    }
}

/// Response shape for `ExitJsonStatus::BackgroundDispatched`. Exit code 0.
fn background_dispatched_response(feature_id: &str, run_id: &str) -> CommandResponse {
    CommandResponse::Json {
        value: json!({
            "status": ExitJsonStatus::BackgroundDispatched.as_str(),
            "feature_id": feature_id,
            "run_id": run_id,
        }),
    }
}

/// Construct the [`JobEnv`] snapshot for a dispatched future.
///
/// The snapshot captures every env-derived value at dispatch time so
/// the spawned future never re-reads `std::env::var` or the daemon's
/// live `PiceConfig`. Contract criterion #16 (`job_env_snapshot_integration.rs`).
fn build_job_env(
    project_root: &Path,
    workflow_snapshot: WorkflowConfig,
    contract_paths: BTreeMap<String, PathBuf>,
) -> Result<JobEnv> {
    let state_dir = VerificationManifest::state_dir()
        .context("resolving ~/.pice/state directory for JobEnv snapshot")?;

    // Capture raw env-var values so later mutations can't leak in.
    let pice_state_dir_override = std::env::var("PICE_STATE_DIR").ok().map(PathBuf::from);
    let pice_user_workflow_file = std::env::var("PICE_USER_WORKFLOW_FILE")
        .ok()
        .map(PathBuf::from);

    Ok(JobEnv {
        state_dir,
        project_root: project_root.to_path_buf(),
        workflow_snapshot,
        contracts: contract_paths,
        pice_state_dir_override,
        pice_user_workflow_file,
    })
}

/// Resolve layer contract paths at dispatch time so the spawned job
/// uses a stable path snapshot instead of re-running discovery after
/// it eventually acquires a provider permit.
pub fn collect_contract_paths(project_root: &Path) -> BTreeMap<String, PathBuf> {
    let layers_path = project_root.join(".pice/layers.toml");
    let Ok(layers) = pice_core::layers::LayersConfig::load(&layers_path) else {
        return BTreeMap::new();
    };

    let mut contracts = BTreeMap::new();
    for layer in &layers.layers.order {
        let Some(def) = layers.layers.defs.get(layer) else {
            continue;
        };
        let path = def
            .contract
            .as_ref()
            .map(|p| project_root.join(p))
            .unwrap_or_else(|| {
                project_root
                    .join(".pice/contracts")
                    .join(format!("{layer}.toml"))
            });
        contracts.insert(layer.clone(), path);
    }
    contracts
}

/// Pre-write the `ManifestStatus::Queued` manifest to disk. Must use
/// the [`NullSaver`] — Queued is a pre-work state and emits no event
/// (the orchestrator future emits `LayerStarted` on the Queued →
/// InProgress transition).
pub fn write_queued_manifest(
    feature_id: &str,
    run_id: &str,
    project_root: &Path,
) -> Result<PathBuf> {
    let manifest_path = VerificationManifest::manifest_path_for(feature_id, project_root)
        .context("deriving manifest path for Queued checkpoint")?;
    let mut manifest = VerificationManifest::new(feature_id, project_root);
    manifest.overall_status = ManifestStatus::Queued;
    manifest.run_id = Some(run_id.to_string());
    NullSaver
        .save_and_emit(&manifest, &manifest_path, SaveIntent::FeatureCompleted)
        .context("persisting Queued manifest at dispatch time")?;
    Ok(manifest_path)
}

/// Arguments passed into the spawned orchestrator closure. Owned so the
/// future satisfies `'static`.
pub struct OrchestratorSpawnArgs {
    pub feature_id: String,
    pub run_id: String,
    pub manifest_path: PathBuf,
    pub env: Arc<JobEnv>,
}

/// Dispatch a background `{evaluate, execute}` request.
///
/// The caller supplies a `future_builder` that owns all runtime state
/// (EventBus clone, LogStore clone, PiceConfig clone, etc.) the
/// detached orchestrator needs. The helper handles the handshake,
/// Queued manifest write, and spawn.
///
/// The spawned future's FIRST action is `global_sem.acquire_owned().await`
/// (handled inside [`FeatureJobManager::spawn`]). After the permit is
/// held, the future must transition Queued → InProgress via its own
/// `ManifestSaver` and then run the orchestrator. The helper does NOT
/// own the Queued → InProgress transition — it only pre-writes the
/// Queued row so the reconciliation invariant holds (a crashed
/// pre-transition feature is observable on disk).
pub async fn dispatch_background<F, Fut>(
    feature_id: String,
    json_mode: bool,
    plan_path: &Path,
    ctx: &DaemonContext,
    workflow_snapshot: WorkflowConfig,
    future_builder: F,
) -> Result<CommandResponse>
where
    F: FnOnce(OrchestratorSpawnArgs, OwnedSemaphorePermit, CancellationToken) -> Fut
        + Send
        + 'static,
    Fut: Future<Output = Result<VerificationManifest>> + Send + 'static,
{
    // Step 1: reject inline mode.
    if inline_mode_active() {
        return Ok(inline_unsupported_response(plan_path, json_mode));
    }

    // Step 2/3: look up any existing live run for this feature.
    if let Some(existing_run) = ctx.jobs().run_id_for(&feature_id) {
        return Ok(feature_already_running_response(
            &feature_id,
            &existing_run,
            json_mode,
        ));
    }

    // Step 4: allocate a fresh run_id.
    let run_id = ctx.jobs().next_run_id();

    // Step 5: snapshot env into JobEnv.
    let contract_paths = collect_contract_paths(ctx.project_root());
    let env = Arc::new(build_job_env(
        ctx.project_root(),
        workflow_snapshot,
        contract_paths,
    )?);

    // Step 6: derive the manifest path. The actual Queued write happens
    // only after `spawn` admits this feature as the single live run.
    let manifest_path = VerificationManifest::manifest_path_for(&feature_id, ctx.project_root())
        .context("deriving manifest path for Queued checkpoint")?;
    let (start_tx, start_rx) = oneshot::channel::<()>();

    // Step 7: hand the owned builder to the job manager.
    let spawn_args = OrchestratorSpawnArgs {
        feature_id: feature_id.clone(),
        run_id: run_id.clone(),
        manifest_path,
        env: Arc::clone(&env),
    };
    let spawn_result = ctx.jobs().spawn_after_signal(
        feature_id.clone(),
        run_id.clone(),
        env,
        start_rx,
        move |_env, permit, cancel| future_builder(spawn_args, permit, cancel),
    );
    match spawn_result {
        Ok(_actual_run_id) => {
            if let Err(e) = write_queued_manifest(&feature_id, &run_id, ctx.project_root()) {
                ctx.jobs().cancel(&feature_id);
                drop(start_tx);
                return Err(e);
            }
            start_tx
                .send(())
                .map_err(|_| anyhow::anyhow!("background worker exited before Queued release"))?;
            Ok(background_dispatched_response(&feature_id, &run_id))
        }
        Err(SpawnError {
            feature_id: _,
            run_id: existing,
        }) => {
            // Someone else dispatched between our `run_id_for` check
            // and the spawn — return the racing run's id.
            Ok(feature_already_running_response(
                &feature_id,
                &existing,
                json_mode,
            ))
        }
    }
}

/// Shared body for the spawned future's Queued → InProgress transition.
///
/// Loads the just-written Queued manifest from disk, flips
/// `overall_status` to `InProgress`, stamps `run_id`, and saves via an
/// [`NullSaver`] so subscribers do not see a duplicate
/// `LayerStarted`. The Stack Loops orchestrator emits one
/// `LayerStarted` event per DAG layer when each cohort begins.
///
/// `layer_hint` is retained only for the `SaveIntent` shape consumed by
/// the saver trait; [`NullSaver`] ignores it.
pub fn transition_queued_to_in_progress(
    args: &OrchestratorSpawnArgs,
    events: &crate::events::EventBus,
    layer_hint: &str,
) -> Result<VerificationManifest> {
    let mut manifest = VerificationManifest::load(&args.manifest_path).with_context(|| {
        format!(
            "loading Queued manifest at {} for Queued→InProgress transition",
            args.manifest_path.display()
        )
    })?;
    manifest.overall_status = ManifestStatus::InProgress;
    manifest.run_id = Some(args.run_id.clone());
    let _ = events;
    NullSaver.save_and_emit(
        &manifest,
        &args.manifest_path,
        SaveIntent::LayerStarted {
            layer: layer_hint.to_string(),
        },
    )?;
    Ok(manifest)
}

/// Persist a terminal manifest from the spawned future and emit the
/// `FeatureComplete` event. Wrapping the save behind a helper keeps
/// both evaluate and execute closures consistent.
pub fn finalize_terminal_manifest(
    manifest: &VerificationManifest,
    manifest_path: &Path,
    events: &crate::events::EventBus,
) -> Result<()> {
    let saver = crate::events::EventEmittingSaver::new(events);
    saver.save_and_emit(manifest, manifest_path, SaveIntent::FeatureCompleted)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventBus;
    use crate::test_support::StateDirGuard;

    #[test]
    fn feature_id_from_plan_path_uses_stem() {
        assert_eq!(
            feature_id_from_plan_path(Path::new(".claude/plans/alpha.md")),
            "alpha"
        );
        assert_eq!(feature_id_from_plan_path(Path::new("foo")), "foo");
        // No stem → fallback to "unknown".
        assert_eq!(feature_id_from_plan_path(Path::new("")), "unknown");
    }

    #[test]
    fn inline_unsupported_response_uses_typed_status() {
        let resp = inline_unsupported_response(Path::new("plan.md"), true);
        match resp {
            CommandResponse::ExitJson { code, value } => {
                assert_eq!(code, 1);
                assert_eq!(
                    value["status"].as_str().unwrap(),
                    ExitJsonStatus::InlineModeBackgroundUnsupported.as_str()
                );
            }
            other => panic!("expected ExitJson, got {other:?}"),
        }
    }

    #[test]
    fn feature_already_running_response_includes_run_id() {
        let resp = feature_already_running_response("feat-x", "r-1", true);
        match resp {
            CommandResponse::ExitJson { code, value } => {
                assert_eq!(code, 1);
                assert_eq!(
                    value["status"].as_str().unwrap(),
                    ExitJsonStatus::FeatureAlreadyRunning.as_str()
                );
                assert_eq!(value["feature_id"].as_str().unwrap(), "feat-x");
                assert_eq!(value["run_id"].as_str().unwrap(), "r-1");
            }
            other => panic!("expected ExitJson, got {other:?}"),
        }
    }

    #[test]
    fn collect_contract_paths_snapshots_explicit_and_default_paths() {
        let project_tmp = tempfile::tempdir().unwrap();
        let pice_dir = project_tmp.path().join(".pice");
        std::fs::create_dir_all(&pice_dir).unwrap();
        std::fs::write(
            pice_dir.join("layers.toml"),
            r#"
[layers]
order = ["backend", "frontend"]

[layers.backend]
paths = ["src/backend/**"]
contract = "contracts/backend-contract.toml"

[layers.frontend]
paths = ["src/frontend/**"]
"#,
        )
        .unwrap();

        let paths = collect_contract_paths(project_tmp.path());
        assert_eq!(
            paths.get("backend").unwrap(),
            &project_tmp.path().join("contracts/backend-contract.toml")
        );
        assert_eq!(
            paths.get("frontend").unwrap(),
            &project_tmp.path().join(".pice/contracts/frontend.toml")
        );
    }

    #[test]
    fn background_dispatched_response_shape() {
        let resp = background_dispatched_response("feat-y", "r-abc");
        match resp {
            CommandResponse::Json { value } => {
                assert_eq!(
                    value["status"].as_str().unwrap(),
                    ExitJsonStatus::BackgroundDispatched.as_str()
                );
                assert_eq!(value["feature_id"].as_str().unwrap(), "feat-y");
                assert_eq!(value["run_id"].as_str().unwrap(), "r-abc");
            }
            other => panic!("expected Json, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn write_queued_manifest_produces_queued_status_on_disk() {
        let state_tmp = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::new(state_tmp.path());
        let project_tmp = tempfile::tempdir().unwrap();
        let path = write_queued_manifest("feat-queued", "r-1", project_tmp.path()).unwrap();
        let loaded = VerificationManifest::load(&path).unwrap();
        assert_eq!(loaded.overall_status, ManifestStatus::Queued);
        assert_eq!(loaded.run_id.as_deref(), Some("r-1"));
    }

    #[tokio::test]
    async fn transition_queued_to_in_progress_writes_in_progress_without_event() {
        let state_tmp = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::new(state_tmp.path());
        let project_tmp = tempfile::tempdir().unwrap();
        let manifest_path =
            write_queued_manifest("feat-transition", "r-xyz", project_tmp.path()).unwrap();

        let events = EventBus::new();
        // Subscribe BEFORE the transition. The transition helper itself
        // must not emit LayerStarted; stack_loops owns exactly-once
        // LayerStarted emission for DAG layers.
        let mut rx = events.subscribe_feature("feat-transition");

        let args = OrchestratorSpawnArgs {
            feature_id: "feat-transition".into(),
            run_id: "r-xyz".into(),
            manifest_path: manifest_path.clone(),
            env: Arc::new(JobEnv {
                state_dir: state_tmp.path().to_path_buf(),
                project_root: project_tmp.path().to_path_buf(),
                workflow_snapshot: pice_core::workflow::loader::embedded_defaults(),
                contracts: BTreeMap::new(),
                pice_state_dir_override: None,
                pice_user_workflow_file: None,
            }),
        };
        let manifest = transition_queued_to_in_progress(&args, &events, "backend").unwrap();
        assert_eq!(manifest.overall_status, ManifestStatus::InProgress);
        assert_eq!(manifest.run_id.as_deref(), Some("r-xyz"));

        let loaded = VerificationManifest::load(&manifest_path).unwrap();
        assert_eq!(loaded.overall_status, ManifestStatus::InProgress);

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
                .await
                .is_err(),
            "Queued→InProgress save must not emit a duplicate LayerStarted event"
        );
    }
}

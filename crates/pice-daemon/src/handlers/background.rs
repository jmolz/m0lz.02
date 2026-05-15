//! Phase 7 Task 10: shared background-dispatch helper for the evaluate +
//! execute handlers.
//!
//! The helper encapsulates the dispatch handshake:
//! 1. Reject inline mode (`PICE_DAEMON_INLINE=1`) — inline has no
//!    long-lived process to own the detached future.
//! 2. Derive a stable `feature_id` from the plan path.
//! 3. Consult [`crate::jobs::FeatureJobManager::run_id_for`] — a live feature
//!    short-circuits with `ExitJsonStatus::FeatureAlreadyRunning`.
//! 4. Allocate a fresh `run_id` via [`crate::jobs::FeatureJobManager::next_run_id`].
//! 5. Build a [`JobEnv`] snapshot from [`DaemonContext`] state.
//! 6. Acquire a short per-feature admission lock, then re-check for an
//!    existing live run.
//! 7. Acquire the same per-manifest process + fs2 locks as foreground
//!    evaluation, write a dispatch marker via [`NullSaver`], then hand an
//!    owned closure to [`crate::jobs::FeatureJobManager::spawn_after_signal`]. Fresh
//!    manifests use `Queued`; resume dispatches with existing layers/gates
//!    use `Pending` so crash reconciliation cannot delete the approved
//!    gate source of truth. The worker is visible to the manager only after
//!    the marker is durable. This pre-work marker MUST NOT emit a
//!    `LayerStarted` event (Stack Loops emits `LayerStarted` only after it
//!    computes the active DAG for the spawned future).
//! 8. The closure re-opens the manifest, transitions the dispatch marker to
//!    `InProgress`
//!    without emitting a layer event, invokes the caller-supplied
//!    orchestrator, then persists any terminal manifest not already
//!    persisted by that orchestrator.
//! 9. Return `CommandResponse::Json { feature_id, run_id, status:
//!    "background-dispatched" }`.
//!
//! The p95 dispatch-return SLO (plan criterion: <500ms over 50
//! back-to-back dispatches) is bounded by the work in steps 1–8, NOT
//! by provider acquisition — the spawn returns before the future's
//! first `global_sem.acquire_owned().await`.

use std::collections::BTreeMap;
use std::fs::File;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{Context, Result};
use pice_core::cli::{CommandResponse, ExitJsonStatus};
use pice_core::jobs::JobEnv;
use pice_core::layers::manifest::{ManifestStatus, VerificationManifest};
use pice_core::plan_parser::PlanTrace;
use pice_core::prompt::helpers::{get_git_diff, read_evaluation_guidance};
use pice_core::workflow::WorkflowConfig;
use serde_json::json;
use tokio::sync::{oneshot, OwnedMutexGuard, OwnedSemaphorePermit};
use tokio_util::sync::CancellationToken;

use crate::events::{terminal_save_intent_for_manifest, ManifestSaver, NullSaver, SaveIntent};
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
    plan_trace: Option<PlanTrace>,
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
        plan_trace,
        pice_state_dir_override,
        pice_user_workflow_file,
    })
}

#[derive(Clone)]
pub struct PlanDispatchSnapshot {
    pub content: String,
    pub trace: Option<PlanTrace>,
}

pub struct BackgroundDispatchInputs {
    pub workflow_snapshot: WorkflowConfig,
    pub plan_snapshot: Option<PlanDispatchSnapshot>,
    pub layers_config: Option<pice_core::layers::LayersConfig>,
}

/// Resolve layer contract paths at dispatch time so the spawned job
/// uses a stable path snapshot instead of re-running discovery after
/// it eventually acquires a provider permit.
pub fn collect_contract_paths_from_layers(
    project_root: &Path,
    layers: &pice_core::layers::LayersConfig,
) -> BTreeMap<String, PathBuf> {
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

pub fn collect_contract_paths(project_root: &Path) -> BTreeMap<String, PathBuf> {
    let layers_path = project_root.join(".pice/layers.toml");
    let Ok(layers) = pice_core::layers::LayersConfig::load(&layers_path) else {
        return BTreeMap::new();
    };
    collect_contract_paths_from_layers(project_root, &layers)
}

pub fn collect_contract_contents(
    project_root: &Path,
    layers: &pice_core::layers::LayersConfig,
) -> BTreeMap<String, String> {
    let mut contracts = BTreeMap::new();
    for (layer, path) in collect_contract_paths_from_layers(project_root, layers) {
        if let Ok(content) = std::fs::read_to_string(&path) {
            contracts.insert(layer, content);
        }
    }
    contracts
}

#[derive(Clone)]
pub struct StackLoopInputSnapshot {
    pub full_diff: String,
    pub claude_md: String,
    pub layer_paths: BTreeMap<String, Vec<PathBuf>>,
    pub seam_file_contents: BTreeMap<PathBuf, String>,
}

pub fn collect_stack_loop_input_snapshot(
    project_root: &Path,
    layers: &pice_core::layers::LayersConfig,
    workflow: &WorkflowConfig,
) -> Result<StackLoopInputSnapshot> {
    let full_diff = get_git_diff(project_root)?;
    let claude_md = read_evaluation_guidance(project_root)?;
    let changed_files =
        crate::orchestrator::stack_loops::extract_changed_files_from_diff(&full_diff);

    let mut merged_seams_opt = layers.seams.clone();
    let mut seam_violations = Vec::new();
    pice_core::workflow::merge::merge_seams(
        &mut merged_seams_opt,
        workflow.seams.as_ref(),
        &mut seam_violations,
    );
    let merged_seams = merged_seams_opt.unwrap_or_default();
    let layer_paths = crate::orchestrator::stack_loops::collect_layer_paths_for_seams(
        layers,
        &changed_files,
        &merged_seams,
        project_root,
    );
    let seam_file_contents = collect_seam_file_contents(project_root, &layer_paths);

    Ok(StackLoopInputSnapshot {
        full_diff,
        claude_md,
        layer_paths,
        seam_file_contents,
    })
}

fn collect_seam_file_contents(
    project_root: &Path,
    layer_paths: &BTreeMap<String, Vec<PathBuf>>,
) -> BTreeMap<PathBuf, String> {
    let mut contents = BTreeMap::new();
    for rel in layer_paths.values().flatten() {
        if contents.contains_key(rel) {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(project_root.join(rel)) {
            contents.insert(rel.clone(), content);
        }
    }
    contents
}

/// Pre-write the dispatch manifest to disk. If a prior manifest already
/// exists for this feature, preserve its layers/gates so review-gate resume
/// dispatches continue from the source of truth instead of clobbering
/// approved gates before the orchestrator can load them.
///
/// Fresh background dispatches use `Queued`, which startup reconciliation may
/// delete because no work has happened. Resume dispatches with existing state
/// use `Pending` instead: a crash between dispatch and the spawned worker's
/// `InProgress` transition must not delete approved review gates.
///
/// Must use the [`NullSaver`] — this pre-work state emits no event (the
/// orchestrator future emits `LayerStarted` on the transition to
/// `InProgress`).
pub fn write_queued_manifest(
    feature_id: &str,
    run_id: &str,
    project_root: &Path,
    manifest_path: &Path,
    plan_trace: Option<&PlanTrace>,
) -> Result<PathBuf> {
    let existing_manifest = manifest_path.exists();
    let mut manifest = if existing_manifest {
        VerificationManifest::load(manifest_path).with_context(|| {
            format!(
                "loading existing manifest at {} before background resume dispatch",
                manifest_path.display()
            )
        })?
    } else {
        VerificationManifest::new(feature_id, project_root)
    };
    if let Some(trace) = plan_trace {
        if let Some(existing) = manifest.plan_trace.as_ref() {
            if existing != trace {
                anyhow::bail!(
                    "existing manifest plan trace does not match dispatch plan: {} != {}",
                    existing.plan_sha256,
                    trace.plan_sha256
                );
            }
        } else {
            manifest.plan_trace = Some(trace.clone());
        }
    }
    manifest.overall_status = if existing_manifest {
        ManifestStatus::Pending
    } else {
        ManifestStatus::Queued
    };
    manifest.run_id = Some(run_id.to_string());
    NullSaver
        .save_and_emit(&manifest, manifest_path, SaveIntent::FeatureCompleted)
        .context("persisting Queued manifest at dispatch time")?;
    Ok(manifest_path.to_path_buf())
}

/// Arguments passed into the spawned orchestrator closure. Owned so the
/// future satisfies `'static`.
#[derive(Clone)]
pub struct OrchestratorSpawnArgs {
    pub feature_id: String,
    pub run_id: String,
    pub plan_path: PathBuf,
    pub manifest_path: PathBuf,
    pub env: Arc<JobEnv>,
    pub plan_content: String,
    pub plan_trace: Option<PlanTrace>,
    pub layers_config: Option<pice_core::layers::LayersConfig>,
    pub contract_contents: BTreeMap<String, String>,
    pub stack_loop_snapshot: Option<StackLoopInputSnapshot>,
}

/// Guards that make a background feature the single writer for its
/// manifest until the spawned worker finishes.
///
/// Foreground evaluation takes both locks directly in `handlers::evaluate`.
/// Background dispatch must take the same locks before the Queued write and
/// keep them alive across the detached future; otherwise foreground and
/// background runs of the same plan can race on the manifest source of truth.
struct BackgroundManifestLocks {
    _process_guard: OwnedMutexGuard<()>,
    _file_guard: File,
}

async fn acquire_background_manifest_locks(
    ctx: &DaemonContext,
    feature_id: &str,
    manifest_path: &Path,
) -> Result<BackgroundManifestLocks> {
    let project_namespace =
        pice_core::layers::manifest::manifest_project_namespace(ctx.project_root().as_path());
    let process_lock = ctx.manifest_lock_for(&project_namespace, feature_id);
    let process_guard = process_lock.lock_owned().await;

    let manifest_path = manifest_path.to_path_buf();
    let file_guard = tokio::task::spawn_blocking(move || {
        use fs2::FileExt;
        use std::fs::OpenOptions;

        if let Some(parent) = manifest_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let lock_path = manifest_path.with_extension("manifest.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        file.lock_exclusive()?;
        anyhow::Ok(file)
    })
    .await
    .map_err(|e| anyhow::anyhow!("spawn_blocking joined with error: {e}"))??;

    Ok(BackgroundManifestLocks {
        _process_guard: process_guard,
        _file_guard: file_guard,
    })
}

/// Dispatch a background `{evaluate, execute}` request.
///
/// The caller supplies a `future_builder` that owns all runtime state
/// (EventBus clone, LogStore clone, PiceConfig clone, etc.) the
/// detached orchestrator needs. The helper handles the handshake,
/// dispatch-marker manifest write, and spawn.
///
/// The spawned future's FIRST action is `global_sem.acquire_owned().await`
/// (handled inside [`crate::jobs::FeatureJobManager::spawn`]). After the
/// permit is held, the future must transition the dispatch marker to
/// `InProgress` via its own `ManifestSaver` and then run the orchestrator.
/// The helper does NOT own that transition — it only pre-writes the dispatch
/// marker so the reconciliation invariant holds (a crashed pre-transition
/// feature is observable on disk).
pub async fn dispatch_background<F, Fut>(
    feature_id: String,
    json_mode: bool,
    plan_path: &Path,
    ctx: &DaemonContext,
    inputs: BackgroundDispatchInputs,
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

    // Step 2/3: fast-path lookup for any existing live run.
    if let Some(existing_run) = ctx.jobs().run_id_for(&feature_id) {
        return Ok(feature_already_running_response(
            &feature_id,
            &existing_run,
            json_mode,
        ));
    }

    // Serialize the brief admission window separately from the long-held
    // manifest lock. Without this, a job can become visible in the manager
    // before its Queued manifest exists on disk.
    let project_namespace =
        pice_core::layers::manifest::manifest_project_namespace(ctx.project_root().as_path());
    let admission_lock = ctx.background_admission_lock_for(&project_namespace, &feature_id);
    let _admission_guard = admission_lock.lock_owned().await;

    // Re-check under the admission lock so a duplicate caller that arrived
    // during the first caller's Queued write returns the existing run_id.
    if let Some(existing_run) = ctx.jobs().run_id_for(&feature_id) {
        return Ok(feature_already_running_response(
            &feature_id,
            &existing_run,
            json_mode,
        ));
    }

    // Step 4: allocate a fresh run_id.
    let run_id = ctx.jobs().next_run_id();

    // Step 5: snapshot env and dispatch-time file content. Execute/evaluate
    // callers that have already parsed a contract pass that exact content
    // here to close the read-validate-dispatch TOCTOU window.
    let BackgroundDispatchInputs {
        workflow_snapshot,
        plan_snapshot,
        layers_config,
    } = inputs;
    let (plan_content, plan_trace) = match plan_snapshot {
        Some(snapshot) => (snapshot.content, snapshot.trace),
        None => {
            let content = std::fs::read_to_string(plan_path).with_context(|| {
                format!(
                    "reading plan content at dispatch from {}",
                    plan_path.display()
                )
            })?;
            (content, None)
        }
    };
    let contract_paths = layers_config
        .as_ref()
        .map(|layers| collect_contract_paths_from_layers(ctx.project_root(), layers))
        .unwrap_or_else(|| collect_contract_paths(ctx.project_root()));
    let contract_contents = layers_config
        .as_ref()
        .map(|layers| collect_contract_contents(ctx.project_root(), layers))
        .unwrap_or_default();
    let stack_loop_snapshot = layers_config
        .as_ref()
        .map(|layers| {
            collect_stack_loop_input_snapshot(ctx.project_root(), layers, &workflow_snapshot)
        })
        .transpose()?;
    let env = Arc::new(build_job_env(
        ctx.project_root(),
        workflow_snapshot,
        contract_paths,
        plan_trace.clone(),
    )?);

    // Step 6: derive the manifest path and durably write Queued before
    // inserting the job into the manager's observable live-run map.
    let manifest_path = VerificationManifest::manifest_path_in_state_dir(
        &feature_id,
        ctx.project_root(),
        &env.state_dir,
    );
    ctx.logs().reset_feature(&feature_id).await;
    let manifest_locks =
        acquire_background_manifest_locks(ctx, &feature_id, &manifest_path).await?;
    write_queued_manifest(
        &feature_id,
        &run_id,
        ctx.project_root(),
        &manifest_path,
        plan_trace.as_ref(),
    )?;

    let (start_tx, start_rx) = oneshot::channel::<()>();
    let manifest_locks_slot: Arc<StdMutex<Option<BackgroundManifestLocks>>> =
        Arc::new(StdMutex::new(Some(manifest_locks)));

    // Step 7: hand the owned builder to the job manager.
    let spawn_args = OrchestratorSpawnArgs {
        feature_id: feature_id.clone(),
        run_id: run_id.clone(),
        plan_path: plan_path.to_path_buf(),
        manifest_path: manifest_path.clone(),
        env: Arc::clone(&env),
        plan_content,
        plan_trace,
        layers_config,
        contract_contents,
        stack_loop_snapshot,
    };
    let events_for_rescue = ctx.events().clone();
    let logs_for_rescue = ctx.logs().clone();
    let manifest_locks_for_task = Arc::clone(&manifest_locks_slot);
    let spawn_result = ctx.jobs().spawn_after_signal(
        feature_id.clone(),
        run_id.clone(),
        env,
        start_rx,
        move |_env, permit, cancel| {
            let rescue_args = spawn_args.clone();
            let events_for_rescue = events_for_rescue.clone();
            let logs_for_rescue = logs_for_rescue.clone();
            let manifest_locks_for_task = Arc::clone(&manifest_locks_for_task);
            async move {
                let _manifest_locks = {
                    let mut slot = manifest_locks_for_task
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    slot.take().ok_or_else(|| {
                        anyhow::anyhow!(
                            "background manifest locks were not installed before worker start"
                        )
                    })?
                };
                match future_builder(spawn_args, permit, cancel).await {
                    Ok(manifest) => Ok(manifest),
                    Err(err) => {
                        fail_closed_after_background_error(
                            &rescue_args,
                            &events_for_rescue,
                            &logs_for_rescue,
                            err,
                        )
                        .await
                    }
                }
            }
        },
    );
    match spawn_result {
        Ok(_actual_run_id) => {
            ctx.defer_background_start(&feature_id, &run_id, start_tx);
            Ok(background_dispatched_response(&feature_id, &run_id))
        }
        Err(SpawnError {
            feature_id: _,
            run_id: existing,
        }) => {
            // This should be unreachable for production dispatches because
            // the admission lock serializes the re-check and spawn. If a
            // test or future caller bypasses that lock, do not leave this
            // abandoned Queued manifest behind.
            let _ = std::fs::remove_file(&manifest_path);
            Ok(feature_already_running_response(
                &feature_id,
                &existing,
                json_mode,
            ))
        }
    }
}

async fn fail_closed_after_background_error(
    args: &OrchestratorSpawnArgs,
    events: &crate::events::EventBus,
    logs: &crate::logs::LogStore,
    err: anyhow::Error,
) -> Result<VerificationManifest> {
    tracing::error!(
        feature_id = %args.feature_id,
        run_id = %args.run_id,
        error = %err,
        "background worker returned Err; attempting fail-closed terminalization"
    );
    let mut manifest = match VerificationManifest::load(&args.manifest_path) {
        Ok(manifest) => manifest,
        Err(load_err) => {
            logs.append_terminal_frame(&args.feature_id, &args.run_id, "manifest-load-failed")
                .await;
            return Err(load_err.context(format!(
                "failed to load manifest {}; refusing to replace source of truth after worker failure: {err:#}",
                args.manifest_path.display()
            )));
        }
    };
    manifest.run_id = Some(args.run_id.clone());
    if manifest.plan_trace.is_none() {
        manifest.plan_trace = args.plan_trace.clone();
    }
    manifest.overall_status = ManifestStatus::Failed;
    manifest
        .layers
        .push(pice_core::layers::manifest::LayerResult {
            name: args.feature_id.clone(),
            status: pice_core::layers::manifest::LayerStatus::Failed,
            passes: Vec::new(),
            seam_checks: Vec::new(),
            halted_by: Some(format!("runtime_error:{err:#}")),
            final_confidence: None,
            total_cost_usd: None,
            escalation_events: None,
        });

    match finalize_terminal_manifest(&manifest, &args.manifest_path, events) {
        Ok(()) => {
            logs.append_terminal_frame(&args.feature_id, &args.run_id, "failed")
                .await;
            Ok(manifest)
        }
        Err(save_err) => {
            logs.append_terminal_frame(&args.feature_id, &args.run_id, "terminal-save-failed")
                .await;
            Err(save_err.context(format!(
                "failed to terminalize background error after worker failure: {err:#}"
            )))
        }
    }
}

/// Shared body for the spawned future's dispatch-marker → InProgress transition.
///
/// Loads the just-written manifest from disk, flips `overall_status` to
/// `InProgress`, stamps `run_id`, and saves via an [`NullSaver`] so the
/// durable transition lands on disk without emitting a layer event. Fresh
/// dispatches arrive as `Queued`; resume dispatches with preserved state may
/// arrive as `Pending`. This transition deliberately does NOT emit a
/// `LayerStarted` event: Stack Loops owns those events after it computes the
/// active DAG, so subscribers never see a false start for an inactive first
/// configured layer.
pub fn transition_queued_to_in_progress(
    args: &OrchestratorSpawnArgs,
) -> Result<VerificationManifest> {
    let mut manifest = VerificationManifest::load(&args.manifest_path).with_context(|| {
        format!(
            "loading dispatch manifest at {} for transition to InProgress",
            args.manifest_path.display()
        )
    })?;
    manifest.overall_status = ManifestStatus::InProgress;
    manifest.run_id = Some(args.run_id.clone());
    if manifest.plan_trace.is_none() {
        manifest.plan_trace = args.plan_trace.clone();
    }
    crate::events::NullSaver.save_and_emit(
        &manifest,
        &args.manifest_path,
        SaveIntent::LayerStarted {
            layer: args.feature_id.clone(),
        },
    )?;
    Ok(manifest)
}

/// Persist a terminal manifest from the spawned future and emit the matching
/// terminal event. Wrapping the save behind a helper keeps both evaluate and
/// execute closures consistent.
pub fn finalize_terminal_manifest(
    manifest: &VerificationManifest,
    manifest_path: &Path,
    events: &crate::events::EventBus,
) -> Result<()> {
    let saver = crate::events::EventEmittingSaver::new(events);
    saver.save_and_emit(
        manifest,
        manifest_path,
        terminal_save_intent_for_manifest(manifest),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventBus;
    use crate::server::router::DaemonContext;
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

    fn json_response_run_id(resp: &CommandResponse) -> String {
        match resp {
            CommandResponse::Json { value } => value["run_id"]
                .as_str()
                .expect("response has run_id")
                .to_string(),
            other => panic!("expected Json response, got {other:?}"),
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
    fn collect_contract_contents_snapshots_file_text() {
        let project_tmp = tempfile::tempdir().unwrap();
        let pice_dir = project_tmp.path().join(".pice");
        let contracts_dir = pice_dir.join("contracts");
        std::fs::create_dir_all(&contracts_dir).unwrap();
        std::fs::write(contracts_dir.join("backend.toml"), "original").unwrap();

        let layers = pice_core::layers::LayersConfig {
            layers: pice_core::layers::LayersTable {
                order: vec!["backend".to_string()],
                defs: BTreeMap::from([(
                    "backend".to_string(),
                    pice_core::layers::LayerDef {
                        paths: vec!["src/**".to_string()],
                        always_run: false,
                        contract: None,
                        depends_on: Vec::new(),
                        layer_type: None,
                        environment_variants: None,
                    },
                )]),
            },
            seams: None,
            external_contracts: None,
            stacks: None,
        };

        let contents = collect_contract_contents(project_tmp.path(), &layers);
        std::fs::write(contracts_dir.join("backend.toml"), "mutated").unwrap();

        assert_eq!(
            contents.get("backend").map(String::as_str),
            Some("original")
        );
    }

    #[test]
    fn collect_stack_loop_input_snapshot_freezes_diff_guidance_and_seam_paths() {
        let project_tmp = tempfile::tempdir().unwrap();
        let root = project_tmp.path();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(root)
            .output()
            .unwrap();
        std::fs::create_dir_all(root.join("src/backend")).unwrap();
        std::fs::create_dir_all(root.join("src/frontend")).unwrap();
        std::fs::write(root.join("src/backend/app.rs"), "fn old() {}\n").unwrap();
        std::fs::write(root.join("src/frontend/app.ts"), "export const old = 1;\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(root)
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
            .current_dir(root)
            .output()
            .unwrap();
        std::fs::write(
            root.join("src/backend/app.rs"),
            "fn original_snapshot() {}\n",
        )
        .unwrap();
        std::fs::write(root.join("AGENTS.md"), "original guidance").unwrap();

        let layers = pice_core::layers::LayersConfig {
            layers: pice_core::layers::LayersTable {
                order: vec!["backend".to_string(), "frontend".to_string()],
                defs: BTreeMap::from([
                    (
                        "backend".to_string(),
                        pice_core::layers::LayerDef {
                            paths: vec!["src/backend/**".to_string()],
                            always_run: false,
                            contract: None,
                            depends_on: Vec::new(),
                            layer_type: None,
                            environment_variants: None,
                        },
                    ),
                    (
                        "frontend".to_string(),
                        pice_core::layers::LayerDef {
                            paths: vec!["src/frontend/**".to_string()],
                            always_run: false,
                            contract: None,
                            depends_on: Vec::new(),
                            layer_type: None,
                            environment_variants: None,
                        },
                    ),
                ]),
            },
            seams: Some(BTreeMap::from([(
                "backend↔frontend".to_string(),
                vec!["config_mismatch".to_string()],
            )])),
            external_contracts: None,
            stacks: None,
        };
        let workflow = pice_core::workflow::loader::embedded_defaults();

        let snapshot = collect_stack_loop_input_snapshot(root, &layers, &workflow).unwrap();
        std::fs::write(root.join("src/backend/app.rs"), "fn late_mutation() {}\n").unwrap();
        std::fs::write(root.join("AGENTS.md"), "late guidance").unwrap();

        assert!(snapshot.full_diff.contains("original_snapshot"));
        assert!(!snapshot.full_diff.contains("late_mutation"));
        assert_eq!(snapshot.claude_md, "original guidance");
        assert!(
            snapshot
                .layer_paths
                .get("frontend")
                .is_some_and(|paths| paths
                    .iter()
                    .any(|p| p == &PathBuf::from("src/frontend/app.ts"))),
            "seam path snapshot should include unchanged counterpart files"
        );
        assert_eq!(
            snapshot
                .seam_file_contents
                .get(&PathBuf::from("src/backend/app.rs"))
                .map(String::as_str),
            Some("fn original_snapshot() {}\n")
        );
        assert_eq!(
            snapshot
                .seam_file_contents
                .get(&PathBuf::from("src/frontend/app.ts"))
                .map(String::as_str),
            Some("export const old = 1;\n")
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
        let path = VerificationManifest::manifest_path_in_state_dir(
            "feat-queued",
            project_tmp.path(),
            state_tmp.path(),
        );
        let path =
            write_queued_manifest("feat-queued", "r-1", project_tmp.path(), &path, None).unwrap();
        let loaded = VerificationManifest::load(&path).unwrap();
        assert_eq!(loaded.overall_status, ManifestStatus::Queued);
        assert_eq!(loaded.run_id.as_deref(), Some("r-1"));
    }

    #[tokio::test]
    async fn write_queued_manifest_preserves_existing_layers_and_gates() {
        let state_tmp = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::new(state_tmp.path());
        let project_tmp = tempfile::tempdir().unwrap();
        let path = VerificationManifest::manifest_path_in_state_dir(
            "feat-resume",
            project_tmp.path(),
            state_tmp.path(),
        );
        let mut existing = VerificationManifest::new("feat-resume", project_tmp.path());
        existing.add_layer_result(pice_core::layers::manifest::LayerResult {
            name: "infrastructure".to_string(),
            status: pice_core::layers::manifest::LayerStatus::Passed,
            passes: Vec::new(),
            seam_checks: Vec::new(),
            halted_by: Some("sprt_confidence_reached".to_string()),
            final_confidence: Some(0.95),
            total_cost_usd: Some(0.001),
            escalation_events: None,
        });
        existing.gates.push(pice_core::layers::manifest::GateEntry {
            id: "feat-resume:infrastructure:0001".to_string(),
            layer: "infrastructure".to_string(),
            status: pice_core::layers::manifest::GateStatus::Approved,
            trigger_expression: "layer == infrastructure".to_string(),
            requested_at: "2026-05-12T00:00:00Z".to_string(),
            timeout_at: "2026-05-13T00:00:00Z".to_string(),
            on_timeout_action: pice_core::workflow::schema::OnTimeout::Reject,
            reject_attempts_remaining: 0,
            decision: Some("approve".to_string()),
            decided_at: Some("2026-05-12T00:01:00Z".to_string()),
        });
        existing.save(&path).unwrap();

        let path =
            write_queued_manifest("feat-resume", "r-2", project_tmp.path(), &path, None).unwrap();
        let loaded = VerificationManifest::load(&path).unwrap();
        assert_eq!(loaded.overall_status, ManifestStatus::Pending);
        assert_eq!(loaded.run_id.as_deref(), Some("r-2"));
        assert_eq!(loaded.layers.len(), 1);
        assert_eq!(loaded.layers[0].name, "infrastructure");
        assert_eq!(loaded.gates.len(), 1);
        assert_eq!(
            loaded.gates[0].status,
            pice_core::layers::manifest::GateStatus::Approved
        );
    }

    #[tokio::test]
    async fn transition_queued_to_in_progress_writes_in_progress_without_start_event() {
        let state_tmp = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::new(state_tmp.path());
        let project_tmp = tempfile::tempdir().unwrap();
        let manifest_path = VerificationManifest::manifest_path_in_state_dir(
            "feat-transition",
            project_tmp.path(),
            state_tmp.path(),
        );
        let manifest_path = write_queued_manifest(
            "feat-transition",
            "r-xyz",
            project_tmp.path(),
            &manifest_path,
            None,
        )
        .unwrap();

        let events = EventBus::new();
        let mut rx = events.subscribe_feature("feat-transition");

        let args = OrchestratorSpawnArgs {
            feature_id: "feat-transition".into(),
            run_id: "r-xyz".into(),
            plan_path: project_tmp.path().join("plan.md"),
            manifest_path: manifest_path.clone(),
            env: Arc::new(JobEnv {
                state_dir: state_tmp.path().to_path_buf(),
                project_root: project_tmp.path().to_path_buf(),
                workflow_snapshot: pice_core::workflow::loader::embedded_defaults(),
                contracts: BTreeMap::new(),
                pice_state_dir_override: None,
                pice_user_workflow_file: None,
                plan_trace: None,
            }),
            plan_content: "# Plan\n".to_string(),
            plan_trace: None,
            layers_config: None,
            contract_contents: BTreeMap::new(),
            stack_loop_snapshot: None,
        };
        let manifest = transition_queued_to_in_progress(&args).unwrap();
        assert_eq!(manifest.overall_status, ManifestStatus::InProgress);
        assert_eq!(manifest.run_id.as_deref(), Some("r-xyz"));

        let loaded = VerificationManifest::load(&manifest_path).unwrap();
        assert_eq!(loaded.overall_status, ManifestStatus::InProgress);

        match rx.try_recv() {
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {}
            other => panic!("dispatch marker transition must not emit LayerStarted, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatch_background_snapshots_plan_layers_and_contract_contents() {
        let state_tmp = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::new(state_tmp.path());
        let project_tmp = tempfile::tempdir().unwrap();
        let project_root = project_tmp.path();
        let pice_dir = project_root.join(".pice");
        let contracts_dir = pice_dir.join("contracts");
        std::fs::create_dir_all(&contracts_dir).unwrap();
        std::fs::write(contracts_dir.join("backend.toml"), "contract-original").unwrap();
        let plan_path = project_root.join("plan.md");
        std::fs::write(&plan_path, "# Original\n").unwrap();
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
        let layers_snapshot = pice_core::layers::LayersConfig::load(&pice_dir.join("layers.toml"))
            .expect("layers load");
        let plan_trace = PlanTrace {
            plan_path: "plan.md".to_string(),
            plan_sha256: "original-plan-sha".to_string(),
            contract_sha256: "original-contract-sha".to_string(),
            contract_feature: "Snapshot Feature".to_string(),
            contract_tier: 2,
            has_spec_traceability: true,
        };
        let ctx = DaemonContext::new("tok".to_string(), project_root.to_path_buf());

        let held = ctx
            .jobs()
            .provider_semaphore()
            .try_acquire_many_owned(ctx.jobs().provider_capacity())
            .expect("hold all provider permits");
        let (obs_tx, obs_rx) = tokio::sync::oneshot::channel();

        let resp = dispatch_background(
            "snapshot-feature".to_string(),
            true,
            &plan_path,
            &ctx,
            BackgroundDispatchInputs {
                workflow_snapshot: pice_core::workflow::loader::embedded_defaults(),
                plan_snapshot: Some(PlanDispatchSnapshot {
                    content: "# Original\n".to_string(),
                    trace: Some(plan_trace.clone()),
                }),
                layers_config: Some(layers_snapshot),
            },
            move |args, _permit, _cancel| async move {
                let observed = (
                    args.plan_content.clone(),
                    args.plan_trace.clone(),
                    args.env.plan_trace.clone(),
                    args.layers_config
                        .as_ref()
                        .map(|layers| layers.layers.order.clone())
                        .unwrap_or_default(),
                    args.contract_contents.get("backend").cloned(),
                    args.stack_loop_snapshot.is_some(),
                );
                let _ = obs_tx.send(observed);
                Ok(VerificationManifest::new(
                    &args.feature_id,
                    &args.env.project_root,
                ))
            },
        )
        .await
        .expect("dispatch");
        assert!(matches!(resp, CommandResponse::Json { .. }));
        let run_id = json_response_run_id(&resp);
        let queued_manifest_path = VerificationManifest::manifest_path_in_state_dir(
            "snapshot-feature",
            project_root,
            state_tmp.path(),
        );
        let queued = VerificationManifest::load(&queued_manifest_path).unwrap();
        assert_eq!(queued.plan_trace.as_ref(), Some(&plan_trace));

        std::fs::write(&plan_path, "# Mutated\n").unwrap();
        std::fs::write(
            pice_dir.join("layers.toml"),
            r#"
[layers]
order = ["frontend"]

[layers.frontend]
paths = ["web/**"]
"#,
        )
        .unwrap();
        std::fs::write(contracts_dir.join("backend.toml"), "contract-mutated").unwrap();
        drop(held);
        assert!(ctx.release_background_start("snapshot-feature", &run_id));

        let (
            plan_content,
            args_trace,
            env_trace,
            layer_order,
            contract_content,
            has_stack_snapshot,
        ) = tokio::time::timeout(std::time::Duration::from_secs(2), obs_rx)
            .await
            .expect("worker observed snapshots")
            .expect("snapshot sender");
        assert_eq!(plan_content, "# Original\n");
        assert_eq!(args_trace.as_ref(), Some(&plan_trace));
        assert_eq!(env_trace.as_ref(), Some(&plan_trace));
        assert_eq!(layer_order, vec!["backend".to_string()]);
        assert_eq!(contract_content.as_deref(), Some("contract-original"));
        assert!(has_stack_snapshot);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatch_background_defers_worker_start_until_response_release() {
        let state_tmp = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::new(state_tmp.path());
        let project_tmp = tempfile::tempdir().unwrap();
        let project_root = project_tmp.path();
        let pice_dir = project_root.join(".pice");
        std::fs::create_dir_all(&pice_dir).unwrap();
        let plan_path = project_root.join("deferred.md");
        std::fs::write(&plan_path, "# Plan\n").unwrap();
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
        let layers_snapshot = pice_core::layers::LayersConfig::load(&pice_dir.join("layers.toml"))
            .expect("layers load");
        let ctx = DaemonContext::new("tok".to_string(), project_root.to_path_buf());
        let (started_tx, mut started_rx) = tokio::sync::mpsc::channel(1);

        let resp = dispatch_background(
            "deferred-feature".to_string(),
            true,
            &plan_path,
            &ctx,
            BackgroundDispatchInputs {
                workflow_snapshot: pice_core::workflow::loader::embedded_defaults(),
                plan_snapshot: None,
                layers_config: Some(layers_snapshot),
            },
            move |args, _permit, _cancel| async move {
                started_tx.send(args.run_id.clone()).await.unwrap();
                Ok(VerificationManifest::new(
                    &args.feature_id,
                    &args.env.project_root,
                ))
            },
        )
        .await
        .expect("dispatch");
        let run_id = json_response_run_id(&resp);

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), started_rx.recv())
                .await
                .is_err(),
            "worker must not start before the dispatch response has been released"
        );
        assert_eq!(ctx.deferred_background_start_count(), 1);
        assert!(ctx.release_background_start("deferred-feature", &run_id));
        let observed_run =
            tokio::time::timeout(std::time::Duration::from_secs(2), started_rx.recv())
                .await
                .expect("worker starts after release")
                .expect("started message");
        assert_eq!(observed_run, run_id);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatch_background_waits_for_manifest_lock_before_queued_write() {
        let state_tmp = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::new(state_tmp.path());
        let project_tmp = tempfile::tempdir().unwrap();
        let project_root = project_tmp.path();
        let pice_dir = project_root.join(".pice");
        std::fs::create_dir_all(&pice_dir).unwrap();
        let plan_path = project_root.join("locked.md");
        std::fs::write(&plan_path, "# Plan\n").unwrap();
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
        let layers_snapshot = pice_core::layers::LayersConfig::load(&pice_dir.join("layers.toml"))
            .expect("layers load");
        let ctx = Arc::new(DaemonContext::new(
            "tok".to_string(),
            project_root.to_path_buf(),
        ));
        let feature_id = "locked-feature".to_string();
        let namespace = pice_core::layers::manifest::manifest_project_namespace(project_root);
        let held_lock = ctx
            .manifest_lock_for(&namespace, &feature_id)
            .lock_owned()
            .await;
        let manifest_path = VerificationManifest::manifest_path_in_state_dir(
            &feature_id,
            project_root,
            state_tmp.path(),
        );

        let ctx_for_task = Arc::clone(&ctx);
        let plan_for_task = plan_path.clone();
        let feature_for_task = feature_id.clone();
        let dispatch_task = tokio::spawn(async move {
            dispatch_background(
                feature_for_task,
                true,
                &plan_for_task,
                ctx_for_task.as_ref(),
                BackgroundDispatchInputs {
                    workflow_snapshot: pice_core::workflow::loader::embedded_defaults(),
                    plan_snapshot: None,
                    layers_config: Some(layers_snapshot),
                },
                move |args, _permit, _cancel| async move {
                    Ok(VerificationManifest::new(
                        &args.feature_id,
                        &args.env.project_root,
                    ))
                },
            )
            .await
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !manifest_path.exists(),
            "background dispatch must not write Queued while the manifest lock is held"
        );
        assert!(
            !dispatch_task.is_finished(),
            "dispatch should wait for the same manifest lock used by foreground evaluate"
        );
        assert_eq!(
            ctx.jobs().active_count(),
            0,
            "job must not become observable before Queued is durable"
        );

        drop(held_lock);
        let resp = tokio::time::timeout(std::time::Duration::from_secs(2), dispatch_task)
            .await
            .expect("dispatch unblocked after manifest lock release")
            .expect("dispatch task joined")
            .expect("dispatch succeeded");
        assert!(matches!(resp, CommandResponse::Json { .. }));
        let run_id = json_response_run_id(&resp);
        assert!(
            manifest_path.exists(),
            "Queued manifest should be written after the lock is released"
        );
        assert!(ctx.release_background_start(&feature_id, &run_id));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn duplicate_dispatch_during_admission_returns_existing_run_after_queued_write() {
        let state_tmp = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::new(state_tmp.path());
        let project_tmp = tempfile::tempdir().unwrap();
        let project_root = project_tmp.path();
        let pice_dir = project_root.join(".pice");
        std::fs::create_dir_all(&pice_dir).unwrap();
        let plan_path = project_root.join("locked.md");
        std::fs::write(&plan_path, "# Plan\n").unwrap();
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
        let layers_snapshot = pice_core::layers::LayersConfig::load(&pice_dir.join("layers.toml"))
            .expect("layers load");
        let ctx = Arc::new(DaemonContext::new(
            "tok".to_string(),
            project_root.to_path_buf(),
        ));
        let feature_id = "locked-feature".to_string();
        let namespace = pice_core::layers::manifest::manifest_project_namespace(project_root);
        let held_lock = ctx
            .manifest_lock_for(&namespace, &feature_id)
            .lock_owned()
            .await;
        let manifest_path = VerificationManifest::manifest_path_in_state_dir(
            &feature_id,
            project_root,
            state_tmp.path(),
        );

        let spawn_dispatch =
            |ctx: Arc<DaemonContext>,
             plan_path: PathBuf,
             feature_id: String,
             layers_snapshot: pice_core::layers::LayersConfig| {
                tokio::spawn(async move {
                    dispatch_background(
                        feature_id,
                        true,
                        &plan_path,
                        ctx.as_ref(),
                        BackgroundDispatchInputs {
                            workflow_snapshot: pice_core::workflow::loader::embedded_defaults(),
                            plan_snapshot: None,
                            layers_config: Some(layers_snapshot),
                        },
                        move |args, _permit, _cancel| async move {
                            Ok(VerificationManifest::new(
                                &args.feature_id,
                                &args.env.project_root,
                            ))
                        },
                    )
                    .await
                })
            };

        let first = spawn_dispatch(
            Arc::clone(&ctx),
            plan_path.clone(),
            feature_id.clone(),
            layers_snapshot.clone(),
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            ctx.jobs().active_count(),
            0,
            "first dispatch must not be visible while queued write is blocked"
        );
        assert!(!manifest_path.exists());

        let second = spawn_dispatch(
            Arc::clone(&ctx),
            plan_path.clone(),
            feature_id.clone(),
            layers_snapshot,
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !second.is_finished(),
            "duplicate dispatch should wait for short admission lock, not create a second job"
        );

        drop(held_lock);

        let first_resp = tokio::time::timeout(std::time::Duration::from_secs(2), first)
            .await
            .expect("first dispatch unblocked")
            .expect("first joined")
            .expect("first succeeded");
        let run_id = json_response_run_id(&first_resp);
        assert!(manifest_path.exists());

        let second_resp = tokio::time::timeout(std::time::Duration::from_secs(2), second)
            .await
            .expect("second dispatch completed after admission")
            .expect("second joined")
            .expect("second returned response");
        match second_resp {
            CommandResponse::ExitJson { code, value } => {
                assert_eq!(code, ExitJsonStatus::FeatureAlreadyRunning.exit_code());
                assert_eq!(
                    value["status"].as_str(),
                    Some(ExitJsonStatus::FeatureAlreadyRunning.as_str())
                );
                assert_eq!(value["run_id"].as_str(), Some(run_id.as_str()));
            }
            other => panic!("expected duplicate ExitJson, got {other:?}"),
        }

        assert!(ctx.release_background_start(&feature_id, &run_id));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatch_background_holds_manifest_lock_until_worker_finishes() {
        let state_tmp = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::new(state_tmp.path());
        let project_tmp = tempfile::tempdir().unwrap();
        let project_root = project_tmp.path();
        let pice_dir = project_root.join(".pice");
        std::fs::create_dir_all(&pice_dir).unwrap();
        let plan_path = project_root.join("held.md");
        std::fs::write(&plan_path, "# Plan\n").unwrap();
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
        let layers_snapshot = pice_core::layers::LayersConfig::load(&pice_dir.join("layers.toml"))
            .expect("layers load");
        let ctx = DaemonContext::new("tok".to_string(), project_root.to_path_buf());
        let feature_id = "held-feature".to_string();
        let namespace = pice_core::layers::manifest::manifest_project_namespace(project_root);
        let lock = ctx.manifest_lock_for(&namespace, &feature_id);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();

        let resp = dispatch_background(
            feature_id.clone(),
            true,
            &plan_path,
            &ctx,
            BackgroundDispatchInputs {
                workflow_snapshot: pice_core::workflow::loader::embedded_defaults(),
                plan_snapshot: None,
                layers_config: Some(layers_snapshot),
            },
            move |args, _permit, _cancel| async move {
                let _ = started_tx.send(());
                finish_rx.await.expect("test releases background worker");
                Ok(VerificationManifest::new(
                    &args.feature_id,
                    &args.env.project_root,
                ))
            },
        )
        .await
        .expect("dispatch");
        assert!(matches!(resp, CommandResponse::Json { .. }));
        let run_id = json_response_run_id(&resp);
        assert!(ctx.release_background_start(&feature_id, &run_id));

        tokio::time::timeout(std::time::Duration::from_secs(2), started_rx)
            .await
            .expect("worker started")
            .expect("started sender");
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                lock.clone().lock_owned()
            )
            .await
            .is_err(),
            "background worker must keep the manifest lock for its full lifecycle"
        );

        finish_tx.send(()).expect("release worker");
        let _guard_after =
            tokio::time::timeout(std::time::Duration::from_secs(2), lock.clone().lock_owned())
                .await
                .expect("manifest lock released after worker finish");
    }

    #[tokio::test]
    async fn background_error_path_terminalizes_failed_manifest_and_log() {
        let state_tmp = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::new(state_tmp.path());
        let project_tmp = tempfile::tempdir().unwrap();
        let manifest_path = VerificationManifest::manifest_path_in_state_dir(
            "feat-error",
            project_tmp.path(),
            state_tmp.path(),
        );
        let manifest_path = write_queued_manifest(
            "feat-error",
            "r-error",
            project_tmp.path(),
            &manifest_path,
            None,
        )
        .unwrap();
        let events = EventBus::new();
        let mut rx = events.subscribe_feature("feat-error");
        let logs = crate::logs::LogStore::new();
        let args = OrchestratorSpawnArgs {
            feature_id: "feat-error".into(),
            run_id: "r-error".into(),
            plan_path: project_tmp.path().join("plan.md"),
            manifest_path: manifest_path.clone(),
            env: Arc::new(JobEnv {
                state_dir: state_tmp.path().to_path_buf(),
                project_root: project_tmp.path().to_path_buf(),
                workflow_snapshot: pice_core::workflow::loader::embedded_defaults(),
                contracts: BTreeMap::new(),
                pice_state_dir_override: None,
                pice_user_workflow_file: None,
                plan_trace: None,
            }),
            plan_content: "# Plan\n".to_string(),
            plan_trace: None,
            layers_config: None,
            contract_contents: BTreeMap::new(),
            stack_loop_snapshot: None,
        };

        let manifest =
            fail_closed_after_background_error(&args, &events, &logs, anyhow::anyhow!("boom"))
                .await
                .expect("fail-closed terminalization");
        assert_eq!(manifest.overall_status, ManifestStatus::Failed);
        let persisted = VerificationManifest::load(&manifest_path).unwrap();
        assert_eq!(persisted.overall_status, ManifestStatus::Failed);

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("terminal event")
            .expect("event");
        assert_eq!(
            event.event,
            pice_core::events::ManifestEvent::FeatureComplete
        );
        assert_eq!(event.data["status"].as_str(), Some("failed"));

        let history = logs.snapshot("feat-error", None).await;
        assert!(history.iter().any(|chunk| {
            chunk.terminal && chunk.run_id == "r-error" && chunk.reason.as_deref() == Some("failed")
        }));
    }

    #[tokio::test]
    async fn background_error_path_does_not_replace_unloadable_manifest() {
        let state_tmp = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::new(state_tmp.path());
        let project_tmp = tempfile::tempdir().unwrap();
        let manifest_path = VerificationManifest::manifest_path_in_state_dir(
            "feat-corrupt",
            project_tmp.path(),
            state_tmp.path(),
        );
        std::fs::create_dir_all(manifest_path.parent().unwrap()).unwrap();
        std::fs::write(&manifest_path, "{not-json").unwrap();

        let events = EventBus::new();
        let mut rx = events.subscribe_feature("feat-corrupt");
        let logs = crate::logs::LogStore::new();
        let args = OrchestratorSpawnArgs {
            feature_id: "feat-corrupt".into(),
            run_id: "r-corrupt".into(),
            plan_path: project_tmp.path().join("plan.md"),
            manifest_path: manifest_path.clone(),
            env: Arc::new(JobEnv {
                state_dir: state_tmp.path().to_path_buf(),
                project_root: project_tmp.path().to_path_buf(),
                workflow_snapshot: pice_core::workflow::loader::embedded_defaults(),
                contracts: BTreeMap::new(),
                pice_state_dir_override: None,
                pice_user_workflow_file: None,
                plan_trace: None,
            }),
            plan_content: "# Plan\n".to_string(),
            plan_trace: None,
            layers_config: None,
            contract_contents: BTreeMap::new(),
            stack_loop_snapshot: None,
        };

        let err =
            fail_closed_after_background_error(&args, &events, &logs, anyhow::anyhow!("boom"))
                .await
                .expect_err("unloadable manifest must not be replaced");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("refusing to replace source of truth"),
            "unexpected error: {rendered}"
        );
        assert_eq!(
            std::fs::read_to_string(&manifest_path).unwrap(),
            "{not-json"
        );

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
                .await
                .is_err(),
            "unloadable manifest path must not emit a terminal manifest/event without a save"
        );

        let history = logs.snapshot("feat-corrupt", None).await;
        assert!(history.iter().any(|chunk| {
            chunk.terminal
                && chunk.run_id == "r-corrupt"
                && chunk.reason.as_deref() == Some("manifest-load-failed")
        }));
    }
}

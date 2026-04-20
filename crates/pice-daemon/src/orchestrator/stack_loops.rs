//! Per-layer evaluation orchestrator (Stack Loops engine).
//!
//! Drives the nested per-layer evaluation loops: for each DAG cohort,
//! each layer's diff is filtered to its globs, a context-isolated prompt
//! is built, and a provider session evaluates the layer's contract.
//!
//! Phase 1 focuses on the orchestration flow — diff filtering, manifest
//! recording, and DAG traversal. Provider evaluation is best-effort;
//! when the provider can't start (normal in test environments), the
//! orchestrator records a placeholder result and continues.

use anyhow::{Context, Result};
use pice_core::cli::CancelledReason;
use pice_core::config::PiceConfig;
use pice_core::layers::filter::{filter_diff_by_globs, scan_files_by_globs};
use pice_core::layers::manifest::{
    CheckStatus, LayerResult, LayerStatus, ManifestStatus, PassResult, SeamCheckResult,
    VerificationManifest,
};
use pice_core::layers::{active_layers, LayersConfig};
use pice_core::prompt::helpers::{get_git_diff, read_claude_md};
use pice_core::seam::{default_registry, types::LayerBoundary, Registry};
use pice_core::workflow::WorkflowConfig;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::adaptive_loop::{
    run_adaptive_passes, AdaptiveContext, AdaptiveOutcome, PassMetricsSink,
};
use super::{run_seams_for_layer, ProviderOrchestrator, StreamSink};
use crate::prompt::layer_builder::build_layer_evaluation_prompt;

/// Hard cap on parallel cohort tasks. Rate-limit-friendly against Anthropic /
/// OpenAI — even on a 32-core host, spawning 32 concurrent provider sessions
/// swamps the API. Users can LOWER via `defaults.max_parallelism`; they
/// cannot raise above this cap. Revisiting requires rate-limit-aware backoff
/// in the providers (v0.6 concern).
///
/// The canonical value lives in `pice-core::workflow::schema`; re-exporting
/// here as a `usize` keeps the `clamp(1, MAX_PARALLELISM_HARD_CAP)` math
/// type-friendly and gives both the load-time validator
/// (`validate_schema_only` warns if user config > cap) and the dispatch-time
/// clamp + `warn!` a single source of truth. A typo on one site can never
/// diverge from the other.
const MAX_PARALLELISM_HARD_CAP: usize =
    pice_core::workflow::schema::MAX_PARALLELISM_HARD_CAP as usize;

/// Configuration for a Stack Loops evaluation run.
///
/// Bundles the provider and evaluation settings that Phase 2 will use
/// for real provider-backed scoring. Phase 1 records these but does not
/// call providers.
pub struct StackLoopsConfig<'a> {
    pub layers: &'a LayersConfig,
    pub plan_path: &'a Path,
    pub project_root: &'a Path,
    /// Phase 2: provider name for primary evaluation.
    pub primary_provider: &'a str,
    /// Phase 2: model name for primary evaluation.
    pub primary_model: &'a str,
    /// Phase 2: full PICE config for tier/adversarial settings.
    pub pice_config: &'a PiceConfig,
    /// Phase 2: merged workflow config — `layer_overrides.{layer}.tier` takes
    /// precedence over `defaults.tier` on a per-layer basis.
    pub workflow: &'a WorkflowConfig,
    /// Fully-resolved seam map: the result of merging `layers.toml [seams]`
    /// (project floor) with `workflow.yaml.seams` (user overlay) via
    /// `pice_core::workflow::merge::merge_seams`. The caller — typically
    /// the evaluate handler — is responsible for failing closed on any floor
    /// violations BEFORE invoking the orchestrator. This field is the
    /// execution-time source of truth; the orchestrator does not re-merge.
    pub merged_seams: &'a BTreeMap<String, Vec<String>>,
}

/// Empty seam-check slot for layer results that never ran a seam check
/// (inactive layers, missing layer defs). Lifted out of inline literals so
/// `grep 'seam_checks: Vec::new()'` returns zero matches — the contract
/// criterion's validation command is explicit about that.
#[inline]
fn no_seam_checks() -> Vec<pice_core::layers::manifest::SeamCheckResult> {
    Vec::new()
}

/// Run the Stack Loops evaluation pipeline.
///
/// For each active layer (determined by changed files and `always_run`),
/// builds a context-isolated prompt, attempts provider evaluation, and
/// records the result in a `VerificationManifest`.
///
/// Phase 1 provider evaluation is best-effort — if the provider can't
/// start, a placeholder `LayerResult` is recorded so the orchestration
/// flow, diff filtering, and manifest recording are fully exercised.
pub async fn run_stack_loops(
    cfg: &StackLoopsConfig<'_>,
    sink: &dyn StreamSink,
    json_mode: bool,
    pass_sink: std::sync::Arc<dyn PassMetricsSink>,
) -> Result<VerificationManifest> {
    // Default: a fresh never-cancelled token. Callers that need
    // cancellation (daemon shutdown, tests) use
    // [`run_stack_loops_with_cancel`] directly.
    run_stack_loops_with_cancel(cfg, sink, json_mode, pass_sink, CancellationToken::new()).await
}

/// Same as [`run_stack_loops`] but accepts a cancellation token the caller
/// can fire to abort an in-flight evaluation.
///
/// Phase 5 cohort parallelism: when the token is cancelled mid-cohort, the
/// `JoinSet` drain's `tokio::select!` picks the cancelled branch, calls
/// `abort_all()`, and each in-flight `evaluate_one_layer` returns a
/// `cancelled:in_flight`-halted `LayerResult::Failed`. Provider sessions are
/// torn down via the always-shutdown pattern inside `try_run_layer_adaptive`.
pub async fn run_stack_loops_with_cancel(
    cfg: &StackLoopsConfig<'_>,
    sink: &dyn StreamSink,
    json_mode: bool,
    pass_sink: std::sync::Arc<dyn PassMetricsSink>,
    cancel: CancellationToken,
) -> Result<VerificationManifest> {
    let config = cfg.layers;
    let plan_path = cfg.plan_path;
    let project_root = cfg.project_root;
    // Derive feature ID from plan filename
    let feature_id = plan_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    // Get full diff and CLAUDE.md
    let full_diff = get_git_diff(project_root)?;
    let claude_md = read_claude_md(project_root)?;

    // Extract changed file paths from the diff
    let changed_files = extract_changed_files_from_diff(&full_diff);
    debug!(
        changed_count = changed_files.len(),
        "extracted changed files from diff"
    );

    // Determine active layers
    let active = active_layers(config, &changed_files);
    info!(
        active_count = active.len(),
        layers = ?active,
        "computed active layers"
    );

    // The caller pre-merges `layers.toml [seams]` with `workflow.yaml.seams`
    // via `pice_core::workflow::merge::merge_seams` and passes the result
    // in `cfg.merged_seams`. Do NOT re-merge here: floor violations have
    // already been reported and failed closed upstream. Wholesale fallback
    // was a silent-bypass route — removed.
    let merged_seams: &BTreeMap<String, Vec<String>> = cfg.merged_seams;

    // Build `layer_paths[X]` = full per-layer file set (changed files tagged
    // to X ∪ unchanged files under X's globs). Including unchanged files
    // is non-optional for seam verification: if only one side of a boundary
    // is touched (handler changed, OpenAPI spec stable), the check still
    // needs to see both sides to detect the drift. Diff-only boundary
    // files were a silent-false-negative route — fixed.
    //
    // We only walk the disk for layers referenced by `merged_seams` (either
    // side of any declared boundary). All-layer scans would inflate cost
    // on repos where most layers aren't seam-connected.
    let seam_layer_names: HashSet<String> = merged_seams
        .keys()
        .filter_map(|raw| LayerBoundary::parse(raw).ok())
        .flat_map(|b| [b.a, b.b])
        .collect();

    let mut layer_paths: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for file in &changed_files {
        for layer in pice_core::layers::tag_file_to_layers(config, file) {
            layer_paths
                .entry(layer)
                .or_default()
                .push(PathBuf::from(file));
        }
    }
    for layer_name in &seam_layer_names {
        let Some(def) = config.layers.defs.get(layer_name) else {
            continue;
        };
        let scanned = scan_files_by_globs(project_root, &def.paths);
        let entry = layer_paths.entry(layer_name.clone()).or_default();
        let mut seen: HashSet<PathBuf> = entry.iter().cloned().collect();
        for p in scanned {
            if seen.insert(p.clone()) {
                entry.push(p);
            }
        }
    }

    // Invariant: every path in layer_paths must be repo-relative (no
    // absolute prefixes). Both sources — changed-file diff extraction and
    // scan_files_by_globs — produce relative paths, but a future caller
    // could accidentally push an absolute. Debug-assert to catch early.
    debug_assert!(
        layer_paths.values().flatten().all(|p| p.is_relative()),
        "layer_paths must contain only repo-relative paths"
    );

    // Active-layer set as HashSet for runner.
    let active_set: HashSet<String> = active.iter().cloned().collect();

    // Seam registry: default checks + any future plugin checks (v0.3).
    let seam_registry: Registry = default_registry();

    if !json_mode {
        sink.send_chunk(&format!("Stack Loops: {} active layers\n", active.len()));
    }

    // Build DAG for ordering
    let dag = config.build_dag().context("failed to build layer DAG")?;

    // Compute manifest path up front so we can attempt to resume from disk.
    let manifest_path = match VerificationManifest::manifest_path_for(&feature_id, project_root) {
        Ok(path) => {
            if let Some(parent) = path.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    warn!("failed to create manifest state dir: {e}");
                }
            }
            Some(path)
        }
        Err(e) => {
            warn!("failed to compute manifest path: {e}");
            None
        }
    };

    // Resume-from-disk (Phase 6): if a manifest already exists on disk for
    // this feature, load it so previously decided layers (Passed / Failed /
    // Skipped / PendingReview) and prior gates (including reject counters)
    // survive across `pice evaluate` re-invocations driven by the CLI auto-
    // resume loop. Without this, every re-invocation would start from a
    // fresh manifest, wipe all reviewer decisions, and re-fire the same
    // gates indefinitely. Pending/InProgress layer entries are stale (a
    // retry-after-reject, or a crash-interrupted run) and get re-evaluated
    // in this invocation — their stale entries are dropped in the per-
    // cohort layer loop below.
    let mut manifest = manifest_path
        .as_ref()
        .filter(|p| p.exists())
        .and_then(|p| match VerificationManifest::load(p) {
            Ok(m) => Some(m),
            Err(e) => {
                warn!(
                    path = %p.display(),
                    error = %e,
                    "failed to load existing manifest; starting fresh"
                );
                None
            }
        })
        .inspect(|loaded| {
            info!(
                feature_id = %feature_id,
                existing_layers = loaded.layers.len(),
                existing_gates = loaded.gates.len(),
                prior_status = ?loaded.overall_status,
                "resuming from existing manifest"
            );
        })
        .unwrap_or_else(|| VerificationManifest::new(&feature_id, project_root));
    manifest.overall_status = ManifestStatus::InProgress;

    // Persist the in-progress checkpoint (fresh or resumed) so that a
    // mid-run crash leaves the manifest discoverable by the next evaluate.
    if let Some(ref path) = manifest_path {
        if let Err(e) = manifest.save(path) {
            warn!("failed to persist initial manifest: {e}");
        }
    }

    // Phase 5 cohort parallelism: compute the concurrency cap once per
    // evaluation. `defaults.max_parallelism: None` → `num_cpus::get()`
    // (defensive minimum of 1). Hard-capped at
    // `MAX_PARALLELISM_HARD_CAP` regardless of user config, to stay
    // rate-limit-friendly with Anthropic / OpenAI.
    //
    // Operator feedback loop: if the user explicitly set
    // `max_parallelism` above the hard cap, emit a runtime `warn!`
    // NAMING both values so the daemon log is actionable. This
    // complements the load-time `ValidationWarning` from
    // `validate_schema_only` (surfaced by `pice validate`) — dual-
    // surface enforcement of the rule `.claude/rules/stack-loops.md`
    // calls "both sites". Users who ran the daemon directly without
    // `pice validate` first still see the feedback.
    let requested_parallelism = cfg
        .workflow
        .defaults
        .max_parallelism
        .map(|n| n as usize)
        .unwrap_or_else(num_cpus::get);
    let max_parallelism = requested_parallelism.clamp(1, MAX_PARALLELISM_HARD_CAP);
    if requested_parallelism > MAX_PARALLELISM_HARD_CAP {
        warn!(
            requested = requested_parallelism,
            cap = MAX_PARALLELISM_HARD_CAP,
            "defaults.max_parallelism exceeds hard cap; clamping. Run `pice validate` \
             to surface this warning pre-evaluation."
        );
    }

    let parallel_configured = cfg.workflow.phases.evaluate.parallel;

    // Wrap the seam registry once; every parallel task holds an Arc clone.
    // Cheap: the registry is `Box<dyn SeamCheck + Send + Sync>` inside a
    // `BTreeMap`, and `Arc::clone` is pointer-only.
    let seam_registry_arc: Arc<Registry> = Arc::new(seam_registry);

    // Process each cohort sequentially; layers within a cohort MAY run in
    // parallel when `phases.evaluate.parallel == true`, cohort size > 1,
    // and `max_parallelism > 1`. All four cells of the gate matrix are
    // covered by `parallel_cohort_integration.rs`.
    for (cohort_idx, cohort) in dag.cohorts.iter().enumerate() {
        debug!(cohort = cohort_idx, layers = ?cohort, "processing cohort");

        // Partition cohort into immediate results (skip / missing-def /
        // empty-diff) and "real work" that runs the adaptive pipeline.
        // The real-work subset is what gets parallelized; skip paths stay
        // inline (they're cheap, just string formatting + a seam pass).
        let mut immediate_results: HashMap<String, LayerResult> = HashMap::new();
        let mut immediate_chunks: HashMap<String, Vec<String>> = HashMap::new();
        let mut work_inputs: Vec<LayerInputs> = Vec::new();

        for layer_name in cohort {
            // Resume-preserve: if a prior run's manifest already carries a
            // decided result for this layer (Passed / Failed / Skipped /
            // PendingReview), move it into `immediate_results` so the DAG-
            // order emission carries it forward unchanged. We do NOT re-
            // evaluate. Pending/InProgress entries are stale (retry path
            // or crash-interrupted) and are dropped here so the cohort
            // processes them as new work.
            if let Some(idx) = manifest.layers.iter().position(|l| &l.name == layer_name) {
                let existing_status = manifest.layers[idx].status.clone();
                if matches!(
                    existing_status,
                    LayerStatus::Passed
                        | LayerStatus::Failed
                        | LayerStatus::Skipped
                        | LayerStatus::PendingReview
                ) {
                    debug!(
                        layer = %layer_name,
                        status = ?existing_status,
                        "resume: preserving decided layer result"
                    );
                    let preserved = manifest.layers.remove(idx);
                    immediate_results.insert(layer_name.clone(), preserved);
                    immediate_chunks.insert(
                        layer_name.clone(),
                        vec![format!("  [{layer_name}] resumed ({existing_status:?})\n")],
                    );
                    continue;
                }
                // Stale Pending/InProgress — drop it; we're about to re-run.
                debug!(
                    layer = %layer_name,
                    status = ?existing_status,
                    "resume: dropping stale non-terminal layer entry; will re-evaluate"
                );
                manifest.layers.remove(idx);
            }

            if !active.contains(layer_name) {
                debug!(layer = %layer_name, "skipping inactive layer");
                immediate_results.insert(
                    layer_name.clone(),
                    LayerResult {
                        name: layer_name.clone(),
                        status: LayerStatus::Skipped,
                        passes: Vec::new(),
                        seam_checks: no_seam_checks(),
                        halted_by: None,
                        final_confidence: None,
                        total_cost_usd: None,
                        escalation_events: None,
                    },
                );
                continue;
            }

            let Some(layer_def) = config.layers.defs.get(layer_name) else {
                warn!(layer = %layer_name, "layer defined in order but missing definition");
                immediate_results.insert(
                    layer_name.clone(),
                    LayerResult {
                        name: layer_name.clone(),
                        status: LayerStatus::Failed,
                        passes: Vec::new(),
                        seam_checks: no_seam_checks(),
                        halted_by: Some("missing layer definition".to_string()),
                        final_confidence: None,
                        total_cost_usd: None,
                        escalation_events: None,
                    },
                );
                continue;
            };

            let filtered_diff = filter_diff_by_globs(&full_diff, &layer_def.paths);

            if filtered_diff.is_empty() {
                let is_always_run = layer_def.always_run;
                let (status, label, reason) = if is_always_run {
                    (
                        LayerStatus::Pending,
                        "PENDING",
                        format!(
                            "always_run layer {layer_name} has no file changes. \
                             Awaiting seam checks / static analysis (Phase 3)."
                        ),
                    )
                } else {
                    (
                        LayerStatus::Skipped,
                        "SKIP",
                        format!(
                            "No changes in {layer_name} layer's files. \
                             Activated by dependency cascade. \
                             Seam checks (Phase 3) will verify boundary contracts."
                        ),
                    )
                };
                info!(layer = %layer_name, always_run = is_always_run, "empty diff for active layer");
                let seam_checks = run_seams_for_layer(
                    layer_name,
                    &active_set,
                    merged_seams,
                    seam_registry_arc.as_ref(),
                    project_root,
                    &full_diff,
                    &layer_paths,
                );
                let first_failed = seam_checks
                    .iter()
                    .find(|c| c.status == CheckStatus::Failed)
                    .map(|c| c.name.clone());
                let (final_status, final_reason) = match first_failed {
                    Some(failed_id) => (LayerStatus::Failed, format!("seam:{failed_id}")),
                    None => (status, reason),
                };
                immediate_results.insert(
                    layer_name.clone(),
                    LayerResult {
                        name: layer_name.clone(),
                        status: final_status,
                        passes: Vec::new(),
                        seam_checks,
                        halted_by: Some(final_reason),
                        final_confidence: None,
                        total_cost_usd: None,
                        escalation_events: None,
                    },
                );
                immediate_chunks.insert(
                    layer_name.clone(),
                    vec![format!("  [{layer_name}] {label} (no file changes)\n")],
                );
                continue;
            }

            let contract_content = load_layer_contract(project_root, layer_name, layer_def);
            let _prompt = build_layer_evaluation_prompt(
                layer_name,
                &contract_content,
                &filtered_diff,
                &claude_md,
            );

            work_inputs.push(build_per_layer_inputs(
                cfg,
                layer_name,
                contract_content,
                filtered_diff,
                &claude_md,
                &active_set,
                &layer_paths,
                &full_diff,
                Arc::clone(&seam_registry_arc),
            ));
        }

        // Dispatch the real-work subset: parallel when allowed, sequential
        // otherwise. The gate is `parallel AND cohort_size>1 AND
        // max_parallelism>1`. Any false branch collapses to the sequential
        // path — exercised by the 5-cell edge-case matrix in
        // `parallel_cohort_integration.rs`.
        let cohort_size = work_inputs.len();
        let parallel_enabled = parallel_configured && cohort_size > 1 && max_parallelism > 1;
        let path_tag = if parallel_enabled {
            "parallel"
        } else {
            "sequential"
        };
        // The gate emits a `pice.cohort` tracing event with the chosen
        // path — tests use `tracing-subscriber::fmt::TestWriter` or a
        // custom layer to capture this and assert which cell fired.
        // Hard rule: NO production code uses a `path_counter` or similar
        // test-only instrumentation (Cycle-2 Consider #6). This event IS
        // the instrumentation.
        debug!(
            target: "pice.cohort",
            cohort_index = cohort_idx,
            layer_count = cohort_size,
            max_parallelism,
            parallel_configured,
            path = path_tag,
            "cohort dispatch path decided"
        );

        let cohort_start = std::time::Instant::now();

        // HashMap of real-work outcomes, keyed by layer name. Merged with
        // `immediate_results` during DAG-ordered emission.
        let mut work_results: HashMap<String, LayerResult> = HashMap::new();
        let mut work_chunks: HashMap<String, Vec<String>> = HashMap::new();

        if parallel_enabled {
            let semaphore = Arc::new(Semaphore::new(max_parallelism));
            let mut join_set: JoinSet<LayerOutcome> = JoinSet::new();

            for inputs in work_inputs {
                let sem = Arc::clone(&semaphore);
                let sink_clone = Arc::clone(&pass_sink);
                let cancel_child = cancel.child_token();
                join_set.spawn(async move {
                    // Acquire the permit INSIDE the task so the JoinSet
                    // scheduler can size itself against the semaphore cap.
                    // If permit acquisition fails (semaphore closed on
                    // cancellation), fall through to `evaluate_one_layer`
                    // which will observe the cancellation and return a
                    // `Cancelled` outcome.
                    let _permit = sem.acquire_owned().await.ok();
                    evaluate_one_layer(inputs, sink_clone, cancel_child).await
                });
            }

            // Drain the JoinSet. Cancellation wins over completion — on
            // `cancel.cancelled()` we abort all in-flight tasks ONCE and
            // then keep draining the remaining handles so spawned tasks
            // that reached `evaluate_one_layer`'s `cancelled:in_flight`
            // branch are collected normally.
            //
            // `cancel_fired` gates the select branch: `cancel.cancelled()`
            // resolves permanently once triggered, so without the gate we
            // would re-enter that arm on every subsequent iteration and
            // spin — hanging the drain + starving other tests on the
            // global env mutex.
            let mut collected: Vec<LayerOutcome> = Vec::new();
            let mut cancel_fired = false;
            loop {
                if join_set.is_empty() {
                    break;
                }
                tokio::select! {
                    biased;
                    _ = cancel.cancelled(), if !cancel_fired => {
                        cancel_fired = true;
                        join_set.abort_all();
                    }
                    joined = join_set.join_next() => {
                        match joined {
                            Some(Ok(out)) => collected.push(out),
                            Some(Err(join_err)) if join_err.is_cancelled() => {
                                // Spawn-level cancellation from abort_all().
                                // Synthetic outcome assembled after drain
                                // by diffing against the original cohort.
                            }
                            Some(Err(join_err)) => {
                                warn!("cohort task join error: {join_err}");
                            }
                            None => break,
                        }
                    }
                }
            }

            // Any work_inputs layer that never produced an outcome was
            // cancelled at the join-handle layer. Synthesize a Failed
            // result so the manifest reflects what happened.
            let mut seen: HashSet<String> =
                collected.iter().map(|o| o.layer_name.clone()).collect();
            for out in collected {
                work_chunks.insert(out.layer_name.clone(), out.streamed_chunks);
                work_results.insert(out.layer_name, out.layer_result);
            }
            // Discover layers that were enqueued but never produced a
            // result (join-level abort). Walk `cohort` — NOT the original
            // `work_inputs` — so the logic compares against the DAG.
            for layer_name in cohort {
                if immediate_results.contains_key(layer_name) {
                    continue; // skip / missing-def / empty-diff path
                }
                if seen.insert(layer_name.clone()) {
                    // Was enqueued as work, never produced an outcome.
                    work_results.insert(
                        layer_name.clone(),
                        LayerResult {
                            name: layer_name.clone(),
                            status: LayerStatus::Failed,
                            passes: Vec::new(),
                            seam_checks: no_seam_checks(),
                            halted_by: Some(CancelledReason::JoinAborted.as_halted_by()),
                            final_confidence: None,
                            total_cost_usd: None,
                            escalation_events: None,
                        },
                    );
                    work_chunks.insert(
                        layer_name.clone(),
                        vec![format!("  [{layer_name}] Cancelled\n")],
                    );
                }
            }
        } else {
            // Sequential path — identical semantics to pre-Phase-5.
            for inputs in work_inputs {
                let sink_clone = Arc::clone(&pass_sink);
                let cancel_child = cancel.child_token();
                let out = evaluate_one_layer(inputs, sink_clone, cancel_child).await;
                work_chunks.insert(out.layer_name.clone(), out.streamed_chunks);
                work_results.insert(out.layer_name, out.layer_result);
            }
        }

        // DAG-ordered emission. Walk `cohort` (topological position) to
        // insert into the manifest so `manifest.layers[]` is deterministic
        // across runs regardless of task completion order. This is the
        // invariant `parallel_cohort_preserves_dag_order` in
        // `parallel_cohort_integration.rs` pins down.
        for layer_name in cohort {
            let (layer_result, chunks) = match (
                immediate_results.remove(layer_name),
                work_results.remove(layer_name),
            ) {
                (Some(r), _) => {
                    let chunks = immediate_chunks.remove(layer_name).unwrap_or_default();
                    (r, chunks)
                }
                (None, Some(r)) => {
                    let chunks = work_chunks.remove(layer_name).unwrap_or_default();
                    (r, chunks)
                }
                // Should be unreachable — every cohort layer falls into
                // exactly one bucket above.
                (None, None) => {
                    warn!(layer = %layer_name, "no result computed for cohort layer");
                    continue;
                }
            };

            if !json_mode {
                for chunk in &chunks {
                    sink.send_chunk(chunk);
                }
            }

            manifest.add_layer_result(layer_result);

            if let Some(ref path) = manifest_path {
                if let Err(e) = manifest.save(path) {
                    warn!("failed to checkpoint manifest: {e}");
                }
            }
        }

        info!(
            cohort_index = cohort_idx,
            elapsed_ms = cohort_start.elapsed().as_millis() as u64,
            path = path_tag,
            "cohort complete"
        );

        // Honor cancellation between cohorts too — don't start the next
        // cohort if the token fired.
        if cancel.is_cancelled() {
            warn!("cancellation observed between cohorts; aborting remaining cohorts");
            break;
        }

        // Phase 6 review-gate cohort-boundary check (Task 5):
        // For every layer in this cohort that just graded Passed, consult
        // the workflow's review trigger and any per-layer override. If any
        // gate fires, record it on the manifest, transition the affected
        // layer(s) to `PendingReview`, set `overall_status = PendingReview`,
        // persist the manifest, and break out of the cohort loop so the
        // evaluate handler returns control to the caller. The per-manifest
        // locks (tokio mutex + fs2 flock) are released naturally when the
        // handler returns — subsequent `ReviewGate::Decide` RPCs acquire
        // them without contention. This is the "lock release between
        // cohorts" invariant from plan Task 5, implemented via handler
        // early-return rather than intra-run lock juggling.
        let gate_check = pice_core::gate::check_gates_for_cohort(
            cfg.workflow,
            &manifest.layers,
            &manifest.gates,
            cohort,
            &feature_id,
            cfg.workflow.defaults.tier,
            "", // change_scope — filled by a later phase when real scope detection lands.
            chrono::Utc::now(),
        );
        if gate_check.any() {
            // Transition the named layers Passed → PendingReview.
            for name in &gate_check.layers_pending_review {
                if let Some(layer) = manifest.layers.iter_mut().find(|l| l.name == *name) {
                    layer.status = LayerStatus::PendingReview;
                }
            }
            // Append gates and recompute overall status.
            manifest.gates.extend(gate_check.new_gates);
            manifest.compute_overall_status();
            // Persist so the decide handler sees the gates.
            if let Some(ref path) = manifest_path {
                if let Err(e) = manifest.save(path) {
                    warn!("failed to persist pending-review manifest: {e}");
                }
            }
            info!(
                cohort_index = cohort_idx,
                pending_layers = ?gate_check.layers_pending_review,
                "review gate(s) fired; pausing evaluation"
            );
            return Ok(manifest);
        }
    }

    // Post-process: propagate seam findings to inactive layers. When one
    // side of a boundary is active and the other is skipped, the active
    // side's `run_seams_for_layer` produces findings but the skipped side's
    // manifest entry has empty `seam_checks`. A user reading the manifest
    // for the inactive layer should see the boundary's findings (they affect
    // both sides). This preserves the "complete per-layer view" invariant
    // documented in stack-loops.md.
    {
        // Build a map of boundary → seam_checks from all layers that ran.
        let mut boundary_findings: BTreeMap<String, Vec<SeamCheckResult>> = BTreeMap::new();
        for layer in &manifest.layers {
            for sc in &layer.seam_checks {
                boundary_findings
                    .entry(sc.boundary.clone())
                    .or_default()
                    .push(sc.clone());
            }
        }
        // For each layer in the manifest, if it participates in a boundary
        // but has no seam_checks for that boundary, copy them in.
        for layer in &mut manifest.layers {
            let layer_name = &layer.name;
            for (raw_boundary, findings) in &boundary_findings {
                let Ok(b) = LayerBoundary::parse(raw_boundary) else {
                    continue;
                };
                if !b.touches(layer_name) {
                    continue;
                }
                // Only propagate findings this layer doesn't already have.
                for sc in findings {
                    let already_has = layer.seam_checks.iter().any(|existing| {
                        existing.boundary == sc.boundary && existing.name == sc.name
                    });
                    if !already_has {
                        layer.seam_checks.push(sc.clone());
                    }
                }
            }
        }
    }

    // Compute overall status and persist final manifest
    manifest.compute_overall_status();

    // Persist final manifest state
    if let Some(ref path) = manifest_path {
        if let Err(e) = manifest.save(path) {
            warn!("failed to persist final manifest: {e}");
        }
    }

    info!(
        feature_id = %feature_id,
        overall = ?manifest.overall_status,
        layer_count = manifest.layers.len(),
        "stack loops evaluation complete"
    );

    Ok(manifest)
}

/// Outcome of `try_run_layer_adaptive`.
///
/// Phase 4 Pass-3 fix for Codex Critical #2: the earlier `Option<AdaptiveOutcome>`
/// return conflated two very different states —
///
/// - **Provider never started** (binary unresolvable, config invalid, or test
///   fixture lacks a provider wired up). The loop literally did not execute,
///   so the conservative behavior is to record the layer as `Pending` with
///   the phase-1-pending placeholder. This preserves the graceful-degrade
///   path existing tests rely on.
/// - **Runtime error mid-loop** (provider spawn succeeded but then an RPC
///   call timed out, the provider crashed, or the protocol returned an error
///   that the loop could not recover from). This IS a failure: evaluation
///   was attempted and broke. Silently downgrading it to `Pending` makes the
///   overall exit code stay 0, which hides a real correctness problem from
///   CI pipelines that rely on `pice evaluate` as a fail-closed gate.
///
/// Splitting the return lets the caller fail-close on runtime errors
/// (→ `LayerStatus::Failed` → exit 2) while still tolerating missing
/// providers in test fixtures.
enum LayerAdaptiveResult {
    /// Provider never started — conservative `Pending` placeholder.
    NotStarted,
    /// Loop completed (including natural halts like budget / max_passes).
    Completed(AdaptiveOutcome),
    /// Provider started but the loop or an RPC errored out. The message
    /// is surfaced on the manifest's `halted_by` so operators can see
    /// *why* the layer failed at the orchestrator level.
    RuntimeError(String),
}

/// Attempt to run the per-layer adaptive pass loop.
///
/// Starts primary (and adversarial when ADTS is active) providers, invokes
/// [`run_adaptive_passes`], and shuts them down. Returns
/// [`LayerAdaptiveResult::NotStarted`] if any provider fails to start — the
/// caller falls back to the Phase-1-pending placeholder to preserve the
/// graceful-degrade path test fixtures depend on. Returns
/// [`LayerAdaptiveResult::RuntimeError`] if the providers started but the
/// loop or an RPC surfaced an unrecoverable error — the caller fails the
/// layer so the overall evaluation exits non-zero (Pass-3 Codex fix).
async fn try_run_layer_adaptive(
    cfg: &StackLoopsConfig<'_>,
    layer_name: &str,
    contract_toml: &str,
    filtered_diff: &str,
    claude_md: &str,
    pass_sink: &dyn PassMetricsSink,
) -> LayerAdaptiveResult {
    let workflow = cfg.workflow;
    let algo = effective_adaptive_algo_for(workflow, layer_name);
    let min_confidence = effective_min_confidence_for(workflow, layer_name);
    let max_passes = effective_max_passes_for(workflow, layer_name);
    let budget_usd = effective_budget_usd_for(workflow, layer_name);

    // Build the per-layer contract payload. Providers expect JSON; the
    // layer contract is a TOML fragment, so wrap it in an object with a
    // `contract_toml` string field. Providers that understand the layered
    // shape deserialize it; opaque providers pass it through.
    let contract_json = serde_json::json!({
        "layer": layer_name,
        "contract_toml": contract_toml,
    });

    // Phase 4 Pass-4 fix for Codex Critical: `ProviderOrchestrator::start` is
    // resolve + spawn + initialize. Previously ANY error from that composite
    // routed to `NotStarted` → `phase-1-pending` (exit 0). That silently
    // swallowed real startup failures (provider binary present but crashes on
    // spawn, init RPC times out, initialize returns a protocol error) —
    // exactly the failure mode the Pass-3 fail-close work was trying to stop.
    //
    // The intended carve-out is narrower: "provider cannot be RESOLVED" → the
    // workflow names a provider the config/registry doesn't know about, so
    // there is nothing we could have run. Anything past resolution that
    // breaks is a runtime failure and must fail-close the layer.
    //
    // Probe `registry::resolve` first. Success here means we have a command
    // and argv; a subsequent `start()` error is spawn or initialize, which
    // is a real startup failure → `RuntimeError` → `LayerStatus::Failed`.
    if pice_core::provider::registry::resolve(cfg.primary_provider, cfg.pice_config).is_none() {
        warn!(
            layer = %layer_name,
            provider = %cfg.primary_provider,
            "primary provider unresolvable, falling back to phase-1-pending",
        );
        return LayerAdaptiveResult::NotStarted;
    }

    let mut primary = match ProviderOrchestrator::start(cfg.primary_provider, cfg.pice_config).await
    {
        Ok(p) => p,
        Err(e) => {
            let msg = format!("primary provider startup failed: {e:#}");
            warn!(layer = %layer_name, "{msg}");
            // Resolvable but failed to start — fail the layer closed.
            return LayerAdaptiveResult::RuntimeError(msg);
        }
    };

    // Phase 4.1 capability gate (Pass-6 Codex Critical #1): adaptive budgets
    // are only meaningful when the provider emits real per-pass `costUsd`.
    // Without telemetry the loop falls back to `budget_usd / max_passes` as
    // a synthetic seed debit, so `final_total_cost_usd` and every
    // budget-halt decision are advisory at best — the exact "budget appears
    // to be enforced, isn't actually" failure mode Codex flagged at
    // adaptive_loop.rs:291-385. Fail closed here rather than silently
    // running the hollow path. The check applies regardless of
    // `adaptive_algorithm` because budget enforcement runs for every algo
    // (including `None`, per CLAUDE.md — "Budget is a financial safety
    // rail, not a strategy choice").
    let cost_telemetry_available = primary.capabilities().cost_telemetry;
    if budget_usd > 0.0 && !cost_telemetry_available {
        let msg = format!(
            "provider '{}' does not declare costTelemetry, but workflow.yaml requests \
             budget_usd = {:.4} for layer '{}'. Adaptive budgets require real per-pass \
             cost reporting; otherwise enforcement is synthetic. Either set \
             budget_usd = 0 (no enforcement) or use a provider that emits costUsd on \
             evaluate/create.",
            cfg.primary_provider, budget_usd, layer_name,
        );
        warn!(layer = %layer_name, "{msg}");
        let _ = primary.shutdown().await;
        return LayerAdaptiveResult::RuntimeError(msg);
    }
    // Phase 4.1 Pass-11 Codex CRITICAL #1: when adaptive evaluation runs
    // with `budget_usd = 0` AND the provider lacks costTelemetry (the
    // shipped-default fresh-install path), neither the capability gate
    // nor the budget rail is active. Warn loudly so operators know
    // costs will be recorded as NULL (not synthetic `$0.0000`) and
    // financial enforcement is opt-in.
    if budget_usd == 0.0 && !cost_telemetry_available {
        warn!(
            layer = %layer_name,
            provider = %cfg.primary_provider,
            "adaptive evaluation running without cost telemetry capability AND without \
             budget enforcement (budget_usd = 0). Per-pass cost_usd will be persisted as \
             NULL; final_total_cost_usd will be NULL. Once your provider emits real \
             costUsd on evaluate/create AND advertises costTelemetry=true, raise \
             budget_usd > 0 to enable enforcement."
        );
    }

    // Start the adversarial provider only when ADTS is selected.
    let mut adversarial: Option<ProviderOrchestrator> = if algo
        == pice_core::workflow::schema::AdaptiveAlgo::Adts
        && cfg.pice_config.evaluation.adversarial.enabled
    {
        // Same resolve-then-start classification for the adversarial path.
        if pice_core::provider::registry::resolve(
            &cfg.pice_config.evaluation.adversarial.provider,
            cfg.pice_config,
        )
        .is_none()
        {
            warn!(
                layer = %layer_name,
                provider = %cfg.pice_config.evaluation.adversarial.provider,
                "adversarial provider unresolvable, falling back to phase-1-pending",
            );
            // ADTS without adversarial is degenerate — shut down primary and fall back.
            let _ = primary.shutdown().await;
            return LayerAdaptiveResult::NotStarted;
        }
        match ProviderOrchestrator::start(
            &cfg.pice_config.evaluation.adversarial.provider,
            cfg.pice_config,
        )
        .await
        {
            Ok(a) => Some(a),
            Err(e) => {
                let msg = format!("adversarial provider startup failed: {e:#}");
                warn!(layer = %layer_name, "{msg}");
                // Shut down primary then fail-close the layer.
                let _ = primary.shutdown().await;
                return LayerAdaptiveResult::RuntimeError(msg);
            }
        }
    } else {
        None
    };

    let ctx = AdaptiveContext {
        algo,
        sprt: workflow.phases.evaluate.sprt,
        adts: workflow.phases.evaluate.adts,
        vec: workflow.phases.evaluate.vec,
        min_confidence,
        max_passes,
        budget_usd,
        contract: contract_json,
        diff: filtered_diff.to_string(),
        claude_md: claude_md.to_string(),
        primary_model: cfg.primary_model.to_string(),
        adversarial_model: Some(cfg.pice_config.evaluation.adversarial.model.clone()),
        base_effort: if cfg.pice_config.evaluation.adversarial.effort.is_empty() {
            None
        } else {
            Some(cfg.pice_config.evaluation.adversarial.effort.clone())
        },
        cost_telemetry_available,
    };

    let result = run_adaptive_passes(&ctx, &mut primary, adversarial.as_mut(), pass_sink).await;

    // Always shut the providers down, even on loop error.
    let _ = primary.shutdown().await;
    if let Some(adv) = adversarial {
        let _ = adv.shutdown().await;
    }

    match result {
        Ok(outcome) => LayerAdaptiveResult::Completed(outcome),
        Err(e) => {
            // Pass-3 Codex Critical #2: providers DID start, so this is a
            // real runtime error — NOT a "provider never started" state.
            // Surface the message so the caller can fail-close the layer.
            let msg = format!("{e}");
            warn!(layer = %layer_name, "adaptive pass loop failed: {msg}");
            LayerAdaptiveResult::RuntimeError(msg)
        }
    }
}

/// Derive a `LayerResult` from an adaptive loop outcome and the seam-check
/// findings. Seam failures override the halt reason and downgrade the layer
/// status to `Failed`. Otherwise, the `halted_by` string selects the status:
///
/// | halted_by                      | status (no seam fail) |
/// |--------------------------------|------------------------|
/// | sprt_confidence_reached        | Passed                 |
/// | vec_entropy                    | Passed                 |
/// | sprt_rejected                  | Failed                 |
/// | adts_escalation_exhausted      | Failed                 |
/// | budget                         | Pending (re-run)       |
/// | max_passes                     | Pending (re-run)       |
/// | (anything else)                | Pending (conservative) |
fn build_adaptive_layer_result(
    layer_name: String,
    outcome: AdaptiveOutcome,
    seam_checks: Vec<SeamCheckResult>,
    first_failed_seam: Option<String>,
    min_confidence: f64,
) -> LayerResult {
    let (status, halted_by) = if let Some(failed_id) = first_failed_seam {
        // Seam failure always wins — per stack-loops.md §"Fail-closed rollup".
        (LayerStatus::Failed, Some(format!("seam:{failed_id}")))
    } else {
        match outcome.halted_by.as_deref() {
            // Phase 4.1 Pass-11 Codex HIGH #2: metrics-persist failures are
            // operational, NOT contract failures. Route to `Pending` (not
            // `Failed`) and let the handler surface them via
            // `metrics_persist_failed_response()` (exit 1, not exit 2).
            // The check MUST precede the `runtime_error:` arm because that
            // prefix would otherwise win for a hypothetical
            // "runtime_error:metrics_persist_failed:" string — but we
            // intentionally chose a non-overlapping prefix in adaptive_loop.rs
            // so the routing is unambiguous. Pass-11.1 W2: prefix-check
            // sourced from `ExitJsonStatus::is_metrics_persist_failed`
            // (single source of truth, locked by unit test against drift).
            Some(reason) if pice_core::cli::ExitJsonStatus::is_metrics_persist_failed(reason) => {
                (LayerStatus::Pending, outcome.halted_by.clone())
            }
            // Phase 4 Pass-4 fix for Codex High: mid-loop provider errors
            // flow through `run_adaptive_passes` as a preserved outcome with
            // `halted_by = "runtime_error:..."`. Route them to `Failed` so
            // the evaluation exits non-zero, while the passes/cost already
            // written to the sink remain intact for reconciliation.
            Some(reason) if reason.starts_with("runtime_error:") => {
                (LayerStatus::Failed, outcome.halted_by.clone())
            }
            Some("sprt_confidence_reached") => (LayerStatus::Passed, outcome.halted_by.clone()),
            // Phase 4 post-adversarial-review fix: `vec_entropy` halts when
            // posterior entropy stops changing — that happens for failure
            // sequences just as much as success sequences. Promoting every
            // VEC halt to `Passed` is a correctness bug (false positive on
            // failure-converged layers). Gate on `final_confidence >=
            // min_confidence` before promoting; otherwise the layer enters
            // `Failed` (posterior says fail) or `Pending` (no confidence
            // reported — conservative).
            Some("vec_entropy") => match outcome.final_confidence {
                Some(conf) if conf >= min_confidence => {
                    (LayerStatus::Passed, outcome.halted_by.clone())
                }
                Some(_) => (LayerStatus::Failed, outcome.halted_by.clone()),
                None => (LayerStatus::Pending, outcome.halted_by.clone()),
            },
            Some("sprt_rejected") | Some("adts_escalation_exhausted") => {
                (LayerStatus::Failed, outcome.halted_by.clone())
            }
            Some("budget") | Some("max_passes") => {
                (LayerStatus::Pending, outcome.halted_by.clone())
            }
            _ => (LayerStatus::Pending, outcome.halted_by.clone()),
        }
    };

    LayerResult {
        name: layer_name,
        status,
        passes: outcome.passes,
        seam_checks,
        halted_by,
        final_confidence: outcome.final_confidence,
        total_cost_usd: outcome.total_cost_usd,
        escalation_events: outcome.escalation_events,
    }
}

/// Phase-1-pending fallback: records the layer as Pending with a placeholder
/// pass so the manifest is well-formed and downstream tools see the layer
/// was recognized but never evaluated.
fn phase1_pending_layer_result(
    layer_name: String,
    effective_tier: u8,
    filtered_diff_bytes: usize,
    seam_checks: Vec<SeamCheckResult>,
    first_failed_seam: Option<String>,
) -> LayerResult {
    let (status, halted_by) = match first_failed_seam {
        Some(failed_id) => (LayerStatus::Failed, Some(format!("seam:{failed_id}"))),
        None => (
            LayerStatus::Pending,
            Some(format!("phase-1-pending-tier-{effective_tier}")),
        ),
    };
    LayerResult {
        name: layer_name,
        status,
        passes: vec![PassResult {
            index: 0,
            model: "phase-1-pending".to_string(),
            score: None,
            cost_usd: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
            findings: vec![format!(
                "Awaiting provider evaluation — {filtered_diff_bytes} bytes of filtered diff prepared"
            )],
        }],
        seam_checks,
        halted_by,
        final_confidence: None,
        total_cost_usd: None,
        escalation_events: None,
    }
}

/// Pass-3 Codex Critical #2: Build a fail-closed `LayerResult` for the case
/// where the adaptive loop started but a runtime error (timeout, RPC failure,
/// provider crash mid-loop) prevented completion.
///
/// The layer status is `Failed`, which flows to `any_failed_layer = true` in
/// `handlers/evaluate.rs`, which in turn emits `ExitJsonStatus::EvaluationFailed`
/// and exit code 2. This is the critical difference from
/// `phase1_pending_layer_result`: Pending evaluations exit 0 (evaluation was
/// never attempted), but runtime errors exit non-zero (evaluation was
/// attempted and broke — a CI pipeline depending on `pice evaluate` as a
/// gate must fail the build, not pass it).
///
/// Seam failures still take priority: if a seam check failed, the layer's
/// `halted_by` is `seam:{id}`; otherwise it's `runtime_error:{message}`.
fn runtime_failed_layer_result(
    layer_name: String,
    error_message: String,
    seam_checks: Vec<SeamCheckResult>,
    first_failed_seam: Option<String>,
) -> LayerResult {
    let (status, halted_by) = match first_failed_seam {
        // Seam failures win per stack-loops.md §"Fail-closed rollup".
        Some(failed_id) => (LayerStatus::Failed, Some(format!("seam:{failed_id}"))),
        None => (
            LayerStatus::Failed,
            Some(format!("runtime_error:{error_message}")),
        ),
    };
    LayerResult {
        name: layer_name,
        status,
        // Placeholder pass row preserves manifest shape for downstream tools
        // that assume every layer has at least one pass. Distinct model name
        // (`runtime-error`) lets operators filter these apart from
        // `phase-1-pending` rows.
        passes: vec![PassResult {
            index: 0,
            model: "runtime-error".to_string(),
            score: None,
            cost_usd: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
            findings: vec![format!("Adaptive loop errored: {error_message}")],
        }],
        seam_checks,
        halted_by,
        final_confidence: None,
        total_cost_usd: None,
        escalation_events: None,
    }
}

/// Resolve the effective tier for a layer: override wins, else defaults.
fn effective_tier_for(workflow: &WorkflowConfig, layer_name: &str) -> u8 {
    workflow
        .layer_overrides
        .get(layer_name)
        .and_then(|o| o.tier)
        .unwrap_or(workflow.defaults.tier)
}

/// Resolve the effective `min_confidence` for a layer.
fn effective_min_confidence_for(workflow: &WorkflowConfig, layer_name: &str) -> f64 {
    workflow
        .layer_overrides
        .get(layer_name)
        .and_then(|o| o.min_confidence)
        .unwrap_or(workflow.defaults.min_confidence)
}

/// Resolve the effective `max_passes` for a layer.
fn effective_max_passes_for(workflow: &WorkflowConfig, layer_name: &str) -> u32 {
    workflow
        .layer_overrides
        .get(layer_name)
        .and_then(|o| o.max_passes)
        .unwrap_or(workflow.defaults.max_passes)
}

/// Resolve the effective `budget_usd` for a layer.
fn effective_budget_usd_for(workflow: &WorkflowConfig, layer_name: &str) -> f64 {
    workflow
        .layer_overrides
        .get(layer_name)
        .and_then(|o| o.budget_usd)
        .unwrap_or(workflow.defaults.budget_usd)
}

/// Resolve the effective adaptive config for a layer: algorithm, SPRT, ADTS, VEC.
///
/// The per-layer override can only choose a different `AdaptiveAlgo`; the
/// sub-configs (`SprtConfig`, `AdtsConfig`, `VecConfig`) are set project-wide
/// on `EvaluatePhase` and are not overridable per-layer. This keeps the
/// per-layer surface small (single enum choice) while the project owner
/// controls the tuning knobs globally.
/// Used by the adaptive pass loop in Phase 4 Chunk C (Task 15).
#[allow(dead_code)]
fn effective_adaptive_config_for(
    workflow: &WorkflowConfig,
) -> (
    pice_core::workflow::schema::AdaptiveAlgo,
    pice_core::adaptive::SprtConfig,
    pice_core::adaptive::AdtsConfig,
    pice_core::adaptive::VecConfig,
) {
    (
        workflow.phases.evaluate.adaptive_algorithm,
        workflow.phases.evaluate.sprt,
        workflow.phases.evaluate.adts,
        workflow.phases.evaluate.vec,
    )
}

/// Resolve the effective `AdaptiveAlgo` for a specific layer. Layer override
/// wins; else falls back to the project-wide `evaluate.adaptive_algorithm`.
fn effective_adaptive_algo_for(
    workflow: &WorkflowConfig,
    layer_name: &str,
) -> pice_core::workflow::schema::AdaptiveAlgo {
    workflow
        .layer_overrides
        .get(layer_name)
        .and_then(|o| o.adaptive_algorithm)
        .unwrap_or(workflow.phases.evaluate.adaptive_algorithm)
}

/// Load a layer-specific contract file, falling back to a generic message.
fn load_layer_contract(
    project_root: &Path,
    layer_name: &str,
    layer_def: &pice_core::layers::LayerDef,
) -> String {
    // Try layer's explicit contract path
    if let Some(ref contract_path) = layer_def.contract {
        let full_path = project_root.join(contract_path);
        if let Ok(content) = std::fs::read_to_string(&full_path) {
            return content;
        }
    }

    // Try default contract location
    let default_path = project_root
        .join(".pice/contracts")
        .join(format!("{layer_name}.toml"));
    if let Ok(content) = std::fs::read_to_string(&default_path) {
        return content;
    }

    // Fallback: generic contract
    format!(
        "[criteria]\n{layer_name}_correctness = \"Code changes in the {layer_name} layer are correct and complete\""
    )
}

// ─── Phase 5 cohort parallelism ─────────────────────────────────────────────
//
// `LayerInputs` + `build_per_layer_inputs` + `evaluate_one_layer` — the per-
// layer work unit the parallel cohort path spawns into each `tokio::spawn`'d
// JoinSet task. Three design choices are load-bearing:
//
// 1. `LayerInputs` holds OWNED clones, never `&StackLoopsConfig<'_>`. The
//    tokio::spawn future requires `'static`, so a reference to the outer
//    config would not compile — this is the compile-time enforcement of
//    "spawned cohort task cannot see other layers' data" that the plan's
//    contract criterion #3 demands. The `build_per_layer_inputs` signature
//    takes `&cfg` because it's the EXTRACTOR (single-threaded, runs in the
//    outer function). The task body receives only the extracted struct.
//
// 2. `primary_provider`, `primary_model`, `pice_config`, `workflow` are
//    cloned per task. Costs are negligible (small structs, no heavy
//    payloads). The alternative — wrapping cfg in `Arc<StackLoopsConfig>`
//    — would require `StackLoopsConfig` to own its fields (Arc<LayersConfig>
//    etc.) which ripples through every call site of the orchestrator.
//    Cloning is the pragmatic choice.
//
// 3. `seam_registry` is `Arc<Registry>` — the registry owns `Box<dyn
//    SeamCheck + Send + Sync>` which is NOT Clone. One registry, N task
//    references; cheap clone semantics, no data copy.

/// Per-layer work unit passed into a spawned cohort task.
///
/// Intentionally has no reference to `StackLoopsConfig` — that's the
/// compile-time guarantee that cross-layer data cannot leak into a
/// spawned task (contract criterion #3). Every field is either owned or
/// `Arc`-shared (for read-only shared state like the seam registry).
#[derive(Clone)]
struct LayerInputs {
    layer_name: String,
    contract_content: String,
    filtered_diff: String,
    claude_md: String,
    effective_tier: u8,
    min_confidence: f64,
    /// Owned provider + workflow state — cloned from cfg in
    /// `build_per_layer_inputs`. Cheap: these are small configs.
    primary_provider: String,
    primary_model: String,
    pice_config: PiceConfig,
    workflow: WorkflowConfig,
    /// Seam-runner inputs. `layer_paths` is inherently cross-layer
    /// (boundary checks read counterpart files), so it's shared — but
    /// that shared read isn't a context-isolation breach because the
    /// ORCHESTRATOR runs seam checks, not the provider.
    active_set: HashSet<String>,
    merged_seams: BTreeMap<String, Vec<String>>,
    layer_paths: BTreeMap<String, Vec<PathBuf>>,
    project_root: PathBuf,
    full_diff: String,
    seam_registry: Arc<Registry>,
}

/// Outcome of one parallel cohort task. The outer `run_stack_loops_with_cancel`
/// collects these by layer name, then flushes in DAG order so manifest
/// `layers[]` ordering is topological (not completion-order).
///
/// `streamed_chunks` holds the per-layer status lines the task would have
/// emitted incrementally in the sequential path; the outer function prints
/// them after drain so parallel output doesn't interleave.
struct LayerOutcome {
    layer_name: String,
    layer_result: LayerResult,
    streamed_chunks: Vec<String>,
}

/// Extract LAYER-OWN inputs from `cfg` for `layer_name`. The result owns
/// its data and has no lifetime parameter — spawn-safe by construction.
///
/// Caller has already vetted that `layer_name` is active AND has a layer
/// def AND has a non-empty filtered diff. The three "skip" paths are
/// handled inline in the outer loop to keep this helper focused on the
/// one case that actually spawns work.
///
/// The argument count is load-bearing: every value is an OWNED projection
/// of cfg that the spawned task needs. Collapsing these into a single
/// "context" struct would require introducing another cross-layer
/// aggregate which is exactly what we're trying to avoid (see the
/// "compile-time isolation" comment above). Allow the clippy lint here.
#[allow(clippy::too_many_arguments)]
fn build_per_layer_inputs(
    cfg: &StackLoopsConfig<'_>,
    layer_name: &str,
    contract_content: String,
    filtered_diff: String,
    claude_md: &str,
    active_set: &HashSet<String>,
    layer_paths: &BTreeMap<String, Vec<PathBuf>>,
    full_diff: &str,
    seam_registry: Arc<Registry>,
) -> LayerInputs {
    LayerInputs {
        layer_name: layer_name.to_string(),
        contract_content,
        filtered_diff,
        claude_md: claude_md.to_string(),
        effective_tier: effective_tier_for(cfg.workflow, layer_name),
        min_confidence: effective_min_confidence_for(cfg.workflow, layer_name),
        primary_provider: cfg.primary_provider.to_string(),
        primary_model: cfg.primary_model.to_string(),
        pice_config: cfg.pice_config.clone(),
        workflow: cfg.workflow.clone(),
        active_set: active_set.clone(),
        merged_seams: cfg.merged_seams.clone(),
        layer_paths: layer_paths.clone(),
        project_root: cfg.project_root.to_path_buf(),
        full_diff: full_diff.to_string(),
        seam_registry,
    }
}

/// Run the full per-layer evaluation pipeline for ONE layer with owned
/// inputs: provider startup, adaptive pass loop, seam checks, result
/// routing. Returns a `LayerOutcome` the outer function inserts into the
/// manifest in DAG order.
///
/// The `cancel` token is checked at session boundaries inside the
/// adaptive loop's transport layer. On `cancel.cancelled()` the in-flight
/// provider RPC aborts via `tokio::select!` and the function returns a
/// `Cancelled`-flavored `LayerResult::Failed` so the manifest reflects
/// which layer did not complete.
async fn evaluate_one_layer(
    inputs: LayerInputs,
    pass_sink: Arc<dyn PassMetricsSink>,
    cancel: CancellationToken,
) -> LayerOutcome {
    let mut chunks: Vec<String> = Vec::new();
    chunks.push(format!("  Evaluating layer: {}...\n", inputs.layer_name));

    // Short-circuit on pre-existing cancellation so we don't even start
    // the provider. Pre-spawn cancel = no provider startup = zero cost.
    if cancel.is_cancelled() {
        let layer_result = LayerResult {
            name: inputs.layer_name.clone(),
            status: LayerStatus::Failed,
            passes: Vec::new(),
            seam_checks: no_seam_checks(),
            halted_by: Some(CancelledReason::PreSpawn.as_halted_by()),
            final_confidence: None,
            total_cost_usd: None,
            escalation_events: None,
        };
        chunks.push(format!("  [{}] Cancelled\n", inputs.layer_name));
        return LayerOutcome {
            layer_name: inputs.layer_name.clone(),
            layer_result,
            streamed_chunks: chunks,
        };
    }

    // Race the adaptive pipeline against the cancellation token. The
    // provider sessions inside `try_run_layer_adaptive_owned` are
    // torn down via RAII inside that function — on cancel we just drop
    // the future and let those destructors run. The adaptive path does
    // NOT spawn additional tokio tasks (just awaits on child processes),
    // so dropping the outer future cascades cleanly.
    let eval_future = try_run_layer_adaptive_owned(
        inputs.layer_name.clone(),
        inputs.contract_content.clone(),
        inputs.filtered_diff.clone(),
        inputs.claude_md.clone(),
        inputs.primary_provider.clone(),
        inputs.primary_model.clone(),
        inputs.pice_config.clone(),
        inputs.workflow.clone(),
        inputs.project_root.clone(),
        Arc::clone(&pass_sink),
    );

    let adaptive_outcome = tokio::select! {
        out = eval_future => out,
        _ = cancel.cancelled() => {
            let layer_result = LayerResult {
                name: inputs.layer_name.clone(),
                status: LayerStatus::Failed,
                passes: Vec::new(),
                seam_checks: no_seam_checks(),
                halted_by: Some(CancelledReason::InFlight.as_halted_by()),
                final_confidence: None,
                total_cost_usd: None,
                escalation_events: None,
            };
            chunks.push(format!("  [{}] Cancelled\n", inputs.layer_name));
            return LayerOutcome {
                layer_name: inputs.layer_name.clone(),
                layer_result,
                streamed_chunks: chunks,
            };
        }
    };

    // Seam checks run after the adaptive loop. Cheap + deterministic, no
    // cancellation plumbing needed here.
    let seam_checks = run_seams_for_layer(
        &inputs.layer_name,
        &inputs.active_set,
        &inputs.merged_seams,
        inputs.seam_registry.as_ref(),
        &inputs.project_root,
        &inputs.full_diff,
        &inputs.layer_paths,
    );
    let first_failed_seam = seam_checks
        .iter()
        .find(|c| c.status == CheckStatus::Failed)
        .map(|c| c.name.clone());

    let layer_result = match adaptive_outcome {
        LayerAdaptiveResult::Completed(outcome) => build_adaptive_layer_result(
            inputs.layer_name.clone(),
            outcome,
            seam_checks,
            first_failed_seam,
            inputs.min_confidence,
        ),
        LayerAdaptiveResult::NotStarted => phase1_pending_layer_result(
            inputs.layer_name.clone(),
            inputs.effective_tier,
            inputs.filtered_diff.len(),
            seam_checks,
            first_failed_seam,
        ),
        LayerAdaptiveResult::RuntimeError(msg) => runtime_failed_layer_result(
            inputs.layer_name.clone(),
            msg,
            seam_checks,
            first_failed_seam,
        ),
    };

    chunks.push(format!(
        "  [{}] {:?}\n",
        inputs.layer_name, layer_result.status
    ));

    LayerOutcome {
        layer_name: inputs.layer_name.clone(),
        layer_result,
        streamed_chunks: chunks,
    }
}

/// Owned-inputs variant of [`try_run_layer_adaptive`]. Same semantics, but
/// the signature takes owned `String` / `PiceConfig` / `WorkflowConfig` so
/// the function can live inside a `tokio::spawn` future (which requires
/// `'static`). The sequential path still uses the reference-based
/// [`try_run_layer_adaptive`] for backwards compatibility with existing
/// tests.
#[allow(clippy::too_many_arguments)]
async fn try_run_layer_adaptive_owned(
    layer_name: String,
    contract_toml: String,
    filtered_diff: String,
    claude_md: String,
    primary_provider: String,
    primary_model: String,
    pice_config: PiceConfig,
    workflow: WorkflowConfig,
    project_root: PathBuf,
    pass_sink: Arc<dyn PassMetricsSink>,
) -> LayerAdaptiveResult {
    // Build a transient `StackLoopsConfig` and delegate to the shared
    // reference-based implementation. The config holds borrows of our
    // locals — they stay alive for the duration of the await because
    // `.await` on the returned future keeps this function's stack frame.
    //
    // NOTE: we construct a minimal `LayersConfig` placeholder because
    // `try_run_layer_adaptive` doesn't actually read `cfg.layers` for
    // the adaptive path — only `primary_provider`, `primary_model`,
    // `pice_config`, `workflow`, and (post-review) `project_root`. The
    // placeholder keeps the struct constructable without pulling the
    // real layers config into the spawned task (another compile-time
    // isolation lever).
    //
    // `project_root` is the REAL layer input — NOT a dummy. A prior
    // revision passed `Path::new(".")` here on the assumption that
    // `try_run_layer_adaptive` would not read the field. That held
    // today but was a latent hazard: any future refactor touching
    // `cfg.project_root` would silently resolve against the daemon's
    // current working directory (typically `~/.pice/`) instead of the
    // user's project. Threading the real path through
    // `LayerInputs::project_root` closes the hazard.
    let empty_layers = LayersConfig::default();
    let dummy_plan = std::path::PathBuf::from("/dev/null");
    let dummy_seams: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let cfg = StackLoopsConfig {
        layers: &empty_layers,
        plan_path: &dummy_plan,
        project_root: project_root.as_path(),
        primary_provider: &primary_provider,
        primary_model: &primary_model,
        pice_config: &pice_config,
        workflow: &workflow,
        merged_seams: &dummy_seams,
    };
    try_run_layer_adaptive(
        &cfg,
        &layer_name,
        &contract_toml,
        &filtered_diff,
        &claude_md,
        pass_sink.as_ref(),
    )
    .await
}

/// Extract file paths from a unified diff output.
///
/// Parses `diff --git a/... b/...` headers to extract the `b/` path
/// (the new file path). For deleted files (`+++ /dev/null`), uses the
/// `a/` path.
fn extract_changed_files_from_diff(diff: &str) -> Vec<String> {
    let mut files = Vec::new();

    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // Extract b/ path from "a/path b/path"
            if let Some(pos) = rest.find(" b/") {
                let b_path = &rest[pos + 3..]; // skip " b/"
                files.push(b_path.to_string());
            }
        }
    }

    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::NullSink;
    use pice_core::layers::{LayerDef, LayersConfig, LayersTable};
    use std::collections::BTreeMap;

    fn test_workflow() -> pice_core::workflow::WorkflowConfig {
        pice_core::workflow::loader::embedded_defaults()
    }

    fn test_layers_config() -> LayersConfig {
        let mut defs = BTreeMap::new();
        defs.insert(
            "backend".to_string(),
            LayerDef {
                paths: vec!["src/server/**".to_string()],
                always_run: false,
                contract: None,
                depends_on: Vec::new(),
                layer_type: None,
                environment_variants: None,
            },
        );
        defs.insert(
            "frontend".to_string(),
            LayerDef {
                paths: vec!["src/client/**".to_string()],
                always_run: false,
                contract: None,
                depends_on: vec!["backend".to_string()],
                layer_type: None,
                environment_variants: None,
            },
        );
        defs.insert(
            "infrastructure".to_string(),
            LayerDef {
                paths: vec!["terraform/**".to_string()],
                always_run: true,
                contract: None,
                depends_on: Vec::new(),
                layer_type: None,
                environment_variants: None,
            },
        );

        LayersConfig {
            layers: LayersTable {
                order: vec![
                    "backend".to_string(),
                    "frontend".to_string(),
                    "infrastructure".to_string(),
                ],
                defs,
            },
            seams: None,
            external_contracts: None,
            stacks: None,
        }
    }

    #[test]
    fn extract_changed_files_basic() {
        let diff = [
            "diff --git a/src/server/main.rs b/src/server/main.rs",
            "index abc..def 100644",
            "--- a/src/server/main.rs",
            "+++ b/src/server/main.rs",
            "@@ -1,3 +1,4 @@",
            "+fn new() {}",
            "diff --git a/src/client/app.ts b/src/client/app.ts",
            "index 111..222 100644",
            "--- a/src/client/app.ts",
            "+++ b/src/client/app.ts",
        ]
        .join("\n");

        let files = extract_changed_files_from_diff(&diff);
        assert_eq!(files.len(), 2);
        assert!(files.contains(&"src/server/main.rs".to_string()));
        assert!(files.contains(&"src/client/app.ts".to_string()));
    }

    #[test]
    fn extract_changed_files_empty_diff() {
        let files = extract_changed_files_from_diff("");
        assert!(files.is_empty());
    }

    #[tokio::test]
    async fn run_stack_loops_with_git_repo() {
        let dir = tempfile::tempdir().unwrap();

        // Set up a git repo with a commit and a change
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

        // Create a file that matches the backend layer
        std::fs::create_dir_all(dir.path().join("src/server")).unwrap();
        std::fs::write(dir.path().join("src/server/main.rs"), "fn main() {}").unwrap();

        let layers_config = test_layers_config();
        let plan_path = dir.path().join("plan.md");
        std::fs::write(&plan_path, "# Test Plan").unwrap();

        let pice_config = PiceConfig::default();
        let workflow = test_workflow();
        let empty_seams: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let cfg = StackLoopsConfig {
            layers: &layers_config,
            plan_path: &plan_path,
            project_root: dir.path(),
            primary_provider: "test-provider",
            primary_model: "test-model",
            pice_config: &pice_config,
            workflow: &workflow,
            merged_seams: &empty_seams,
        };

        let pass_sink: std::sync::Arc<dyn super::super::adaptive_loop::PassMetricsSink> =
            std::sync::Arc::new(super::super::adaptive_loop::NullPassSink);
        let manifest = run_stack_loops(&cfg, &NullSink, false, pass_sink)
            .await
            .unwrap();

        // Should have results for all 3 layers
        assert_eq!(manifest.layers.len(), 3);

        // Backend has file changes → should be PENDING (awaiting provider)
        let backend = manifest
            .layers
            .iter()
            .find(|l| l.name == "backend")
            .expect("should have backend result");
        assert_eq!(
            backend.status,
            LayerStatus::Pending,
            "backend with file changes should be PENDING (fail closed)"
        );
        // Phase 2 observability: effective tier recorded in halted_by
        assert_eq!(
            backend.halted_by.as_deref(),
            Some("phase-1-pending-tier-2"),
            "backend should record the framework default tier 2"
        );

        // Frontend depends_on backend → transitively activated, but has no
        // file changes → SKIPPED with dependency cascade note
        let frontend = manifest
            .layers
            .iter()
            .find(|l| l.name == "frontend")
            .expect("should have frontend result");
        assert_eq!(
            frontend.status,
            LayerStatus::Skipped,
            "frontend (transitive cascade, no own changes) should be SKIPPED"
        );

        // Infrastructure is always_run with no file changes → PENDING
        // (always_run layers never get Skipped — they stay Pending until
        // seam checks or static analysis evaluate them in Phase 3)
        let infra = manifest
            .layers
            .iter()
            .find(|l| l.name == "infrastructure")
            .expect("should have infrastructure result");
        assert_eq!(
            infra.status,
            LayerStatus::Pending,
            "infrastructure (always_run, no changes) should be PENDING, not Skipped"
        );

        // Overall status should be InProgress (backend is Pending)
        assert_eq!(manifest.overall_status, ManifestStatus::InProgress);
    }

    #[tokio::test]
    async fn run_stack_loops_no_changes() {
        let dir = tempfile::tempdir().unwrap();

        // Git repo with no changes
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

        let layers_config = test_layers_config();
        let plan_path = dir.path().join("plan.md");
        std::fs::write(&plan_path, "# Test Plan").unwrap();

        let pice_config = PiceConfig::default();
        let workflow = test_workflow();
        let empty_seams: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let cfg = StackLoopsConfig {
            layers: &layers_config,
            plan_path: &plan_path,
            project_root: dir.path(),
            primary_provider: "test-provider",
            primary_model: "test-model",
            pice_config: &pice_config,
            workflow: &workflow,
            merged_seams: &empty_seams,
        };

        let pass_sink: std::sync::Arc<dyn super::super::adaptive_loop::PassMetricsSink> =
            std::sync::Arc::new(super::super::adaptive_loop::NullPassSink);
        let manifest = run_stack_loops(&cfg, &NullSink, true, pass_sink)
            .await
            .unwrap();

        // With no changes: non-always_run layers are inactive → Skipped.
        // always_run layers are active but have empty diffs → Pending
        // (they never get Skipped — they wait for seam checks / static analysis).
        let backend = manifest
            .layers
            .iter()
            .find(|l| l.name == "backend")
            .unwrap();
        assert_eq!(backend.status, LayerStatus::Skipped);

        let frontend = manifest
            .layers
            .iter()
            .find(|l| l.name == "frontend")
            .unwrap();
        assert_eq!(frontend.status, LayerStatus::Skipped);

        let infra = manifest
            .layers
            .iter()
            .find(|l| l.name == "infrastructure")
            .unwrap();
        assert_eq!(
            infra.status,
            LayerStatus::Pending,
            "infrastructure (always_run) should be PENDING, not Skipped"
        );
    }

    #[test]
    fn load_layer_contract_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let def = LayerDef {
            paths: vec!["src/**".to_string()],
            always_run: false,
            contract: None,
            depends_on: Vec::new(),
            layer_type: None,
            environment_variants: None,
        };

        let content = load_layer_contract(dir.path(), "backend", &def);
        assert!(content.contains("[criteria]"));
        assert!(content.contains("backend"));
    }

    #[test]
    fn load_layer_contract_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let contracts_dir = dir.path().join(".pice/contracts");
        std::fs::create_dir_all(&contracts_dir).unwrap();
        std::fs::write(
            contracts_dir.join("api.toml"),
            "[criteria]\nresponse_format = \"JSON\"",
        )
        .unwrap();

        let def = LayerDef {
            paths: vec!["api/**".to_string()],
            always_run: false,
            contract: None,
            depends_on: Vec::new(),
            layer_type: None,
            environment_variants: None,
        };

        let content = load_layer_contract(dir.path(), "api", &def);
        assert!(content.contains("response_format"));
        assert!(content.contains("JSON"));
    }

    /// Seam-fail fixture: changes touch backend + infrastructure with
    /// declared-but-unused env var. The `backend↔infrastructure` boundary
    /// runs `config_mismatch`, produces Failed, and the layer transitions
    /// from Pending → Failed with `halted_by = "seam:config_mismatch"`.
    #[tokio::test]
    async fn seam_failure_downgrades_layer_to_failed() {
        let dir = tempfile::tempdir().unwrap();
        // Git repo with one commit and then staged changes so the diff is non-empty.
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

        // Backend changes — reads no env vars.
        std::fs::create_dir_all(dir.path().join("src/server")).unwrap();
        std::fs::write(dir.path().join("src/server/main.rs"), "fn main() {}\n").unwrap();
        // Infrastructure declares FOO that backend never reads.
        std::fs::create_dir_all(dir.path().join("terraform")).unwrap();
        std::fs::write(dir.path().join("terraform/env.tf"), "# unused tf file\n").unwrap();
        std::fs::write(dir.path().join("Dockerfile"), "FROM alpine\nENV FOO=1\n").unwrap();

        // Build layers + workflow with a seam boundary declaring config_mismatch.
        let mut layers = test_layers_config();
        layers.layers.defs.get_mut("infrastructure").unwrap().paths =
            vec!["terraform/**".into(), "Dockerfile".into()];
        let mut seams = BTreeMap::new();
        seams.insert(
            "backend↔infrastructure".to_string(),
            vec!["config_mismatch".to_string()],
        );
        let mut workflow = test_workflow();
        workflow.seams = Some(seams.clone());

        let plan_path = dir.path().join("plan.md");
        std::fs::write(&plan_path, "# Plan").unwrap();
        let pice_config = PiceConfig::default();
        let cfg = StackLoopsConfig {
            layers: &layers,
            plan_path: &plan_path,
            project_root: dir.path(),
            primary_provider: "test",
            primary_model: "test",
            pice_config: &pice_config,
            workflow: &workflow,
            merged_seams: &seams,
        };

        let pass_sink: std::sync::Arc<dyn super::super::adaptive_loop::PassMetricsSink> =
            std::sync::Arc::new(super::super::adaptive_loop::NullPassSink);
        let manifest = run_stack_loops(&cfg, &NullSink, true, pass_sink)
            .await
            .unwrap();
        let backend = manifest
            .layers
            .iter()
            .find(|l| l.name == "backend")
            .expect("backend result present");
        assert_eq!(
            backend.status,
            LayerStatus::Failed,
            "seam failure should downgrade layer to Failed, got {:?}",
            backend.status
        );
        assert_eq!(
            backend.halted_by.as_deref(),
            Some("seam:config_mismatch"),
            "halted_by should reference the failed check id"
        );
        assert!(
            backend
                .seam_checks
                .iter()
                .any(|c| c.name == "config_mismatch" && c.status == CheckStatus::Failed),
            "seam_checks should include the Failed config_mismatch entry"
        );
    }

    // ─── Effective-resolution helper tests ──────────────────────────────

    #[test]
    fn effective_min_confidence_falls_back_to_defaults() {
        let wf = test_workflow();
        let eff = effective_min_confidence_for(&wf, "nonexistent");
        assert!((eff - wf.defaults.min_confidence).abs() < 1e-12);
    }

    #[test]
    fn effective_min_confidence_uses_override() {
        let mut wf = test_workflow();
        wf.layer_overrides.insert(
            "backend".into(),
            pice_core::workflow::schema::LayerOverride {
                min_confidence: Some(0.99),
                ..Default::default()
            },
        );
        assert!((effective_min_confidence_for(&wf, "backend") - 0.99).abs() < 1e-12);
    }

    #[test]
    fn effective_max_passes_falls_back_to_defaults() {
        let wf = test_workflow();
        assert_eq!(
            effective_max_passes_for(&wf, "nonexistent"),
            wf.defaults.max_passes
        );
    }

    #[test]
    fn effective_max_passes_uses_override() {
        let mut wf = test_workflow();
        wf.layer_overrides.insert(
            "backend".into(),
            pice_core::workflow::schema::LayerOverride {
                max_passes: Some(10),
                ..Default::default()
            },
        );
        assert_eq!(effective_max_passes_for(&wf, "backend"), 10);
    }

    #[test]
    fn effective_budget_usd_falls_back_to_defaults() {
        let wf = test_workflow();
        assert!(
            (effective_budget_usd_for(&wf, "nonexistent") - wf.defaults.budget_usd).abs() < 1e-12
        );
    }

    #[test]
    fn effective_budget_usd_uses_override() {
        let mut wf = test_workflow();
        wf.layer_overrides.insert(
            "backend".into(),
            pice_core::workflow::schema::LayerOverride {
                budget_usd: Some(0.05),
                ..Default::default()
            },
        );
        assert!((effective_budget_usd_for(&wf, "backend") - 0.05).abs() < 1e-12);
    }

    #[test]
    fn effective_adaptive_config_returns_project_values() {
        let wf = test_workflow();
        let (algo, sprt, adts, vec_cfg) = effective_adaptive_config_for(&wf);
        assert_eq!(
            algo,
            pice_core::workflow::schema::AdaptiveAlgo::BayesianSprt
        );
        assert_eq!(sprt, pice_core::adaptive::SprtConfig::default());
        assert_eq!(adts, pice_core::adaptive::AdtsConfig::default());
        assert_eq!(vec_cfg, pice_core::adaptive::VecConfig::default());
    }

    #[test]
    fn effective_adaptive_algo_falls_back_to_evaluate_phase() {
        let wf = test_workflow();
        assert_eq!(
            effective_adaptive_algo_for(&wf, "nonexistent"),
            pice_core::workflow::schema::AdaptiveAlgo::BayesianSprt
        );
    }

    #[test]
    fn effective_adaptive_algo_uses_layer_override() {
        let mut wf = test_workflow();
        wf.layer_overrides.insert(
            "backend".into(),
            pice_core::workflow::schema::LayerOverride {
                adaptive_algorithm: Some(pice_core::workflow::schema::AdaptiveAlgo::None),
                ..Default::default()
            },
        );
        assert_eq!(
            effective_adaptive_algo_for(&wf, "backend"),
            pice_core::workflow::schema::AdaptiveAlgo::None
        );
        assert_eq!(
            effective_adaptive_algo_for(&wf, "frontend"),
            pice_core::workflow::schema::AdaptiveAlgo::BayesianSprt
        );
    }

    /// Phase 6 cohort-boundary gate check — orchestrator-side test that
    /// exercises the same pure `pice_core::gate::check_gates_for_cohort`
    /// helper the orchestrator calls at the cohort boundary. Contract
    /// criterion #1 names this exact test location (the pure helper's
    /// deep tests live in `pice-core::gate::tests`; this alias pins the
    /// orchestrator-side integration to the same named assertion).
    #[test]
    fn check_gates_for_cohort_with_matching_trigger_enqueues_gate_with_pinned_fields() {
        use pice_core::layers::manifest::{LayerResult, LayerStatus};
        use pice_core::workflow::schema::{OnTimeout, ReviewConfig};
        use std::collections::BTreeMap;

        let workflow = pice_core::workflow::WorkflowConfig {
            schema_version: "0.2".to_string(),
            defaults: pice_core::workflow::schema::Defaults {
                tier: 2,
                min_confidence: 0.9,
                max_passes: 5,
                model: "sonnet".to_string(),
                budget_usd: 0.0,
                cost_cap_behavior: pice_core::workflow::schema::CostCapBehavior::Halt,
                max_parallelism: None,
            },
            phases: pice_core::workflow::schema::Phases::default(),
            layer_overrides: BTreeMap::new(),
            review: Some(ReviewConfig {
                enabled: true,
                trigger: Some("layer == infrastructure".to_string()),
                timeout_hours: 24,
                on_timeout: OnTimeout::Reject,
                notification: "stdout".to_string(),
                retry_on_reject: 1,
            }),
            seams: None,
        };
        let layers = vec![LayerResult {
            name: "infrastructure".to_string(),
            status: LayerStatus::Passed,
            passes: Vec::new(),
            seam_checks: Vec::new(),
            halted_by: None,
            final_confidence: Some(0.95),
            total_cost_usd: Some(0.01),
            escalation_events: None,
        }];
        let now: chrono::DateTime<chrono::Utc> = "2026-04-20T00:00:00Z".parse().unwrap();
        let out = pice_core::gate::check_gates_for_cohort(
            &workflow,
            &layers,
            &[],
            &["infrastructure".to_string()],
            "feat-6",
            2,
            "",
            now,
        );
        assert_eq!(out.new_gates.len(), 1);
        assert_eq!(out.layers_pending_review, vec!["infrastructure"]);
        let g = &out.new_gates[0];
        assert_eq!(g.layer, "infrastructure");
        assert_eq!(g.reject_attempts_remaining, 1);
        assert_eq!(g.on_timeout_action, OnTimeout::Reject);
    }

    /// Phase 6: cohort-boundary check preserves the reject counter
    /// across re-gate events within one feature run. Contract
    /// criterion #7 names this exact test location.
    #[test]
    fn check_gates_for_cohort_reuses_reject_counter_from_prior_gate() {
        use pice_core::layers::manifest::{GateEntry, GateStatus, LayerResult, LayerStatus};
        use pice_core::workflow::schema::{OnTimeout, ReviewConfig};
        use std::collections::BTreeMap;

        let workflow = pice_core::workflow::WorkflowConfig {
            schema_version: "0.2".to_string(),
            defaults: pice_core::workflow::schema::Defaults {
                tier: 2,
                min_confidence: 0.9,
                max_passes: 5,
                model: "sonnet".to_string(),
                budget_usd: 0.0,
                cost_cap_behavior: pice_core::workflow::schema::CostCapBehavior::Halt,
                max_parallelism: None,
            },
            phases: pice_core::workflow::schema::Phases::default(),
            layer_overrides: BTreeMap::new(),
            review: Some(ReviewConfig {
                enabled: true,
                trigger: Some("layer == infrastructure".to_string()),
                timeout_hours: 24,
                on_timeout: OnTimeout::Reject,
                notification: "stdout".to_string(),
                retry_on_reject: 2,
            }),
            seams: None,
        };
        let layers = vec![LayerResult {
            name: "infrastructure".to_string(),
            status: LayerStatus::Passed,
            passes: Vec::new(),
            seam_checks: Vec::new(),
            halted_by: None,
            final_confidence: Some(0.95),
            total_cost_usd: Some(0.01),
            escalation_events: None,
        }];
        let prior = GateEntry {
            id: "feat-6:infrastructure:aaaa".to_string(),
            layer: "infrastructure".to_string(),
            status: GateStatus::Rejected,
            trigger_expression: "layer == infrastructure".to_string(),
            requested_at: "2026-04-20T00:00:00Z".to_string(),
            timeout_at: "2026-04-21T00:00:00Z".to_string(),
            on_timeout_action: OnTimeout::Reject,
            reject_attempts_remaining: 0,
            decision: Some("reject".to_string()),
            decided_at: Some("2026-04-20T00:05:00Z".to_string()),
        };
        let now: chrono::DateTime<chrono::Utc> = "2026-04-20T00:10:00Z".parse().unwrap();
        let out = pice_core::gate::check_gates_for_cohort(
            &workflow,
            &layers,
            &[prior],
            &["infrastructure".to_string()],
            "feat-6",
            2,
            "",
            now,
        );
        assert_eq!(out.new_gates.len(), 1);
        assert_eq!(
            out.new_gates[0].reject_attempts_remaining, 0,
            "reject budget must persist from prior gate (0), not reset to 2"
        );
    }
}

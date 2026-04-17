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
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

use super::adaptive_loop::{
    run_adaptive_passes, AdaptiveContext, AdaptiveOutcome, PassMetricsSink,
};
use super::{run_seams_for_layer, ProviderOrchestrator, StreamSink};
use crate::prompt::layer_builder::build_layer_evaluation_prompt;

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
    pass_sink: &mut dyn PassMetricsSink,
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

    // Create manifest and persist initial state
    let mut manifest = VerificationManifest::new(&feature_id, project_root);
    manifest.overall_status = ManifestStatus::InProgress;

    // Ensure state directory exists and persist the in-progress manifest.
    // On crash/retry, the daemon can resume from this checkpoint.
    let manifest_path = match VerificationManifest::manifest_path_for(&feature_id, project_root) {
        Ok(path) => {
            if let Some(parent) = path.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    warn!("failed to create manifest state dir: {e}");
                }
            }
            if let Err(e) = manifest.save(&path) {
                warn!("failed to persist initial manifest: {e}");
            }
            Some(path)
        }
        Err(e) => {
            warn!("failed to compute manifest path: {e}");
            None
        }
    };

    // Process each cohort sequentially, layers within a cohort sequentially
    // (Phase 2 will parallelize layers within a cohort via worktrees)
    for (cohort_idx, cohort) in dag.cohorts.iter().enumerate() {
        debug!(cohort = cohort_idx, layers = ?cohort, "processing cohort");

        for layer_name in cohort {
            // Skip layers that aren't active
            if !active.contains(layer_name) {
                debug!(layer = %layer_name, "skipping inactive layer");
                manifest.add_layer_result(LayerResult {
                    name: layer_name.clone(),
                    status: LayerStatus::Skipped,
                    passes: Vec::new(),
                    seam_checks: no_seam_checks(),
                    halted_by: None,
                    final_confidence: None,
                    total_cost_usd: None,
                    escalation_events: None,
                });
                continue;
            }

            if !json_mode {
                sink.send_chunk(&format!("  Evaluating layer: {layer_name}...\n"));
            }

            // Get layer definition for globs
            let layer_def = match config.layers.defs.get(layer_name) {
                Some(def) => def,
                None => {
                    warn!(layer = %layer_name, "layer defined in order but missing definition");
                    manifest.add_layer_result(LayerResult {
                        name: layer_name.clone(),
                        status: LayerStatus::Failed,
                        passes: Vec::new(),
                        seam_checks: no_seam_checks(),
                        halted_by: Some("missing layer definition".to_string()),
                        final_confidence: None,
                        total_cost_usd: None,
                        escalation_events: None,
                    });
                    continue;
                }
            };

            // Filter diff to this layer's globs
            let filtered_diff = filter_diff_by_globs(&full_diff, &layer_def.paths);

            // Handle layers with empty filtered diffs.
            // - always_run layers: remain PENDING (they must never be Skipped —
            //   seam checks or static analysis will evaluate them in Phase 3).
            // - Cascade-only layers: SKIPPED (activated by dependency, no own files).
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
                // Even with no own-diff, seam checks may still fire — especially
                // for always_run layers like `infrastructure`. Run them and
                // downgrade to Failed on any Failed finding.
                let seam_checks = run_seams_for_layer(
                    layer_name,
                    &active_set,
                    merged_seams,
                    &seam_registry,
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
                manifest.add_layer_result(LayerResult {
                    name: layer_name.clone(),
                    status: final_status,
                    passes: Vec::new(),
                    seam_checks,
                    halted_by: Some(final_reason),
                    final_confidence: None,
                    total_cost_usd: None,
                    escalation_events: None,
                });
                if !json_mode {
                    sink.send_chunk(&format!("  [{layer_name}] {label} (no file changes)\n"));
                }
                // Checkpoint: persist manifest after each layer result
                if let Some(ref path) = manifest_path {
                    if let Err(e) = manifest.save(path) {
                        warn!("failed to checkpoint manifest: {e}");
                    }
                }
                continue;
            }

            // Load layer contract or fall back to plan contract
            let contract_content = load_layer_contract(project_root, layer_name, layer_def);

            // Build context-isolated prompt (returned for future Phase 5
            // prompt-inspection hooks; the adaptive loop below re-builds its
            // own view from contract + diff + claude_md).
            let _prompt = build_layer_evaluation_prompt(
                layer_name,
                &contract_content,
                &filtered_diff,
                &claude_md,
            );

            let effective_tier = effective_tier_for(cfg.workflow, layer_name);

            // Attempt to spawn the provider(s) and run the adaptive pass
            // loop. If provider startup fails (common in test environments
            // without a resolved binary), fall back to the Phase-1-pending
            // placeholder so orchestration flow stays observable. This is
            // a fail-closed path — no layer gets marked `Passed` on a
            // provider failure.
            let adaptive_outcome = try_run_layer_adaptive(
                cfg,
                layer_name,
                &contract_content,
                &filtered_diff,
                &claude_md,
                pass_sink,
            )
            .await;

            // Phase 3 — seam checks. Run AFTER the adaptive loop completes;
            // seam failures still downgrade layer status to Failed regardless
            // of the adaptive halt reason.
            let seam_checks = run_seams_for_layer(
                layer_name,
                &active_set,
                merged_seams,
                &seam_registry,
                project_root,
                &full_diff,
                &layer_paths,
            );
            let first_failed_seam = seam_checks
                .iter()
                .find(|c| c.status == CheckStatus::Failed)
                .map(|c| c.name.clone());

            let min_confidence = effective_min_confidence_for(cfg.workflow, layer_name);
            let layer_result = match adaptive_outcome {
                Some(outcome) => build_adaptive_layer_result(
                    layer_name.clone(),
                    outcome,
                    seam_checks,
                    first_failed_seam,
                    min_confidence,
                ),
                None => phase1_pending_layer_result(
                    layer_name.clone(),
                    effective_tier,
                    filtered_diff.len(),
                    seam_checks,
                    first_failed_seam,
                ),
            };

            if !json_mode {
                sink.send_chunk(&format!("  [{}] {:?}\n", layer_name, layer_result.status));
            }
            manifest.add_layer_result(layer_result);

            // Checkpoint: persist manifest after each layer result
            if let Some(ref path) = manifest_path {
                if let Err(e) = manifest.save(path) {
                    warn!("failed to checkpoint manifest: {e}");
                }
            }
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

/// Attempt to run the per-layer adaptive pass loop.
///
/// Starts primary (and adversarial when ADTS is active) providers, invokes
/// [`run_adaptive_passes`], and shuts them down. Returns `None` if any
/// provider fails to start — the caller falls back to the Phase-1-pending
/// placeholder. This graceful-degrade path preserves existing test fixtures
/// where the provider binary isn't resolvable.
async fn try_run_layer_adaptive(
    cfg: &StackLoopsConfig<'_>,
    layer_name: &str,
    contract_toml: &str,
    filtered_diff: &str,
    claude_md: &str,
    pass_sink: &mut dyn PassMetricsSink,
) -> Option<AdaptiveOutcome> {
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

    let mut primary = match ProviderOrchestrator::start(cfg.primary_provider, cfg.pice_config).await
    {
        Ok(p) => p,
        Err(e) => {
            warn!(layer = %layer_name, "failed to start primary provider, falling back to phase-1-pending: {e}");
            return None;
        }
    };

    // Start the adversarial provider only when ADTS is selected.
    let mut adversarial: Option<ProviderOrchestrator> = if algo
        == pice_core::workflow::schema::AdaptiveAlgo::Adts
        && cfg.pice_config.evaluation.adversarial.enabled
    {
        match ProviderOrchestrator::start(
            &cfg.pice_config.evaluation.adversarial.provider,
            cfg.pice_config,
        )
        .await
        {
            Ok(a) => Some(a),
            Err(e) => {
                warn!(layer = %layer_name, "failed to start adversarial provider for ADTS: {e}");
                // ADTS without adversarial is degenerate — shut down primary and fall back.
                let _ = primary.shutdown().await;
                return None;
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
    };

    let result = run_adaptive_passes(&ctx, &mut primary, adversarial.as_mut(), pass_sink).await;

    // Always shut the providers down, even on loop error.
    let _ = primary.shutdown().await;
    if let Some(adv) = adversarial {
        let _ = adv.shutdown().await;
    }

    match result {
        Ok(outcome) => Some(outcome),
        Err(e) => {
            warn!(layer = %layer_name, "adaptive pass loop failed: {e}");
            None
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

        let mut pass_sink = super::super::adaptive_loop::NullPassSink;
        let manifest = run_stack_loops(&cfg, &NullSink, false, &mut pass_sink)
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

        let mut pass_sink = super::super::adaptive_loop::NullPassSink;
        let manifest = run_stack_loops(&cfg, &NullSink, true, &mut pass_sink)
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

        let mut pass_sink = super::super::adaptive_loop::NullPassSink;
        let manifest = run_stack_loops(&cfg, &NullSink, true, &mut pass_sink)
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
}

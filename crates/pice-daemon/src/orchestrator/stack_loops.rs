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
use pice_core::layers::filter::filter_diff_by_globs;
use pice_core::layers::manifest::{
    LayerResult, LayerStatus, ManifestStatus, PassResult, VerificationManifest,
};
use pice_core::layers::{active_layers, LayersConfig};
use pice_core::prompt::helpers::{get_git_diff, read_claude_md};
use pice_core::workflow::WorkflowConfig;
use std::path::Path;
use tracing::{debug, info, warn};

use super::StreamSink;
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
                    seam_checks: Vec::new(),
                    halted_by: None,
                    final_confidence: None,
                    total_cost_usd: None,
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
                        seam_checks: Vec::new(),
                        halted_by: Some("missing layer definition".to_string()),
                        final_confidence: None,
                        total_cost_usd: None,
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
                manifest.add_layer_result(LayerResult {
                    name: layer_name.clone(),
                    status,
                    passes: Vec::new(),
                    seam_checks: Vec::new(),
                    halted_by: Some(reason),
                    final_confidence: None,
                    total_cost_usd: None,
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

            // Build context-isolated prompt
            let _prompt = build_layer_evaluation_prompt(
                layer_name,
                &contract_content,
                &filtered_diff,
                &claude_md,
            );

            // Phase 1: Record as PENDING — no provider evaluation yet.
            // Full provider evaluation is wired in Phase 2. We fail closed:
            // layers are NOT marked as PASSED without real evaluation.
            //
            // Phase 2 observability: the effective tier (from workflow
            // `layer_overrides.{layer}.tier` with fallback to `defaults.tier`)
            // is recorded in `halted_by` so workflow.yaml changes drive
            // observable manifest output (PRDv2 Phase 2 validation criterion).
            let effective_tier = effective_tier_for(cfg.workflow, layer_name);
            let timestamp = chrono::Utc::now().to_rfc3339();

            let layer_result = LayerResult {
                name: layer_name.clone(),
                status: LayerStatus::Pending,
                passes: vec![PassResult {
                    index: 0,
                    model: "phase-1-pending".to_string(),
                    score: None,
                    cost_usd: None,
                    timestamp,
                    findings: vec![format!(
                        "Awaiting provider evaluation — {} bytes of filtered diff prepared",
                        filtered_diff.len()
                    )],
                }],
                seam_checks: Vec::new(),
                halted_by: Some(format!("phase-1-pending-tier-{effective_tier}")),
                final_confidence: None,
                total_cost_usd: None,
            };

            manifest.add_layer_result(layer_result);

            if !json_mode {
                sink.send_chunk(&format!(
                    "  [{layer_name}] PENDING (provider evaluation deferred)\n"
                ));
            }

            // Checkpoint: persist manifest after each layer result
            if let Some(ref path) = manifest_path {
                if let Err(e) = manifest.save(path) {
                    warn!("failed to checkpoint manifest: {e}");
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

/// Resolve the effective tier for a layer: override wins, else defaults.
fn effective_tier_for(workflow: &WorkflowConfig, layer_name: &str) -> u8 {
    workflow
        .layer_overrides
        .get(layer_name)
        .and_then(|o| o.tier)
        .unwrap_or(workflow.defaults.tier)
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
        let cfg = StackLoopsConfig {
            layers: &layers_config,
            plan_path: &plan_path,
            project_root: dir.path(),
            primary_provider: "test-provider",
            primary_model: "test-model",
            pice_config: &pice_config,
            workflow: &workflow,
        };

        let manifest = run_stack_loops(&cfg, &NullSink, false).await.unwrap();

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
        let cfg = StackLoopsConfig {
            layers: &layers_config,
            plan_path: &plan_path,
            project_root: dir.path(),
            primary_provider: "test-provider",
            primary_model: "test-model",
            pice_config: &pice_config,
            workflow: &workflow,
        };

        let manifest = run_stack_loops(&cfg, &NullSink, true).await.unwrap();

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
}

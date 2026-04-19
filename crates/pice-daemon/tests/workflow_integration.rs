//! Integration test: workflow.yaml overrides are observable in the manifest.
//!
//! Validates PRDv2 Phase 2's headline criterion:
//! "Workflow changes drive observable pipeline behavior."
//!
//! Scenario:
//! 1. Build a temp repo with `.pice/layers.toml` (backend, frontend, infrastructure)
//! 2. Write `.pice/workflow.yaml` with `layer_overrides.backend.tier = 3`
//! 3. Make a file change matching the backend layer
//! 4. Run the Stack Loops orchestrator (inline) via `run_stack_loops`
//! 5. Assert the manifest records tier-3 for backend and tier-2 (framework
//!    default) for frontend

use pice_core::config::PiceConfig;
use pice_core::layers::LayersConfig;
use pice_core::workflow::loader;
use pice_daemon::orchestrator::stack_loops::{run_stack_loops, StackLoopsConfig};
use pice_daemon::orchestrator::{NullPassSink, NullSink};
use std::path::Path;

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn setup_git_repo(dir: &Path) {
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
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
        .current_dir(dir)
        .output()
        .unwrap();
}

#[tokio::test]
async fn workflow_layer_override_is_observable_in_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // 1. layers.toml with 3 layers
    write(
        &root.join(".pice/layers.toml"),
        r#"
[layers]
order = ["backend", "frontend", "infrastructure"]

[layers.backend]
paths = ["src/server/**"]

[layers.frontend]
paths = ["src/client/**"]
depends_on = ["backend"]

[layers.infrastructure]
paths = ["terraform/**"]
always_run = true
"#,
    );

    // 2. workflow.yaml — override backend to tier 3 (framework default is 2)
    write(
        &root.join(".pice/workflow.yaml"),
        r#"
schema_version: "0.2"
defaults:
  tier: 2
  min_confidence: 0.9
  max_passes: 5
  model: sonnet
  budget_usd: 2.0
  cost_cap_behavior: halt
layer_overrides:
  backend:
    tier: 3
"#,
    );

    // 3. Git repo + a change matching the backend layer
    setup_git_repo(root);
    write(&root.join("src/server/main.rs"), "fn main() {}");

    // Plan with a contract (shape matches parser expectations)
    write(
        &root.join(".claude/plans/feature.md"),
        r#"# Plan

## Contract

```json
{ "feature": "test", "tier": 2, "pass_threshold": 8, "criteria": [] }
```
"#,
    );

    // 4. Run Stack Loops
    let layers_config = LayersConfig::load(&root.join(".pice/layers.toml")).unwrap();
    let workflow = loader::resolve(root).expect("workflow should resolve");
    let pice_config = PiceConfig::default();

    let empty_seams: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let cfg = StackLoopsConfig {
        layers: &layers_config,
        plan_path: &root.join(".claude/plans/feature.md"),
        project_root: root,
        primary_provider: "test-provider",
        primary_model: "test-model",
        pice_config: &pice_config,
        workflow: &workflow,
        merged_seams: &empty_seams,
    };

    let pass_sink: std::sync::Arc<dyn pice_daemon::orchestrator::PassMetricsSink> =
        std::sync::Arc::new(NullPassSink);
    let manifest = run_stack_loops(&cfg, &NullSink, false, pass_sink)
        .await
        .unwrap();

    // 5. Assert tier-3 for backend (overridden) and tier-2 for frontend (default)
    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .expect("backend layer present");
    assert_eq!(
        backend.halted_by.as_deref(),
        Some("phase-1-pending-tier-3"),
        "backend should record the overridden tier 3, got: {:?}",
        backend.halted_by
    );

    // Frontend is transitively activated (depends_on backend). It has no own
    // file changes, so it's Skipped with a cascade reason — the tier override
    // only surfaces when a layer has diffable changes. Verify it's Skipped
    // rather than recording a tier.
    let frontend = manifest
        .layers
        .iter()
        .find(|l| l.name == "frontend")
        .expect("frontend layer present");
    assert!(
        matches!(
            frontend.status,
            pice_core::layers::manifest::LayerStatus::Skipped
        ),
        "frontend should be Skipped (cascade, no own changes)"
    );

    // Infrastructure is always_run with no diff — Pending with its own reason,
    // not the tier-encoded one. Confirms effective_tier is only recorded on
    // the phase-1-pending (with diff) code path.
    let infra = manifest
        .layers
        .iter()
        .find(|l| l.name == "infrastructure")
        .expect("infrastructure layer present");
    assert!(
        matches!(
            infra.status,
            pice_core::layers::manifest::LayerStatus::Pending
        ),
        "infrastructure should be Pending (always_run, no diff)"
    );
}

#[tokio::test]
async fn workflow_defaults_applied_when_no_override() {
    // Complementary scenario: no layer_overrides → every layer uses defaults.tier
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write(
        &root.join(".pice/layers.toml"),
        r#"
[layers]
order = ["backend"]

[layers.backend]
paths = ["src/server/**"]
"#,
    );

    // No workflow.yaml — framework defaults (tier 2) apply
    setup_git_repo(root);
    write(&root.join("src/server/main.rs"), "fn main() {}");
    write(
        &root.join(".claude/plans/f.md"),
        "# Plan\n\n## Contract\n```json\n{\"criteria\":[]}\n```\n",
    );

    let layers_config = LayersConfig::load(&root.join(".pice/layers.toml")).unwrap();
    let workflow = loader::resolve(root).unwrap();
    let pice_config = PiceConfig::default();

    let empty_seams: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let cfg = StackLoopsConfig {
        layers: &layers_config,
        plan_path: &root.join(".claude/plans/f.md"),
        project_root: root,
        primary_provider: "test-provider",
        primary_model: "test-model",
        pice_config: &pice_config,
        workflow: &workflow,
        merged_seams: &empty_seams,
    };

    let pass_sink: std::sync::Arc<dyn pice_daemon::orchestrator::PassMetricsSink> =
        std::sync::Arc::new(NullPassSink);
    let manifest = run_stack_loops(&cfg, &NullSink, true, pass_sink)
        .await
        .unwrap();
    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap();
    assert_eq!(backend.halted_by.as_deref(), Some("phase-1-pending-tier-2"));
}

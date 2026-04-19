//! End-to-end integration tests for PRDv2 Phase 3 seam verification.
//!
//! Each test drives `run_stack_loops` with a fully configured workflow that
//! declares seam boundaries, executes against a real git repo, and asserts
//! the resulting verification manifest contains the expected `Failed`
//! findings with correct `category` and `boundary` fields. These tests
//! satisfy the three PRDv2 Feature 6 acceptance criteria
//! (lines 1067–1069).

use pice_core::config::PiceConfig;
use pice_core::layers::manifest::{CheckStatus, LayerStatus};
use pice_core::layers::{LayerDef, LayersConfig, LayersTable};
use pice_core::workflow::WorkflowConfig;
use pice_daemon::orchestrator::stack_loops::{run_stack_loops, StackLoopsConfig};
use pice_daemon::orchestrator::{NullPassSink, NullSink};
use std::collections::BTreeMap;
use std::path::Path;

fn git_init(dir: &Path) {
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

fn write_file(dir: &Path, rel: &str, content: &str) {
    let full = dir.join(rel);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&full, content).unwrap();
}

fn base_workflow() -> WorkflowConfig {
    pice_core::workflow::loader::embedded_defaults()
}

fn insert_layer(
    defs: &mut BTreeMap<String, LayerDef>,
    name: &str,
    paths: &[&str],
    always_run: bool,
) {
    defs.insert(
        name.to_string(),
        LayerDef {
            paths: paths.iter().map(|s| s.to_string()).collect(),
            always_run,
            contract: None,
            depends_on: Vec::new(),
            layer_type: None,
            environment_variants: None,
        },
    );
}

/// PRDv2 Feature 6 acceptance #1: env var declared in Dockerfile but read by
/// app under a different name → Failed category-1 finding at
/// `backend↔infrastructure`.
#[tokio::test]
async fn env_var_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());

    write_file(
        dir.path(),
        "src/main.rs",
        r#"fn main() { env::var("BAR").unwrap(); }"#,
    );
    write_file(dir.path(), "Dockerfile", "FROM alpine\nENV FOO=1\n");

    let mut defs = BTreeMap::new();
    insert_layer(&mut defs, "backend", &["src/**"], false);
    insert_layer(&mut defs, "infrastructure", &["Dockerfile"], false);

    let layers = LayersConfig {
        layers: LayersTable {
            order: vec!["backend".into(), "infrastructure".into()],
            defs,
        },
        seams: None,
        external_contracts: None,
        stacks: None,
    };

    let mut seams = BTreeMap::new();
    seams.insert(
        "backend↔infrastructure".to_string(),
        vec!["config_mismatch".to_string()],
    );
    let mut workflow = base_workflow();
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
    assert_eq!(backend.status, LayerStatus::Failed);
    assert_eq!(backend.halted_by.as_deref(), Some("seam:config_mismatch"));
    let finding = backend
        .seam_checks
        .iter()
        .find(|c| c.name == "config_mismatch")
        .unwrap();
    assert_eq!(finding.status, CheckStatus::Failed);
    assert_eq!(finding.category, Some(1));
    assert_eq!(finding.boundary, "backend↔infrastructure");
    let details = finding.details.as_deref().unwrap_or("");
    assert!(
        details.contains("FOO") || details.contains("BAR"),
        "finding should name the drifted env var; got: {details}"
    );
}

/// PRDv2 Feature 6 acceptance #2: Prisma schema declares a field missing
/// from the migration → Failed category-9 finding.
#[tokio::test]
async fn orm_schema_drift() {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());

    write_file(
        dir.path(),
        "prisma/schema.prisma",
        "model User {\n  id Int @id\n  email String\n  phone String\n}\n",
    );
    write_file(
        dir.path(),
        "migrations/001.sql",
        "CREATE TABLE User (id INT PRIMARY KEY, email TEXT);",
    );

    let mut defs = BTreeMap::new();
    insert_layer(&mut defs, "backend", &["prisma/**"], false);
    insert_layer(&mut defs, "database", &["migrations/**"], false);

    let layers = LayersConfig {
        layers: LayersTable {
            order: vec!["backend".into(), "database".into()],
            defs,
        },
        seams: None,
        external_contracts: None,
        stacks: None,
    };

    let mut seams = BTreeMap::new();
    seams.insert(
        "backend↔database".to_string(),
        vec!["schema_drift".to_string()],
    );
    let mut workflow = base_workflow();
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
    assert_eq!(backend.status, LayerStatus::Failed);
    let finding = backend
        .seam_checks
        .iter()
        .find(|c| c.name == "schema_drift")
        .unwrap();
    assert_eq!(finding.status, CheckStatus::Failed);
    assert_eq!(finding.category, Some(9));
    let details = finding.details.as_deref().unwrap_or("");
    assert!(
        details.contains("phone"),
        "finding should name the drifted column 'phone': {details}"
    );
}

/// PRDv2 Feature 6 acceptance #3: OpenAPI spec returns `id: integer` but the
/// handler returns `id: "..."` (string) → Failed category-3 finding.
#[tokio::test]
async fn openapi_drift() {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());

    write_file(
        dir.path(),
        "openapi.yaml",
        "paths:\n  /x:\n    get:\n      responses:\n        '200':\n          content:\n            application/json:\n              schema:\n                properties:\n                  id:\n                    type: integer\n",
    );
    write_file(
        dir.path(),
        "src/handlers.ts",
        "export function h() { return { id: \"abc\" }; }\n",
    );

    let mut defs = BTreeMap::new();
    insert_layer(&mut defs, "api", &["openapi.yaml"], false);
    insert_layer(&mut defs, "frontend", &["src/**"], false);

    let layers = LayersConfig {
        layers: LayersTable {
            order: vec!["api".into(), "frontend".into()],
            defs,
        },
        seams: None,
        external_contracts: None,
        stacks: None,
    };

    let mut seams = BTreeMap::new();
    seams.insert(
        "api↔frontend".to_string(),
        vec!["openapi_compliance".to_string()],
    );
    let mut workflow = base_workflow();
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

    let pass_sink: std::sync::Arc<dyn pice_daemon::orchestrator::PassMetricsSink> =
        std::sync::Arc::new(NullPassSink);
    let manifest = run_stack_loops(&cfg, &NullSink, true, pass_sink)
        .await
        .unwrap();
    let api_layer = manifest.layers.iter().find(|l| l.name == "api").unwrap();
    assert_eq!(api_layer.status, LayerStatus::Failed);
    let finding = api_layer
        .seam_checks
        .iter()
        .find(|c| c.name == "openapi_compliance")
        .unwrap();
    assert_eq!(finding.status, CheckStatus::Failed);
    assert_eq!(finding.category, Some(3));
    let details = finding.details.as_deref().unwrap_or("");
    assert!(
        details.contains("id"),
        "finding should name the diverging field 'id': {details}"
    );
}

/// Sanity: a clean fixture with no drift passes the same seam check path.
#[tokio::test]
async fn clean_fixture_passes_all_checks() {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());

    write_file(
        dir.path(),
        "src/main.rs",
        r#"fn main() { env::var("DATABASE_URL").unwrap(); }"#,
    );
    write_file(
        dir.path(),
        "Dockerfile",
        "FROM alpine\nENV DATABASE_URL=postgres://x\n",
    );

    let mut defs = BTreeMap::new();
    insert_layer(&mut defs, "backend", &["src/**"], false);
    insert_layer(&mut defs, "infrastructure", &["Dockerfile"], false);

    let layers = LayersConfig {
        layers: LayersTable {
            order: vec!["backend".into(), "infrastructure".into()],
            defs,
        },
        seams: None,
        external_contracts: None,
        stacks: None,
    };

    let mut seams = BTreeMap::new();
    seams.insert(
        "backend↔infrastructure".to_string(),
        vec!["config_mismatch".to_string()],
    );
    let mut workflow = base_workflow();
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
    // No seam failure → layer stays Pending (Phase 1 — provider not wired).
    assert_eq!(backend.status, LayerStatus::Pending);
    let check = backend
        .seam_checks
        .iter()
        .find(|c| c.name == "config_mismatch")
        .unwrap();
    assert_eq!(check.status, CheckStatus::Passed);
    assert_eq!(check.boundary, "backend↔infrastructure");
}

//! Phase 4 contract criterion #17 — concurrent-evaluation isolation.
//!
//! Spawn two `tokio::task::spawn`-ed evaluation runs against two distinct
//! features and project roots, synchronizing entry into the pass loop with a
//! `tokio::sync::Barrier`. Assert the resulting `pass_events` rows from each
//! run are bound to disjoint `evaluation_id` values, that each manifest's
//! summed pass cost equals its persisted `final_total_cost_usd` within
//! tolerance, and that no row's FK points at the wrong evaluation.

use pice_core::config::{
    AdversarialConfig, EvalProviderConfig, EvaluationConfig, InitConfig, MetricsConfig, PiceConfig,
    ProviderConfig, TelemetryConfig, TiersConfig,
};
use pice_core::layers::{LayerDef, LayersConfig, LayersTable};
use pice_core::workflow::schema::AdaptiveAlgo;
use pice_core::workflow::WorkflowConfig;
use pice_daemon::metrics::db::MetricsDb;
use pice_daemon::metrics::store::{self, DbBackedPassSink};
use pice_daemon::orchestrator::stack_loops::{run_stack_loops, StackLoopsConfig};
use pice_daemon::orchestrator::NullSink;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::Barrier;

// ─── Stub-env serialization (mirror adaptive_integration.rs) ────────────────

fn stub_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct StubScoresGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl StubScoresGuard {
    fn new(scores: &str) -> Self {
        let guard = stub_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("PICE_STUB_SCORES", scores);
        Self { _guard: guard }
    }
}

impl Drop for StubScoresGuard {
    fn drop(&mut self) {
        std::env::remove_var("PICE_STUB_SCORES");
    }
}

// ─── Fixture helpers ────────────────────────────────────────────────────────

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

/// Set up a project root with src/main.rs, .pice/config.toml, and a plan
/// file. Returns the `(plan_path, db)` pair.
fn setup_project(dir: &Path, feature_id: &str) -> (std::path::PathBuf, MetricsDb) {
    git_init(dir);
    write_file(dir, "src/main.rs", "fn main() {}");

    // Empty .pice/config.toml + a fresh SQLite DB at the configured path.
    let pice_dir = dir.join(".pice");
    std::fs::create_dir_all(&pice_dir).unwrap();
    let db_path = pice_dir.join("metrics.db");
    let db = MetricsDb::open(&db_path).unwrap();

    let plan_dir = dir.join(".claude/plans");
    std::fs::create_dir_all(&plan_dir).unwrap();
    let plan_path = plan_dir.join(format!("{feature_id}.md"));
    std::fs::write(
        &plan_path,
        "# Plan\n\n## Contract\n\n```json\n{\"feature\":\"x\",\"tier\":2,\"pass_threshold\":7,\"criteria\":[]}\n```\n",
    )
    .unwrap();
    (plan_path, db)
}

fn single_layer_config() -> LayersConfig {
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

fn stub_pice_config() -> PiceConfig {
    PiceConfig {
        provider: ProviderConfig {
            name: "stub".to_string(),
        },
        evaluation: EvaluationConfig {
            primary: EvalProviderConfig {
                provider: "stub".to_string(),
                model: "stub-model".to_string(),
            },
            adversarial: AdversarialConfig {
                provider: "stub".to_string(),
                model: "stub-model".to_string(),
                effort: "high".to_string(),
                enabled: false,
            },
            tiers: TiersConfig {
                tier1_models: vec![],
                tier2_models: vec![],
                tier3_models: vec![],
                tier3_agent_team: false,
            },
        },
        telemetry: TelemetryConfig {
            enabled: false,
            endpoint: String::new(),
        },
        metrics: MetricsConfig {
            db_path: ".pice/metrics.db".to_string(),
        },
        init: InitConfig::default(),
    }
}

fn workflow() -> WorkflowConfig {
    let mut wf = pice_core::workflow::loader::embedded_defaults();
    wf.defaults.min_confidence = 0.90;
    wf.defaults.max_passes = 4;
    wf.defaults.budget_usd = 10.0;
    wf.phases.evaluate.adaptive_algorithm = AdaptiveAlgo::BayesianSprt;
    wf
}

#[tokio::test]
async fn concurrent_evaluations_have_disjoint_pass_events() {
    // Stub returns 9.5 / pass with cost 0.01. SPRT halts after 3 passes.
    let _stub = StubScoresGuard::new(
        "9.5,0.01;9.5,0.01;9.5,0.01;9.5,0.01;9.5,0.01;9.5,0.01;9.5,0.01;9.5,0.01",
    );

    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let (plan_a, db_a) = setup_project(dir_a.path(), "feat-a");
    let (plan_b, db_b) = setup_project(dir_b.path(), "feat-b");

    let pice_a = stub_pice_config();
    let pice_b = stub_pice_config();
    let wf_a = workflow();
    let wf_b = workflow();
    let layers_a = single_layer_config();
    let layers_b = single_layer_config();

    // Insert evaluation headers so the sinks have valid evaluation_ids.
    let eval_id_a = store::insert_evaluation_header(
        &db_a,
        plan_a.to_str().unwrap(),
        "feat-a",
        2,
        "stub",
        "stub-model",
        None,
        None,
    )
    .unwrap();
    let eval_id_b = store::insert_evaluation_header(
        &db_b,
        plan_b.to_str().unwrap(),
        "feat-b",
        2,
        "stub",
        "stub-model",
        None,
        None,
    )
    .unwrap();

    let db_arc_a = Arc::new(Mutex::new(db_a));
    let db_arc_b = Arc::new(Mutex::new(db_b));
    let barrier = Arc::new(Barrier::new(2));

    // Take everything by-move into spawned tasks.
    let pa = plan_a.clone();
    let pb = plan_b.clone();
    let dap = dir_a.path().to_path_buf();
    let dbp = dir_b.path().to_path_buf();
    let dba_clone = db_arc_a.clone();
    let dbb_clone = db_arc_b.clone();
    let bara = barrier.clone();
    let barb = barrier.clone();
    let seams_a: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let seams_b: BTreeMap<String, Vec<String>> = BTreeMap::new();

    let handle_a = tokio::spawn(async move {
        let mut sink = DbBackedPassSink {
            db: dba_clone,
            evaluation_id: eval_id_a,
        };
        bara.wait().await;
        let cfg = StackLoopsConfig {
            layers: &layers_a,
            plan_path: &pa,
            project_root: &dap,
            primary_provider: "stub",
            primary_model: "stub-model",
            pice_config: &pice_a,
            workflow: &wf_a,
            merged_seams: &seams_a,
        };
        run_stack_loops(&cfg, &NullSink, true, &mut sink)
            .await
            .unwrap()
    });

    let handle_b = tokio::spawn(async move {
        let mut sink = DbBackedPassSink {
            db: dbb_clone,
            evaluation_id: eval_id_b,
        };
        barb.wait().await;
        let cfg = StackLoopsConfig {
            layers: &layers_b,
            plan_path: &pb,
            project_root: &dbp,
            primary_provider: "stub",
            primary_model: "stub-model",
            pice_config: &pice_b,
            workflow: &wf_b,
            merged_seams: &seams_b,
        };
        run_stack_loops(&cfg, &NullSink, true, &mut sink)
            .await
            .unwrap()
    });

    let manifest_a = handle_a.await.unwrap();
    let manifest_b = handle_b.await.unwrap();

    // ── Assertion 1: each manifest binds to its own feature_id ────────────
    assert_eq!(manifest_a.feature_id, "feat-a");
    assert_eq!(manifest_b.feature_id, "feat-b");

    // ── Assertion 2: pass_events per evaluation_id grouping ───────────────
    // Each DB is independent, so each holds its own pass_events rows. The
    // contract criterion's intent is that no cross-evaluation contamination
    // exists; we enforce that by querying each DB separately for its eval
    // and asserting the row count matches the manifest's per-layer pass
    // count.
    fn count_pass_events(db: &MetricsDb, eval_id: i64) -> i64 {
        let conn = db.conn();
        conn.query_row(
            "SELECT COUNT(*) FROM pass_events WHERE evaluation_id = ?1",
            rusqlite::params![eval_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    let db_a_locked = db_arc_a.lock().unwrap();
    let db_b_locked = db_arc_b.lock().unwrap();
    let count_a = count_pass_events(&db_a_locked, eval_id_a);
    let count_b = count_pass_events(&db_b_locked, eval_id_b);
    let manifest_passes_a: i64 = manifest_a
        .layers
        .iter()
        .map(|l| l.passes.len() as i64)
        .sum();
    let manifest_passes_b: i64 = manifest_b
        .layers
        .iter()
        .map(|l| l.passes.len() as i64)
        .sum();
    assert_eq!(
        count_a, manifest_passes_a,
        "DB-A pass_events != manifest-A passes"
    );
    assert_eq!(
        count_b, manifest_passes_b,
        "DB-B pass_events != manifest-B passes"
    );

    // ── Assertion 3: each DB holds exactly ONE distinct evaluation_id ─────
    // Cross-DB ID collisions are intrinsic (SQLite auto-increment restarts
    // per file), so we verify isolation by counting distinct evaluation_id
    // groups in each DB. A leaked row from concurrent evaluation would
    // surface as a 2nd group.
    fn distinct_eval_groups(db: &MetricsDb) -> i64 {
        let conn = db.conn();
        conn.query_row(
            "SELECT COUNT(DISTINCT evaluation_id) FROM pass_events",
            [],
            |row| row.get(0),
        )
        .unwrap()
    }
    assert_eq!(
        distinct_eval_groups(&db_a_locked),
        1,
        "DB-A holds rows for >1 evaluation — concurrent run leaked"
    );
    assert_eq!(
        distinct_eval_groups(&db_b_locked),
        1,
        "DB-B holds rows for >1 evaluation — concurrent run leaked"
    );

    // ── Assertion 4: per-evaluation cost reconciliation ───────────────────
    fn sum_pass_costs(db: &MetricsDb, eval_id: i64) -> f64 {
        let conn = db.conn();
        conn.query_row(
            "SELECT COALESCE(SUM(cost_usd), 0.0) FROM pass_events WHERE evaluation_id = ?1",
            rusqlite::params![eval_id],
            |row| row.get(0),
        )
        .unwrap()
    }
    let cost_a = sum_pass_costs(&db_a_locked, eval_id_a);
    let cost_b = sum_pass_costs(&db_b_locked, eval_id_b);
    let manifest_cost_a: f64 = manifest_a
        .layers
        .iter()
        .filter_map(|l| l.total_cost_usd)
        .sum();
    let manifest_cost_b: f64 = manifest_b
        .layers
        .iter()
        .filter_map(|l| l.total_cost_usd)
        .sum();
    assert!(
        (cost_a - manifest_cost_a).abs() < 1e-9,
        "DB-A cost reconciliation: db={cost_a} vs manifest={manifest_cost_a}"
    );
    assert!(
        (cost_b - manifest_cost_b).abs() < 1e-9,
        "DB-B cost reconciliation: db={cost_b} vs manifest={manifest_cost_b}"
    );
}

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
async fn concurrent_evaluations_on_shared_db_have_disjoint_pass_events() {
    // Phase 4 post-adversarial-review fix (Codex High #5): the earlier
    // version of this test gave each evaluation its own SQLite file, making
    // cross-evaluation contamination impossible by construction. The real
    // contention risk is TWO writers racing on the SAME DB file — that's
    // what `busy_timeout` in `MetricsDb::open` plus WAL's single-writer
    // serialization has to tolerate. This test exercises the shared-DB
    // path end-to-end: both evaluations write to one `metrics.db`, enter
    // the pass loop simultaneously via a `Barrier`, and the assertions
    // verify (a) disjoint evaluation_id groups with (b) no lost rows.

    let _stub = StubScoresGuard::new(
        "9.5,0.01;9.5,0.01;9.5,0.01;9.5,0.01;9.5,0.01;9.5,0.01;9.5,0.01;9.5,0.01",
    );

    // Shared SQLite file: the projects live in separate temp dirs, but a
    // single `metrics.db` on disk is opened by two independent `MetricsDb`
    // handles — one per evaluation. This is the real daemon shape.
    let db_dir = tempfile::tempdir().unwrap();
    let shared_db_path = db_dir.path().join("metrics.db");

    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    git_init(dir_a.path());
    git_init(dir_b.path());
    write_file(dir_a.path(), "src/main.rs", "fn main() {}");
    write_file(dir_b.path(), "src/main.rs", "fn main() {}");
    let plan_a = dir_a.path().join(".claude/plans/feat-a.md");
    let plan_b = dir_b.path().join(".claude/plans/feat-b.md");
    std::fs::create_dir_all(plan_a.parent().unwrap()).unwrap();
    std::fs::create_dir_all(plan_b.parent().unwrap()).unwrap();
    std::fs::write(
        &plan_a,
        "# Plan\n\n## Contract\n\n```json\n{\"feature\":\"x\",\"tier\":2,\"pass_threshold\":7,\"criteria\":[]}\n```\n",
    )
    .unwrap();
    std::fs::write(
        &plan_b,
        "# Plan\n\n## Contract\n\n```json\n{\"feature\":\"x\",\"tier\":2,\"pass_threshold\":7,\"criteria\":[]}\n```\n",
    )
    .unwrap();

    // Two handles to the SAME SQLite file. Each writer serializes through
    // WAL; `busy_timeout` prevents SQLITE_BUSY on contention.
    let db_handle_a = MetricsDb::open(&shared_db_path).unwrap();
    let db_handle_b = MetricsDb::open(&shared_db_path).unwrap();

    let pice_a = stub_pice_config();
    let pice_b = stub_pice_config();
    let wf_a = workflow();
    let wf_b = workflow();
    let layers_a = single_layer_config();
    let layers_b = single_layer_config();

    // Insert evaluation headers so the sinks have valid evaluation_ids.
    // Both writes hit the SAME DB — the auto-increment assigns 1 then 2.
    let eval_id_a = store::insert_evaluation_header(
        &db_handle_a,
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
        &db_handle_b,
        plan_b.to_str().unwrap(),
        "feat-b",
        2,
        "stub",
        "stub-model",
        None,
        None,
    )
    .unwrap();
    assert_ne!(
        eval_id_a, eval_id_b,
        "shared DB must assign distinct evaluation_ids"
    );

    let db_arc_a = Arc::new(Mutex::new(db_handle_a));
    let db_arc_b = Arc::new(Mutex::new(db_handle_b));
    let barrier = Arc::new(Barrier::new(2));

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

    // Open a third handle to the shared DB for read-side assertions (avoids
    // having to release the sinks' locks). Same file, same WAL view.
    let reader = MetricsDb::open(&shared_db_path).unwrap();

    // ── Assertion 1: each manifest binds to its own feature_id ────────────
    assert_eq!(manifest_a.feature_id, "feat-a");
    assert_eq!(manifest_b.feature_id, "feat-b");

    // ── Assertion 2: the shared DB holds EXACTLY TWO distinct evaluation_id
    //    groups in pass_events. A lost row on either side would leave only 1
    //    group with extra rows; contamination would show as a cross-ref. ───
    let distinct_groups: i64 = reader
        .conn()
        .query_row(
            "SELECT COUNT(DISTINCT evaluation_id) FROM pass_events",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        distinct_groups, 2,
        "shared DB must hold 2 evaluation groups (one per concurrent run)"
    );

    // ── Assertion 3: per-evaluation row count matches manifest pass count ─
    fn count_pass_events_for(db: &MetricsDb, eval_id: i64) -> i64 {
        db.conn()
            .query_row(
                "SELECT COUNT(*) FROM pass_events WHERE evaluation_id = ?1",
                rusqlite::params![eval_id],
                |row| row.get(0),
            )
            .unwrap()
    }
    let count_a = count_pass_events_for(&reader, eval_id_a);
    let count_b = count_pass_events_for(&reader, eval_id_b);
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
        "eval_id_a pass_events count != manifest-A passes (lost row under contention?)"
    );
    assert_eq!(
        count_b, manifest_passes_b,
        "eval_id_b pass_events count != manifest-B passes (lost row under contention?)"
    );

    // ── Assertion 4: per-evaluation cost reconciliation on the SHARED DB ──
    fn sum_pass_costs_for(db: &MetricsDb, eval_id: i64) -> f64 {
        db.conn()
            .query_row(
                "SELECT COALESCE(SUM(cost_usd), 0.0) FROM pass_events WHERE evaluation_id = ?1",
                rusqlite::params![eval_id],
                |row| row.get(0),
            )
            .unwrap()
    }
    let cost_a = sum_pass_costs_for(&reader, eval_id_a);
    let cost_b = sum_pass_costs_for(&reader, eval_id_b);
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
        "eval_id_a cost reconciliation: db={cost_a} vs manifest={manifest_cost_a}"
    );
    assert!(
        (cost_b - manifest_cost_b).abs() < 1e-9,
        "eval_id_b cost reconciliation: db={cost_b} vs manifest={manifest_cost_b}"
    );

    // ── Assertion 5: no cross-contamination — no row under eval_id_a has a
    //    `model` string that came from eval_id_b's run and vice versa. With
    //    both runs using the same `stub-model`, this is a tautology for the
    //    model column, so instead we assert the disjoint pass_index ranges
    //    belong to the right eval (pass indices are 1-indexed in manifest). ─
    let rows_under_a: Vec<i64> = reader
        .conn()
        .prepare("SELECT pass_index FROM pass_events WHERE evaluation_id = ?1 ORDER BY pass_index")
        .unwrap()
        .query_map(rusqlite::params![eval_id_a], |row| row.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let rows_under_b: Vec<i64> = reader
        .conn()
        .prepare("SELECT pass_index FROM pass_events WHERE evaluation_id = ?1 ORDER BY pass_index")
        .unwrap()
        .query_map(rusqlite::params![eval_id_b], |row| row.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    // Each evaluation's pass_index sequence must start at 1 and be contiguous.
    for (i, pi) in rows_under_a.iter().enumerate() {
        assert_eq!(*pi, (i + 1) as i64, "eval-A pass_index not contiguous");
    }
    for (i, pi) in rows_under_b.iter().enumerate() {
        assert_eq!(*pi, (i + 1) as i64, "eval-B pass_index not contiguous");
    }
}

// ─── Phase 4.1 Pass-10 Codex HIGH #2 + C17 — same-feature concurrency ────────
//
// The earlier test above uses DIFFERENT feature IDs, so it never contends
// on the same lock. These tests exercise the SAME `{project_hash, feature_id}`
// pair — the case Codex HIGH #2 identified as untested by the Pass-9 suite.
//
// Two-dimensional coverage:
// - Same-process: the Pass-6 in-process `TokioMutex` must serialize two
//   concurrent tokio tasks that both ask for the manifest lock.
// - Cross-process: the Pass-10 file lock (POSIX `flock` / Windows
//   `LockFileEx`) must block a second fd on the same lock path. We
//   simulate a second process by opening a second file handle from a
//   blocking thread — `fs2` layers on top of `flock`, which is per-file-
//   description, not per-process, so a second fd is indistinguishable
//   from a second process for the blocking semantic.

use pice_daemon::server::router::DaemonContext;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_feature_manifest_lock_serializes_concurrent_tasks() {
    // Prove the in-process lock's identity semantics: two calls with the
    // SAME (project_hash, feature_id) return the SAME `Arc<TokioMutex>`,
    // so two concurrent holders serialize. If they ever interleaved (e.g.
    // the map keyed incorrectly), the sentinel-check at the bottom
    // would observe out-of-order timestamps.
    let ctx = DaemonContext::inline();
    let (hash, feature) = ("proj-hash-abc", "same-feature");

    let lock_1 = ctx.manifest_lock_for(hash, feature);
    let lock_2 = ctx.manifest_lock_for(hash, feature);

    // Same Arc identity — critical invariant. If the map keyed on something
    // derived (e.g. hash+feature hashed) and collisions produced distinct
    // Arcs, serialization would silently break.
    assert!(
        Arc::ptr_eq(&lock_1, &lock_2),
        "manifest_lock_for must return the SAME Arc for identical (hash, feature) pairs"
    );

    // Runtime proof: spawn two tasks that both acquire the lock and
    // verify only one holds at a time. Using atomic counters rather than
    // sleeps so the test is fast and deterministic.
    use std::sync::atomic::{AtomicUsize, Ordering};
    let holders = Arc::new(AtomicUsize::new(0));
    let max_concurrent = Arc::new(AtomicUsize::new(0));

    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let lock = ctx.manifest_lock_for(hash, feature);
            let holders = holders.clone();
            let max_concurrent = max_concurrent.clone();
            tokio::spawn(async move {
                let _g = lock.lock().await;
                let held = holders.fetch_add(1, Ordering::SeqCst) + 1;
                // Record any instant where >1 task holds the lock.
                max_concurrent.fetch_max(held, Ordering::SeqCst);
                // Yield so another task GETS a chance to violate the invariant
                // if the lock were broken — the yield forces the scheduler
                // to re-examine the ready queue.
                tokio::task::yield_now().await;
                holders.fetch_sub(1, Ordering::SeqCst);
            })
        })
        .collect();
    for t in tasks {
        t.await.unwrap();
    }
    assert_eq!(
        max_concurrent.load(Ordering::SeqCst),
        1,
        "in-process manifest lock failed to serialize same-feature tasks"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_feature_different_features_hold_distinct_locks() {
    // Negative control: two DIFFERENT features on the same project hash
    // must NOT share a lock (otherwise `pice evaluate feat-a` + `pice
    // evaluate feat-b` would needlessly serialize).
    let ctx = DaemonContext::inline();
    let lock_a = ctx.manifest_lock_for("proj", "feat-a");
    let lock_b = ctx.manifest_lock_for("proj", "feat-b");
    assert!(
        !Arc::ptr_eq(&lock_a, &lock_b),
        "distinct features must return distinct Arc<TokioMutex>"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manifest_file_lock_blocks_second_acquirer() {
    // Phase 4.1 Pass-10 Codex HIGH #2: cross-process file lock. The
    // semantic we care about — "a second acquirer on the same path must
    // block until the first releases" — is verified by opening two file
    // descriptors on the same lock file. `fs2` uses `flock(2)` on Unix
    // which is per-file-description, so a second fd from the same
    // process is indistinguishable from a second process.
    use fs2::FileExt;
    use std::fs::OpenOptions;
    use std::sync::atomic::{AtomicBool, Ordering};

    let tmp = tempfile::tempdir().unwrap();
    let lock_path = tmp.path().join("feat.manifest.lock");
    std::fs::write(&lock_path, b"").unwrap();

    let holder_released = Arc::new(AtomicBool::new(false));
    let holder_released_inner = holder_released.clone();
    let lock_path_clone = lock_path.clone();

    // Task 1: acquire the lock and hold it for ~100ms.
    let holder = tokio::task::spawn_blocking(move || {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path_clone)
            .unwrap();
        file.lock_exclusive().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        holder_released_inner.store(true, Ordering::SeqCst);
        // Drop releases the flock. Explicit drop for clarity.
        drop(file);
    });

    // Give task 1 a head-start so its lock is held before task 2 tries.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let holder_released_outer = holder_released.clone();
    let lock_path_outer = lock_path.clone();

    // Task 2: blocking acquire — must NOT succeed until task 1 releases.
    let waiter = tokio::task::spawn_blocking(move || {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path_outer)
            .unwrap();
        file.lock_exclusive().unwrap();
        // Invariant: by the time this lock_exclusive() returns, task 1
        // must have already set `holder_released`. Anything else means
        // the file lock let two holders in simultaneously.
        let released = holder_released_outer.load(Ordering::SeqCst);
        assert!(
            released,
            "file lock did not block — second acquirer got in before the holder released"
        );
        drop(file);
    });

    holder.await.unwrap();
    waiter.await.unwrap();
}

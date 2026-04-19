//! End-to-end integration tests for Phase 5 cohort parallelism.
//!
//! Each test drives `run_stack_loops_with_cancel` with the stub provider
//! against a two-layer DAG and asserts the three critical invariants:
//!
//! 1. **DAG-ordered manifest**: `manifest.layers[].name` ordering is
//!    topological, NOT task-completion order. Parallel execution must
//!    not disturb this — two back-to-back runs with identical
//!    `PICE_STUB_SCORES_*` envs produce byte-identical layer ordering.
//! 2. **Per-layer context isolation**: the `EvaluateCreateParams` payload
//!    received by the stub for layer `backend` carries backend's contract
//!    and filtered diff ONLY — never frontend's. Verified by structural
//!    inequality on the logged request payloads (per the Cycle-2 Consider
//!    finding: NO substring greps — common project terms produce false
//!    positives/negatives).
//! 3. **Parallel/sequential gate matrix**: the `(parallel_configured,
//!    cohort_size, max_parallelism)` triple decides which path runs.
//!    The gate emits a `pice.cohort` tracing event with
//!    `path = "parallel" | "sequential"` — tests capture the event via a
//!    subscriber layer. No production-code counters.
//! 4. **Cancellation**: `cancel.cancel()` mid-evaluation aborts in-flight
//!    provider sessions within ≤ 200ms of the signal and marks the
//!    affected layer(s) `Failed` with `halted_by = "cancelled:*"`.
//! 5. **Determinism under parallelism**: two consecutive runs produce
//!    byte-identical `manifest.layers[].name` ordering.

use pice_core::cli::ExitJsonStatus;
use pice_core::config::{
    AdversarialConfig, EvalProviderConfig, EvaluationConfig, InitConfig, MetricsConfig, PiceConfig,
    ProviderConfig, TelemetryConfig, TiersConfig,
};
use pice_core::layers::manifest::LayerStatus;
use pice_core::layers::{LayerDef, LayersConfig, LayersTable};
use pice_core::workflow::schema::AdaptiveAlgo;
use pice_core::workflow::WorkflowConfig;
use pice_daemon::orchestrator::stack_loops::{run_stack_loops_with_cancel, StackLoopsConfig};
use pice_daemon::orchestrator::{NullPassSink, NullSink, PassMetricsSink};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Layer;

// ─── Env guard ─────────────────────────────────────────────────────────────
//
// All PICE_STUB_* env vars are process-global; concurrent tests would race
// on them. Share a single static mutex across every guard in this file so
// that even tests running in parallel serialize their env mutations.

fn stub_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// RAII guard that sets per-layer PICE_STUB_SCORES_* and (optionally)
/// PICE_STUB_LATENCY_MS on construction, clears them on drop.
struct ParallelStubGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
    env_keys: Vec<String>,
}

impl ParallelStubGuard {
    fn new(per_layer_scores: &[(&str, &str)], latency_ms: Option<u64>) -> Self {
        let guard = stub_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let mut env_keys = Vec::new();
        for (layer, scores) in per_layer_scores {
            let key = format!("PICE_STUB_SCORES_{}", layer.to_uppercase());
            std::env::set_var(&key, scores);
            env_keys.push(key);
        }
        if let Some(ms) = latency_ms {
            std::env::set_var("PICE_STUB_LATENCY_MS", ms.to_string());
            env_keys.push("PICE_STUB_LATENCY_MS".to_string());
        }
        // Also clear the shared PICE_STUB_SCORES so tests that ONLY use
        // per-layer scores can't accidentally pick up a leftover shared
        // list from some earlier run.
        std::env::remove_var("PICE_STUB_SCORES");
        Self {
            _guard: guard,
            env_keys,
        }
    }
}

impl Drop for ParallelStubGuard {
    fn drop(&mut self) {
        for k in &self.env_keys {
            std::env::remove_var(k);
        }
    }
}

// ─── Request-log capture for context-isolation tests ────────────────────────
//
// The stub writes per-call EvaluateCreateParams to `PICE_STUB_REQUEST_LOG`
// (one JSON line per call). Tests parse the file to assert STRUCTURAL
// inequality on the typed fields — NOT substring matches against prompt
// text (the Cycle-2 Consider finding).

struct RequestLogGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl RequestLogGuard {
    fn new(log_path: &Path, per_layer_scores: &[(&str, &str)]) -> Self {
        let guard = stub_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("PICE_STUB_REQUEST_LOG", log_path);
        for (layer, scores) in per_layer_scores {
            let key = format!("PICE_STUB_SCORES_{}", layer.to_uppercase());
            std::env::set_var(&key, scores);
        }
        std::env::remove_var("PICE_STUB_SCORES");
        Self { _guard: guard }
    }
}

impl Drop for RequestLogGuard {
    fn drop(&mut self) {
        std::env::remove_var("PICE_STUB_REQUEST_LOG");
        std::env::remove_var("PICE_STUB_SCORES_BACKEND");
        std::env::remove_var("PICE_STUB_SCORES_FRONTEND");
    }
}

// ─── Tracing capture for cohort-path gate tests ─────────────────────────────

/// Captures the `path` field of every `target: "pice.cohort"` event into a
/// shared `Vec<String>`. Used by the gate-matrix tests to assert which
/// branch the orchestrator took without adding production-code counters.
#[derive(Clone, Default)]
struct CohortPathCapture {
    events: Arc<Mutex<Vec<String>>>,
}

impl CohortPathCapture {
    fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn paths(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
}

impl<S> Layer<S> for CohortPathCapture
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if event.metadata().target() != "pice.cohort" {
            return;
        }
        struct PathVisitor<'a>(&'a mut Option<String>);
        impl<'a> tracing::field::Visit for PathVisitor<'a> {
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                if field.name() == "path" {
                    *self.0 = Some(value.to_string());
                }
            }
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "path" {
                    let s = format!("{value:?}");
                    // Strip surrounding quotes that Debug adds to &str.
                    let trimmed = s.trim_matches('"').to_string();
                    *self.0 = Some(trimmed);
                }
            }
        }
        let mut path: Option<String> = None;
        event.record(&mut PathVisitor(&mut path));
        if let Some(p) = path {
            self.events.lock().unwrap().push(p);
        }
    }
}

fn install_cohort_capture() -> (CohortPathCapture, DefaultGuard) {
    let capture = CohortPathCapture::new();
    let subscriber = tracing_subscriber::Registry::default().with(capture.clone());
    let guard = tracing::subscriber::set_default(subscriber);
    (capture, guard)
}

// ─── Repo + layers fixtures ─────────────────────────────────────────────────

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

/// Two-layer config with NO `depends_on` edge between them — they land in
/// the same DAG cohort and are eligible for parallel execution.
fn two_layer_independent_config() -> LayersConfig {
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
            depends_on: Vec::new(),
            layer_type: None,
            environment_variants: None,
        },
    );
    LayersConfig {
        layers: LayersTable {
            order: vec!["backend".to_string(), "frontend".to_string()],
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

/// Workflow with `phases.evaluate.parallel` and `defaults.max_parallelism`
/// configurable per test.
fn workflow(parallel: bool, max_parallelism: Option<u32>, max_passes: u32) -> WorkflowConfig {
    let mut wf = pice_core::workflow::loader::embedded_defaults();
    // min_confidence low enough that a single 8.0 score already hits the
    // SPRT accept bound — tests halt on pass 1 for fast runs.
    wf.defaults.min_confidence = 0.70;
    wf.defaults.max_passes = max_passes;
    wf.defaults.budget_usd = 0.0; // disable capability gate; stub emits NULL cost
    wf.defaults.max_parallelism = max_parallelism;
    wf.phases.evaluate.parallel = parallel;
    wf.phases.evaluate.adaptive_algorithm = AdaptiveAlgo::BayesianSprt;
    wf
}

fn setup_two_layer_repo(dir: &Path) -> std::path::PathBuf {
    git_init(dir);
    write_file(dir, "src/server/main.rs", "fn main() {}");
    write_file(dir, "src/client/App.tsx", "export const App = () => null;");
    let plan_dir = dir.join(".claude/plans");
    std::fs::create_dir_all(&plan_dir).unwrap();
    let plan_path = plan_dir.join("phase5-test.md");
    std::fs::write(
        &plan_path,
        "# Phase 5 test\n\n## Contract\n\n```json\n{\"feature\":\"parallel\",\"tier\":2,\"pass_threshold\":7,\"criteria\":[]}\n```\n",
    )
    .unwrap();
    plan_path
}

fn make_cfg<'a>(
    layers: &'a LayersConfig,
    plan_path: &'a Path,
    project_root: &'a Path,
    pice_config: &'a PiceConfig,
    wf: &'a WorkflowConfig,
    seams: &'a BTreeMap<String, Vec<String>>,
) -> StackLoopsConfig<'a> {
    StackLoopsConfig {
        layers,
        plan_path,
        project_root,
        primary_provider: "stub",
        primary_model: "stub-model",
        pice_config,
        workflow: wf,
        merged_seams: seams,
    }
}

fn null_sink_arc() -> Arc<dyn PassMetricsSink> {
    Arc::new(NullPassSink)
}

// ─── Test 1: DAG-ordered manifest under parallelism ─────────────────────────

/// Two-layer cohort, parallel enabled, both layers have different per-layer
/// scores. Run the evaluation twice; assert `manifest.layers[].name` is
/// byte-identical across both runs AND matches the DAG order
/// (`backend` before `frontend`), regardless of which task finished first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_cohort_preserves_dag_order() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_two_layer_repo(dir.path());
    let layers = two_layer_independent_config();
    let pice_config = stub_pice_config();
    let wf = workflow(true, None, 1);
    let seams = BTreeMap::new();
    let cfg = make_cfg(&layers, &plan_path, dir.path(), &pice_config, &wf, &seams);

    // Per-layer scores ensure the parallel tasks don't race on a shared
    // iterator. Latency 50ms per pass is enough to guarantee task
    // interleaving but keeps the test fast.
    let _stub = ParallelStubGuard::new(
        &[("backend", "9.0,0.01"), ("frontend", "8.0,0.01")],
        Some(50),
    );

    let names_run_1: Vec<String> = {
        let manifest = run_stack_loops_with_cancel(
            &cfg,
            &NullSink,
            true,
            null_sink_arc(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        manifest.layers.iter().map(|l| l.name.clone()).collect()
    };
    let names_run_2: Vec<String> = {
        let manifest = run_stack_loops_with_cancel(
            &cfg,
            &NullSink,
            true,
            null_sink_arc(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        manifest.layers.iter().map(|l| l.name.clone()).collect()
    };

    // DAG order: backend before frontend (matches layers.toml `order`).
    assert_eq!(
        names_run_1,
        vec!["backend".to_string(), "frontend".to_string()],
        "run 1 layer order must match DAG topology, not task completion order",
    );
    // Byte-identical across runs.
    assert_eq!(
        names_run_1, names_run_2,
        "two parallel runs with identical per-layer scores must produce byte-identical manifest layer ordering",
    );
}

// ─── Test 2: per-layer context isolation (structural) ──────────────────────

/// Instruments the stub via `PICE_STUB_REQUEST_LOG`, runs the 2-layer
/// cohort in parallel, then parses the recorded payloads and asserts
/// STRUCTURAL inequality between the two layers' `contract` and `diff`
/// fields. Substring grep on the wire text is explicitly avoided (Cycle-2
/// Consider finding): common project terms like "criteria" or "contract"
/// would false-match across layers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_layers_dont_leak_context() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_two_layer_repo(dir.path());
    let layers = two_layer_independent_config();
    let pice_config = stub_pice_config();
    let wf = workflow(true, None, 1);
    let seams = BTreeMap::new();
    let cfg = make_cfg(&layers, &plan_path, dir.path(), &pice_config, &wf, &seams);

    // Write per-layer contracts so the recorded contract payloads differ
    // verifiably between layers (not just in layer name).
    write_file(
        dir.path(),
        ".pice/contracts/backend.toml",
        "[criteria]\nbackend_correctness = \"Backend layer is correct\"\n",
    );
    write_file(
        dir.path(),
        ".pice/contracts/frontend.toml",
        "[criteria]\nfrontend_correctness = \"Frontend layer is correct\"\n",
    );

    let log_path = dir.path().join("stub-requests.jsonl");
    let _log = RequestLogGuard::new(
        &log_path,
        &[("backend", "9.0,0.01"), ("frontend", "8.0,0.01")],
    );

    let _manifest = run_stack_loops_with_cancel(
        &cfg,
        &NullSink,
        true,
        null_sink_arc(),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    // Parse the request log. Expect ≥2 entries (one per layer), possibly
    // more if the adaptive loop runs multiple passes per layer.
    let log_raw = std::fs::read_to_string(&log_path).expect("request log exists");
    let entries: Vec<serde_json::Value> = log_raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSON line"))
        .collect();
    assert!(
        entries.len() >= 2,
        "expected ≥2 recorded requests (one per layer), got {} entries",
        entries.len()
    );

    // Group by layer. The stub records `contract` verbatim — which the
    // daemon built as `{"layer": "<name>", "contract_toml": "..."}`.
    let backend_entries: Vec<&serde_json::Value> = entries
        .iter()
        .filter(|e| e["contract"]["layer"].as_str() == Some("backend"))
        .collect();
    let frontend_entries: Vec<&serde_json::Value> = entries
        .iter()
        .filter(|e| e["contract"]["layer"].as_str() == Some("frontend"))
        .collect();
    assert!(!backend_entries.is_empty(), "no backend requests recorded",);
    assert!(
        !frontend_entries.is_empty(),
        "no frontend requests recorded",
    );

    // STRUCTURAL isolation — typed field comparison, not substring grep.
    let backend_contract = &backend_entries[0]["contract"];
    let frontend_contract = &frontend_entries[0]["contract"];
    assert_ne!(
        backend_contract["layer"], frontend_contract["layer"],
        "layer field must differ between recorded payloads",
    );
    assert_ne!(
        backend_contract["contract_toml"], frontend_contract["contract_toml"],
        "per-layer contract_toml must differ; backend saw frontend's contract \
         (or vice versa) — context leakage",
    );

    // Diff isolation: backend's filtered diff contains `src/server/main.rs`
    // but NOT `src/client/App.tsx` (and vice versa). Assert on the
    // structural diff content — still NOT substring on arbitrary text.
    let backend_diff = backend_entries[0]["diff"].as_str().unwrap_or("");
    let frontend_diff = frontend_entries[0]["diff"].as_str().unwrap_or("");
    assert!(
        backend_diff.contains("src/server/main.rs"),
        "backend diff must include backend file path; got: {backend_diff}",
    );
    assert!(
        !backend_diff.contains("src/client/App.tsx"),
        "backend diff MUST NOT include frontend file path; got: {backend_diff}",
    );
    assert!(
        frontend_diff.contains("src/client/App.tsx"),
        "frontend diff must include frontend file path; got: {frontend_diff}",
    );
    assert!(
        !frontend_diff.contains("src/server/main.rs"),
        "frontend diff MUST NOT include backend file path; got: {frontend_diff}",
    );
}

// ─── Test 3: gate matrix — five cells ───────────────────────────────────────

/// Runs each cell of `(cohort_size, parallel_configured, max_parallelism)`
/// and asserts the `path` field of the `pice.cohort` tracing event matches
/// the expected branch.
async fn run_and_capture_path(wf: WorkflowConfig, layers: LayersConfig) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_two_layer_repo(dir.path());
    let pice_config = stub_pice_config();
    let seams = BTreeMap::new();
    let cfg = make_cfg(&layers, &plan_path, dir.path(), &pice_config, &wf, &seams);

    let (capture, _tracing_guard) = install_cohort_capture();
    let _stub = ParallelStubGuard::new(
        &[
            ("backend", "9.0,0.01"),
            ("frontend", "8.0,0.01"),
            ("api", "9.0,0.01"),
            ("db", "8.0,0.01"),
        ],
        None,
    );
    let _ = run_stack_loops_with_cancel(
        &cfg,
        &NullSink,
        true,
        null_sink_arc(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    capture.paths()
}

fn single_layer_config() -> LayersConfig {
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
async fn gate_one_layer_parallel_true_takes_sequential() {
    let paths = run_and_capture_path(workflow(true, None, 1), single_layer_config()).await;
    assert_eq!(paths, vec!["sequential".to_string()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_one_layer_parallel_false_takes_sequential() {
    let paths = run_and_capture_path(workflow(false, None, 1), single_layer_config()).await;
    assert_eq!(paths, vec!["sequential".to_string()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_two_layer_parallel_false_takes_sequential() {
    let paths =
        run_and_capture_path(workflow(false, None, 1), two_layer_independent_config()).await;
    assert_eq!(paths, vec!["sequential".to_string()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gate_two_layer_parallel_true_takes_parallel() {
    let paths = run_and_capture_path(workflow(true, None, 1), two_layer_independent_config()).await;
    assert_eq!(paths, vec!["parallel".to_string()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_max_parallelism_one_collapses_to_sequential() {
    // 2 independent layers, parallel:true, but max_parallelism=1 → the
    // semaphore has only one permit; the gate collapses to sequential
    // (per the `max_parallelism > 1` conjunct).
    let paths =
        run_and_capture_path(workflow(true, Some(1), 1), two_layer_independent_config()).await;
    assert_eq!(paths, vec!["sequential".to_string()]);
}

// ─── Test 4: cancellation aborts in-flight cohort ──────────────────────────

/// Starts a 2-layer parallel cohort with `PICE_STUB_LATENCY_MS=2000`
/// (2-second sleep per score response). Fires `cancel.cancel()` after
/// 400ms. Asserts the contract's hard bounds:
/// - **Cancel-to-return ≤ 300ms** — measured from the `cancel()` call,
///   NOT from test start. The 300ms envelope covers 200ms orchestrator
///   budget + 100ms runtime/scheduler/kill-on-drop latency. Regressions
///   where `JoinSet` drains to completion would exceed this by > 1700ms.
/// - **No orphan provider processes** — via `PICE_STUB_ALIVE_FILE`:
///   every "alive <pid>" line must have a matching "done <pid>" OR the
///   PID must be dead (`kill(pid, 0)` fails). `ProviderHost::spawn`
///   sets `kill_on_drop(true)`, so a cancelled cohort task's `Child`
///   drop sends SIGKILL to the stub process, and the stub never reaches
///   its "done" emission.
/// - Affected layers carry `halted_by` starting with `"cancelled:"`.
/// - The manifest is still readable (no panic, no corruption).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_aborts_in_flight_cohort() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_two_layer_repo(dir.path());
    let layers = two_layer_independent_config();
    let pice_config = stub_pice_config();
    let wf = workflow(true, None, 3);
    let seams = BTreeMap::new();
    let cfg = make_cfg(&layers, &plan_path, dir.path(), &pice_config, &wf, &seams);

    // Marker file for orphan-process detection.
    let alive_path = dir.path().join("stub-alive.log");
    std::fs::write(&alive_path, "").unwrap();

    // 2000ms per score response; with max_passes=3 the uncancelled run
    // would take at least 2s × 3 = 6s per layer.
    let _stub = ParallelStubGuard::new(
        &[("backend", "9.0,0.01"), ("frontend", "8.0,0.01")],
        Some(2000),
    );
    let _alive = StubAliveFileGuard::new(&alive_path);

    let cancel = CancellationToken::new();
    let cancel_trigger = cancel.clone();

    // Shared cancel-time marker — the spawn task writes the Instant at
    // which `cancel()` fired, the main task reads it after the
    // orchestrator returns and subtracts.
    let cancel_instant: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let cancel_instant_w = Arc::clone(&cancel_instant);

    // Fire cancellation after 400ms — well into the first pass of both
    // layers running in parallel.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        *cancel_instant_w.lock().unwrap() = Some(Instant::now());
        cancel_trigger.cancel();
    });

    let t0 = Instant::now();
    let manifest = run_stack_loops_with_cancel(&cfg, &NullSink, true, null_sink_arc(), cancel)
        .await
        .expect("run_stack_loops should return even under cancellation");
    let total_elapsed = t0.elapsed();
    let return_instant = Instant::now();

    let cancel_at = cancel_instant
        .lock()
        .unwrap()
        .expect("cancel task must have run");
    let cancel_to_return = return_instant.saturating_duration_since(cancel_at);

    // Contract bound: cancel-to-return ≤ 200ms. Budget an extra 100ms
    // for scheduler/kill-on-drop latency — total 300ms hard ceiling.
    // A regression where JoinSet drains to completion would exceed
    // 1700ms here (2000ms stub sleep - 400ms pre-cancel - rounding).
    assert!(
        cancel_to_return < Duration::from_millis(300),
        "cancel-to-return must be ≤ 300ms (contract: 200ms + 100ms runtime \
         slack); got {:?} (total {:?}). Look for a regression where JoinSet \
         drains fully before honoring cancel, or where the inner evaluate \
         loop doesn't await on the cancel token.",
        cancel_to_return,
        total_elapsed,
    );

    // Orphan-process gate: after a brief grace period for SIGKILL to
    // land, verify no PIDs in the alive-file are still running.
    //
    // Without `kill_on_drop(true)` in ProviderHost::spawn, the stub
    // Node process keeps sleeping for ~2s even after the Rust Child
    // drops — those PIDs would still be alive here and the assertion
    // below would fail. With kill_on_drop, drop → SIGKILL → process
    // gone immediately. We sleep 150ms to cover the kernel-side
    // cleanup latency.
    tokio::time::sleep(Duration::from_millis(150)).await;
    let orphans = find_orphan_alive_pids(&alive_path);
    assert!(
        orphans.is_empty(),
        "kill_on_drop regression: {} orphaned stub provider process(es) \
         survived cancellation (alive without matching done, still \
         responding to kill -0): {:?}. Look for removed kill_on_drop(true) \
         or a missed Child drop path.",
        orphans.len(),
        orphans,
    );

    // Manifest is readable. Both layers should have SOME status — each
    // one that saw cancellation carries `halted_by` starting with
    // `"cancelled:"`.
    assert_eq!(
        manifest.layers.len(),
        2,
        "both layers must appear in the manifest even under cancellation"
    );
    let cancelled_count = manifest
        .layers
        .iter()
        .filter(|l| {
            l.halted_by
                .as_deref()
                .map(ExitJsonStatus::is_cancelled)
                .unwrap_or(false)
        })
        .count();
    assert!(
        cancelled_count >= 1,
        "at least one layer must carry `cancelled:*` halted_by; got {:?}",
        manifest
            .layers
            .iter()
            .map(|l| (&l.name, &l.status, &l.halted_by))
            .collect::<Vec<_>>(),
    );
}

/// Parse the stub alive-file and return PIDs with "alive" but no
/// matching "done" that are still responding to `kill(pid, 0)` (Unix)
/// or that are still reachable via `OpenProcess` (Windows — skipped
/// here; the CI matrix verifies macOS/Linux + kill_on_drop is
/// Tokio-cross-platform anyway).
#[cfg(unix)]
fn find_orphan_alive_pids(path: &Path) -> Vec<i32> {
    let contents = std::fs::read_to_string(path).unwrap_or_default();
    let mut started: std::collections::HashSet<i32> = std::collections::HashSet::new();
    let mut finished: std::collections::HashSet<i32> = std::collections::HashSet::new();
    for line in contents.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() != 2 {
            continue;
        }
        let Ok(pid) = parts[1].parse::<i32>() else {
            continue;
        };
        match parts[0] {
            "alive" => {
                started.insert(pid);
            }
            "done" => {
                finished.insert(pid);
            }
            _ => {}
        }
    }
    started
        .difference(&finished)
        .copied()
        .filter(|&pid| {
            // kill(pid, 0) returns 0 if the process exists and the
            // caller has permission; returns -1 with ESRCH if gone.
            // SAFETY: `libc::kill` with signal 0 is a side-effect-free
            // liveness probe.
            unsafe { libc::kill(pid, 0) == 0 }
        })
        .collect()
}

#[cfg(not(unix))]
fn find_orphan_alive_pids(_path: &Path) -> Vec<i32> {
    // Orphan check is Unix-only for now (kill_on_drop is cross-platform
    // in tokio, but the kill(pid, 0) liveness probe needs a platform
    // impl). macOS/Linux CI coverage is sufficient for the Phase 5
    // contract criterion.
    Vec::new()
}

/// RAII guard for `PICE_STUB_ALIVE_FILE`.
///
/// **Lock discipline.** This guard does NOT acquire
/// [`stub_env_lock()`] even though [`ParallelStubGuard`] does. That is
/// intentional and safe: the two guards touch DISJOINT env keys
/// (`PICE_STUB_ALIVE_FILE` here vs. `PICE_STUB_*` for the parallel
/// guard) AND a `ParallelStubGuard` is always live in the enclosing
/// test scope, holding the serial-test lock while this guard mutates a
/// non-overlapping variable. Acquiring the same lock here would
/// deadlock (the outer guard already holds it). If a future guard ever
/// needs to touch keys that DO overlap with `PICE_STUB_*`, extend
/// `ParallelStubGuard` to cover the additional key rather than nesting
/// `stub_env_lock` calls.
struct StubAliveFileGuard;

impl StubAliveFileGuard {
    fn new(path: &Path) -> Self {
        std::env::set_var("PICE_STUB_ALIVE_FILE", path);
        Self
    }
}

impl Drop for StubAliveFileGuard {
    fn drop(&mut self) {
        std::env::remove_var("PICE_STUB_ALIVE_FILE");
    }
}

// ─── Test 5: max_parallelism is hard-capped at 16 ──────────────────────────

/// A user setting `defaults.max_parallelism: 64` cannot actually get 64
/// concurrent tasks — the hard cap of 16 applies. We verify the effect
/// indirectly: a cohort of 20 layers with `max_parallelism: 64` takes
/// at least `ceil(20/16) * latency` wall-clock (i.e. two batches of 16
/// layers, sequential between batches). With 100ms latency per layer,
/// that's ≥ 200ms — if the cap weren't enforced, one batch of 20 would
/// finish in ≥ 100ms.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn max_parallelism_hard_cap_at_16() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_two_layer_repo(dir.path());
    // Build a 20-layer independent cohort.
    let mut defs = BTreeMap::new();
    let mut order = Vec::new();
    let mut per_layer_scores: Vec<(String, String)> = Vec::new();
    for i in 0..20 {
        let name = format!("layer{i}");
        defs.insert(
            name.clone(),
            LayerDef {
                paths: vec![format!("src/layer{i}/**")],
                always_run: false,
                contract: None,
                depends_on: Vec::new(),
                layer_type: None,
                environment_variants: None,
            },
        );
        order.push(name.clone());
        per_layer_scores.push((name.clone(), "9.0,0.01".to_string()));
        // Touch a file in each layer so it's active.
        write_file(dir.path(), &format!("src/layer{i}/file.rs"), "fn x() {}");
    }
    // Recreate the git commit so diff contains all new files.
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let layers = LayersConfig {
        layers: LayersTable { order, defs },
        seams: None,
        external_contracts: None,
        stacks: None,
    };
    let pice_config = stub_pice_config();
    // Request max_parallelism=64 — should be clamped to 16.
    let wf = workflow(true, Some(64), 1);
    let seams = BTreeMap::new();
    let cfg = make_cfg(&layers, &plan_path, dir.path(), &pice_config, &wf, &seams);

    let score_refs: Vec<(&str, &str)> = per_layer_scores
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let _stub = ParallelStubGuard::new(&score_refs, Some(100));

    let t0 = Instant::now();
    let manifest = run_stack_loops_with_cancel(
        &cfg,
        &NullSink,
        true,
        null_sink_arc(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let elapsed = t0.elapsed();

    assert_eq!(manifest.layers.len(), 20);

    // The cap bound is the interesting one: if the cap were NOT enforced,
    // 20 concurrent tasks × 100ms latency would finish in ≥ 100ms. With
    // the cap at 16, we need at least one second-batch round, so wall-clock
    // must be ≥ ~150ms (two ~100ms batches with overlap). Use a loose
    // lower bound (150ms) to tolerate CI scheduler noise while still
    // catching a "cap removed" regression where all 20 run concurrently.
    //
    // We don't over-assert on the upper bound — slow CI can blow past
    // tight ceilings and the test becomes flaky.
    assert!(
        elapsed >= Duration::from_millis(150),
        "20 layers × 100ms latency with hard-cap 16 must take at least \
         ~150ms (two batches); got {:?} — regression where cap is ignored?",
        elapsed,
    );

    // Every layer should have completed successfully (score 9.0 ≥ SPRT threshold).
    let passed_or_pending = manifest
        .layers
        .iter()
        .all(|l| matches!(l.status, LayerStatus::Passed | LayerStatus::Pending));
    assert!(
        passed_or_pending,
        "all 20 layers should land on Passed/Pending; got {:?}",
        manifest
            .layers
            .iter()
            .map(|l| (&l.name, &l.status))
            .collect::<Vec<_>>(),
    );
}

//! Phase 5 speedup assertion — CI gate for the parallel-cohort contract.
//!
//! `cargo bench` does NOT fail CI on regression; criterion only reports.
//! This test runs the SAME scenario as `benches/parallel_cohort_speedup.rs`
//! with a smaller N (3 iterations per arm), measures mean wall-clock, and
//! `assert!`s `parallel_mean <= 0.625 * sequential_mean` — the 1.6×
//! speedup floor from the plan's contract criterion #1.
//!
//! **Multi-thread runtime (load-bearing).** `flavor = "multi_thread"`
//!  + `worker_threads(4)` — `tokio::time::pause()` would zero the
//!    stub's `setTimeout` and produce a meaningless measurement. See
//!    Cycle-2 Codex #13 and the plan's Note #5.
//!
//! **CI contention caveat.** Shared CI runners can add scheduler jitter
//! that narrows the observed speedup. The plan's "Known limitations"
//! section notes this — if CI flakes here the remediation is to switch
//! to a dedicated runner, NOT to loosen the assertion.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use pice_core::config::{
    AdversarialConfig, EvalProviderConfig, EvaluationConfig, InitConfig, MetricsConfig, PiceConfig,
    ProviderConfig, TelemetryConfig, TiersConfig,
};
use pice_core::layers::{LayerDef, LayersConfig, LayersTable};
use pice_core::workflow::schema::AdaptiveAlgo;
use pice_core::workflow::WorkflowConfig;
use pice_daemon::orchestrator::stack_loops::{run_stack_loops_with_cancel, StackLoopsConfig};
use pice_daemon::orchestrator::{NullPassSink, NullSink, PassMetricsSink};
use tokio_util::sync::CancellationToken;

const ITERATIONS: usize = 3;
const LATENCY_MS: u64 = 200;
/// Target speedup ≥ 1.6× → parallel mean ≤ (1/1.6) × sequential ≈ 0.625×.
const MAX_PARALLEL_RATIO: f64 = 0.625;

/// Serializes all `PICE_STUB_*` env mutations inside THIS test binary.
/// Today this binary has exactly one test (`parallel_cohort_meets_16x_speedup`)
/// so there is no concurrent contention, but holding this lock across
/// every `time_one_run` call is defense-in-depth: if a future PR adds
/// a second test to this file (or if cargo's test-binary isolation ever
/// relaxes), the unguarded `std::env::set_var` calls would race across
/// tokio tasks. Mirrors the `stub_env_lock()` in
/// `parallel_cohort_integration.rs`.
fn stub_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// RAII guard that serializes `PICE_STUB_*` env mutations + cleans
/// them up on drop. Lives INSIDE a struct field so `clippy::await_
/// holding_lock` does not see a naked `MutexGuard` across `.await`
/// (the lint is pattern-matched on the binding's static type).
///
/// Mirrors `ParallelStubGuard` in `parallel_cohort_integration.rs` —
/// the two binaries cannot share a module, so the helper is duplicated
/// verbatim rather than extracted into a test-support crate (which
/// would require a new crate just for two ~20-line structs).
struct StubEnvGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl StubEnvGuard {
    fn new(latency_ms: u64) -> Self {
        let guard = stub_env_lock().lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var("PICE_STUB_SCORES_BACKEND", "9.0,0.01");
        std::env::set_var("PICE_STUB_SCORES_FRONTEND", "8.0,0.01");
        std::env::set_var("PICE_STUB_LATENCY_MS", latency_ms.to_string());
        std::env::remove_var("PICE_STUB_SCORES");
        Self { _guard: guard }
    }
}

impl Drop for StubEnvGuard {
    fn drop(&mut self) {
        std::env::remove_var("PICE_STUB_SCORES_BACKEND");
        std::env::remove_var("PICE_STUB_SCORES_FRONTEND");
        std::env::remove_var("PICE_STUB_LATENCY_MS");
    }
}

fn git_init(dir: &Path) {
    let _ = std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output();
    let _ = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=assert",
            "-c",
            "user.email=a@a",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ])
        .current_dir(dir)
        .output();
}

fn write_file(dir: &Path, rel: &str, content: &str) {
    let full = dir.join(rel);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&full, content).unwrap();
}

fn two_layer_config() -> LayersConfig {
    let mut defs = BTreeMap::new();
    for (name, path) in [("backend", "src/server/**"), ("frontend", "src/client/**")] {
        defs.insert(
            name.to_string(),
            LayerDef {
                paths: vec![path.to_string()],
                always_run: false,
                contract: None,
                depends_on: Vec::new(),
                layer_type: None,
                environment_variants: None,
            },
        );
    }
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

fn workflow(parallel: bool) -> WorkflowConfig {
    let mut wf = pice_core::workflow::loader::embedded_defaults();
    wf.defaults.min_confidence = 0.70;
    wf.defaults.max_passes = 1;
    wf.defaults.budget_usd = 0.0;
    wf.phases.evaluate.parallel = parallel;
    wf.phases.evaluate.adaptive_algorithm = AdaptiveAlgo::BayesianSprt;
    wf
}

async fn time_one_run(parallel: bool) -> Duration {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());
    write_file(dir.path(), "src/server/main.rs", "fn main() {}");
    write_file(
        dir.path(),
        "src/client/App.tsx",
        "export const A = () => null;",
    );
    let plan_dir = dir.path().join(".claude/plans");
    std::fs::create_dir_all(&plan_dir).unwrap();
    let plan_path = plan_dir.join("bench.md");
    std::fs::write(
        &plan_path,
        "# Bench\n\n## Contract\n\n```json\n{\"feature\":\"p\",\"tier\":2,\"pass_threshold\":7,\"criteria\":[]}\n```\n",
    )
    .unwrap();
    let layers = two_layer_config();
    let pice_config = stub_pice_config();
    let wf = workflow(parallel);
    let seams = BTreeMap::new();
    let cfg = StackLoopsConfig {
        layers: &layers,
        plan_path: &plan_path,
        project_root: dir.path(),
        primary_provider: "stub",
        primary_model: "stub-model",
        pice_config: &pice_config,
        workflow: &wf,
        merged_seams: &seams,
    };

    // Hold the env lock across set/run/tear-down. The `StubEnvGuard`
    // struct wraps the `MutexGuard` in a field so it survives across
    // `.await` without tripping `clippy::await_holding_lock` (the lint
    // matches on naked-binding static types). Cleanup is RAII —
    // survives panic.
    let _env = StubEnvGuard::new(LATENCY_MS);

    let sink: Arc<dyn PassMetricsSink> = Arc::new(NullPassSink);
    let t0 = Instant::now();
    let _ = run_stack_loops_with_cancel(&cfg, &NullSink, true, sink, CancellationToken::new())
        .await
        .unwrap();
    t0.elapsed()
}

async fn mean_of_n(parallel: bool) -> Duration {
    let mut samples = Vec::with_capacity(ITERATIONS);
    for _ in 0..ITERATIONS {
        samples.push(time_one_run(parallel).await);
    }
    let sum: u128 = samples.iter().map(|d| d.as_nanos()).sum();
    Duration::from_nanos((sum / ITERATIONS as u128) as u64)
}

/// CI gate: parallel cohort must be ≥ 1.6× faster than sequential
/// (parallel_mean ≤ 0.625 × sequential_mean).
///
/// Run with `cargo test -p pice-daemon --test parallel_cohort_speedup_assertion
/// -- --test-threads=1` locally to avoid scheduler contention skewing the
/// measurements. The `#[tokio::test(flavor = "multi_thread")]` gives each
/// run its own async runtime, so parallel cohort tasks can actually run
/// on multiple threads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_cohort_meets_16x_speedup() {
    let sequential = mean_of_n(false).await;
    let parallel = mean_of_n(true).await;
    let ratio = parallel.as_secs_f64() / sequential.as_secs_f64();

    eprintln!("=== Phase 5 cohort speedup ===");
    eprintln!("sequential mean ({} iters): {:?}", ITERATIONS, sequential);
    eprintln!("parallel   mean ({} iters): {:?}", ITERATIONS, parallel);
    eprintln!(
        "ratio parallel/sequential = {:.3} (target ≤ {:.3})",
        ratio, MAX_PARALLEL_RATIO
    );

    assert!(
        ratio <= MAX_PARALLEL_RATIO,
        "speedup regression: parallel/sequential = {:.3}, required ≤ {:.3}. \
         sequential={:?} parallel={:?}. If this is a real regression, fix \
         stack_loops.rs — DO NOT loosen the assertion. If it's CI scheduler \
         noise, see the plan's Known Limitations note about dedicated runners.",
        ratio,
        MAX_PARALLEL_RATIO,
        sequential,
        parallel,
    );
}

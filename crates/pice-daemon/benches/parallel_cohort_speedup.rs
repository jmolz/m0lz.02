//! Criterion bench: sequential vs parallel cohort wall-clock.
//!
//! Fixture: two independent layers (`backend`, `frontend`) with no
//! `depends_on` edge. Each layer runs one adaptive pass; the stub
//! provider sleeps `PICE_STUB_LATENCY_MS=200` per score response, so
//! wall-clock is dominated by the sleep — exactly the measurement
//! surface `parallel_cohort_integration.rs`'s gate tests can't
//! reliably exercise (they use tight timings suitable for correctness,
//! not perf).
//!
//! **Multi-thread runtime (load-bearing).** `tokio::test(flavor =
//! "multi_thread")` + a real `Runtime::new_multi_thread()` here —
//! `tokio::time::pause()` on the Rust side would silently zero the
//! stub's `setTimeout`, making the bench measure nothing. This is
//! documented in Cycle-2 Codex finding #13 and in the plan's Note #5.
//!
//! **CI gating.** `cargo bench` does NOT fail CI on regression —
//! criterion only reports. The companion dedicated assertion test at
//! `tests/parallel_cohort_speedup_assertion.rs` is what actually gates
//! CI. This file produces the human-readable report for inspection.
//!
//! Run: `cargo bench -p pice-daemon --bench parallel_cohort_speedup`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
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

fn git_init(dir: &Path) {
    let _ = std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output();
    let _ = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=bench",
            "-c",
            "user.email=b@b",
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

fn bench_workflow(parallel: bool) -> WorkflowConfig {
    let mut wf = pice_core::workflow::loader::embedded_defaults();
    wf.defaults.min_confidence = 0.70;
    wf.defaults.max_passes = 1; // single-pass — latency dominates
    wf.defaults.budget_usd = 0.0;
    wf.phases.evaluate.parallel = parallel;
    wf.phases.evaluate.adaptive_algorithm = AdaptiveAlgo::BayesianSprt;
    wf
}

async fn run_once(parallel: bool, latency_ms: u64) {
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
    let wf = bench_workflow(parallel);
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

    // Env setup — serialized by the caller (criterion runs benches
    // sequentially within one process, so there is no race between
    // iterations).
    std::env::set_var("PICE_STUB_SCORES_BACKEND", "9.0,0.01");
    std::env::set_var("PICE_STUB_SCORES_FRONTEND", "8.0,0.01");
    std::env::set_var("PICE_STUB_LATENCY_MS", latency_ms.to_string());
    std::env::remove_var("PICE_STUB_SCORES");

    let sink: Arc<dyn PassMetricsSink> = Arc::new(NullPassSink);
    let _ = run_stack_loops_with_cancel(&cfg, &NullSink, true, sink, CancellationToken::new())
        .await
        .unwrap();

    std::env::remove_var("PICE_STUB_SCORES_BACKEND");
    std::env::remove_var("PICE_STUB_SCORES_FRONTEND");
    std::env::remove_var("PICE_STUB_LATENCY_MS");
}

fn bench_cohort_speedup(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("multi-thread runtime for bench — pause()d runtime would zero stub latency");

    let mut group = c.benchmark_group("cohort_speedup");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    for parallel in [false, true] {
        let label = if parallel { "parallel" } else { "sequential" };
        group.bench_with_input(BenchmarkId::from_parameter(label), &parallel, |b, &par| {
            b.to_async(&rt).iter(|| run_once(par, 200));
        });
    }

    group.finish();
}

criterion_group!(benches, bench_cohort_speedup);
criterion_main!(benches);

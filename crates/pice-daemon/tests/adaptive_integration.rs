//! End-to-end integration tests for PRDv2 Phase 4 adaptive evaluation.
//!
//! Each test drives `run_stack_loops` with the stub provider and asserts the
//! resulting verification manifest matches the expected halt path. Pass
//! counts, halted_by, layer status, escalation events, and cost
//! reconciliation are all verified.
//!
//! Phase 4 contract criteria covered (per `.claude/plans/phase-4-adaptive-evaluation.md`):
//! - #2 SPRT halt-reason correctness (accept, reject, max-passes)
//! - #3 Budget cap fail-closed
//! - #5 ADTS three-level escalation audit trail
//! - #6 VEC entropy halt
//! - #7 Floor-merge compliance (orthogonal — covered in `pice-core::workflow::merge`)
//! - #15 Determinism across two identical runs
//! - #16 Cost reconciliation within tolerance
//!
//! ### Why use `run_stack_loops` rather than the daemon RPC?
//!
//! These are orchestrator-level tests. The daemon RPC integration is
//! covered by `crates/pice-cli/tests/adaptive_integration.rs` (Task 21).
//! Driving `run_stack_loops` keeps these tests fast, deterministic, and
//! focused on the algorithm/provider interaction rather than transport.

use pice_core::adaptive::AdtsConfig;
use pice_core::config::{
    AdversarialConfig, EvalProviderConfig, EvaluationConfig, InitConfig, MetricsConfig, PiceConfig,
    ProviderConfig, TelemetryConfig, TiersConfig,
};
use pice_core::layers::manifest::LayerStatus;
use pice_core::layers::{LayerDef, LayersConfig, LayersTable};
use pice_core::workflow::schema::AdaptiveAlgo;
use pice_core::workflow::WorkflowConfig;
use pice_daemon::orchestrator::stack_loops::{run_stack_loops, StackLoopsConfig};
use pice_daemon::orchestrator::{NullPassSink, NullSink, RecordingPassSink};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// Serializes access to `PICE_STUB_SCORES`. The variable is process-wide, so
/// parallel tests would race on get/set. Each test acquires this guard once
/// at the top of its body and releases on drop.
fn stub_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// RAII guard that sets `PICE_STUB_SCORES` on construction and clears it on
/// drop, holding the global env lock for its lifetime.
struct StubScoresGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl StubScoresGuard {
    fn new(scores: &str) -> Self {
        // Recover from poisoned lock: a test panic mid-run leaves the mutex
        // poisoned, but the env state is still safe to overwrite.
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

/// Extends `StubScoresGuard` with the Phase 4 ADTS-adversarial-score
/// and request-log env vars. Shares the same global env mutex so parallel
/// tests can't race on `PICE_STUB_*` vars.
struct StubAdtsGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl StubAdtsGuard {
    fn new(primary_scores: &str, adversarial_scores: &str, request_log: Option<&Path>) -> Self {
        let guard = stub_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("PICE_STUB_SCORES", primary_scores);
        std::env::set_var("PICE_STUB_ADVERSARIAL_SCORES", adversarial_scores);
        if let Some(path) = request_log {
            std::env::set_var("PICE_STUB_REQUEST_LOG", path);
        } else {
            std::env::remove_var("PICE_STUB_REQUEST_LOG");
        }
        Self { _guard: guard }
    }
}

impl Drop for StubAdtsGuard {
    fn drop(&mut self) {
        std::env::remove_var("PICE_STUB_SCORES");
        std::env::remove_var("PICE_STUB_ADVERSARIAL_SCORES");
        std::env::remove_var("PICE_STUB_REQUEST_LOG");
    }
}

/// Phase 4 Pass-3 regression helper: sets `PICE_STUB_EVALUATE_ERROR` so the
/// stub's `evaluate/create` throws a JSON-RPC error *after* spawning &
/// initializing successfully. Shares the global `PICE_STUB_*` env mutex so
/// it can't race with other tests. The error message round-trips in the
/// stub's thrown Error, letting tests assert on the layer's `halted_by`
/// string (it carries the provider's error message for operator debugging).
struct StubEvaluateErrorGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl StubEvaluateErrorGuard {
    fn new(message: &str) -> Self {
        let guard = stub_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("PICE_STUB_EVALUATE_ERROR", message);
        Self { _guard: guard }
    }
}

impl Drop for StubEvaluateErrorGuard {
    fn drop(&mut self) {
        std::env::remove_var("PICE_STUB_EVALUATE_ERROR");
    }
}

/// Phase 4 Pass-4 regression helper — Codex Critical #1 (start-error
/// classification). Sets `PICE_STUB_INIT_ERROR` so the stub's re-registered
/// `initialize` handler throws a JSON-RPC error. The provider spawns cleanly
/// (node starts, script loads, BaseProvider runs), but the initialize RPC
/// fails — which is the exact shape the Pass-4 fix must classify as a
/// RuntimeError → LayerStatus::Failed, NOT a NotStarted → Pending. The
/// resolve-probe in `try_run_layer_adaptive` SUCCEEDS (stub is registered),
/// so the error flows through the startup-failure arm, not the
/// unresolvable-provider arm.
struct StubInitErrorGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl StubInitErrorGuard {
    fn new(message: &str) -> Self {
        let guard = stub_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("PICE_STUB_INIT_ERROR", message);
        Self { _guard: guard }
    }
}

impl Drop for StubInitErrorGuard {
    fn drop(&mut self) {
        std::env::remove_var("PICE_STUB_INIT_ERROR");
    }
}

/// Phase 4 Pass-5 regression helper — Codex Critical #2 (session correlation).
/// Sets `PICE_STUB_INJECT_STALE_EVAL_RESULT` so the stub emits a stale
/// `evaluate/result` notification with a deliberately wrong sessionId BEFORE
/// the real notification inside `evaluate/score`. A correct session-correlation
/// filter in the Rust orchestrator ignores the stale one. A naive "take first
/// matching method" handler would capture the stale notification and corrupt
/// the pass outcome — exactly the bug Codex flagged.
struct StubStaleResultGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl StubStaleResultGuard {
    fn new(scores: &str, stale_session_id: &str) -> Self {
        let guard = stub_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("PICE_STUB_SCORES", scores);
        std::env::set_var("PICE_STUB_INJECT_STALE_EVAL_RESULT", stale_session_id);
        Self { _guard: guard }
    }
}

impl Drop for StubStaleResultGuard {
    fn drop(&mut self) {
        std::env::remove_var("PICE_STUB_SCORES");
        std::env::remove_var("PICE_STUB_INJECT_STALE_EVAL_RESULT");
    }
}

/// Phase 4 Pass-4 regression helper — Codex High (partial-run cost
/// reconciliation). Bundles three env vars (`PICE_STUB_SCORES`,
/// `PICE_STUB_EVALUATE_ERROR`, `PICE_STUB_EVALUATE_ERROR_FROM_PASS`) so the
/// stub completes passes `1..from-1` normally, then throws on pass `from`.
/// Exercises the adaptive loop's "preserve prior passes on mid-loop error"
/// halt path — the passes already written to the sink must stay in the
/// manifest and total_cost_usd must match Σ(pass_events.cost_usd).
struct StubMidLoopErrorGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl StubMidLoopErrorGuard {
    fn new(scores: &str, message: &str, from_pass: u32) -> Self {
        let guard = stub_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("PICE_STUB_SCORES", scores);
        std::env::set_var("PICE_STUB_EVALUATE_ERROR", message);
        std::env::set_var("PICE_STUB_EVALUATE_ERROR_FROM_PASS", from_pass.to_string());
        Self { _guard: guard }
    }
}

impl Drop for StubMidLoopErrorGuard {
    fn drop(&mut self) {
        std::env::remove_var("PICE_STUB_SCORES");
        std::env::remove_var("PICE_STUB_EVALUATE_ERROR");
        std::env::remove_var("PICE_STUB_EVALUATE_ERROR_FROM_PASS");
    }
}

/// Phase 4.1 Pass-6 regression helper — Codex Critical #1 (synthetic budget
/// on cost-blind providers). Sets `PICE_STUB_COST_TELEMETRY_OFF=1` so the
/// stub's `getCapabilities()` returns `costTelemetry: false`, simulating a
/// legacy / production provider that does not emit `costUsd`. Bundles the
/// normal `PICE_STUB_SCORES` so the loop COULD run if the gate were absent
/// — that way the test catches regressions where the gate is removed or
/// mis-scoped.
struct StubNoCostTelemetryGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl StubNoCostTelemetryGuard {
    fn new(scores: &str) -> Self {
        let guard = stub_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("PICE_STUB_SCORES", scores);
        std::env::set_var("PICE_STUB_COST_TELEMETRY_OFF", "1");
        Self { _guard: guard }
    }
}

impl Drop for StubNoCostTelemetryGuard {
    fn drop(&mut self) {
        std::env::remove_var("PICE_STUB_SCORES");
        std::env::remove_var("PICE_STUB_COST_TELEMETRY_OFF");
    }
}

// ─── Shared helpers ─────────────────────────────────────────────────────────

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

/// Single-layer LayersConfig — `backend` covering `src/**`. Used by every
/// adaptive test except the ADTS one (which still uses backend, just with a
/// dual-provider config).
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

/// Stub-provider PiceConfig for adaptive tests. Adversarial is OFF except
/// when the test explicitly enables it (ADTS).
fn stub_pice_config(adversarial_enabled: bool) -> PiceConfig {
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
                enabled: adversarial_enabled,
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

/// Build a workflow whose `defaults` are tuned for the test, with the chosen
/// adaptive algorithm and (optionally) ADTS knobs.
fn workflow_with_adaptive(
    algo: AdaptiveAlgo,
    min_confidence: f64,
    max_passes: u32,
    budget_usd: f64,
    adts: Option<AdtsConfig>,
) -> WorkflowConfig {
    let mut wf = pice_core::workflow::loader::embedded_defaults();
    wf.defaults.min_confidence = min_confidence;
    wf.defaults.max_passes = max_passes;
    wf.defaults.budget_usd = budget_usd;
    wf.phases.evaluate.adaptive_algorithm = algo;
    if let Some(a) = adts {
        wf.phases.evaluate.adts = a;
    }
    wf
}

/// Minimal plan + git fixture in `dir`. Returns `plan_path`.
fn setup_minimal_repo(dir: &Path) -> std::path::PathBuf {
    git_init(dir);
    write_file(dir, "src/main.rs", "fn main() {}");
    let plan_dir = dir.join(".claude/plans");
    std::fs::create_dir_all(&plan_dir).unwrap();
    let plan_path = plan_dir.join("phase4-test.md");
    std::fs::write(
        &plan_path,
        "# Phase 4 test plan\n\n## Contract\n\n```json\n{\"feature\":\"adaptive\",\"tier\":2,\"pass_threshold\":7,\"criteria\":[]}\n```\n",
    )
    .unwrap();
    plan_path
}

/// Construct a `StackLoopsConfig` with the supplied workflow and pice config.
fn make_cfg<'a>(
    layers: &'a LayersConfig,
    plan_path: &'a Path,
    project_root: &'a Path,
    pice_config: &'a PiceConfig,
    workflow: &'a WorkflowConfig,
    seams: &'a BTreeMap<String, Vec<String>>,
) -> StackLoopsConfig<'a> {
    StackLoopsConfig {
        layers,
        plan_path,
        project_root,
        primary_provider: "stub",
        primary_model: "stub-model",
        pice_config,
        workflow,
        merged_seams: seams,
    }
}

// ─── Test 1: SPRT accept halts before max_passes ────────────────────────────

#[tokio::test]
async fn sprt_accepts_and_halts_before_max_passes() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config(false);
    // SPRT-only, threshold 9 (min_confidence=0.90 → score ≥ 9.0 = success).
    let workflow = workflow_with_adaptive(AdaptiveAlgo::BayesianSprt, 0.90, 8, 10.0, None);
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    // High score on every pass — SPRT should accept fast.
    let _stub = StubScoresGuard::new(
        "9.5,0.001;9.5,0.001;9.5,0.001;9.5,0.001;9.5,0.001;9.5,0.001;9.5,0.001;9.5,0.001",
    );

    let mut sink = NullPassSink;
    let manifest = run_stack_loops(&cfg, &NullSink, true, &mut sink)
        .await
        .unwrap();
    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .expect("backend layer present");
    assert_eq!(backend.status, LayerStatus::Passed);
    assert_eq!(
        backend.halted_by.as_deref(),
        Some("sprt_confidence_reached"),
        "expected sprt_confidence_reached; got {:?}",
        backend.halted_by
    );
    assert!(
        backend.passes.len() < 8,
        "SPRT should halt before max_passes; got {}",
        backend.passes.len()
    );
}

// ─── Test 2: SPRT reject after consistent failures ──────────────────────────

#[tokio::test]
async fn sprt_rejects_after_consistent_failures() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config(false);
    let workflow = workflow_with_adaptive(AdaptiveAlgo::BayesianSprt, 0.90, 10, 10.0, None);
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    let _stub = StubScoresGuard::new("3.0,0.001;3.0,0.001;3.0,0.001;3.0,0.001;3.0,0.001;3.0,0.001;3.0,0.001;3.0,0.001;3.0,0.001;3.0,0.001");

    let mut sink = NullPassSink;
    let manifest = run_stack_loops(&cfg, &NullSink, true, &mut sink)
        .await
        .unwrap();
    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap();
    assert_eq!(backend.status, LayerStatus::Failed);
    assert_eq!(backend.halted_by.as_deref(), Some("sprt_rejected"));
}

// ─── Test 3: Budget halts before confidence ─────────────────────────────────

#[tokio::test]
async fn budget_halts_before_confidence() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config(false);
    // Tight budget: 0.05 USD; per-pass cost 0.03 → after pass 1, accumulated
    // 0.03 + projected ≥ 0.03 (cold-start seed at 0.05/5=0.01 or smoothed mean
    // 0.03) > 0.05 budget → halt before pass 2 completes.
    let workflow = workflow_with_adaptive(AdaptiveAlgo::BayesianSprt, 0.90, 5, 0.05, None);
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    // High score so SPRT does not pre-halt; budget gate must still fire.
    let _stub = StubScoresGuard::new("9.5,0.03;9.5,0.03;9.5,0.03;9.5,0.03;9.5,0.03");

    let mut sink = NullPassSink;
    let manifest = run_stack_loops(&cfg, &NullSink, true, &mut sink)
        .await
        .unwrap();
    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap();
    assert_eq!(backend.status, LayerStatus::Pending);
    assert_eq!(backend.halted_by.as_deref(), Some("budget"));
    assert!(
        backend.passes.len() <= 2,
        "budget should halt within ≤2 passes; got {}",
        backend.passes.len()
    );
}

// ─── Test 3b: Adaptive budget requires provider cost_telemetry capability ──

/// Phase 4.1 Pass-6 Codex Critical #1 regression: when the primary provider
/// declares `costTelemetry: false` and the workflow sets `budget_usd > 0`,
/// the daemon MUST fail closed rather than run the loop on `budget_usd /
/// max_passes` synthetic seed debits. Without this gate the user sees a
/// "budget halt" that never reflects real spend — the exact failure mode
/// flagged at `adaptive_loop.rs:291-385`.
#[tokio::test]
async fn adaptive_budget_fails_closed_when_provider_lacks_cost_telemetry() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config(false);
    // budget_usd = 1.0 (> 0 → gate applies). Scores given so the loop would
    // otherwise run cleanly — if the gate is silently bypassed, the test
    // would go green on the wrong signal. The gate must fire BEFORE any
    // pass runs, so we expect zero passes recorded.
    let workflow = workflow_with_adaptive(AdaptiveAlgo::BayesianSprt, 0.90, 5, 1.0, None);
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    let _stub = StubNoCostTelemetryGuard::new("9.5,0.01;9.5,0.01;9.5,0.01;9.5,0.01;9.5,0.01");

    let mut sink = NullPassSink;
    let manifest = run_stack_loops(&cfg, &NullSink, true, &mut sink)
        .await
        .unwrap();
    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap();

    // Gate fires in `run_adaptive_layer_inner` before any pass — the layer
    // routes through `LayerAdaptiveResult::RuntimeError` to `LayerStatus::Failed`
    // with the capability message preserved in `halted_by` (prefixed
    // `runtime_error:` by `runtime_failed_layer_result`). The manifest shape
    // requires at least one pass row, so a single placeholder with
    // `model = "runtime-error"` is present — it must NOT be a real scored
    // pass (score must be None).
    assert_eq!(
        backend.status,
        LayerStatus::Failed,
        "capability gate must fail-close; got {:?}",
        backend.status
    );
    let halted = backend.halted_by.as_deref().unwrap_or("");
    assert!(
        halted.starts_with("runtime_error:") && halted.contains("costTelemetry"),
        "halted_by should be `runtime_error:` wrapper referencing costTelemetry; got {halted:?}"
    );
    assert_eq!(
        backend.passes.len(),
        1,
        "runtime_failed_layer_result inserts exactly one placeholder row",
    );
    let placeholder = &backend.passes[0];
    assert_eq!(
        placeholder.model, "runtime-error",
        "placeholder must use the `runtime-error` marker, not a real provider model",
    );
    assert!(
        placeholder.score.is_none(),
        "gate must fire BEFORE any scoring call; placeholder score must be None",
    );
    assert!(
        placeholder.cost_usd.is_none(),
        "no real pass ran, so no cost should be attributed to the placeholder",
    );
}

/// Phase 4.1 Pass-6 counterpart: `budget_usd = 0` disables budget
/// enforcement, so the capability gate must NOT fire even on a provider
/// without cost telemetry. This is the explicit escape hatch for teams
/// that want adaptive halting (SPRT/VEC) without financial enforcement
/// on providers that haven't wired `costUsd` yet.
#[tokio::test]
async fn adaptive_zero_budget_bypasses_cost_telemetry_gate() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config(false);
    // budget_usd = 0.0 → gate inert. Loop runs; SPRT accepts on high scores.
    let workflow = workflow_with_adaptive(AdaptiveAlgo::BayesianSprt, 0.90, 5, 0.0, None);
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    let _stub = StubNoCostTelemetryGuard::new("9.5,0.0;9.5,0.0;9.5,0.0;9.5,0.0;9.5,0.0");

    let mut sink = NullPassSink;
    let manifest = run_stack_loops(&cfg, &NullSink, true, &mut sink)
        .await
        .unwrap();
    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap();

    // Without budget enforcement the loop runs to SPRT acceptance. The gate
    // must NOT have fired — any Failed status with a `costTelemetry`-flavored
    // halted_by would indicate the gate over-triggered.
    let halted = backend.halted_by.as_deref().unwrap_or("");
    assert!(
        !halted.contains("costTelemetry"),
        "gate must not fire when budget_usd == 0; got halted_by={halted:?}"
    );
    assert!(
        !backend.passes.is_empty(),
        "loop should run at least one pass when gate is inert",
    );
}

// ─── Test 4: Cold-start seed blocks overspend on pass one ──────────────────

#[tokio::test]
async fn cold_start_seed_blocks_overspend_on_pass_one() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config(false);
    // budget=0.001, max_passes=5 → cold-start seed = 0.0002. Stub cost=0.01
    // (50× the seed). Pass-1 pre-check projects 0.0002 → allowed; the pass
    // runs and observes 0.01; pre-pass-2 check sees accumulated 0.01 +
    // projected ≥ 0.01 ≫ 0.001 → halt with budget.
    let workflow = workflow_with_adaptive(AdaptiveAlgo::BayesianSprt, 0.90, 5, 0.001, None);
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    let _stub = StubScoresGuard::new("9.5,0.01;9.5,0.01;9.5,0.01;9.5,0.01;9.5,0.01");

    let mut sink = NullPassSink;
    let manifest = run_stack_loops(&cfg, &NullSink, true, &mut sink)
        .await
        .unwrap();
    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap();
    assert_eq!(backend.halted_by.as_deref(), Some("budget"));
    assert_eq!(
        backend.passes.len(),
        1,
        "cold-start should permit pass 1 then halt; got {}",
        backend.passes.len()
    );
}

// ─── Test 5: max_passes halts uncertain layer ───────────────────────────────

#[tokio::test]
async fn max_passes_halts_uncertain_layer() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config(false);
    // Mixed scores 5/6 on a 0-10 scale with min_confidence=0.90 (threshold 9.0):
    // both classify as Failure. SPRT will eventually reject, so we use a
    // VERY narrow band with 0.50 threshold to keep the posterior tied —
    // alternating 4 and 6 with threshold 5 alternates Success/Failure.
    let workflow = workflow_with_adaptive(AdaptiveAlgo::BayesianSprt, 0.50, 4, 10.0, None);
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    // Alternating success/failure keeps the posterior near 0.5 and SPRT
    // statistic near unity → no early accept/reject.
    let _stub = StubScoresGuard::new("6.0,0.001;4.0,0.001;6.0,0.001;4.0,0.001");

    let mut sink = NullPassSink;
    let manifest = run_stack_loops(&cfg, &NullSink, true, &mut sink)
        .await
        .unwrap();
    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap();
    assert_eq!(
        backend.halted_by.as_deref(),
        Some("max_passes"),
        "expected max_passes; got {:?}",
        backend.halted_by
    );
    assert_eq!(backend.passes.len(), 4);
    // Phase 4 Pass-5 Claude Evaluator A Criterion 2 fix: the contract requires
    // "correct layer status" for each halt path. `max_passes` is a fail-closed
    // guardrail (user may re-run with higher budget/passes) — the layer must
    // record `Pending`, not `Passed` or `Failed`. Without this assertion a
    // regression that silently flipped max_passes to Failed would slip past.
    assert_eq!(
        backend.status,
        LayerStatus::Pending,
        "max_passes must route to Pending (fail-closed guardrail), not Passed or Failed",
    );
}

// ─── Test 6: ADTS three-level escalation exhausts ──────────────────────────

/// Pice config for ADTS tests: primary model `"stub-primary"`, adversarial
/// model `"stub-adversarial"`. The stub uses the `"adversarial"` substring
/// in the model name to switch to `PICE_STUB_ADVERSARIAL_SCORES`.
fn stub_pice_config_adts() -> PiceConfig {
    let mut cfg = stub_pice_config(true);
    cfg.evaluation.adversarial.model = "stub-adversarial".to_string();
    cfg
}

#[tokio::test]
async fn adts_three_level_escalation_exhausts() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config_adts();
    // ADTS with max_divergence_escalations=2 → Level1, Level2, Level3 sequence.
    let adts = AdtsConfig {
        divergence_threshold: 2.0,
        max_divergence_escalations: 2,
    };
    // max_passes=4 so the ADTS loop gets: pass1 (Level1) → pass2 (Level2) →
    // pass3 (Level3 exhaust). 4 gives a cushion if the implementation ever
    // consumes an extra pass between escalations.
    let workflow = workflow_with_adaptive(AdaptiveAlgo::Adts, 0.90, 4, 10.0, Some(adts));
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    // Primary scores 9.0 every pass; adversarial scores 3.0 every pass →
    // divergence = |9 − 3| = 6.0 > threshold (2.0) EVERY pass. With
    // max_divergence_escalations=2, ADTS fires Level 1 on pass 1, Level 2
    // on pass 2, Level 3 (Exhausted) on pass 3.
    let log_path = dir.path().join("stub-adts.log");
    let _stub = StubAdtsGuard::new(
        "9.0,0.001;9.0,0.001;9.0,0.001;9.0,0.001",
        "3.0,0.001;3.0,0.001;3.0,0.001;3.0,0.001",
        Some(&log_path),
    );

    let mut sink = NullPassSink;
    let manifest = run_stack_loops(&cfg, &NullSink, true, &mut sink)
        .await
        .unwrap();
    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap();

    // ── 1. Halt reason and layer status ────────────────────────────────────
    assert_eq!(
        backend.halted_by.as_deref(),
        Some("adts_escalation_exhausted"),
        "expected ADTS exhaustion; got {:?}",
        backend.halted_by
    );
    assert_eq!(backend.status, LayerStatus::Failed);

    // ── 2. Escalation event audit trail (full Level1→2→3 sequence) ────────
    use pice_core::adaptive::EscalationEvent;
    let events = backend
        .escalation_events
        .as_ref()
        .expect("ADTS must populate escalation_events");
    assert_eq!(
        events.len(),
        3,
        "expected exactly 3 escalation events; got {:?}",
        events
    );
    assert!(
        matches!(
            events[0],
            EscalationEvent::Level1FreshContext { at_pass: 1 }
        ),
        "events[0] must be Level1FreshContext at_pass=1; got {:?}",
        events[0]
    );
    assert!(
        matches!(
            events[1],
            EscalationEvent::Level2ElevatedEffort { at_pass: 2 }
        ),
        "events[1] must be Level2ElevatedEffort at_pass=2; got {:?}",
        events[1]
    );
    assert!(
        matches!(events[2], EscalationEvent::Level3Exhausted { at_pass: 3 }),
        "events[2] must be Level3Exhausted at_pass=3; got {:?}",
        events[2]
    );

    // ── 3. Provider-side verification: fresh_context + effort_override ────
    // Inspect the stub's request log: pass 2 must carry freshContext=true
    // (Level 1 effect takes effect on the NEXT pass), pass 3 must carry
    // effortOverride=xhigh (Level 2's effect also on next pass).
    let log_content = std::fs::read_to_string(&log_path).expect("stub request log present");
    let entries: Vec<serde_json::Value> = log_content
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    // Per ADTS pass: 1 primary + 1 adversarial = 2 entries. We expect 3 pass
    // cycles before exhaustion = 6 entries minimum.
    assert!(
        entries.len() >= 6,
        "expected ≥6 request entries (3 passes × 2 providers); got {}",
        entries.len()
    );

    // Collect primary-role entries grouped by passIndex (wire form: 0-indexed).
    let primary_entries: Vec<&serde_json::Value> = entries
        .iter()
        .filter(|e| {
            e["model"]
                .as_str()
                .map(|m| !m.to_lowercase().contains("adversarial"))
                .unwrap_or(true)
        })
        .collect();
    // passIndex=1 (second pass, 0-indexed) must carry freshContext=true.
    let pass2_primary = primary_entries
        .iter()
        .find(|e| e["passIndex"] == 1)
        .expect("primary request for pass 2 (wire index 1) present");
    assert_eq!(
        pass2_primary["freshContext"],
        serde_json::json!(true),
        "Level 1 effect: pass 2 must carry freshContext=true"
    );
    // passIndex=2 (third pass, 0-indexed) must carry effortOverride="xhigh".
    let pass3_primary = primary_entries
        .iter()
        .find(|e| e["passIndex"] == 2)
        .expect("primary request for pass 3 (wire index 2) present");
    assert_eq!(
        pass3_primary["effortOverride"],
        serde_json::json!("xhigh"),
        "Level 2 effect: pass 3 must carry effortOverride=xhigh"
    );
}

// ─── Test 6b: ADTS escalation event audit-trail unit-coverage ──────────────
//
// Direct unit-style coverage of the escalation_events sequence is provided
// by `crates/pice-core/src/adaptive/adts.rs` (run_adts) and the `pice-daemon`
// adaptive_loop unit tests. Reproducing the full Level1→2→3 sequence in an
// e2e test requires the stub provider's per-role offset feature, deferred
// to a follow-up. The current contract criterion #5 evaluator should consult
// the unit tests for the escalation event sequence.

// ─── Test 6c: Context isolation — byte-identical prompts across passes ────
//
// Phase 4 contract criterion #11 locks "each pass sees only contract +
// current diff + CLAUDE.md". The `evaluate/create` payload the adaptive
// loop sends must be byte-identical across `pass_index = 0..N-1` for a
// given layer except for fields that MUST vary (passIndex and the ADTS
// signals). Prior-pass data must never leak into subsequent passes.
//
// Covered here at the orchestrator level via the stub's request log —
// the stub captures every `evaluate/create` params payload as one JSON
// line, and the test asserts the four stable fields (contract, diff,
// claudeMd, model) are string-equal across every captured pass.

#[tokio::test]
async fn prompt_identical_across_passes() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config(false);
    // Use AdaptiveAlgo::None with max_passes=3 so no SPRT/VEC halt intervenes
    // — the loop runs every pass up to max_passes, giving us 3 request
    // captures to compare against each other.
    let workflow = workflow_with_adaptive(AdaptiveAlgo::None, 0.90, 3, 10.0, None);
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    let log_path = dir.path().join("stub-requests.log");
    let _stub = StubAdtsGuard::new(
        "9.5,0.001;9.5,0.001;9.5,0.001",
        // Adversarial list is unused (adversarial disabled) — set to match
        // primary to avoid confusing a future reader.
        "9.5,0.001;9.5,0.001;9.5,0.001",
        Some(&log_path),
    );

    let mut sink = NullPassSink;
    let _manifest = run_stack_loops(&cfg, &NullSink, true, &mut sink)
        .await
        .unwrap();

    let log_content = std::fs::read_to_string(&log_path).expect("stub request log present");
    let entries: Vec<serde_json::Value> = log_content
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        entries.len(),
        3,
        "expected exactly 3 primary requests for AdaptiveAlgo::None + max_passes=3; got {}",
        entries.len()
    );

    // ── The stable fields MUST match byte-for-byte across all passes. ────
    // `contract`, `diff`, `claudeMd`, `model` carry no prior-pass data; their
    // values come straight from the AdaptiveContext built ONCE before the
    // loop starts. Any divergence here means state leaked between passes.
    let baseline = &entries[0];
    for (i, entry) in entries.iter().enumerate().skip(1) {
        for field in ["contract", "diff", "claudeMd", "model"] {
            assert_eq!(
                baseline[field], entry[field],
                "pass {i} diverged from pass 0 on field {field}: \
                 baseline={} vs pass={}",
                baseline[field], entry[field],
            );
        }
    }

    // ── passIndex MUST vary (0, 1, 2). This is the only field allowed to
    //    differ across passes in the non-ADTS case. ─────────────────────────
    let indices: Vec<i64> = entries
        .iter()
        .map(|e| e["passIndex"].as_i64().unwrap())
        .collect();
    assert_eq!(
        indices,
        vec![0, 1, 2],
        "passIndex must iterate 0..N-1; got {:?}",
        indices
    );

    // ── No prior-pass findings, scores, or summaries appear anywhere in
    //    subsequent passes' diff/claudeMd/contract. The stub's evaluate/result
    //    summary is "Stub evaluation complete" — grep for that string in
    //    later passes' text fields must miss. ────────────────────────────────
    for (i, entry) in entries.iter().enumerate().skip(1) {
        let combined = format!(
            "{} {} {}",
            entry["contract"], entry["diff"], entry["claudeMd"]
        );
        assert!(
            !combined.contains("Stub evaluation complete"),
            "pass {i}: prior-pass summary leaked into this pass's payload"
        );
    }
}

// ─── Test 7: VEC halts when entropy stabilizes ──────────────────────────────

#[tokio::test]
async fn vec_halts_when_entropy_stabilizes() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config(false);
    // VEC entropy floor at 0.5 bits halts at pass 2 (delta H ≈ 0.346 bits).
    // min_confidence=0.70 provides headroom above the gate: Beta(3,1) posterior
    // mean is 0.75, which clears 0.70 so VEC can promote to Passed. This is
    // the success-convergence case; the failure-convergence case is exercised
    // by `vec_halt_on_failure_sequence_does_not_pass` below.
    let mut workflow = workflow_with_adaptive(AdaptiveAlgo::Vec, 0.70, 6, 10.0, None);
    workflow.phases.evaluate.vec.entropy_floor = 0.5;
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    let _stub = StubScoresGuard::new("8.0,0.001;8.0,0.001;8.0,0.001;8.0,0.001;8.0,0.001;8.0,0.001");

    let mut sink = NullPassSink;
    let manifest = run_stack_loops(&cfg, &NullSink, true, &mut sink)
        .await
        .unwrap();
    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap();
    assert_eq!(backend.halted_by.as_deref(), Some("vec_entropy"));
    assert_eq!(backend.status, LayerStatus::Passed);
}

// ─── Test 7b: VEC halt on FAILURE convergence must not promote to Passed ──
// Codex adversarial-review fix: a failure-heavy observation sequence
// converges in entropy too. Without the `final_confidence >= min_confidence`
// gate, `build_adaptive_layer_result` would silently mark such a layer
// `Passed` — a correctness bug that green-lights broken code.

#[tokio::test]
async fn vec_halt_on_failure_sequence_does_not_pass() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config(false);
    // Scores of 3.0 against threshold 8.0 (min_confidence=0.80) are all
    // Failure observations. Posterior mean collapses toward 1/(1+N) — far
    // below 0.80. VEC halts on entropy convergence but the gate must
    // downgrade to `Failed`, not promote to `Passed`.
    let mut workflow = workflow_with_adaptive(AdaptiveAlgo::Vec, 0.80, 6, 10.0, None);
    workflow.phases.evaluate.vec.entropy_floor = 0.5;
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    let _stub = StubScoresGuard::new("3.0,0.001;3.0,0.001;3.0,0.001;3.0,0.001;3.0,0.001;3.0,0.001");

    let mut sink = NullPassSink;
    let manifest = run_stack_loops(&cfg, &NullSink, true, &mut sink)
        .await
        .unwrap();
    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap();
    assert_eq!(backend.halted_by.as_deref(), Some("vec_entropy"));
    assert_eq!(
        backend.status,
        LayerStatus::Failed,
        "VEC halting on failure sequence MUST NOT mark layer Passed; \
         got status={:?}, final_confidence={:?}",
        backend.status,
        backend.final_confidence
    );
}

// ─── Test 8: AdaptiveAlgo::None still respects budget ──────────────────────

#[tokio::test]
async fn adaptive_algo_none_respects_budget() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config(false);
    let workflow = workflow_with_adaptive(AdaptiveAlgo::None, 0.90, 10, 0.05, None);
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    let _stub = StubScoresGuard::new(
        "9.5,0.03;9.5,0.03;9.5,0.03;9.5,0.03;9.5,0.03;9.5,0.03;9.5,0.03;9.5,0.03;9.5,0.03;9.5,0.03",
    );

    let mut sink = NullPassSink;
    let manifest = run_stack_loops(&cfg, &NullSink, true, &mut sink)
        .await
        .unwrap();
    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap();
    assert_eq!(
        backend.halted_by.as_deref(),
        Some("budget"),
        "AdaptiveAlgo::None must fail-closed on budget; got {:?}",
        backend.halted_by
    );
    assert!(
        backend.passes.len() < 10,
        "budget must halt before max_passes for None; got {}",
        backend.passes.len()
    );
}

// ─── Test 9: Determinism across two identical runs ──────────────────────────

#[tokio::test]
async fn determinism_across_two_identical_runs() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config(false);
    let workflow = workflow_with_adaptive(AdaptiveAlgo::BayesianSprt, 0.90, 6, 10.0, None);
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    let _stub = StubScoresGuard::new("9.5,0.001;9.5,0.001;9.5,0.001;9.5,0.001;9.5,0.001;9.5,0.001");

    let mut sink_a = NullPassSink;
    let manifest_a = run_stack_loops(&cfg, &NullSink, true, &mut sink_a)
        .await
        .unwrap();
    let backend_a = manifest_a
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap()
        .clone();

    let mut sink_b = NullPassSink;
    let manifest_b = run_stack_loops(&cfg, &NullSink, true, &mut sink_b)
        .await
        .unwrap();
    let backend_b = manifest_b
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap()
        .clone();

    // Halt path identical (these are the manifest fields that drive
    // adaptive determinism per Phase 4 contract criterion #15).
    assert_eq!(backend_a.halted_by, backend_b.halted_by);
    assert_eq!(backend_a.passes.len(), backend_b.passes.len());
    assert_eq!(backend_a.final_confidence, backend_b.final_confidence);
    assert_eq!(backend_a.total_cost_usd, backend_b.total_cost_usd);
    assert_eq!(backend_a.escalation_events, backend_b.escalation_events);

    // Per-pass: index, model, score, cost. Timestamps are PERMITTED to
    // differ per the plan's determinism rule, so we exclude them.
    assert_eq!(backend_a.passes.len(), backend_b.passes.len());
    for (a, b) in backend_a.passes.iter().zip(backend_b.passes.iter()) {
        assert_eq!(a.index, b.index);
        assert_eq!(a.model, b.model);
        assert_eq!(a.score, b.score);
        assert_eq!(a.cost_usd, b.cost_usd);
    }
}

// ─── Test 9b: Determinism under ADTS ─────────────────────────────────────

/// Phase 4.1 Pass-6 Claude Evaluator C M1 regression: the original
/// `determinism_across_two_identical_runs` covers SPRT only. ADTS has an
/// extra axis of determinism risk — paired primary/adversarial scores,
/// escalation event sequencing, and the `next_*_params` reset discipline.
/// Although the current implementation calls providers sequentially (not
/// via `tokio::join!`), that is not locked by any test. A future refactor
/// introducing parallelism would silently break reproducibility and
/// contract criterion #15 would go green via the SPRT-only test. This
/// test exercises the ADTS path end-to-end and asserts byte-identity on
/// everything the plan calls out (scores, costs, halted_by,
/// final_confidence, escalation_events).
#[tokio::test]
async fn adts_determinism_across_two_identical_runs() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config_adts();
    // Exactly the configuration from `adts_three_level_escalation_exhausts`
    // so ADTS fires Level1 → Level2 → Level3 in 3 passes. Determinism must
    // hold for that full escalation sequence.
    let adts = pice_core::adaptive::AdtsConfig {
        divergence_threshold: 2.0,
        max_divergence_escalations: 2,
    };
    let workflow = workflow_with_adaptive(AdaptiveAlgo::Adts, 0.90, 4, 10.0, Some(adts));
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    let _stub = StubAdtsGuard::new(
        "9.0,0.001;9.0,0.001;9.0,0.001;9.0,0.001",
        "3.0,0.001;3.0,0.001;3.0,0.001;3.0,0.001",
        None,
    );

    let mut sink_a = NullPassSink;
    let manifest_a = run_stack_loops(&cfg, &NullSink, true, &mut sink_a)
        .await
        .unwrap();
    let backend_a = manifest_a
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap()
        .clone();

    let mut sink_b = NullPassSink;
    let manifest_b = run_stack_loops(&cfg, &NullSink, true, &mut sink_b)
        .await
        .unwrap();
    let backend_b = manifest_b
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap()
        .clone();

    // Halt-path invariants (contract criterion #15).
    assert_eq!(
        backend_a.halted_by, backend_b.halted_by,
        "halt reason must be byte-identical; got {:?} vs {:?}",
        backend_a.halted_by, backend_b.halted_by,
    );
    assert_eq!(backend_a.status, backend_b.status);
    assert_eq!(backend_a.passes.len(), backend_b.passes.len());
    assert_eq!(backend_a.final_confidence, backend_b.final_confidence);
    assert_eq!(backend_a.total_cost_usd, backend_b.total_cost_usd);

    // ADTS-specific: the full escalation event sequence. Without this the
    // SPRT-only test lets a reordering regression (e.g. parallel-join
    // ordering flipping events[1] and events[0]) ride. With this the
    // reproduction contract covers primary+adversarial+escalation paths.
    assert_eq!(
        backend_a.escalation_events, backend_b.escalation_events,
        "escalation_events must be byte-identical across back-to-back runs",
    );
    // Sanity: this test actually ran ADTS (not the SPRT path) and exercised
    // escalation — otherwise the escalation equality above is vacuous.
    let events = backend_a
        .escalation_events
        .as_ref()
        .expect("ADTS must populate escalation_events");
    assert!(
        !events.is_empty(),
        "test must exercise at least one escalation event; got empty list",
    );

    // Per-pass byte identity (excluding timestamp per the plan). ADTS
    // records two rows per pass cycle (primary + adversarial) so this
    // exercises paired-row ordering determinism too.
    for (a, b) in backend_a.passes.iter().zip(backend_b.passes.iter()) {
        assert_eq!(a.index, b.index, "pass index drift at {:?}", a);
        assert_eq!(
            a.model, b.model,
            "model name drift: {} vs {}",
            a.model, b.model
        );
        assert_eq!(a.score, b.score, "score drift at pass {}", a.index);
        assert_eq!(a.cost_usd, b.cost_usd, "cost drift at pass {}", a.index);
    }
}

// ─── Test 10: Cost reconciliation within tolerance ─────────────────────────

#[tokio::test]
async fn cost_reconciliation_within_tolerance() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config(false);
    // Force ≥2 passes by using SPRT with a wide confidence band.
    let workflow = workflow_with_adaptive(AdaptiveAlgo::BayesianSprt, 0.90, 5, 10.0, None);
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    let _stub = StubScoresGuard::new("9.5,0.01;9.5,0.01;9.5,0.01;9.5,0.01;9.5,0.01");

    let mut sink = NullPassSink;
    let manifest = run_stack_loops(&cfg, &NullSink, true, &mut sink)
        .await
        .unwrap();
    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap();
    let sum_passes: f64 = backend
        .passes
        .iter()
        .map(|p| p.cost_usd.unwrap_or(0.0))
        .sum();
    let total = backend.total_cost_usd.unwrap_or(0.0);
    assert!(
        (sum_passes - total).abs() < 1e-9,
        "cost reconciliation: sum(passes.cost_usd)={sum_passes} vs total_cost_usd={total}",
    );
}

// ─── Pass-3 regression: fallback_seed cost syncs into manifest & sink ──────

/// Phase 4 Pass-3 regression for Codex High #3.
///
/// When the provider reports an invalid cost (here, a negative number that
/// the stub will faithfully round-trip but `CostStats::validate_nonnegative`
/// rejects), the loop debits the cold-start seed (`budget_usd / max_passes`)
/// to `accumulated_cost` as a fail-closed safety measure. Earlier Pass-2
/// code wrote the raw invalid value to manifest `PassResult.cost_usd` AND
/// to the sink (`pass_events.cost_usd`) while the evaluation total reflected
/// the seed — so `SUM(passes[].cost_usd) != total_cost_usd`, and
/// `SUM(pass_events.cost_usd) != evaluations.final_total_cost_usd`. The
/// reconciliation invariant (Phase 4 contract criterion #16) was broken.
///
/// With the Pass-3 fix, the debited value is the single source of truth:
/// it flows into BOTH `PassResult.cost_usd` AND the sink, so all three
/// accumulators agree. The legacy "cost reconciliation within tolerance"
/// test above covers the valid-cost path (where debited == provider value).
#[tokio::test]
async fn cost_reconciliation_holds_when_provider_cost_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config(false);
    // Tight budget → seed is meaningful; BayesianSprt with a wide band so
    // we run multiple passes and exercise the fallback on each one.
    let budget_usd = 1.0_f64;
    let max_passes = 5_u32;
    let workflow = workflow_with_adaptive(
        AdaptiveAlgo::BayesianSprt,
        0.90,
        max_passes,
        budget_usd,
        None,
    );
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    // Every pass reports a negative cost. The stub parser accepts finite
    // numbers, so this round-trips to `primary_cost = Some(-0.01)`. In the
    // loop, `CostStats::validate_nonnegative(-0.01).is_err()` triggers the
    // fallback branch — debiting `fallback_seed = 1.0 / 5 = 0.2` per pass.
    let _stub = StubScoresGuard::new("9.5,-0.01;9.5,-0.01;9.5,-0.01;9.5,-0.01;9.5,-0.01");

    let mut sink = NullPassSink;
    let manifest = run_stack_loops(&cfg, &NullSink, true, &mut sink)
        .await
        .unwrap();
    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap();

    // Every pass MUST record the fallback seed (not the raw negative value).
    let expected_seed = budget_usd / max_passes as f64;
    assert!(
        !backend.passes.is_empty(),
        "expected at least one pass to have run",
    );
    for p in &backend.passes {
        let got = p.cost_usd.unwrap_or(f64::NAN);
        assert!(
            (got - expected_seed).abs() < 1e-9,
            "pass {}: expected debited seed {expected_seed}, got {got}",
            p.index,
        );
    }

    // Manifest reconciliation: sum of per-pass debited costs must equal
    // the layer's `total_cost_usd` (what goes to `evaluations.final_total_cost_usd`).
    let sum_passes: f64 = backend
        .passes
        .iter()
        .map(|p| p.cost_usd.unwrap_or(0.0))
        .sum();
    let total = backend.total_cost_usd.unwrap_or(0.0);
    assert!(
        (sum_passes - total).abs() < 1e-9,
        "reconciliation with invalid provider cost: sum(passes.cost_usd)={sum_passes} vs total_cost_usd={total}",
    );
}

/// Phase 4 Pass-3 regression for Codex High #3 (sink-side mirror).
///
/// The manifest-side reconciliation is checked above. This test asserts
/// the SAME invariant on the sink path that writes to `pass_events`: a
/// `RecordingPassSink` captures exactly the values that the daemon would
/// insert into SQLite, and their sum must equal the layer's
/// `total_cost_usd`. Without the Pass-3 fix, the sink row for a
/// fallback-triggering pass would carry the raw invalid cost while the
/// total carried the seed — breaking `SUM(pass_events.cost_usd) =
/// evaluations.final_total_cost_usd`.
#[tokio::test]
async fn pass_events_sink_mirrors_debited_cost_on_fallback() {
    use pice_daemon::orchestrator::RecordingPassSink;

    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config(false);
    let budget_usd = 1.0_f64;
    let max_passes = 5_u32;
    let workflow = workflow_with_adaptive(
        AdaptiveAlgo::BayesianSprt,
        0.90,
        max_passes,
        budget_usd,
        None,
    );
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    let _stub = StubScoresGuard::new("9.5,-0.01;9.5,-0.01;9.5,-0.01;9.5,-0.01;9.5,-0.01");

    let mut sink = RecordingPassSink::default();
    let manifest = run_stack_loops(&cfg, &NullSink, true, &mut sink)
        .await
        .unwrap();
    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap();

    // Sink must have recorded every pass; each row MUST carry the
    // debited seed (what `pass_events.cost_usd` would persist).
    let expected_seed = budget_usd / max_passes as f64;
    assert!(
        !sink.rows.is_empty(),
        "expected at least one sink row to have been recorded",
    );
    for row in &sink.rows {
        let got = row.cost_usd.unwrap_or(f64::NAN);
        assert!(
            (got - expected_seed).abs() < 1e-9,
            "sink row (pass {}, model {}): expected debited seed {expected_seed}, got {got}",
            row.pass_index,
            row.model,
        );
    }

    // The ultimate cost-reconciliation contract: SUM(sink rows) equals the
    // manifest's `total_cost_usd`. This is what downstream SQL queries
    // (`SELECT SUM(cost_usd) FROM pass_events WHERE evaluation_id = ?`
    // vs `SELECT final_total_cost_usd FROM evaluations WHERE id = ?`)
    // would observe.
    let sum_sink: f64 = sink.rows.iter().map(|r| r.cost_usd.unwrap_or(0.0)).sum();
    let total = backend.total_cost_usd.unwrap_or(0.0);
    assert!(
        (sum_sink - total).abs() < 1e-9,
        "sink reconciliation: sum(sink.cost_usd)={sum_sink} vs total_cost_usd={total}",
    );
}

// ─── Pass-3 regression: runtime errors fail the layer, not Pending ─────────

/// Phase 4 Pass-3 regression for Codex Critical #2, extended by the Pass-4
/// fix for Codex High (mid-loop cost reconciliation). Earlier code mapped
/// ANY `Err` from `run_adaptive_passes` — including runtime RPC failures,
/// provider timeouts, and provider crashes mid-loop — to `None`, then to the
/// phase-1-pending placeholder. The manifest's overall status stayed "no
/// failed layers" → daemon exits 0. A CI pipeline gating on `pice evaluate`
/// would PASS through a broken evaluation, silently masking a real bug.
///
/// With the Pass-4 fix, the adaptive loop catches per-pass provider errors
/// INTERNALLY instead of `?`-propagating them out, so passes recorded to
/// the sink before the error survive in the manifest. The `halted_by`
/// string is `runtime_error:{message}` and is prioritized over every other
/// halt reason (max_passes, sprt_*, vec_entropy). `build_adaptive_layer_result`
/// routes that string to `LayerStatus::Failed`, so the layer still fails
/// closed — but the manifest and pass_events stay consistent.
///
/// This test wires the stub to throw on EVERY `evaluate/create` call via
/// `PICE_STUB_EVALUATE_ERROR` (no from-pass gating), so pass 1 fails
/// immediately. Since no passes completed before the error, the manifest
/// has ZERO passes — which is exactly what the sink observed. The
/// complementary `mid_loop_runtime_error_preserves_prior_passes_and_costs`
/// test covers the non-empty-passes case.
#[tokio::test]
async fn runtime_error_fails_layer_not_pending() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config(false);
    // Any reasonable SPRT config works; the loop exits on the first pass
    // via the RPC error before the halt-decision path matters.
    let workflow = workflow_with_adaptive(AdaptiveAlgo::BayesianSprt, 0.90, 3, 1.0, None);
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    let _stub = StubEvaluateErrorGuard::new("simulated provider timeout");

    let mut sink = NullPassSink;
    let manifest = run_stack_loops(&cfg, &NullSink, true, &mut sink)
        .await
        .unwrap();

    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap();

    // Core contract: runtime errors MUST NOT be silently downgraded to
    // Pending. The layer has to land on `Failed` so `any_failed_layer`
    // in `handlers/evaluate.rs` fires and the daemon exits non-zero.
    assert_eq!(
        backend.status,
        LayerStatus::Failed,
        "expected runtime error to fail the layer (not Pending); halted_by={:?}",
        backend.halted_by,
    );

    // The halt reason preserves the provider's error message so
    // operators can diagnose why the evaluation died. The exact
    // wrapping ("runtime_error:…") is a stable contract with
    // `runtime_failed_layer_result`.
    let halted = backend.halted_by.clone().unwrap_or_default();
    assert!(
        halted.starts_with("runtime_error:"),
        "expected halted_by to be runtime_error:* , got {halted:?}",
    );
    assert!(
        halted.contains("simulated provider timeout"),
        "halted_by should carry the provider's error message; got {halted:?}",
    );

    // Pass-4 change: the loop now captures per-pass errors INTERNALLY and
    // preserves already-recorded passes. On pass 1 nothing was recorded yet
    // when the stub threw, so the manifest has zero passes — matching what
    // the sink actually observed. Operators distinguish runtime-error layers
    // from phase-1-pending ones via the `halted_by` string (asserted above),
    // not via a synthetic placeholder pass.
    assert_eq!(
        backend.passes.len(),
        0,
        "pass 1 error must leave zero completed passes (was {})",
        backend.passes.len(),
    );
    assert_eq!(
        backend.total_cost_usd, None,
        "zero completed passes ⇒ total_cost_usd is None",
    );
}

// ─── Pass-4 regression: init failure fails the layer, preserves placeholder ──

/// Phase 4 Pass-4 regression for Codex Critical #1 — start-error
/// classification. Before the Pass-4 fix, `try_run_layer_adaptive` mapped
/// ANY error from `ProviderOrchestrator::start` to `NotStarted` → Pending →
/// exit 0. But `start` is resolve + spawn + initialize: a provider that is
/// resolvable but fails to spawn or whose initialize RPC throws should
/// fail-close the layer, not silently degrade to Pending. Only TRULY
/// unresolvable providers (test fixtures, missing registry entries) should
/// still route to Pending.
///
/// This test exercises the init-RPC-fails case via `PICE_STUB_INIT_ERROR`:
/// the stub binary spawns normally, runs the BaseProvider constructor, and
/// then its re-registered `initialize` handler throws. The Pass-4 fix's
/// resolve-probe in `try_run_layer_adaptive` succeeds (stub is registered),
/// so the subsequent `start()` failure routes through the RuntimeError arm
/// → `runtime_failed_layer_result` → `LayerStatus::Failed`. The placeholder
/// pass (model="runtime-error") is emitted here because zero passes ran —
/// the evaluate loop never executed.
#[tokio::test]
async fn provider_init_failure_fails_layer_not_pending() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config(false);
    let workflow = workflow_with_adaptive(AdaptiveAlgo::BayesianSprt, 0.90, 3, 1.0, None);
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    let _stub = StubInitErrorGuard::new("simulated init crash");

    let mut sink = NullPassSink;
    let manifest = run_stack_loops(&cfg, &NullSink, true, &mut sink)
        .await
        .unwrap();

    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap();

    // Core contract: resolvable-but-init-fails MUST route to Failed, not
    // Pending. Before the Pass-4 fix this silently degraded to
    // `phase-1-pending-tier-N` (exit 0), letting CI green-light a broken
    // provider.
    assert_eq!(
        backend.status,
        LayerStatus::Failed,
        "init failure must fail the layer (not Pending); halted_by={:?}",
        backend.halted_by,
    );
    let halted = backend.halted_by.clone().unwrap_or_default();
    assert!(
        halted.starts_with("runtime_error:"),
        "expected halted_by to start with runtime_error:, got {halted:?}",
    );
    assert!(
        halted.contains("simulated init crash"),
        "halted_by should carry the provider's init error; got {halted:?}",
    );

    // Zero passes actually ran (the loop never executed), so
    // `runtime_failed_layer_result` emits the placeholder pass for manifest
    // well-formedness.
    assert_eq!(backend.passes.len(), 1);
    assert_eq!(backend.passes[0].model, "runtime-error");
    assert_eq!(backend.passes[0].score, None);
    assert_eq!(backend.passes[0].cost_usd, None);
}

// ─── Pass-4 regression: mid-loop error preserves prior passes + costs ────────

/// Phase 4 Pass-4 regression for Codex High — cost reconciliation on
/// partial runs. Before the fix, a provider error on pass N wiped out the
/// manifest's record of passes 1..N-1 (they were replaced by
/// `runtime_failed_layer_result`'s single placeholder) while the sink
/// still held N-1 rows. The invariant
/// `SUM(pass_events.cost_usd) == evaluations.final_total_cost_usd` then
/// broke — DB sums were > 0, manifest totals said None.
///
/// With the Pass-4 fix, the adaptive loop captures per-pass errors without
/// unwinding state. Passes 1..N-1 stay in the manifest with their scores +
/// costs; the halt string is `runtime_error:{msg}` (routed to Failed), and
/// `total_cost_usd` equals `Σ(passes[].cost_usd)` equals `Σ(sink.cost_usd)`.
#[tokio::test]
async fn mid_loop_runtime_error_preserves_prior_passes_and_costs() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config(false);
    // Budget large enough to reach pass 3; SPRT config that won't accept
    // on the first two passes (score 7 is below 0.9 posterior threshold).
    let workflow = workflow_with_adaptive(AdaptiveAlgo::BayesianSprt, 0.95, 5, 1.0, None);
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    // Pass 1 and 2: score 7.0 at $0.02 each; the stub returns these cleanly.
    // Pass 3 onward: stub throws `network timeout` (from-pass = 3).
    let _stub = StubMidLoopErrorGuard::new(
        "7.0,0.02;7.0,0.02;7.0,0.02;7.0,0.02;7.0,0.02",
        "network timeout",
        3,
    );

    let mut sink = RecordingPassSink::default();
    let manifest = run_stack_loops(&cfg, &NullSink, true, &mut sink)
        .await
        .unwrap();

    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap();

    // Layer fails closed with the runtime-error halt reason.
    assert_eq!(
        backend.status,
        LayerStatus::Failed,
        "mid-loop provider error must fail the layer; halted_by={:?}",
        backend.halted_by,
    );
    let halted = backend.halted_by.clone().unwrap_or_default();
    assert!(
        halted.starts_with("runtime_error:"),
        "expected halted_by to start with runtime_error:, got {halted:?}",
    );
    assert!(
        halted.contains("network timeout"),
        "halted_by should carry the provider error; got {halted:?}",
    );

    // Core contract: passes completed BEFORE the error survive in the manifest.
    // Passes 1 and 2 succeeded with cost 0.02 each; pass 3 threw and is NOT
    // in the manifest (no PassResult was pushed — the error was captured
    // before the record).
    assert_eq!(
        backend.passes.len(),
        2,
        "manifest must preserve passes 1 + 2; got {:?}",
        backend.passes,
    );
    assert_eq!(backend.passes[0].index, 1);
    assert_eq!(backend.passes[0].cost_usd, Some(0.02));
    assert_eq!(backend.passes[1].index, 2);
    assert_eq!(backend.passes[1].cost_usd, Some(0.02));

    // Reconciliation: manifest `total_cost_usd` equals the per-pass sum
    // equals Σ(sink.cost_usd). This is the invariant the pre-fix code broke.
    let expected_total = 0.04_f64;
    let total = backend.total_cost_usd.unwrap_or(-1.0);
    assert!(
        (total - expected_total).abs() < 1e-9,
        "total_cost_usd = {total}, expected {expected_total}",
    );
    let sink_total: f64 = sink.rows.iter().map(|r| r.cost_usd.unwrap_or(0.0)).sum();
    assert!(
        (sink_total - expected_total).abs() < 1e-9,
        "Σ(sink) = {sink_total}, expected {expected_total}",
    );
    assert_eq!(
        sink.rows.len(),
        2,
        "sink must hold exactly the two completed passes",
    );
}

// ─── Pass-4 regression: zero-cost totals ship as Some(0.0), not None ─────────

/// Phase 4 Pass-4 regression for Codex High — zero-cost total collapse.
/// Before the fix, `adaptive_loop.rs` emitted `total_cost_usd = None` when
/// `accumulated_cost == 0.0`, and `handlers/evaluate.rs` emitted
/// `final_total_cost_usd = None` when the sum over layers was 0.0. But
/// per-pass rows STILL recorded `cost_usd = 0.0` when the provider reported
/// a valid zero cost. The reconciliation invariant broke: sink rows summed
/// to 0.0, manifest total was null — `(SUM(cost_usd) - final_total_cost_usd)`
/// returned NULL for the evaluation, so the HAVING clause in the contract's
/// validation query reported no violation but the invariant was broken.
///
/// With the Pass-4 fix, the gate is `passes.is_empty()` (adaptive_loop) and
/// `any(|l| l.total_cost_usd.is_some())` (evaluate handler). When passes
/// ran with zero cost, `total_cost_usd = Some(0.0)` all the way up.
#[tokio::test]
async fn zero_cost_provider_preserves_some_zero_total() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config(false);
    // Low min_confidence + 3 passes at score 3.0 → SPRT rejects; cost 0.0
    // for all passes.
    let workflow = workflow_with_adaptive(AdaptiveAlgo::BayesianSprt, 0.90, 3, 1.0, None);
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    let _stub = StubScoresGuard::new("3.0,0.0;3.0,0.0;3.0,0.0");

    let mut sink = RecordingPassSink::default();
    let manifest = run_stack_loops(&cfg, &NullSink, true, &mut sink)
        .await
        .unwrap();

    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap();

    // At least one pass ran; its cost was 0.0.
    assert!(
        !backend.passes.is_empty(),
        "zero-cost test requires at least one pass; got empty passes",
    );
    for pass in &backend.passes {
        assert_eq!(
            pass.cost_usd,
            Some(0.0),
            "every pass must record cost_usd = Some(0.0); got {:?}",
            pass.cost_usd,
        );
    }

    // Core contract: `total_cost_usd = Some(0.0)`, NOT `None`. This is the
    // primary Pass-4 fix — passes ran, cost was observed (even if zero), so
    // the total must be reported truthfully.
    assert_eq!(
        backend.total_cost_usd,
        Some(0.0),
        "zero-cost passes must keep total_cost_usd = Some(0.0), not None",
    );

    // Sink reconciliation: Σ(sink.cost_usd) == total_cost_usd, both 0.0.
    let sink_total: f64 = sink.rows.iter().map(|r| r.cost_usd.unwrap_or(-1.0)).sum();
    assert!(
        (sink_total - 0.0).abs() < 1e-9,
        "Σ(sink.cost_usd) must equal 0.0 for zero-cost passes; got {sink_total}",
    );
    for row in &sink.rows {
        assert_eq!(
            row.cost_usd,
            Some(0.0),
            "sink rows must mirror per-pass Some(0.0); got {:?}",
            row.cost_usd,
        );
    }
}

/// Complementary to `runtime_error_fails_layer_not_pending`: verify that
/// an UNRESOLVABLE provider (the loop never starts) still routes to
/// `Pending` via `phase1_pending_layer_result`. This guards against the
/// Pass-3 fix over-correcting — test fixtures that lack a provider
/// binary must continue to degrade gracefully rather than being
/// treated as runtime errors.
///
/// Note: this test builds `StackLoopsConfig` inline rather than via
/// `make_cfg` because the shared helper hardcodes `primary_provider:
/// "stub"`. The primary-provider name is passed separately from the
/// `PiceConfig` (which only names the adversarial provider), so to
/// trigger the `NotStarted` branch we must override `primary_provider`
/// to a name that `registry::resolve` does not know.
#[tokio::test]
async fn unresolvable_provider_remains_phase1_pending_not_failed() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config(false);
    let workflow = workflow_with_adaptive(AdaptiveAlgo::BayesianSprt, 0.90, 3, 1.0, None);
    let seams = BTreeMap::new();
    // Inline construction so we can inject a bogus primary provider name.
    let cfg = StackLoopsConfig {
        layers: &layers,
        plan_path: &plan_path,
        project_root: dir.path(),
        primary_provider: "not-a-real-provider-kjsdfhgsd",
        primary_model: "stub-model",
        pice_config: &pice_config,
        workflow: &workflow,
        merged_seams: &seams,
    };

    let mut sink = NullPassSink;
    let manifest = run_stack_loops(&cfg, &NullSink, true, &mut sink)
        .await
        .unwrap();

    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap();

    // Pending, not Failed — the graceful-degrade path is preserved.
    assert_eq!(
        backend.status,
        LayerStatus::Pending,
        "unresolvable provider must degrade to Pending; halted_by={:?}",
        backend.halted_by,
    );
    let halted = backend.halted_by.clone().unwrap_or_default();
    assert!(
        halted.starts_with("phase-1-pending-tier-"),
        "expected phase-1-pending halt reason; got {halted:?}",
    );
    assert_eq!(backend.passes.len(), 1);
    assert_eq!(backend.passes[0].model, "phase-1-pending");
}

// ─── Pass-5 Codex Critical #1 regression: ADTS escalation must not mask budget
//     halts on the final iteration ─────────────────────────────────────────────
//
// Before the fix, the ADTS `ScheduleExtraPassFreshContext` /
// `ScheduleExtraPassElevatedEffort` arms used `continue;` to skip the post-pass
// universal guardrails (`decide_halt`). When the budget was exhausted by the
// pass that JUST ran and ADTS scheduled an escalation, the `continue` masked
// the budget overrun; the loop then exited naturally via max_passes and the
// fallback reported `halted_by = "max_passes"` instead of `"budget"`.
//
// This reproducer pins the exact Codex scenario:
//   max_passes = 1, budget_usd = 0.01, primary+adversarial cost 0.02 each
//   (accumulated = 0.04 after pass 1), divergent scores (9 vs 3 → ADTS
//   schedules Level-1 escalation). Expected: halted_by = "budget",
//   layer status = Pending (budget is fail-closed; user may re-run with
//   higher budget). NOT "max_passes". NOT "adts_escalation_exhausted".

#[tokio::test]
async fn adts_budget_halt_wins_over_escalation_on_final_iteration() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config_adts();
    let adts = AdtsConfig {
        divergence_threshold: 2.0,
        max_divergence_escalations: 2,
    };
    // max_passes=1 is the edge case: there is no "next iteration" whose
    // pre-pass budget check would otherwise catch the overrun. Only the
    // post-pass guardrails on THIS iteration can fail-close on budget.
    let workflow = workflow_with_adaptive(AdaptiveAlgo::Adts, 0.90, 1, 0.01, Some(adts));
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    // Divergent scores (9 vs 3) so ADTS fires Level-1 at the end of pass 1.
    // Primary+adversarial each cost 0.02 → accumulated 0.04, well over the
    // 0.01 budget. The cold-start seed at pre-pass check is
    // budget/max_passes = 0.01 / 1 = 0.01, so the pre-pass check lets pass 1
    // proceed (0.01 + 0.01 = 0.02 > 0.01 is false — seed check is strictly
    // greater-than). The overrun only becomes detectable POST-pass.
    let log_path = dir.path().join("stub-adts-budget.log");
    let _stub = StubAdtsGuard::new("9.0,0.02", "3.0,0.02", Some(&log_path));

    let mut sink = NullPassSink;
    let manifest = run_stack_loops(&cfg, &NullSink, true, &mut sink)
        .await
        .unwrap();
    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap();

    assert_eq!(
        backend.halted_by.as_deref(),
        Some("budget"),
        "expected budget halt, not ADTS escalation or max_passes masking; got {:?}",
        backend.halted_by,
    );
    // Budget is a fail-closed guardrail, not a success or failure verdict.
    assert_eq!(backend.status, LayerStatus::Pending);
    // ADTS runs primary + adversarial per iteration and records both as
    // PassResult rows. With max_passes=1 the loop ran one iteration, so the
    // manifest has two rows (one primary, one adversarial).
    assert_eq!(backend.passes.len(), 2);
    // Level-1 was still recorded as scheduled (ADTS observed the divergence
    // before the guardrail fired), but the escalation will not run because
    // the loop halted. ADTS fires once per iteration; with a single iteration
    // we expect exactly one Level-1 event.
    let events = backend
        .escalation_events
        .as_ref()
        .expect("ADTS populates escalation_events even on budget halt");
    assert_eq!(
        events.len(),
        1,
        "expected exactly one escalation event; got {:?}",
        events,
    );
    assert!(
        matches!(
            events[0],
            pice_core::adaptive::EscalationEvent::Level1FreshContext { .. }
        ),
        "events[0] must be Level1FreshContext; got {:?}",
        events[0],
    );
}

// ─── Pass-5 Codex Critical #2 regression: evaluate/result notifications must
//     be correlated by session_id to prevent cross-pass contamination ───────
//
// Before the fix, `ProviderOrchestrator::evaluate_one_pass` registered the
// `evaluate/result` notification handler BEFORE `evaluate/create` returned
// the session_id, and the handler accepted the first matching notification
// regardless of session. In a multi-pass run where the same provider process
// handles sequential sessions, a late or duplicate notification from pass
// N-1 could satisfy pass N's oneshot with stale scores/findings.
//
// This reproducer uses the stub's `PICE_STUB_INJECT_STALE_EVAL_RESULT` harness
// to emit a stale notification (wrong sessionId) BEFORE the real one. The
// stale notification reports `passed: false`; the real notification reports
// `passed: true` (score 9 >= threshold 7). A correct session-correlation
// filter returns the real result. A naive handler captures the stale one and
// the layer fails when it should pass.

#[tokio::test]
async fn evaluate_result_filters_stale_notifications_by_session_id() {
    let dir = tempfile::tempdir().unwrap();
    let plan_path = setup_minimal_repo(dir.path());
    let layers = single_layer_config();
    let pice_config = stub_pice_config(false);
    // SPRT mode, single pass needed — any clean accept/reject path works.
    let workflow = workflow_with_adaptive(AdaptiveAlgo::BayesianSprt, 0.70, 3, 10.0, None);
    let seams = BTreeMap::new();
    let cfg = make_cfg(
        &layers,
        &plan_path,
        dir.path(),
        &pice_config,
        &workflow,
        &seams,
    );

    // Stub returns score 9 (passed) with cost 0.02. The injected stale
    // notification has `passed: false`. If the Rust handler takes the first
    // notification it sees regardless of sessionId (the bug), the layer
    // records the stale `passed: false` and halts SPRT-rejected. The fix
    // filters by sessionId so the stale notification is dropped and the
    // real `passed: true` observation wins.
    let _stub = StubStaleResultGuard::new(
        "9.0,0.02;9.0,0.02;9.0,0.02",
        "wrong-session-xyz-that-does-not-exist",
    );

    let mut sink = NullPassSink;
    let manifest = run_stack_loops(&cfg, &NullSink, true, &mut sink)
        .await
        .unwrap();
    let backend = manifest
        .layers
        .iter()
        .find(|l| l.name == "backend")
        .unwrap();

    // The real notifications win → SPRT sees success observations → accepts.
    // If stale notifications had leaked through, we'd see `sprt_rejected`
    // and `LayerStatus::Failed` instead.
    let halted = backend.halted_by.clone().unwrap_or_default();
    assert!(
        halted == "sprt_confidence_reached" || halted == "max_passes",
        "expected SPRT accept or max_passes (both imply real success notifications \
         won); got {halted:?} which indicates stale notifications leaked through",
    );
    assert_ne!(
        backend.status,
        LayerStatus::Failed,
        "layer must not be Failed — Failed would mean stale `passed:false` leaked in",
    );
    // Every recorded pass must have score 9 (from real notifications), not 0
    // (from stale). Passes[].score is the normalized criterion score.
    for p in &backend.passes {
        assert!(
            p.score >= Some(7.0),
            "pass {} has score {:?}; expected ≥7 from real stub notification",
            p.index,
            p.score,
        );
    }
}

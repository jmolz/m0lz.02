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
use pice_daemon::orchestrator::{NullPassSink, NullSink};
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

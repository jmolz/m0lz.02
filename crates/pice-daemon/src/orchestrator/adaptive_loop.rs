//! Async adaptive pass loop — Phase 4.
//!
//! Drives the per-layer evaluation through the four [`AdaptiveAlgo`] variants
//! (`BayesianSprt`, `Adts`, `Vec`, `None`). Owns pass state, invokes the
//! provider(s), feeds scores to [`decide_halt`] (and [`run_adts`] when ADTS
//! is active), records per-pass metrics BEFORE the halt-decision check, and
//! returns a summary outcome for the caller to persist on the manifest.
//!
//! # Invariants
//!
//! - **Sink writes happen BEFORE the halt decision.** A budget-halted loop
//!   still persists the triggering pass cost. Enforced by the call order in
//!   the loop body and tested in `crates/pice-daemon/tests/adaptive_integration.rs`.
//! - **`next_*_params` reset after non-ADTS verdicts.** ADTS flags apply
//!   only to the immediately following pass; a `Continue` verdict rolls
//!   them back to the project baseline. Missing this causes escalation bleed.
//! - **`escalation_events` only populated for ADTS.** Other algorithms
//!   never emit level transitions; `LayerResult.escalation_events` stays `None`.
//! - **Confidence ceiling (0.966) is enforced in `decide_halt`.** This
//!   module just returns what `decide_halt` reports.
//! - **`run_adts` exhaustion is the only halt that does NOT go through
//!   `decide_halt`.** ADTS divergence is the orchestrator's concern, per
//!   [`pice_core::adaptive::decide`]'s contract.

use anyhow::{Context, Result};
use pice_core::adaptive::{
    decide_halt, run_adts, AdtsConfig, AdtsVerdict, CostStats, EscalationEvent, HaltReason,
    PairedScore, PassObservation, SprtConfig, VecConfig,
};
use pice_core::layers::manifest::PassResult;
use pice_core::workflow::schema::AdaptiveAlgo;
use serde_json::Value;

use super::core::{PerPassOutcome, ProviderOrchestrator};

/// Outcome returned from the adaptive loop. Consumed by `run_stack_loops`
/// to build the final `LayerResult`.
///
/// `halted_by` is the wire form of a [`HaltReason`] (e.g. `"sprt_rejected"`)
/// or `"adts_escalation_exhausted"`. Seam failures compose LATER in
/// `stack_loops.rs` and are not emitted by the loop itself.
#[derive(Debug, Clone)]
pub struct AdaptiveOutcome {
    pub passes: Vec<PassResult>,
    pub halted_by: Option<String>,
    pub final_confidence: Option<f64>,
    pub total_cost_usd: Option<f64>,
    /// Populated only when `algorithm == Adts` AND at least one level
    /// transition fired. `None` otherwise so manifest serialization stays
    /// lean for SPRT/VEC/None runs.
    pub escalation_events: Option<Vec<EscalationEvent>>,
    pub algorithm: AdaptiveAlgo,
}

/// All inputs the loop needs that are stable across passes.
///
/// Separating this from the loop's mutable state keeps the adaptive logic
/// from pretending to own the provider handles — the caller is responsible
/// for starting/stopping them and can reuse them across layers when that
/// makes sense.
#[derive(Debug, Clone)]
pub struct AdaptiveContext {
    pub algo: AdaptiveAlgo,
    pub sprt: SprtConfig,
    pub adts: AdtsConfig,
    pub vec: VecConfig,
    pub min_confidence: f64,
    pub max_passes: u32,
    pub budget_usd: f64,
    pub contract: Value,
    pub diff: String,
    pub claude_md: String,
    pub primary_model: String,
    pub adversarial_model: Option<String>,
    /// Base effort sent to each provider on the first pass (and any pass
    /// following a non-escalating verdict). ADTS Level-2 overrides this
    /// to `"xhigh"` for exactly one pass.
    pub base_effort: Option<String>,
}

/// Metrics callback invoked BEFORE the halt-decision check for each provider
/// invocation.
///
/// Production implementations write to `pass_events` via
/// [`crate::metrics::store::insert_pass_event`]; test implementations may
/// collect rows in a `Vec` for inspection. The sink is called once per
/// provider call — so an ADTS pass writes TWO rows (primary + adversarial).
///
/// Phase 4.1 Pass-6 Codex High #3: sink errors are now FATAL. Once the
/// evaluation header exists, a silent pass_event write failure produces
/// a manifest+DB pair that looks successful but hides missing rows,
/// breaking cost reconciliation downstream. The trait therefore surfaces
/// errors to the caller (the adaptive loop), which propagates them via
/// `?` into a `LayerAdaptiveResult::RuntimeError` → `LayerStatus::Failed`
/// — the same path used for any other runtime failure. Metrics absence
/// (`NullPassSink`) is distinct from metrics failure and stays silent.
pub trait PassMetricsSink: Send {
    fn record_pass(
        &mut self,
        pass_index: u32,
        model: &str,
        score: Option<f64>,
        cost_usd: Option<f64>,
    ) -> anyhow::Result<()>;
}

/// Discard sink used when metrics are disabled.
pub struct NullPassSink;

impl PassMetricsSink for NullPassSink {
    fn record_pass(
        &mut self,
        _: u32,
        _: &str,
        _: Option<f64>,
        _: Option<f64>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

/// In-memory sink used by unit and integration tests. Each call appends one
/// record to `rows`.
#[derive(Debug, Default)]
pub struct RecordingPassSink {
    pub rows: Vec<RecordingPassEvent>,
}

#[derive(Debug, Clone)]
pub struct RecordingPassEvent {
    pub pass_index: u32,
    pub model: String,
    pub score: Option<f64>,
    pub cost_usd: Option<f64>,
}

impl PassMetricsSink for RecordingPassSink {
    fn record_pass(
        &mut self,
        pass_index: u32,
        model: &str,
        score: Option<f64>,
        cost_usd: Option<f64>,
    ) -> anyhow::Result<()> {
        self.rows.push(RecordingPassEvent {
            pass_index,
            model: model.to_string(),
            score,
            cost_usd,
        });
        Ok(())
    }
}

/// Run the adaptive pass loop.
///
/// Drives `primary` (always) and `adversarial` (when `algo == Adts` and the
/// caller supplied it) until one of the halt conditions fires. Per-pass
/// metrics are written to `sink` BEFORE each halt-decision check so a
/// budget-halted loop still records the triggering pass cost.
///
/// # Pseudocode
///
/// ```text
/// for pass_index in 1..=max_passes:
///   pre = decide_halt(...)
///   if pre.halt && pre.reason == Budget: halt(budget)
///   run primary(pass_index, next_effort), observe cost, sink.record_pass
///   if algo == Adts: run adversarial(pass_index, next_effort), sink.record_pass
///   classify primary score → observation
///   if algo == Adts:
///     verdict = run_adts(paired, escalations_used, adts_cfg)
///     Continue: reset next_* = defaults
///     Level1: fresh_context, bump escalations; push event; `continue`
///     Level2: fresh + effort="xhigh", bump; push event; `continue`
///     Exhausted: push event, halt(adts_escalation_exhausted)
///   post = decide_halt(...)
///   if post.halt: halt(post.reason)
/// if halted_by is None: halted_by = max_passes
/// ```
pub async fn run_adaptive_passes(
    ctx: &AdaptiveContext,
    primary: &mut ProviderOrchestrator,
    mut adversarial: Option<&mut ProviderOrchestrator>,
    sink: &mut dyn PassMetricsSink,
) -> Result<AdaptiveOutcome> {
    let mut passes: Vec<PassResult> = Vec::new();
    let mut observations: Vec<PassObservation> = Vec::new();
    let mut paired_scores: Vec<PairedScore> = Vec::new();
    let mut escalation_events: Vec<EscalationEvent> = Vec::new();
    let mut cost_stats = CostStats::new();
    let mut accumulated_cost = 0.0_f64;
    let mut adts_escalations_used: u32 = 0;

    // ADTS re-arms these each pass. `next_effort_override` is `Some("xhigh")`
    // only for the pass immediately following a Level 2 escalation; baseline
    // `effort` lives separately in `ctx.base_effort`. `next_fresh_context` is
    // `Some(true)` only for the pass immediately following a Level 1+
    // escalation. Both roll back to `None` after consumption — Phase 4
    // invariant "next_* reset after non-ADTS verdicts" in stack-loops.md.
    let mut next_effort_override: Option<String> = None;
    let mut next_fresh_context: Option<bool> = None;

    let mut halted_by: Option<HaltReason> = None;
    let mut adts_halt_str: Option<String> = None;
    // Phase 4 Pass-4 fix for Codex High: when a per-pass provider RPC fails
    // mid-loop, the prior code propagated the error via `?`, discarding all
    // accumulated state — including the passes/pass_events that had already
    // been persisted for earlier passes. `runtime_failed_layer_result` then
    // emitted a placeholder pass with no cost, so manifest totals stopped
    // matching the sink rows that were actually written (Crit #16 breakage).
    //
    // Capture the error here instead. The loop breaks, preserves accumulated
    // `passes`, `accumulated_cost`, and `escalation_events`, and the outer
    // `halted_by_str` construction prioritizes this reason so
    // `build_adaptive_layer_result` routes the layer to `Failed`.
    let mut halted_by_runtime_error: Option<String> = None;
    let mut final_confidence: Option<f64> = None;

    for pass_index in 1..=ctx.max_passes {
        // ── Pre-pass budget check ─────────────────────────────────────
        // Runs even on the first pass: cold-start seed may already exceed
        // a tight budget (see `cold_start_seed_blocks_overspend_on_pass_one`
        // integration test).
        let pre = decide_halt(
            ctx.algo,
            &observations,
            &ctx.sprt,
            &ctx.vec,
            ctx.min_confidence,
            ctx.max_passes,
            accumulated_cost,
            &cost_stats,
            ctx.budget_usd,
        )?;
        if pre.halt && pre.reason == Some(HaltReason::Budget) {
            halted_by = Some(HaltReason::Budget);
            final_confidence = Some(pre.confidence);
            break;
        }

        // ── Primary provider ─────────────────────────────────────────
        // Wire `pass_index` 0-indexed per `pice-protocol::EvaluateCreateParams`
        // docstring — the loop iterates 1..=max_passes but the wire form
        // matches the stub's `PICE_STUB_SCORES` array indexing (0-based).
        // Manifest `PassResult.index` stays 1-indexed (user-facing audit trail).
        let wire_pass_index = pass_index.saturating_sub(1);
        let primary_out = match primary
            .evaluate_one_pass(
                ctx.contract.clone(),
                ctx.diff.clone(),
                ctx.claude_md.clone(),
                Some(ctx.primary_model.clone()),
                ctx.base_effort.clone(),
                Some(wire_pass_index),
                next_fresh_context,
                next_effort_override.clone(),
            )
            .await
        {
            Ok(out) => out,
            Err(e) => {
                // Pass-4 fix: capture and halt with partial state preserved.
                halted_by_runtime_error = Some(format!("runtime_error:{e}"));
                break;
            }
        };
        let primary_score = scalar_score(&primary_out);
        let primary_cost = primary_out.cost_usd;
        let primary_model_name = primary.provider_name().to_string();
        let timestamp = chrono::Utc::now().to_rfc3339();

        // Phase 4 Pass-3 fix (Codex Critical #1): derive the SPRT/VEC
        // observation from the provider's VERDICT (and per-criterion
        // `passed` flags), NOT from the numeric score average. Averaging
        // masks single-criterion failures — e.g. nine criteria at 10/10
        // + one at 0/10 yields mean 9.0, which a naive `score >= threshold`
        // check classified as Success even though `result.passed = false`
        // and one criterion literally failed its own threshold. SPRT/VEC
        // then halted early with false confidence on a layer whose
        // contract explicitly failed. Compute here, BEFORE the PassResult
        // construction below consumes `primary_out.result.summary`.
        //
        // `scalar_score` continues to drive numeric reporting (manifest
        // `PassResult.score`) and ADTS `paired_scores` divergence math.
        let primary_observation = observation_from(&primary_out);

        // Phase 4 post-adversarial-review fix (Codex High #3): when the
        // provider omits or mis-reports cost_usd (None, NaN, ∞, negative),
        // the naive "ignore it" path was fail-OPEN — a provider without
        // cost telemetry could run unbounded past the configured budget.
        // Fail-closed by debiting the conservative cold-start seed so the
        // budget gate in `decide_halt` still binds on the next pre-pass check.
        //
        // Phase 4 Pass-3 fix (Codex High #3, round 2): the debited value is
        // now the single source of truth — it flows into manifest
        // `PassResult.cost_usd`, `cost_stats`, `accumulated_cost`, AND the
        // sink (pass_events). Earlier code wrote the RAW provider value to
        // the manifest/sink while debiting the seed to `accumulated_cost`,
        // so `SUM(passes[].cost_usd)` drifted from `total_cost_usd` whenever
        // the provider's cost was missing/invalid. This breaks the
        // reconciliation invariant (Phase 4 contract criterion #16). Now
        // all four locations agree — and when the provider reported a
        // valid cost, the debited value IS `primary_cost`, so legacy
        // behavior is unchanged.
        // Compute the debited cost WITHOUT mutating cost_stats or
        // accumulated_cost yet — see the write-ahead ordering note below.
        let fallback_seed = ctx.budget_usd / ctx.max_passes as f64;
        let (primary_debited_cost, primary_observed_cost): (Option<f64>, f64) = match primary_cost {
            Some(c) if CostStats::validate_nonnegative(c).is_ok() => (Some(c), c),
            _ => (Some(fallback_seed), fallback_seed),
        };

        // ── Persist FIRST, then mutate in-memory state ──────────────
        // Phase 4.1 Pass-7 Codex High #3: the sink is the audit-trail
        // ground truth for `SUM(pass_events.cost_usd)`. Before this fix,
        // `passes.push` + `accumulated_cost += c` ran BEFORE the sink,
        // and the sink errored via `?` — which unwound the loop. The
        // caller rebuilt the layer as a `runtime-error` placeholder with
        // zero passes and no cost, discarding already-persisted earlier
        // passes that HAD landed in `pass_events`. Σ(pass_events) then
        // diverged from the manifest's `total_cost_usd`, re-opening the
        // reconciliation hole Pass-3 Codex High #3 closed.
        //
        // Write-ahead logging pattern: persist to the durable store
        // first. If the persist fails, no in-memory mutation has
        // happened, so manifest state at pass N matches sink state at
        // pass N (both reflect "pass N did not land"). `break` into the
        // `halted_by_runtime_error` capture path — mirrors the
        // provider-error handling at line 260-266 — preserves
        // passes 1..N-1 intact on both sides. Downstream routing via
        // `runtime_error:` prefix sends the layer to `LayerStatus::Failed`
        // in `build_adaptive_layer_result`.
        //
        // Sink still runs BEFORE the halt dispatcher — the halt check
        // happens at the bottom of the iteration, after both primary
        // and (optionally) adversarial have persisted. A budget-halted
        // loop still records the triggering pass cost.
        if let Err(e) = sink
            .record_pass(
                pass_index,
                &primary_model_name,
                primary_score,
                primary_debited_cost,
            )
            .context("failed to persist primary pass_event")
        {
            halted_by_runtime_error = Some(format!("runtime_error:metrics_persist_failed:{e}"));
            break;
        }

        // Persist succeeded — commit in-memory state.
        cost_stats.observe(primary_observed_cost);
        accumulated_cost += primary_observed_cost;

        let primary_pr = PassResult {
            index: pass_index,
            model: primary_model_name.clone(),
            score: primary_score,
            cost_usd: primary_debited_cost,
            timestamp: timestamp.clone(),
            findings: primary_out
                .result
                .summary
                .map(|s| vec![s])
                .unwrap_or_default(),
        };
        passes.push(primary_pr.clone());

        // ── Adversarial provider (ADTS only) ─────────────────────────
        let mut adversarial_score: Option<f64> = None;
        if ctx.algo == AdaptiveAlgo::Adts {
            if let Some(adv) = adversarial.as_deref_mut() {
                let adv_model = ctx
                    .adversarial_model
                    .clone()
                    .unwrap_or_else(|| ctx.primary_model.clone());
                let adv_out = match adv
                    .evaluate_one_pass(
                        ctx.contract.clone(),
                        ctx.diff.clone(),
                        ctx.claude_md.clone(),
                        Some(adv_model),
                        ctx.base_effort.clone(),
                        Some(wire_pass_index),
                        next_fresh_context,
                        next_effort_override.clone(),
                    )
                    .await
                {
                    Ok(out) => out,
                    Err(e) => {
                        // Pass-4 fix: the primary's PassResult + pass_events
                        // row for this pass are ALREADY written above; halt
                        // here with a runtime_error reason so reconciliation
                        // stays intact (manifest and DB both have pass N
                        // primary, no adversarial — accurate reflection of
                        // what ran).
                        halted_by_runtime_error = Some(format!("runtime_error:{e}"));
                        break;
                    }
                };
                let adv_score = scalar_score(&adv_out);
                adversarial_score = adv_score;
                let adv_cost = adv_out.cost_usd;
                let adv_model_name = adv.provider_name().to_string();

                // Same fail-closed fallback as the primary: debit the seed
                // when the adversarial provider omits or mis-reports cost.
                // The debited value is the single source of truth that
                // flows into BOTH the manifest `PassResult.cost_usd` AND
                // the sink (pass_events), keeping reconciliation intact
                // (Pass-3 Codex High #3 fix).
                //
                // Pass-7 Codex High #3: compute debited cost WITHOUT mutating
                // state; persist first; then mutate. See the primary-path
                // write-ahead comment above for the full rationale.
                let (adv_debited_cost, adv_observed_cost): (Option<f64>, f64) = match adv_cost {
                    Some(c) if CostStats::validate_nonnegative(c).is_ok() => (Some(c), c),
                    _ => (Some(fallback_seed), fallback_seed),
                };

                // Pass-7 Codex High #3: persist to the sink BEFORE pushing
                // the pass to the manifest and mutating cost state. A sink
                // failure now breaks cleanly with no manifest-vs-sink drift
                // for this pass (both sides reflect "pass N adversarial did
                // not land"). Primary persistence for this pass already
                // succeeded above; its passes[] entry and cost bookkeeping
                // stay intact. Halt string prefix `runtime_error:` routes
                // the layer to `Failed` via `build_adaptive_layer_result`.
                if let Err(e) = sink
                    .record_pass(pass_index, &adv_model_name, adv_score, adv_debited_cost)
                    .context("failed to persist adversarial pass_event")
                {
                    halted_by_runtime_error =
                        Some(format!("runtime_error:metrics_persist_failed:{e}"));
                    break;
                }

                // Persist succeeded — commit in-memory state.
                cost_stats.observe(adv_observed_cost);
                accumulated_cost += adv_observed_cost;

                passes.push(PassResult {
                    index: pass_index,
                    model: adv_model_name.clone(),
                    score: adv_score,
                    cost_usd: adv_debited_cost,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    findings: adv_out.result.summary.map(|s| vec![s]).unwrap_or_default(),
                });
            }
        }

        // ── Classify primary outcome into a Success/Failure observation ──
        // (Derived above before `primary_out` was consumed into the
        // PassResult construction.)
        observations.push(primary_observation);

        // ── ADTS three-level escalation ──────────────────────────────
        // Only touches state for `algo == Adts`. Other algorithms fall
        // through directly to the post-pass halt decision below.
        if ctx.algo == AdaptiveAlgo::Adts {
            if let (Some(p), Some(a)) = (primary_score, adversarial_score) {
                paired_scores.push((p, a));
            }
            let verdict = run_adts(&paired_scores, adts_escalations_used, &ctx.adts)?;
            match verdict {
                AdtsVerdict::Continue => {
                    // Converged on this pass — reset next-pass overrides so a
                    // Level 1/2 flag from earlier does not bleed into a
                    // later Continue pass. Phase 4 invariant: "next_* reset
                    // after non-ADTS verdicts" (stack-loops.md).
                    next_effort_override = None;
                    next_fresh_context = None;
                }
                AdtsVerdict::ScheduleExtraPassFreshContext => {
                    // Level 1: ask the provider to drop prior-pass context on
                    // the NEXT pass via `freshContext=true`. Effort stays at
                    // baseline (orchestrator's `ctx.base_effort`).
                    next_fresh_context = Some(true);
                    next_effort_override = None;
                    adts_escalations_used += 1;
                    escalation_events.push(EscalationEvent::Level1FreshContext {
                        at_pass: pass_index,
                    });
                    // Phase 4 Pass-5 Codex Critical #1 fix: DO NOT `continue`
                    // past the universal post-pass guardrails. An ADTS
                    // scheduling verdict must not mask a budget overrun or
                    // max_passes exhaustion accumulated during the pass that
                    // JUST ran. `decide_halt` for `AdaptiveAlgo::Adts` only
                    // enforces budget + max_passes (algorithm-specific branch
                    // returns `halt=false`), so falling through is safe:
                    // legitimate escalation scheduling survives for the next
                    // iteration's pre-pass check unless the accumulated cost
                    // already exhausted the budget — in which case we halt
                    // with `budget` (correct fail-closed behavior), not the
                    // natural-exit `max_passes` fallback. The `next_*` flags
                    // stay set so the next iteration picks them up. Regression
                    // test: `adts_budget_halt_wins_over_escalation_on_final_iteration`.
                }
                AdtsVerdict::ScheduleExtraPassElevatedEffort => {
                    // Level 2: elevate compute for the NEXT pass only via
                    // `effortOverride="xhigh"`. Keep fresh context too — the
                    // post-L1 session reuse should not be restored just
                    // because we're escalating further.
                    next_fresh_context = Some(true);
                    next_effort_override = Some("xhigh".to_string());
                    adts_escalations_used += 1;
                    escalation_events.push(EscalationEvent::Level2ElevatedEffort {
                        at_pass: pass_index,
                    });
                    // Phase 4 Pass-5 Codex Critical #1 fix: same reasoning as
                    // the Level-1 branch above — fall through to the universal
                    // post-pass `decide_halt` so budget/max_passes wins over
                    // ADTS escalation scheduling when the accumulated cost
                    // exhausted the budget during THIS pass.
                }
                AdtsVerdict::EscalationExhausted => {
                    escalation_events.push(EscalationEvent::Level3Exhausted {
                        at_pass: pass_index,
                    });
                    adts_halt_str = Some("adts_escalation_exhausted".to_string());
                    final_confidence = Some(posterior_mean_capped(&observations));
                    break;
                }
            }
        }

        // ── Post-pass halt decision (SPRT / VEC / None / max_passes) ──
        let post = decide_halt(
            ctx.algo,
            &observations,
            &ctx.sprt,
            &ctx.vec,
            ctx.min_confidence,
            ctx.max_passes,
            accumulated_cost,
            &cost_stats,
            ctx.budget_usd,
        )?;
        if post.halt {
            halted_by = post.reason;
            final_confidence = Some(post.confidence);
            break;
        }
    }

    // If we exited the loop without a halt reason, the natural explanation
    // is `max_passes` — we ran the full budget and no early halt fired.
    //
    // Phase 4 Pass-4 fix: a runtime error captured mid-loop wins over every
    // other halt reason. This is NOT a natural-convergence halt, so we must
    // not label it `max_passes` or `sprt_*` or `vec_entropy`; downstream
    // routing in `build_adaptive_layer_result` depends on the string starting
    // with `"runtime_error:"` to route the layer to `Failed`.
    let halted_by_str = match (halted_by_runtime_error, halted_by, adts_halt_str) {
        (Some(s), _, _) => Some(s),
        (None, _, Some(s)) => Some(s),
        (None, Some(reason), None) => Some(reason.as_str().to_string()),
        (None, None, None) => {
            if final_confidence.is_none() {
                final_confidence = Some(posterior_mean_capped(&observations));
            }
            Some(HaltReason::MaxPasses.as_str().to_string())
        }
    };

    // Phase 4 Pass-4 fix for Codex High: previously `accumulated_cost == 0.0`
    // collapsed to `None`, even when passes DID run with zero-cost providers.
    // That broke the cost-reconciliation invariant:
    // `SUM(passes[].cost_usd) == total_cost_usd` — the sum would be 0.0 while
    // the total was null. Gate instead on "did any pass actually run".
    // If zero passes ran (e.g. cold-start seed blocked pass 1), None is still
    // the right answer because the manifest truly has no cost observations.
    let total_cost_usd = if passes.is_empty() {
        None
    } else {
        Some(accumulated_cost)
    };

    let escalation_events = if ctx.algo == AdaptiveAlgo::Adts && !escalation_events.is_empty() {
        Some(escalation_events)
    } else {
        None
    };

    Ok(AdaptiveOutcome {
        passes,
        halted_by: halted_by_str,
        final_confidence,
        total_cost_usd,
        escalation_events,
        algorithm: ctx.algo,
    })
}

/// Derive a `PassObservation` (Success/Failure) from a provider's
/// `PerPassOutcome` for SPRT / VEC consumption.
///
/// **Uses the boolean verdict, not the numeric score average.** Averaging
/// hides single-criterion failures: nine criteria at 10/10 + one at 0/10
/// yields mean 9.0, which a naive threshold check would classify as a
/// Success observation — even though the provider explicitly reports
/// `passed = false` and one criterion's own threshold was not met. The
/// adaptive algorithms would then halt early with false confidence on a
/// layer whose contract semantically failed.
///
/// Rule: `Success ⇔ result.passed && scores.iter().all(|s| s.passed)`.
/// When `scores` is empty, only `result.passed` contributes (legacy
/// providers without per-criterion telemetry).
///
/// This helper is intentionally separate from [`scalar_score`]:
/// - `scalar_score` drives numeric reporting and ADTS `paired_scores`
///   divergence math — divergence between two providers is a different
///   axis from the boolean pass/fail verdict.
/// - `observation_from` drives SPRT / VEC halt decisions — those want the
///   verdict, not the mean.
fn observation_from(out: &PerPassOutcome) -> PassObservation {
    if out.result.passed && out.result.scores.iter().all(|s| s.passed) {
        PassObservation::Success
    } else {
        PassObservation::Failure
    }
}

/// Derive a scalar pass score (0–10 scale) from a provider's `PerPassOutcome`.
///
/// Preference order:
/// 1. Average of `result.scores[].score` when non-empty — matches the legacy
///    single-pass aggregation and the scores shown to users.
/// 2. `result.passed` fallback — `true` → 10.0, `false` → 0.0. This is a
///    last-resort signal for providers that don't emit per-criterion scores.
///
/// Returns `None` only if the provider emitted neither scores nor a
/// `passed` verdict, which the adaptive loop treats as a missing observation.
///
/// **Not used for SPRT/VEC observations** — see [`observation_from`].
/// This helper is retained for reporting (manifest `PassResult.score`) and
/// ADTS `paired_scores` divergence (numeric agreement between providers).
fn scalar_score(out: &PerPassOutcome) -> Option<f64> {
    if !out.result.scores.is_empty() {
        let sum: i64 = out.result.scores.iter().map(|s| s.score as i64).sum();
        return Some(sum as f64 / out.result.scores.len() as f64);
    }
    // Fallback to the boolean verdict.
    Some(if out.result.passed { 10.0 } else { 0.0 })
}

// Phase 4 Pass-5 Claude Evaluator C Criterion 1 fix: the daemon previously
// carried a duplicate `posterior_mean_capped` helper alongside the one in
// `pice-core::adaptive::decide`. Both implementations were byte-identical and
// both capped via `CONFIDENCE_CEILING`, but the drift risk was real — a
// future refactor that changed the cap or the prior in one file would have
// silently broken the invariant in the other. We now call the pice-core
// re-export so there is exactly ONE capping path in production code.
use pice_core::adaptive::posterior_mean_capped;

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // `scalar_score` is the single most important pure helper in this module
    // — if it drifts, SPRT observations lose their meaning. A small unit
    // suite locks its behavior. Full loop tests go in Task 20.

    fn outcome_with(passed: bool, scores: Vec<i32>, cost: Option<f64>) -> PerPassOutcome {
        use pice_protocol::{CriterionScore, EvaluateResultParams};
        PerPassOutcome {
            result: EvaluateResultParams {
                session_id: "s".into(),
                scores: scores
                    .into_iter()
                    .map(|score| CriterionScore {
                        name: "t".into(),
                        score: score as u8,
                        threshold: 7,
                        passed: score >= 7,
                        findings: None,
                    })
                    .collect(),
                passed,
                summary: None,
            },
            cost_usd: cost,
            confidence: None,
        }
    }

    #[test]
    fn scalar_score_averages_when_scores_present() {
        let o = outcome_with(true, vec![9, 7, 8], Some(0.01));
        let s = scalar_score(&o).unwrap();
        assert!((s - 8.0).abs() < 1e-12);
    }

    #[test]
    fn scalar_score_falls_back_to_passed_flag_when_no_scores() {
        let pass = outcome_with(true, vec![], None);
        assert_eq!(scalar_score(&pass), Some(10.0));
        let fail = outcome_with(false, vec![], None);
        assert_eq!(scalar_score(&fail), Some(0.0));
    }

    #[test]
    fn null_sink_never_panics() {
        let mut s = NullPassSink;
        assert!(s.record_pass(1, "m", Some(9.0), Some(0.01)).is_ok());
        assert!(s.record_pass(2, "m", None, None).is_ok());
    }

    #[test]
    fn recording_sink_captures_rows_in_order() {
        let mut s = RecordingPassSink::default();
        s.record_pass(1, "claude", Some(9.0), Some(0.02)).unwrap();
        s.record_pass(1, "codex", Some(3.0), Some(0.03)).unwrap();
        s.record_pass(2, "claude", Some(9.1), Some(0.02)).unwrap();
        assert_eq!(s.rows.len(), 3);
        assert_eq!(s.rows[0].model, "claude");
        assert_eq!(s.rows[1].model, "codex");
        assert_eq!(s.rows[2].pass_index, 2);
    }

    /// Phase 4.1 Pass-6 Codex High #3: a sink implementation that fails on
    /// insert must surface the error through `?` to the adaptive loop, which
    /// routes it to `LayerStatus::Failed`. This guards the trait signature
    /// itself — if a future refactor reverts `record_pass` to an infallible
    /// `fn(..)` returning `()`, this test stops compiling.
    #[test]
    fn sink_error_is_surfaced_and_not_swallowed() {
        struct FailingSink;
        impl PassMetricsSink for FailingSink {
            fn record_pass(
                &mut self,
                _: u32,
                _: &str,
                _: Option<f64>,
                _: Option<f64>,
            ) -> anyhow::Result<()> {
                Err(anyhow::anyhow!("simulated DB write failure"))
            }
        }
        let mut s = FailingSink;
        let result = s.record_pass(1, "m", Some(9.0), Some(0.01));
        assert!(result.is_err(), "failing sink must return Err");
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("simulated DB write failure"),
            "error must propagate verbatim; got {err:?}",
        );
    }

    #[test]
    fn posterior_mean_capped_at_ceiling() {
        let many_successes = vec![PassObservation::Success; 1000];
        let conf = posterior_mean_capped(&many_successes);
        assert!(conf <= pice_core::adaptive::CONFIDENCE_CEILING);
    }

    #[test]
    fn posterior_mean_near_half_at_empty() {
        // Beta(1, 1) has mean 0.5 — the uninformed prior, used when we halt
        // before observing anything (should not happen in practice, but the
        // helper must be total).
        let conf = posterior_mean_capped(&[]);
        assert!((conf - 0.5).abs() < 1e-12);
    }

    /// Phase 4 Pass-3 regression for Codex Critical #1.
    ///
    /// A single failing criterion must produce `Failure`, even when the
    /// other criteria pull the score average above the user's reporting
    /// threshold. The previous logic classified this as `Success` by
    /// comparing the AVERAGE of criterion scores to `min_confidence * 10`
    /// — a provider emitting `passed = false` with mean 9.0 would be fed
    /// to SPRT as a win, halting early with false confidence on a layer
    /// whose contract explicitly failed.
    #[test]
    fn observation_from_derives_from_verdict_not_average() {
        use pice_protocol::{CriterionScore, EvaluateResultParams};
        // Nine passing criteria at 10/10 + one failing criterion at 0/10.
        // Mean = 9.0. Provider's verdict = false (contract explicitly failed).
        let mut scores: Vec<CriterionScore> = (0..9)
            .map(|i| CriterionScore {
                name: format!("c{i}"),
                score: 10,
                threshold: 7,
                passed: true,
                findings: None,
            })
            .collect();
        scores.push(CriterionScore {
            name: "failing".into(),
            score: 0,
            threshold: 7,
            passed: false,
            findings: Some("failed".into()),
        });
        let out = PerPassOutcome {
            result: EvaluateResultParams {
                session_id: "s".into(),
                scores,
                passed: false,
                summary: None,
            },
            cost_usd: Some(0.01),
            confidence: None,
        };

        // Average-based reporting still yields 9.0 — used for paired-score
        // divergence math and for manifest `PassResult.score`.
        let mean = scalar_score(&out).unwrap();
        assert!((mean - 9.0).abs() < 1e-9);

        // But the SPRT observation must respect the provider's verdict.
        assert_eq!(observation_from(&out), PassObservation::Failure);
    }

    #[test]
    fn observation_from_success_requires_all_criteria_and_verdict() {
        use pice_protocol::{CriterionScore, EvaluateResultParams};
        let scores = (0..3)
            .map(|i| CriterionScore {
                name: format!("c{i}"),
                score: 9,
                threshold: 7,
                passed: true,
                findings: None,
            })
            .collect();
        let out = PerPassOutcome {
            result: EvaluateResultParams {
                session_id: "s".into(),
                scores,
                passed: true,
                summary: None,
            },
            cost_usd: Some(0.01),
            confidence: None,
        };
        assert_eq!(observation_from(&out), PassObservation::Success);
    }

    #[test]
    fn observation_from_verdict_false_is_failure_even_when_all_criteria_pass() {
        // Defence-in-depth: a provider reporting `passed=false` with every
        // criterion `passed=true` is internally inconsistent, but we honor
        // the top-level verdict (conservative — the provider saw something
        // we did not).
        use pice_protocol::{CriterionScore, EvaluateResultParams};
        let scores = vec![CriterionScore {
            name: "c0".into(),
            score: 10,
            threshold: 7,
            passed: true,
            findings: None,
        }];
        let out = PerPassOutcome {
            result: EvaluateResultParams {
                session_id: "s".into(),
                scores,
                passed: false,
                summary: None,
            },
            cost_usd: Some(0.01),
            confidence: None,
        };
        assert_eq!(observation_from(&out), PassObservation::Failure);
    }
}

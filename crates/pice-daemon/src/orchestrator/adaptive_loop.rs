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

use anyhow::Result;
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
/// Errors are logged by the caller (not surfaced through this trait) —
/// metrics failures are never fatal per CLAUDE.md.
pub trait PassMetricsSink: Send {
    fn record_pass(
        &mut self,
        pass_index: u32,
        model: &str,
        score: Option<f64>,
        cost_usd: Option<f64>,
    );
}

/// Discard sink used when metrics are disabled.
pub struct NullPassSink;

impl PassMetricsSink for NullPassSink {
    fn record_pass(&mut self, _: u32, _: &str, _: Option<f64>, _: Option<f64>) {}
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
    ) {
        self.rows.push(RecordingPassEvent {
            pass_index,
            model: model.to_string(),
            score,
            cost_usd,
        });
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

    // ADTS re-arms these each pass. `next_effort` seeds with `base_effort`
    // so the first pass and every Continue-branch pass use the baseline.
    let mut next_effort: Option<String> = ctx.base_effort.clone();

    let mut halted_by: Option<HaltReason> = None;
    let mut adts_halt_str: Option<String> = None;
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
        let primary_out = primary
            .evaluate_one_pass(
                ctx.contract.clone(),
                ctx.diff.clone(),
                ctx.claude_md.clone(),
                Some(ctx.primary_model.clone()),
                next_effort.clone(),
                Some(pass_index),
            )
            .await?;
        let primary_score = scalar_score(&primary_out);
        let primary_cost = primary_out.cost_usd;
        let primary_model_name = primary.provider_name().to_string();
        let timestamp = chrono::Utc::now().to_rfc3339();
        let primary_pr = PassResult {
            index: pass_index,
            model: primary_model_name.clone(),
            score: primary_score,
            cost_usd: primary_cost,
            timestamp: timestamp.clone(),
            findings: primary_out
                .result
                .summary
                .map(|s| vec![s])
                .unwrap_or_default(),
        };
        passes.push(primary_pr.clone());
        // Phase 4 post-adversarial-review fix (Codex High #3): when the
        // provider omits or mis-reports cost_usd (None, NaN, ∞, negative),
        // the naive "ignore it" path was fail-OPEN — a provider without
        // cost telemetry could run unbounded past the configured budget.
        // Fail-closed by debiting the conservative cold-start seed so the
        // budget gate in `decide_halt` still binds on the next pre-pass check.
        let fallback_seed = ctx.budget_usd / ctx.max_passes as f64;
        match primary_cost {
            Some(c) if CostStats::validate_nonnegative(c).is_ok() => {
                cost_stats.observe(c);
                accumulated_cost += c;
            }
            _ => {
                // Unknown/invalid cost — debit the seed so budget stays honored.
                cost_stats.observe(fallback_seed);
                accumulated_cost += fallback_seed;
            }
        }
        // Sink BEFORE halt decision — crash-safety invariant.
        sink.record_pass(pass_index, &primary_model_name, primary_score, primary_cost);

        // ── Adversarial provider (ADTS only) ─────────────────────────
        let mut adversarial_score: Option<f64> = None;
        if ctx.algo == AdaptiveAlgo::Adts {
            if let Some(adv) = adversarial.as_deref_mut() {
                let adv_model = ctx
                    .adversarial_model
                    .clone()
                    .unwrap_or_else(|| ctx.primary_model.clone());
                let adv_out = adv
                    .evaluate_one_pass(
                        ctx.contract.clone(),
                        ctx.diff.clone(),
                        ctx.claude_md.clone(),
                        Some(adv_model),
                        next_effort.clone(),
                        Some(pass_index),
                    )
                    .await?;
                let adv_score = scalar_score(&adv_out);
                adversarial_score = adv_score;
                let adv_cost = adv_out.cost_usd;
                let adv_model_name = adv.provider_name().to_string();
                passes.push(PassResult {
                    index: pass_index,
                    model: adv_model_name.clone(),
                    score: adv_score,
                    cost_usd: adv_cost,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    findings: adv_out.result.summary.map(|s| vec![s]).unwrap_or_default(),
                });
                // Same fail-closed fallback as the primary: debit the seed
                // when the adversarial provider omits or mis-reports cost.
                match adv_cost {
                    Some(c) if CostStats::validate_nonnegative(c).is_ok() => {
                        cost_stats.observe(c);
                        accumulated_cost += c;
                    }
                    _ => {
                        cost_stats.observe(fallback_seed);
                        accumulated_cost += fallback_seed;
                    }
                }
                sink.record_pass(pass_index, &adv_model_name, adv_score, adv_cost);
            }
        }

        // ── Classify primary score into a Success/Failure observation ──
        // Threshold is `min_confidence * 10` because the SPRT observation
        // feed is on the 0–10 scale (see `PassObservation` docs).
        if let Some(score) = primary_score {
            let threshold = ctx.min_confidence * 10.0;
            observations.push(if score >= threshold {
                PassObservation::Success
            } else {
                PassObservation::Failure
            });
        }

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
                    // Converged on this pass — reset next-pass overrides.
                    next_effort = ctx.base_effort.clone();
                }
                AdtsVerdict::ScheduleExtraPassFreshContext => {
                    // Level 1: fresh context is the default for each pass
                    // (session is recreated per `evaluate_one_pass` call).
                    // Effort stays at baseline.
                    next_effort = ctx.base_effort.clone();
                    adts_escalations_used += 1;
                    escalation_events.push(EscalationEvent::Level1FreshContext {
                        at_pass: pass_index,
                    });
                    continue; // skip post-pass halt; force next pass
                }
                AdtsVerdict::ScheduleExtraPassElevatedEffort => {
                    // Level 2: elevate compute for the next pass only.
                    next_effort = Some("xhigh".to_string());
                    adts_escalations_used += 1;
                    escalation_events.push(EscalationEvent::Level2ElevatedEffort {
                        at_pass: pass_index,
                    });
                    continue;
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
    let halted_by_str = match (halted_by, adts_halt_str) {
        (_, Some(s)) => Some(s),
        (Some(reason), None) => Some(reason.as_str().to_string()),
        (None, None) => {
            if final_confidence.is_none() {
                final_confidence = Some(posterior_mean_capped(&observations));
            }
            Some(HaltReason::MaxPasses.as_str().to_string())
        }
    };

    let total_cost_usd = if accumulated_cost > 0.0 {
        Some(accumulated_cost)
    } else {
        None
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
fn scalar_score(out: &PerPassOutcome) -> Option<f64> {
    if !out.result.scores.is_empty() {
        let sum: i64 = out.result.scores.iter().map(|s| s.score as i64).sum();
        return Some(sum as f64 / out.result.scores.len() as f64);
    }
    // Fallback to the boolean verdict.
    Some(if out.result.passed { 10.0 } else { 0.0 })
}

/// Same Beta(1+s, 1+f) posterior mean as `pice-core::adaptive::decide`, used
/// here for the ADTS-exhausted and max-passes halt paths where `decide_halt`
/// did not compute a confidence.
fn posterior_mean_capped(obs: &[PassObservation]) -> f64 {
    let (s, f) = obs.iter().fold((0u32, 0u32), |(s, f), o| match o {
        PassObservation::Success => (s + 1, f),
        PassObservation::Failure => (s, f + 1),
    });
    let alpha = 1.0 + s as f64;
    let beta = 1.0 + f as f64;
    (alpha / (alpha + beta)).min(pice_core::adaptive::CONFIDENCE_CEILING)
}

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
        s.record_pass(1, "m", Some(9.0), Some(0.01));
        s.record_pass(2, "m", None, None);
    }

    #[test]
    fn recording_sink_captures_rows_in_order() {
        let mut s = RecordingPassSink::default();
        s.record_pass(1, "claude", Some(9.0), Some(0.02));
        s.record_pass(1, "codex", Some(3.0), Some(0.03));
        s.record_pass(2, "claude", Some(9.1), Some(0.02));
        assert_eq!(s.rows.len(), 3);
        assert_eq!(s.rows[0].model, "claude");
        assert_eq!(s.rows[1].model, "codex");
        assert_eq!(s.rows[2].pass_index, 2);
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
}

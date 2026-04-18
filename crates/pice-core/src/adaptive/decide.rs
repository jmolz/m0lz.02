//! Halt-decision dispatcher.
//!
//! `decide_halt` is the single entry point the orchestrator calls before AND
//! after each pass. It enforces the universal guardrails (budget, max_passes)
//! ahead of any algorithm-specific halt — ensuring `AdaptiveAlgo::None` still
//! respects the budget cap and that no algorithm can talk past the budget.
//!
//! ADTS escalation (`fresh_context`, `effort` upgrade, exhaustion) is handled
//! by the orchestrator's [`adts::run_adts`] call OUTSIDE this dispatcher;
//! `decide_halt` for `AdaptiveAlgo::Adts` only enforces the universal
//! guardrails and returns `halt=false` for the algorithm-specific branch.
//!
//! [`adts::run_adts`]: crate::adaptive::adts::run_adts

use crate::adaptive::cost::CostStats;
use crate::adaptive::sprt::run_sprt;
use crate::adaptive::types::{
    cap_confidence, AdaptiveError, HaltDecision, HaltReason, PassObservation, SprtConfig, VecConfig,
};
use crate::adaptive::vec::run_vec;
use crate::workflow::schema::AdaptiveAlgo;

/// Decide whether the pass loop should halt right now.
///
/// The function is pure — it never mutates `cost_stats`. The orchestrator
/// owns the loop state and calls this both pre-pass (with the projected next
/// cost included in the budget check) and post-pass (with algorithm halt
/// logic engaged).
///
/// # Order of checks
///
/// 1. **Budget** — fail-closed. `accumulated_cost_usd + cost_stats.project_next(seed)`
///    must not exceed `budget_usd`. Cold-start seed is `budget_usd / max_passes`.
///    Applies to ALL algorithms including `AdaptiveAlgo::None`.
/// 2. **Max passes** — `passes.len() >= max_passes` halts. Applies to ALL.
/// 3. **Algorithm-specific** — SPRT or VEC halt logic. ADTS and None return
///    `halt=false` from this branch (ADTS exhaustion handled by orchestrator).
///
/// # Errors
///
/// - [`AdaptiveError::InvalidMaxPasses`] when `max_passes == 0`.
/// - [`AdaptiveError::InvalidBudget`] when `budget_usd` is non-finite or
///   negative.
/// - Anything propagated from `run_sprt` / `run_vec` (invalid config).
#[allow(clippy::too_many_arguments)]
pub fn decide_halt(
    algo: AdaptiveAlgo,
    passes: &[PassObservation],
    sprt_cfg: &SprtConfig,
    vec_cfg: &VecConfig,
    min_confidence: f64,
    max_passes: u32,
    accumulated_cost_usd: f64,
    cost_stats: &CostStats,
    budget_usd: f64,
) -> Result<HaltDecision, AdaptiveError> {
    if max_passes == 0 {
        return Err(AdaptiveError::InvalidMaxPasses);
    }
    if !budget_usd.is_finite() || budget_usd < 0.0 {
        return Err(AdaptiveError::InvalidBudget(budget_usd));
    }

    let confidence = posterior_mean_capped(passes);

    // ── 1. Budget gate (universal, fail-closed) ────────────────────────
    // `budget_usd == 0.0` is the documented "no financial enforcement"
    // escape hatch (see `.claude/plans/phase-4-adaptive-evaluation.md`
    // Known Limitations). Skip the gate entirely in that case — otherwise
    // any positive per-pass cost trips `cost + projected > 0` on the first
    // post-pass check, which silently invalidates the escape hatch.
    // Pass-7 Codex Critical #1 fix.
    //
    // `max_passes` still binds universally; a negative budget is already
    // rejected via `InvalidBudget` above.
    if budget_usd > 0.0 {
        let cold_start_seed = budget_usd / max_passes as f64;
        let projected = cost_stats.project_next(cold_start_seed);
        if accumulated_cost_usd + projected > budget_usd {
            return Ok(HaltDecision {
                halt: true,
                reason: Some(HaltReason::Budget),
                confidence,
            });
        }
    }

    // ── 2. Max-passes gate (universal) ────────────────────────────────
    if passes.len() as u32 >= max_passes {
        return Ok(HaltDecision {
            halt: true,
            reason: Some(HaltReason::MaxPasses),
            confidence,
        });
    }

    // ── 3. Algorithm-specific halt ────────────────────────────────────
    match algo {
        AdaptiveAlgo::BayesianSprt => run_sprt(passes, sprt_cfg, min_confidence),
        AdaptiveAlgo::Vec => run_vec(passes, vec_cfg),
        AdaptiveAlgo::Adts | AdaptiveAlgo::None => Ok(HaltDecision {
            halt: false,
            reason: None,
            confidence,
        }),
    }
}

/// Beta(1+s, 1+f) posterior mean, capped via [`cap_confidence`]. Used for
/// the confidence field on guard-driven halts where no algorithm computed
/// one. Made `pub` in Pass-5 so the daemon's adaptive loop uses this
/// canonical helper instead of a hand-rolled duplicate — eliminating the
/// drift risk Evaluator C flagged on Criterion 1.
pub fn posterior_mean_capped(passes: &[PassObservation]) -> f64 {
    let (s, f) = passes.iter().fold((0u32, 0u32), |(s, f), o| match o {
        PassObservation::Success => (s + 1, f),
        PassObservation::Failure => (s, f + 1),
    });
    let alpha = 1.0 + s as f64;
    let beta = 1.0 + f as f64;
    cap_confidence(alpha / (alpha + beta))
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adaptive::types::CONFIDENCE_CEILING;

    fn defaults() -> (SprtConfig, VecConfig) {
        (SprtConfig::default(), VecConfig::default())
    }

    /// Helper: a CostStats that has observed `obs` already.
    fn stats_after(obs: &[f64]) -> CostStats {
        let mut s = CostStats::new();
        for &c in obs {
            s.observe(c);
        }
        s
    }

    // ── Universal gate: budget ────────────────────────────────────────────

    #[test]
    fn budget_trumps_confidence_halt() {
        // 5 successes would normally trigger SPRT accept under default config,
        // but a tight budget intervenes BEFORE the algorithm halt fires.
        let (sprt, vc) = defaults();
        let passes = vec![PassObservation::Success; 5];
        // Each pass cost 0.03; 5 passes = 0.15 accumulated, projection ≈ 0.03.
        let stats = stats_after(&[0.03, 0.03, 0.03, 0.03, 0.03]);
        let out = decide_halt(
            AdaptiveAlgo::BayesianSprt,
            &passes,
            &sprt,
            &vc,
            0.95,
            10,
            0.15,
            &stats,
            0.16, // budget — accumulated 0.15 + projected ~0.03 > 0.16
        )
        .unwrap();
        assert!(out.halt);
        assert_eq!(out.reason, Some(HaltReason::Budget));
    }

    #[test]
    fn budget_halts_none_algorithm_too() {
        // Critical for Phase 4 contract: AdaptiveAlgo::None still respects budget.
        let (sprt, vc) = defaults();
        let passes = vec![PassObservation::Success; 3];
        let stats = stats_after(&[0.03, 0.03, 0.03]);
        let out = decide_halt(
            AdaptiveAlgo::None,
            &passes,
            &sprt,
            &vc,
            0.95,
            10,
            0.09,
            &stats,
            0.10, // 0.09 + ~0.03 > 0.10
        )
        .unwrap();
        assert!(out.halt);
        assert_eq!(out.reason, Some(HaltReason::Budget));
    }

    #[test]
    fn cold_start_seed_blocks_when_seed_exceeds_budget() {
        // No observations yet (n=0) → projected = cold-start seed. With
        // budget 0.001 / max_passes 5 = seed 0.0002, but accumulated 0.0009
        // already, the seed pushes us over.
        let (sprt, vc) = defaults();
        let passes: Vec<PassObservation> = Vec::new();
        let stats = CostStats::new();
        let out = decide_halt(
            AdaptiveAlgo::BayesianSprt,
            &passes,
            &sprt,
            &vc,
            0.95,
            5,
            0.0009,
            &stats,
            0.001,
        )
        .unwrap();
        assert!(out.halt);
        assert_eq!(out.reason, Some(HaltReason::Budget));
    }

    // ── Universal gate: max_passes ────────────────────────────────────────

    #[test]
    fn max_passes_halts_at_limit() {
        let (sprt, vc) = defaults();
        let passes = vec![PassObservation::Success, PassObservation::Failure];
        // No spike, no SPRT halt, but at max=2 we stop.
        let stats = stats_after(&[0.001, 0.001]);
        let out = decide_halt(
            AdaptiveAlgo::BayesianSprt,
            &passes,
            &sprt,
            &vc,
            0.95,
            2,
            0.002,
            &stats,
            10.0,
        )
        .unwrap();
        assert!(out.halt);
        assert_eq!(out.reason, Some(HaltReason::MaxPasses));
    }

    #[test]
    fn max_passes_halts_none_algorithm() {
        // None algorithm with max=3 must halt at exactly 3 passes.
        let (sprt, vc) = defaults();
        let passes = vec![PassObservation::Success; 3];
        let stats = stats_after(&[0.001; 3]);
        let out = decide_halt(
            AdaptiveAlgo::None,
            &passes,
            &sprt,
            &vc,
            0.95,
            3,
            0.003,
            &stats,
            10.0,
        )
        .unwrap();
        assert!(out.halt);
        assert_eq!(out.reason, Some(HaltReason::MaxPasses));
    }

    #[test]
    fn budget_takes_priority_over_max_passes_when_both_fire() {
        // Both conditions hold; budget check runs FIRST in the dispatcher.
        let (sprt, vc) = defaults();
        let passes = vec![PassObservation::Success; 3];
        let stats = stats_after(&[0.04, 0.04, 0.04]);
        let out = decide_halt(
            AdaptiveAlgo::BayesianSprt,
            &passes,
            &sprt,
            &vc,
            0.95,
            3,    // max_passes hit
            0.12, // budget exceeded as well
            &stats,
            0.13,
        )
        .unwrap();
        assert_eq!(out.reason, Some(HaltReason::Budget));
    }

    // ── Algorithm-specific halts ───────────────────────────────────────────

    #[test]
    fn sprt_accept_fires_when_within_budget_and_under_max() {
        let (sprt, vc) = defaults();
        let passes = vec![PassObservation::Success; 5];
        let stats = stats_after(&[0.001; 5]);
        let out = decide_halt(
            AdaptiveAlgo::BayesianSprt,
            &passes,
            &sprt,
            &vc,
            0.95,
            10,
            0.005,
            &stats,
            10.0,
        )
        .unwrap();
        assert!(out.halt);
        assert_eq!(out.reason, Some(HaltReason::SprtConfidenceReached));
    }

    #[test]
    fn vec_entropy_halt_fires_when_within_budget() {
        let (sprt, _) = defaults();
        let vc = VecConfig { entropy_floor: 0.5 }; // generous floor for fast halt
        let passes = vec![PassObservation::Success, PassObservation::Success];
        let stats = stats_after(&[0.001, 0.001]);
        let out = decide_halt(
            AdaptiveAlgo::Vec,
            &passes,
            &sprt,
            &vc,
            0.95,
            10,
            0.002,
            &stats,
            10.0,
        )
        .unwrap();
        assert!(out.halt);
        assert_eq!(out.reason, Some(HaltReason::VecEntropy));
    }

    #[test]
    fn adts_algorithm_branch_returns_no_halt() {
        // The dispatcher does NOT inspect ADTS divergence — that's the
        // orchestrator's job. ADTS in decide_halt only honors guardrails.
        let (sprt, vc) = defaults();
        let passes = vec![PassObservation::Success; 2];
        let stats = stats_after(&[0.001; 2]);
        let out = decide_halt(
            AdaptiveAlgo::Adts,
            &passes,
            &sprt,
            &vc,
            0.95,
            10,
            0.002,
            &stats,
            10.0,
        )
        .unwrap();
        assert!(!out.halt);
        assert_eq!(out.reason, None);
    }

    #[test]
    fn none_algorithm_with_room_to_spare_does_not_halt() {
        let (sprt, vc) = defaults();
        let passes = vec![PassObservation::Success; 2];
        let stats = stats_after(&[0.001; 2]);
        let out = decide_halt(
            AdaptiveAlgo::None,
            &passes,
            &sprt,
            &vc,
            0.95,
            10,
            0.002,
            &stats,
            10.0,
        )
        .unwrap();
        assert!(!out.halt);
        assert_eq!(out.reason, None);
    }

    // ── Validation ────────────────────────────────────────────────────────

    #[test]
    fn rejects_zero_max_passes() {
        let (sprt, vc) = defaults();
        let stats = CostStats::new();
        assert!(matches!(
            decide_halt(
                AdaptiveAlgo::BayesianSprt,
                &[],
                &sprt,
                &vc,
                0.95,
                0,
                0.0,
                &stats,
                1.0,
            ),
            Err(AdaptiveError::InvalidMaxPasses)
        ));
    }

    #[test]
    fn rejects_invalid_budget() {
        let (sprt, vc) = defaults();
        let stats = CostStats::new();
        for bad in [-0.1f64, f64::NAN, f64::INFINITY] {
            assert!(matches!(
                decide_halt(
                    AdaptiveAlgo::BayesianSprt,
                    &[],
                    &sprt,
                    &vc,
                    0.95,
                    5,
                    0.0,
                    &stats,
                    bad,
                ),
                Err(AdaptiveError::InvalidBudget(_))
            ));
        }
    }

    // ── Confidence reporting ──────────────────────────────────────────────

    #[test]
    fn confidence_capped_on_budget_halt() {
        // 100 successes would push posterior mean toward 1.0; the budget
        // halt path must still report ≤ ceiling.
        let (sprt, vc) = defaults();
        let passes = vec![PassObservation::Success; 100];
        // No spike, no observations to track; budget exhausted by accumulated.
        let stats = CostStats::new();
        let out = decide_halt(
            AdaptiveAlgo::BayesianSprt,
            &passes,
            &sprt,
            &vc,
            0.95,
            200, // intentionally larger than passes.len()
            10.0,
            &stats,
            5.0, // accumulated > budget
        )
        .unwrap();
        assert!(out.halt);
        assert_eq!(out.reason, Some(HaltReason::Budget));
        assert!(out.confidence <= CONFIDENCE_CEILING);
    }

    /// Pass-7 Codex Critical #1 regression: `budget_usd == 0.0` is the
    /// documented "no financial enforcement" escape hatch. Before the fix
    /// in `decide_halt`, the universal `accumulated + projected > budget_usd`
    /// check evaluated `positive + positive > 0` as true after the first
    /// pass with any real cost, halting with `halted_by = "budget"` and
    /// invalidating the escape hatch. This test runs the dispatcher with
    /// a positive accumulated cost and `CostStats` that has observed a
    /// positive per-pass cost, under `budget_usd = 0.0`, and asserts no
    /// budget halt fires — the algorithm-layer halt (or no-halt) wins.
    #[test]
    fn budget_usd_zero_does_not_trip_on_positive_costs() {
        let (sprt, vc) = defaults();
        // Two ambiguous observations — SPRT neither accepts nor rejects.
        let passes = vec![PassObservation::Success, PassObservation::Failure];
        // Prior passes cost $0.02 each; stats have observed that; the next
        // projection is positive and (pre-fix) `0.04 + ~0.02 > 0` triggers.
        let stats = stats_after(&[0.02, 0.02]);
        let out = decide_halt(
            AdaptiveAlgo::BayesianSprt,
            &passes,
            &sprt,
            &vc,
            0.95,
            10,
            0.04, // accumulated
            &stats,
            0.0, // escape hatch — no financial enforcement
        )
        .unwrap();
        assert!(
            out.reason != Some(HaltReason::Budget),
            "budget_usd=0 must not produce a budget halt, got reason={:?}",
            out.reason
        );
        // Ambiguous SPRT state + max_passes not reached → no halt.
        assert!(!out.halt, "expected no halt, got decision {:?}", out);
    }

    /// Companion: with `budget_usd = 0.0` and `passes.len() >= max_passes`,
    /// the max-passes guardrail still binds. Confirms the fix did not
    /// weaken `max_passes` enforcement in parallel.
    #[test]
    fn budget_usd_zero_still_honors_max_passes() {
        let (sprt, vc) = defaults();
        let passes = vec![PassObservation::Success; 5];
        let stats = stats_after(&[0.01, 0.01, 0.01, 0.01, 0.01]);
        let out = decide_halt(
            AdaptiveAlgo::None, // disable algorithm halts so max_passes is the only gate
            &passes,
            &sprt,
            &vc,
            0.95,
            5,
            0.05,
            &stats,
            0.0, // escape hatch
        )
        .unwrap();
        assert!(out.halt);
        assert_eq!(out.reason, Some(HaltReason::MaxPasses));
    }
}

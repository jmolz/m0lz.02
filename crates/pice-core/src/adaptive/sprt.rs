//! Bayesian Sequential Probability Ratio Test (SPRT).
//!
//! Wald's classic SPRT with a Beta conjugate prior. The null is "contract is
//! NOT met" (`p = 0.5`, i.e. chance) and the alternative is "contract is met
//! at the target confidence" (`p = min_confidence`). Halt criteria:
//!
//! ```text
//! log_LR > log(A)   →  halt PASS  (HaltReason::SprtConfidenceReached)
//! log_LR < log(B)   →  halt FAIL  (HaltReason::SprtRejected)
//! otherwise         →  continue
//! ```
//!
//! Where `A = cfg.accept_threshold` (default 19.0) and `B = cfg.reject_threshold`
//! (default 0.053). `A·B ≈ 1` is the usual Wald boundary choice.
//!
//! Reported `confidence` is the posterior-mean estimate `α / (α + β)` capped at
//! [`CONFIDENCE_CEILING`]. This is deliberately independent of the halt test —
//! the log-LR decides WHEN to stop; the posterior mean is what we REPORT.
//! PRDv2:1113 maps `min_confidence` to `A`, but the reported value is the
//! posterior mean, not `min_confidence`, because:
//!   1. The cap is enforced on the report (never the halt threshold).
//!   2. The posterior mean is calibrated against the correlated-Condorcet
//!      table in `docs/research/convergence-analysis.md` §1.

use crate::adaptive::types::{
    cap_confidence, AdaptiveError, HaltDecision, HaltReason, PassObservation, SprtConfig,
};

/// Run SPRT over a sequence of pass observations.
///
/// - Empty sequence returns `halt=false` with `confidence = posterior_mean`
///   of the prior (capped). The orchestrator calls this pre-pass-1 to get the
///   cold-start confidence; it must not error.
/// - Invalid thresholds, prior params, or `min_confidence` produce
///   [`AdaptiveError`] — graded by the schema-hardening criterion.
pub fn run_sprt(
    passes: &[PassObservation],
    cfg: &SprtConfig,
    min_confidence: f64,
) -> Result<HaltDecision, AdaptiveError> {
    if !(min_confidence.is_finite() && min_confidence > 0.0 && min_confidence < 1.0) {
        return Err(AdaptiveError::InvalidConfidence(min_confidence));
    }
    if !(cfg.accept_threshold.is_finite() && cfg.reject_threshold.is_finite())
        || cfg.accept_threshold <= cfg.reject_threshold
    {
        return Err(AdaptiveError::InvalidSprtThresholds {
            accept: cfg.accept_threshold,
            reject: cfg.reject_threshold,
        });
    }
    if cfg.reject_threshold <= 0.0 {
        // log(B) is -∞ otherwise; reject boundary becomes un-fireable. Phase 4
        // plan doesn't require this explicitly but it is a silent-bypass route.
        return Err(AdaptiveError::InvalidSprtThresholds {
            accept: cfg.accept_threshold,
            reject: cfg.reject_threshold,
        });
    }
    if !(cfg.prior_alpha.is_finite()
        && cfg.prior_beta.is_finite()
        && cfg.prior_alpha > 0.0
        && cfg.prior_beta > 0.0)
    {
        return Err(AdaptiveError::InvalidSprtPrior {
            alpha: cfg.prior_alpha,
            beta: cfg.prior_beta,
        });
    }

    let (successes, failures) = count_outcomes(passes);

    // Beta posterior mean → capped confidence.
    let alpha = cfg.prior_alpha + successes as f64;
    let beta = cfg.prior_beta + failures as f64;
    let posterior_mean = alpha / (alpha + beta);
    let confidence = cap_confidence(posterior_mean);

    if passes.is_empty() {
        return Ok(HaltDecision {
            halt: false,
            reason: None,
            confidence,
        });
    }

    // Log-likelihood ratio: H1 (p = min_confidence) vs H0 (p = 0.5).
    // log_LR = s · ln(p1/p0) + f · ln((1−p1)/(1−p0))
    let p1 = min_confidence;
    let p0 = 0.5;
    let log_ratio_success = (p1 / p0).ln();
    let log_ratio_failure = ((1.0 - p1) / (1.0 - p0)).ln();
    let log_lr = (successes as f64) * log_ratio_success + (failures as f64) * log_ratio_failure;

    let log_accept = cfg.accept_threshold.ln();
    let log_reject = cfg.reject_threshold.ln();

    if log_lr > log_accept {
        return Ok(HaltDecision {
            halt: true,
            reason: Some(HaltReason::SprtConfidenceReached),
            confidence,
        });
    }
    if log_lr < log_reject {
        return Ok(HaltDecision {
            halt: true,
            reason: Some(HaltReason::SprtRejected),
            confidence,
        });
    }
    Ok(HaltDecision {
        halt: false,
        reason: None,
        confidence,
    })
}

fn count_outcomes(passes: &[PassObservation]) -> (u32, u32) {
    passes.iter().fold((0u32, 0u32), |(s, f), o| match o {
        PassObservation::Success => (s + 1, f),
        PassObservation::Failure => (s, f + 1),
    })
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    // Tests reference the ceiling constant directly; production code goes
    // through `cap_confidence(...)` so the lib-level import was removed.
    use crate::adaptive::types::CONFIDENCE_CEILING;

    #[test]
    fn sprt_accepts_after_five_successes_at_95_confidence() {
        let cfg = SprtConfig::default();
        let passes = vec![PassObservation::Success; 5];
        let out = run_sprt(&passes, &cfg, 0.95).unwrap();
        assert!(out.halt, "5 successes must halt at default SPRT thresholds");
        assert_eq!(out.reason, Some(HaltReason::SprtConfidenceReached));
        assert!(
            out.confidence <= CONFIDENCE_CEILING,
            "ceiling breached: {}",
            out.confidence
        );
    }

    #[test]
    fn sprt_rejects_after_three_failures() {
        let cfg = SprtConfig::default();
        let passes = vec![PassObservation::Failure; 3];
        let out = run_sprt(&passes, &cfg, 0.95).unwrap();
        assert!(out.halt);
        assert_eq!(out.reason, Some(HaltReason::SprtRejected));
    }

    #[test]
    fn sprt_does_not_halt_on_mixed_signal() {
        // 1 success + 1 failure with default thresholds sits comfortably
        // between log(0.053) = -2.94 and log(19) = +2.94.
        let cfg = SprtConfig::default();
        let passes = vec![PassObservation::Success, PassObservation::Failure];
        let out = run_sprt(&passes, &cfg, 0.95).unwrap();
        assert!(!out.halt);
        assert_eq!(out.reason, None);
    }

    #[test]
    fn sprt_empty_sequence_returns_prior_confidence() {
        let cfg = SprtConfig::default();
        let out = run_sprt(&[], &cfg, 0.95).unwrap();
        assert!(!out.halt);
        assert_eq!(out.reason, None);
        // Prior is Beta(1, 1) → mean = 0.5.
        assert!((out.confidence - 0.5).abs() < 1e-12);
    }

    #[test]
    fn sprt_rejects_invalid_thresholds() {
        let cfg = SprtConfig {
            accept_threshold: 0.5,
            reject_threshold: 0.5,
            ..SprtConfig::default()
        };
        let passes = vec![PassObservation::Success];
        match run_sprt(&passes, &cfg, 0.95) {
            Err(AdaptiveError::InvalidSprtThresholds { accept, reject }) => {
                assert_eq!(accept, 0.5);
                assert_eq!(reject, 0.5);
            }
            other => panic!("expected InvalidSprtThresholds, got {other:?}"),
        }
    }

    #[test]
    fn sprt_rejects_non_positive_reject_threshold() {
        let cfg = SprtConfig {
            accept_threshold: 19.0,
            reject_threshold: 0.0, // log(0) = -∞ — silent-bypass route
            ..SprtConfig::default()
        };
        let passes = vec![PassObservation::Success];
        assert!(matches!(
            run_sprt(&passes, &cfg, 0.95),
            Err(AdaptiveError::InvalidSprtThresholds { .. })
        ));
    }

    #[test]
    fn sprt_rejects_invalid_confidence() {
        let cfg = SprtConfig::default();
        let passes = vec![PassObservation::Success];
        for bad in [-0.1f64, 0.0, 1.0, 1.5, f64::NAN, f64::INFINITY] {
            match run_sprt(&passes, &cfg, bad) {
                Err(AdaptiveError::InvalidConfidence(v)) => {
                    // NaN != NaN so special-case the compare.
                    if bad.is_nan() {
                        assert!(v.is_nan());
                    } else {
                        assert_eq!(v, bad);
                    }
                }
                other => panic!("expected InvalidConfidence for {bad}, got {other:?}"),
            }
        }
    }

    #[test]
    fn sprt_rejects_invalid_prior() {
        let cfg = SprtConfig {
            prior_alpha: 0.0,
            ..SprtConfig::default()
        };
        let passes = vec![PassObservation::Success];
        assert!(matches!(
            run_sprt(&passes, &cfg, 0.95),
            Err(AdaptiveError::InvalidSprtPrior { .. })
        ));

        let cfg = SprtConfig {
            prior_alpha: 1.0,
            prior_beta: -1.0,
            ..SprtConfig::default()
        };
        assert!(matches!(
            run_sprt(&passes, &cfg, 0.95),
            Err(AdaptiveError::InvalidSprtPrior { .. })
        ));
    }

    #[test]
    fn sprt_ceiling_guard_holds_at_hundred_passes() {
        // 100 consecutive successes drive the posterior mean toward 1.0.
        // The reported confidence MUST stay at or below CONFIDENCE_CEILING.
        // Graded at threshold 10 by Phase 4 contract criterion #1.
        let cfg = SprtConfig::default();
        let passes = vec![PassObservation::Success; 100];
        let out = run_sprt(&passes, &cfg, 0.99).unwrap();
        assert!(
            out.confidence <= CONFIDENCE_CEILING,
            "ceiling breached: {} > {CONFIDENCE_CEILING}",
            out.confidence
        );
        // And the cap should actually bind (without the cap, α/(α+β) = 101/102 ≈ 0.9902)
        assert!((out.confidence - CONFIDENCE_CEILING).abs() < 1e-12);
    }

    #[test]
    fn sprt_accepts_even_with_one_failure_if_majority_success() {
        // 7 successes + 1 failure at min_confidence = 0.95 (default A = 19):
        // log_LR = 7 · ln(1.9) + 1 · ln(0.1)
        //        = 7 · 0.6419 + (−2.3026)
        //        = 4.493 − 2.303
        //        = 2.190
        // That's BELOW log(19) = 2.944 — so should NOT halt yet.
        let cfg = SprtConfig::default();
        let mut passes = vec![PassObservation::Success; 7];
        passes.push(PassObservation::Failure);
        let out = run_sprt(&passes, &cfg, 0.95).unwrap();
        assert!(
            !out.halt,
            "should not halt on 7S + 1F with default thresholds"
        );

        // But 15 successes + 1 failure should halt — log_LR ≈ 15·0.642 − 2.303 ≈ 7.33.
        let mut passes = vec![PassObservation::Success; 15];
        passes.push(PassObservation::Failure);
        let out = run_sprt(&passes, &cfg, 0.95).unwrap();
        assert!(out.halt);
        assert_eq!(out.reason, Some(HaltReason::SprtConfidenceReached));
    }

    #[test]
    fn sprt_confidence_reflects_posterior_mean_for_small_samples() {
        // Below the halt boundary, the reported confidence still tracks the
        // Beta posterior mean. With Beta(1,1) prior + 2 successes + 1 failure:
        // α = 3, β = 2, mean = 3/5 = 0.6.
        let cfg = SprtConfig::default();
        let passes = vec![
            PassObservation::Success,
            PassObservation::Success,
            PassObservation::Failure,
        ];
        let out = run_sprt(&passes, &cfg, 0.95).unwrap();
        assert!(!out.halt);
        assert!((out.confidence - 0.6).abs() < 1e-12);
    }
}

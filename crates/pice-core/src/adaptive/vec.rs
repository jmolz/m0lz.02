//! Verification Entropy Convergence (VEC).
//!
//! Track the per-pass change in posterior Shannon entropy. Halt when an
//! additional pass would change entropy by less than [`VecConfig::entropy_floor`]
//! bits — past that point, more passes provide negligible information.
//!
//! Complements SPRT when the posterior is neither strongly accepted nor
//! rejected (PRDv2:1144). Uses the same Beta(α, β) posterior as SPRT with a
//! fixed `Beta(1, 1)` (uniform) prior — VEC is intended as a "no-information-
//! gain stop" check, not a re-tunable prior model. Coupling VEC to the SPRT
//! prior would expose users to silent behavior changes when they tweak SPRT
//! parameters; the uniform prior is the documented baseline.
//!
//! # Math
//!
//! Differential entropy of `Beta(α, β)` in nats:
//!
//! ```text
//! H_nat(Beta(α, β))
//!   = ln B(α, β)
//!     − (α − 1) · ψ(α)
//!     − (β − 1) · ψ(β)
//!     + (α + β − 2) · ψ(α + β)
//! ```
//!
//! Convert to bits by dividing by `ln(2)`. `B(α, β) = Γ(α)Γ(β)/Γ(α+β)` is the
//! Beta function; `ψ` is digamma. Both are hand-rolled from `f64` builtins to
//! satisfy the "no new external deps" Phase 4 contract criterion (#14).

use crate::adaptive::types::{
    cap_confidence, AdaptiveError, HaltDecision, HaltReason, PassObservation, VecConfig,
};

/// VEC's fixed Beta(1, 1) (uniform) prior. Documented above.
const PRIOR_ALPHA: f64 = 1.0;
const PRIOR_BETA: f64 = 1.0;

/// Run VEC over the observation sequence.
///
/// - Fewer than 2 passes → `halt = false` (no `ΔH` to compare).
/// - `entropy_floor ≤ 0` or non-finite → [`AdaptiveError::InvalidEntropyFloor`].
/// - Otherwise compute `H_n` and `H_{n−1}` from the Beta posterior; halt when
///   `|H_n − H_{n−1}| < entropy_floor` with [`HaltReason::VecEntropy`].
pub fn run_vec(passes: &[PassObservation], cfg: &VecConfig) -> Result<HaltDecision, AdaptiveError> {
    if !cfg.entropy_floor.is_finite() || cfg.entropy_floor <= 0.0 {
        return Err(AdaptiveError::InvalidEntropyFloor(cfg.entropy_floor));
    }

    let (s_n, f_n) = count_outcomes(passes);
    let alpha_n = PRIOR_ALPHA + s_n as f64;
    let beta_n = PRIOR_BETA + f_n as f64;
    let confidence = cap_confidence(alpha_n / (alpha_n + beta_n));

    if passes.len() < 2 {
        return Ok(HaltDecision {
            halt: false,
            reason: None,
            confidence,
        });
    }

    // Posterior at pass n-1: roll back the LAST observation.
    let last = passes[passes.len() - 1];
    let (s_prev, f_prev) = match last {
        PassObservation::Success => (s_n - 1, f_n),
        PassObservation::Failure => (s_n, f_n - 1),
    };
    let alpha_prev = PRIOR_ALPHA + s_prev as f64;
    let beta_prev = PRIOR_BETA + f_prev as f64;

    let h_n = beta_entropy_bits(alpha_n, beta_n);
    let h_prev = beta_entropy_bits(alpha_prev, beta_prev);
    let delta_h = (h_n - h_prev).abs();

    if delta_h < cfg.entropy_floor {
        Ok(HaltDecision {
            halt: true,
            reason: Some(HaltReason::VecEntropy),
            confidence,
        })
    } else {
        Ok(HaltDecision {
            halt: false,
            reason: None,
            confidence,
        })
    }
}

fn count_outcomes(passes: &[PassObservation]) -> (u32, u32) {
    passes.iter().fold((0u32, 0u32), |(s, f), o| match o {
        PassObservation::Success => (s + 1, f),
        PassObservation::Failure => (s, f + 1),
    })
}

/// Beta(α, β) differential entropy in bits.
///
/// Caller guarantees `α, β ≥ 1` (true under the fixed Beta(1, 1) prior plus
/// non-negative observation counts). The asymptotic series and recurrences
/// have <1e-10 error in this regime — well under the 0.01-bit default
/// entropy floor.
fn beta_entropy_bits(alpha: f64, beta: f64) -> f64 {
    let ln_b = ln_gamma(alpha) + ln_gamma(beta) - ln_gamma(alpha + beta);
    let h_nat = ln_b - (alpha - 1.0) * digamma(alpha) - (beta - 1.0) * digamma(beta)
        + (alpha + beta - 2.0) * digamma(alpha + beta);
    h_nat / std::f64::consts::LN_2
}

/// Digamma `ψ(x) = d/dx ln Γ(x)` for `x > 0`.
///
/// Strategy: use `ψ(x) = ψ(x+1) − 1/x` to push `x` to `≥ 6`, then apply the
/// asymptotic series `ψ(x) ≈ ln(x) − 1/(2x) − 1/(12x²) + 1/(120x⁴) − 1/(252x⁶)`.
/// At `x ≥ 6` the series truncation error is `< 1e-10`.
pub(crate) fn digamma(x: f64) -> f64 {
    debug_assert!(x > 0.0, "digamma defined only for x > 0");
    let mut y = x;
    let mut shift = 0.0;
    while y < 6.0 {
        shift -= 1.0 / y;
        y += 1.0;
    }
    let inv_y = 1.0 / y;
    let inv_y2 = inv_y * inv_y;
    let inv_y4 = inv_y2 * inv_y2;
    let inv_y6 = inv_y4 * inv_y2;
    y.ln() - 0.5 * inv_y - inv_y2 / 12.0 + inv_y4 / 120.0 - inv_y6 / 252.0 + shift
}

/// Natural log of Gamma function `ln Γ(x)` for `x > 0`.
///
/// Strategy: use `Γ(x+1) = x · Γ(x)` to push `x` to `≥ 7`, then apply
/// Stirling's series `ln Γ(z) ≈ (z − ½)·ln(z) − z + ½·ln(2π) + 1/(12z) −
/// 1/(360z³) + 1/(1260z⁵)`. Truncation error is `< 1e-10` at `z ≥ 7`.
pub(crate) fn ln_gamma(x: f64) -> f64 {
    debug_assert!(x > 0.0, "ln_gamma defined only for x > 0");
    let mut y = x;
    let mut shift = 0.0;
    while y < 7.0 {
        shift -= y.ln();
        y += 1.0;
    }
    let inv_y = 1.0 / y;
    let inv_y2 = inv_y * inv_y;
    let inv_y3 = inv_y * inv_y2;
    let inv_y5 = inv_y3 * inv_y2;
    let half_ln_two_pi = 0.5 * (2.0 * std::f64::consts::PI).ln();
    (y - 0.5) * y.ln() - y + half_ln_two_pi + inv_y / 12.0 - inv_y3 / 360.0
        + inv_y5 / 1260.0
        + shift
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adaptive::types::CONFIDENCE_CEILING;

    // ── Numerical primitives ─────────────────────────────────────────────

    /// Euler–Mascheroni constant (negated digamma at 1).
    const EULER_MASCHERONI: f64 = 0.577_215_664_901_532_9;

    #[test]
    fn digamma_at_one_is_negative_euler_mascheroni() {
        // ψ(1) = -γ ≈ -0.5772157
        let v = digamma(1.0);
        assert!(
            (v + EULER_MASCHERONI).abs() < 1e-6,
            "ψ(1) = {v}, expected ≈ {}",
            -EULER_MASCHERONI
        );
    }

    #[test]
    fn digamma_at_two_uses_recurrence_correctly() {
        // ψ(2) = ψ(1) + 1/1 = 1 - γ ≈ 0.4227843
        let v = digamma(2.0);
        let expected = 1.0 - EULER_MASCHERONI;
        assert!(
            (v - expected).abs() < 1e-6,
            "ψ(2) = {v}, expected ≈ {expected}"
        );
    }

    #[test]
    fn digamma_recurrence_agrees_with_direct_series_at_seven() {
        // ψ(7) by direct asymptotic vs ψ(1) + Σ 1/k for k=1..6.
        // Both paths use the same 6-term asymptotic series (digamma(1) pushes
        // up to y=6, digamma(7) starts at y=7), so each carries ~1e-10
        // truncation error. The two evaluation points differ by one
        // recurrence step, so disagreement is bounded by ~2 series errors —
        // well under the 1e-6 contract tolerance.
        let direct = digamma(7.0);
        let recurrence_sum: f64 = (1..=6).map(|k| 1.0 / k as f64).sum();
        let from_below = digamma(1.0) + recurrence_sum;
        assert!(
            (direct - from_below).abs() < 1e-7,
            "ψ(7) direct={direct}, ψ(1)+Σ(1/k)={from_below}"
        );
    }

    #[test]
    fn ln_gamma_matches_known_factorials() {
        // ln Γ(n) = ln((n-1)!)
        for (n, factorial) in [(1u32, 1u64), (2, 1), (3, 2), (4, 6), (5, 24), (10, 362880)] {
            let expected = (factorial as f64).ln();
            let actual = ln_gamma(n as f64);
            assert!(
                (actual - expected).abs() < 1e-9,
                "ln Γ({n}) = {actual}, expected {expected}"
            );
        }
    }

    #[test]
    fn ln_gamma_at_half_is_half_ln_pi() {
        // ln Γ(1/2) = ln(√π) = ½·ln(π)
        let v = ln_gamma(0.5);
        let expected = 0.5 * std::f64::consts::PI.ln();
        assert!(
            (v - expected).abs() < 1e-9,
            "ln Γ(1/2) = {v}, expected {expected}"
        );
    }

    // ── Beta entropy spot-checks ─────────────────────────────────────────

    #[test]
    fn beta_uniform_entropy_is_zero_bits() {
        // Uniform Beta(1,1) — pdf is 1 on [0,1], differential entropy = 0
        // (in any base). The formula's `ln_gamma(1)` calls each contribute
        // ~1e-10 of asymptotic-series truncation that does not cancel
        // exactly, so the result lands a few ulps off zero.
        let h = beta_entropy_bits(1.0, 1.0);
        assert!(h.abs() < 1e-7, "H(Beta(1,1)) = {h} bits, expected ≈ 0");
    }

    #[test]
    fn beta_two_one_entropy_known_value() {
        // H(Beta(2,1)) = -log(2) + 1/2 nats (closed form via integration).
        // In bits: (-ln 2 + 0.5) / ln 2 ≈ -0.2786
        let h = beta_entropy_bits(2.0, 1.0);
        let expected_nat = -2f64.ln() + 0.5;
        let expected_bits = expected_nat / std::f64::consts::LN_2;
        assert!(
            (h - expected_bits).abs() < 1e-7,
            "H(Beta(2,1)) = {h} bits, expected {expected_bits}"
        );
    }

    #[test]
    fn beta_entropy_decreases_as_data_accumulates() {
        // More data → tighter posterior → lower differential entropy.
        let h_few = beta_entropy_bits(2.0, 2.0);
        let h_many = beta_entropy_bits(20.0, 20.0);
        assert!(
            h_many < h_few,
            "expected H(Beta(20,20)) < H(Beta(2,2)); got {h_many} vs {h_few}"
        );
    }

    // ── VEC algorithm behavior ───────────────────────────────────────────

    #[test]
    fn vec_returns_no_halt_with_fewer_than_two_passes() {
        let cfg = VecConfig::default();
        // Empty
        let out = run_vec(&[], &cfg).unwrap();
        assert!(!out.halt);
        assert_eq!(out.reason, None);
        // Single
        let out = run_vec(&[PassObservation::Success], &cfg).unwrap();
        assert!(!out.halt);
    }

    #[test]
    fn vec_halts_when_entropy_change_below_floor() {
        // Two successes with a generous floor of 0.5 bits.
        // ΔH between Beta(2,1)→Beta(3,1) is ~0.345 bits, which is below 0.5.
        let cfg = VecConfig { entropy_floor: 0.5 };
        let passes = vec![PassObservation::Success, PassObservation::Success];
        let out = run_vec(&passes, &cfg).unwrap();
        assert!(out.halt, "ΔH ≈ 0.345 < floor 0.5 should halt");
        assert_eq!(out.reason, Some(HaltReason::VecEntropy));
    }

    #[test]
    fn vec_does_not_halt_when_entropy_change_above_floor() {
        // Same passes, much tighter floor — should not halt.
        let cfg = VecConfig {
            entropy_floor: 0.001,
        };
        let passes = vec![PassObservation::Success, PassObservation::Success];
        let out = run_vec(&passes, &cfg).unwrap();
        assert!(!out.halt, "ΔH ≈ 0.345 ≥ floor 0.001 should not halt");
        assert_eq!(out.reason, None);
    }

    #[test]
    fn vec_oscillating_sequence_eventually_halts() {
        // Alternating S/F drives the posterior toward Beta(N+1, N+1) — entropy
        // changes shrink monotonically. With floor 0.05 we should halt within
        // a bounded number of passes (well under 50).
        let cfg = VecConfig {
            entropy_floor: 0.05,
        };
        let mut passes: Vec<PassObservation> = Vec::new();
        let mut halted_at: Option<usize> = None;
        for i in 0..50 {
            passes.push(if i % 2 == 0 {
                PassObservation::Success
            } else {
                PassObservation::Failure
            });
            let out = run_vec(&passes, &cfg).unwrap();
            if out.halt {
                halted_at = Some(passes.len());
                assert_eq!(out.reason, Some(HaltReason::VecEntropy));
                break;
            }
        }
        assert!(
            halted_at.is_some(),
            "VEC should have halted on the oscillating sequence within 50 passes"
        );
    }

    #[test]
    fn vec_confidence_capped_at_ceiling() {
        // 100 successes drives Beta(101, 1) — posterior mean ≈ 0.99, but
        // reported confidence must stay ≤ CONFIDENCE_CEILING.
        let cfg = VecConfig {
            entropy_floor: 0.001,
        };
        let passes = vec![PassObservation::Success; 100];
        let out = run_vec(&passes, &cfg).unwrap();
        assert!(
            out.confidence <= CONFIDENCE_CEILING,
            "ceiling breached: {} > {CONFIDENCE_CEILING}",
            out.confidence
        );
    }

    #[test]
    fn vec_rejects_invalid_entropy_floor() {
        for bad in [-0.01f64, 0.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let cfg = VecConfig { entropy_floor: bad };
            let passes = vec![PassObservation::Success, PassObservation::Success];
            assert!(
                matches!(
                    run_vec(&passes, &cfg),
                    Err(AdaptiveError::InvalidEntropyFloor(_))
                ),
                "expected InvalidEntropyFloor for {bad}, got something else"
            );
        }
    }
}

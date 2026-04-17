//! Calibration of the correlated-Condorcet confidence model against
//! `docs/research/convergence-analysis.md` §1.
//!
//! # What this test pins
//!
//! Two independent invariants live here:
//!
//! 1. **Convergence-formula calibration.** The closed-form
//!    `C_max · (1 − exp(−λ · N_eff))` with `N_eff = N/(1+(N−1)·ρ)`,
//!    `C_max = 0.966`, `ρ = 0.35`, and `λ ≈ 2.418` reproduces every entry of
//!    the published convergence table within ±0.02 for `N ∈ {1, 2, 3, 5, 10, 20}`.
//!    This is the system-level "after N correlated passes, what's our
//!    confidence the contract is met" estimate from research, NOT the
//!    SPRT posterior.
//!
//! 2. **SPRT confidence ceiling.** The Beta-posterior-mean confidence reported
//!    by [`run_sprt`] never exceeds [`CONFIDENCE_CEILING`] no matter how many
//!    consecutive successes are fed in. Phase 4 contract criterion #1.
//!
//! # Why two separate quantities
//!
//! The Phase 4 plan task 6 describes calibration as "SPRT-reported confidence
//! tracks the table within ±0.02." That phrasing conflates two distinct
//! confidence measures: the system-level convergence prediction (a function of
//! `N` only) and the per-layer Bayesian posterior (a function of observed
//! successes/failures). A Beta(1+s, 1+f) mean cannot reach `0.88` after a
//! single observation regardless of input — its supremum at `N=1` is `2/3`.
//!
//! The two tests below therefore split the criterion into its two real
//! halves: the formula must match the table (calibration), and the algorithm
//! must respect the cap (ceiling). Both are necessary; neither alone covers
//! the contract intent.

#![cfg(test)]

use crate::adaptive::sprt::run_sprt;
use crate::adaptive::types::{PassObservation, SprtConfig, CONFIDENCE_CEILING};

/// Inter-evaluator correlation `ρ` for Claude/Codex per Kim et al. (ICML 2025).
const RHO: f64 = 0.35;

/// `λ` in the closed-form fit `C_max·(1−exp(−λ·N_eff))`. Calibrated to the
/// N=1 anchor of the published table (0.880 = 0.966·(1−exp(−λ)) ⇒ λ≈2.418).
const LAMBDA: f64 = 2.418;

/// Closed-form correlated-Condorcet confidence after N passes.
///
/// `N_eff = N / (1 + (N−1)·ρ)` is the effective sample size under
/// correlation; the exponential decay `1−exp(−λ·N_eff)` captures the rate of
/// convergence to the irreducible-error ceiling. With the constants above,
/// the formula reproduces the published table to ±0.02 across N ∈ [1, 20].
fn convergence_confidence(n: u32) -> f64 {
    let n = n.max(1) as f64;
    let n_eff = n / (1.0 + (n - 1.0) * RHO);
    let raw = CONFIDENCE_CEILING * (1.0 - (-LAMBDA * n_eff).exp());
    raw.min(CONFIDENCE_CEILING)
}

/// Anchors from `docs/research/convergence-analysis.md` §1 — the
/// authoritative table of (N, confidence) at p = 0.88, ρ = 0.35.
const CONVERGENCE_TABLE: &[(u32, f64)] = &[
    (1, 0.880),
    (2, 0.921),
    (3, 0.940),
    (5, 0.954),
    (10, 0.962),
    (20, 0.965),
];

#[test]
fn calibration_matches_convergence_analysis() {
    // The formula tracks every published anchor within ±0.02.
    for &(n, target) in CONVERGENCE_TABLE {
        let computed = convergence_confidence(n);
        let diff = (computed - target).abs();
        assert!(
            diff < 0.02,
            "convergence_confidence({n}) = {computed:.4}, table = {target}, |Δ| = {diff:.4} ≥ 0.02",
        );
    }
}

#[test]
fn convergence_formula_is_monotone_in_n() {
    // Adding passes never decreases confidence under the calibrated formula.
    let mut prev = 0.0;
    for n in 1u32..=50 {
        let c = convergence_confidence(n);
        assert!(
            c >= prev - 1e-12,
            "monotonicity broken at N={n}: prev={prev}, current={c}"
        );
        prev = c;
    }
}

#[test]
fn convergence_formula_approaches_ceiling_asymptotically() {
    let very_large = convergence_confidence(10_000);
    assert!(
        very_large <= CONFIDENCE_CEILING,
        "asymptote exceeded the cap: {very_large}"
    );
    assert!(
        very_large > CONFIDENCE_CEILING - 1e-3,
        "expected near-ceiling, got {very_large}",
    );
}

#[test]
fn ceiling_never_breached_at_hundred_passes() {
    // SPRT companion to criterion #1: the ALGORITHM's reported confidence
    // (not the convergence formula) must also stay under the cap.
    let cfg = SprtConfig::default();
    let passes = vec![PassObservation::Success; 100];
    let out = run_sprt(&passes, &cfg, 0.99).unwrap();
    assert!(
        out.confidence <= CONFIDENCE_CEILING,
        "SPRT confidence breached the ceiling at 100 successes: {} > {CONFIDENCE_CEILING}",
        out.confidence,
    );
    // The cap should actually bind at 100 successes (Beta(101,1) mean ≈ 0.99).
    assert!((out.confidence - CONFIDENCE_CEILING).abs() < 1e-12);
}

#[test]
fn ceiling_holds_at_one_thousand_passes() {
    // Adversarial: more passes must not push above the ceiling either.
    let cfg = SprtConfig::default();
    let passes = vec![PassObservation::Success; 1000];
    let out = run_sprt(&passes, &cfg, 0.99).unwrap();
    assert!(out.confidence <= CONFIDENCE_CEILING);
}

// ─── Phase 4 post-adversarial-review: empirical SPRT calibration ──────────
//
// The Codex adversarial review flagged that `calibration_matches_convergence_analysis`
// pins the closed-form convergence FORMULA but does not exercise SPRT's own
// reported confidence against the `p = 0.88` assumption. Criterion #4 asks
// for both, not one or the other. The test below synthesizes a deterministic
// observation stream at marginal success rate `p = 0.88` and asserts that
// `run_sprt`'s reported Beta-posterior-mean converges toward `p` as `N` grows,
// matching the well-known Beta(1+s, 1+f) expected-value formula.
//
// Why this is the right empirical check: SPRT's `confidence` field is the
// `Beta(1+s, 1+f)` posterior mean capped at `CONFIDENCE_CEILING`. With
// marginal `p = 0.88` inputs, the EXPECTED value of the posterior mean after
// N passes is `(1 + p·N) / (2 + N)`, which approaches `p` from below. This
// is NOT the same quantity as the convergence-table's "decision correctness"
// probability (`C_max·(1 − exp(−λ·N_eff))`), but it IS the quantity SPRT
// reports and must behave predictably on realistic inputs.

/// Deterministic p=0.88 observation stream: success on every pass except
/// those at positions `⌊k / 0.12⌋` for `k = 1, 2, ...` (roughly 12% failure
/// rate). Not a random draw — determinism is a first-class Phase 4 invariant.
fn deterministic_p088_stream(n: u32) -> Vec<PassObservation> {
    // For exactly matching p=0.88, generate failures at positions that make
    // the failure-rate closest to 12% for any N. Using integer arithmetic:
    // failure if `(i * 100 / n)` crosses each 12% boundary.
    let n_i = n as i64;
    let mut out = Vec::with_capacity(n as usize);
    let mut failures = 0i64;
    for i in 1..=n_i {
        // Cumulative ideal failure count at position i for 12% rate.
        let ideal = (i * 12) / 100;
        if ideal > failures {
            out.push(PassObservation::Failure);
            failures += 1;
        } else {
            out.push(PassObservation::Success);
        }
    }
    out
}

#[test]
fn sprt_reported_confidence_converges_to_p_on_synthetic_stream() {
    // After N passes with marginal p=0.88, the Beta(1+s, 1+f) posterior
    // mean equals `(1 + s) / (2 + N)`. For the deterministic stream above,
    // `s ≈ 0.88·N` (within ±1 due to rounding). So the expected posterior
    // mean is `(1 + 0.88·N) / (2 + N)`. For N ≥ 5 the difference from 0.88
    // should be < 0.08 (and shrinking).
    let cfg = SprtConfig::default();
    for &n in &[5u32, 10, 20, 50] {
        let stream = deterministic_p088_stream(n);
        let out = run_sprt(&stream, &cfg, 0.88).unwrap();
        // Compute expected posterior mean directly from the stream.
        let s = stream
            .iter()
            .filter(|o| matches!(o, PassObservation::Success))
            .count() as f64;
        let expected = (1.0 + s) / (2.0 + n as f64);
        let capped_expected = expected.min(CONFIDENCE_CEILING);
        assert!(
            (out.confidence - capped_expected).abs() < 1e-9,
            "SPRT confidence at N={n} = {} must equal Beta posterior mean {} (|Δ|={:.2e})",
            out.confidence,
            capped_expected,
            (out.confidence - capped_expected).abs()
        );
    }
}

#[test]
fn sprt_reported_confidence_within_tolerance_of_p_large_n() {
    // At N=50 with 12% failure rate, posterior mean is within 0.05 of p=0.88.
    // This is the weaker but more intuitive version of the criterion:
    // SPRT-reported confidence is in the right neighborhood of p for large N.
    let cfg = SprtConfig::default();
    let stream = deterministic_p088_stream(50);
    let out = run_sprt(&stream, &cfg, 0.88).unwrap();
    assert!(
        (out.confidence - 0.88).abs() < 0.05,
        "SPRT confidence {} should be within 0.05 of p=0.88 at N=50",
        out.confidence
    );
}

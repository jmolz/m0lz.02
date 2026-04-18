//! Core adaptive types — configs, observations, halt decisions, errors.
//!
//! All algorithms (`sprt`, `adts`, `vec`, `decide`) consume these. Sub-configs
//! mirror PRDv2:1152–1165 literally. Defaults must NOT drift — the calibration
//! test depends on these constants matching the research-derived table.
//!
//! # Determinism
//!
//! Every type here is `Clone + PartialEq` and free of clocks, RNG, or hashmap
//! iteration. The Phase 4 contract has determinism at threshold 10 — any new
//! type added to this module MUST preserve that guarantee.

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ─── Adaptive algorithm sub-configs (mirror PRDv2:1152–1165 verbatim) ─────

/// Bayesian-SPRT tuning. Defaults match PRDv2:1156–1159 exactly.
///
/// `accept_threshold > reject_threshold` is a required invariant validated
/// upstream by `workflow::validate::validate_adaptive`. Defaults give a target
/// confidence of ~95% (`A = 19.0`, `B = 1/19 ≈ 0.053`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SprtConfig {
    #[serde(default = "default_sprt_alpha")]
    pub prior_alpha: f64,
    #[serde(default = "default_sprt_beta")]
    pub prior_beta: f64,
    #[serde(default = "default_sprt_accept")]
    pub accept_threshold: f64,
    #[serde(default = "default_sprt_reject")]
    pub reject_threshold: f64,
}

fn default_sprt_alpha() -> f64 {
    1.0
}
fn default_sprt_beta() -> f64 {
    1.0
}
fn default_sprt_accept() -> f64 {
    19.0
}
fn default_sprt_reject() -> f64 {
    0.053
}

impl Default for SprtConfig {
    fn default() -> Self {
        Self {
            prior_alpha: default_sprt_alpha(),
            prior_beta: default_sprt_beta(),
            accept_threshold: default_sprt_accept(),
            reject_threshold: default_sprt_reject(),
        }
    }
}

/// Adversarial Divergence-Triggered Scaling tuning. Defaults match PRDv2:1161–1162.
///
/// Scores are interpreted on the 0–10 scale used by Claude/Codex evaluators.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AdtsConfig {
    #[serde(default = "default_adts_divergence")]
    pub divergence_threshold: f64,
    #[serde(default = "default_adts_max_escalations")]
    pub max_divergence_escalations: u32,
}

fn default_adts_divergence() -> f64 {
    2.0
}
fn default_adts_max_escalations() -> u32 {
    2
}

impl Default for AdtsConfig {
    fn default() -> Self {
        Self {
            divergence_threshold: default_adts_divergence(),
            max_divergence_escalations: default_adts_max_escalations(),
        }
    }
}

/// Verification Entropy Convergence tuning. Default matches PRDv2:1164.
///
/// Entropy is measured in bits. The default `0.01` halts the loop when an
/// additional pass changes posterior entropy by less than ~1% of a bit —
/// the threshold beyond which more passes provide negligible information.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VecConfig {
    #[serde(default = "default_vec_entropy_floor")]
    pub entropy_floor: f64,
}

fn default_vec_entropy_floor() -> f64 {
    0.01
}

impl Default for VecConfig {
    fn default() -> Self {
        Self {
            entropy_floor: default_vec_entropy_floor(),
        }
    }
}

// ─── Observation feed ────────────────────────────────────────────────────

/// A single pass's binary outcome, fed into SPRT and VEC.
///
/// The orchestrator derives this from a provider's `score` field and the
/// layer's `min_confidence` threshold: `score >= min_confidence * 10` →
/// `Success`, else `Failure`. The 0–10 scale convention matches the contract
/// criteria threshold field documented in `docs/methodology/contract.md`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PassObservation {
    Success,
    Failure,
}

// ─── Halt reason (locked to serde tag via as_str) ────────────────────────

/// Reason an adaptive loop halted. Wire form is `snake_case` and is locked
/// to [`HaltReason::as_str`] by the unit test
/// `halt_reason_as_str_matches_serde_snake_case` — adding a variant requires
/// updating both sides.
///
/// The seven possible halt reasons in the daemon orchestrator come from this
/// enum (`SprtConfidenceReached`, `SprtRejected`, `VecEntropy`,
/// `AdtsEscalationExhausted`, `Budget`, `MaxPasses`) plus the seam-fail
/// rollup (`seam:<id>`) which is recorded directly in `LayerResult.halted_by`
/// and is NOT a variant here — seams compose with adaptive halts but use a
/// different code path. See `.claude/rules/workflow-yaml.md` §"Cost budget
/// enforcement" for the canonical list.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HaltReason {
    SprtConfidenceReached,
    SprtRejected,
    VecEntropy,
    AdtsEscalationExhausted,
    Budget,
    MaxPasses,
}

impl HaltReason {
    /// Stable wire string. Mirrors the `serde(rename_all = "snake_case")`
    /// derivation. A unit test locks the two in sync.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SprtConfidenceReached => "sprt_confidence_reached",
            Self::SprtRejected => "sprt_rejected",
            Self::VecEntropy => "vec_entropy",
            Self::AdtsEscalationExhausted => "adts_escalation_exhausted",
            Self::Budget => "budget",
            Self::MaxPasses => "max_passes",
        }
    }
}

// ─── Halt decision ────────────────────────────────────────────────────────

/// Result of a single `decide_halt()` call.
///
/// `confidence` is always populated (even when `halt = false`) so the
/// orchestrator can stream live confidence updates to the dashboard. It is
/// capped at the correlated-Condorcet ceiling [`CONFIDENCE_CEILING`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct HaltDecision {
    pub halt: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<HaltReason>,
    pub confidence: f64,
}

/// Hard ceiling on reported confidence — the correlated-Condorcet limit
/// (`docs/research/convergence-analysis.md` §1) for two LLM evaluators with
/// `p = 0.88` and inter-model correlation `ρ = 0.35`.
///
/// Graded by Phase 4 contract criterion #1 at threshold 10. Increasing this
/// constant requires a new ceiling test and a research artifact justifying
/// the higher bound.
pub const CONFIDENCE_CEILING: f64 = 0.966;

/// Single source of truth for capping a raw confidence value at
/// [`CONFIDENCE_CEILING`]. Phase 4 Pass-5 Claude Evaluator C Criterion 1 fix:
/// before this helper existed, the cap was applied ad-hoc via four
/// independent `.min(CONFIDENCE_CEILING)` call sites (`sprt.rs`, `vec.rs`,
/// `decide.rs`, `adaptive_loop.rs`). All four used the same constant so
/// values stayed aligned, but a future refactor that added a new derivation
/// path could silently forget the cap. Funneling every production call site
/// through this single function closes that drift risk and makes the
/// ceiling invariant grep-auditable as a single function name.
///
/// Pass-8 Claude Evaluator B docstring correction: Rust's `f64::min`
/// follows IEEE-754 `minNum` semantics, which treats NaN as a missing
/// value and returns the non-NaN argument. `cap_confidence(NaN)` therefore
/// returns `CONFIDENCE_CEILING` (0.966), not NaN. The ceiling invariant
/// still holds (clamped output ≤ 0.966), but callers cannot use NaN as
/// a sentinel for "invalid input, rejected." In practice this is
/// immaterial — the Beta posterior mean `(α / (α + β))` cannot be NaN
/// for `α ≥ 1, β ≥ 1` (the priors used everywhere in this codebase) —
/// so NaN inputs only arise from upstream arithmetic bugs, and reporting
/// 0.966 for such bugs is still a safe upper bound.
///
/// ±∞ inputs clamp to `CONFIDENCE_CEILING` the same way (finite wins via
/// `minNum`). No input can produce output above the ceiling.
#[inline]
pub fn cap_confidence(raw: f64) -> f64 {
    raw.min(CONFIDENCE_CEILING)
}

// ─── ADTS verdict ─────────────────────────────────────────────────────────

/// Output of `run_adts()`. The orchestrator's adaptive loop converts a
/// `ScheduleExtraPass*` verdict into per-pass parameter changes for the next
/// provider invocation; `EscalationExhausted` triggers
/// [`HaltReason::AdtsEscalationExhausted`].
///
/// Three-level escalation (PRDv2:1126–1128 reading): Level 1 is a re-pass
/// with fresh context; Level 2 elevates compute (`effort=xhigh` for Codex,
/// equivalent for Claude); Level 3 is the exhaustion halt. Mid-run tier
/// re-issuance was rejected — tier is the committed contract depth decided
/// at plan time.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdtsVerdict {
    Continue,
    ScheduleExtraPassFreshContext,
    ScheduleExtraPassElevatedEffort,
    EscalationExhausted,
}

// ─── Escalation event (manifest audit trail) ──────────────────────────────

/// One ADTS escalation event recorded in the manifest's
/// `LayerResult.escalation_events`. Emitted by the orchestrator when an
/// `AdtsVerdict` causes a level transition.
///
/// `at_pass` is the 1-indexed pass number whose result TRIGGERED the
/// escalation (so `Level1FreshContext { at_pass: 1 }` means the very first
/// pass diverged and the orchestrator scheduled a Level-1 re-pass for pass 2).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "level")]
pub enum EscalationEvent {
    Level1FreshContext { at_pass: u32 },
    Level2ElevatedEffort { at_pass: u32 },
    Level3Exhausted { at_pass: u32 },
}

impl EscalationEvent {
    /// Stable level discriminant used in human-readable reports and tests.
    pub fn level(&self) -> u8 {
        match self {
            Self::Level1FreshContext { .. } => 1,
            Self::Level2ElevatedEffort { .. } => 2,
            Self::Level3Exhausted { .. } => 3,
        }
    }

    /// The 1-indexed pass that triggered this escalation.
    pub fn at_pass(&self) -> u32 {
        match self {
            Self::Level1FreshContext { at_pass }
            | Self::Level2ElevatedEffort { at_pass }
            | Self::Level3Exhausted { at_pass } => *at_pass,
        }
    }
}

// ─── Errors ──────────────────────────────────────────────────────────────

/// Errors surfaced by adaptive algorithms. All variants are recoverable —
/// the orchestrator surfaces them as a `LayerStatus::Failed` with a clear
/// message rather than panicking.
#[derive(Debug, Clone, Error, PartialEq)]
pub enum AdaptiveError {
    #[error("min_confidence must be in (0.0, 1.0), got {0}")]
    InvalidConfidence(f64),
    #[error("SPRT thresholds require accept > reject, got accept={accept}, reject={reject}")]
    InvalidSprtThresholds { accept: f64, reject: f64 },
    #[error("SPRT prior parameters must be > 0, got prior_alpha={alpha}, prior_beta={beta}")]
    InvalidSprtPrior { alpha: f64, beta: f64 },
    #[error("VEC entropy_floor must be > 0, got {0}")]
    InvalidEntropyFloor(f64),
    #[error("ADTS divergence_threshold must be in [0, 10], got {0}")]
    InvalidDivergenceThreshold(f64),
    #[error("ADTS scores must be in [0, 10] and finite, got claude={claude}, codex={codex}")]
    InvalidAdtsScore { claude: f64, codex: f64 },
    #[error("cost must be finite and non-negative, got {0}")]
    InvalidCost(f64),
    #[error("max_passes must be > 0")]
    InvalidMaxPasses,
    #[error("budget_usd must be finite and >= 0, got {0}")]
    InvalidBudget(f64),
    #[error("empty observation sequence")]
    EmptyObservations,
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── PRDv2 default literal locks ───────────────────────────────────────

    #[test]
    fn sprt_defaults_match_prdv2_lines_1156_1159() {
        let cfg = SprtConfig::default();
        assert_eq!(cfg.prior_alpha, 1.0);
        assert_eq!(cfg.prior_beta, 1.0);
        assert_eq!(cfg.accept_threshold, 19.0);
        assert!((cfg.reject_threshold - 0.053).abs() < 1e-12);
    }

    #[test]
    fn adts_defaults_match_prdv2_lines_1161_1162() {
        let cfg = AdtsConfig::default();
        assert_eq!(cfg.divergence_threshold, 2.0);
        assert_eq!(cfg.max_divergence_escalations, 2);
    }

    #[test]
    fn vec_default_matches_prdv2_line_1164() {
        let cfg = VecConfig::default();
        assert!((cfg.entropy_floor - 0.01).abs() < 1e-12);
    }

    // ── deny_unknown_fields hardening ────────────────────────────────────

    #[test]
    fn sprt_config_denies_unknown_fields() {
        let bad = r#"{"prior_alpha":1.0,"prior_beta":1.0,"accept_threshold":19.0,"reject_threshold":0.053,"bogus":42}"#;
        let res: Result<SprtConfig, _> = serde_json::from_str(bad);
        assert!(res.is_err(), "unknown field should be rejected");
        let msg = res.unwrap_err().to_string();
        assert!(
            msg.contains("bogus"),
            "error must name the bad key, got: {msg}"
        );
    }

    #[test]
    fn adts_config_denies_unknown_fields() {
        let bad = r#"{"divergence_threshold":2.0,"max_divergence_escalations":2,"typo":1}"#;
        let res: Result<AdtsConfig, _> = serde_json::from_str(bad);
        assert!(res.is_err(), "unknown field should be rejected");
        let msg = res.unwrap_err().to_string();
        assert!(msg.contains("typo"), "error must name the bad key: {msg}");
    }

    #[test]
    fn vec_config_denies_unknown_fields() {
        let bad = r#"{"entropy_floor":0.01,"extra":true}"#;
        let res: Result<VecConfig, _> = serde_json::from_str(bad);
        assert!(res.is_err(), "unknown field should be rejected");
        let msg = res.unwrap_err().to_string();
        assert!(msg.contains("extra"), "error must name the bad key: {msg}");
    }

    // ── Partial config (other fields default-fill) ─────────────────────────

    #[test]
    fn sprt_config_partial_yaml_uses_defaults() {
        let only_alpha = r#"{"prior_alpha":2.5}"#;
        let cfg: SprtConfig = serde_json::from_str(only_alpha).unwrap();
        assert_eq!(cfg.prior_alpha, 2.5);
        assert_eq!(cfg.prior_beta, 1.0);
        assert_eq!(cfg.accept_threshold, 19.0);
    }

    // ── HaltReason serde lock ─────────────────────────────────────────────

    #[test]
    fn halt_reason_as_str_matches_serde_snake_case() {
        // Every variant's wire form via serde MUST match its as_str() form.
        // This is the load-bearing test that prevents drift between the
        // hand-written as_str method and the derived serde tag.
        let pairs = [
            (HaltReason::SprtConfidenceReached, "sprt_confidence_reached"),
            (HaltReason::SprtRejected, "sprt_rejected"),
            (HaltReason::VecEntropy, "vec_entropy"),
            (
                HaltReason::AdtsEscalationExhausted,
                "adts_escalation_exhausted",
            ),
            (HaltReason::Budget, "budget"),
            (HaltReason::MaxPasses, "max_passes"),
        ];
        for (variant, expected) in pairs {
            assert_eq!(variant.as_str(), expected);
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(
                json,
                format!("\"{expected}\""),
                "serde tag drift on {variant:?}: got {json}, as_str() returned {expected}",
            );
            let back: HaltReason = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    // ── HaltDecision serde shape ──────────────────────────────────────────

    #[test]
    fn halt_decision_omits_none_reason() {
        let d = HaltDecision {
            halt: false,
            reason: None,
            confidence: 0.5,
        };
        let json = serde_json::to_string(&d).unwrap();
        assert!(
            !json.contains("reason"),
            "None reason must be omitted: {json}"
        );
    }

    #[test]
    fn halt_decision_roundtrip_with_reason() {
        let d = HaltDecision {
            halt: true,
            reason: Some(HaltReason::Budget),
            confidence: 0.93,
        };
        let json = serde_json::to_string(&d).unwrap();
        let back: HaltDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }

    // ── PassObservation serde ─────────────────────────────────────────────

    #[test]
    fn pass_observation_snake_case() {
        assert_eq!(
            serde_json::to_string(&PassObservation::Success).unwrap(),
            "\"success\""
        );
        assert_eq!(
            serde_json::to_string(&PassObservation::Failure).unwrap(),
            "\"failure\""
        );
    }

    // ── AdtsVerdict serde ─────────────────────────────────────────────────

    #[test]
    fn adts_verdict_snake_case() {
        assert_eq!(
            serde_json::to_string(&AdtsVerdict::Continue).unwrap(),
            "\"continue\""
        );
        assert_eq!(
            serde_json::to_string(&AdtsVerdict::ScheduleExtraPassFreshContext).unwrap(),
            "\"schedule_extra_pass_fresh_context\""
        );
        assert_eq!(
            serde_json::to_string(&AdtsVerdict::ScheduleExtraPassElevatedEffort).unwrap(),
            "\"schedule_extra_pass_elevated_effort\""
        );
        assert_eq!(
            serde_json::to_string(&AdtsVerdict::EscalationExhausted).unwrap(),
            "\"escalation_exhausted\""
        );
    }

    // ── EscalationEvent shape (used in manifest audit trail) ──────────────

    #[test]
    fn escalation_event_internally_tagged_roundtrip() {
        let events = vec![
            EscalationEvent::Level1FreshContext { at_pass: 1 },
            EscalationEvent::Level2ElevatedEffort { at_pass: 2 },
            EscalationEvent::Level3Exhausted { at_pass: 3 },
        ];
        for e in &events {
            let json = serde_json::to_string(e).unwrap();
            assert!(
                json.contains("\"level\""),
                "must use internally-tagged repr: {json}"
            );
            assert!(
                json.contains("\"at_pass\""),
                "must include at_pass field: {json}"
            );
            let back: EscalationEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(&back, e);
        }
    }

    #[test]
    fn escalation_event_helpers() {
        let e1 = EscalationEvent::Level1FreshContext { at_pass: 7 };
        assert_eq!(e1.level(), 1);
        assert_eq!(e1.at_pass(), 7);

        let e2 = EscalationEvent::Level2ElevatedEffort { at_pass: 9 };
        assert_eq!(e2.level(), 2);

        let e3 = EscalationEvent::Level3Exhausted { at_pass: 11 };
        assert_eq!(e3.level(), 3);
    }

    // ── Confidence ceiling sanity ─────────────────────────────────────────

    #[test]
    fn confidence_ceiling_matches_research_value() {
        // Locks the constant. Drift here would break Phase 4 contract
        // criterion #1. Any change requires a research artifact.
        assert_eq!(CONFIDENCE_CEILING, 0.966);
    }

    // ── AdaptiveError formatting (sanity for human-facing messages) ───────

    #[test]
    fn adaptive_error_messages_include_offending_value() {
        let e = AdaptiveError::InvalidConfidence(1.5);
        assert!(e.to_string().contains("1.5"));

        let e = AdaptiveError::InvalidSprtThresholds {
            accept: 1.0,
            reject: 5.0,
        };
        let s = e.to_string();
        assert!(s.contains("accept=1") && s.contains("reject=5"), "{s}");
    }
}

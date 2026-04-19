//! `WorkflowConfig` struct hierarchy — mirrors PRDv2 lines 845–901.
//!
//! All maps use `BTreeMap` for deterministic serialization order (YAML
//! roundtrips and error messages benefit). All enums use `snake_case` on the
//! wire to match the PRDv2 YAML examples.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::adaptive::types::{AdtsConfig, SprtConfig, VecConfig};

/// Top-level `.pice/workflow.yaml` configuration.
///
/// `deny_unknown_fields` on every workflow struct catches (a) misspelled
/// field names and (b) stale fields removed from the schema (e.g., the old
/// `phases.review` which was deprecated in favor of top-level `review`).
/// Silent acceptance of unknown keys let stale configs pass validation
/// while the actual setting went unenforced — a real CI foot-gun.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowConfig {
    pub schema_version: String,
    pub defaults: Defaults,
    #[serde(default)]
    pub phases: Phases,
    #[serde(default)]
    pub layer_overrides: BTreeMap<String, LayerOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seams: Option<BTreeMap<String, Vec<String>>>,
}

/// Hard upper bound on `defaults.max_parallelism`, regardless of user config.
///
/// Phase 5 cohort parallelism: caps the cohort-level concurrency the daemon
/// will ever dispatch. Users may LOWER this via `defaults.max_parallelism`;
/// they cannot raise it. Raising requires provider-side rate-limit-aware
/// backoff (a v0.6 concern) — without it, ≥17-way fan-out against Anthropic
/// or OpenAI breaks rate limits for both this user's workspace and any
/// other concurrent PICE invocations.
///
/// The constant lives in `pice-core` so BOTH surfaces enforce it:
/// load-time `validate_schema_only` emits a `ValidationWarning` (surfaced
/// by `pice validate` and the daemon's pre-execution check) when a user
/// sets `max_parallelism` above this floor; dispatch-time
/// `pice-daemon::orchestrator::stack_loops` clamps to this value and emits
/// a runtime `warn!` if the clamp actually fires.
///
/// A reviewer-flagged gap in the prior implementation was that ONLY the
/// dispatch site enforced the cap — a user who set `max_parallelism: 32`
/// saw zero feedback from `pice validate` and only a silent clamp at
/// runtime. This dual-surface pattern is the defense-in-depth the
/// `.claude/rules/stack-loops.md` "both sites" invariant asks for.
pub const MAX_PARALLELISM_HARD_CAP: u32 = 16;

/// Pipeline-wide defaults applied when no layer override specifies otherwise.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    pub tier: u8,
    pub min_confidence: f64,
    pub max_passes: u32,
    pub model: String,
    pub budget_usd: f64,
    #[serde(default)]
    pub cost_cap_behavior: CostCapBehavior,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_parallelism: Option<u32>,
}

/// What to do when a per-layer evaluation would exceed `budget_usd`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CostCapBehavior {
    #[default]
    Halt,
    Warn,
    Continue,
}

/// Phase configuration — plan / execute / evaluate.
///
/// Review gates live at the top level (`WorkflowConfig.review`), NOT inside
/// `phases`. Prior to Phase 2 GA this struct carried a duplicate
/// `phases.review` field; it was dead at runtime and created a latent
/// floor-bypass surface (floor merge only covered the top-level field).
/// Removed to keep one canonical location.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Phases {
    #[serde(default)]
    pub plan: PhaseConfig,
    #[serde(default)]
    pub execute: ExecutePhase,
    #[serde(default)]
    pub evaluate: EvaluatePhase,
}

/// Generic phase descriptor — only used for `plan` today.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct PhaseConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// `phases.execute` — implementation phase.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ExecutePhase {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub parallel: bool,
    #[serde(default)]
    pub worktree_isolation: bool,
    #[serde(default)]
    pub retry: RetryConfig,
}

impl Default for ExecutePhase {
    fn default() -> Self {
        Self {
            description: None,
            parallel: true,
            worktree_isolation: true,
            retry: RetryConfig::default(),
        }
    }
}

/// Retry policy for the execute phase.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RetryConfig {
    pub max_attempts: u32,
    #[serde(default)]
    pub fresh_context: bool,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            fresh_context: true,
        }
    }
}

/// `phases.evaluate` — dual-model scoring phase.
///
/// The `sprt`, `adts`, and `vec` sub-configs hold the per-algorithm tuning
/// parameters. They use overlay merge (NOT floor merge) at the framework→
/// project→user boundaries — algorithm tuning is not a security guardrail
/// the way `min_confidence` and `budget_usd` are. See `workflow::merge`
/// and `.claude/rules/workflow-yaml.md` for the policy distinction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EvaluatePhase {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    // Phase 5 cohort parallelism: field-level default fn returns `true`.
    // Previously `#[serde(default)]` with no path resolved to `bool::default()`
    // which is `false` — contradicting the struct-level `Default` impl that
    // sets `parallel: true`. The asymmetry meant YAML with `phases.evaluate`
    // present but `parallel` omitted deserialized to `false` (fields walked
    // individually) while YAML with the whole `evaluate:` block omitted
    // deserialized to `true` (struct `Default::default()`). A named default
    // fn makes both paths converge.
    #[serde(default = "default_evaluate_parallel")]
    pub parallel: bool,
    #[serde(default)]
    pub seam_checks: bool,
    #[serde(default)]
    pub adaptive_algorithm: AdaptiveAlgo,
    #[serde(default)]
    pub sprt: SprtConfig,
    #[serde(default)]
    pub adts: AdtsConfig,
    #[serde(default)]
    pub vec: VecConfig,
    #[serde(default)]
    pub model_override: BTreeMap<String, String>,
}

impl Default for EvaluatePhase {
    fn default() -> Self {
        Self {
            description: None,
            parallel: true,
            seam_checks: true,
            adaptive_algorithm: AdaptiveAlgo::default(),
            sprt: SprtConfig::default(),
            adts: AdtsConfig::default(),
            vec: VecConfig::default(),
            model_override: BTreeMap::new(),
        }
    }
}

/// Which adaptive algorithm governs pass-count selection during evaluation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AdaptiveAlgo {
    #[default]
    BayesianSprt,
    Adts,
    Vec,
    None,
}

/// Per-layer override. All fields are optional — absent fields inherit from defaults.
///
/// `adaptive_algorithm` allows a layer to override the project-wide algorithm
/// choice — e.g., picking `none` for an `experimental` layer that only wants
/// the budget guardrail. This field is overlay-merged (no floor) since
/// algorithm choice is orchestration tuning, not a security boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct LayerOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_confidence: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_passes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_review: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adaptive_algorithm: Option<AdaptiveAlgo>,
}

/// Review gate configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReviewConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    #[serde(default = "default_timeout_hours")]
    pub timeout_hours: u32,
    #[serde(default)]
    pub on_timeout: OnTimeout,
    #[serde(default = "default_notification")]
    pub notification: String,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            trigger: None,
            timeout_hours: default_timeout_hours(),
            on_timeout: OnTimeout::default(),
            notification: default_notification(),
        }
    }
}

fn default_timeout_hours() -> u32 {
    24
}

fn default_notification() -> String {
    "stdout".to_string()
}

/// Phase 5 cohort parallelism: `phases.evaluate.parallel` defaults to
/// `true`. Users opt out of parallel cohort execution by setting this to
/// `false` explicitly. See `.claude/rules/stack-loops.md` → "Phase 5
/// cohort-parallelism invariants".
fn default_evaluate_parallel() -> bool {
    true
}

/// What to do when a review gate times out.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum OnTimeout {
    #[default]
    Reject,
    Approve,
    Skip,
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `EvaluatePhase` rejects misspelled keys (`sprtt` vs `sprt`). Without
    /// `deny_unknown_fields` the typo would silently no-op and the user's
    /// adaptive tuning would have no effect — the foot-gun every `pice-core`
    /// schema closes per `.claude/rules/rust-core.md`.
    #[test]
    fn evaluate_phase_deny_unknown_fields() {
        let yaml = "sprtt: {}\n";
        let err = serde_yaml::from_str::<EvaluatePhase>(yaml).unwrap_err();
        assert!(
            err.to_string().contains("sprtt"),
            "error message must name the bad field: {err}"
        );
    }

    #[test]
    fn sprt_config_denies_unknown_fields() {
        let yaml = "accept_thresholdd: 19.0\n";
        let err = serde_yaml::from_str::<SprtConfig>(yaml).unwrap_err();
        assert!(
            err.to_string().contains("accept_thresholdd"),
            "error must name the bad field: {err}"
        );
    }

    #[test]
    fn adts_config_denies_unknown_fields() {
        let yaml = "divergence_thresholdz: 2.0\n";
        let err = serde_yaml::from_str::<AdtsConfig>(yaml).unwrap_err();
        assert!(
            err.to_string().contains("divergence_thresholdz"),
            "error must name the bad field: {err}"
        );
    }

    #[test]
    fn vec_config_denies_unknown_fields() {
        let yaml = "entropy_floorz: 0.01\n";
        let err = serde_yaml::from_str::<VecConfig>(yaml).unwrap_err();
        assert!(
            err.to_string().contains("entropy_floorz"),
            "error must name the bad field: {err}"
        );
    }

    /// `LayerOverride.adaptive_algorithm` is optional and parses the
    /// `snake_case` enum tag. Confirms the new field is wired up to the
    /// shared `AdaptiveAlgo` enum (not duplicated locally).
    #[test]
    fn layer_override_parses_adaptive_algorithm() {
        let yaml = "adaptive_algorithm: vec\n";
        let parsed: LayerOverride = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(parsed.adaptive_algorithm, Some(AdaptiveAlgo::Vec));

        let yaml_none = "adaptive_algorithm: none\n";
        let parsed_none: LayerOverride = serde_yaml::from_str(yaml_none).unwrap();
        assert_eq!(parsed_none.adaptive_algorithm, Some(AdaptiveAlgo::None));
    }

    /// Defaults applied to omitted sub-configs match the algorithm modules'
    /// own defaults — guards against silent drift if someone tweaks
    /// `SprtConfig::default()` without re-running schema tests.
    #[test]
    fn evaluate_phase_default_sub_configs_match_module_defaults() {
        let phase = EvaluatePhase::default();
        assert_eq!(phase.sprt, SprtConfig::default());
        assert_eq!(phase.adts, AdtsConfig::default());
        assert_eq!(phase.vec, VecConfig::default());
    }

    /// Phase 5 cohort parallelism: `EvaluatePhase::default().parallel`
    /// MUST be `true`. Locks in the new opt-out behavior — changing the
    /// default to `false` silently breaks every project whose
    /// `.pice/workflow.yaml` omits the field.
    #[test]
    fn evaluate_phase_default_parallel_is_true() {
        assert!(EvaluatePhase::default().parallel);
    }

    /// Phase 5 cohort parallelism: when `phases.evaluate.parallel` is
    /// omitted, the field-level `#[serde(default = "default_evaluate_parallel")]`
    /// fires and produces `true`. Prior to the named-default fix this
    /// test would fail (serde used `bool::default() = false` for omitted
    /// fields when the parent struct IS present). Regression guard.
    #[test]
    fn evaluate_phase_parallel_defaults_to_true_when_omitted_in_yaml() {
        let yaml = "seam_checks: true\n";
        let parsed: EvaluatePhase = serde_yaml::from_str(yaml).unwrap();
        assert!(
            parsed.parallel,
            "parallel must default to true when omitted; got {:?}",
            parsed.parallel
        );
    }

    /// Phase 5 cohort parallelism: the `parallel` field is opt-outable
    /// but a typo (`parallell`) must NOT silently be ignored. Mirrors the
    /// existing `evaluate_phase_deny_unknown_fields` pattern — a stale
    /// config with a misspelled key would otherwise run parallel on a
    /// project that explicitly opted out.
    #[test]
    fn evaluate_phase_denies_typo_in_parallel() {
        let yaml = "parallell: false\n";
        let err = serde_yaml::from_str::<EvaluatePhase>(yaml).unwrap_err();
        assert!(
            err.to_string().contains("parallell"),
            "error must name the bad field: {err}"
        );
    }
}

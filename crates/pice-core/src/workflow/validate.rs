//! Workflow validation — schema, triggers, cross-references, models.
//!
//! Validation is split into focused functions so the caller can reuse parts
//! (e.g., `validate_schema_only` is called during loader parse; the full suite
//! is called by the `pice validate` CLI). All functions collect every error
//! they find before returning — the CLI prints them all at once.

use serde::Serialize;

use crate::layers::LayersConfig;
use crate::seam::types::{LayerBoundary, ParseBoundaryError};
use crate::seam::Registry;
use crate::workflow::schema::{AdaptiveAlgo, WorkflowConfig};
use crate::workflow::trigger;
use crate::workflow::SCHEMA_VERSION;

/// A single validation error with a field path and a human-readable message.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
}

/// Same shape as an error, but non-fatal — printed but exit code stays 0.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ValidationWarning {
    pub field: String,
    pub message: String,
}

/// Aggregate report — serializable for `pice validate --json`.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct ValidationReport {
    pub errors: Vec<ValidationError>,
    pub warnings: Vec<ValidationWarning>,
}

impl ValidationReport {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }

    pub fn extend(&mut self, other: ValidationReport) {
        self.errors.extend(other.errors);
        self.warnings.extend(other.warnings);
    }
}

// ─── Schema-only checks ─────────────────────────────────────────────────────

/// Check `schema_version`, required-field presence, and value ranges.
pub fn validate_schema_only(cfg: &WorkflowConfig) -> ValidationReport {
    let mut report = ValidationReport::default();

    if cfg.schema_version != SCHEMA_VERSION {
        report.errors.push(ValidationError {
            field: "schema_version".into(),
            message: format!(
                "schema_version \"{}\" is not supported; expected \"{}\"",
                cfg.schema_version, SCHEMA_VERSION
            ),
            line: None,
            column: None,
        });
    }

    if !(0.0..=1.0).contains(&cfg.defaults.min_confidence) {
        report.errors.push(ValidationError {
            field: "defaults.min_confidence".into(),
            message: format!(
                "min_confidence must be between 0 and 1; got {}",
                cfg.defaults.min_confidence
            ),
            line: None,
            column: None,
        });
    }

    if !(1..=3).contains(&cfg.defaults.tier) {
        report.errors.push(ValidationError {
            field: "defaults.tier".into(),
            message: format!("tier must be 1, 2, or 3; got {}", cfg.defaults.tier),
            line: None,
            column: None,
        });
    }

    // Phase 4.1 Pass-10 Codex MEDIUM #2: `is_finite()` first so a NaN/Inf
    // YAML literal (`.nan` / `.inf`) surfaces as a deterministic config
    // error at parse time, not as a runtime layer failure. NaN comparisons
    // return false for every operator except `!=`, so the naive `< 0.0`
    // check below would silently pass.
    if !cfg.defaults.budget_usd.is_finite() {
        report.errors.push(ValidationError {
            field: "defaults.budget_usd".into(),
            message: format!("budget_usd must be finite; got {}", cfg.defaults.budget_usd),
            line: None,
            column: None,
        });
    } else if cfg.defaults.budget_usd < 0.0 {
        report.errors.push(ValidationError {
            field: "defaults.budget_usd".into(),
            message: format!(
                "budget_usd must be non-negative; got {}",
                cfg.defaults.budget_usd
            ),
            line: None,
            column: None,
        });
    }

    if cfg.defaults.max_passes == 0 {
        report.errors.push(ValidationError {
            field: "defaults.max_passes".into(),
            message: "max_passes must be ≥ 1".into(),
            line: None,
            column: None,
        });
    }

    // Per-layer override sanity: tier range, confidence range.
    for (layer, o) in &cfg.layer_overrides {
        if let Some(t) = o.tier {
            if !(1..=3).contains(&t) {
                report.errors.push(ValidationError {
                    field: format!("layer_overrides.{layer}.tier"),
                    message: format!("tier must be 1, 2, or 3; got {t}"),
                    line: None,
                    column: None,
                });
            }
        }
        if let Some(mc) = o.min_confidence {
            // Pass-10 Codex MEDIUM #2: finite-check first (NaN escapes range check).
            if !mc.is_finite() {
                report.errors.push(ValidationError {
                    field: format!("layer_overrides.{layer}.min_confidence"),
                    message: format!("min_confidence must be finite; got {mc}"),
                    line: None,
                    column: None,
                });
            } else if !(0.0..=1.0).contains(&mc) {
                report.errors.push(ValidationError {
                    field: format!("layer_overrides.{layer}.min_confidence"),
                    message: format!("min_confidence must be between 0 and 1; got {mc}"),
                    line: None,
                    column: None,
                });
            }
        }
        if let Some(b) = o.budget_usd {
            // Pass-10 Codex MEDIUM #2: finite-check first (NaN escapes `<`).
            if !b.is_finite() {
                report.errors.push(ValidationError {
                    field: format!("layer_overrides.{layer}.budget_usd"),
                    message: format!("budget_usd must be finite; got {b}"),
                    line: None,
                    column: None,
                });
            } else if b < 0.0 {
                report.errors.push(ValidationError {
                    field: format!("layer_overrides.{layer}.budget_usd"),
                    message: format!("budget_usd must be non-negative; got {b}"),
                    line: None,
                    column: None,
                });
            }
        }
    }

    validate_adaptive_into(cfg, &mut report);

    report
}

/// Check adaptive sub-config ranges on the resolved evaluate phase.
///
/// Called inside `validate_schema_only` so both `pice validate` and the
/// daemon's pre-execution validation catch misconfigured algorithm tuning.
fn validate_adaptive_into(cfg: &WorkflowConfig, report: &mut ValidationReport) {
    let eval = &cfg.phases.evaluate;

    // Phase 4.1 Pass-10 Codex MEDIUM #2: NaN/Inf rejection for every
    // adaptive numeric. NaN silently passes every subsequent `<=`, `>=`,
    // `contains()` check (NaN comparisons return false), so a `.nan` YAML
    // literal would slip through the whole validator and trip at runtime
    // with a type-unsafe numeric operation. Surface as a config error
    // here with the offending field named.
    for (name, value) in [
        ("prior_alpha", eval.sprt.prior_alpha),
        ("prior_beta", eval.sprt.prior_beta),
        ("accept_threshold", eval.sprt.accept_threshold),
        ("reject_threshold", eval.sprt.reject_threshold),
        ("divergence_threshold", eval.adts.divergence_threshold),
        ("entropy_floor", eval.vec.entropy_floor),
    ] {
        if !value.is_finite() {
            report.errors.push(ValidationError {
                field: format!("phases.evaluate.{name}"),
                message: format!("{name} must be finite; got {value}"),
                line: None,
                column: None,
            });
        }
    }

    // SPRT: accept > reject, both positive
    if eval.sprt.accept_threshold <= eval.sprt.reject_threshold {
        report.errors.push(ValidationError {
            field: "phases.evaluate.sprt".into(),
            message: format!(
                "accept_threshold ({}) must be > reject_threshold ({})",
                eval.sprt.accept_threshold, eval.sprt.reject_threshold
            ),
            line: None,
            column: None,
        });
    }
    if eval.sprt.prior_alpha <= 0.0 || eval.sprt.prior_beta <= 0.0 {
        report.errors.push(ValidationError {
            field: "phases.evaluate.sprt".into(),
            message: format!(
                "prior_alpha ({}) and prior_beta ({}) must both be > 0",
                eval.sprt.prior_alpha, eval.sprt.prior_beta
            ),
            line: None,
            column: None,
        });
    }

    // ADTS: divergence in [0, 10]
    if !(0.0..=10.0).contains(&eval.adts.divergence_threshold) {
        report.errors.push(ValidationError {
            field: "phases.evaluate.adts.divergence_threshold".into(),
            message: format!(
                "divergence_threshold must be in [0, 10]; got {}",
                eval.adts.divergence_threshold
            ),
            line: None,
            column: None,
        });
    }

    // Phase 4 Pass-5 Codex Critical #3 + High #5: standalone ADTS has no
    // success path. `decide_halt(AdaptiveAlgo::Adts, ...)` returns halt=false
    // from the algorithm-specific branch; `build_adaptive_layer_result` only
    // marks `Passed` for `sprt_confidence_reached` or `vec_entropy`. Result:
    // `adaptive_algorithm: adts` can only end Failed (escalation exhausted)
    // or Pending (budget/max_passes) — never Passed. Additionally, ADTS
    // silently degrades to a primary-only no-op when adversarial evaluation
    // is disabled (High #5), because `paired_scores` stays empty and
    // `run_adts` returns Continue forever.
    //
    // Overlay redesign (ADTS mutates next-pass params while SPRT/VEC drives
    // halt) is tracked as follow-up. Until then, reject standalone ADTS at
    // validation time with a clear upgrade path.
    if matches!(eval.adaptive_algorithm, AdaptiveAlgo::Adts) {
        report.errors.push(ValidationError {
            field: "phases.evaluate.adaptive_algorithm".into(),
            message: "standalone `adts` is not supported — it has no success path \
                     (can only halt Failed on escalation exhaustion or Pending on \
                     budget/max_passes). Use `bayesian_sprt` or `vec` as the base \
                     completion rule; divergence-triggered escalation will be \
                     re-enabled as an overlay in a follow-up release. See Phase 4 \
                     Pass-5 Codex Critical #3 for details."
                .into(),
            line: None,
            column: None,
        });
    }
    for (layer, o) in &cfg.layer_overrides {
        if matches!(o.adaptive_algorithm, Some(AdaptiveAlgo::Adts)) {
            report.errors.push(ValidationError {
                field: format!("layer_overrides.{layer}.adaptive_algorithm"),
                message: "standalone `adts` is not supported (no success path). \
                         Use `bayesian_sprt` or `vec`; see Phase 4 Pass-5 \
                         Codex Critical #3."
                    .into(),
                line: None,
                column: None,
            });
        }
    }

    // VEC: entropy_floor > 0
    if eval.vec.entropy_floor <= 0.0 {
        report.errors.push(ValidationError {
            field: "phases.evaluate.vec.entropy_floor".into(),
            message: format!("entropy_floor must be > 0; got {}", eval.vec.entropy_floor),
            line: None,
            column: None,
        });
    }

    // Per-layer override: max_passes and budget_usd (not already covered above
    // because max_passes=0 on a layer was not previously checked in the loop)
    for (layer, o) in &cfg.layer_overrides {
        if let Some(mp) = o.max_passes {
            if mp == 0 {
                report.errors.push(ValidationError {
                    field: format!("layer_overrides.{layer}.max_passes"),
                    message: "max_passes must be ≥ 1".into(),
                    line: None,
                    column: None,
                });
            }
        }
    }
}

// ─── Trigger checks ─────────────────────────────────────────────────────────

/// Parse every trigger expression in the config; collect syntax errors.
pub fn validate_triggers(cfg: &WorkflowConfig) -> ValidationReport {
    let mut report = ValidationReport::default();

    if let Some(review) = cfg.review.as_ref() {
        if let Some(expr) = review.trigger.as_deref() {
            if let Err(e) = trigger::parse(expr) {
                report.errors.push(ValidationError {
                    field: "review.trigger".into(),
                    message: e.message.clone(),
                    line: Some(e.line),
                    column: Some(e.column),
                });
            }
        }
    }

    for (layer, o) in &cfg.layer_overrides {
        if let Some(expr) = o.trigger.as_deref() {
            if let Err(e) = trigger::parse(expr) {
                report.errors.push(ValidationError {
                    field: format!("layer_overrides.{layer}.trigger"),
                    message: e.message.clone(),
                    line: Some(e.line),
                    column: Some(e.column),
                });
            }
        }
    }

    report
}

// ─── Cross-reference checks ─────────────────────────────────────────────────

/// Validate that every `layer_overrides` key exists in `layers.toml`, and
/// that every `seams` boundary references known layers.
pub fn validate_cross_references(cfg: &WorkflowConfig, layers: &LayersConfig) -> ValidationReport {
    let mut report = ValidationReport::default();

    // Authoritative set is the runtime-live intersection of `order` and
    // `defs`. The orchestrator's `build_dag` / `active_layers` iterates
    // `order` and skips any name missing from `defs`; conversely a `defs`
    // entry without an `order` position is never visited. Both kinds of
    // ghost are runtime-dead, so validation must reject cross-references
    // to either. Using only `order` would admit defs-only ghosts; using
    // only `defs` would admit order-only ghosts. Intersection matches the
    // live set runtime actually sees.
    let order_set: std::collections::HashSet<&str> =
        layers.layers.order.iter().map(|s| s.as_str()).collect();
    let defs_set: std::collections::HashSet<&str> =
        layers.layers.defs.keys().map(|s| s.as_str()).collect();
    let known_layers_set: std::collections::HashSet<&str> =
        order_set.intersection(&defs_set).copied().collect();
    let mut known_layers: Vec<&str> = known_layers_set.iter().copied().collect();
    known_layers.sort();

    for layer in cfg.layer_overrides.keys() {
        if !known_layers_set.contains(layer.as_str()) {
            report.errors.push(ValidationError {
                field: format!("layer_overrides.{layer}"),
                message: format!(
                    "unknown layer '{layer}'; known layers in .pice/layers.toml: {}",
                    known_layers.join(", ")
                ),
                line: None,
                column: None,
            });
        }
    }

    for (layer, model) in &cfg.phases.evaluate.model_override {
        if !known_layers_set.contains(layer.as_str()) {
            report.errors.push(ValidationError {
                field: format!("phases.evaluate.model_override.{layer}"),
                message: format!(
                    "unknown layer '{layer}' in model_override (set to '{model}'); known layers: {}",
                    known_layers.join(", ")
                ),
                line: None,
                column: None,
            });
        }
    }

    if let Some(seams) = cfg.seams.as_ref() {
        for boundary in seams.keys() {
            if !seam_boundary_references_known_layers(boundary, &known_layers_set) {
                report.errors.push(ValidationError {
                    field: format!("seams.{boundary}"),
                    message: format!(
                        "seam boundary '{boundary}' does not reference known layers; \
                         use 'A↔B' or 'A<->B' where both A and B appear in layers.order ({})",
                        known_layers.join(", ")
                    ),
                    line: None,
                    column: None,
                });
            }
        }
    }

    report
}

fn seam_boundary_references_known_layers(
    boundary: &str,
    known: &std::collections::HashSet<&str>,
) -> bool {
    // Use the canonical parser instead of ad-hoc tokenization.
    // (Phase 3 third-round adversarial review fix — the previous
    // `split(['-', '>', '<', '↔', '→'])` shredded valid hyphenated layer
    // names like `auth-service↔api-gateway` into four tokens, then rejected
    // the boundary as unknown-layer even though `LayerBoundary::parse`
    // accepts those names everywhere else. validate_all runs before
    // orchestration, so a repo with hyphenated layer IDs would fail closed
    // before any seam check ran.)
    match LayerBoundary::parse(boundary) {
        Ok(b) => known.contains(b.a.as_str()) && known.contains(b.b.as_str()),
        Err(_) => false,
    }
}

// ─── Seam checks ────────────────────────────────────────────────────────────

/// Validate every `seams.{boundary}` entry against the layer graph and the
/// registered seam-check library. Enforces the six rules documented at
/// `.claude/rules/stack-loops.md`:
///
/// 1. Boundary key parses as `"A↔B"` or `"A<->B"`.
/// 2. Both `A` and `B` appear in `layers.order ∩ layers.defs`.
/// 3. `A != B` (enforced by `LayerBoundary::parse`).
/// 4. Every check ID exists in `registry.ids_in_order()`.
/// 5. No duplicate check IDs within a single boundary's list.
/// 6. No inverted duplicate boundaries (`"A↔B"` and `"B↔A"` both present).
///
/// Collect-all semantics: every error surfaces, no short-circuit.
pub fn validate_seams(
    cfg: &WorkflowConfig,
    layers: &LayersConfig,
    registry: &Registry,
) -> ValidationReport {
    let mut report = ValidationReport::default();

    let Some(seams) = cfg.seams.as_ref() else {
        return report;
    };

    // Authoritative layer set = `order ∩ defs` (see `validate_cross_references`
    // for the invariant).
    let order_set: std::collections::HashSet<&str> =
        layers.layers.order.iter().map(|s| s.as_str()).collect();
    let defs_set: std::collections::HashSet<&str> =
        layers.layers.defs.keys().map(|s| s.as_str()).collect();
    let known: std::collections::HashSet<&str> =
        order_set.intersection(&defs_set).copied().collect();
    let mut known_sorted: Vec<&str> = known.iter().copied().collect();
    known_sorted.sort();

    // Rule 6: track seen canonical boundaries to surface inverted duplicates.
    let mut seen_canonical: std::collections::HashSet<String> = Default::default();

    for (raw_key, check_ids) in seams {
        // Rule 1 & 3: parse boundary.
        let boundary = match LayerBoundary::parse(raw_key) {
            Ok(b) => b,
            Err(e) => {
                let msg = match &e {
                    ParseBoundaryError::MissingSeparator(_) => {
                        "boundary key must contain '↔' or '<->'".to_string()
                    }
                    ParseBoundaryError::TooManySeparators(_) => {
                        "boundary key must reference exactly two layers".to_string()
                    }
                    ParseBoundaryError::EmptySide(_) => {
                        "boundary key has an empty layer name on one side".to_string()
                    }
                    ParseBoundaryError::SelfBoundary(n) => {
                        format!("boundary '{n}↔{n}' references the same layer on both sides")
                    }
                    ParseBoundaryError::NameTooLong { limit, .. } => {
                        format!("boundary layer name exceeds {limit} characters")
                    }
                };
                report.errors.push(ValidationError {
                    field: format!("seams.{raw_key}"),
                    message: msg,
                    line: None,
                    column: None,
                });
                continue;
            }
        };

        // Rule 2: both layers must be known.
        if !known.contains(boundary.a.as_str()) {
            report.errors.push(ValidationError {
                field: format!("seams.{raw_key}"),
                message: format!(
                    "unknown layer '{}' in seam boundary; known layers: {}",
                    boundary.a,
                    known_sorted.join(", ")
                ),
                line: None,
                column: None,
            });
        }
        if !known.contains(boundary.b.as_str()) {
            report.errors.push(ValidationError {
                field: format!("seams.{raw_key}"),
                message: format!(
                    "unknown layer '{}' in seam boundary; known layers: {}",
                    boundary.b,
                    known_sorted.join(", ")
                ),
                line: None,
                column: None,
            });
        }

        // Rule 6: detect inverted duplicate AFTER a prior canonical entry
        // was already recorded. `LayerBoundary::parse` canonicalizes, so two
        // distinct raw keys that canonicalize to the same form are the
        // diagnostic signal. The RAW key must not yet be the canonical form
        // (otherwise we'd be complaining about a self-collision on the first
        // pass).
        let canonical = boundary.canonical();
        if !seen_canonical.insert(canonical.clone()) {
            report.errors.push(ValidationError {
                field: format!("seams.{raw_key}"),
                message: format!(
                    "inverted duplicate boundary: '{raw_key}' and an earlier entry both \
                     canonicalize to '{canonical}'; keep only one"
                ),
                line: None,
                column: None,
            });
            // Don't also surface rule 4/5 for the shadowed entry — the dup is
            // the real bug and listing its checks twice is noise.
            continue;
        }

        // Rule 5: duplicate check ids within the same boundary.
        let mut seen_ids: std::collections::HashSet<&str> = Default::default();
        for id in check_ids {
            if !seen_ids.insert(id.as_str()) {
                report.errors.push(ValidationError {
                    field: format!("seams.{raw_key}"),
                    message: format!("duplicate seam check id '{id}' in boundary"),
                    line: None,
                    column: None,
                });
            }
        }

        // Rule 4: every check ID must be registered AND applicable to this
        // boundary. A registered check whose `applies_to()` returns false for
        // the boundary would be silently skipped at runtime — indistinguishable
        // from "no check configured". Surface it as a config error so users
        // don't believe a boundary is covered when it isn't.
        for id in check_ids {
            match registry.get(id) {
                None => {
                    let known_ids = registry.ids_in_order().join(", ");
                    report.errors.push(ValidationError {
                        field: format!("seams.{raw_key}"),
                        message: format!(
                            "unknown seam check id '{id}'; registered checks: {known_ids}"
                        ),
                        line: None,
                        column: None,
                    });
                }
                Some(check) if !check.applies_to(&boundary) => {
                    report.errors.push(ValidationError {
                        field: format!("seams.{raw_key}"),
                        message: format!(
                            "seam check '{id}' does not apply to boundary \
                             '{}↔{}' (category {}); pick a check whose applies_to() \
                             accepts this boundary or remove it from the seams map",
                            boundary.a,
                            boundary.b,
                            check.category(),
                        ),
                        line: None,
                        column: None,
                    });
                }
                Some(_) => {}
            }
        }
    }

    report
}

// ─── Model checks ───────────────────────────────────────────────────────────

/// Validate model names against a provider's capability list. If `known_models`
/// is `None`, emit warnings instead of errors (the validator didn't get a
/// chance to query the provider).
pub fn validate_models(cfg: &WorkflowConfig, known_models: Option<&[String]>) -> ValidationReport {
    let mut report = ValidationReport::default();

    let check = |field: String, model: &str, report: &mut ValidationReport| match known_models {
        Some(list) if !list.iter().any(|m| m == model) => {
            report.errors.push(ValidationError {
                field,
                message: format!(
                    "unknown model '{model}'; provider reports: {}",
                    list.join(", ")
                ),
                line: None,
                column: None,
            });
        }
        None => {
            report.warnings.push(ValidationWarning {
                field,
                message: format!(
                    "cannot verify model '{model}' — provider capability list unavailable"
                ),
            });
        }
        _ => {}
    };

    check("defaults.model".into(), &cfg.defaults.model, &mut report);
    for (layer, model) in &cfg.phases.evaluate.model_override {
        check(
            format!("phases.evaluate.model_override.{layer}"),
            model,
            &mut report,
        );
    }

    report
}

// ─── Umbrella ───────────────────────────────────────────────────────────────

/// Run every validation pass and aggregate the results.
///
/// If `seam_registry` is `None`, seam-check ID validation is skipped with a
/// warning; callers that have no registry handy (e.g. legacy preview paths)
/// get permissive behavior but the daemon always passes `Some(&registry)`.
pub fn validate_all(
    cfg: &WorkflowConfig,
    layers: Option<&LayersConfig>,
    known_models: Option<&[String]>,
    seam_registry: Option<&Registry>,
) -> ValidationReport {
    let mut report = ValidationReport::default();
    report.extend(validate_schema_only(cfg));
    report.extend(validate_triggers(cfg));
    if let Some(layers) = layers {
        report.extend(validate_cross_references(cfg, layers));
        match seam_registry {
            Some(reg) => report.extend(validate_seams(cfg, layers, reg)),
            None if cfg.seams.is_some() => report.warnings.push(ValidationWarning {
                field: "seams".into(),
                message: "seam check registry not provided; skipping seam ID validation".into(),
            }),
            None => {}
        }
    } else {
        report.warnings.push(ValidationWarning {
            field: "layers.toml".into(),
            message: ".pice/layers.toml not found; skipping cross-reference validation".into(),
        });
    }
    report.extend(validate_models(cfg, known_models));
    report
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layers::{LayerDef, LayersConfig, LayersTable};
    use crate::workflow::loader::embedded_defaults;
    use crate::workflow::schema::{LayerOverride, ReviewConfig};
    use std::collections::BTreeMap;

    fn sample_layers() -> LayersConfig {
        let mut defs = BTreeMap::new();
        defs.insert(
            "backend".into(),
            LayerDef {
                paths: vec!["src/**".into()],
                always_run: false,
                contract: None,
                depends_on: vec![],
                layer_type: None,
                environment_variants: None,
            },
        );
        defs.insert(
            "frontend".into(),
            LayerDef {
                paths: vec!["web/**".into()],
                always_run: false,
                contract: None,
                depends_on: vec![],
                layer_type: None,
                environment_variants: None,
            },
        );
        defs.insert(
            "infrastructure".into(),
            LayerDef {
                paths: vec!["infra/**".into()],
                always_run: true,
                contract: None,
                depends_on: vec![],
                layer_type: None,
                environment_variants: None,
            },
        );
        defs.insert(
            "deployment".into(),
            LayerDef {
                paths: vec!["deploy/**".into()],
                always_run: true,
                contract: None,
                depends_on: vec![],
                layer_type: None,
                environment_variants: None,
            },
        );
        LayersConfig {
            layers: LayersTable {
                order: vec![
                    "backend".into(),
                    "frontend".into(),
                    "infrastructure".into(),
                    "deployment".into(),
                ],
                defs,
            },
            seams: None,
            external_contracts: None,
            stacks: None,
        }
    }

    #[test]
    fn valid_workflow_passes() {
        let cfg = embedded_defaults();
        let layers = sample_layers();
        let report = validate_all(&cfg, Some(&layers), Some(&["sonnet".into()]), None);
        assert!(report.is_ok(), "unexpected errors: {:?}", report.errors);
    }

    #[test]
    fn bad_schema_version_errors() {
        let mut cfg = embedded_defaults();
        cfg.schema_version = "0.1".into();
        let report = validate_schema_only(&cfg);
        assert!(!report.is_ok());
        assert!(report.errors.iter().any(|e| e.field == "schema_version"));
    }

    #[test]
    fn trigger_syntax_error_has_line_col() {
        let mut cfg = embedded_defaults();
        cfg.review = Some(ReviewConfig {
            enabled: true,
            trigger: Some("tier ==".into()),
            ..Default::default()
        });
        let report = validate_triggers(&cfg);
        assert!(!report.is_ok());
        let err = &report.errors[0];
        assert_eq!(err.field, "review.trigger");
        assert!(err.line.is_some());
        assert!(err.column.is_some());
    }

    #[test]
    fn unknown_layer_in_overrides_errors() {
        let mut cfg = embedded_defaults();
        cfg.layer_overrides.insert(
            "ghost_layer".into(),
            LayerOverride {
                tier: Some(3),
                ..Default::default()
            },
        );
        let layers = sample_layers();
        let report = validate_cross_references(&cfg, &layers);
        assert!(!report.is_ok());
        let err = &report.errors[0];
        assert!(err.message.contains("ghost_layer"));
        assert!(err.message.contains("backend"));
    }

    #[test]
    fn defs_only_ghost_layer_rejected() {
        // A layer defined in `[layers.X]` but missing from `layers.order`
        // is never visited by `build_dag()` / `active_layers()`. Workflow
        // references to it are just as dead as order-only ghosts, so
        // validation must reject them too.
        let mut layers = sample_layers();
        layers.layers.defs.insert(
            "defs_only_ghost".into(),
            crate::layers::LayerDef {
                paths: vec!["whatever/**".into()],
                always_run: false,
                contract: None,
                depends_on: vec![],
                layer_type: None,
                environment_variants: None,
            },
        );
        // NOTE: not added to `layers.order`.

        let mut cfg = embedded_defaults();
        cfg.layer_overrides.insert(
            "defs_only_ghost".into(),
            LayerOverride {
                tier: Some(3),
                ..Default::default()
            },
        );
        let report = validate_cross_references(&cfg, &layers);
        assert!(!report.is_ok(), "defs-only ghost should be rejected");
        assert!(report.errors[0].message.contains("defs_only_ghost"));
    }

    #[test]
    fn order_only_ghost_layer_rejected() {
        // A name listed in `layers.order` but missing from `layers.defs`
        // never activates at runtime. Workflow cross-references to it must
        // be rejected — otherwise the override silently no-ops.
        let mut layers = sample_layers();
        layers.layers.order.push("ghost_in_order".into());
        // NOTE: no corresponding `defs` entry.

        let mut cfg = embedded_defaults();
        cfg.layer_overrides.insert(
            "ghost_in_order".into(),
            LayerOverride {
                tier: Some(3),
                ..Default::default()
            },
        );
        let report = validate_cross_references(&cfg, &layers);
        assert!(!report.is_ok(), "expected order-only ghost to be rejected");
        assert!(report.errors[0].message.contains("ghost_in_order"));
    }

    #[test]
    fn unknown_seam_boundary_errors() {
        let mut cfg = embedded_defaults();
        let mut seams = BTreeMap::new();
        seams.insert("backend-nonexistent".into(), vec!["check1".into()]);
        cfg.seams = Some(seams);
        let layers = sample_layers();
        let report = validate_cross_references(&cfg, &layers);
        assert!(!report.is_ok());
        assert!(report.errors.iter().any(|e| e.field.starts_with("seams.")));
    }

    #[test]
    fn known_seam_boundary_passes() {
        // Boundary spelled with the canonical `↔` separator (or `<->`).
        // Phase 3 third-round adversarial review fix tightened the
        // boundary parser to reject ad-hoc `-` delimiters since hyphens
        // are valid layer-name characters; tests must use the documented
        // separator.
        let mut cfg = embedded_defaults();
        let mut seams = BTreeMap::new();
        seams.insert("backend↔frontend".into(), vec!["check1".into()]);
        cfg.seams = Some(seams);
        let layers = sample_layers();
        let report = validate_cross_references(&cfg, &layers);
        assert!(report.is_ok(), "{:?}", report.errors);
    }

    #[test]
    fn unknown_model_with_known_list_errors() {
        let mut cfg = embedded_defaults();
        cfg.defaults.model = "ghost".into();
        let report = validate_models(&cfg, Some(&["sonnet".into(), "opus".into()]));
        assert!(!report.is_ok());
        assert!(report.errors[0].message.contains("ghost"));
    }

    #[test]
    fn unknown_model_without_list_warns() {
        let mut cfg = embedded_defaults();
        cfg.defaults.model = "ghost".into();
        let report = validate_models(&cfg, None);
        assert!(report.is_ok());
        assert_eq!(report.warnings.len(), 1);
    }

    #[test]
    fn multiple_errors_collected() {
        let mut cfg = embedded_defaults();
        cfg.schema_version = "0.1".into();
        cfg.defaults.min_confidence = 2.0;
        cfg.defaults.tier = 9;
        let report = validate_schema_only(&cfg);
        assert!(report.errors.len() >= 3);
    }

    // ─── Phase 4 Pass-5 Codex Critical #3 + High #5 ────────────────────────
    //
    // Standalone `adaptive_algorithm: adts` has no success path (can only halt
    // Failed on escalation exhaustion or Pending on budget/max_passes). It
    // also silently degrades to a primary-only no-op when adversarial
    // evaluation is disabled. Until the overlay redesign lands, reject it at
    // validation.

    #[test]
    fn adts_rejected_at_defaults() {
        let mut cfg = embedded_defaults();
        cfg.phases.evaluate.adaptive_algorithm = AdaptiveAlgo::Adts;
        let report = validate_schema_only(&cfg);
        assert!(!report.is_ok(), "ADTS at defaults must fail validation");
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.field == "phases.evaluate.adaptive_algorithm"
                    && e.message.contains("standalone `adts` is not supported")),
            "expected standalone-ADTS rejection error; got {:?}",
            report.errors
        );
    }

    #[test]
    fn adts_rejected_at_layer_override() {
        let mut cfg = embedded_defaults();
        let mut overrides = BTreeMap::new();
        overrides.insert(
            "backend".into(),
            LayerOverride {
                adaptive_algorithm: Some(AdaptiveAlgo::Adts),
                ..LayerOverride::default()
            },
        );
        cfg.layer_overrides = overrides;
        let report = validate_schema_only(&cfg);
        assert!(!report.is_ok(), "ADTS at layer override must fail");
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.field == "layer_overrides.backend.adaptive_algorithm"),
            "expected layer-level ADTS rejection; got {:?}",
            report.errors
        );
    }

    #[test]
    fn bayesian_sprt_and_vec_and_none_remain_accepted() {
        for algo in [
            AdaptiveAlgo::BayesianSprt,
            AdaptiveAlgo::Vec,
            AdaptiveAlgo::None,
        ] {
            let mut cfg = embedded_defaults();
            cfg.phases.evaluate.adaptive_algorithm = algo;
            let report = validate_schema_only(&cfg);
            assert!(
                report.errors.is_empty(),
                "algo {:?} must remain valid; got errors {:?}",
                algo,
                report.errors
            );
        }
    }

    // ─── Seam validation tests ──────────────────────────────────────────

    fn sample_registry() -> crate::seam::Registry {
        crate::seam::default_registry()
    }

    #[test]
    fn seam_validator_accepts_well_formed_boundary() {
        let mut cfg = embedded_defaults();
        let mut seams = BTreeMap::new();
        seams.insert(
            "backend↔infrastructure".into(),
            vec!["config_mismatch".into()],
        );
        cfg.seams = Some(seams);
        let report = validate_seams(&cfg, &sample_layers(), &sample_registry());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
    }

    #[test]
    fn seam_validator_rejects_missing_separator() {
        let mut cfg = embedded_defaults();
        let mut seams = BTreeMap::new();
        seams.insert("backendinfra".into(), vec!["config_mismatch".into()]);
        cfg.seams = Some(seams);
        let report = validate_seams(&cfg, &sample_layers(), &sample_registry());
        assert!(!report.is_ok());
        assert!(report.errors[0].message.contains("'↔'"));
    }

    #[test]
    fn seam_validator_rejects_self_boundary() {
        let mut cfg = embedded_defaults();
        let mut seams = BTreeMap::new();
        seams.insert("backend↔backend".into(), vec!["config_mismatch".into()]);
        cfg.seams = Some(seams);
        let report = validate_seams(&cfg, &sample_layers(), &sample_registry());
        assert!(!report.is_ok());
        assert!(report.errors[0].message.contains("same layer"));
    }

    #[test]
    fn seam_validator_rejects_unknown_layer() {
        let mut cfg = embedded_defaults();
        let mut seams = BTreeMap::new();
        seams.insert("ghost↔backend".into(), vec!["config_mismatch".into()]);
        cfg.seams = Some(seams);
        let report = validate_seams(&cfg, &sample_layers(), &sample_registry());
        assert!(!report.is_ok());
        assert!(report.errors.iter().any(|e| e.message.contains("ghost")));
    }

    #[test]
    fn seam_validator_rejects_unknown_check_id() {
        let mut cfg = embedded_defaults();
        let mut seams = BTreeMap::new();
        seams.insert(
            "backend↔infrastructure".into(),
            vec!["does_not_exist".into()],
        );
        cfg.seams = Some(seams);
        let report = validate_seams(&cfg, &sample_layers(), &sample_registry());
        assert!(!report.is_ok());
        assert!(report.errors[0]
            .message
            .contains("unknown seam check id 'does_not_exist'"));
    }

    #[test]
    fn seam_validator_rejects_duplicate_check_id_in_boundary() {
        let mut cfg = embedded_defaults();
        let mut seams = BTreeMap::new();
        seams.insert(
            "backend↔infrastructure".into(),
            vec!["config_mismatch".into(), "config_mismatch".into()],
        );
        cfg.seams = Some(seams);
        let report = validate_seams(&cfg, &sample_layers(), &sample_registry());
        assert!(!report.is_ok());
        assert!(report.errors[0]
            .message
            .contains("duplicate seam check id 'config_mismatch'"));
    }

    #[test]
    fn seam_validator_rejects_inverted_duplicate_boundary() {
        let mut cfg = embedded_defaults();
        let mut seams = BTreeMap::new();
        // BTreeMap iteration is alphabetical — `backend↔frontend` comes
        // before `frontend↔backend`. The second (inverted) key should
        // surface as the duplicate error. Use `openapi_compliance` because
        // its `applies_to` covers backend+frontend — otherwise the new
        // applies_to validation (rule 4b) would emit a noise error here.
        seams.insert("backend↔frontend".into(), vec!["openapi_compliance".into()]);
        seams.insert("frontend↔backend".into(), vec!["openapi_compliance".into()]);
        cfg.seams = Some(seams);
        let report = validate_seams(&cfg, &sample_layers(), &sample_registry());
        assert!(!report.is_ok());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("inverted duplicate")),
            "expected inverted duplicate error, got: {:?}",
            report.errors
        );
    }

    #[test]
    fn seam_validator_rejects_check_that_does_not_apply() {
        // A registered check whose `applies_to()` returns false for this
        // boundary would be silently skipped at runtime. Surfacing this as
        // a config error prevents silent-bypass: users configuring a
        // boundary think they have coverage when they don't.
        let mut cfg = embedded_defaults();
        let mut seams = BTreeMap::new();
        // `config_mismatch` applies to infrastructure/deployment boundaries
        // only. `backend↔frontend` doesn't touch either.
        seams.insert("backend↔frontend".into(), vec!["config_mismatch".into()]);
        cfg.seams = Some(seams);
        let report = validate_seams(&cfg, &sample_layers(), &sample_registry());
        assert!(!report.is_ok());
        let err = report
            .errors
            .iter()
            .find(|e| e.message.contains("does not apply"))
            .unwrap_or_else(|| panic!("expected applies_to error, got: {:?}", report.errors));
        assert!(err.message.contains("config_mismatch"));
        assert!(err.message.contains("backend"));
        assert!(err.message.contains("frontend"));
    }

    #[test]
    fn seam_validator_accepts_hyphenated_layer_names() {
        // Phase 3 third-round adversarial review fix: the previous tokenizer
        // split on `-`, exploding `auth-service↔api-gateway` into four parts
        // and rejecting it as unknown-layer. Both `↔` and `<->` boundary
        // forms must round-trip when layer names contain hyphens.
        use crate::layers::{LayerDef, LayersConfig, LayersTable};
        let mut defs = BTreeMap::new();
        defs.insert(
            "auth-service".into(),
            LayerDef {
                paths: vec!["services/auth/**".into()],
                always_run: false,
                contract: None,
                depends_on: vec![],
                layer_type: None,
                environment_variants: None,
            },
        );
        defs.insert(
            "api-gateway".into(),
            LayerDef {
                paths: vec!["services/gateway/**".into()],
                always_run: false,
                contract: None,
                depends_on: vec![],
                layer_type: None,
                environment_variants: None,
            },
        );
        let layers = LayersConfig {
            layers: LayersTable {
                order: vec!["auth-service".into(), "api-gateway".into()],
                defs,
            },
            seams: None,
            external_contracts: None,
            stacks: None,
        };
        // Both spellings of the boundary separator should be accepted.
        for raw in [
            "auth-service↔api-gateway",
            "api-gateway↔auth-service",
            "auth-service<->api-gateway",
        ] {
            let mut cfg = embedded_defaults();
            let mut seams = BTreeMap::new();
            // version_skew applies to every boundary, so applies_to noise
            // doesn't pollute this hyphenated-name regression test.
            seams.insert(raw.into(), vec!["version_skew".into()]);
            cfg.seams = Some(seams);
            let report = validate_seams(&cfg, &layers, &sample_registry());
            assert!(
                report.is_ok(),
                "hyphenated layer boundary '{raw}' must validate; got errors: {:?}",
                report.errors
            );
        }
    }

    #[test]
    fn seam_validator_collects_all_errors() {
        // Single invalid entry producing multiple simultaneous findings.
        let mut cfg = embedded_defaults();
        let mut seams = BTreeMap::new();
        seams.insert(
            "ghost↔other_ghost".into(),
            vec!["unknown_check".into(), "unknown_check".into()],
        );
        cfg.seams = Some(seams);
        let report = validate_seams(&cfg, &sample_layers(), &sample_registry());
        assert!(!report.is_ok());
        // Two unknown layers + one duplicate id + one unknown-id error.
        assert!(
            report.errors.len() >= 3,
            "expected multiple errors collected, got {}: {:?}",
            report.errors.len(),
            report.errors
        );
    }

    #[test]
    fn seam_validator_accepts_ascii_separator() {
        let mut cfg = embedded_defaults();
        let mut seams = BTreeMap::new();
        seams.insert(
            "backend<->infrastructure".into(),
            vec!["config_mismatch".into()],
        );
        cfg.seams = Some(seams);
        let report = validate_seams(&cfg, &sample_layers(), &sample_registry());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
    }

    // ─── Adaptive validation tests ────────────────────────────────────

    #[test]
    fn adaptive_defaults_pass_validation() {
        let cfg = embedded_defaults();
        let report = validate_schema_only(&cfg);
        assert!(report.is_ok(), "defaults must pass: {:?}", report.errors);
    }

    #[test]
    fn sprt_accept_le_reject_rejected() {
        let mut cfg = embedded_defaults();
        cfg.phases.evaluate.sprt.accept_threshold = 0.5;
        cfg.phases.evaluate.sprt.reject_threshold = 0.5;
        let report = validate_schema_only(&cfg);
        assert!(report
            .errors
            .iter()
            .any(|e| e.field.contains("sprt") && e.message.contains("accept_threshold")));
    }

    #[test]
    fn sprt_prior_alpha_zero_rejected() {
        let mut cfg = embedded_defaults();
        cfg.phases.evaluate.sprt.prior_alpha = 0.0;
        let report = validate_schema_only(&cfg);
        assert!(report
            .errors
            .iter()
            .any(|e| e.field.contains("sprt") && e.message.contains("prior_alpha")));
    }

    #[test]
    fn adts_divergence_out_of_range_rejected() {
        let mut cfg = embedded_defaults();
        cfg.phases.evaluate.adts.divergence_threshold = 11.0;
        let report = validate_schema_only(&cfg);
        assert!(report
            .errors
            .iter()
            .any(|e| e.field.contains("adts") && e.message.contains("divergence_threshold")));
    }

    #[test]
    fn vec_entropy_floor_zero_rejected() {
        let mut cfg = embedded_defaults();
        cfg.phases.evaluate.vec.entropy_floor = 0.0;
        let report = validate_schema_only(&cfg);
        assert!(report
            .errors
            .iter()
            .any(|e| e.field.contains("vec") && e.message.contains("entropy_floor")));
    }

    #[test]
    fn layer_override_max_passes_zero_rejected() {
        let mut cfg = embedded_defaults();
        cfg.layer_overrides.insert(
            "backend".into(),
            LayerOverride {
                max_passes: Some(0),
                ..Default::default()
            },
        );
        let report = validate_schema_only(&cfg);
        assert!(report
            .errors
            .iter()
            .any(|e| e.field.contains("backend") && e.message.contains("max_passes")));
    }

    #[test]
    fn adaptive_collects_all_errors() {
        let mut cfg = embedded_defaults();
        cfg.phases.evaluate.sprt.accept_threshold = 0.01;
        cfg.phases.evaluate.sprt.reject_threshold = 1.0;
        cfg.phases.evaluate.sprt.prior_alpha = -1.0;
        cfg.phases.evaluate.adts.divergence_threshold = -0.5;
        cfg.phases.evaluate.vec.entropy_floor = -0.01;
        let report = validate_schema_only(&cfg);
        assert!(
            report.errors.len() >= 4,
            "expected multiple adaptive errors collected, got {}: {:?}",
            report.errors.len(),
            report.errors
        );
    }

    #[test]
    fn all_presets_valid() {
        use crate::workflow::loader;

        let fixture_layers = sample_layers();
        let preset_names = ["greenfield", "brownfield", "ci", "strict", "permissive"];
        // Resolve preset directory relative to the crate root at compile time.
        let crate_dir = env!("CARGO_MANIFEST_DIR");
        let preset_dir =
            std::path::Path::new(crate_dir).join("../../templates/pice/workflow-presets");

        for name in preset_names {
            let path = preset_dir.join(format!("{name}.yaml"));
            let content = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("preset {name} not found at {}", path.display()));
            let cfg: WorkflowConfig = serde_yaml::from_str(&content)
                .unwrap_or_else(|e| panic!("preset {name} failed to parse: {e}"));

            // Framework → project is a simple overlay (no floor).
            let framework = loader::embedded_defaults();
            let merged = crate::workflow::merge::overlay(framework, cfg.clone());

            let report = validate_all(
                &merged,
                Some(&fixture_layers),
                Some(&["sonnet".into(), "opus".into(), "haiku".into()]),
                None,
            );
            assert!(
                report.is_ok(),
                "preset {name} failed validation: {:?}",
                report.errors
            );
        }
    }
}

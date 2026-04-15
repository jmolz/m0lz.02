//! Floor-based merge semantics — user overrides can restrict, never relax.
//!
//! The floor table is the HARD RULE from `.claude/rules/workflow-yaml.md` and
//! PRDv2 lines 908–918. A single merge function enforces it for both the
//! framework→project merge (project raising the floor) and the project→user
//! merge (user restricting further). Every would-be relaxation becomes a
//! [`FloorViolation`] and all violations are collected before returning so the
//! CLI can print them in one pass.
//!
//! Direction summary (see `.claude/rules/workflow-yaml.md` lines 38–45):
//!
//! | Field             | Stricter means             |
//! |-------------------|----------------------------|
//! | `tier`            | higher number              |
//! | `min_confidence`  | higher fraction            |
//! | `max_passes`      | higher count (more rigor)  |
//! | `budget_usd`      | lower dollars              |
//! | `require_review`  | true                       |
//!
//! ## Per-layer floor derivation
//!
//! A user override on layer `X` is checked against the **effective project
//! value** for that layer — `project.defaults` overlaid with
//! `project.layer_overrides[X]` (if any). This closes the escape path where a
//! user adds a fresh per-layer override on a layer the project never listed:
//! without this rule, the floor would collapse to `LayerOverride::default()`
//! and any value would pass.

use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeMap;
use thiserror::Error;

use crate::workflow::schema::{Defaults, LayerOverride, ReviewConfig, WorkflowConfig};
use crate::workflow::trigger;

/// Returns true iff two trigger expressions parse to AST-equivalent
/// expressions. Tolerates surface-level differences (whitespace, the
/// `always` ↔ `true` keyword aliasing, formatting). If either expression
/// fails to parse, returns false — `validate_triggers` surfaces the parse
/// error separately, and we conservatively treat unparseable triggers as
/// non-equivalent so floor enforcement does not silently accept garbage.
///
/// This is **structural** equivalence, not logical implication. A user
/// trigger that is *semantically stricter* than the project trigger
/// (e.g., user `always` vs project `tier >= 3`, where `always` is a
/// superset of conditions) will still be rejected. Full AST-implication
/// checking is deferred to v0.3 — see the trigger floor comments at the
/// call sites. Tracked against PRDv2 Feature 9 (review gates) since the
/// same grammar drives both surfaces; if users report false positives on
/// v0.2, upgrade this to a subset check via truth-table enumeration over
/// the finite context domain (tier 1–3, layer names, cost buckets, etc.).
fn triggers_equivalent(project: &str, user: &str) -> bool {
    // Byte-identical strings are always equivalent — no need to parse, and
    // crucially we MUST NOT collapse a parse failure to a "rewrite"
    // violation when the user has merely restated the same (possibly
    // invalid) text. That would mask the real parse error with a
    // misleading floor violation. `validate_triggers` reports the syntax
    // error separately on the resolved config.
    if project == user {
        return true;
    }
    match (trigger::parse(project), trigger::parse(user)) {
        (Ok(p), Ok(u)) => p == u,
        _ => false,
    }
}

/// A single floor-rule violation. Named fields so downstream tooling (the
/// daemon RPC and `pice validate --json`) can serialize the diff cleanly.
#[derive(Debug, Clone, Error, Serialize, PartialEq)]
#[error("{field}: override violates project floor ({reason}); project={project}, override={user}")]
pub struct FloorViolation {
    pub field: String,
    pub project: String,
    pub user: String,
    pub reason: &'static str,
}

/// Aggregate of floor violations detected during one merge call.
#[derive(Debug, Clone, Error, Serialize, PartialEq)]
pub struct FloorViolations {
    pub violations: Vec<FloorViolation>,
}

impl std::fmt::Display for FloorViolations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "workflow override violates {} project floor(s):",
            self.violations.len()
        )?;
        for v in &self.violations {
            writeln!(f, "  - {v}")?;
        }
        Ok(())
    }
}

/// Simple overlay merge — `overlay` wins on every field. Used for the
/// framework→project step, which PRDv2 lines 903–918 do NOT floor-guard (only
/// project→user is floor-bound). Scalar fields from overlay replace base; map
/// fields are merged key-by-key with overlay keys taking precedence; review
/// and seams are replaced wholesale when overlay sets them.
pub fn overlay(base: WorkflowConfig, overlay: WorkflowConfig) -> WorkflowConfig {
    let mut out = base;
    out.defaults = overlay.defaults;
    out.phases = overlay.phases;
    for (k, v) in overlay.layer_overrides {
        out.layer_overrides.insert(k, v);
    }
    if overlay.review.is_some() {
        out.review = overlay.review;
    }
    if let Some(overlay_seams) = overlay.seams {
        let base_seams = out.seams.get_or_insert_with(BTreeMap::new);
        for (k, v) in overlay_seams {
            base_seams.insert(k, v);
        }
    }
    out
}

/// Merge `overlay` on top of `base` using floor-based semantics. Any field
/// `overlay` sets that would relax `base` becomes a [`FloorViolation`]. All
/// violations are collected before returning.
///
/// Called for the project→user step during `loader::resolve`. The framework→
/// project step uses [`overlay`] instead — see PRDv2 lines 903–918.
pub fn merge_with_floor(base: WorkflowConfig, overlay: WorkflowConfig) -> Result<WorkflowConfig> {
    let mut violations: Vec<FloorViolation> = Vec::new();

    // Snapshot project values BEFORE mutation — these are the true floors used
    // by per-layer floor derivation. If we only consulted `out.defaults` after
    // `merge_defaults`, a user raising defaults could then be checked against
    // their own raised value in the per-layer step; that's still safe, but the
    // snapshot makes intent unambiguous: the floor is what the PROJECT said.
    let project_defaults = base.defaults.clone();
    let project_review = base.review.clone();
    let project_layer_overrides = base.layer_overrides.clone();

    let mut out = base.clone();

    // schema_version: overlay must match base exactly (handled pre-merge in loader).
    // We still keep base's value to avoid drift.

    merge_defaults(&mut out.defaults, &overlay.defaults, &mut violations);

    // phases: overlay fully replaces base for scalar sub-structs. The PRD
    // doesn't floor-guard phase-level fields (they're orchestration knobs, not
    // security guardrails), so overlay wins without further checks.
    out.phases = overlay.phases.clone();

    merge_layer_overrides(
        &mut out.layer_overrides,
        &overlay.layer_overrides,
        &project_defaults,
        project_review.as_ref(),
        &project_layer_overrides,
        &mut violations,
    );

    merge_review(&mut out.review, overlay.review.as_ref(), &mut violations);

    // Seams use per-boundary replacement with an existence floor — see
    // `merge_seams`. An empty user override for a project-declared boundary
    // is rejected as a floor violation; new user boundaries are additive.
    merge_seams(&mut out.seams, overlay.seams.as_ref(), &mut violations);

    if !violations.is_empty() {
        return Err(FloorViolations { violations }.into());
    }
    Ok(out)
}

fn merge_defaults(base: &mut Defaults, overlay: &Defaults, violations: &mut Vec<FloorViolation>) {
    // tier — overlay can only raise
    if overlay.tier < base.tier {
        violations.push(FloorViolation {
            field: "defaults.tier".into(),
            project: base.tier.to_string(),
            user: overlay.tier.to_string(),
            reason: "tier may only be raised",
        });
    } else {
        base.tier = overlay.tier;
    }

    // min_confidence — overlay can only raise
    if overlay.min_confidence < base.min_confidence {
        violations.push(FloorViolation {
            field: "defaults.min_confidence".into(),
            project: base.min_confidence.to_string(),
            user: overlay.min_confidence.to_string(),
            reason: "min_confidence may only be raised",
        });
    } else {
        base.min_confidence = overlay.min_confidence;
    }

    // max_passes — not floor-guarded per PRDv2 lines 908–916. Overlay wins.
    // (Rationale: more passes increases rigor and cost; fewer passes decreases
    // both. Direction is not monotonic, so the PRD leaves this unconstrained.
    // See `.claude/rules/workflow-yaml.md` for the policy alignment note.)
    base.max_passes = overlay.max_passes;

    // budget_usd — overlay can only LOWER (lower budget = more restrictive)
    if overlay.budget_usd > base.budget_usd {
        violations.push(FloorViolation {
            field: "defaults.budget_usd".into(),
            project: base.budget_usd.to_string(),
            user: overlay.budget_usd.to_string(),
            reason: "budget_usd may only be lowered",
        });
    } else {
        base.budget_usd = overlay.budget_usd;
    }

    // model — not floor-guarded (team pref; restrictiveness is domain-specific)
    base.model = overlay.model.clone();

    // cost_cap_behavior — not floor-guarded (halt is strictest and the default)
    base.cost_cap_behavior = overlay.cost_cap_behavior;

    // max_parallelism — overlay wins if set
    if overlay.max_parallelism.is_some() {
        base.max_parallelism = overlay.max_parallelism;
    }
}

fn merge_layer_overrides(
    base: &mut BTreeMap<String, LayerOverride>,
    overlay: &BTreeMap<String, LayerOverride>,
    project_defaults: &Defaults,
    project_review: Option<&ReviewConfig>,
    project_layer_overrides: &BTreeMap<String, LayerOverride>,
    violations: &mut Vec<FloorViolation>,
) {
    let empty = LayerOverride::default();
    for (layer, o) in overlay {
        let project_layer = project_layer_overrides.get(layer).unwrap_or(&empty);
        let entry = base.entry(layer.clone()).or_default();
        merge_layer_override_fields(
            layer,
            entry,
            o,
            project_defaults,
            project_review,
            project_layer,
            violations,
        );
    }
}

fn merge_layer_override_fields(
    layer: &str,
    base: &mut LayerOverride,
    overlay: &LayerOverride,
    project_defaults: &Defaults,
    project_review: Option<&ReviewConfig>,
    project_layer: &LayerOverride,
    violations: &mut Vec<FloorViolation>,
) {
    if let Some(o_tier) = overlay.tier {
        let floor = project_layer.tier.unwrap_or(project_defaults.tier);
        if o_tier < floor {
            violations.push(FloorViolation {
                field: format!("layer_overrides.{layer}.tier"),
                project: floor.to_string(),
                user: o_tier.to_string(),
                reason: "tier may only be raised",
            });
        } else {
            base.tier = Some(o_tier);
        }
    }

    if let Some(o_mc) = overlay.min_confidence {
        let floor = project_layer
            .min_confidence
            .unwrap_or(project_defaults.min_confidence);
        if o_mc < floor {
            violations.push(FloorViolation {
                field: format!("layer_overrides.{layer}.min_confidence"),
                project: floor.to_string(),
                user: o_mc.to_string(),
                reason: "min_confidence may only be raised",
            });
        } else {
            base.min_confidence = Some(o_mc);
        }
    }

    if let Some(o_mp) = overlay.max_passes {
        // max_passes is not floor-guarded (see defaults merge comment).
        base.max_passes = Some(o_mp);
    }

    if let Some(o_b) = overlay.budget_usd {
        let ceiling = project_layer
            .budget_usd
            .unwrap_or(project_defaults.budget_usd);
        if o_b > ceiling {
            violations.push(FloorViolation {
                field: format!("layer_overrides.{layer}.budget_usd"),
                project: ceiling.to_string(),
                user: o_b.to_string(),
                reason: "budget_usd may only be lowered",
            });
        } else {
            base.budget_usd = Some(o_b);
        }
    }

    if let Some(o_rr) = overlay.require_review {
        // Effective project floor for require_review on this layer:
        //   - if project_layer.require_review is explicitly set, that IS the
        //     floor (including `Some(false)` — a project-committed exemption
        //     is a legitimate value a user may match).
        //   - otherwise fall back to `project_review.enabled` (global gate).
        //
        // `or`ing both sources would incorrectly reject a user keeping a
        // project-committed per-layer exemption when the global gate is on.
        let global_rr_floor = project_review.map(|r| r.enabled).unwrap_or(false);
        let floor = project_layer.require_review.unwrap_or(global_rr_floor);
        if floor && !o_rr {
            violations.push(FloorViolation {
                field: format!("layer_overrides.{layer}.require_review"),
                project: "true".into(),
                user: "false".into(),
                reason: "required review cannot be disabled",
            });
        } else {
            base.require_review = Some(o_rr);
        }
    }

    if overlay.trigger.is_some() {
        // The project trigger is a SPECIFIC committed expression. Removal is
        // a violation; semantic weakening (e.g., rewriting `always` to
        // `false`, or `tier >= 3` to `tier >= 999` so the gate never fires)
        // would also bypass the project floor — but proving "stricter"
        // requires AST implication checking, which we don't have yet.
        //
        // Interim policy: when the project has set a trigger, the user must
        // either OMIT their override (no change to the project trigger) or
        // provide an EXACT MATCH. Any divergent value — including the empty
        // string sentinel for removal — is a floor violation. AST-based
        // implication checking can relax this in a future phase.
        let project_trigger = project_layer.trigger.as_deref();
        if let (Some(pt), Some(o)) = (project_trigger, overlay.trigger.as_deref()) {
            // Equivalent ASTs (e.g., `always` vs `true`, whitespace diffs)
            // are accepted as a no-op restatement of the project trigger.
            // Any other rewrite — including weakening to `false` and
            // semantically stricter rewrites — is rejected. AST implication
            // is deferred; see triggers_equivalent docs.
            if !triggers_equivalent(pt, o) {
                let reason = if o.is_empty() {
                    "required gate trigger cannot be removed"
                } else {
                    "required gate trigger cannot be rewritten; \
                     omit the override to keep the project trigger or restate it equivalently"
                };
                violations.push(FloorViolation {
                    field: format!("layer_overrides.{layer}.trigger"),
                    project: pt.to_string(),
                    user: o.to_string(),
                    reason,
                });
                return;
            }
        }
        base.trigger = overlay.trigger.clone();
    }
}

fn merge_review(
    base: &mut Option<ReviewConfig>,
    overlay: Option<&ReviewConfig>,
    violations: &mut Vec<FloorViolation>,
) {
    let Some(overlay) = overlay else {
        return;
    };

    match base {
        None => {
            *base = Some(overlay.clone());
        }
        Some(b) => {
            // require_review as a project-level flag is `enabled`. Overlay
            // may not disable a project-required review.
            if b.enabled && !overlay.enabled {
                violations.push(FloorViolation {
                    field: "review.enabled".into(),
                    project: "true".into(),
                    user: "false".into(),
                    reason: "required review cannot be disabled",
                });
            } else {
                b.enabled = overlay.enabled;
            }

            // Trigger floor: the project committed to a SPECIFIC expression.
            // Removing it is a violation; rewriting it to a weaker expression
            // (e.g., `always` → `false`, or `tier >= 3` → `tier >= 999`)
            // would also bypass the gate, but proving semantic strictness
            // requires AST implication. Interim policy: any non-matching
            // override of an existing project trigger is a violation. The
            // user must omit the override (keep project's) or match exactly.
            // AST-based implication can relax this in a future phase.
            match (b.trigger.as_deref(), overlay.trigger.as_deref()) {
                (Some(_), None) => {
                    violations.push(FloorViolation {
                        field: "review.trigger".into(),
                        project: b.trigger.clone().unwrap_or_default(),
                        user: "(removed)".into(),
                        reason: "required gate trigger cannot be removed",
                    });
                }
                (Some(pt), Some(o)) if !triggers_equivalent(pt, o) => {
                    violations.push(FloorViolation {
                        field: "review.trigger".into(),
                        project: pt.to_string(),
                        user: o.to_string(),
                        reason: "required gate trigger cannot be rewritten; \
                                 omit the override to keep the project trigger or restate it equivalently",
                    });
                }
                (_, Some(_)) => {
                    // No project trigger set, OR exact match — overlay wins.
                    b.trigger = overlay.trigger.clone();
                }
                (None, None) => {}
            }

            // Other fields are tuning knobs — overlay wins.
            b.timeout_hours = overlay.timeout_hours;
            b.on_timeout = overlay.on_timeout;
            b.notification = overlay.notification.clone();
        }
    }
}

/// Merge a user/overlay `seams` map onto a base/project map with per-boundary
/// replacement semantics and an existence floor.
///
/// **Framework → project** callers use [`overlay`] instead (additive, no
/// floor). This function is only called by [`merge_with_floor`] for the
/// project → user step.
///
/// Rules (documented in `.claude/rules/stack-loops.md` seam section):
///
/// 1. **Project boundary existence is a floor.** If the project declares
///    boundary `"A↔B"`, the user may:
///    - Omit the boundary → the project list is preserved.
///    - Restate the boundary with a non-empty list → the list REPLACES the
///      project list.
///    - Restate with `[]` (empty) → FLOOR VIOLATION; a user cannot
///      silently turn off required boundary checks.
/// 2. **New user boundaries are additive.** A boundary declared by the user
///    but not by the project is inserted as-is (empty-list still rejected
///    since a boundary with no checks is indistinguishable from "off").
/// 3. **Check-list direction is not floor-guarded.** Swapping a strict check
///    for a lenient one, or reordering, is a legitimate project-specific
///    choice — mirrors the `max_passes` exemption from Phase 2 floor merge.
///    The validator still rejects UNKNOWN check IDs and duplicate IDs.
/// 4. **Boundary keys are canonicalized.** `"A↔B"` and `"B↔A"` address the
///    same boundary. If user and project spell the same boundary
///    differently, the user entry replaces the project entry and the
///    canonical key wins in the output map. Malformed boundary keys
///    (missing separator, self-boundary) are passed through unchanged —
///    `validate_seams` will reject them on the resolved config.
pub fn merge_seams(
    base: &mut Option<BTreeMap<String, Vec<String>>>,
    overlay: Option<&BTreeMap<String, Vec<String>>>,
    violations: &mut Vec<FloorViolation>,
) {
    let Some(overlay) = overlay else {
        return;
    };

    // Build a canonical-form lookup of project-declared boundaries so user
    // entries expressed with either separator (`↔` or `<->`) or inverted
    // order collide with the correct project entry.
    let base_map = base.get_or_insert_with(BTreeMap::new);
    let mut project_canonical_keys: std::collections::HashMap<String, String> = Default::default();
    for key in base_map.keys() {
        if let Ok(b) = crate::seam::types::LayerBoundary::parse(key) {
            project_canonical_keys.insert(b.canonical(), key.clone());
        }
    }

    for (raw_key, user_list) in overlay {
        let canonical = crate::seam::types::LayerBoundary::parse(raw_key)
            .map(|b| b.canonical())
            .ok();

        // Is this a project-declared boundary (by canonical form)?
        let project_key = canonical
            .as_ref()
            .and_then(|c| project_canonical_keys.get(c).cloned());

        if user_list.is_empty() {
            // Empty list = disable the boundary. Forbidden for any boundary
            // (project-declared or brand-new user addition) because the
            // semantics are ambiguous — use a real check ID or omit the key.
            let project_display = project_key
                .as_ref()
                .and_then(|k| base_map.get(k))
                .map(|v| format!("{v:?}"))
                .unwrap_or_else(|| "(none)".into());
            violations.push(FloorViolation {
                field: format!("seams.{raw_key}"),
                project: project_display,
                user: "[]".into(),
                reason: "seam boundary check list cannot be empty — \
                         omit the key to inherit the project list, or list real check IDs",
            });
            continue;
        }

        match project_key {
            Some(existing_key) => {
                // User is replacing a project-declared boundary. Remove the
                // project's original key (may differ in spelling) and insert
                // under the user's raw key so the output reflects user intent.
                base_map.remove(&existing_key);
                base_map.insert(raw_key.clone(), user_list.clone());
            }
            None => {
                // New user boundary — additive. If raw_key is already in
                // base_map (same raw spelling without a parseable
                // canonical), just replace.
                base_map.insert(raw_key.clone(), user_list.clone());
            }
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::schema::{
        CostCapBehavior, Defaults, LayerOverride, OnTimeout, ReviewConfig, WorkflowConfig,
    };

    fn base() -> WorkflowConfig {
        WorkflowConfig {
            schema_version: "0.2".into(),
            defaults: Defaults {
                tier: 2,
                min_confidence: 0.90,
                max_passes: 5,
                model: "sonnet".into(),
                budget_usd: 2.0,
                cost_cap_behavior: CostCapBehavior::Halt,
                max_parallelism: None,
            },
            phases: Default::default(),
            layer_overrides: BTreeMap::new(),
            review: None,
            seams: None,
        }
    }

    fn overlay_from(base: &WorkflowConfig) -> WorkflowConfig {
        base.clone()
    }

    #[test]
    fn user_raises_min_confidence_allowed() {
        let mut u = overlay_from(&base());
        u.defaults.min_confidence = 0.95;
        let merged = merge_with_floor(base(), u).unwrap();
        assert!((merged.defaults.min_confidence - 0.95).abs() < 1e-9);
    }

    #[test]
    fn user_lowers_min_confidence_rejected() {
        let mut u = overlay_from(&base());
        u.defaults.min_confidence = 0.80;
        let err = merge_with_floor(base(), u).unwrap_err().to_string();
        assert!(err.contains("min_confidence"), "err: {err}");
        assert!(err.contains("0.9") || err.contains("0.90"), "err: {err}");
    }

    #[test]
    fn user_lowers_budget_allowed() {
        let mut u = overlay_from(&base());
        u.defaults.budget_usd = 1.0;
        let merged = merge_with_floor(base(), u).unwrap();
        assert!((merged.defaults.budget_usd - 1.0).abs() < 1e-9);
    }

    #[test]
    fn user_raises_budget_rejected() {
        let mut u = overlay_from(&base());
        u.defaults.budget_usd = 10.0;
        let err = merge_with_floor(base(), u).unwrap_err().to_string();
        assert!(err.contains("budget_usd"), "err: {err}");
    }

    #[test]
    fn user_raises_tier_allowed() {
        let mut u = overlay_from(&base());
        u.defaults.tier = 3;
        let merged = merge_with_floor(base(), u).unwrap();
        assert_eq!(merged.defaults.tier, 3);
    }

    #[test]
    fn user_lowers_tier_rejected() {
        let mut b = base();
        b.defaults.tier = 3;
        let mut u = overlay_from(&b);
        u.defaults.tier = 2;
        let err = merge_with_floor(b, u).unwrap_err().to_string();
        assert!(err.contains("tier"), "err: {err}");
    }

    #[test]
    fn user_changes_max_passes_always_allowed() {
        // max_passes isn't floor-guarded per PRDv2 lines 908–916 — both
        // directions are fine. Test both.
        let mut up = overlay_from(&base());
        up.defaults.max_passes = 10;
        assert_eq!(
            merge_with_floor(base(), up).unwrap().defaults.max_passes,
            10
        );

        let mut down = overlay_from(&base());
        down.defaults.max_passes = 2;
        assert_eq!(
            merge_with_floor(base(), down).unwrap().defaults.max_passes,
            2
        );
    }

    #[test]
    fn user_enables_review_when_project_false_allowed() {
        let mut b = base();
        b.review = Some(ReviewConfig {
            enabled: false,
            ..Default::default()
        });
        let mut u = overlay_from(&b);
        u.review = Some(ReviewConfig {
            enabled: true,
            trigger: Some("tier >= 3".into()),
            ..Default::default()
        });
        let merged = merge_with_floor(b, u).unwrap();
        assert!(merged.review.unwrap().enabled);
    }

    #[test]
    fn user_disables_required_review_rejected() {
        let mut b = base();
        b.review = Some(ReviewConfig {
            enabled: true,
            trigger: Some("always".into()),
            timeout_hours: 24,
            on_timeout: OnTimeout::Reject,
            notification: "stdout".into(),
        });
        let mut u = overlay_from(&b);
        u.review = Some(ReviewConfig {
            enabled: false,
            ..b.review.clone().unwrap()
        });
        let err = merge_with_floor(b, u).unwrap_err().to_string();
        assert!(err.contains("review"), "err: {err}");
    }

    #[test]
    fn review_trigger_weakening_to_false_rejected() {
        // Project committed `review.trigger = "always"`. User rewrites to
        // `"false"` — syntactically a valid trigger expression, semantically
        // a gate that never fires. This is a floor bypass: the user has
        // disabled the gate without removing the field.
        let mut b = base();
        b.review = Some(ReviewConfig {
            enabled: true,
            trigger: Some("always".into()),
            ..Default::default()
        });
        let mut u = overlay_from(&b);
        u.review = Some(ReviewConfig {
            enabled: true,
            trigger: Some("false".into()),
            ..b.review.clone().unwrap()
        });
        let err = merge_with_floor(b, u).unwrap_err();
        let fv = err.downcast_ref::<FloorViolations>().unwrap();
        assert!(
            fv.violations
                .iter()
                .any(|v| v.field == "review.trigger" && v.user == "false"),
            "expected review.trigger weakening violation, got: {fv:?}"
        );
    }

    #[test]
    fn review_trigger_rewriting_to_arbitrary_expression_rejected() {
        // Project committed `tier >= 3`. User rewrites to `tier >= 999` —
        // syntactically valid but semantically unreachable. Floor bypass.
        let mut b = base();
        b.review = Some(ReviewConfig {
            enabled: true,
            trigger: Some("tier >= 3".into()),
            ..Default::default()
        });
        let mut u = overlay_from(&b);
        u.review = Some(ReviewConfig {
            enabled: true,
            trigger: Some("tier >= 999".into()),
            ..b.review.clone().unwrap()
        });
        let err = merge_with_floor(b, u).unwrap_err();
        let fv = err.downcast_ref::<FloorViolations>().unwrap();
        assert!(fv.violations.iter().any(|v| v.field == "review.trigger"));
    }

    #[test]
    fn identical_invalid_trigger_does_not_mask_parse_error() {
        // Both project and user have an identical (but syntactically
        // invalid) trigger. Floor-merge must NOT report this as a rewrite
        // violation — that would mask the real parse error which
        // `validate_triggers` will surface separately on the resolved
        // config. Identical text is always equivalent.
        let mut b = base();
        b.review = Some(ReviewConfig {
            enabled: true,
            trigger: Some("tier ==".into()), // invalid
            ..Default::default()
        });
        let mut u = overlay_from(&b);
        u.review = Some(ReviewConfig {
            enabled: true,
            trigger: Some("tier ==".into()), // identical to project
            ..b.review.clone().unwrap()
        });
        // Merge succeeds; no floor violation is fabricated. The resolved
        // config still has the invalid trigger, which `validate_triggers`
        // (called in the validate handler and the evaluate handler) will
        // catch with a real parse error including line/column.
        let merged =
            merge_with_floor(b, u).expect("identical triggers must not be flagged as rewrites");
        assert_eq!(merged.review.unwrap().trigger.as_deref(), Some("tier =="));
    }

    #[test]
    fn review_trigger_ast_equivalent_restatement_allowed() {
        // Project trigger `always`. User restates as `true` — semantically
        // identical (both lex to a literal-true), so AST comparison treats
        // them as the same expression. Floor-merge should accept.
        let mut b = base();
        b.review = Some(ReviewConfig {
            enabled: true,
            trigger: Some("always".into()),
            ..Default::default()
        });
        let mut u = overlay_from(&b);
        u.review = Some(ReviewConfig {
            enabled: true,
            trigger: Some("true".into()),
            ..b.review.clone().unwrap()
        });
        let merged =
            merge_with_floor(b, u).expect("AST-equivalent trigger restatement should pass");
        assert_eq!(
            merged.review.unwrap().trigger.as_deref(),
            // The user's spelling wins when ASTs match (both are valid).
            Some("true")
        );
    }

    #[test]
    fn review_trigger_whitespace_differences_allowed() {
        // Project: "tier >= 3"; user: "tier>=3" with no spaces. Same AST.
        let mut b = base();
        b.review = Some(ReviewConfig {
            enabled: true,
            trigger: Some("tier >= 3".into()),
            ..Default::default()
        });
        let mut u = overlay_from(&b);
        u.review = Some(ReviewConfig {
            enabled: true,
            trigger: Some("tier>=3".into()),
            ..b.review.clone().unwrap()
        });
        merge_with_floor(b, u).expect("whitespace-only difference should pass");
    }

    #[test]
    fn review_trigger_exact_match_allowed() {
        // Confirming the policy escape valve: a user may match the project
        // trigger exactly. This is the supported way to "acknowledge" the
        // gate without overriding it.
        let mut b = base();
        b.review = Some(ReviewConfig {
            enabled: true,
            trigger: Some("tier >= 3".into()),
            ..Default::default()
        });
        let mut u = overlay_from(&b);
        u.review = Some(ReviewConfig {
            enabled: true,
            trigger: Some("tier >= 3".into()),
            timeout_hours: 12, // tighten timeout — allowed (tuning knob)
            ..b.review.clone().unwrap()
        });
        let merged = merge_with_floor(b, u).expect("exact-match trigger should pass");
        assert_eq!(merged.review.unwrap().trigger.as_deref(), Some("tier >= 3"));
    }

    #[test]
    fn layer_override_trigger_weakening_rejected() {
        // Same policy applied to per-layer triggers: project sets
        // `infrastructure.trigger = "always"`, user rewrites to `"false"`.
        let mut b = base();
        b.layer_overrides.insert(
            "infrastructure".into(),
            LayerOverride {
                trigger: Some("always".into()),
                ..Default::default()
            },
        );
        let mut u = overlay_from(&b);
        u.layer_overrides.insert(
            "infrastructure".into(),
            LayerOverride {
                trigger: Some("false".into()),
                ..Default::default()
            },
        );
        let err = merge_with_floor(b, u).unwrap_err();
        let fv = err.downcast_ref::<FloorViolations>().unwrap();
        assert!(fv
            .violations
            .iter()
            .any(|v| v.field == "layer_overrides.infrastructure.trigger"));
    }

    #[test]
    fn required_review_trigger_removal_rejected() {
        // Project review has a required trigger; user attempts to remove it
        // (sets trigger = None in overlay). This is the Row 5 floor-table rule.
        let mut b = base();
        b.review = Some(ReviewConfig {
            enabled: true,
            trigger: Some("tier >= 3".into()),
            ..Default::default()
        });
        let mut u = overlay_from(&b);
        u.review = Some(ReviewConfig {
            enabled: true,
            trigger: None, // removal attempt
            ..b.review.clone().unwrap()
        });
        let err = merge_with_floor(b, u).unwrap_err();
        let fv = err.downcast_ref::<FloorViolations>().unwrap();
        assert!(
            fv.violations.iter().any(|v| v.field == "review.trigger"
                && v.reason == "required gate trigger cannot be removed"),
            "expected review.trigger removal violation, got: {fv:?}"
        );
    }

    #[test]
    fn layer_override_require_review_downgrade_rejected() {
        // Project layer override sets require_review: true; user attempts
        // to disable it. Row 4 of the floor table, per-layer variant.
        let mut b = base();
        b.layer_overrides.insert(
            "infrastructure".into(),
            LayerOverride {
                require_review: Some(true),
                ..Default::default()
            },
        );
        let mut u = overlay_from(&b);
        u.layer_overrides.insert(
            "infrastructure".into(),
            LayerOverride {
                require_review: Some(false),
                ..Default::default()
            },
        );
        let err = merge_with_floor(b, u).unwrap_err();
        let fv = err.downcast_ref::<FloorViolations>().unwrap();
        assert!(fv
            .violations
            .iter()
            .any(|v| v.field == "layer_overrides.infrastructure.require_review"));
    }

    #[test]
    fn fresh_layer_tier_override_checked_against_defaults() {
        // Project has defaults.tier = 3 and NO layer_overrides entry for
        // "backend". A user adding layer_overrides.backend.tier = 1 must be
        // rejected against the project's defaults floor (Codex critical
        // finding regression).
        let mut b = base();
        b.defaults.tier = 3;
        let mut u = overlay_from(&b);
        u.layer_overrides.insert(
            "backend".into(),
            LayerOverride {
                tier: Some(1),
                ..Default::default()
            },
        );
        let err = merge_with_floor(b, u).unwrap_err();
        let fv = err.downcast_ref::<FloorViolations>().unwrap();
        assert!(
            fv.violations
                .iter()
                .any(|v| v.field == "layer_overrides.backend.tier"),
            "expected floor violation on fresh layer, got: {fv:?}"
        );
    }

    #[test]
    fn fresh_layer_min_confidence_override_checked_against_defaults() {
        let mut b = base();
        b.defaults.min_confidence = 0.95;
        let mut u = overlay_from(&b);
        u.layer_overrides.insert(
            "backend".into(),
            LayerOverride {
                min_confidence: Some(0.80),
                ..Default::default()
            },
        );
        let err = merge_with_floor(b, u).unwrap_err();
        let fv = err.downcast_ref::<FloorViolations>().unwrap();
        assert!(fv
            .violations
            .iter()
            .any(|v| v.field == "layer_overrides.backend.min_confidence"));
    }

    #[test]
    fn fresh_layer_budget_override_checked_against_defaults() {
        let mut b = base();
        b.defaults.budget_usd = 1.0;
        let mut u = overlay_from(&b);
        u.layer_overrides.insert(
            "backend".into(),
            LayerOverride {
                budget_usd: Some(10.0), // higher = relaxation
                ..Default::default()
            },
        );
        let err = merge_with_floor(b, u).unwrap_err();
        let fv = err.downcast_ref::<FloorViolations>().unwrap();
        assert!(fv
            .violations
            .iter()
            .any(|v| v.field == "layer_overrides.backend.budget_usd"));
    }

    #[test]
    fn project_committed_layer_exemption_preserved_under_global_gate() {
        // Project globally enables review AND explicitly exempts one layer
        // via `layer_overrides.experimental.require_review: false`. A user
        // workflow that keeps the exemption (require_review: false) while
        // tightening some other field must NOT be rejected — the project
        // committed to the exemption, so the user is matching, not relaxing.
        let mut b = base();
        b.review = Some(ReviewConfig {
            enabled: true,
            ..Default::default()
        });
        b.layer_overrides.insert(
            "experimental".into(),
            LayerOverride {
                require_review: Some(false),
                ..Default::default()
            },
        );

        let mut u = overlay_from(&b);
        u.layer_overrides.insert(
            "experimental".into(),
            LayerOverride {
                require_review: Some(false), // match the project exemption
                min_confidence: Some(0.95),  // tighten something else
                ..Default::default()
            },
        );

        // Must succeed — user is matching the project's effective per-layer
        // value, not undercutting it.
        let merged = merge_with_floor(b, u).expect("legitimate exemption should merge");
        assert_eq!(
            merged.layer_overrides["experimental"].require_review,
            Some(false)
        );
    }

    #[test]
    fn fresh_layer_require_review_override_checked_against_global() {
        // Project has global review.enabled = true and NO per-layer entry.
        // User tries to exempt one layer: require_review = false.
        let mut b = base();
        b.review = Some(ReviewConfig {
            enabled: true,
            ..Default::default()
        });
        let mut u = overlay_from(&b);
        u.layer_overrides.insert(
            "backend".into(),
            LayerOverride {
                require_review: Some(false),
                ..Default::default()
            },
        );
        let err = merge_with_floor(b, u).unwrap_err();
        let fv = err.downcast_ref::<FloorViolations>().unwrap();
        assert!(fv
            .violations
            .iter()
            .any(|v| v.field == "layer_overrides.backend.require_review"));
    }

    #[test]
    fn layer_override_restrict_confidence_allowed() {
        let mut b = base();
        b.layer_overrides.insert(
            "infrastructure".into(),
            LayerOverride {
                min_confidence: Some(0.92),
                ..Default::default()
            },
        );
        let mut u = overlay_from(&b);
        u.layer_overrides.insert(
            "infrastructure".into(),
            LayerOverride {
                min_confidence: Some(0.97),
                ..Default::default()
            },
        );
        let merged = merge_with_floor(b, u).unwrap();
        assert_eq!(
            merged.layer_overrides["infrastructure"].min_confidence,
            Some(0.97)
        );
    }

    #[test]
    fn layer_override_relax_confidence_rejected() {
        let mut b = base();
        b.layer_overrides.insert(
            "infrastructure".into(),
            LayerOverride {
                min_confidence: Some(0.95),
                ..Default::default()
            },
        );
        let mut u = overlay_from(&b);
        u.layer_overrides.insert(
            "infrastructure".into(),
            LayerOverride {
                min_confidence: Some(0.85),
                ..Default::default()
            },
        );
        let err = merge_with_floor(b, u).unwrap_err().to_string();
        assert!(err.contains("infrastructure"), "err: {err}");
        assert!(err.contains("min_confidence"), "err: {err}");
    }

    #[test]
    fn empty_user_returns_base_unchanged() {
        let merged = merge_with_floor(base(), base()).unwrap();
        assert_eq!(merged, base());
    }

    #[test]
    fn adversarial_relax_all_floors_collects_all_violations() {
        // Exercises EVERY floor-protected vector in one pass:
        //   1. defaults.tier (lower)
        //   2. defaults.min_confidence (lower)
        //   3. defaults.budget_usd (raise)
        //   4. review.enabled (disable)
        //   5. review.trigger (remove)
        //   6. layer_overrides.{existing}.require_review (downgrade)
        //   7. layer_overrides.{fresh}.tier (undercut defaults via new layer)
        //
        // max_passes is intentionally excluded (not floor-guarded per PRDv2).
        // Asserts ALL seven violations are collected in a single merge call —
        // a single-miss bypass defeats the team-wide policy guarantee.
        let mut b = base();
        b.defaults.tier = 3;
        b.defaults.min_confidence = 0.95;
        b.defaults.budget_usd = 2.0;
        b.review = Some(ReviewConfig {
            enabled: true,
            trigger: Some("tier >= 3".into()),
            ..Default::default()
        });
        b.layer_overrides.insert(
            "infrastructure".into(),
            LayerOverride {
                require_review: Some(true),
                ..Default::default()
            },
        );

        let mut u = overlay_from(&b);
        u.defaults.tier = 2; // lower (violation #1)
        u.defaults.min_confidence = 0.80; // lower (violation #2)
        u.defaults.budget_usd = 10.0; // raise (violation #3)
        u.review = Some(ReviewConfig {
            enabled: false, // disable required review (violation #4)
            trigger: None,  // remove required trigger (violation #5)
            ..Default::default()
        });
        // Downgrade existing layer's require_review (violation #6):
        u.layer_overrides.insert(
            "infrastructure".into(),
            LayerOverride {
                require_review: Some(false),
                ..Default::default()
            },
        );
        // Fresh layer override that undercuts defaults.tier (violation #7):
        u.layer_overrides.insert(
            "backend".into(),
            LayerOverride {
                tier: Some(1),
                ..Default::default()
            },
        );

        let err = merge_with_floor(b, u).unwrap_err();
        let fv = err.downcast_ref::<FloorViolations>().unwrap();
        let fields: Vec<&str> = fv.violations.iter().map(|v| v.field.as_str()).collect();

        assert!(
            fields.contains(&"defaults.tier"),
            "missing defaults.tier: {fields:?}"
        );
        assert!(
            fields.contains(&"defaults.min_confidence"),
            "missing defaults.min_confidence: {fields:?}"
        );
        assert!(
            fields.contains(&"defaults.budget_usd"),
            "missing defaults.budget_usd: {fields:?}"
        );
        assert!(
            fields.contains(&"review.enabled"),
            "missing review.enabled: {fields:?}"
        );
        assert!(
            fields.contains(&"review.trigger"),
            "missing review.trigger: {fields:?}"
        );
        assert!(
            fields.contains(&"layer_overrides.infrastructure.require_review"),
            "missing layer require_review: {fields:?}"
        );
        assert!(
            fields.contains(&"layer_overrides.backend.tier"),
            "missing fresh-layer tier escape: {fields:?}"
        );
        assert!(
            fv.violations.len() >= 7,
            "expected at least 7 violations, got {}: {fields:?}",
            fv.violations.len()
        );
    }

    // ─── Seam merge tests ───────────────────────────────────────────────

    fn base_with_seams(entries: &[(&str, &[&str])]) -> WorkflowConfig {
        let mut cfg = base();
        let mut map = BTreeMap::new();
        for (k, v) in entries {
            map.insert(k.to_string(), v.iter().map(|s| s.to_string()).collect());
        }
        cfg.seams = Some(map);
        cfg
    }

    fn overlay_with_seams(entries: &[(&str, &[&str])]) -> WorkflowConfig {
        let mut cfg = base();
        let mut map = BTreeMap::new();
        for (k, v) in entries {
            map.insert(k.to_string(), v.iter().map(|s| s.to_string()).collect());
        }
        cfg.seams = Some(map);
        cfg
    }

    #[test]
    fn seam_replace_wins_on_user_override() {
        let project = base_with_seams(&[("backend↔infrastructure", &["config_mismatch"])]);
        let user =
            overlay_with_seams(&[("backend↔infrastructure", &["schema_drift", "version_skew"])]);
        let merged = merge_with_floor(project, user).unwrap();
        let seams = merged.seams.unwrap();
        assert_eq!(
            seams.get("backend↔infrastructure").unwrap(),
            &vec!["schema_drift".to_string(), "version_skew".to_string()]
        );
    }

    #[test]
    fn seam_omit_preserves_project_list() {
        let project = base_with_seams(&[("backend↔infrastructure", &["config_mismatch"])]);
        let user = base(); // no seams at all
        let merged = merge_with_floor(project, user).unwrap();
        let seams = merged.seams.unwrap();
        assert_eq!(
            seams.get("backend↔infrastructure").unwrap(),
            &vec!["config_mismatch".to_string()]
        );
    }

    #[test]
    fn seam_empty_list_for_project_boundary_is_violation() {
        let project = base_with_seams(&[("backend↔infrastructure", &["config_mismatch"])]);
        let user = overlay_with_seams(&[("backend↔infrastructure", &[])]);
        let err = merge_with_floor(project, user).unwrap_err();
        let fv: &FloorViolations = err.downcast_ref::<FloorViolations>().expect("FloorViolations");
        assert!(fv
            .violations
            .iter()
            .any(|v| v.field == "seams.backend↔infrastructure"
                && v.reason.contains("cannot be empty")));
    }

    #[test]
    fn seam_empty_list_for_new_user_boundary_also_rejected() {
        let project = base();
        let user = overlay_with_seams(&[("backend↔infrastructure", &[])]);
        let err = merge_with_floor(project, user).unwrap_err();
        let fv: &FloorViolations = err.downcast_ref::<FloorViolations>().expect("FloorViolations");
        assert!(fv.violations.iter().any(|v| v.user == "[]"));
    }

    #[test]
    fn seam_user_may_add_new_boundary() {
        let project = base_with_seams(&[("backend↔database", &["schema_drift"])]);
        let user =
            overlay_with_seams(&[("frontend↔api", &["openapi_compliance"])]);
        let merged = merge_with_floor(project, user).unwrap();
        let seams = merged.seams.unwrap();
        assert!(seams.contains_key("backend↔database"));
        assert!(seams.contains_key("frontend↔api"));
    }

    #[test]
    fn seam_framework_to_project_is_overlay_not_floor() {
        // The framework→project step uses `overlay`, which does NOT apply
        // the empty-list floor. Projects MAY remove a framework-declared
        // boundary by setting it to a new list; but this test just verifies
        // overlay's additive behavior for seams.
        let framework = base_with_seams(&[("backend↔infrastructure", &["config_mismatch"])]);
        let project =
            overlay_with_seams(&[("frontend↔api", &["openapi_compliance"])]);
        let merged = overlay(framework, project);
        let seams = merged.seams.unwrap();
        assert!(seams.contains_key("backend↔infrastructure"));
        assert!(seams.contains_key("frontend↔api"));
    }

    #[test]
    fn seam_user_list_ordering_preserved() {
        let project = base_with_seams(&[("backend↔infrastructure", &["config_mismatch"])]);
        let user = overlay_with_seams(&[(
            "backend↔infrastructure",
            &["schema_drift", "auth_handoff", "version_skew"],
        )]);
        let merged = merge_with_floor(project, user).unwrap();
        let list = merged
            .seams
            .unwrap()
            .get("backend↔infrastructure")
            .cloned()
            .unwrap();
        assert_eq!(list, vec!["schema_drift", "auth_handoff", "version_skew"]);
    }

    #[test]
    fn seam_canonicalization_collides_inverted_keys() {
        // User expresses the boundary in inverted form with `<->`; it
        // must collide with the project's canonical `↔` form and replace.
        let project = base_with_seams(&[("backend↔infrastructure", &["config_mismatch"])]);
        let user =
            overlay_with_seams(&[("infrastructure<->backend", &["version_skew"])]);
        let merged = merge_with_floor(project, user).unwrap();
        let seams = merged.seams.unwrap();
        // The project's original key is gone; user's raw key wins.
        assert!(
            !seams.contains_key("backend↔infrastructure"),
            "project key should be replaced: {seams:?}"
        );
        assert_eq!(
            seams.get("infrastructure<->backend").unwrap(),
            &vec!["version_skew".to_string()]
        );
    }
}

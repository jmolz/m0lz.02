//! Phase 6 review-gate primitives — pure helpers consumed by the daemon.
//!
//! This module owns the gate lifecycle logic that does NOT touch I/O,
//! async, or locks: deciding whether a gate has expired, building the
//! audit string for a decision, constructing gate IDs, and populating a
//! `TriggerContext` for the workflow-trigger AST evaluator.
//!
//! Living in `pice-core` keeps these invariants unit-testable at
//! millisecond speed (one of the Phase-4 lessons pinned in
//! `.claude/rules/rust-core.md`). The daemon's orchestrator + RPC
//! handlers consume them; the CLI never calls this module directly —
//! gate decisions always flow through the daemon RPC surface.
//!
//! Halt-prefix constants for gate-originated halts live in
//! [`crate::cli::ExitJsonStatus`] (next to the existing `sprt_*` /
//! `cancelled:*` / `metrics_persist_failed:` prefixes) so every halt
//! family shares one const + helper site.

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::layers::manifest::{GateEntry, GateStatus};
use crate::workflow::schema::OnTimeout;
use crate::workflow::trigger::TriggerContext;

// ─── Gate identifiers ──────────────────────────────────────────────────────

/// Globally-unique gate id in the form
/// `{feature_id}:{layer}:{ts_ms:012x}{counter:08x}`.
///
/// The timestamp-plus-atomic-counter composition guarantees uniqueness
/// even under concurrent calls (the counter is process-static) without
/// pulling in a `ulid` or `uuid` dependency — pice-core stays dep-light.
/// Callers in tests can pass a frozen `now` for deterministic output;
/// in production the daemon threads `ctx.clock.now()`.
pub fn new_gate_id(feature_id: &str, layer: &str, now: DateTime<Utc>) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let ts = now.timestamp_millis().max(0) as u64;
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{feature_id}:{layer}:{ts:012x}{c:08x}")
}

// ─── TriggerContext constructor ────────────────────────────────────────────

/// Phase 6 constructor for evaluating a gate's trigger expression against
/// a specific layer's current state.
///
/// Phase 2 shipped the trigger parser + evaluator; this constructor
/// gives `check_gates_for_cohort` + the gate-decide handler a single
/// builder so call sites don't hand-build the context and risk
/// forgetting a field.
pub fn trigger_context_for_layer(
    tier: u8,
    layer: &str,
    confidence: f64,
    cost: f64,
    passes: u32,
    change_scope: &str,
) -> TriggerContext {
    TriggerContext {
        tier,
        layer: layer.to_string(),
        confidence,
        cost,
        passes,
        change_scope: change_scope.to_string(),
    }
}

// ─── Decision + origin ─────────────────────────────────────────────────────

/// Reviewer-supplied decision. Serialized in kebab-case on the CLI ↔
/// daemon RPC boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GateDecision {
    Approve,
    Reject,
    Skip,
}

/// Did the decision come from a human (`Manual`) or the timeout
/// reconciler (`Timeout`)? The audit-decision string encodes this
/// distinction so dashboards can report manual-reject rates separately
/// from timeout-reject rates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecisionOrigin {
    Manual,
    Timeout,
}

/// The pair `(decision, origin)` that fully characterizes a gate
/// resolution. Produced by [`GateDecisionOutcome::manual`] and
/// [`GateDecisionOutcome::timeout`]; consumed by the SQLite writer,
/// the manifest mutator, and the CLI renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateDecisionOutcome {
    pub decision: GateDecision,
    pub origin: GateDecisionOrigin,
}

impl GateDecisionOutcome {
    /// Construct a manual-origin outcome for an approve / reject / skip
    /// decision a reviewer supplied interactively or via CLI flag.
    pub fn manual(decision: GateDecision) -> Self {
        Self {
            decision,
            origin: GateDecisionOrigin::Manual,
        }
    }

    /// Construct a timeout-origin outcome by projecting `OnTimeout` onto
    /// the reviewer-decision space. `OnTimeout::Reject` → `GateDecision::Reject`,
    /// etc. The origin is recorded as `Timeout` so the audit row labels
    /// it `timeout_reject` (etc.), not `reject`.
    pub fn timeout(on_timeout: OnTimeout) -> Self {
        let decision = match on_timeout {
            OnTimeout::Approve => GateDecision::Approve,
            OnTimeout::Reject => GateDecision::Reject,
            OnTimeout::Skip => GateDecision::Skip,
        };
        Self {
            decision,
            origin: GateDecisionOrigin::Timeout,
        }
    }

    /// String persisted in the SQLite `gate_decisions.decision` column
    /// and the manifest `GateEntry.decision` field. One of the six values
    /// enforced by the v4 migration's CHECK constraint:
    ///
    /// ```sql
    /// decision IN ('approve','reject','skip',
    ///              'timeout_reject','timeout_approve','timeout_skip')
    /// ```
    ///
    /// Any change here must move the CHECK constraint in the same PR.
    pub fn audit_decision_string(&self) -> &'static str {
        match (self.decision, self.origin) {
            (GateDecision::Approve, GateDecisionOrigin::Manual) => "approve",
            (GateDecision::Reject, GateDecisionOrigin::Manual) => "reject",
            (GateDecision::Skip, GateDecisionOrigin::Manual) => "skip",
            (GateDecision::Approve, GateDecisionOrigin::Timeout) => "timeout_approve",
            (GateDecision::Reject, GateDecisionOrigin::Timeout) => "timeout_reject",
            (GateDecision::Skip, GateDecisionOrigin::Timeout) => "timeout_skip",
        }
    }

    /// Reverse of [`audit_decision_string`]. Used by idempotent crash-
    /// recovery in the decide handler: when the SQLite `gate_decisions`
    /// row exists but the manifest gate is still `Pending` (a prior
    /// decide crashed between audit insert and manifest save), the
    /// handler re-derives the original outcome from the durable audit
    /// row and re-applies the manifest mutation.
    ///
    /// Returns `None` on unrecognized strings — callers log + abort
    /// recovery rather than guessing.
    pub fn from_audit_decision_string(s: &str) -> Option<Self> {
        let (decision, origin) = match s {
            "approve" => (GateDecision::Approve, GateDecisionOrigin::Manual),
            "reject" => (GateDecision::Reject, GateDecisionOrigin::Manual),
            "skip" => (GateDecision::Skip, GateDecisionOrigin::Manual),
            "timeout_approve" => (GateDecision::Approve, GateDecisionOrigin::Timeout),
            "timeout_reject" => (GateDecision::Reject, GateDecisionOrigin::Timeout),
            "timeout_skip" => (GateDecision::Skip, GateDecisionOrigin::Timeout),
            _ => return None,
        };
        Some(Self { decision, origin })
    }
}

// ─── Timeout resolution ────────────────────────────────────────────────────

/// Decide whether a pending gate has expired at `now`, INLINE-checking
/// the gate's current status. Returns `Some(outcome)` only when
/// `status == Pending AND now >= requested_at + timeout_hours`.
///
/// The status check inside this function closes the Codex Cycle-2
/// decide/reap race: a concurrent manual decision may have already
/// transitioned the gate out of `Pending` before the reconciler
/// reached the mutation. Callers MUST use this helper (or
/// [`apply_timeout_if_expired`] which wraps it) rather than inlining
/// the `now >= deadline` check — inlining misses the status guard.
pub fn resolve_timeout_action(
    status: GateStatus,
    requested_at: DateTime<Utc>,
    timeout_hours: u32,
    on_timeout: OnTimeout,
    now: DateTime<Utc>,
) -> Option<GateDecisionOutcome> {
    if status != GateStatus::Pending {
        return None;
    }
    let deadline = requested_at + ChronoDuration::hours(timeout_hours as i64);
    if now >= deadline {
        Some(GateDecisionOutcome::timeout(on_timeout))
    } else {
        None
    }
}

/// In-place apply a timeout to a pending gate, reading the pinned
/// `timeout_at` field from the entry itself. Caller holds the manifest
/// lock.
///
/// - `Some(outcome)` — gate was `Pending` and `now >= timeout_at`; the
///   gate's `status`, `decision`, and `decided_at` fields have been
///   mutated to reflect the timeout. Caller must still write the
///   `gate_decisions` audit row BEFORE saving the manifest.
/// - `None` — gate was not `Pending` OR `now < timeout_at` OR
///   `timeout_at` could not be parsed (operationally impossible for a
///   daemon-written gate; log and skip if it ever happens).
///
/// The plan signature accepts `on_timeout_action` as a parameter rather
/// than reading `gate.on_timeout_action` inline — this keeps the helper
/// flexible for the reconciler case where an operator-facing override
/// might one day apply, and surfaces the action at the call site so a
/// future reviewer sees "what timeout action is this applying?" without
/// having to chase the gate's history.
pub fn apply_timeout_if_expired(
    gate: &mut GateEntry,
    on_timeout_action: OnTimeout,
    now: DateTime<Utc>,
) -> Option<GateDecisionOutcome> {
    if gate.status != GateStatus::Pending {
        return None;
    }
    let deadline: DateTime<Utc> = match gate.timeout_at.parse() {
        Ok(t) => t,
        Err(_) => {
            tracing::warn!(
                gate_id = %gate.id,
                timeout_at = %gate.timeout_at,
                "gate.timeout_at is not RFC3339; skipping timeout check",
            );
            return None;
        }
    };
    if now < deadline {
        return None;
    }
    let outcome = GateDecisionOutcome::timeout(on_timeout_action);
    gate.decision = Some(outcome.audit_decision_string().to_string());
    gate.decided_at = Some(now.to_rfc3339());
    gate.status = match outcome.decision {
        GateDecision::Approve => GateStatus::Approved,
        GateDecision::Reject => GateStatus::Rejected,
        GateDecision::Skip => GateStatus::Skipped,
    };
    Some(outcome)
}

// ─── Cohort-boundary gate firing ───────────────────────────────────────────

use crate::layers::manifest::LayerStatus;
use crate::workflow::trigger;
use crate::workflow::WorkflowConfig;

/// Decide, for a layer that just reached `Passed` at a cohort boundary,
/// whether a review gate should fire. Returns the effective trigger string
/// (verbatim, for audit) when one fires; `None` otherwise.
///
/// Trigger resolution order (matches plan lines 270-272):
/// 1. `layer_overrides.<layer>.require_review == Some(true)` → literal `"true"`
/// 2. `layer_overrides.<layer>.trigger` — a per-layer trigger expression
/// 3. `review.trigger` (only if `review.enabled`) — the project default
///
/// Any other combination returns `None` (gate does not fire). Parse errors
/// on either trigger surface are logged once and treated as "does not fire"
/// — the validator should have failed-closed upstream.
pub fn resolve_gate_trigger(
    workflow: &WorkflowConfig,
    layer: &str,
    ctx: &TriggerContext,
) -> Option<String> {
    if let Some(lo) = workflow.layer_overrides.get(layer) {
        if lo.require_review == Some(true) {
            return Some("true".to_string());
        }
        if let Some(expr) = &lo.trigger {
            return if evaluate_expr_or_false(expr, ctx) {
                Some(expr.clone())
            } else {
                None
            };
        }
    }
    if let Some(rev) = workflow.review.as_ref() {
        if rev.enabled {
            if let Some(expr) = &rev.trigger {
                return if evaluate_expr_or_false(expr, ctx) {
                    Some(expr.clone())
                } else {
                    None
                };
            }
        }
    }
    None
}

fn evaluate_expr_or_false(expr: &str, ctx: &TriggerContext) -> bool {
    match trigger::parse(expr) {
        Ok(ast) => trigger::evaluate_ast(&ast, ctx),
        Err(e) => {
            tracing::warn!(
                trigger = %expr,
                error = %e,
                "review-gate trigger failed to parse; treating as no-fire"
            );
            false
        }
    }
}

/// Effective `retry_on_reject` budget for a layer: project review config's
/// value, raised by any layer-override (floor-merge invariant enforced at
/// workflow merge time — this is a simple max). Workflow merge guarantees
/// user overlays cannot LOWER project floors, so `max` here is safe.
pub fn effective_retry_on_reject(workflow: &WorkflowConfig, layer: &str) -> u32 {
    let project = workflow
        .review
        .as_ref()
        .map(|r| r.retry_on_reject)
        .unwrap_or(0);
    let per_layer = workflow
        .layer_overrides
        .get(layer)
        .and_then(|lo| lo.retry_on_reject)
        .unwrap_or(0);
    project.max(per_layer)
}

/// Result of checking whether gates should fire on a cohort boundary. Pure
/// so the daemon orchestrator can call this inside `run_stack_loops` under
/// the manifest lock without introducing async.
pub struct CohortGateCheck {
    /// Newly-fired gates to append to `manifest.gates`.
    pub new_gates: Vec<GateEntry>,
    /// Layer names whose status must transition `Passed → PendingReview`.
    pub layers_pending_review: Vec<String>,
}

impl CohortGateCheck {
    pub fn empty() -> Self {
        Self {
            new_gates: Vec::new(),
            layers_pending_review: Vec::new(),
        }
    }

    pub fn any(&self) -> bool {
        !self.new_gates.is_empty()
    }
}

/// Pure cohort-boundary gate check.
///
/// For each layer in `cohort_layers` whose CURRENT `manifest.layers` entry
/// has `status == Passed`:
/// 1. Build a `TriggerContext` from the layer's result + defaults.
/// 2. Resolve effective trigger via [`resolve_gate_trigger`].
/// 3. On fire, create a `GateEntry` reusing `reject_attempts_remaining`
///    from any prior gate for this layer (Codex Cycle-2 C6 rule). If no
///    prior gate exists, initialize from [`effective_retry_on_reject`].
///
/// The caller is responsible for:
/// - Transitioning `LayerStatus::Passed → PendingReview` on the named layers.
/// - Appending the new gates to `manifest.gates`.
/// - Writing the audit notification (reconciler wake).
/// - Recomputing `overall_status` via [`compute_overall_status`].
///
/// This keeps the function pure (no I/O, no locks, no mutation).
#[allow(clippy::too_many_arguments)]
pub fn check_gates_for_cohort(
    workflow: &WorkflowConfig,
    manifest_layers: &[crate::layers::manifest::LayerResult],
    prior_gates: &[GateEntry],
    cohort_layers: &[String],
    feature_id: &str,
    default_tier: u8,
    change_scope: &str,
    now: DateTime<Utc>,
) -> CohortGateCheck {
    let mut out = CohortGateCheck::empty();

    // Build a layer → most-recent-gate map once rather than scanning
    // prior_gates from the tail twice per cohort layer (for the
    // already-resolved skip AND the reject-counter persistence lookup).
    // Walk prior_gates tail-first and insert only the first hit per
    // layer; subsequent iterations of the same layer are older gates
    // and irrelevant.
    let mut most_recent: std::collections::BTreeMap<&str, &GateEntry> =
        std::collections::BTreeMap::new();
    for g in prior_gates.iter().rev() {
        most_recent.entry(g.layer.as_str()).or_insert(g);
    }

    for layer_name in cohort_layers {
        let layer_res = match manifest_layers.iter().find(|l| &l.name == layer_name) {
            Some(l) => l,
            None => continue,
        };
        if layer_res.status != LayerStatus::Passed {
            continue;
        }
        // Resume-safety: don't re-fire a gate for a layer whose most recent
        // prior gate is already resolved positively (Approved/Skipped) or
        // currently open (Pending). A Rejected prior gate is the retry path
        // — the layer was re-evaluated after the reviewer rejected, so we
        // DO fire a new gate (with the reject counter carried forward via
        // `reject_budget` below per Codex C6). TimedOut is treated the same
        // as Rejected (timeout-reject semantic).
        let prior = most_recent.get(layer_name.as_str()).copied();
        let most_recent_gate_status = prior.map(|g| g.status.clone());
        if matches!(
            most_recent_gate_status,
            Some(GateStatus::Pending) | Some(GateStatus::Approved) | Some(GateStatus::Skipped)
        ) {
            continue;
        }
        let tier = workflow
            .layer_overrides
            .get(layer_name)
            .and_then(|lo| lo.tier)
            .unwrap_or(default_tier);
        let ctx = trigger_context_for_layer(
            tier,
            layer_name,
            layer_res.final_confidence.unwrap_or(0.0),
            layer_res.total_cost_usd.unwrap_or(0.0),
            layer_res.passes.len() as u32,
            change_scope,
        );
        let Some(trigger_expr) = resolve_gate_trigger(workflow, layer_name, &ctx) else {
            continue;
        };
        // Reject-counter persistence across re-gates (Codex C6) —
        // sourced from the same cached `most_recent` lookup.
        let reject_budget = prior
            .map(|g| g.reject_attempts_remaining)
            .unwrap_or_else(|| effective_retry_on_reject(workflow, layer_name));
        let timeout_hours = workflow
            .review
            .as_ref()
            .map(|r| r.timeout_hours)
            .unwrap_or(24);
        let on_timeout = workflow
            .review
            .as_ref()
            .map(|r| r.on_timeout)
            .unwrap_or(OnTimeout::Reject);
        let timeout_at = now + ChronoDuration::hours(timeout_hours as i64);
        let gate = GateEntry {
            id: new_gate_id(feature_id, layer_name, now),
            layer: layer_name.clone(),
            status: GateStatus::Pending,
            trigger_expression: trigger_expr,
            requested_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            timeout_at: timeout_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            on_timeout_action: on_timeout,
            reject_attempts_remaining: reject_budget,
            decision: None,
            decided_at: None,
        };
        out.new_gates.push(gate);
        out.layers_pending_review.push(layer_name.clone());
    }
    out
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn fixed(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn pending_gate(timeout_at: &str) -> GateEntry {
        GateEntry {
            id: "feat:layer:0000".to_string(),
            layer: "infrastructure".to_string(),
            status: GateStatus::Pending,
            trigger_expression: "layer == infrastructure".to_string(),
            requested_at: "2026-04-20T00:00:00Z".to_string(),
            timeout_at: timeout_at.to_string(),
            on_timeout_action: OnTimeout::Reject,
            reject_attempts_remaining: 1,
            decision: None,
            decided_at: None,
        }
    }

    // ── new_gate_id ────────────────────────────────────────────────────

    #[test]
    fn new_gate_id_contains_feature_and_layer() {
        let now = fixed("2026-04-20T00:00:00Z");
        let id = new_gate_id("feat-x", "infrastructure", now);
        assert!(id.starts_with("feat-x:infrastructure:"));
    }

    #[test]
    fn new_gate_id_uniqueness_under_concurrent_calls() {
        // Spawn many threads; all must produce distinct IDs. The atomic
        // counter guarantees this even when timestamps collide.
        use std::sync::Arc;
        use std::thread;
        let now = fixed("2026-04-20T00:00:00Z");
        let seen = Arc::new(std::sync::Mutex::new(HashSet::<String>::new()));
        let mut handles = Vec::new();
        for _ in 0..16 {
            let seen = Arc::clone(&seen);
            handles.push(thread::spawn(move || {
                for _ in 0..128 {
                    let id = new_gate_id("feat", "layer", now);
                    let inserted = seen.lock().unwrap().insert(id.clone());
                    assert!(inserted, "duplicate gate id generated: {id}");
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(seen.lock().unwrap().len(), 16 * 128);
    }

    // ── audit_decision_string ──────────────────────────────────────────

    #[test]
    fn audit_decision_string_table_covers_every_combination() {
        // 3 decisions × 2 origins = 6 rows; the SQLite CHECK constraint
        // permits exactly this set of strings.
        let cases: &[(GateDecision, GateDecisionOrigin, &str)] = &[
            (GateDecision::Approve, GateDecisionOrigin::Manual, "approve"),
            (GateDecision::Reject, GateDecisionOrigin::Manual, "reject"),
            (GateDecision::Skip, GateDecisionOrigin::Manual, "skip"),
            (
                GateDecision::Approve,
                GateDecisionOrigin::Timeout,
                "timeout_approve",
            ),
            (
                GateDecision::Reject,
                GateDecisionOrigin::Timeout,
                "timeout_reject",
            ),
            (
                GateDecision::Skip,
                GateDecisionOrigin::Timeout,
                "timeout_skip",
            ),
        ];
        for (decision, origin, expected) in cases {
            let outcome = GateDecisionOutcome {
                decision: *decision,
                origin: *origin,
            };
            assert_eq!(
                outcome.audit_decision_string(),
                *expected,
                "decision={decision:?} origin={origin:?}"
            );
        }
    }

    #[test]
    fn manual_constructor_labels_origin_manual() {
        let o = GateDecisionOutcome::manual(GateDecision::Approve);
        assert_eq!(o.origin, GateDecisionOrigin::Manual);
        assert_eq!(o.audit_decision_string(), "approve");
    }

    #[test]
    fn timeout_constructor_projects_on_timeout_onto_decision() {
        assert_eq!(
            GateDecisionOutcome::timeout(OnTimeout::Reject).decision,
            GateDecision::Reject
        );
        assert_eq!(
            GateDecisionOutcome::timeout(OnTimeout::Approve).decision,
            GateDecision::Approve
        );
        assert_eq!(
            GateDecisionOutcome::timeout(OnTimeout::Skip).decision,
            GateDecision::Skip
        );
    }

    // ── resolve_timeout_action ────────────────────────────────────────

    #[test]
    fn resolve_timeout_action_fires_when_pending_and_past_deadline() {
        let requested = fixed("2026-04-20T00:00:00Z");
        let now = fixed("2026-04-20T06:00:00Z");
        let got = resolve_timeout_action(GateStatus::Pending, requested, 3, OnTimeout::Reject, now);
        assert!(got.is_some());
        assert_eq!(got.unwrap().audit_decision_string(), "timeout_reject");
    }

    #[test]
    fn resolve_timeout_action_returns_none_when_status_not_pending() {
        // Criterion 9 explicitly names this test — the status guard is
        // load-bearing for the decide/reap race.
        let requested = fixed("2026-04-20T00:00:00Z");
        let now = fixed("2030-01-01T00:00:00Z"); // way past
        for non_pending in [
            GateStatus::Approved,
            GateStatus::Rejected,
            GateStatus::Skipped,
            GateStatus::TimedOut,
        ] {
            let status_dbg = format!("{non_pending:?}");
            assert_eq!(
                resolve_timeout_action(non_pending, requested, 1, OnTimeout::Reject, now),
                None,
                "status {status_dbg} must short-circuit timeout resolution"
            );
        }
    }

    #[test]
    fn resolve_timeout_action_each_on_timeout_branch() {
        let requested = fixed("2026-04-20T00:00:00Z");
        let now = fixed("2026-04-20T02:00:00Z");
        let r = resolve_timeout_action(GateStatus::Pending, requested, 1, OnTimeout::Reject, now)
            .unwrap();
        assert_eq!(r.audit_decision_string(), "timeout_reject");
        let a = resolve_timeout_action(GateStatus::Pending, requested, 1, OnTimeout::Approve, now)
            .unwrap();
        assert_eq!(a.audit_decision_string(), "timeout_approve");
        let s = resolve_timeout_action(GateStatus::Pending, requested, 1, OnTimeout::Skip, now)
            .unwrap();
        assert_eq!(s.audit_decision_string(), "timeout_skip");
    }

    #[test]
    fn resolve_timeout_action_returns_none_before_deadline() {
        let requested = fixed("2026-04-20T00:00:00Z");
        let now = fixed("2026-04-20T00:30:00Z"); // 30m elapsed, deadline is 1h
        assert_eq!(
            resolve_timeout_action(GateStatus::Pending, requested, 1, OnTimeout::Reject, now),
            None
        );
    }

    // ── apply_timeout_if_expired ──────────────────────────────────────

    #[test]
    fn apply_timeout_if_expired_mutates_expired_pending_gate() {
        let mut gate = pending_gate("2026-04-20T01:00:00Z");
        let now = fixed("2026-04-20T02:00:00Z");
        let outcome = apply_timeout_if_expired(&mut gate, OnTimeout::Reject, now).unwrap();
        assert_eq!(outcome.audit_decision_string(), "timeout_reject");
        assert_eq!(gate.status, GateStatus::Rejected);
        assert_eq!(gate.decision.as_deref(), Some("timeout_reject"));
        assert_eq!(
            gate.decided_at.as_deref(),
            Some("2026-04-20T02:00:00+00:00")
        );
    }

    #[test]
    fn apply_timeout_if_expired_returns_none_if_not_expired() {
        let mut gate = pending_gate("2026-04-21T00:00:00Z");
        let now = fixed("2026-04-20T02:00:00Z");
        assert_eq!(
            apply_timeout_if_expired(&mut gate, OnTimeout::Reject, now),
            None
        );
        assert_eq!(gate.status, GateStatus::Pending, "gate must not be mutated");
        assert!(gate.decision.is_none());
    }

    #[test]
    fn apply_timeout_if_expired_returns_none_if_already_decided() {
        // Decide/reap race: the reconciler wakes up, but a manual
        // decision already transitioned the gate to Approved. The
        // inline status check must prevent a second mutation.
        let mut gate = pending_gate("2026-04-20T01:00:00Z");
        gate.status = GateStatus::Approved;
        gate.decision = Some("approve".to_string());
        let now = fixed("2030-01-01T00:00:00Z");
        assert_eq!(
            apply_timeout_if_expired(&mut gate, OnTimeout::Reject, now),
            None
        );
        assert_eq!(
            gate.status,
            GateStatus::Approved,
            "a concurrently-decided gate must not be overwritten by timeout"
        );
        assert_eq!(gate.decision.as_deref(), Some("approve"));
    }

    #[test]
    fn apply_timeout_skip_branch_preserves_skip_semantics() {
        // Skip is semantically distinct from approve/reject — the layer
        // status upstream stays Passed, the gate status becomes Skipped.
        // `apply_timeout_if_expired` mutates only the gate; upstream
        // layer-status logic lives in the handler.
        let mut gate = pending_gate("2026-04-20T01:00:00Z");
        let now = fixed("2026-04-20T02:00:00Z");
        let outcome = apply_timeout_if_expired(&mut gate, OnTimeout::Skip, now).unwrap();
        assert_eq!(outcome.audit_decision_string(), "timeout_skip");
        assert_eq!(gate.status, GateStatus::Skipped);
    }

    // ── trigger_context_for_layer ─────────────────────────────────────

    #[test]
    fn trigger_context_for_layer_populates_all_fields() {
        let ctx = trigger_context_for_layer(3, "infrastructure", 0.95, 0.12, 4, "database_schema");
        assert_eq!(ctx.tier, 3);
        assert_eq!(ctx.layer, "infrastructure");
        assert_eq!(ctx.confidence, 0.95);
        assert_eq!(ctx.cost, 0.12);
        assert_eq!(ctx.passes, 4);
        assert_eq!(ctx.change_scope, "database_schema");
    }

    // ── check_gates_for_cohort ────────────────────────────────────────
    //
    // Contract criteria 1 and 7 name these three tests. Each exercises a
    // distinct branch of the pure gate-firing logic.

    use crate::layers::manifest::{LayerResult, LayerStatus};
    use crate::workflow::schema::{Defaults, Phases, ReviewConfig};
    use std::collections::BTreeMap;

    fn passed_layer(name: &str) -> LayerResult {
        LayerResult {
            name: name.to_string(),
            status: LayerStatus::Passed,
            passes: Vec::new(),
            seam_checks: Vec::new(),
            halted_by: None,
            final_confidence: Some(0.95),
            total_cost_usd: Some(0.01),
            escalation_events: None,
        }
    }

    fn workflow_with_review(trigger: &str, retry_on_reject: u32) -> WorkflowConfig {
        WorkflowConfig {
            schema_version: "0.2".to_string(),
            defaults: Defaults {
                tier: 2,
                min_confidence: 0.9,
                max_passes: 5,
                model: "sonnet".to_string(),
                budget_usd: 0.0,
                cost_cap_behavior: crate::workflow::schema::CostCapBehavior::Halt,
                max_parallelism: None,
            },
            phases: Phases::default(),
            layer_overrides: BTreeMap::new(),
            review: Some(ReviewConfig {
                enabled: true,
                trigger: Some(trigger.to_string()),
                timeout_hours: 24,
                on_timeout: OnTimeout::Reject,
                notification: "stdout".to_string(),
                retry_on_reject,
            }),
            seams: None,
        }
    }

    #[test]
    fn check_gates_for_cohort_with_matching_trigger_enqueues_gate_with_pinned_fields() {
        // Contract criterion 1: trigger fires, timeout + retry are PINNED
        // at request time (not lazily read from workflow).
        let workflow = workflow_with_review("layer == infrastructure", 1);
        let layers = vec![passed_layer("infrastructure")];
        let now = fixed("2026-04-20T00:00:00Z");
        let out = check_gates_for_cohort(
            &workflow,
            &layers,
            &[],
            &["infrastructure".to_string()],
            "feat-6",
            2,
            "",
            now,
        );
        assert_eq!(out.new_gates.len(), 1);
        assert_eq!(out.layers_pending_review, vec!["infrastructure"]);
        let g = &out.new_gates[0];
        assert_eq!(g.layer, "infrastructure");
        assert_eq!(g.status, GateStatus::Pending);
        assert_eq!(g.trigger_expression, "layer == infrastructure");
        assert_eq!(g.reject_attempts_remaining, 1);
        assert_eq!(g.on_timeout_action, OnTimeout::Reject);
        // Pinned timeout_at: now + 24h.
        let expected_timeout =
            (now + ChronoDuration::hours(24)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        assert_eq!(g.timeout_at, expected_timeout);
    }

    #[test]
    fn check_gates_for_cohort_reuses_reject_counter_from_prior_gate() {
        // Contract criterion 7: the reject budget persists across re-gate
        // events. A prior gate with counter=0 must NOT reset on re-fire.
        let workflow = workflow_with_review("layer == infrastructure", 2);
        let layers = vec![passed_layer("infrastructure")];
        let prior = GateEntry {
            id: "feat-6:infrastructure:aaaa".to_string(),
            layer: "infrastructure".to_string(),
            status: GateStatus::Rejected,
            trigger_expression: "layer == infrastructure".to_string(),
            requested_at: "2026-04-20T00:00:00Z".to_string(),
            timeout_at: "2026-04-21T00:00:00Z".to_string(),
            on_timeout_action: OnTimeout::Reject,
            reject_attempts_remaining: 0, // already exhausted
            decision: Some("reject".to_string()),
            decided_at: Some("2026-04-20T00:05:00Z".to_string()),
        };
        let now = fixed("2026-04-20T00:10:00Z");
        let out = check_gates_for_cohort(
            &workflow,
            &layers,
            &[prior],
            &["infrastructure".to_string()],
            "feat-6",
            2,
            "",
            now,
        );
        assert_eq!(out.new_gates.len(), 1);
        assert_eq!(
            out.new_gates[0].reject_attempts_remaining, 0,
            "counter must persist across re-gates (not reset to 2)"
        );
    }

    #[test]
    fn require_review_override_forces_gate_regardless_of_trigger_expression() {
        // Per-layer override: `require_review: true` wins over the review
        // block's trigger. Even if the global trigger evaluates false for
        // this layer, the layer-override forces a gate.
        let mut workflow = workflow_with_review("layer == deployment", 1);
        let lo = crate::workflow::schema::LayerOverride {
            require_review: Some(true),
            ..Default::default()
        };
        workflow.layer_overrides.insert("api".to_string(), lo);
        let layers = vec![passed_layer("api")];
        let now = fixed("2026-04-20T00:00:00Z");
        let out = check_gates_for_cohort(
            &workflow,
            &layers,
            &[],
            &["api".to_string()],
            "feat-6",
            2,
            "",
            now,
        );
        assert_eq!(out.new_gates.len(), 1);
        assert_eq!(out.new_gates[0].layer, "api");
        assert_eq!(out.new_gates[0].trigger_expression, "true");
    }

    #[test]
    fn check_gates_for_cohort_skips_non_passed_layers() {
        // A layer whose status is Failed / Pending / InProgress does not
        // fire a gate — the plan only gates on clean Passed transitions.
        let workflow = workflow_with_review("layer == infrastructure", 1);
        let mut layer = passed_layer("infrastructure");
        layer.status = LayerStatus::Failed;
        let layers = vec![layer];
        let now = fixed("2026-04-20T00:00:00Z");
        let out = check_gates_for_cohort(
            &workflow,
            &layers,
            &[],
            &["infrastructure".to_string()],
            "feat-6",
            2,
            "",
            now,
        );
        assert!(out.new_gates.is_empty());
    }
}

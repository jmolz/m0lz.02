---
paths:
  - "crates/pice-core/src/workflow/**"
  - "crates/pice-core/src/adaptive/**"
  - "crates/pice-daemon/src/gate/**"
  - "crates/pice-cli/src/commands/validate.rs"
  - "crates/pice-cli/src/commands/review_gate.rs"
  - "templates/pice/workflow.yaml"
---

# Workflow YAML, Adaptive Evaluation, and Review Gates Rules (v0.2+)

See `PRDv2.md` → Features 4, 7, 9 and `docs/research/convergence-analysis.md` for the grounding. This file captures the invariants.

## `.pice/workflow.yaml` — the committable pipeline

The workflow file codifies which tiers run, when parallelism applies, where review gates fire, how retries work, and what budgets apply. Teams commit it; everyone runs the same pipeline.

### Schema basics

- `schema_version: "0.2"` is required. Daemon refuses to load incompatible versions with a clear upgrade message.
- Top-level sections: `defaults`, `phases`, `layer_overrides`, `review`, `seams` (optional)
- Parse with `serde_yaml` in `pice-core::workflow`
- Validation lives in `pice-core::workflow::validate` — both CLI (`pice validate`) and daemon (at execution time) use the same validator. Never duplicate.

### Inheritance (three levels)

1. **Framework defaults** — embedded in the binary via `include_str!`, baseline for every phase
2. **Project workflow** — `.pice/workflow.yaml`, committed to the repo, team-wide
3. **User workflow** — `~/.pice/workflow.yaml`, NOT committed, personal overrides

Resolution order: framework → project → user. Each level is merged into the previous.

### Floor-based merge semantics (HARD RULE)

User overrides can only **restrict** project defaults. They can NEVER relax them.

| User can... | User cannot... |
|-------------|----------------|
| Raise `min_confidence` | Lower `min_confidence` below project floor |
| Lower `budget_usd` | Raise `budget_usd` above project ceiling |
| Raise `tier` (2 → 3) | Lower `tier` (3 → 2) |
| Enable `require_review` that was false | Disable `require_review` that was true |
| Add a gate trigger | Remove a gate trigger required by the project |

Violations are reported at workflow load time with a clear error showing the specific field and the project floor. This prevents an individual dev from locally bypassing team-wide guardrails.

The validator implementation lives in `pice-core::workflow::merge`. Every merge operation passes through a `check_floor()` guard.

### Floor merge implementation invariants

These are learned from Phase 2 adversarial review — each one closed a concrete bypass.

- **Snapshot project state BEFORE mutation.** `merge_with_floor` clones `base.defaults`, `base.review`, and `base.layer_overrides` at entry. Per-layer floors derive from those snapshots, not from `out.*` mid-mutation — otherwise a user who raises defaults can then be checked against their own raised value in the layer step. The snapshot pattern also makes intent unambiguous: the floor is what the PROJECT said, not the partially-merged result.
- **Layer floors fall back to defaults, not `LayerOverride::default()`.** When the project has no explicit `layer_overrides.<layer>.tier`, the floor for user-overridden `tier` on that layer is `project_defaults.tier`, not `0` (from a fresh `LayerOverride`). Same pattern applies to `min_confidence`, `max_passes`, `budget_usd`, `require_review`. A fresh per-layer user override is still floor-checked.
- **`require_review` per-layer floor is `project_layer.require_review.unwrap_or(project_review.enabled)`.** Do NOT OR the global gate onto the layer floor — that would prevent a project-committed `require_review: false` exemption from being carried forward when the user also leaves it `false`.
- **Trigger floors use byte-equality FIRST, then AST equivalence.** `triggers_equivalent(project, user)` returns true iff `project == user` OR `trigger::parse(project) == trigger::parse(user)`. The byte-equality fast path is load-bearing: if the user restates the same (possibly invalid) trigger text, we must NOT collapse that into a "rewrite" violation — the real parse error is separately surfaced by `validate_triggers` on the resolved config. Without byte-equality, identical-but-invalid triggers would be masked by a misleading floor violation.
- **Trigger equivalence is structural, not logical.** AST equality tolerates `always` ↔ `true` aliasing and whitespace differences but rejects `always` vs `tier >= 3` even when the user trigger is semantically stricter. Full AST-implication checking is deferred to v0.3 — see `triggers_equivalent` docs for the upgrade path (truth-table enumeration over the finite context domain).
- **Collect ALL violations before returning.** Never short-circuit on the first floor violation. Adversarial bypass tests explicitly relax every floored field at once and assert each violation surfaces.

### Trigger expression grammar

Used by `review.trigger` and conditional layer overrides. Grammar:

```
expression := term ( ('AND' | 'OR') term )*
term       := 'NOT'? primary
primary    := comparison | grouped | literal
comparison := identifier ( '==' | '>=' | '<=' | '>' | '<' | '!=' ) value
grouped    := '(' expression ')'
literal    := 'true' | 'false' | 'always'
identifier := 'tier' | 'layer' | 'confidence' | 'cost' | 'passes' | 'change_scope'
value      := integer | float | string
```

Examples:
- `tier >= 3`
- `layer == infrastructure OR layer == deployment`
- `confidence < 0.95 AND tier >= 2`
- `NOT (change_scope == css_only)`

Parser lives in `pice-core::workflow::trigger`. Parsing errors surface with line + column. Test every grammar production with a failing fixture.

The recursive-descent parser enforces a `MAX_PARSE_DEPTH = 128` guard in `parse_not` and `parse_primary`'s `LParen` branch. Untrusted YAML input must not be able to stack-overflow the daemon — deeply nested `(((…)))` or `NOT NOT NOT …` must return a parse error, not crash. When adding new recursive productions, increment/decrement the same `depth` field on the parser struct.

### Validation

`pice validate` checks:
- YAML schema compliance
- `schema_version` matches supported version
- Trigger expression parses successfully
- `layer_overrides` keys all exist in `layers.toml`
- Model names valid for the configured provider (queries provider capabilities)
- Floor violations in user overrides
- `seam_checks` references exist in the seam check registry

Invalid workflows block evaluation with specific errors — no silent defaults.

### Schema hardening and cross-reference invariants

- **Every workflow struct MUST carry `#[serde(deny_unknown_fields)]`.** Renamed or removed fields (e.g. the ex-`phases.review` block) must fail parsing, not be silently dropped. `WorkflowConfig`, `Defaults`, `Phases`, `PhaseConfig`, `ExecutePhase`, `RetryConfig`, `EvaluatePhase`, `LayerOverride`, `ReviewConfig` all carry the attribute — any new struct in `pice-core::workflow::schema` must too.
- **Cross-reference uses `order ∩ defs`, not just `defs`.** Runtime only executes layers that appear in both `layers.order` AND `layers.defs`. `validate_all` must catch BOTH `order`-only and `defs`-only ghost layers — the intersection is the "known layers" set. A layer in only one side is a config bug and must surface as a cross-reference error, not a warning.
- **`pice validate --json` on failure returns `CommandResponse::ExitJson { code: 1, value }` — never `Exit { message: <stringified json> }`.** See `.claude/rules/daemon.md` → "Structured JSON failure responses" for the rationale. The `evaluate` handler fails closed on the same validation errors at execution time (mirrors `validate_all` against the resolved workflow before spawning Stack Loops).
- **Contract criterion #6 requires integration tests via the real `pice` binary.** `crates/pice-cli/tests/validate_integration.rs` exercises the adapter stack with `assert_cmd` + `PICE_DAEMON_INLINE=1`. Unit tests in the daemon handler are necessary but not sufficient — CLI-layer routing (stdout vs stderr, exit code propagation, JSON shape) must be covered end-to-end.

## Adaptive Evaluation — the ~96.6% ceiling

**HARD RULE**: Confidence for dual-model correlated evaluators never exceeds ~96.6%.

Grounding: the correlated Condorcet Jury Theorem with `ρ ≈ 0.35` between Claude and Codex (Kim et al., ICML 2025). Full derivation in `docs/research/convergence-analysis.md`.

| Passes | Effective N | Estimated confidence |
|--------|-------------|----------------------|
| 1      | 1.00        | 88.0% |
| 3      | 1.87        | 94.0% |
| 5      | 2.27        | 95.4% |
| 10     | 2.63        | 96.2% |
| ∞      | 2.86        | ~96.6% |

Passes 1→3 capture 70% of total achievable improvement. Passes 1→5 capture 86%. Beyond 5, marginal gain < 0.5% per pass. Adaptive algorithms honor this — they halt, they do not grind past the ceiling.

### Three algorithms (all pure functions in `pice-core::adaptive`)

#### Bayesian-SPRT (default)

Sequential Probability Ratio Test with Bayesian posterior updates over `Beta(α, β)` for "contract is met":

```
For each pass n:
  observe pass_result, update posterior
  likelihood_ratio = P(obs | H1) / P(obs | H0)
  if LR > A → halt, PASS
  if LR < B → halt, FAIL
  if n >= max_passes → halt, return current posterior
```

`A` and `B` are derived from `min_confidence` in workflow config. Default: `A = 19.0` (≈95% target), `B = 1/19 ≈ 0.053`.

#### ADTS (Adversarial Divergence-Triggered Scaling)

When Claude and Codex diverge beyond threshold, escalate:

```
divergence = |claude_score - codex_score|
if divergence > divergence_threshold (default 2.0 on 0–10 scale):
  → extra pass with fresh context
  → if still divergent: escalate tier
  → if still divergent at max tier: request human review gate
```

Catches "both models are confident but disagree" — a strong signal that the contract is underdetermined or there's a subtle issue one model missed.

#### VEC (Verification Entropy Convergence)

Track per-pass entropy reduction of the posterior. Halt when marginal entropy drops below a floor:

```
H_n = entropy(posterior after n passes)
ΔH = H_{n-1} - H_n
if ΔH < entropy_floor (default 0.01 bits) → halt
```

Complements SPRT when the posterior is neither strongly accepted nor rejected. Stops the loop when additional passes no longer reduce uncertainty.

### Cost budget enforcement

- Every pass writes `cost_usd` to `cost_events` (see `.claude/rules/metrics.md`)
- Before spawning pass N+1, the adaptive controller checks if adding the projected cost would exceed `workflow.defaults.budget_usd` (or layer-specific override)
- If yes, halt with `halted_by: budget` regardless of confidence state
- The `halted_by` field must be one of: `sprt_confidence_reached` | `sprt_rejected` | `budget` | `max_passes` | `vec_entropy` | `gate_rejected` | `adts_escalation_exhausted`

### Testing

- Calibration test: run adaptive SPRT on synthetic evaluators with known `p` and `ρ`. Verify reported confidence tracks the correlated Condorcet prediction within ±2%.
- Ceiling test: run 100 passes on a contrived scenario. Assert reported confidence never exceeds 96.7%.
- Budget test: set tight budget, verify halt fires before confidence target.
- ADTS divergence test: construct divergent scores, verify escalation behavior.

## Review Gates

Gates are configured in `workflow.yaml` under the `review` phase and `layer_overrides.{layer}.require_review`:

```yaml
review:
  enabled: true
  trigger: "tier >= 3 OR layer == infrastructure OR confidence < 0.95"
  timeout_hours: 24
  on_timeout: reject        # reject | approve | skip
  notification: stdout      # stdout | slack (v0.3+) | webhook (v0.3+)
```

### Gate state machine

```
[running] → [gate_requested] → [decision_recorded] → [running | failed | retrying]
```

- `gate_requested`: written to manifest, event fired to subscribers, paused evaluation
- Foreground mode: CLI prompts immediately with approve/reject/details/skip options
- Background mode: manifest remains in `pending_review` state; user actions via `pice review-gate {feature} --layer {layer}`
- Timeout behavior is configurable and enforced by the daemon's timer subsystem

### Foreground gate prompt shape

Uses Unicode box drawing characters (same style as existing evaluation reports in `crates/pice-cli/src/engine/output.rs`):

```
╔═══════════════════════════════════════════════════════════╗
║  REVIEW GATE: Infrastructure layer                        ║
╠═══════════════════════════════════════════════════════════╣
║  Evaluation: PASS at 95.1% confidence (4 passes, $0.12)  ║
║  Seam checks: 3/3 verified                                ║
║  [a]pprove  [r]eject  [d]etails  [s]kip                  ║
╚═══════════════════════════════════════════════════════════╝
```

### Audit trail (mandatory)

Every gate decision writes a row to `gate_decisions` SQLite table. See `.claude/rules/metrics.md` for schema. Fields: `feature_id`, `layer`, `trigger_expression`, `decision`, `reviewer`, `reason`, `requested_at`, `decided_at`, `elapsed_seconds`. Decisions are INSERTs (never UPDATEs) — a changed decision creates a new row.

### Trigger expression reuse

Gates use the same trigger grammar as workflow conditional overrides. Parser in `pice-core::workflow::trigger` is shared. Never reinvent the grammar.

### Skip semantics

- `skip` bypasses the gate with a warning logged to the audit trail
- A skipped gate is distinct from a timeout_skip — the former is an explicit user decision, the latter is a configured timeout fallback
- Both record to audit trail with the specific decision value

### Background gate deferral invariants

- When a gate fires during a background evaluation, the manifest status transitions to `pending_review`
- The evaluation is fully paused — no further passes run, no parallel cohorts advance past the gated layer
- The state survives daemon restart
- Multiple gates from different features can be pending simultaneously
- `pice review-gate --list` enumerates all pending gates across features

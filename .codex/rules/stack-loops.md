---
paths:
  - "crates/pice-core/src/layers/**"
  - "crates/pice-core/src/seam/**"
  - "crates/pice-daemon/src/orchestrator/**"
  - "crates/pice-daemon/src/worktree/**"
  - "templates/pice/layers.toml"
  - "templates/pice/contracts/**"
---

# Stack Loops Rules (v0.2+)

See `PRDv2.md` → Features 3, 5, 6, 8 and `docs/research/seam-blindspot.md` for the empirical basis. This file captures the invariants and patterns.

## What Stack Loops means

A feature is PASS only when **every layer passes**. Instead of one PICE loop per feature, the orchestrator runs nested per-layer loops. Infrastructure, deployment, and observability layers never get skipped.

## Layer detection — six-level heuristic stack

Run in order; later levels override earlier:

1. **Manifest files** — `package.json`, `pyproject.toml`, `Cargo.toml`, `go.mod`, `Gemfile`
2. **Directory patterns** — `app/`, `api/`, `infra/`, `deploy/`, `src/server/`, `src/client/`, `terraform/`, `helm/`
3. **Framework signals** — Next.js `app/` = frontend + API, Prisma schema = database, FastAPI = API + backend
4. **Config files** — `Dockerfile`, `docker-compose.yml`, `terraform/`, `.github/workflows/`
5. **Import graph** — static analysis to classify architectural clusters
6. **Override file** — `.pice/layers.toml` always wins

**File-level layer tagging** is non-negotiable for fullstack-in-one frameworks. A single `pages/api/users.ts` can belong to `api`, `frontend`, AND `database` layers simultaneously. Each layer's contract applies its own evaluation lens to the same file.

## `.pice/layers.toml` invariants

- `[layers]` defines the ordered list; `order = [...]` is the authoritative sequence
- Each `[layers.NAME]` has `paths`, optional `always_run`, optional `depends_on`, optional `contract`, optional `type = "meta"`
- `always_run = true` layers execute regardless of change scope unless explicitly overridden in `workflow.yaml` with audit trail entry
- `depends_on` forms a DAG; cycles are rejected with a clear error
- `type = "meta"` marks IaC layers (Terraform/Pulumi/CDK) that create other layers — these use provisioning-seam verification, not runtime-seam verification
- `environment_variants = ["staging", "production"]` on the deployment layer triggers environment-specific contract property evaluation
- Polyrepo external contracts go in `[external_contracts]`, deferred to v0.4 for real inference

## Dependency cascade (transitive closure)

Layer activation uses **transitive** dependency cascade: if database changes activate `api` (depends_on database), then `frontend` (depends_on api) also activates. This catches downstream breakage — exactly the class of failures Stack Loops was built to detect.

Layers activated by cascade but with no file changes of their own get `Skipped` status (they have no diff to evaluate), EXCEPT `always_run` layers which get `Pending` status (they must never be marked Skipped — seam checks / static analysis will evaluate them in Phase 3).

## DAG construction and parallel cohorts

The orchestrator builds a topological DAG from `layers.toml` + plan-declared dependencies, then groups layers into **cohorts** where every layer in a cohort has no pending dependencies. Cohorts execute sequentially; layers within a cohort execute in parallel via git worktree isolation.

- Max parallelism is configurable via `workflow.defaults.max_parallelism` (default = CPU count)
- Dependency edges always win — a layer with upstream in the current cohort never starts early
- `always_run` layers are evaluated even if their upstreams fail, unless the user configured `halt_on_upstream_failure = true`

## Fail-closed evaluation

Layers are **never** marked as PASSED without real provider-backed evaluation. Phase 1 records `LayerStatus::Pending` with `model: "phase-1-pending"` and `score: None`. The manifest overall status is `InProgress`, not `Passed`, until Phase 2 wires real provider scoring. This prevents false confidence.

## Manifest persistence

- Manifest paths are **namespaced by project hash**: `~/.pice/state/{project_hash_12chars}/{feature_id}.manifest.json`. This prevents cross-repo collisions when different projects use the same plan filename.
- Manifests are persisted **incrementally**: initial checkpoint before the evaluation loop, per-layer checkpoint after each result, and final checkpoint after overall status computation.
- `save()` uses **crash-safe atomic writes**: fsync temp file → atomic rename → fsync parent directory. After `save()` returns, the checkpoint survives power loss.

## Context isolation (HARD RULE)

Each layer's evaluator sees ONLY:
- The layer's contract (`.pice/contracts/{layer}.toml`)
- The git diff filtered to the layer's tagged files
- `AGENTS.md` (project-level, unchanged)

It does NOT see:
- Other layers' contracts
- Other layers' diffs
- Other layers' findings
- The cross-layer plan rationale
- Previous evaluation pass findings for the same layer (use `fresh_context: true` for retries)

Violating this rule recreates the self-grading bias PICE was built to eliminate. Test harness must verify this — grep the provider's session prompt for known contents from other layers and assert they are absent.

## Worktree isolation

- Create one worktree per parallel layer: `.pice/worktrees/{feature-id}/{layer}/`
- Use `git worktree add` from the Rust daemon via `git2-rs`
- Pass the worktree path as `workingDirectory` in `session/create`
- Creation overhead: target < 300ms
- On layer PASS: merge worktree back to main worktree using the configured `merge_strategy` (default: `apply_to_main` — working directory changes, user commits)
- On layer FAIL with retries remaining: reset worktree, retry with fresh provider context
- On layer FAIL without retries: if `preserve_failed_worktrees = true`, keep worktree and record path in manifest; else remove
- On merge conflict: halt the cohort with clear error naming the conflicting paths

**Evaluation-only mode** (`pice evaluate` without `pice execute`): subagents are read-only (`tools: [Read, Grep, Glob]`). Read-only agents cannot conflict, so worktrees are NOT created by default. Use `--isolate` to force worktree creation (e.g., for experimental seam checks that write to the filesystem).

## Seam verification

After each layer's contract grading, run seam checks for that layer's boundaries. Seam checks target the 12 empirically validated failure categories:

1. Configuration/deployment mismatches (Google SRE: 31% of triggers)
2. Binary/version incompatibilities (Google SRE: 37% of triggers)
3. Protocol/API contract violations (Adyen: 60K+ daily errors)
4. Authentication handoff failures
5. Cascading failures from dependencies
6. Retry storm / timeout conflicts
7. Service discovery failures
8. Health check blind spots
9. Serialization / schema drift
10. Cold start / ordering dependencies
11. Network topology assumptions
12. Resource exhaustion at boundaries

### `SeamCheck` trait

```rust
pub trait SeamCheck {
    fn id(&self) -> &str;
    fn applies_to(&self, boundary: &LayerBoundary) -> bool;
    fn run(&self, ctx: &SeamContext) -> SeamResult;
}
```

- Default checks live in `pice-core::seam::defaults`
- Community check plugins ship as separate crates (e.g., `pice-seam-checks-grpc`)
- Daemon discovers plugins at startup via a registry pattern
- Per-boundary checks are declared in `.pice/layers.toml` under `[seams]`
- v0.2 ships 12 default checks, one per PRDv2 category. LLM-based seam reasoning is deferred to v0.4 implicit contract inference.
- Static checks must be **deterministic and fast** (< 100ms each) — run on every pass, not cached
- Seam findings are written to `seam_findings` SQLite table with `category` (1–12) labeled

### Phase 3 implementation invariants

These are codified in the v0.2 Phase 3 implementation and enforced by tests. Several were sharpened by the Phase 3 adversarial review — each one closed a concrete silent-bypass route.

- **`LayerBoundary::parse` canonicalizes alphabetically.** `"A↔B"` and `"B↔A"` compare equal via canonical form `{a≤b}↔{b≥a}`. Raw user spellings (including `<->`) are accepted; storage is always `↔`.
- **Boundary seam runs require AT LEAST ONE layer active.** (Phase 3 review fix — previously required BOTH active.) If both sides are inactive, nothing changed that could have introduced drift, so the check is skipped. If either side changed, the boundary runs — the "changed one side, forgot the other" case is exactly the failure mode seams exist to catch. Requires the companion `boundary_files` rule below.
- **`boundary_files` is the FULL per-layer file set, not just the changed diff.** (Phase 3 review fix — previously diff-only.) `stack_loops.rs` walks the repo under each seam-participating layer's globs via `pice_core::layers::filter::scan_files_by_globs` and unions the result with changed-file tagging. A seam check reading `ctx.repo_root.join(p)` for `p` in `ctx.boundary_files` thus sees stable counterpart artifacts (unchanged OpenAPI spec, unchanged Dockerfile) that drift-detection depends on.
- **Context isolation via `SeamContext`.** The struct has no `Debug` derive to prevent accidental `{:?}` leaks of other layers' data. The seam runner assembles `boundary_files = layer_paths[a] ∪ layer_paths[b]` before constructing the context.
- **Fail-closed rollup in `run_stack_loops`.** Any `SeamCheckResult.status == Failed` sets `LayerResult.status = Failed` with `halted_by = "seam:<check-id>"`. `Warning` findings preserve `Pending` (they are advisory, never block).
- **Runtime seam map is the floor-merged result of `layers.toml [seams]` + `workflow.yaml.seams`.** (Phase 3 review fix — previously the workflow map wholesale replaced the project map.) The `evaluate` handler calls `pice_core::workflow::merge::merge_seams` with `layers.toml [seams]` as the project floor, fails closed on any `FloorViolation`, then re-validates the merged map against the registry before invoking the orchestrator. The orchestrator does NOT re-merge; it consumes `StackLoopsConfig::merged_seams` as the execution-time source of truth.
- **Merge semantics are NOT floor-guarded on check-list content.** Users may swap `schema_drift` for `config_mismatch` — the floor is only on boundary *existence*. An empty check list for a project-declared boundary is a floor violation (equivalent to "silently turn off"); `merge_seams` in `pice-core::workflow::merge` rejects it.
- **`validate_seams` rejects checks whose `applies_to()` returns false for the configured boundary.** (Phase 3 review fix.) At runtime the seam runner silently `continue`s past inapplicable checks; without config-time validation, a user who writes `backend↔database = ["config_mismatch"]` would think they have coverage while the boundary quietly runs zero checks. The validator fetches each ID from the registry and rejects the config with a specific error pointing at the offending `{boundary, check, category}` tuple.
- **Heuristic checks (categories 5, 10, 11, and retry_storm cat 6) always emit `Warning`**, never `Failed`. They document this in their module docstring. Full runtime semantics are v0.4 scope.
- **`schema_drift` (cat 9) fail-closes on missing counterparts.** (Phase 3 second-round review fix — previously returned `Passed` when a model had no matching DDL table.) An ORM model with no matching migration table emits `Failed` ("migration is missing or the table was renamed"); the symmetric case — a migration table with no matching ORM model — also emits `Failed`. This closes the archetypal category-9 drift where someone adds/renames a model without updating the migration (or vice versa).
- **`openapi_compliance` (cat 3) warns on asymmetric parse results.** (Phase 3 second-round review fix — previously returned `Passed` when either side yielded zero parsed properties.) Seeing a spec file but no recognizable handler (or vice versa) emits `Warning`, not `Passed` — the boundary could not be compared, so silently reporting "clean" was a false negative. Seeing neither artifact on the boundary is still `Passed` (nothing to evaluate); seeing both but with zero-parse on one side emits `Warning` so operators know the parser could not recognize their shape.
- **Pre-orchestrator validation errors route through `ExitJson` under `--json`, with typed discriminants.** (Phase 3 second-round review fix; widened in third-round; typed in fourth-round.) Workflow validation failures, seam-floor violations, merged-seam validation failures, missing plan files, plan-parse failures, and missing-contract-section failures in `handlers/evaluate.rs` all emit a structured `ExitJson { code, value }` to stdout when `req.json` is true. The `value.status` field carries one of six **typed** variants from `pice_core::cli::ExitJsonStatus` (`PlanNotFound`, `PlanParseFailed`, `NoContractSection`, `WorkflowValidationFailed`, `SeamFloorViolation`, `MergedSeamValidationFailed`), serialized as kebab-case. Promoting from raw `json!` literals to a typed enum (round-4 fix) means a typo at the call site fails to compile. Tests use `ExitJsonStatus::X.as_str()` so a rename mechanically updates both sides. Text mode still uses `Exit { code, message }` to stderr. **All six paths have a CLI binary integration test** in `crates/pice-cli/tests/evaluate_integration.rs` (eight tests total: six discriminants + clean-fixture + failing-seam).
- **Bilateral-active boundaries dedupe at SQLite persistence.** (Phase 3 round-4 adversarial review fix.) When both layers of a boundary are active, `run_seams_for_layer` is called per-layer and attributes the same `SeamCheckResult` to both layers' `seam_checks` (the per-layer manifest copy is intentional — each layer's view is complete). The `evaluate` handler dedupes on `(boundary, check_id)` before calling `insert_seam_finding` so analytics reflect one row per `(eval_id, boundary, check_id)` tuple, not two. The first-encountered layer wins the `layer` column (deterministic across runs because manifest layer order is stable).
- **TS-side roundtrip covers result/finding shapes.** (Phase 3 round-4 adversarial review fix.) `@pice/provider-protocol` exports `SeamCheckResult`, `SeamFinding`, `SeamCheckStatus` mirroring the Rust types in `pice-core::layers::manifest`. `packages/provider-base/src/__tests__/roundtrip.test.ts` exercises wire-shape roundtrip for each — including the four `SeamCheckStatus` variants, optional-field omission, and null `category` for unregistered-check synthetic rows. Round-1 only mirrored `SeamCheckSpec`, leaving result/finding types without protocol-level coverage.
- **Boundary parsing uses `LayerBoundary::parse` end-to-end.** (Phase 3 third-round adversarial review fix — the previous `seam_boundary_references_known_layers` ad-hoc tokenizer split on `-`, exploding hyphenated layer names like `auth-service↔api-gateway` into four tokens and rejecting valid configs as unknown-layer.) `validate.rs` now feeds the raw boundary key directly to the canonical parser and validates `boundary.a` / `boundary.b` against the known-layer set. Both `↔` and `<->` separators round-trip; layer names may contain `-`.
- **`schema_drift` honors Prisma `@@map` / `@map` mappings.** (Phase 3 third-round adversarial review fix — the round-2 fail-closed sweep would hard-fail valid unchanged Prisma schemas using physical-name mappings, because the checker compared the ORM model name against the migration table name.) `parse_prisma` now captures `@@map("phys_table")` and `@map("phys_col")` and the checker compares PHYSICAL names against migration DDL. Findings still surface ORM names for operator clarity; an unmatched `@@map`-ed table still fail-closes.
- **`openapi_compliance` double-miss guard.** (Phase 3 third-round adversarial review fix — the round-2 asymmetry guard caught one-side-recognized cases but `(saw_spec_file=false, saw_handler_file=false)` still returned `Passed` even when the boundary had plausible artifacts neither narrow heuristic could classify.) The check now collects "plausible-but-unrecognized" artifacts during the file scan: `.yaml`/`.yml`/`.json` files containing `openapi:`/`swagger:`/`paths:` markers, and source files with recognized extensions. When BOTH plausible buckets are non-empty AND neither narrow heuristic fired, the boundary emits `Warning` instead of `Passed`. Single-side plausibles do NOT trigger Warning to keep noise low — the asymmetry path is documentation-only in that case.
- **100ms budget is enforced post-hoc AND NEVER downgrades a Failed result.** (Phase 3 review fix — previously over-budget replaced the result with Warning.) The runner records elapsed wall time after `run()` returns; if the budget is exceeded, the result is preserved (Failed stays Failed, Warning stays Warning, Passed becomes Warning) and a budget-exceeded `SeamFinding` is appended. Rust threads cannot be safely cancelled — v0.2 accepts that a pathologically stuck plugin check would hang the process.
- **Seam findings are persisted to SQLite in the `seam_findings` table per evaluation.** (Phase 3 review fix.) The `evaluate` handler calls `metrics::store::record_evaluation` → `insert_seam_finding` for each manifest entry. Without this write, the new table and CHECK constraints would exist only in tests.
- **`seam_findings` CHECK constraints are load-bearing.** `category BETWEEN 1 AND 12` and `status IN ('passed','warning','failed')` catch bad insertions at the DB layer. `PRAGMA foreign_keys = ON` is set on every connection so `ON DELETE CASCADE` on `evaluation_id` works.

## IaC (meta-layer) semantics

- IaC layers (Terraform, Pulumi, CDK) are categorically different from application layers — they *create* other layers and *define* seams
- Verification is tiered: Tier 1 = static analysis only (`terraform validate`, `tfsec`, `checkov`), Tier 2 = AI evaluation of config correctness, Tier 3 = plan-based verification (`terraform plan` → evaluate diff)
- Actual deployment testing is out of scope — that's staging
- Provisioning seams verify IaC outputs ↔ application inputs (e.g., "does the provisioned DB endpoint match DATABASE_URL?")
- Runtime seams verify operational behavior (e.g., "does the API query match the DB schema?")

## Environment-specific contract properties

Contracts distinguish:
- **Invariant properties** — always checked (e.g., `response_format = "json"`)
- **Environment-specific properties** — only checked when targeting that environment (`[contract.api.environments.production]`)
- **Feature-flag-indexed contracts** — use pairwise coverage for flag combinations, not 2^N

## Deployment transitions

- During canary / blue-green, multiple versions of a layer exist simultaneously
- `pice evaluate --transition` tests both versions against shared downstream contracts
- Seam checks verify expand-and-contract migration compatibility
- After full cutover, transition checks retire

## Retry policy

- `retry.max_attempts` per layer (configured in `workflow.yaml`)
- `retry.fresh_context: true` (default) destroys and recreates the provider session between attempts — never reuse context from a failed attempt, it biases the retry
- Retries consume the layer's budget
- Exceeding max_attempts → layer marked `failed` → feature halts unless workflow allows proceeding (e.g., when evaluating always-run layers after an upstream failure)

## Adaptive pass loop invariants (v0.4 / Phase 4)

These are codified in `crates/pice-daemon/src/orchestrator/adaptive_loop.rs` and enforced by tests in `crates/pice-daemon/tests/adaptive_integration.rs`.

- **Sink writes happen BEFORE the halt decision.** A budget-halted loop still persists the triggering pass cost. Without this invariant, `SUM(pass_events.cost_usd)` would silently undercount on every budget halt — breaking cost reconciliation (Phase 4 contract criterion #16).
- **`next_*_params` reset after non-ADTS verdicts.** ADTS flags (`fresh_context`, `effort=xhigh`) apply to the immediately following pass only. A `Continue` verdict rolls them back to the project baseline. Missing this causes "escalation bleed" — flags from a Level-1 pass leak into subsequent passes that should run at baseline.
- **`escalation_events` only populated for ADTS.** SPRT, VEC, and None never emit level transitions. `LayerResult.escalation_events` is `None` for those algorithms — never `Some(vec![])` — so legacy manifest readers don't see a spurious empty array.
- **Confidence ceiling (~96.6%) enforced in `decide_halt`.** No reported confidence ever exceeds `CONFIDENCE_CEILING` (Phase 4 contract criterion #1). The cap applies even on budget-halted loops with 100+ consecutive successes — `posterior_mean_capped` enforces it before the value reaches the manifest.
- **`run_adts` exhaustion is the ONLY halt that bypasses `decide_halt`.** All other halt reasons (SPRT accept/reject, VEC entropy, budget, max_passes) flow through the universal dispatcher. ADTS Level 3 exhaustion is the orchestrator's concern — the pure-functional `decide_halt` does not know about adversarial divergence.
- **Per-pass context isolation extends Phase 3's per-layer rule.** Each pass's `evaluate/create` payload is byte-identical for a given layer across `pass_index = 1..N`. Prior-pass scores or findings are NEVER replayed into subsequent passes. Re-creating the provider session per pass enforces this at the protocol layer; verified by Phase 4 contract criterion #11.
- **`AdaptiveAlgo::None` respects budget AND max_passes.** None disables algorithm-driven halting but the universal guardrails still apply. A user who truly wants unbounded evaluation must raise `budget_usd` explicitly — there is no escape hatch.
- **Cold-start budget seed is `budget_usd / max_passes`.** When `CostStats` has zero observations, the projection falls back to this seed. A tight budget where the seed alone exceeds remaining capacity halts the loop on the pre-pass-1 check — see `cold_start_seed_blocks_overspend_on_pass_one` integration test.
- **Determinism is a first-class invariant.** Two back-to-back evaluations with identical `PICE_STUB_SCORES`, plan, and workflow produce byte-identical `passes[].index/score/cost_usd`, `halted_by`, `final_confidence`, `total_cost_usd`, `escalation_events`. Only `passes[].timestamp` and `evaluations.timestamp` may differ. Any non-deterministic source (HashMap iteration order, unsynchronized parallel ordering for ADTS `tokio::join!`) breaks contract criterion #15.
- **Capability declaration is the source of truth for cost (Phase 4.1).** The adaptive loop ignores any reported `cost_usd` when the provider does not declare `costTelemetry` — even well-formed values like `Some(0.0)`. `AdaptiveContext.cost_telemetry_available` is plumbed from `primary.capabilities().cost_telemetry`. The cost branch is a three-outcome match (real / fail-closed under budget / NULL); there is no synthetic cold-start seed that masks missing telemetry. Re-introducing a `fallback_seed = budget_usd / max_passes` would silently fabricate `$0.0000` totals and break contract criterion #16. See `.codex/rules/metrics.md` → "Capability declaration is the source of truth for cost".
- **Operational vs contract failure routing (Phase 4.1).** Mid-loop sink failures use the `metrics_persist_failed:` halted_by prefix (→ `LayerStatus::Pending`, exit 1). Provider/data failures use the `runtime_error:` prefix (→ `LayerStatus::Failed`, exit 2). All three call sites that consume the prefix MUST use `pice_core::cli::ExitJsonStatus::is_metrics_persist_failed(...)` — never inline `starts_with("metrics_persist_failed:")`. A typo would silently misroute exit codes; the unit test `metrics_persist_failed_prefix_helper_agrees_with_constant` locks the helper to the constant.

## Phase 5 cohort-parallelism invariants (v0.5 / Phase 5)

These are codified in `crates/pice-daemon/src/orchestrator/stack_loops.rs` and enforced by tests in `crates/pice-daemon/tests/parallel_cohort_integration.rs` + `tests/parallel_cohort_speedup_assertion.rs`.

- **Parallel path gate is conjunction:** `phases.evaluate.parallel == true` **AND** `cohort.len() > 1` **AND** `max_parallelism > 1`. Any false conjunct collapses to the sequential path. The five-cell gate matrix is pinned by `gate_*_takes_{parallel|sequential}` integration tests; production code uses `tracing::debug!(target: "pice.cohort", path = "...")` events, NEVER a test-only counter (the Cycle-2 Consider finding on gate observability).
- **`max_parallelism` defaults + cap (dual-surface enforcement).** `defaults.max_parallelism: None` → `num_cpus::get()`. Hard cap `MAX_PARALLELISM_HARD_CAP = 16` regardless of user config, to stay rate-limit-friendly against Anthropic / OpenAI. Users can LOWER the cap; they cannot raise it. Raising requires provider-side rate-limit-aware backoff (v0.6 concern).
  - **Single source of truth:** the constant lives in `pice-core::workflow::schema::MAX_PARALLELISM_HARD_CAP` (a `u32`). `pice-daemon::orchestrator::stack_loops` re-casts it to `usize` for the local `.clamp()` math; a typo on either side would not diverge because the `usize` alias is a direct re-cast.
  - **Load-time surface:** `pice_core::workflow::validate::validate_schema_only` emits a `ValidationWarning` (not Error) on `defaults.max_parallelism` when the user value exceeds the cap. Surfaced by `pice validate` and the daemon's pre-execution check. Warning (not Error) keeps exit 0 on a field whose only consequence is a logged clamp — refusing to load would block valid configs where other fields are fine.
  - **Dispatch-time surface:** `run_stack_loops_with_cancel` computes `requested = num_cpus or user value`, then `clamped = requested.clamp(1, cap)`. When `requested > cap`, emits a `warn!(requested, cap, ...)` so users who bypassed `pice validate` still see actionable feedback. Enforcement at BOTH sites is the defense-in-depth pattern (pinned by `max_parallelism_above_hard_cap_warns_without_erroring` + `max_parallelism_at_or_below_cap_does_not_warn` + `max_parallelism_unset_does_not_warn` in `pice-core::workflow::validate::tests`).
- **Manifest `layers[]` order = DAG topological order, NOT task completion order.** The parallel drain collects into a `HashMap<String, LayerOutcome>`, then `for layer_name in cohort { ... }` emits in DAG order. Two back-to-back parallel runs with identical per-layer score envs produce byte-identical `manifest.layers[].name` ordering. Pinned by `parallel_cohort_preserves_dag_order`.
- **Per-layer context isolation is compile-time enforced.** `LayerInputs` contains NO reference to `StackLoopsConfig<'_>`. A spawned cohort task receives OWNED clones of the layer's contract, filtered diff, and provider/workflow config. Referencing the outer cfg from inside a `tokio::spawn` future would not compile (the `'static` bound catches it). Pinned at runtime by `parallel_layers_dont_leak_context`, which asserts STRUCTURAL inequality on `EvaluateCreateParams.contract` / `.diff` (NOT substring grep on prompt text — that's the Cycle-2 Consider finding).
- **Owned-wrapper functions MUST thread `project_root`, not synthesize it.** `try_run_layer_adaptive_owned` (the `'static`-safe spawn target) re-builds a transient `StackLoopsConfig` to delegate into the shared reference-based `try_run_layer_adaptive`. Every field of that transient cfg must come from the real `LayerInputs`, NOT a placeholder. A prior revision passed `project_root: Path::new(".")` on the assumption that the callee ignored the field — this was a latent hazard: any future refactor that starts reading `cfg.project_root` would silently resolve against the daemon's CWD (typically `~/.pice/`) instead of the user's project. Rule: the only placeholder fields permitted are `layers` (`LayersConfig::default()`) and `plan_path` (`"/dev/null"`), which are genuinely unused by the adaptive path; everything else must be threaded. Add a comment naming which fields are placeholders and why they're safe.
- **`StackLoopsConfig` field changes require a full call-site sweep, including benches.** When adding/removing fields, run `rg -n "StackLoopsConfig \\{" crates/pice-daemon` and update every struct literal under `src/`, `tests/`, and `benches/`. Phase 7 merge validation caught stale bench fields (`contract_paths`, `prestarted_layer`) only when `cargo test --workspace --all-targets` compiled `crates/pice-daemon/benches/parallel_cohort_speedup.rs`; ordinary `cargo test` can miss bench target drift.
- **`PassMetricsSink` is `&self + Send + Sync`.** The orchestrator holds `Arc<dyn PassMetricsSink>` and clones it into every cohort task. Concrete impls own their interior mutability (`NullPassSink` — stateless; `RecordingPassSink` — `Mutex<Vec<_>>`; `DbBackedPassSink` — `Arc<Mutex<MetricsDb>>`). Pinned by `pass_sink_concurrent_record_no_data_race_{null,recording}`.
- **Cost aggregator is task-local, not shared.** `CostStats` is instantiated fresh inside each `run_adaptive_passes` call — zero parallel contention by construction. The write side flows through `PassMetricsSink::record_pass`, already parallel-safe. `metrics::aggregator` is READ-only (query functions for `pice metrics`) and takes `&MetricsDb`. No shared mutable cost state was present to redesign; the audit confirmed this.
- **Cancellation primitive: `tokio_util::sync::CancellationToken`.** Child tokens propagate cleanly through the cohort task tree. The orchestrator's `JoinSet` drain uses `tokio::select!` with `cancel.cancelled()` as a competing branch; on cancellation it calls `abort_all()` and marks affected layers `Failed` with `halted_by` starting with `"cancelled:"`. Pinned by `cancellation_aborts_in_flight_cohort`, which measures **cancel-to-return ≤ 300ms** (contract: 200ms + 100ms scheduler slack), NOT total elapsed.
- **Cancellation is fail-closed on the provider process.** `ProviderHost::spawn` sets `tokio::process::Command::kill_on_drop(true)` — when the Rust `Child` drops (e.g., `JoinSet::abort_all()` fires a cohort task's future off the runtime), the OS process receives SIGKILL immediately. Without this, a cancelled cohort task would drop the Rust handle while the Node stub kept sleeping through its `setTimeout`, orphaning the provider process and continuing to burn API quota after the manifest returned. Verified by `cancellation_aborts_in_flight_cohort`'s orphan probe: the stub writes `alive <pid>` / `done <pid>` to `PICE_STUB_ALIVE_FILE`; after cancel + grace, any `alive`-without-`done` PID must fail `kill(pid, 0)`. If you change `ProviderHost::spawn`, keep `kill_on_drop(true)` or replace it with an explicit `Drop` that awaits a bounded `shutdown()`.
- **Speedup ≥ 1.6× is a CI gate, not just a bench.** `cargo bench` does NOT fail CI on regression; criterion reports only. The dedicated `#[tokio::test(flavor = "multi_thread")]` at `tests/parallel_cohort_speedup_assertion.rs` runs the same fixture as the bench with smaller N and `assert!`s `parallel_mean <= 0.625 * sequential_mean`. Both run on a real multi-thread runtime; `tokio::time::pause()` would zero out the stub's `setTimeout` latency and produce a meaningless measurement.
- **Stub provider's parallel knobs are test-only.** `PICE_STUB_LATENCY_MS` (every score response sleeps this many ms before returning) and `PICE_STUB_SCORES_<LAYER_UPPER>` (per-layer score list — zero contention because each layer owns a disjoint array) ship only for benchmarks + determinism tests. Production providers NEVER read `PICE_STUB_*` envs.
- **Worktrees are NOT in Phase 5.** The evaluate path uses the `evaluate/create` / `evaluate/score` protocol, which does not currently accept `working_directory`. Phase 5.5 ships worktree isolation AFTER extending `EvaluateCreateParams` (in `pice-protocol`) + every provider impl in `packages/provider-*` to honor `working_directory`. Until that prerequisite lands, any worktree work is dead code.

## Phase 6 review-gate invariants (v0.6 / Phase 6)

These are codified in `crates/pice-core/src/gate.rs`, `crates/pice-daemon/src/handlers/review_gate.rs`, `crates/pice-daemon/src/handlers/evaluate.rs` (reconcile-on-resume), and `crates/pice-daemon/src/orchestrator/stack_loops.rs` (resume-from-disk). Tests: `crates/pice-daemon/tests/review_gate_lifecycle_integration.rs` + the 11-test inline module `handlers::review_gate::tests`.

- **Locks release at cohort boundary via handler return, not intra-run juggling.** When `check_gates_for_cohort` fires a gate at a cohort boundary, `run_stack_loops_with_cancel` transitions the affected layer(s) to `PendingReview`, saves the manifest, and returns `Ok(manifest)`. The evaluate handler then returns too, which drops the tokio `MutexGuard` + `fs2` file lock naturally. A subsequent `ReviewGate::Decide` RPC acquires the SAME per-manifest locks without nested re-entry. This is the plan's "release locks between cohorts" invariant — implemented as early-return, not as intra-run release/re-acquire. Pinned by `evaluate_releases_locks_between_cohorts` (`handlers::evaluate::tests`): sequential `lock.lock().await` calls wrapped in `tokio::time::timeout(250ms)` — a leaked guard would trip the timeout.
- **Resume-from-disk preserves decided state across re-invocations.** `run_stack_loops_with_cancel` attempts `VerificationManifest::load(manifest_path)` BEFORE creating a fresh manifest. If a manifest already exists, decided layers (Passed/Failed/Skipped/PendingReview) and prior gates (including reject counters) survive the next `pice evaluate` invocation. Per-cohort layer loop MOVES terminal/PendingReview entries into `immediate_results` unchanged, DROPS stale Pending/InProgress entries so a retry-after-reject re-evaluates only the affected layer. Without this, the CLI's 10-attempt auto-resume loop would wipe every decision and re-fire the same gates indefinitely. This fix addresses the Phase-6 eval Pass-1 Codex HIGH finding "post-decision resume restarts from empty manifest."
- **`check_gates_for_cohort` skips already-resolved layers.** On cohort re-processing (resume), the pure function checks each layer's `most_recent` prior gate and skips firing if the status is `Pending` / `Approved` / `Skipped`. A `Rejected` or `TimedOut` prior gate is the retry path — fire a NEW gate with the decremented `reject_budget` carried forward. A `BTreeMap<&str, &GateEntry>` is built once at function entry so the tail-scan lookup is amortized O(1) per cohort layer (cost concern flagged in the Pass-3 code review).
- **Project-scoped list/decide via `manifest_project_namespace(ctx.project_root())`.** `run_list` and `run_decide` restrict their state-dir walk to `state_dir/{project_hash_12chars}/*.manifest.json`. Without this, a gate decide from project A would mutate project B's manifest (if a feature_id prefix matched) while writing the audit row to project A's `.pice/metrics.db` — a split-brain state that silently corrupts both. The `resolve_project_scoped_state_dir` helper composes `pice_core::layers::manifest::state_dir` (honors `PICE_STATE_DIR`) + the project namespace. This fix addresses the Pass-1 Codex HIGH finding "gate lookup not scoped to the current project."
- **Idempotent decide on UNIQUE(gate_id) violation — decision-match gate.** If the SQLite audit row exists for a gate_id AND the manifest still shows the gate as `Pending`, the handler enters crash-recovery: re-derive the outcome from the durable audit via `GateDecisionOutcome::from_audit_decision_string`, apply the manifest mutation, reuse the prior `audit_id` (no new row). Matching decisions (same-actor crash retry) recover silently at `tracing::info!` level. MISMATCHING decisions (concurrent actor with different intent) surface `ReviewGateConflict` — preserves contract criterion #10. Pinned by `decide_same_decision_recovers_idempotently` + `decide_unique_violation_on_gate_id_surfaces_as_conflict`.
- **Audit-before-manifest ordering — decide handler AND reconciler.** Both code paths that mutate gate/layer state write the `gate_decisions` SQLite row BEFORE any in-memory manifest mutation. The decide handler: load manifest → compute outcome → INSERT audit → mutate manifest → save. The inline reconciler (`reconcile_expired_gates_inline`): pure scan to collect actions → INSERT audit (skip gate on failure, keep it Pending for next reconcile) → mutate manifest on audit success. This addresses the Pass-2 regression where the reconciler was calling `apply_timeout_if_expired` (which mutates in-place) before the audit write. Pinned by `decide_writes_audit_before_manifest_on_success`, `decide_audit_failure_does_not_mutate_manifest`, and `decide_audit_insert_failure_preserves_manifest_state` (chmod 0o444 mid-insert test).
- **RFC3339 canonicalization at the write boundary.** `metrics::store::canonicalize_rfc3339` normalizes `+00:00` → `Z` at insert time AND at filter time. Without this, lexicographic `ORDER BY requested_at` and `WHERE requested_at >= ?` break across mixed `Z` / `+00:00` encodings of the same instant. Pinned by `canonicalize_rfc3339_normalizes_plus_zero_to_z` + `query_gate_decisions_since_matches_mixed_utc_encodings`.
- **Inline timeout reconciliation on resume (partial Task 8).** When `pice evaluate` observes `overall_status == PendingReview` on disk, it runs `reconcile_expired_gates_inline` against the current clock BEFORE the short-circuit. For each Pending gate whose `timeout_at` has elapsed, the reconciler applies the pinned `on_timeout_action` (timeout-reject decrements retry budget if >0 OR halts with `gate_rejected` if =0; timeout-approve/skip mark the layer Passed). A gate that times out while nobody runs `pice evaluate` stays Pending until someone runs evaluate OR the Task 8 background reconciler ships (deferred to Phase 6.1). **Task 8 status: deferred.** The `Clock` trait + `MockClock` scaffolding live in `crates/pice-daemon/src/clock.rs` (`MockClock` gated `#[cfg(test)]` to keep `.expect()` out of production per `clippy::expect_used` deny). Phase 6.1 adds the daemon-startup polling reconciler that consumes this trait.
- **PTY test harness deferred (Task 19).** Contract criterion #3 calls for PTY-verified approve/reject/skip prompts via a `DecisionSource` trait. Phase 6 initially shipped the trait as scaffolding; the Pass-3 review noted `StdinLock: !Send` blocked wiring it into the async handler path, so both production prompt call sites (`commands/review_gate.rs` + `commands/evaluate.rs`) read stdin directly. The trait was removed as unused scaffolding; only the pure `render_prompt` helper (in `input/decision_source.rs`) survives. **Task 19 status: deferred.** Phase 7 reconstructs the input abstraction once a real consumer exists (e.g. a remote-dashboard `DecisionSource` or a `tokio::task::spawn_blocking`-wrapped TTY source for the PTY test harness).
- **MockClock is `#[cfg(test)]`-gated.** `crates/pice-daemon/src/clock.rs` exposes `Clock` + `SystemClock` in production; `MockClock` and its impls live behind `#[cfg(test)]` so the `.expect("MockClock mutex poisoned")` sites don't trip the strict `clippy::expect_used` deny on production code. The gate widens to a `test-utils` feature the day Phase 6.1 needs the mock from an integration-test binary.
- **`PICE_STATE_DIR` env override is honored at ONE layer.** `pice_core::layers::manifest::state_dir` checks `PICE_STATE_DIR` first, falling back to `~/.pice/state/`. The daemon's `resolve_state_dir` (review_gate handler) + all CLI-side helpers delegate to `state_dir`. This single source of truth closes the Phase-6 eval Pass-2 defect where `handlers/review_gate.rs::resolve_state_dir` honored the env var but `manifest_path_for` in pice-core did not, so test fixtures seeded one location while the handler looked in another.
- **Shared test support in `pice_daemon::test_support`.** `StateDirGuard` (RAII `PICE_STATE_DIR` swap + mutex serialization) lives in a `pub` module at crate root so the lib-level `#[cfg(test)]` inline modules AND the integration-test binaries under `tests/*.rs` import the SAME struct definition. Each test binary gets its own static `Mutex<()>` via `OnceLock` — which is the correct semantic, since cargo-test binaries run in separate processes. Centralization removes the "struct definition drift across copies" risk the Pass-3 review flagged.

## Phase 7 background-execution invariants (v0.7 / Phase 7)

These extend the Phase 5 cohort-parallelism invariants (same file) with dispatch-spawn + cross-feature concurrency semantics. Codified in `crates/pice-daemon/src/jobs/manager.rs`, `crates/pice-daemon/src/handlers/background.rs`, and `crates/pice-daemon/src/handlers/evaluate.rs` (background branch). Tests: `crates/pice-daemon/tests/background_dispatch_integration.rs`, `concurrent_features_integration.rs`.

- **Background dispatch is a "spawn-then-return" handshake.** `pice evaluate --background` handler writes a `ManifestStatus::Queued` manifest to disk, registers the feature in `FeatureJobManager`, spawns the orchestrator future via `tokio::spawn`, and returns `CommandResponse::Json { status: "background-dispatched", feature_id, run_id }` — all BEFORE the first provider RPC fires. Verified by `background_dispatch_returns_within_slo_with_expected_shape` (single call <1s) and `fifty_dispatches_meet_p95_slo` (p95 <500ms over 50).
- **`FeatureJobManager` uses a DashMap-keyed-on-feature-id for dedup.** Concurrent dispatch calls for the same `feature_id` serialize on the DashMap's `entry` API; the loser aborts its pre-permit-acquire worker and returns `SpawnError { run_id: existing }` which the handler surfaces as `ExitJsonStatus::FeatureAlreadyRunning` (exit 1). Verified by `second_dispatch_for_live_feature_surfaces_feature_already_running`.
- **Cross-feature `LogStore` isolation.** `LogStore` keys on `feature_id` through a `DashMap<String, Arc<Mutex<LogBuffer>>>`. `append_chunk("feat-a", ...)` followed by `snapshot("feat-b", ...)` returns an empty vec — no cross-feature contamination. Verified by `log_store_isolates_chunks_per_feature`.
- **`EventBus` fans per-feature AND wildcard.** Subscribers to `subscribe_feature("feat-a")` receive ONLY feat-a's events; a `subscribe_wildcard()` subscriber receives events from every feature. Verified by `event_bus_fans_per_feature_and_wildcard_correctly`.
- **Dual-semaphore layering.** The global provider semaphore (`FeatureJobManager::global_sem`, cap = `max_global_provider_concurrency`, hard-clamped to 32) bounds the number of features executing ANY orchestrator work at once. Within a feature, the cohort semaphore (cap = `max_parallelism`, hard-clamped to 16) bounds per-cohort parallel layer evaluation. The two are independent: a single feature can acquire up to `max_parallelism` provider sessions in a cohort even while other features queue on the global semaphore. Operators size them independently per rate-limit envelope.
- **`OwnedSemaphorePermit` parameter name matters.** The spawn future receives the permit as a function parameter. Binding it as `_permit` tells the compiler the value is unused and drops it immediately on entry, releasing the slot before the future's critical section — a silent concurrency bug that breaks the cap. Tests MUST bind as `permit` and optionally rebind `let _hold = permit` (or alias). Verified by `global_semaphore_bounds_concurrent_provider_holds` — a regression on the `_permit` early-drop would fail this test.
- **Cancel is per-feature and cooperative.** `FeatureJobManager::cancel(feature_id)` fires the feature's `CancellationToken`. The orchestrator's future honors the token at cohort boundaries via `tokio::select!`. Cancelling feat-B leaves feat-A and feat-C running; their own tokens are unchanged. Verified by `cancel_one_feature_leaves_others_running` (cancel-to-return <1s; Phase 5 invariant budget is 300ms with CI slack).
- **Supervisor emits `Cancelled` on panic.** A panicked orchestrator future is caught by the supervisor task wrapping the `JoinHandle`; the supervisor emits `ManifestEvent::Cancelled { reason: "panic" }` via the shared EventBus and removes the feature from the DashMap. The panicked feature's manifest is left as-is — the next startup reconciliation rewrites any `InProgress` status to `Failed(failed-interrupted)` with the `halted_by = "failed-interrupted"` prefix.
- **`JobEnv` is captured at spawn, not on every read.** The background handler snapshots `state_dir`, `project_root`, the workflow bytes, and per-layer contracts into an `Arc<JobEnv>` at dispatch time. The orchestrator future reads ONLY from this snapshot. Runtime env mutation (a user `export PICE_STATE_DIR=...` after dispatch) does NOT redirect the in-flight task. Prevents a race where a user could inject state-dir poisoning mid-run.

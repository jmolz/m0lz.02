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
- `CLAUDE.md` (project-level, unchanged)

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

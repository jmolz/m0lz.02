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

## DAG construction and parallel cohorts

The orchestrator builds a topological DAG from `layers.toml` + plan-declared dependencies, then groups layers into **cohorts** where every layer in a cohort has no pending dependencies. Cohorts execute sequentially; layers within a cohort execute in parallel via git worktree isolation.

- Max parallelism is configurable via `workflow.defaults.max_parallelism` (default = CPU count)
- Dependency edges always win — a layer with upstream in the current cohort never starts early
- `always_run` layers are evaluated even if their upstreams fail, unless the user configured `halt_on_upstream_failure = true`

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
- v0.2 ships ~30 static checks. LLM-based seam reasoning is deferred to v0.4 implicit contract inference.
- Static checks must be **deterministic and fast** (< 100ms each) — run on every pass, not cached
- Seam findings are written to `seam_findings` SQLite table with `category` (1–12) labeled

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

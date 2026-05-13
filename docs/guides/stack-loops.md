# Stack Loops Guide

Stack Loops run PICE per technology layer instead of treating a feature as one monolithic contract. A feature passes only when every activated layer has passed provider-backed evaluation and required seam checks.

## Core Model

- A layer has paths, dependencies, contract settings, and optional always-run behavior.
- Dependency cascade is transitive: if `database` activates `api`, and `api` activates `frontend`, the frontend layer is included too.
- Infrastructure, deployment, and observability are always-run by default. With no own diff they remain pending for seam/static checks, not skipped.
- Manifest state is the source of truth for every adapter.

## Commands

```bash
pice init --upgrade
pice layers detect --json
pice layers list --json
pice layers check --json
pice layers graph
pice validate --json
pice evaluate <plan.md> --background --wait --timeout-secs 120 --json
pice status <feature-id> --follow --stream-json
pice logs <feature-id> --follow --stream-json
pice review-gate --list --feature-id <feature-id> --json
```

## Layer Configuration

`.pice/layers.toml` is committed project policy.

```toml
[layers]
order = ["database", "api", "frontend", "infrastructure", "deployment", "observability"]

[layers.api]
paths = ["app/api/**", "src/routes/**"]
depends_on = ["database"]

[layers.deployment]
paths = [".github/workflows/**", "deploy/**"]
always_run = true
depends_on = ["infrastructure"]
```

Run `pice layers check --json` in CI so new files do not bypass all layers.

## Workflow Policy

`.pice/workflow.yaml` controls default tier, confidence, max passes, parallel evaluation, review gates, budget behavior, and model overrides.

Use `phases.evaluate.parallel: false` when provider rate limits matter more than latency. Use `defaults.max_parallelism` to cap independent cohort concurrency; the implementation hard-caps the value at 16.

## Contracts

Layer contracts should name evidence that belongs to that layer:

- database: migrations, schema drift, backfill safety
- api: auth, request/response shape, timeout and retry behavior
- frontend: user-visible state, API error handling, accessibility
- infrastructure: environment variables, secrets, network and resource boundaries
- deployment: release workflow, rollback, health checks
- observability: logs, metrics, alerts, runbook hooks

Do not mark a layer passed without provider-backed scoring. Phase 1 or scaffold-only runs record pending status until evaluation runs.

## Seam Checks

Seam checks evaluate boundaries between layers. Typical examples include schema drift, OpenAPI compatibility, auth handoff, service discovery, config mismatch, cold-start order, and resource exhaustion.

Seams run in the daemon. Provider support is optional; providers may ignore seam fields and still be compatible.

## Adaptive Evaluation

The default adaptive algorithm is Bayesian SPRT. It can halt on confidence, rejection, budget, review gate, cancellation, or max passes. Confidence reports must respect the correlated-evaluator ceiling described in `docs/research/convergence-analysis.md`.

## Review Gates

Review gates pause a layer and persist pending state in the manifest. Background mode returns a pending-review status; users decide gates with `pice review-gate`.

Gate decisions are append-only audit rows. Reject-with-retry consumes retry budget; approve and skip do not.

## CI Pattern

For offline CI, use the stub provider and isolated state:

```bash
export PICE_STATE_DIR="$RUNNER_TEMP/pice-state"
export PICE_DAEMON_SOCKET="$RUNNER_TEMP/pice-daemon.sock"
export PICE_STUB_SCORES="9.5,0.001;9.5,0.001"
node scripts/acceptance/phase8-reference-projects.mjs
```

## Troubleshooting

- `--background` fails under inline mode: unset `PICE_DAEMON_INLINE`.
- `status --follow` has no live frames: confirm the same `PICE_STATE_DIR` and socket are used by dispatch and subscribe commands.
- A layer is skipped unexpectedly: inspect dependency cascade and `always_run` flags in `.pice/layers.toml`.
- Cost budgets halt immediately: ensure every enabled provider declares cost telemetry or set `budget_usd: 0`.

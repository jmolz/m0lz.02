---
description: Review code changes for bugs, security issues, and improvements — includes cumulative regression suite
---

# Code Review

Perform a thorough code review of the current changes AND run the cumulative regression suite to ensure all previously built features still work.

## Phase 0: Contract Check

Before starting the standard review, check if the most recent plan has a contract:

```bash
# Find the most recently modified plan file
ls -t .claude/plans/*.md 2>/dev/null | head -1
```

If a plan file exists, read its `## Contract` section. If a contract is found:

1. Note the tier and criteria in the review output
2. After Phase 3 (Code Review), add a **Phase 3.5: Contract Evaluation** that spawns a fresh sub-agent to grade the implementation against the contract (see `/evaluate` for the full evaluator protocol)
3. Include the contract evaluation results in the final output

If no contract exists, skip this and proceed normally. The contract evaluation is additive — it does not replace the standard code review phases.

---

## Phase 0.5: Database Migration Check

PICE embeds SQLite migrations in `crates/pice-daemon/src/metrics/db.rs` (functions `migrate_v3`, etc.). There is no external migrations directory — schema evolution is in-process and idempotency is asserted by `migrate_v3_is_idempotent`, `migrate_from_v1_to_v3`, and `migrate_from_v2_to_v3` in `db.rs` inline `#[cfg(test)]` modules.

### Step 1: Check for schema drift

```bash
# A schema change ALWAYS modifies metrics/db.rs's migrate_* function bodies.
git diff HEAD --name-only -- 'crates/pice-daemon/src/metrics/db.rs' 'crates/pice-daemon/src/metrics/store.rs'
```

If `db.rs` was modified, verify the new migration is **idempotent**, **forward-compatible** (existing rows survive), and the schema_version constant was bumped. Flag missing migration tests as **Critical**.

### Step 2: Apply migrations

PICE migrations apply on daemon startup — no separate apply step. Confirm by running the migration tests:

```bash
cargo test -p pice-daemon --lib metrics::db::tests -- --test-threads=1
```

The command must succeed before proceeding. If it fails, flag as **Critical**.

## Phase 1: Regression Suite

Run these tests FIRST to verify that all previously shipped features are intact. This suite grows with every feature — when you ship a feature, add its tests here. If any fail, flag them as **Critical** and investigate before proceeding with the code review.

PICE's full test corpus runs through two commands. The Rust workspace runner picks up every `#[cfg(test)]` module AND every `tests/*.rs` integration target automatically; the TS runner picks up every `__tests__/*.test.ts` file. Listing individual integration targets below documents what's covered for human review — the actual CI command is the workspace one.

```bash
# Full workspace regression — covers every test target
cargo test --workspace --all-targets && pnpm test
```

For targeted re-runs of specific milestones during a review:

```bash
# v0.1 baseline (provider host, CLI commands, validate, evaluate)
cargo test -p pice-cli --test command_integration --test provider_integration \
  --test provider_host_integration --test validate_integration --test evaluate_integration

# v0.2 daemon split (lifecycle, auth, streaming, stale-socket recovery, workflow loader)
cargo test -p pice-daemon --test lifecycle --test auth --test streaming \
  --test server_unix_stale_socket --test workflow_integration

# v0.2 Stack Loops + seam checks
cargo test -p pice-daemon --test seam_integration

# Phase 4 adaptive evaluation (SPRT/ADTS/VEC + concurrency + CLI exit routing)
cargo test -p pice-daemon --test adaptive_integration --test adaptive_concurrent
cargo test -p pice-cli --test adaptive_integration

# Phase 5 cohort parallelism (gate matrix, DAG order, context isolation, speedup, cancellation, hard cap)
cargo test -p pice-daemon --test parallel_cohort_integration --test parallel_cohort_speedup_assertion

# Phase 5 criterion bench (advisory — does NOT fail CI on regression; speedup gate lives in the assertion test above)
cargo bench -p pice-daemon --bench parallel_cohort_speedup -- --quick

# TS provider stack
pnpm test
```

### What each test covers

**v0.1 baseline (commit 00c7e74 — pre-Phase-4)**

| Test File | Feature | What It Validates |
| --------- | ------- | ----------------- |
| `pice-cli/tests/command_integration.rs` | CLI command dispatch | Top-level `pice` command parsing, `--help`, `--version`, JSON-mode flag propagation |
| `pice-cli/tests/provider_integration.rs` | Provider registry | Resolve by name, walk-up search for `packages/`, error when absent |
| `pice-cli/tests/provider_host_integration.rs` | Provider host process model | Spawn, JSON-RPC roundtrip, shutdown timeout split, notification forwarding |
| `pice-cli/tests/validate_integration.rs` | `pice validate` end-to-end | Workflow YAML schema check, layer cross-references, typed `ExitJson` failure shape |
| `pice-cli/tests/evaluate_integration.rs` | `pice evaluate` end-to-end | All six typed `ExitJsonStatus` discriminants (PlanNotFound, PlanParseFailed, NoContractSection, WorkflowValidationFailed, SeamFloorViolation, MergedSeamValidationFailed), clean-fixture exit 0, failing-seam exit 2 |

**v0.2 headless daemon (Phase 1-3)**

| Test File | Feature | What It Validates |
| --------- | ------- | ----------------- |
| `pice-daemon/tests/lifecycle.rs` | Daemon start/stop/restart | SIGTERM graceful shutdown, manifest flush before exit, socket cleanup |
| `pice-daemon/tests/auth.rs` | Bearer-token auth on socket | Token rotation per startup, `-32002` rejection for missing/invalid token, file mode 0600 |
| `pice-daemon/tests/streaming.rs` | Streaming notifications over socket | Chunk forwarding, gate on `!req.json` (no stream in JSON mode) |
| `pice-daemon/tests/server_unix_stale_socket.rs` | Stale socket recovery | Detect ECONNREFUSED, remove + recreate, idempotent multi-daemon prevention |
| `pice-daemon/tests/workflow_integration.rs` | Workflow YAML loader + merge | Floor-merge semantics, deny_unknown_fields, schema_version mismatch error |
| `pice-daemon/tests/seam_integration.rs` | Seam checks (12 categories) | Boundary parsing, fail-closed schema_drift, asymmetric openapi_compliance warning, dedupe at SQLite, 100ms budget enforcement |

**Phase 4 adaptive evaluation (commits 722b264..b74e9c2)**

| Test File | Feature | What It Validates |
| --------- | ------- | ----------------- |
| `pice-daemon/tests/adaptive_integration.rs` (~27 tests) | SPRT / ADTS / VEC end-to-end | All four halt reasons, ADTS three-level escalation audit trail, VEC entropy halt, budget halt before algorithm halt, context isolation (byte-identical prompt across passes), determinism, cost reconciliation, mid-loop sink failure parity (Pass-11 routes to `Pending` via `metrics_persist_failed:` prefix, exit 1 not 2), telemetry-off NULL-cost ground-truth at the sink layer (Pass-11.1 S3) |
| `pice-daemon/tests/adaptive_concurrent.rs` (4 tests) | Per-manifest concurrency isolation | Same-feature lock serializes concurrent tasks, different-feature distinct locks, cross-process file lock blocks second acquirer (fs2 flock), disjoint pass_events on shared DB |
| `pice-cli/tests/adaptive_integration.rs` (12 tests) | CLI exit-code routing + telemetry semantics | SPRT reject → exit 2 via typed `ExitJsonStatus::EvaluationFailed`; budget/max-passes → exit 0; corrupt-DB legacy + Stack Loops → `MetricsPersistFailed` exit 1; **stock-defaults workflow (capability-gate regression guard)**; **telemetry-off path collapses `total_cost_usd` to NULL with warning (Pass-11 CRITICAL #1 regression guard)** |
| `provider-base/__tests__/roundtrip.test.ts` (43 tests) | TS-side protocol roundtrip | Every wire shape: session create/result, evaluate/create with passIndex/costUsd/freshContext/effortOverride/confidence camelCase, seam check result + finding, deny_unknown_fields on request params |
| `provider-stub/__tests__/deterministic.test.ts` (9 tests) | Deterministic stub provider | `PICE_STUB_SCORES` parsing, `PICE_STUB_COST_TELEMETRY_OFF` capability override, mid-loop error trigger, cost field omission |
| `provider-base/__tests__/provider.test.ts` (3 tests) | Base provider abstraction | initialize/createSession/destroy lifecycle |
| `provider-base/__tests__/transport.test.ts` (11 tests) | stdio JSON-RPC transport | Framing, partial reads, error response shape |
| `provider-claude-code/__tests__/claude-code.test.ts` (7 tests) | Claude Code SDK provider | Capability declaration, prompt assembly, error propagation |
| `provider-codex/__tests__/codex.test.ts` (5 tests) | Codex/OpenAI evaluator provider | Adversarial review structuring, cost extraction |

**Phase 5 cohort parallelism (commits 1f6424f..84aa43f)**

| Test File | Feature | What It Validates |
| --------- | ------- | ----------------- |
| `pice-daemon/tests/parallel_cohort_integration.rs` (~10 tests) | Gate matrix + DAG order + context isolation + cancellation + hard cap | Five-cell gate matrix via `tracing`-layer `path=` capture (`parallel` vs `sequential`), `parallel_cohort_preserves_dag_order` (manifest order = topological, not completion), `parallel_layers_dont_leak_context` (structural `EvaluateCreateParams.contract`/`.diff` inequality + `PICE_STUB_REQUEST_LOG`), `cancellation_aborts_in_flight_cohort` (cancel-to-return ≤ 300ms = 200ms + 100ms scheduler slack, `halted_by` begins `"cancelled:"`, Unix `libc::kill(pid, 0)` orphan probe via `PICE_STUB_ALIVE_FILE`), `max_parallelism_hard_cap_at_16` (20 layers × 100ms, requested 64 → clamped to 16) |
| `pice-daemon/tests/parallel_cohort_speedup_assertion.rs` (1 test) | Speedup CI gate | `parallel_cohort_meets_16x_speedup` — real `#[tokio::test(flavor = "multi_thread")]` (NOT `tokio::time::pause()`), asserts `parallel_mean ≤ 0.625 × sequential_mean` (≥ 1.6× speedup). Advisory criterion bench at `crates/pice-daemon/benches/parallel_cohort_speedup.rs` runs same fixture at bench N for humans; CI-failing gate lives HERE |
| `pice-daemon/tests/adaptive_concurrent.rs` (+ 3 Phase-5 additions, 7 total) | `PassMetricsSink` thread-safety + task-local cost rollup | `pass_sink_concurrent_record_no_data_race_null` (8×1000 on `NullPassSink`), `pass_sink_concurrent_record_no_data_race_recording` (4×250 on `RecordingPassSink`), `cost_aggregator_concurrent_record_produces_correct_rollup` (8 tasks × 100 observations × $0.01 = $8.00 ± 1e-9 — proves `CostStats` is task-local, no shared aggregator) |
| `pice-daemon/src/metrics/store.rs::tests::db_backed_pass_sink_concurrent_record_no_lost_writes` (inline, 1 test) | SQLite-backed sink concurrency | 4 tasks × 250 `record_pass` calls on `Arc<DbBackedPassSink>` wrapping `Arc<Mutex<MetricsDb>>` → 1000 rows persisted, zero lost writes |
| `pice-core/src/workflow/schema.rs::tests` (+ 3 Phase-5 additions) | `phases.evaluate.parallel` serde default | `evaluate_phase_parallel_default_true_when_field_omitted`, `workflow_yaml_empty_evaluate_block_applies_parallel_default`, `evaluate_phase_rejects_unknown_field_parralel_typo` (deny_unknown_fields) |
| `provider-stub/__tests__/atomic-scores.test.ts` (8 tests) | Per-layer score isolation contract | `perLayerScoreEnvName` normalization, `parseStubScores` independence, 6 concurrent `Promise.all` interleaved (backend → [8,9,10], frontend → [7,7,7]), 50-iteration stress test across two layers, read-only array semantics |
| `provider-stub/__tests__/latency.test.ts` (10 tests) | `PICE_STUB_LATENCY_MS` real-clock wait | env variants (unset, `200`, invalid), elapsed ≥ 190 ms at 200 ms env (documents ~50 ms jitter tolerance) |

### Source files these tests protect

- `crates/pice-cli/src/main.rs` — CLI entrypoint
- `crates/pice-cli/src/commands/*.rs` — render_response, JSON vs text output
- `crates/pice-cli/src/provider/*.rs` — provider host process model
- `crates/pice-daemon/src/lifecycle.rs` — SIGTERM/SIGINT, shutdown, watchdog
- `crates/pice-daemon/src/server/router.rs` — RPC dispatch + per-manifest lock map
- `crates/pice-daemon/src/server/auth.rs` — bearer token rotation, file mode 0600
- `crates/pice-daemon/src/handlers/evaluate.rs` — `pice evaluate` backend, finalize, metrics-persist routing (mid-loop + finalize)
- `crates/pice-daemon/src/handlers/status.rs` — `pice status` aggregation, confidence ceiling clamp at report boundary
- `crates/pice-daemon/src/orchestrator/stack_loops.rs` — Stack Loops engine, seam runner, capability gate, telemetry-off warning
- `crates/pice-daemon/src/orchestrator/adaptive_loop.rs` — SPRT/ADTS/VEC pass loop, write-ahead sink ordering, telemetry-aware cost resolution
- `crates/pice-daemon/src/orchestrator/core.rs` — provider orchestrator, capability deserialization
- `crates/pice-daemon/src/metrics/db.rs` — SQLite migrations (v1→v2→v3), foreign keys, CHECK constraints
- `crates/pice-daemon/src/metrics/store.rs` — pass_events / evaluations / seam_findings / cost reconciliation SQL
- `crates/pice-core/src/adaptive/*.rs` — pure SPRT/ADTS/VEC/cost/decide algorithms, `cap_confidence`, calibration
- `crates/pice-core/src/workflow/*.rs` — YAML loader, schema, validate, floor-merge, trigger grammar
- `crates/pice-core/src/layers/*.rs` — layers.toml parsing, manifest schema, file-tag filtering, confidence-clamp on load
- `crates/pice-core/src/seam/*.rs` — SeamCheck trait, registry, default 12-category checks
- `crates/pice-core/src/cli/mod.rs` — `ExitJsonStatus` typed discriminants
- `crates/pice-protocol/src/lib.rs` — JSON-RPC contract types (Rust side)
- `packages/provider-protocol/src/messages.ts` — JSON-RPC contract types (TS side)
- `packages/provider-base/src/*.ts` — base provider, transport, capabilities helpers
- `packages/provider-stub/src/*.ts` — deterministic test stub
- `packages/provider-claude-code/src/*.ts` — Claude Code SDK bridge
- `packages/provider-codex/src/*.ts` — Codex/OpenAI bridge
- `templates/pice/workflow.yaml` + `templates/pice/workflow-presets/*.yaml` — shipped defaults (capability-gate compatible)
- `crates/pice-daemon/src/orchestrator/stack_loops.rs` — Phase 5 cohort parallel path: `MAX_PARALLELISM_HARD_CAP=16`, gate conjunction `parallel_configured && cohort_size>1 && max_parallelism>1`, `LayerInputs` owned-struct compile-time context-isolation boundary, `build_per_layer_inputs` single-threaded extractor, `tokio::JoinSet` + `Semaphore` dispatch, `biased` `tokio::select!` with `cancel_fired` gate, `CancellationToken` child-token propagation, `"cancelled:{pre_spawn,in_flight,join_aborted}"` halted_by markers, DAG-order manifest emission, `debug!(target: "pice.cohort", path=...)` gate observability
- `crates/pice-daemon/src/orchestrator/adaptive_loop.rs` — Phase 5 `PassMetricsSink: Send + Sync` trait with `&self` `record_pass`, `NullPassSink` stateless, `RecordingPassSink` with `Mutex<Vec<_>>` + poison-safe `rows()` reader, task-local `CostStats` (no shared aggregator)
- `crates/pice-daemon/src/provider/host.rs` — Phase 5 `tokio::process::Command::kill_on_drop(true)` on `ProviderHost::spawn` — load-bearing for zero-orphan-session invariant on cohort cancellation
- `crates/pice-daemon/src/metrics/store.rs` — Phase 5 `DbBackedPassSink` wrapping `Arc<Mutex<MetricsDb>>` for concurrent SQLite writes from parallel cohort tasks
- `crates/pice-core/src/workflow/schema.rs` — Phase 5 `EvaluatePhase.parallel: bool` with `#[serde(default = "default_evaluate_parallel")]` returning `true` (deny_unknown_fields closes the empty-evaluate-block regression)
- `crates/pice-daemon/benches/parallel_cohort_speedup.rs` — Phase 5 criterion bench (advisory only — no CI failure on regression)
- `crates/pice-daemon/Cargo.toml` — Phase 5 `[target.'cfg(unix)'.dev-dependencies] libc = "0.2"` for orphan-PID liveness probe in cancellation test; `tokio-util` with `rt` feature for `CancellationToken`
- `packages/provider-stub/src/deterministic.ts` — Phase 5 `perLayerScoreEnvName` + `PICE_STUB_SCORES_<LAYER>` per-layer isolation (disjoint score arrays, zero shared-iterator contention)
- `packages/provider-stub/src/index.ts` — Phase 5 `PICE_STUB_LATENCY_MS` real-clock setTimeout, `PICE_STUB_ALIVE_FILE` alive/done PID sentinel, `PICE_STUB_REQUEST_LOG` per-request JSONL capture

### Expected results

All tests should pass. Baseline: **829 Rust tests (1 ignored — doc-test in `crates/pice-daemon/src/handlers/mod.rs` line 5), 96 TypeScript tests, 0 lint errors, 0 warnings, clean release build.**

If any fail after your changes:

1. Check if you modified the source files listed above
2. Read the failing test to understand what behavior it expects
3. Fix your code to preserve the expected behavior, or update the test if the behavior change is intentional

### Updating the regression suite

After running the regression suite and before finishing the review, check if any test files touched in this session are NOT already in the suite above. To find them:

```bash
# Compare test files modified in uncommitted changes against the suite list
git diff --name-only main...HEAD -- 'crates/**/tests/*.rs' 'packages/**/__tests__/*.test.ts'
```

For each test file that exercises a newly shipped or migrated feature and is NOT already in the regression suite:

1. **Add it to the test runner command** in the bash block above
2. **Add a row to the "What each test covers" table** with: file name, test count, feature name, what it validates
3. **Add any new source files to the "Source files these tests protect" list**
4. **Add a line to the output format** checklist in Phase 4

Also check inline `#[cfg(test)]` modules in `crates/*/src/**/*.rs` — Rust unit tests live next to source code, not in `tests/`. They are picked up automatically by `cargo test --workspace`, but new modules deserve a documentation row when they cover a new feature.

This ensures the suite is always exhaustive: every feature we ship gets regression-protected automatically.

## Phase 2: Full Validation

After regression tests pass, run the full suite:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --lib -p pice-core -- -D clippy::unwrap_used -D clippy::expect_used
cargo clippy --lib -p pice-daemon -- -D clippy::unwrap_used -D clippy::expect_used
cargo test --workspace --all-targets
pnpm lint
pnpm typecheck
pnpm test
pnpm build
cargo build --release
```

Expected baseline: **829 Rust tests passing (1 ignored — doc-test in `pice-daemon/src/handlers/mod.rs`), 96 TypeScript tests passing, 0 lint errors, 0 clippy warnings (workspace + lib unwrap/expect denies), clean release build.**

## Phase 3: Code Review of Current Changes

```bash
git branch --show-current
git status
```

**If on a feature branch (worktree)**, diff against main to see the full feature scope:

```bash
git diff main...HEAD
git diff main...HEAD --stat
```

**If on main**, diff against the last commit:

```bash
git diff HEAD
```

If reviewing a specific commit, check it out or diff against it.

### Focus Areas

1. **Logic errors** and incorrect behavior
2. **Edge cases** that aren't handled
3. **Null/undefined reference** issues — Rust `Option::unwrap`, TS `!.` non-null assertion
4. **Race conditions** or concurrency issues — tokio task ordering, shared `Arc<Mutex<...>>` lock keying, cross-process flock
5. **Security vulnerabilities** — command injection, SQL injection, unsafe `transmute`, file permissions
6. **Resource management** — leaks, unclosed connections, RAII drop guards (`AutoStageGuard` pattern), tokio task cancellation
7. **API contract violations** — JSON-RPC method names, kebab-case vs camelCase wire forms, `deny_unknown_fields` consistency between Rust + TS
8. **Caching bugs** — staleness, bad keys, invalid invalidation, ineffective caching
9. **Pattern violations** — check `CLAUDE.md` and `.claude/rules/` (especially `daemon.md`, `stack-loops.md`, `workflow-yaml.md`, `metrics.md`, `protocol.md`) for project conventions
10. **PICE-specific invariants** — confidence ceiling 0.966, budget halt before algorithm halt, write-ahead sink ordering, byte-identical prompt across passes, fail-closed evaluation, capability gate

### Rules

- Use sub-agents to explore the codebase in parallel for efficiency
- Report pre-existing bugs found near the changed code — code quality matters everywhere
- Do NOT report speculative or low-confidence issues — conclusions must be based on actual code understanding
- If reviewing a specific git commit, note that local code may differ from that commit

## Phase 4: Output Format

### Migration Status

```
Schema Drift: NONE / DETECTED (db.rs migrate_* changes)
New Migration: bumped schema_version to vN — idempotency test added/updated YES/NO
Action: Re-run `cargo test -p pice-daemon --lib metrics::db::tests` or N/A
```

### Regression Suite Results

```
Regression Suite: PASS / FAIL

v0.1 baseline:
  - command_integration (N tests): ✓ / ✗
  - provider_integration / provider_host_integration: ✓ / ✗
  - validate_integration: ✓ / ✗
  - evaluate_integration: ✓ / ✗

v0.2 daemon split:
  - lifecycle / auth / streaming / server_unix_stale_socket: ✓ / ✗
  - workflow_integration: ✓ / ✗
  - seam_integration: ✓ / ✗

Phase 4 adaptive evaluation:
  - daemon adaptive_integration (~27 tests): ✓ / ✗
  - daemon adaptive_concurrent (4 original + 3 Phase-5 concurrent-sink tests): ✓ / ✗
  - cli adaptive_integration (12 tests, including Pass-11 telemetry-off + stock-defaults): ✓ / ✗
  - TS roundtrip + deterministic stub (52 tests): ✓ / ✗

Phase 5 cohort parallelism:
  - parallel_cohort_integration (~10 tests — gate matrix, DAG order, context isolation, cancellation + orphan probe, hard cap): ✓ / ✗
  - parallel_cohort_speedup_assertion (1 test — ≥1.6× speedup CI gate on real multi-thread runtime): ✓ / ✗
  - parallel_cohort_speedup bench (advisory, criterion): ✓ / ✗
  - workflow/schema Phase-5 additions (3 tests — evaluate.parallel default + deny_unknown_fields): ✓ / ✗
  - metrics/store db_backed_pass_sink_concurrent_record_no_lost_writes (1 test): ✓ / ✗
  - TS atomic-scores + latency (18 tests): ✓ / ✗

Full Suite: 829 / 96 tests passing
Lint: 0 errors, 0 warnings (workspace + lib unwrap/expect denies)
Build: PASS / FAIL
```

### Contract Evaluation (if applicable)

```
Contract: {feature name} — Tier {N}
Evaluator: Isolated sub-agent (no implementation context)

| Criterion | Threshold | Score | Pass |
|-----------|-----------|-------|------|
| {name} | {T}/10 | {S}/10 | YES/NO |

Overall: PASS / FAIL
```

If no contract was found in the plan, output: `Contract: N/A — no contract in plan`

### Code Review Findings

Group findings by severity:

**Critical** — Must fix before merge (bugs, security, data loss)

- `file:line` — description of the issue and recommended fix

**Warning** — Should fix (performance, maintainability, pattern violations)

- `file:line` — description and suggestion

**Suggestion** — Consider improving (readability, minor optimizations)

- `file:line` — description and suggestion

**Positive** — What's done well (reinforce good patterns)

- Description of what was done right

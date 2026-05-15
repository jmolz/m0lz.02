---
name: "source-command-review"
description: "Review code changes for bugs, security issues, and improvements — includes cumulative regression suite"
---

# source-command-review

Use this skill when the user asks to run the migrated source command `review`.

## Command Template


# Code Review

Perform a thorough code review of the current changes AND run the cumulative regression suite to ensure all previously built features still work.

## Phase 0: Contract Check

Before starting the standard review, check if the most recent plan has a contract:

```bash
# Find the most recently modified plan file across Codex-native and legacy roots.
find .codex/plans .claude/plans -maxdepth 1 -type f -name '*.md' -print 2>/dev/null \
  | while IFS= read -r plan; do
      printf '%s\t%s\n' "$(stat -f '%m' "$plan" 2>/dev/null || stat -c '%Y' "$plan")" "$plan"
    done \
  | sort -rn \
  | head -1 \
  | cut -f2-
```

If a plan file exists, read its `## Contract` section. If a contract is found:

1. Note the tier and criteria in the review output
2. After Phase 3 (Code Review), add a **Phase 3.5: Contract Evaluation** that spawns a fresh sub-agent to grade the implementation against the contract (see `/evaluate` for the full evaluator protocol)
3. Include the contract evaluation results in the final output

Phase 3.5 must follow `/evaluate`'s Validation Proof Discipline: a validation command passing is not enough unless the evaluator identifies the assertion, request-log check, fixture, or observable output that would fail if the criterion regressed. If the named validation target is too broad or does not directly cover the criterion, report the coverage gap instead of treating the criterion as proven.

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
env RUST_TEST_THREADS=1 PATH="$PWD/target/debug:$PATH" cargo test --workspace --all-targets
pnpm test
```

The Rust command intentionally serializes tests and exposes `target/debug` on
`PATH`. Phase 8 review caught that daemon-spawning CLI tests can fail
spuriously when `pice-daemon` is not discoverable or socket tests race under the
default parallel runner.

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

# Phase 6 review gates (lifecycle scenarios, resume-from-disk, timeout reconciliation, idempotent recovery, project scoping)
cargo test -p pice-daemon --test review_gate_lifecycle_integration
cargo test -p pice-cli --test evaluate_review_gate_pending --test audit_gates_csv_roundtrip

# TS provider stack
pnpm test

# Release and local-CI tripwires (NPM publish gate, artifact smoke, Docker/Windows harness policy)
pnpm exec vitest run scripts/acceptance/release-workflow-policy.test.mjs \
  scripts/acceptance/release-artifact-smoke.test.mjs \
  scripts/acceptance/local-ci-policy.test.mjs
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
| `provider-claude-code/__tests__/claude-code.test.ts` (7 tests) | Codex SDK provider | Capability declaration, prompt assembly, error propagation |
| `provider-codex/__tests__/codex.test.ts` (5 tests) | Codex/OpenAI evaluator provider | Adversarial review structuring, cost extraction |
| `scripts/acceptance/release-workflow-policy.test.mjs` | Release publishing policy | Tag-triggered GitHub releases depend on `npm-publish`, tag releases fail closed on package-version mismatch, manual dry-runs do not publish, and every publishable package version stays aligned |
| `scripts/acceptance/release-artifact-smoke.test.mjs` (3 tests) | Release artifact smoke hardening | Windows daemon-stop named-pipe disconnects are treated as shutdown races only after status polling confirms the daemon is stopped; non-Windows stop failures remain fatal |
| `scripts/acceptance/local-ci-policy.test.mjs` (4 tests) | Local CI and Windows smoke policy | `Dockerfile.ci`, `scripts/ci/local-linux.sh`, `scripts/ci/windows-smoke.ps1`, and the manual `Windows Smoke` workflow keep Linux Docker preflight and Windows-specific validation discoverable |

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

**Phase 6 review gates (branch feature/phase-6-review-gates, 938 tests total after Phase 6 ships)**

| Test File | Feature | What It Validates |
| --------- | ------- | ----------------- |
| `pice-daemon/tests/review_gate_lifecycle_integration.rs` (11 tests) | End-to-end gate scenarios | `scenario_1_trigger_fires` (pure `check_gates_for_cohort` fires gate with pinned `timeout_at` + `reject_budget`), `scenario_2_list_returns_pinned_fields`, `scenario_3_approve_completes` (audit + layer back to Passed), `scenario_4a/4b` (reject-with-retry decrements, reject-no-retry halts exit 2) + `scenario_4_reject_retry_cycle` (full retry cycle chained), `scenario_5_skip_keeps_layer_passed`, `scenario_7_concurrent_decide` (UNIQUE CAS → `ReviewGateConflict`), `scenario_8_cancellation_during_pending_review` (cancelled token + PendingReview manifest must not deadlock), `approve_does_not_decrement_reject_counter` + `skip_does_not_decrement_reject_counter` (counter invariants for contract criterion #7) |
| `pice-cli/tests/evaluate_review_gate_pending.rs` (1 test) | Non-TTY/JSON exit 3 | `evaluate_json_mode_returns_review_gate_pending_exit_three` — seeds pending-review manifest via project hash, runs `pice evaluate --json`, asserts exit 3 + `status: "review-gate-pending"` + `pending_gates[0].layer`; uses `project_root.canonicalize()` for macOS `/var/folders/` symlink handling |
| `pice-cli/tests/audit_gates_csv_roundtrip.rs` (3 tests) | `pice audit gates` export | `csv_has_header_plus_three_data_rows`, `csv_filters_by_feature`, `json_mode_emits_decisions_array` — contract criterion #11 exportable audit trail |
| `pice-daemon/src/handlers/review_gate.rs::tests` (11 inline tests) | Decide handler unit coverage | `decide_approve_records_audit_and_updates_manifest`, `decide_skip_keeps_layer_passed_records_audit`, `decide_reject_with_retry_decrements_counter_layer_returns_pending`, `decide_reject_without_retry_halts_with_gate_rejected`, `decide_on_already_decided_gate_returns_review_gate_conflict`, `decide_unique_violation_on_gate_id_surfaces_as_conflict` (mismatched decisions → Conflict), `decide_same_decision_recovers_idempotently` (matching decisions → reuse prior audit_id, no duplicate insert), `decide_writes_audit_before_manifest_on_success`, `decide_audit_failure_does_not_mutate_manifest` (DB-open failure), `decide_audit_insert_failure_preserves_manifest_state` (chmod 0o444 mid-insert failure), `list_returns_pending_gates_across_features` |
| `pice-daemon/src/handlers/evaluate.rs::tests::evaluate_releases_locks_between_cohorts` (1 inline test) | Per-manifest lock release invariant | Exercises `DaemonContext::manifest_lock_for` with `tokio::time::timeout(250ms)` on sequential acquires; leaked guard would trip the timeout. Also asserts same `Arc<Mutex<_>>` for same `(namespace, feature_id)` — catches lock-identity regressions |
| `pice-daemon/src/handlers/audit.rs::tests` (~5 inline tests) | Audit RPC | Query filters (feature_id, since), CSV export shape, corrupt-DB surfaces as error (not empty result — Codex bug #4 regression guard), fresh-repo returns empty result |
| `pice-daemon/src/metrics/store.rs::tests` (Phase 6 additions: +8 tests) | gate_decisions SQLite surface | `insert_gate_decision` canonicalizes RFC3339 (`+00:00` → `Z`), UNIQUE(gate_id) surfaces as typed `DuplicateGateId`, `find_gate_decision_by_id` roundtrip (idempotent-recovery helper), `query_gate_decisions` ordering + filtering, `canonicalize_rfc3339_normalizes_plus_zero_to_z` + `canonicalize_rfc3339_passes_through_unparseable_with_warn` (Codex bug #5 regression guard), CHECK constraint on `decision` column |
| `pice-daemon/src/metrics/db.rs::tests` (Phase 6 additions: `migrate_from_v3_to_v4`, `migration_v4_is_idempotent_across_reopens`) | v4 SQLite migration | `gate_decisions` table created with full schema; `UNIQUE(gate_id)` + `CHECK(decision IN (...))` + `CHECK(elapsed_seconds >= 0)` constraints; indexes exist; idempotent across reopens |
| `pice-core/src/gate.rs::tests` (~18 inline tests) | Pure gate state-machine | `resolve_timeout_action_returns_none_when_status_not_pending` (Codex C3 decide/reap race), `apply_timeout_if_expired_*` (Approve/Reject/Skip branches), `from_audit_decision_string` roundtrip coverage for all 6 decision strings, `check_gates_for_cohort_with_matching_trigger_enqueues_gate_with_pinned_fields`, `check_gates_for_cohort_reuses_reject_counter_from_prior_gate` (Codex C6 persistence), `require_review_override_forces_gate_regardless_of_trigger_expression`, `check_gates_for_cohort_skips_non_passed_layers`, `new_gate_id_uniqueness_stress_16x128_threads` |
| `pice-core/src/layers/manifest.rs::tests` (Phase 6 additions: +4 tests) | Schema v0.2 → v0.3 migration | `load_accepts_v0_2_manifest_with_empty_gates_default` (soft migration in-memory upgrade), `save_always_writes_v0_3`, `schema_version_unknown_rejects_with_named_error` (typed `ManifestError::UnsupportedSchema`), `compute_overall_status_pending_review_wins_over_in_progress` (PendingReview > InProgress rule, Failed > PendingReview rule) |
| `pice-core/src/workflow/merge.rs::tests` (Phase 6 additions: `retry_on_reject` floor-merge tests) | `retry_on_reject` raise-only floor | User overlay can raise but not lower the project-committed reject budget; per-layer override floors to `max(project_review.retry_on_reject, project_layer.retry_on_reject)`; approve/skip don't trigger floor logic |
| `pice-daemon/src/clock.rs::tests` (Phase 6 new, 4 inline tests) | MockClock + Clock trait | `system_clock_now_returns_utc`, `mock_clock_advance_wakes_sleepers`, `mock_clock_set_jumps_time`, `mock_clock_trait_object_works` — `MockClock` gated `#[cfg(test)]` to keep `expect()` out of production code (clippy::expect_used deny) |
| `pice-cli/src/commands/evaluate.rs::tests` (Phase 6 additions) | TTY auto-resume loop | `is_review_gate_pending_detects_status_discriminant`, `extract_pending_gates_from_response` shape, CLI exits 1 after 10-iteration cap reached |
| `pice-cli/src/input/decision_source.rs::tests` (Phase 6, 2 inline tests) | `render_prompt` pure helper | `render_prompt_includes_details_when_provided`, `render_prompt_omits_detail_separator_when_none`. The original `DecisionSource` trait was Phase-6 scaffolding that `StdinLock: !Send` blocked from wiring into the async handler path; the Pass-3 review removed it along with the `Scripted`/`Piped`/`Tty` impl structs. Only the pure `render_prompt` helper survived — both production prompt call sites read stdin directly while using this helper for the box-drawing string |

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
- `packages/provider-claude-code/src/*.ts` — Codex SDK bridge
- `packages/provider-codex/src/*.ts` — Codex/OpenAI bridge
- `templates/pice/workflow.yaml` + `templates/pice/workflow-presets/*.yaml` — shipped defaults (capability-gate compatible)
- `scripts/acceptance/release-artifact-smoke.mjs` — release archive smoke runner, including Windows daemon-stop disconnect recovery and npm pack smoke path
- `Dockerfile.ci` + `scripts/ci/local-linux.sh` — local Linux CI-equivalent preflight with Node 22, pnpm 9, Rust stable, host-owned temporary dependency mounts, Phase 8 acceptance, and artifact smoke
- `scripts/ci/windows-smoke.ps1` + `.github/workflows/windows-smoke.yml` — Windows-specific pre-release smoke path for named pipes, `.cmd`, PowerShell, and path behavior
- `crates/pice-daemon/src/orchestrator/stack_loops.rs` — Phase 5 cohort parallel path: `MAX_PARALLELISM_HARD_CAP=16`, gate conjunction `parallel_configured && cohort_size>1 && max_parallelism>1`, `LayerInputs` owned-struct compile-time context-isolation boundary, `build_per_layer_inputs` single-threaded extractor, `tokio::JoinSet` + `Semaphore` dispatch, `biased` `tokio::select!` with `cancel_fired` gate, `CancellationToken` child-token propagation, `"cancelled:{pre_spawn,in_flight,join_aborted}"` halted_by markers, DAG-order manifest emission, `debug!(target: "pice.cohort", path=...)` gate observability
- `crates/pice-daemon/src/orchestrator/adaptive_loop.rs` — Phase 5 `PassMetricsSink: Send + Sync` trait with `&self` `record_pass`, `NullPassSink` stateless, `RecordingPassSink` with `Mutex<Vec<_>>` + poison-safe `rows()` reader, task-local `CostStats` (no shared aggregator)
- `crates/pice-daemon/src/provider/host.rs` — Phase 5 `tokio::process::Command::kill_on_drop(true)` on `ProviderHost::spawn` — load-bearing for zero-orphan-session invariant on cohort cancellation
- `crates/pice-daemon/src/metrics/store.rs` — Phase 5 `DbBackedPassSink` wrapping `Arc<Mutex<MetricsDb>>` for concurrent SQLite writes from parallel cohort tasks
- `crates/pice-core/src/workflow/schema.rs` — Phase 5 `EvaluatePhase.parallel: bool` with `#[serde(default = "default_evaluate_parallel")]` returning `true` (deny_unknown_fields closes the empty-evaluate-block regression)
- `crates/pice-daemon/benches/parallel_cohort_speedup.rs` — Phase 5 criterion bench (advisory only — no CI failure on regression)
- `crates/pice-daemon/Cargo.toml` — Phase 5 `[target.'cfg(unix)'.dev-dependencies] libc = "0.2"` for orphan-PID liveness probe in cancellation test; `tokio-util` with `rt` feature for `CancellationToken`
- `packages/provider-stub/src/deterministic.ts` — Phase 5 `perLayerScoreEnvName` + `PICE_STUB_SCORES_<LAYER>` per-layer isolation (disjoint score arrays, zero shared-iterator contention)
- `packages/provider-stub/src/index.ts` — Phase 5 `PICE_STUB_LATENCY_MS` real-clock setTimeout, `PICE_STUB_ALIVE_FILE` alive/done PID sentinel, `PICE_STUB_REQUEST_LOG` per-request JSONL capture
- `crates/pice-core/src/gate.rs` — Phase 6 pure gate state-machine: `GateDecision`, `GateDecisionOutcome` (with `from_audit_decision_string` reverse-parse for crash recovery), `resolve_timeout_action`, `apply_timeout_if_expired` (in-place mutator), `check_gates_for_cohort` (cohort-boundary firing + already-resolved skip), `effective_retry_on_reject`, `new_gate_id` (stress-tested for uniqueness)
- `crates/pice-core/src/layers/manifest.rs` — Phase 6 schema v0.3 with `gates: Vec<GateEntry>`, `GateStatus` (Pending/Approved/Rejected/Skipped/TimedOut), `LayerStatus::PendingReview`, `ManifestStatus::PendingReview`, `compute_overall_status` (PendingReview > InProgress, Failed > PendingReview), soft-migration v0.2 load with `gates: []` default, typed `ManifestError::UnsupportedSchema`
- `crates/pice-core/src/cli/mod.rs` — Phase 6 `CommandRequest::{ReviewGate, Audit}` variants, `ReviewGateRequest` + `ReviewGateSubcommand::{List, Decide}`, `AuditRequest` + `AuditSubcommand::Gates`, `GateDecideResponse` / `GateListEntry` / `GateListResponse` DTOs, `ExitJsonStatus` variants (`ReviewGatePending` exit 3, `ReviewGateRejected` exit 2, `ReviewGateConflict` exit 1) with `HALTED_GATE_REJECTED` / `HALTED_GATE_TIMEOUT_REJECT` prefix constants + `is_gate_halt()` helper
- `crates/pice-core/src/workflow/schema.rs` — Phase 6 `ReviewConfig.retry_on_reject: u32` raise-only floor, `LayerOverride.{require_review, retry_on_reject}` per-layer grants
- `crates/pice-core/src/workflow/merge.rs` — Phase 6 `retry_on_reject` raise-only floor-merge (user overlay can raise but never lower)
- `crates/pice-daemon/src/clock.rs` — Phase 6 `Clock` trait + `SystemClock` production impl + `MockClock` `#[cfg(test)]` test impl (gated to keep `.expect()` out of production per clippy::expect_used deny). Scaffolding for the Task 8 background reconciler
- `crates/pice-daemon/src/handlers/review_gate.rs` — Phase 6 `pice review-gate` handler: project-scoped `resolve_project_scoped_state_dir` (prevents cross-project gate mutation + split-brain audit), `run_list` (filters Pending gates in caller's project namespace), `run_decide` with audit-before-manifest ordering, idempotent crash-recovery via `find_gate_decision_by_id` (same-decision retry reuses prior audit_id; mismatched decision surfaces `ReviewGateConflict`), UNIQUE CAS race-loser re-fetch fallback
- `crates/pice-daemon/src/handlers/audit.rs` — Phase 6 `pice audit gates` handler: CSV/JSON export, feature_id + since filters, fresh-repo returns empty (missing DB file), corrupt-DB surfaces error (Codex bug #4 fix)
- `crates/pice-daemon/src/handlers/evaluate.rs` — Phase 6 `review_gate_pending_response` (exit 3 + `pending_gates[]` array), `reconcile_expired_gates_inline` (pure scan → audit first → mutate on success; partial Task 8), auto-resume short-circuit on existing PendingReview manifest (before `run_stack_loops`), PendingReview post-run routing to exit 3
- `crates/pice-daemon/src/orchestrator/stack_loops.rs` — Phase 6 resume-from-disk: `VerificationManifest::load(manifest_path)` on entry preserves decided layers (Passed/Failed/Skipped/PendingReview) + prior gates; per-cohort layer loop moves terminal/PendingReview entries into `immediate_results` unchanged, drops stale Pending/InProgress entries; cohort-boundary `check_gates_for_cohort` call with early-return on gate fire (natural lock release via handler return — the "release between cohorts" invariant implementation)
- `crates/pice-daemon/src/metrics/db.rs` — Phase 6 v4 migration adding `gate_decisions` table with `UNIQUE(gate_id)`, `CHECK(decision IN ('approve','reject','skip','timeout_approve','timeout_reject','timeout_skip'))`, `CHECK(elapsed_seconds >= 0)`, indexes on `(feature_id, decided_at)`; idempotent across reopens
- `crates/pice-daemon/src/metrics/store.rs` — Phase 6 `insert_gate_decision` with RFC3339 canonicalization at write boundary, typed `GateInsertError::{DuplicateGateId, Other}`, `find_gate_decision_by_id` (crash-recovery helper), `query_gate_decisions` with filters + LIMIT, `GateDecisionRecord` owned-row shape; `canonicalize_rfc3339` normalizes `+00:00`/`Z` → `Z` (Codex bug #5 fix)
- `crates/pice-cli/src/commands/review_gate.rs` — Phase 6 `pice review-gate` CLI (list/decide modes, TTY prompt via direct stdin reads due to `StdinLock: !Send`, `$USER`/`$USERNAME` reviewer fallback, MissingDecision exit)
- `crates/pice-cli/src/commands/audit.rs` — Phase 6 `pice audit gates` CLI (CSV/JSON output, feature_id + since filters)
- `crates/pice-cli/src/commands/evaluate.rs` — Phase 6 TTY auto-resume loop: detects exit-3 review-gate-pending, prompts via `DecisionSource`, re-invokes evaluate, bounded at 10 iterations
- `crates/pice-cli/src/input/decision_source.rs` — Phase 6 `render_prompt` pure helper for the Unicode box-drawing reviewer prompt (writes to stderr per the Channel ownership invariant). Earlier trait-based abstraction was removed after the Pass-3 review flagged it as unused scaffolding
- `crates/pice-daemon/src/test_support.rs` — Phase 6 `StateDirGuard` RAII helper for `PICE_STATE_DIR` mutation across the lib-test binary and integration-test binaries. Shared `pub` module so the struct definition can't drift; each binary gets its own static `Mutex<()>` via `OnceLock`
- `templates/pice/workflow.yaml` — Phase 6 `review.retry_on_reject` default + per-layer `require_review` examples

### Expected results

All tests should pass. Baseline: **1262 Rust tests (1 ignored doc-test in `crates/pice-daemon/src/handlers/mod.rs` line 5), 125 TypeScript tests, 0 lint errors, 0 warnings, clean release build.**

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

For CI, release, publishing, or release-smoke changes, also run the Linux CI-equivalent Docker preflight when Docker is available:

```bash
scripts/ci/local-linux.sh
```

Expected baseline: **1262 Rust tests passing (1 ignored doc-test in `crates/pice-daemon/src/handlers/mod.rs`), 125 TypeScript tests passing, 0 lint errors, 0 clippy warnings (workspace + lib unwrap/expect denies), clean release build.**

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
9. **Pattern violations** — check `AGENTS.md` and `.codex/rules/` (especially `daemon.md`, `stack-loops.md`, `workflow-yaml.md`, `metrics.md`, `protocol.md`) for project conventions
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

Phase 6 review gates:
  - review_gate_lifecycle_integration (11 tests — trigger fires, list/pinned fields, approve/reject/skip, retry cycle, concurrent decide, cancellation during PendingReview): ✓ / ✗
  - evaluate_review_gate_pending (1 test — JSON mode exit 3 with pending_gates payload): ✓ / ✗
  - audit_gates_csv_roundtrip (3 tests — CSV header+rows, filter, JSON mode): ✓ / ✗
  - handlers::review_gate::tests (11 inline tests — decide lifecycle, UNIQUE CAS, idempotent recovery, audit-insert-failure ordering): ✓ / ✗
  - handlers::evaluate::tests::evaluate_releases_locks_between_cohorts (1 test — per-manifest lock release via tokio timeout): ✓ / ✗
  - metrics/db::tests v4 migration (migrate_from_v3_to_v4 + idempotency-across-reopens): ✓ / ✗
  - metrics/store::tests gate_decisions (~8 tests — canonicalize_rfc3339, UNIQUE error typing, find_gate_decision_by_id roundtrip, query filters, CHECK constraint): ✓ / ✗
  - pice-core gate::tests (~18 tests — timeout resolution, check_gates_for_cohort cohort firing + already-resolved skip + reject-counter persistence, GateDecisionOutcome roundtrip): ✓ / ✗
  - pice-core manifest::tests schema-v0.3 (4 tests — soft-migration v0.2→v0.3, typed UnsupportedSchema, PendingReview overall-status precedence): ✓ / ✗
  - pice-core workflow/merge::tests retry_on_reject floor-merge: ✓ / ✗
  - clock.rs inline tests (MockClock gated `#[cfg(test)]`): ✓ / ✗

Release/CI tripwires:
  - release-workflow-policy (6 tests — NPM publish gate + tag/package alignment): ✓ / ✗
  - release-artifact-smoke unit tests (3 tests — Windows pipe disconnect recovery): ✓ / ✗
  - local-ci-policy (4 tests — Dockerfile/local Linux/Windows smoke invariants): ✓ / ✗
  - local Linux Docker preflight for CI/release changes: ✓ / ✗ / N/A

Full Suite: 1262 / 123 tests passing
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

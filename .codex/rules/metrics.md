---
paths:
  - "crates/pice-cli/src/metrics/**"
  - "crates/pice-cli/src/commands/metrics.rs"
  - "crates/pice-cli/src/commands/benchmark.rs"
  - "crates/pice-daemon/src/state/**"
---

# Metrics & Telemetry Rules

## SQLite Schema

- Use `rusqlite` with WAL mode for concurrent read access
- Schema versioning via a `schema_version` table — check and migrate on startup
- Tables (v0.1): `evaluations`, `criteria_scores`, `loop_events`, `telemetry_queue`
- Tables (v0.2+): `gate_decisions`, `cost_events`, `seam_findings`, `layer_runs`
- Tables (v0.5+): `check_outcomes`, `model_predictions` (for predictive check selection)
- All timestamps are UTC ISO 8601 (RFC 3339)
- Multi-table inserts (e.g., evaluation + criteria_scores) must use `BEGIN TRANSACTION` / `COMMIT` / `ROLLBACK` to prevent orphaned rows
- Schema migrations are forward-only. Every new table or column gets a numbered migration. Never edit existing migration files.

## Passive Collection

Metrics are collected automatically by every orchestration command. No user action required:
- `pice plan` → records plan creation event
- `pice execute` → records execution start/complete
- `pice evaluate` → records per-criterion scores, pass/fail, tier, models used
- `pice commit` → records commit event linked to the plan

## Non-Fatal Recording Pattern

Workflow commands (evaluate, plan, execute, commit) must NEVER fail due to metrics errors. Use this pattern:

```rust
if let Ok(Some(db)) = metrics::open_metrics_db(&project_root) {
    if let Err(e) = metrics::store::record_loop_event(&db, ...) {
        tracing::warn!("failed to record event: {e}");
    }
}
```

- `open_metrics_db` returns `Ok(None)` when the DB file doesn't exist — the `if let` skips silently.
- `open_metrics_db` returns `Err(...)` for corrupt DBs — the `if let Ok(...)` skips silently.
- Recording errors use `tracing::warn!` (degraded behavior), telemetry errors use `tracing::debug!` (silent).
- **Reporting commands** (`pice metrics`, `pice benchmark`) MAY propagate DB errors — the user needs to know.

## Plan Path Normalization

Plan paths stored in metrics DB must be normalized to project-relative canonical form via `metrics::normalize_plan_path()`. This prevents history fragmentation from different path spellings (absolute, relative, `./`-prefixed).

- Always normalize before writing to `evaluations.plan_path` or `loop_events.plan_path`
- The canonical form is `.codex/plans/<filename>` — same format `pice status` uses for lookups
- Status enrichment queries use the same canonical form

## Telemetry

- **Opt-in only.** Default is `false`. Set via `pice init` prompt or `.pice/config.toml`.
- **Anonymized.** No code, file paths, project names, or user identifiers.
- **Transparent.** Every payload is logged to `.pice/telemetry-log.jsonl` before sending.
- **Inspectable.** `pice telemetry show` displays recent payloads.
- Telemetry endpoint failures are silent (logged to debug, never user-facing errors).

### HTTP Sending

Telemetry events are sent via HTTP POST using `telemetry::send_batch()` — the single implementation of the HTTP logic. Both the library/test path (`TelemetryClient::flush_inner()`) and the production path (`commands::evaluate::flush_telemetry()`) call this function.

- **Batch size:** Up to 50 pending events per flush
- **Timeout:** 10 seconds (`HTTP_TIMEOUT` constant)
- **TLS:** `reqwest` with `rustls-tls` (pure Rust, no OpenSSL dependency)
- **Production path:** `flush_telemetry()` reads pending events synchronously, then spawns a detached `tokio::spawn` for the HTTP POST so it never blocks CLI output. If the process exits before the spawn completes, unsent events stay in the SQLite queue and retry on the next `pice evaluate` invocation.
- **DB reopening:** The spawned task reopens `MetricsDb` to mark events as sent because `rusqlite::Connection` isn't `Sync` and can't cross the spawn boundary.

### Wire-Format Safety

Telemetry uses a separate `AnonymizedPayload` struct (not `TelemetryEvent` itself) as the wire format. The `anonymize()` function destructures `TelemetryEvent` exhaustively — adding a new field to `TelemetryEvent` causes a compile error, forcing an explicit decision about whether to include it in the wire format. This is a compile-time guarantee against accidental data leakage.

## Aggregation Queries

`pice metrics` runs aggregation queries against the local SQLite DB:
- Total loops, pass rate, average score, trend (last 30 days)
- Output as terminal table (default), JSON (`--json`), or CSV (`--csv`)
- CSV output uses RFC 4180 escaping (embedded quotes doubled, fields with commas/quotes/newlines wrapped)

## Init Behavior

- `pice init` creates a real SQLite DB with schema (not an empty file)
- `pice init --force` runs migrations on the existing DB — it NEVER deletes metrics history
- The DB path is resolved from `config.metrics.db_path`, not hardcoded

## v0.2+ Audit Trail

Every gate decision writes a row to `gate_decisions`:

```sql
CREATE TABLE gate_decisions (
    id INTEGER PRIMARY KEY,
    feature_id TEXT NOT NULL,
    layer TEXT NOT NULL,
    trigger_expression TEXT NOT NULL,
    decision TEXT NOT NULL,           -- approve | reject | skip | timeout_reject | timeout_approve | timeout_skip
    reviewer TEXT,                    -- $USER by default, or dashboard authenticated user
    reason TEXT,                      -- optional free text
    requested_at TEXT NOT NULL,       -- RFC 3339
    decided_at TEXT NOT NULL,
    elapsed_seconds INTEGER NOT NULL
);
```

- **Write-only from the caller's perspective.** The daemon INSERTs; nothing UPDATEs rows. A changed decision creates a new row (linked by `feature_id` + `layer`).
- **Deletion is explicit** via `pice audit prune --before DATE`. Default retention is 365 days via `[audit] retention_days` in `config.toml`.
- **Reviewer identity** is `$USER` for CLI-actioned gates. For dashboard gates, the reviewer comes from the dashboard session's token → user mapping (v0.3).
- Reporting surface: `pice audit gates [--feature F] [--since DATE]`.

### Phase 6 invariants (v0.6 / Phase 6)

These sharpen the audit-trail semantics after the Phase 6 eval cycle exposed two silent-corruption paths. Read before touching `crates/pice-daemon/src/metrics/store.rs::insert_gate_decision` or the `ReviewGate::Decide` handler.

- **v4 migration adds `UNIQUE(gate_id)` + CHECK constraints.** `gate_decisions.gate_id TEXT NOT NULL UNIQUE` is the CAS primitive for concurrent decide (no in-process lock needed — SQLite surfaces the race as `DuplicateGateId` at the first writer's `COMMIT`). The `decision` column carries `CHECK(decision IN ('approve','reject','skip','timeout_approve','timeout_reject','timeout_skip'))` — the six-value whitelist is LOAD-BEARING for `pice_core::gate::GateDecisionOutcome::from_audit_decision_string`'s reverse-parse during crash recovery. Any new decision family (e.g. future `dashboard_approve`) must migrate the CHECK constraint in the SAME PR that adds the `audit_decision_string` match arm. `CHECK(elapsed_seconds >= 0)` prevents a clock-skewed reviewer write from poisoning aggregation.
- **RFC3339 canonicalization at the write boundary.** `canonicalize_rfc3339` in `metrics/store.rs` normalizes `+00:00` → `Z` (UTC, `SecondsFormat::Secs`) at BOTH `insert_gate_decision` time AND `query_gate_decisions` `since` filter time. Storing timestamps as raw TEXT with mixed encodings breaks lexicographic `ORDER BY requested_at` and `WHERE requested_at >= ?` — the same instant sorts before or after depending on how the writer emitted it. An unparseable timestamp falls back to verbatim storage with a `tracing::warn!` — CHECK constraints are NOT NULL only (not format-specific), so we don't reject legacy rows. Pinned by `canonicalize_rfc3339_normalizes_plus_zero_to_z`, `canonicalize_rfc3339_passes_through_unparseable_with_warn`, and `query_gate_decisions_since_matches_mixed_utc_encodings`.
- **Idempotent crash-recovery on UNIQUE violation — decision-match required.** If a prior decide succeeded at the audit `INSERT` but crashed before the manifest save landed, the next decide attempt on the same `gate_id` hits the UNIQUE violation. The handler recovers via `find_gate_decision_by_id` (see `metrics/store.rs`): it fetches the durable audit row, parses its `decision` via `GateDecisionOutcome::from_audit_decision_string`, and — IFF the caller's requested decision matches the stored one — completes the manifest mutation with the SAME decision (reusing the prior `audit_id`, no duplicate row). If decisions MISMATCH (two concurrent actors with different intents), the handler surfaces `ReviewGateConflict` instead of silently overwriting the caller's intent. This preserves contract criterion #10 (UNIQUE CAS) while making the decide flow crash-recoverable. Pinned by `decide_same_decision_recovers_idempotently` + `decide_unique_violation_on_gate_id_surfaces_as_conflict`.
- **Audit-before-manifest ordering is enforced in TWO paths — decide handler AND reconciler.** Both `run_decide` and the inline `reconcile_expired_gates_inline` (evaluate handler's PendingReview short-circuit) write the `gate_decisions` row BEFORE any in-memory manifest mutation. The reconciler uses a 3-phase pattern: (1) pure scan collects `{idx, outcome, requested_at, layer, gate_id, trigger}` actions without touching `manifest.gates`, (2) audit insert per action — on failure, skip the mutation and keep the gate `Pending` for the next reconcile, (3) mutate the in-memory manifest and layer. This closes the Pass-2 regression where `apply_timeout_if_expired` mutated gate fields in-place before the audit row landed. Pinned by `decide_writes_audit_before_manifest_on_success`, `decide_audit_failure_does_not_mutate_manifest`, and `decide_audit_insert_failure_preserves_manifest_state` (chmod 0o444 read-only DB mid-insert test).
- **`find_gate_decision_by_id` is the only crash-recovery primitive.** Callers MUST go through this function rather than hand-rolling their own `SELECT` — the helper does the `LIMIT 1` pagination, plumbs `rusqlite::Error` through `anyhow::Context`, and returns `Ok(None)` for the distinct "no row exists yet" case vs surfacing other errors. A future audit-query helper that bypasses it would risk diverging on edge cases (multi-row `gate_id` shouldn't happen, but if a migration bug ever produced one, `LIMIT 1` is the safer default).

## v0.2+ Cost Tracking

Every provider pass writes a row to `cost_events`:

```sql
CREATE TABLE cost_events (
    id INTEGER PRIMARY KEY,
    feature_id TEXT NOT NULL,
    layer TEXT,                   -- NULL for feature-level events
    pass_index INTEGER,           -- NULL for non-evaluation events
    provider TEXT NOT NULL,       -- "claude-code" | "codex" | ...
    model TEXT NOT NULL,
    cost_usd REAL NOT NULL,
    tokens_input INTEGER,
    tokens_output INTEGER,
    timestamp TEXT NOT NULL
);
```

- Cost is attributed to the layer and pass that incurred it.
- `pice metrics cost [--by-day|--by-feature|--by-layer]` reports aggregated spend.
- Budget enforcement reads from this table in real time — if adding the next projected pass would exceed `workflow.defaults.budget_usd`, the adaptive controller halts with `halted_by: budget`.
- The non-fatal recording pattern applies here too: a cost_events write failure must NOT abort the evaluation. Log at `warn` and continue.

## v0.2+ Layer Runs and Seam Findings

`layer_runs` captures per-layer execution summary:

```sql
CREATE TABLE layer_runs (
    id INTEGER PRIMARY KEY,
    feature_id TEXT NOT NULL,
    layer TEXT NOT NULL,
    status TEXT NOT NULL,          -- running | passed | failed | skipped | pending_review
    contract_tier INTEGER NOT NULL,
    passes INTEGER NOT NULL,
    final_confidence REAL,
    total_cost_usd REAL NOT NULL,
    halted_by TEXT,                -- sprt_confidence_reached | sprt_rejected | budget | max_passes | vec_entropy | gate_rejected
    started_at TEXT NOT NULL,
    completed_at TEXT
);
```

`seam_findings` captures per-boundary seam check results:

```sql
CREATE TABLE seam_findings (
    id INTEGER PRIMARY KEY,
    feature_id TEXT NOT NULL,
    boundary TEXT NOT NULL,         -- "backend↔database", "api↔frontend", ...
    check_id TEXT NOT NULL,         -- "schema_match", "openapi_compliance", ...
    severity TEXT NOT NULL,         -- pass | warn | fail
    category INTEGER NOT NULL,      -- 1–12 from the 12 seam failure categories
    details TEXT,
    timestamp TEXT NOT NULL
);
```

These tables are populated by the daemon. The CLI reads from them for `pice status` and `pice metrics`. Dashboard (v0.3) reads the same tables.

## v0.4+ Adaptive Evaluation

Phase 4 adds the `pass_events` table (one row per provider invocation) and five new columns on `evaluations` (post-loop adaptive summary). Schema migrations are guarded by `PRAGMA table_info` so they're idempotent across daemon restarts.

```sql
-- New columns added to existing evaluations table
ALTER TABLE evaluations ADD COLUMN passes_used         INTEGER;
ALTER TABLE evaluations ADD COLUMN halted_by           TEXT;
ALTER TABLE evaluations ADD COLUMN adaptive_algorithm  TEXT;
ALTER TABLE evaluations ADD COLUMN final_confidence    REAL;
ALTER TABLE evaluations ADD COLUMN final_total_cost_usd REAL;

-- New table: per-pass audit trail
CREATE TABLE pass_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    evaluation_id INTEGER NOT NULL,
    pass_index INTEGER NOT NULL,            -- 1-indexed within the layer's loop
    model TEXT NOT NULL,                    -- "stub-echo" | "claude-opus-4-7" | ...
    score REAL,                             -- nullable: provider may not score
    cost_usd REAL,                          -- nullable: provider may omit cost
    timestamp TEXT NOT NULL,                -- ISO 8601
    FOREIGN KEY (evaluation_id) REFERENCES evaluations(id) ON DELETE CASCADE
);
```

### Write-path invariants

- **Header insert before the loop, finalize after.** `insert_evaluation_header` writes a placeholder row with `passed=0, summary=NULL` so the adaptive loop has a valid `evaluation_id` to FK-attach pass_events to. `finalize_evaluation` rewrites `passed` and `summary` after the loop returns.
- **`pass_events` writes happen BEFORE the halt-decision check.** A budget-halted loop still records the triggering pass cost. Skipping this would silently undercount on every budget halt.
- **`update_evaluation_adaptive_summary` aggregates across layers.** `passes_used = SUM(layer.passes.len())`, `final_total_cost_usd = SUM(layer.total_cost_usd)` (so `evaluations.final_total_cost_usd` matches `SUM(pass_events.cost_usd)` per evaluation_id within 1e-9).
- **`MetricsDb` is `!Sync`.** The rusqlite Connection's prepared-statement cache uses `RefCell`. The daemon wraps it in `Arc<Mutex<MetricsDb>>` so the per-pass sink can stay `Send` across the orchestrator's `await`. No contention because the sink is the only writer during the loop.
- **`PRAGMA foreign_keys = ON` is set on every connection.** Required for `ON DELETE CASCADE` to fire when an evaluation row is deleted (e.g., during retention-policy GC).
- **Concurrent isolation.** Two concurrent evaluations on different features write to disjoint `evaluation_id` groups in the same DB. There is no shared lock between evaluations beyond per-DB serialization (Phase 4 contract criterion #17).

### Capability declaration is the source of truth for cost (Phase 4.1)

When a provider does NOT declare `costTelemetry` in its `ProviderCapabilities`, the orchestrator MUST ignore any numeric `cost_usd` it reports — even if the value is well-formed (e.g., `Some(0.0)`). Declaring the capability is what asserts the value is real; without it, the value is meaningless instrument output.

The adaptive loop carries `AdaptiveContext.cost_telemetry_available: bool` (sourced from `primary.capabilities().cost_telemetry` in `stack_loops::try_run_layer_adaptive`). The cost-resolution branch in `adaptive_loop::run_adaptive_passes` is a **three-outcome match**, with no "synthetic seed" fallback:

```rust
let (debited_cost, observed_cost): (Option<f64>, f64) = match primary_cost {
    // 1. Real cost: provider declared telemetry AND value is non-negative.
    Some(c) if CostStats::validate_nonnegative(c).is_ok() && ctx.cost_telemetry_available => {
        any_real_cost_observed = true;
        (Some(c), c)
    }
    // 2. Fail closed: budget enforced but provider lacks usable telemetry.
    _ if ctx.budget_usd > 0.0 => {
        halted_by_runtime_error = Some(format!("runtime_error:invalid_cost_usd:..."));
        break;
    }
    // 3. Persist NULL: telemetry off AND no budget enforcement.
    _ => (None, 0.0),
};
```

- `pass_events.cost_usd` is `Option<f64>` and lands in SQLite as NULL via `rusqlite::params!` for outcomes 2-via-break-skipped and 3.
- `LayerResult.total_cost_usd` is `Option<f64>` and is `None` when `passes.is_empty() || !any_real_cost_observed`. The single `any_real_cost_observed` boolean covers both "no passes ran" and "passes ran but no real cost observed" — do NOT reintroduce a `telemetry_unmeasured` flag or any per-pass synthetic seed (see Pass-11.1 W1).
- The reconciliation invariant `evaluations.final_total_cost_usd == SUM(pass_events.cost_usd)` continues to hold under SQLite NULL semantics: `SUM(NULL, NULL, ...) = NULL`. The aggregator uses COALESCE only to default missing layer columns to `0.0` for display.
- Adversarial path inherits the primary's `cost_telemetry_available` value as a Phase 4.1 simplification. When per-provider telemetry diverges (one declares, the other does not), redesign as `cost_telemetry_per_provider: HashMap<String, bool>` — see `TODO(adts-v2)` in `adaptive_loop.rs`.

### Mid-loop sink failures route to Pending, not Failed (Phase 4.1)

A sink-write failure during the loop is an **operational** failure (we cannot trust persisted metrics), not a **contract** failure (the provider scored fine). The adaptive loop signals this with a distinct `halted_by` prefix:

- `metrics_persist_failed:` — operational. `LayerStatus::Pending`, exit code 1, `ExitJsonStatus::MetricsPersistFailed`. The evaluation can be retried.
- `runtime_error:` — contract/data failure. `LayerStatus::Failed`, exit code 2, `ExitJsonStatus::EvaluationFailed`.

Both prefixes flow through the same `halted_by_runtime_error: Option<String>` field on the loop result. The status-mapping site in `stack_loops::build_adaptive_layer_result` discriminates them — and **MUST** discriminate them via the centralized helper, never via inline string match:

```rust
match halted_by.as_deref() {
    // Operational arm MUST come first; runtime_error is the catch-all.
    Some(reason) if pice_core::cli::ExitJsonStatus::is_metrics_persist_failed(reason) => {
        LayerStatus::Pending
    }
    Some(reason) if reason.starts_with("runtime_error:") => LayerStatus::Failed,
    _ => LayerStatus::Passed,
}
```

A typo at any one of the three call sites (loop construction, status mapping, evaluate handler) would silently misroute exit codes. The unit test `metrics_persist_failed_prefix_helper_agrees_with_constant` in `pice-core::cli` locks the helper to the constant at compile/test time. When adding a new structured halted_by prefix, follow the same pattern: add a const + `is_*` helper to `ExitJsonStatus`, route every site through the helper, and add a parity unit test.

## v0.5 Predictive Selection Data

When v0.5 ships, `check_outcomes` captures the label needed for model training:

```sql
CREATE TABLE check_outcomes (
    id INTEGER PRIMARY KEY,
    feature_id TEXT NOT NULL,
    check_id TEXT NOT NULL,
    fired BOOLEAN NOT NULL,         -- did the check flag an issue?
    true_positive BOOLEAN,          -- was the flag a real bug? NULL until labeled
    labeled_at TEXT,
    labeled_by TEXT,                -- "developer" | "inferred"
    cost_usd REAL NOT NULL,
    timestamp TEXT NOT NULL
);
```

Labels come from `pice feedback {true-positive|false-positive}` (explicit) or inferred from post-evaluation commit patterns (heuristic). The `pice model train` command consumes this table to train the predictive check selector.

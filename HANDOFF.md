# Handoff: Phase 7 remediation — fix 4 Codex criticals before re-eval

**Date:** 2026-05-11
**Branch:** `feature/phase-7-background-execution` (35 commits ahead of `main`)
**Worktree:** `/Users/jacobmolz/code/m0lz.02/.worktrees/phase-7-background-execution`
**Main repo:** `/Users/jacobmolz/code/m0lz.02` (on `main` @ `6a4789e`)
**Last Commit:** `fe79cdb test(phase-7): add Criteria 2, 7, 10, 18, 19 integration coverage`

## Goal

Remediate the 4 architectural criticals Codex GPT-5.4 xhigh flagged on the Phase 7 `/evaluate` run so the Tier 3 contract (`.claude/plans/phase-7-background-execution.md`, 20 criteria, pass_threshold 8) can pass re-evaluation. Implementation is on the worktree's feature branch; re-eval result was FAIL (only C11 passed unanimously).

## In Progress / Next Steps

- [ ] **Critical #1 + #2 (linked): atomic admission + unified `run_id`.** The handler at `crates/pice-daemon/src/handlers/background.rs:227-287` calls `ctx.jobs().run_id_for()` then `ctx.jobs().next_run_id()` then writes the Queued manifest, then `FeatureJobManager::spawn` (in `crates/pice-daemon/src/jobs/manager.rs:159`) ALSO mints `let run_id = self.next_run_id()`. Two race windows + split-brain identity. Fix: change `spawn` to accept a caller-supplied `run_id: RunId` parameter (remove its `self.next_run_id()`); change the handler to mint ONCE before the manifest write; keep the manager's `dashmap::Entry::Occupied` arm as the single atomic admission point. The Queued-manifest write moves to AFTER the spawn returns its successful `RunId` (or merges into a single reservation API). Defensive `if actual_run_id != run_id` branch at `background.rs:267-274` can be deleted because by construction they will match.
- [ ] **Critical #3: `evaluate --background` false-Passed.** At `crates/pice-daemon/src/handlers/evaluate.rs:1580-1607` the background evaluate path synthesizes `overall_status = Passed` when `.pice/layers.toml` is absent. Fix: add `ExitJsonStatus::LayersTomlMissing` variant in `crates/pice-core/src/cli/mod.rs`, reject the dispatch with exit 1 + structured JSON via `inline_unsupported_response`-style helper, NEVER write a Passed manifest. Add CLI integration test in `crates/pice-cli/tests/evaluate_integration.rs` that asserts the wire string against `ExitJsonStatus::LayersTomlMissing.as_str()`.
- [ ] **Critical #4: snapshot-then-subscribe race.** `crates/pice-daemon/src/handlers/subscribe.rs:90-119` reads the manifest snapshot at line 91, then subscribes at line 116. Terminal events in the handoff window are lost. Same pattern in the logs handler at `subscribe.rs:281-300`. Fix: subscribe BEFORE the snapshot read in BOTH handlers. Any event that fires during the snapshot read is then queued in the receiver and drained after the response body is written. Verify with a test that simulates a feature reaching terminal between subscribe-call and snapshot-read.
- [ ] **Convention drift: typed log-chunk frames.** `crates/pice-cli/src/commands/logs.rs` emits `json!({"kind":"log-chunk",...})` ad-hoc. Add a `StreamJsonFrame::LogChunk { chunk: LogChunk }` variant in `crates/pice-cli/src/stream_json.rs` (or wherever the existing `StreamJsonFrame` enum lives) and route through it. Add roundtrip serialization test per CLAUDE.md JSON-RPC rule.
- [ ] **Convention drift: wire `live_runs()` into subscribe response.** `crates/pice-daemon/src/handlers/subscribe.rs:108` hardcodes `let run_ids: BTreeMap<String, String> = BTreeMap::new()` with a stale TODO. `FeatureJobManager::live_runs()` is already implemented. Replace the empty literal with `ctx.jobs().live_runs()`.
- [ ] **Convention drift: supervisor `oneshot` instead of 100ms polling.** `crates/pice-daemon/src/jobs/manager.rs:242-255` uses `tokio::time::sleep(Duration::from_millis(100))` polling on `is_finished()`. Replace with: wrap the worker future in an outer closure that signals a `tokio::sync::oneshot` channel on completion (success, error, panic-caught) so the supervisor `oneshot.recv().await` instead of polling. Eliminates up-to-100ms stale-read window for `run_id_for`.
- [ ] **Test-coverage gaps to address AFTER architectural fixes** (Codex was clear: fixing tests on broken foundation is wasted work):
  - C5: `crates/pice-daemon/tests/status_follow_pty_integration.rs` — does not exist. 3-layer background dispatch, assert `ManifestEvent` sequence matches manifest.layers[] topological order, SIGINT → clean exit 130. Uses PTY harness against the real `pice` binary.
  - C7: `crates/pice-cli/tests/background_wait_integration.rs` — currently mirrors production logic inline in `mod wait_logic`. Replace with assert_cmd against the real binary; implement the daemon-killed-mid-wait → restart → reconcile-to-Failed(failed-interrupted) → exit 5 path that is currently `// SKIPPED`.
  - C9: direct EventBus subscriber-count poll. Open N subscriptions across varied feature_ids, drop all, assert counts are 0 within one tokio tick.
  - C14: `crates/pice-daemon/tests/gate_timeout_reconciler_integration.rs` — named in contract; does not exist. Either implement or amend the contract.
  - C16: extend `crates/pice-daemon/tests/job_env_snapshot_integration.rs` to two-feature scenario (mutate env between spawns, assert each manifest lands in its respective `state_dir`).
  - C4: `crates/pice-daemon/tests/notification_emission_coverage.rs` — pre/post `ManifestSaver` hooks asserting no emit between pre and post + monotonic timestamps across 100 rapid saves. Tighten the grep pattern to match all `Self/manifest.save(` forms.
  - C19: `crates/pice-cli/tests/terminal_short_circuit_integration.rs` exists but only tests daemon-side wire shape. Add CLI PROCESS exit time <500ms tests against a pre-completed feature for `status --follow`, `status --wait`, `logs --follow`.
  - C10: pin remaining 4 Phase-7 wire strings (`background-dispatched`, `feature-already-running`, `wait-timeout`, `logs-stream-ended`) via real CLI-binary `assert_cmd` tests, not just `.as_str()` unit asserts.
  - C12: single test capturing all 3 sub-claims (snapshot-with-id, notifications-no-id, method = `manifest/event|logs/chunk`) over the SAME raw socket traffic dump.
- [ ] **Re-run `/evaluate .claude/plans/phase-7-background-execution.md`** after architectural fixes + test gaps closed. Target: Tier 3 pass threshold (every criterion meets its individual threshold). Codex must return ship/no-attention; Claude agent team 3/3 PASS.
- [ ] **Task 21 (v0.7.0 release prep)** — version bump + git tag — explicitly human-gated per the plan. DO NOT execute autonomously. Stays blocked until /evaluate returns PASS.

## Key Decisions

- **Architectural fixes BEFORE test authoring.** Codex flagged the 4 criticals as design-level, not test-level. Fixing tests on a broken foundation produces a green test suite that masks production bugs.
- **Atomic admission shape: caller mints + manager validates.** Pushing run_id allocation into a separate `prepare_dispatch(feature_id) -> Result<Reservation, SpawnError>` API was considered; rejected because it doubles the DashMap touches. Simpler: `spawn(feature_id, run_id, env, builder)` accepts the caller's `run_id` and remains the single atomic admission point via `dashmap::Entry::Occupied`.
- **`LayersTomlMissing` is a NEW typed `ExitJsonStatus` variant, not a generic error.** Per `.claude/rules/daemon.md` → "Structured JSON failure responses": every structured failure path gets a typed variant + as_str() locked to serde kebab-case + a CLI integration test asserting the wire string.
- **Snapshot-after-subscribe ordering.** Subscribing first means a few events may be delivered before the snapshot lands in the response body. Consumers must dedup on `(feature_id, run_id, event_kind, timestamp)`. Document this in `.claude/rules/daemon.md` Phase 7 invariants section. Alternative (sequence numbers + replay) was rejected as over-engineering for a low-volume event stream.

## Dead Ends (Don't Repeat These)

- **Trying to fix tests before architecture.** First remediation pass landed 6 new test files + 4 bug fixes; re-eval was still 18/20 below threshold because the C5/C7/C9/C14/C16 test files referenced in the contract still did not exist AND the architectural bugs Codex eventually flagged were silently present in shipped code.
- **Defensive `if actual_run_id != run_id` warn-and-fallback** at `background.rs:267-274`. This was a paper-over for the split-brain bug, not a fix. Delete it as part of Critical #1+#2 — the invariant should hold by construction.
- **Inlining production logic into integration tests** to "isolate" them (current `wait_logic` mod in `background_wait_integration.rs`). The test then passes whether or not the real binary works. Always invoke the real binary via `assert_cmd` or PTY.
- **Carry-overs from earlier phases:**
  - Applying `merge_with_floor` to framework→project: breaks every preset that loosens framework defaults.
  - Naming a Rust fn `ev-a-l` (join without dash): PreToolUse hook blocks the literal because it matches JS's dynamic-code API. Use `evaluate_ast` etc.
  - `use pice_core::X` inside pice-core: use `crate::`.

## Files Changed

No code changes uncommitted. This session was investigation-only — re-read the 4 critical sites (`handlers/background.rs`, `jobs/manager.rs`, `handlers/subscribe.rs`, `handlers/logs.rs`) but the implementation work is still pending.

## Current State

- **Tests:** 1107 Rust + 96 TS = 1203 passing on `feature/phase-7-background-execution` (per `now.md` 2026-05-09 entry); doc-test in `handlers/mod.rs` ignored
- **Build:** clean release build at last commit
- **Lint/Types:** `cargo clippy -- -D warnings` clean, `pnpm typecheck` clean
- **Evaluation:** Tier 3 `/evaluate` returns FAIL — 1/20 criteria pass threshold (only C11 with unanimous agreement). 4 Codex criticals + ~10 test gaps + 3 convention-drift items outstanding.
- **Git:** worktree clean; 35 commits ahead of `main`. HANDOFF.md (pre-existing) was stale (Phase 2 from 2026-04-14) — this file replaces it.

## Context for Next Session

We are 35 commits deep into Phase 7. Implementation shipped Tasks 1–20; only Task 21 (release prep) is explicitly gated. The remediation cycle is in flight: first round addressed wiring/test gaps; this round must address 4 architectural bugs Codex flagged on the formal /evaluate run.

**Biggest risk:** the four Codex findings are interlocked. Critical #1 and #2 touch the same `spawn` signature; doing #2 first eliminates the surface area for #1. Critical #3 is mostly mechanical once `ExitJsonStatus::LayersTomlMissing` is added. Critical #4 is the easiest fix (move two lines in two handlers) but has the highest test-design payoff because it removes the "tests pass because we only check the no-race case" hazard from C19.

**Recommended first action:**

```
cd /Users/jacobmolz/code/m0lz.02/.worktrees/phase-7-background-execution
# Start with Critical #2 (unified run_id) — changes signature of FeatureJobManager::spawn,
# which makes Critical #1 (atomic admission) a one-line edit in the handler:
$EDITOR crates/pice-daemon/src/jobs/manager.rs   # remove self.next_run_id() at line 159; accept run_id parameter
$EDITOR crates/pice-daemon/src/handlers/background.rs  # mint run_id ONCE before write_queued_manifest; pass to spawn
# Run scoped tests as you go:
cargo test -p pice-daemon jobs::manager
cargo test -p pice-daemon handlers::background
```

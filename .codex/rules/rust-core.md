---
paths:
  - "crates/**"
  - "Cargo.toml"
  - "Cargo.lock"
---

# Rust Core Rules

## Crate Organization

### v0.1 (historical — shipped, now superseded by v0.2)

- `pice-cli` — monolithic binary crate. Owned everything: state machine, provider host, metrics, templates.
- `pice-protocol` — library crate, zero external dependencies beyond serde. Shared contract types for core↔provider JSON-RPC.

### v0.2 (current)

- `pice-cli` — thin CLI adapter binary. Owns: arg parsing (clap), config discovery + validation, terminal rendering, desktop notifications, keyboard input for gate prompts, shell completions. Dispatches everything else to the daemon over a Unix socket / named pipe.
- `pice-daemon` — long-running daemon binary. Owns: orchestrator (Stack Loops engine, adaptive algorithms, gate state manager, worktree lifecycle), provider process host (moved from cli), manifest CRUD, SQLite writes, daemon RPC server.
- `pice-core` — shared library crate. Owns: config parsing (TOML + YAML), layer detection + `layers.toml` types, workflow.yaml types + validation, verification manifest schema + helpers, seam check trait + default library, adaptive algorithms (SPRT/ADTS/VEC as pure functions), daemon RPC types. Zero async dependencies, zero network. Pure logic + data types. Both CLI and daemon depend on it.
- `pice-protocol` — unchanged. Still the shared contract for core↔provider JSON-RPC. Do NOT put daemon RPC types here; use `pice-core::protocol`.

**Crate boundary rule**: if the CLI needs to preview something the daemon will execute (config parse, workflow validation, layer detection dry-run), the logic lives in `pice-core`. Both sides import from there. Never duplicate parsing or validation between `pice-cli` and `pice-daemon` — divergence is a bug.

## Error Handling

- `pice-protocol`: Use `thiserror` for all error types. Every error variant has a human-readable message.
- `pice-cli`: Use `anyhow::Result` at the command handler level. Convert `pice-protocol` errors into user-facing messages.
- Never `unwrap()` or `expect()` in non-test code. Use `?` operator everywhere.

## Async

- All provider communication is async (tokio). Commands that launch providers must be `async fn`.
- `pice evaluate` launches multiple providers in parallel using `tokio::join!` or `tokio::select!`.
- Set timeouts on all provider communication. A hung provider must not block the CLI.

## State Machine

- The PICE loop state is managed in `engine/`. States: `Idle`, `Planning`, `Executing`, `Evaluating`, `Reviewing`.
- State transitions are explicit. Never skip states (e.g., no executing without a plan file).
- `pice status` derives state from filesystem scanning (plan files, git status) enriched with metrics DB lookups (latest evaluation per plan). A formal `.pice/state.json` for state transitions was deferred — filesystem + metrics DB remains sufficient.

## Binary Embedding

- Template files from `templates/` are embedded using `rust-embed` in `pice-daemon/src/templates/mod.rs`. The CLI no longer embeds templates — the daemon owns all template extraction (init handler).
- Test that embedded templates match the actual template files in CI.

## Provider Resolution

- `ProviderHost::spawn(command, args)` launches a provider as a tokio child process.
- `registry::resolve(name, config)` maps provider names to commands. It locates provider binaries by walking up from the pice binary's own location looking for `packages/`.
- Notifications received during `request()` are forwarded to an optional `NotificationHandler` callback (set via `on_notification()`). Phase 2 streaming depends on this.
- `ProviderHost::shutdown(timeout)` splits the timeout budget: `min(timeout, 5s)` for the shutdown RPC, remainder for process exit wait.

## Session Runner

- `pice-daemon/src/orchestrator/session.rs` provides `run_session()` and `run_session_and_capture()`. All provider-backed handlers use these — never duplicate the session lifecycle.
- `streaming_handler()` creates the standard notification handler for text-mode streaming. Use it instead of inline closures. **Never install the streaming handler when `req.json` is true** — it writes chunks to stdout that corrupt JSON output.
- The always-shutdown pattern: `let result = session::run_session(...); orchestrator.shutdown(); result?;` — the provider shuts down even on failure.
- The `to_shared_sink()` bridge in `handlers/mod.rs` converts `&dyn StreamSink` to `SharedSink` (`Arc<dyn StreamSink>`) via unsafe raw pointer transmute. Every call site MUST have a `// SAFETY INVARIANT:` comment documenting that the session is awaited to completion before the handler returns.

## Contract Parsing

- `plan_parser.rs` detects `## Contract` headings using line-level matching (`find_h2_heading`), not substring search.
- Only level-2 headings (`##`) are matched. `###` and deeper headings are rejected. Up to 3 leading spaces are allowed per CommonMark.
- If `## Contract` exists but has no ` ```json ` fence, the parser returns an error (not `Ok(None)`). Half-written contracts must be surfaced, not silently ignored.
- `status.rs` includes malformed plans in output with a `parse_error` field rather than silently dropping them.

## CLI Conventions

- Use `clap` derive macros for arg parsing.
- Every command has a `--json` flag for machine-readable output. When `--json` is active, suppress `println!` messages and emit a single JSON object to stdout. In JSON mode, capture/suppress subprocess stdout (use `output()` not `status()`) to keep stdout as valid JSON.
- Exit codes: 0 = success, 1 = failure, 2 = evaluation failed (contract criteria not met).
- JSON-mode failure responses use `CommandResponse::ExitJson { code, value }`, not `Exit { message: <stringified json> }`. See `.codex/rules/daemon.md` → "Structured JSON failure responses".
- Phase-N scaffolding uses `#[allow(dead_code)]` with a `///` doc comment explaining which phase uses the code.

## Schema Hardening

- **Every serde-derived config struct that represents a user-editable file MUST carry `#[serde(deny_unknown_fields)]`.** TOML and YAML readers silently drop unknown keys by default — a renamed or deprecated field in a stale config will then be silently ignored at runtime. This class of bug is invisible from the user's perspective: the workflow "runs" but respects no override. `deny_unknown_fields` converts that into a parse error with the bad key name.
- The rule applies to: `pice-core::config::PiceConfig`, `pice-core::layers::LayersConfig`, all of `pice-core::workflow::schema::*`, and any future `.pice/*.{toml,yaml}` schema types. It does NOT apply to internal-only types (JSON-RPC wire types, manifest records that may be forward-extended) where unknown fields are expected during version drift.
- Add a test that asserts a stale/misspelled top-level field produces a parse error whose message names the bad field. See `crates/pice-core/src/workflow/loader.rs::load_project_rejects_unknown_top_level_fields` for the pattern.

## Centralize cross-crate string prefixes behind a const + helper

When a string literal (e.g., a `halted_by` prefix, a status discriminant) is consumed by **2 or more sites** AND a typo would cause a silent semantic divergence (e.g., misrouted exit code, missed status mapping), centralize it in `pice-core` as both a `pub const &'static str` AND a small predicate helper. Every consumer site uses the helper; nobody re-types the literal.

Pattern in `pice-core::cli::ExitJsonStatus`:

```rust
impl ExitJsonStatus {
    pub const METRICS_PERSIST_FAILED_PREFIX: &'static str = "metrics_persist_failed:";

    pub fn is_metrics_persist_failed(halted_by: &str) -> bool {
        halted_by.starts_with(Self::METRICS_PERSIST_FAILED_PREFIX)
    }
}
```

Lock the agreement with a unit test that exercises both: builds a string from the constant and asserts the helper accepts it. See `crates/pice-core/src/cli/mod.rs::metrics_persist_failed_prefix_helper_agrees_with_constant`. Without this test, a refactor that updates the const but forgets the helper (or vice versa) compiles and silently changes routing semantics.

This rule is the runtime-string analogue of the `ExitJsonStatus::as_str()` ↔ serde-kebab-case parity test for typed discriminants. Apply it whenever a literal crosses crate boundaries with semantic meaning.

## Pure-function tests in `pice-core` complement orchestrator tests

When you add or extend a pure function in `pice-core` (e.g., `LayerDag::build_dag`, `decide_halt`, seam validators) that the daemon orchestrator consumes, write pure-function unit tests in `pice-core` EVEN IF the orchestrator already has an integration test exercising the behavior end-to-end.

Rationale:
- The daemon integration test requires spinning up the stub provider, scheduling tokio tasks, and running the full Stack Loops pipeline — slow feedback, noisy failures.
- The pure-function test runs in milliseconds on `cargo test -p pice-core` and pinpoints the bug to a specific input/output pair.
- Integration tests verify the seams between components; pure-function tests verify the components themselves. Both matter; neither substitutes for the other.

Minimum coverage when introducing or modifying a `pice-core` pure function consumed by orchestration: 1 happy path + 1 edge case (e.g., diamond topology, entropy floor boundary) + 1 error case (e.g., cycle rejection, NaN input). Example: Phase 5 added 4 `build_dag` tests (`diamond_topology_produces_three_cohorts`, `all_independent_layers_produces_single_cohort`, `deterministic_across_runs`, `rejects_cycle`) to complement the single pre-existing `dag_cohort_grouping` test — each covers a distinct shape category.

## Holding `std::sync::MutexGuard` across `.await` in tests

`clippy::await_holding_lock` pattern-matches on naked `let guard = some_lock();` bindings whose static type is `MutexGuard`. Tests that serialize env-var mutations (`std::env::set_var` — process-global and inherently racy across tokio tasks) often need to hold a `OnceLock<Mutex<()>>` guard across `.await` on the system under test. The lint fires even when the mutex is test-binary-local with a single consumer.

**Two equally acceptable fixes — pick based on how much the test does:**

1. **Wrap the guard in an RAII struct with env-setup + cleanup-on-drop.** Clippy no longer sees a naked `MutexGuard` binding because the struct field's type is private. Bonus: cleanup survives panics. Pattern: `ParallelStubGuard` in `crates/pice-daemon/tests/parallel_cohort_integration.rs` and `StubEnvGuard` in `crates/pice-daemon/tests/parallel_cohort_speedup_assertion.rs`.

2. **Add `#[allow(clippy::await_holding_lock)]`** with a comment naming the single-consumer invariant and why the lock never contends.

**Prefer option 1** when the test also needs env cleanup on panic, which is almost always. The struct-field pattern is structural, not a suppression — a future refactor that moves the guard around can't accidentally expose it. NEVER swap the `std::sync::Mutex` for `tokio::sync::Mutex` as a lint-dodge: `std::env::set_var` is synchronous, and yielding mid-critical-section defeats the "this block runs atomically" semantic. The lint is about scheduler fairness, not correctness — our single-consumer tests satisfy the real concern either way.

## Share RAII env-guard structs across test binaries via a `pub` module

When more than ONE test binary (lib-level `#[cfg(test)]` inline tests AND integration tests under `tests/*.rs`) needs the same env-guard RAII struct, promote the struct to a crate-level `pub mod test_support` rather than duplicating it. Rust statics are binary-local (cargo-test compiles each integration file + the lib as SEPARATE processes), so the `OnceLock<Mutex<()>>` inside the module still produces a distinct lock per binary — which is the correct semantic, since each test binary runs in its own OS process. Centralizing the STRUCT prevents definition drift (e.g., one copy forgets to restore the prior env var on Drop while another remembers).

Pattern from Phase 6: `crates/pice-daemon/src/test_support.rs` exposes `StateDirGuard` (RAII `PICE_STATE_DIR` swap) + `state_dir_lock()`. Both `handlers::review_gate::tests` (inline) and `tests/review_gate_lifecycle_integration.rs` (integration binary) `use pice_daemon::test_support::StateDirGuard;`. The Pass-3 adversarial review of Phase 6 flagged two uncoordinated copies of this struct as a drift risk; consolidation closed it.

Cross-binary struct sharing WORKS despite Rust statics being binary-local because the binary boundary IS the process boundary — lock sharing across binaries wouldn't help anyway (each cargo-test binary spawns as its own OS process and doesn't share memory). The shared struct is for DEFINITION hygiene, not runtime mutex sharing.

## Gate test-only helpers that use `.expect()` behind `#[cfg(test)]`

`pice-daemon` enforces `-D clippy::unwrap_used -D clippy::expect_used` on its library surface. A documented-panic test helper (e.g., `MockClock` whose `expect("MockClock mutex poisoned")` is load-bearing — poisoning means an earlier assertion panicked with the lock held, and the test SHOULD fail loud) still trips the lint if it lives in non-gated `pub` code.

Rule: gate test-only types that legitimately use `.expect()` behind `#[cfg(test)]` (or a `test-utils` feature if integration-test binaries need them). When the gated type moves behind `#[cfg(test)]`, any imports that ONLY the gated type references (e.g. `use tokio::sync::Notify;` / `use std::sync::Arc;`) must also move behind the same cfg — otherwise they surface as `unused import` warnings in `cargo build --release`.

Phase 6 example: `crates/pice-daemon/src/clock.rs` gates `MockClock` + its `Clock` impl behind `#[cfg(test)]`. `SystemClock` (production) remains unconditionally `pub`. The `Notify` + `Arc` imports used only by `MockClock` sit behind `#[cfg(test)]` too. If Phase 6.1's background reconciler needs `MockClock` from an integration-test binary, widen the gate to `#[cfg(any(test, feature = "test-utils"))]` and expose via a dev-dependencies feature flag — don't ungate back to production.

## Don't ship trait-based scaffolding ahead of a real consumer

A generic trait + impl set added speculatively ("future consumers will plug in here") that nothing in production currently calls becomes scaffolding debt: unit tests exercise it but the real code path bypasses it, which creates a silent testability gap — a refactor to the bypassed path ships with zero coverage.

Phase 6 example: the initial `DecisionSource` trait + 3 impls (`Scripted` / `Piped` / `Tty`) shipped before the production prompt path was written. The production paths at `commands/review_gate.rs::prompt_tty_for_decision` and `commands/evaluate.rs::prompt_decision_for_gate` ended up reading stdin directly because `StdinLock: !Send` blocked the trait from being wired into the async handler — the trait's unit tests passed but exercised no real code. Pass-3 review removed the trait. Only the pure `render_prompt` helper (actually shared by both call sites) survived.

Rule: when adding a trait + impls, the PR that lands the trait must also wire at least one production call site through it. If the production wiring can't land yet (type-system blocker, missing async primitive, etc.), keep the trait design in a plan doc and ship the production path first — THEN reintroduce the trait when a second consumer needs the abstraction.

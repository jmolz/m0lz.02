# Handoff: Phase 0 Daemon Refactor — C2 Passed, Ready for T15

**Date:** 2026-04-10
**Branch:** `v0.2/phase-0-daemon` (worktree at `.worktrees/phase-0-daemon/`)
**Last Commit:** `dbeb1db refactor(pice-daemon): move metrics db/store/telemetry from pice-cli (T14)`

## Goal

Execute Phase 0 of the PICE v0.2 refactor — split `pice-cli` into three crates (`pice-cli` thin adapter + `pice-daemon` orchestrator + `pice-core` shared logic) so Stack Loops, workflow YAML, and adaptive algorithms (v0.2 Phase 1+) have a foundation. **Tier 3 contract**, 16 criteria. See `.claude/plans/phase-0-daemon-foundation.md` (T15 section at line 616).

## Recently Completed (This Session)

- [x] **T12**: `orchestrator` + `session` + `host` moved to `pice-daemon`. Introduced `StreamSink` trait (`SharedSink = Arc<dyn StreamSink>`), `StreamEvent` + `NoticeLevel` enums (both `#[non_exhaustive]`), `NullSink`, and `TerminalSink`. Rewired 7 command files to import `session`/`ProviderOrchestrator`/`NullSink` from `pice_daemon::orchestrator`. Git renames preserved via the "rename to fresh `core.rs` + glue `mod.rs`" pattern. (`8416306` + `7bac1e0`)
- [x] **T13**: 8 `build_*_prompt` builder functions + 15 tests moved from `pice-cli/src/engine/prompt.rs` to `pice-daemon/src/prompt/builders.rs` with a glue `mod.rs`. 6 command-file imports updated. Zero caller churn inside the builder file itself because T6 had already extracted the pure helpers into `pice-core::prompt::helpers`. (`f73d34c`)
- [x] **T14**: `metrics/{db,store,telemetry}.rs` (23 tests) moved to `pice-daemon::metrics`. `aggregator.rs` stays in `pice-cli`. Used re-export facade in `pice-cli/src/metrics/mod.rs` (`pub use pice_daemon::metrics::{db, store, telemetry, open_metrics_db, …}`) so ~30 call sites didn't need touching. `MetricsDb::open_in_memory()` un-`cfg(test)`'d so aggregator's tests can still reach it cross-crate. (`dbeb1db`)
- [x] **Flake fix**: Pre-existing race in `pice-core::transport::tests` where `PICE_DAEMON_SOCKET` writes from `default_socket_path_respects_env_var_unix` leaked into `default_socket_path_platform_fallback_unix`'s read window. Fixed with module-local `ENV_LOCK: Mutex<()>` applied to all three env-touching tests (unix x2 + windows x1). 10 consecutive clean `cargo test --workspace` runs post-fix. (`b419232`)
- [x] **C2 checkpoint**: All 4 criteria testable at T14 boundary PASS (pice-core purity, 168+ Rust tests, 49 TS tests, no silent test downgrades). `comm -23` of test names main→HEAD returned 0 removals. `#[ignore]` count 0 on both sides.

## In Progress / Next Steps

- [ ] **T15: Unix socket transport** — CREATE `crates/pice-daemon/src/server/{mod,unix}.rs`. Use `tokio::net::UnixListener`, newline-delimited JSON framing (`BufReader::lines()` for reads, `write_all(&buf).await; write_all(b"\n").await` for writes). On bind `AddrInUse`, try to connect — `ConnectionRefused` = stale socket, remove and retry; success = another daemon is running, exit with error. Set socket file perms to 0600 via `std::os::unix::fs::PermissionsExt`. Add a bind/accept/roundtrip test. Plan line 616. Depends on T11 (done).
- [ ] T16: Windows named pipe transport at `crates/pice-daemon/src/server/windows.rs` (`#[cfg(windows)]`). Mirrors T15 using `tokio::net::windows::named_pipe`. `#[cfg(windows)]` test only runs on Windows CI.
- [ ] T17: Auth token — generate 32 random bytes, hex-encode, write to `~/.pice/daemon.token` with 0600 perms, rotate on every daemon start. Reject requests missing/mismatched token with JSON-RPC error `-32002`.
- [ ] T18: RPC router wiring (transport + auth → handlers dispatch).
- [ ] T19: 11 per-command handlers. Plan says: do `init.rs` + `execute.rs` manually (trivial + complex streaming exemplars), then dispatch 9 subagents in parallel for the rest. Handlers must construct `CommandResponse` with struct-variant syntax (`CommandResponse::Text { content: "…".to_string() }`, NOT newtype).
- [ ] T20: `PICE_DAEMON_INLINE=1` bypass in `pice_daemon::inline::run_command`.
- [ ] T21: Lifecycle (startup, signal handling, graceful shutdown budget of 10s).
- [ ] T22: CLI adapter refactor + auto-start (100ms health-check timeout, 2s bind wait).
- [ ] T23–T32: `pice daemon` subcommand, integration tests, CI/NPM updates, Tier 3 `/evaluate` of the whole phase.

## Key Decisions

- **Worktree isolation**: `.worktrees/phase-0-daemon/` — `main` stays shippable, rollback is `git reset --hard` in the worktree with zero main-branch blast radius.
- **`StreamSink` = `Arc<dyn StreamSink>` (aliased as `SharedSink`)** — not `&dyn` or a generic. Forced by `NotificationHandler = Box<dyn Fn + Send>` being `'static`; a reference cannot survive into the captured closure. `#[non_exhaustive]` on both `StreamEvent` and `NoticeLevel` for forward-compat; `TerminalSink` has wildcard match arms.
- **Git rename preservation pattern**: when moving a file into a slot where a pre-existing stub lives, `git mv` to a fresh path (e.g. `orchestrator.rs` → `orchestrator/core.rs`, `prompt.rs` → `prompt/builders.rs`) and rewrite the pre-existing stub as a glue `mod.rs`. Preserves 100% rename and `git log --follow`. The T12 dead-end (trying to overwrite the stub directly) lost rename detection entirely.
- **T14 facade re-exports over call-site rewrites**: `pice-cli/src/metrics/mod.rs` does `pub use pice_daemon::metrics::{db, store, telemetry, …}`. NOT duplication — path alias. Avoids rewriting 30 sites that T19 will rewrite *again* when CLI writers become RPC clients. Do the churn once (in T19) instead of twice.
- **`MetricsDb::open_in_memory()` unconditionally public** — `#[cfg(test)]` is crate-local, so when `pice-cli` runs tests, pice-daemon's test-gated items are invisible. Removing the gate was the minimum-churn fix for aggregator's tests. Doc-commented as "intended for tests."
- **`CommandResponse` struct variants (not newtype)**: `Json { value: Value }` and `Text { content: String }`. serde's `#[serde(tag = "type")]` cannot serialize a tagged newtype variant wrapping a primitive. T19 handlers MUST use struct-variant syntax.
- **Per-task validation cadence**: `cargo test --workspace` + explicit test-count arithmetic after every task. Invariant: **zero silent test downgrades**. Enforced via `comm -23 main-tests head-tests` at C2.

## Dead Ends (Don't Repeat These)

- **`use pice_core::X` inside pice-core itself** — Rust `error[E0433]`. When moving files INTO pice-core, grep for `pice_core::` and rewrite to `crate::`.
- **Newtype variants in `#[serde(tag = "type")]` enums wrapping primitives** — "cannot serialize tagged newtype variant containing a string". Use struct variants (`{ field: T }`) or `#[serde(tag = "type", content = "data")]`.
- **Batching `Edit` calls without prior `Read`** — the Edit tool requires each target file to be Read first in the current session. After a `git mv`, the file at its NEW path has not been Read — you must Read it at the new location before editing.
- **`git mv` into an existing stub file** — rename detection gets confused when the target exists with different content. Use the "move to fresh path + rewrite stub as glue `mod.rs`" pattern instead.
- **`git checkout main` from a worktree** — fatal, `main` is already checked out in the sibling worktree. To baseline-test main, `cd /Users/jacobmolz/code/pice-framework` (the main worktree) and run there.
- **`#[cfg(test)]` on cross-crate test helpers** — invisible from a downstream crate's tests. Remove the gate (and document intent in the docstring) when moving a helper into a dependency crate.
- **`cargo clippy --workspace` without `--all-targets`** — silently ignores test-code warnings. Baseline CLAUDE.md validation uses the no-flag form; latent warnings hide there. Always use `--all-targets`.
- **Process-global env tests without a mutex guard** — `set_var`/`remove_var` races across parallel test threads. Use a module-local `Mutex<()>` and acquire it at the top of every env-mutating test. `unwrap_or_else(|poisoned| poisoned.into_inner())` keeps the lock usable after a panicked sibling.
- **Fighting `rustfmt` import ordering** — rustfmt sorts `crate::*` before `pice_*::*` alphabetically within a `use` group. Let `cargo fmt --all` run; don't hand-craft order.

## Files Changed (This Session)

- `crates/pice-daemon/src/prompt/builders.rs` — NEW via `git mv` from `pice-cli/src/engine/prompt.rs` (T13)
- `crates/pice-daemon/src/prompt/mod.rs` — glue: `pub mod builders; pub use builders::*`
- `crates/pice-daemon/src/metrics/{db,store,telemetry}.rs` — NEW via `git mv` from `pice-cli/src/metrics/` (T14)
- `crates/pice-daemon/src/metrics/mod.rs` — declares submodules + owns `open_metrics_db` / `resolve_metrics_db_path` / `normalize_plan_path` re-export
- `crates/pice-cli/src/metrics/mod.rs` — collapses to `pub mod aggregator;` + facade `pub use pice_daemon::metrics::{…}`
- `crates/pice-cli/src/engine/mod.rs` — `pub mod prompt;` removed
- `crates/pice-cli/src/commands/{commit,execute,handoff,plan,prime,review}.rs` — imports switched to `use pice_daemon::prompt;`
- `crates/pice-core/src/transport/mod.rs` — `ENV_LOCK` mutex added to three env-touching tests (flake fix)

## Current State

- **Tests:** **199 Rust** (11 binaries) + **49 TypeScript** — matches T13 baseline, no downgrade. Delta vs `main`: +31 tests (all additions — daemon RPC roundtrip, stream sink, new daemon lib tests).
- **Build:** `cargo build --release` clean; both `pice` and `pice-daemon` binaries compile.
- **Lint/Types:** `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `pnpm lint`, `pnpm typecheck` all clean.
- **pice-core purity:** `cargo tree -p pice-core -e normal | grep -E '(tokio|reqwest|rusqlite|hyper)'` empty ✅ (C2 criterion 1)
- **Flake status:** Fixed. 10 consecutive clean `cargo test --workspace` runs post-fix.
- **Phase 0 progress:** **14/32 tasks complete** + flake fix + C2 checkpoint passed.

## Context for Next Session

T15 is the first **net-new-code** task in Phase 0 — all prior tasks were moves. The risk profile shifts from "don't break what's already tested" to "write correct async socket code with stale-socket detection and 0600 file perms." The Unix socket impl is the sibling to the (eventual) Windows named pipe impl in T16; both go behind a `DaemonTransport` abstraction. T15 is the reference implementation — get the framing right, the stale-socket detection right, and the test pattern right, and T16 mostly mirrors it with `#[cfg(windows)]`.

**Biggest risk: stale-socket detection must differentiate live-daemon-binding from stale-file.** Plan explicitly says: on `AddrInUse`, attempt to connect — `ConnectionRefused` means stale (remove + retry bind), successful connect means another daemon is live (exit with clear error). Getting this wrong either leaks sockets across unclean shutdowns or races with a legitimate second-daemon attempt.

**Note:** `crates/pice-core/src/transport/mod.rs` already defines `SocketPath::{Unix, Windows}` + `default_from_env()` — that's the type T15 will consume. Don't redefine it.

**Recommended first action:**
```bash
cd /Users/jacobmolz/code/pice-framework/.worktrees/phase-0-daemon
git log --oneline main..HEAD                    # expect 8 commits ending at dbeb1db
cargo test --workspace 2>&1 | grep "test result: ok" | awk '{s+=$4} END {print s}'  # expect: 199
# Then read, in order:
# 1. .claude/plans/phase-0-daemon-foundation.md (Task 15 section, line 616)
# 2. .claude/rules/daemon.md (stale-socket detection + auth + transport invariants)
# 3. crates/pice-core/src/transport/mod.rs (SocketPath type — input to T15)
# 4. crates/pice-daemon/src/server/mod.rs (7-line stub from T11 — overwrite)
# Then implement: bind → set 0600 → accept → read lines → dispatch stub → test roundtrip.
```

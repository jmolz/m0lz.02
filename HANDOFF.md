# Handoff: Phase 0 Daemon Refactor — COMPLETE (32/32)

**Date:** 2026-04-13
**Branch:** `v0.2/phase-0-daemon` (worktree at `.worktrees/phase-0-daemon/`)
**Last Commit:** `38bfc2b feat(pice-cli): add daemon subcommand + integration tests + CI updates (T24-T31)`

## Goal

Phase 0 of PICE v0.2 — split `pice-cli` into three crates (`pice-cli` thin adapter + `pice-daemon` orchestrator + `pice-core` shared logic). **All 32 tasks complete.** The crate split is done, the daemon starts/stops/accepts RPCs, and every CLI command routes through the adapter pipeline. Handlers are stubs — Phase 1 ports them to real logic.

## Recently Completed (This Session)

- [x] **T24: `pice daemon` subcommand** — start/stop/status/restart/logs actions, `DaemonClient::health_query()`, `spawn_daemon()` promoted to `pub(crate)`. 6 tests.
- [x] **T25-T28: Verification sweep** — main.rs wiring confirmed, 242 Rust + 49 TS tests pass, fmt/clippy/lint clean.
- [x] **T27: Daemon integration tests** — `lifecycle.rs` (4 tests: connection reuse, concurrent clients, socket cleanup, all 11 command dispatch) + `auth.rs` (3 tests: wrong token, empty token, connection survives rejection).
- [x] **T29-T30: CI + NPM updates** — `release.yml` builds/ships both `pice` and `pice-daemon` binaries; npm resolver exports `getDaemonBinaryPath()`.
- [x] **T31-T32: Full validation + smoke test** — all commands confirmed via `PICE_DAEMON_INLINE=1`.

## In Progress / Next Steps

- [ ] **Merge `v0.2/phase-0-daemon` to `main`** — 20 commits, all green. Consider squash-merge or PR.
- [ ] **Phase 1: Port handler stubs to real logic** — 22 ignored integration tests in `command_integration.rs` serve as the re-enablement checklist. Start with `status` and `init` (simplest), then `plan`/`execute` (streaming), then `evaluate` (dual-model).
- [ ] **File-based daemon logging** — `logging.rs` still uses stderr (T11 stub). Replace with `tracing_appender::rolling::daily("~/.pice/logs", "daemon.log")` so `pice daemon logs` works against a real file.
- [ ] **`pice daemon start` binary discovery** — currently uses PATH lookup. Should check adjacent to the CLI binary first (for npm installs where both binaries are in the same package dir).

## Key Decisions

- **Worktree isolation**: `.worktrees/phase-0-daemon/` — `main` stays shippable.
- **`StreamSink` = `Arc<dyn StreamSink>` (aliased as `SharedSink`)** — forced by `NotificationHandler = Box<dyn Fn + Send>` being `'static`.
- **`CommandResponse` struct variants (not newtype)**: serde's `#[serde(tag = "type")]` cannot serialize tagged newtypes wrapping primitives.
- **`render_response()` takes no `json` param** — daemon handler already picks the right variant based on `req.json`.
- **22 integration tests `#[ignore]`d with reason strings** — deliberate debt markers, re-enable as each handler graduates from stub.
- **No `DaemonTransport` trait** — deferred; two impls mirror API shape, trait falls out naturally later.
- **No stale-pipe recovery on Windows** — named pipes are kernel objects, no dead-corpse case.

## Dead Ends (Don't Repeat These)

- **`use pice_core::X` inside pice-core itself** — use `crate::`.
- **Newtype variants in `#[serde(tag = "type")]` enums wrapping primitives** — use struct variants.
- **macOS fd-inheritance race in stale-socket tests** — must stay in separate integration test binary. Do NOT inline back into `server::unix`.
- **Probing a named pipe you own via `ClientOptions::open`** — consumes next-server slot, breaks accept loop.
- **`DaemonClient` doesn't impl `Debug`** — inner types (`UnixConnection`) don't. Use `result.is_err()` pattern in tests, not `expect_err()`.
- **`cargo run --bin pice` from main repo vs worktree** — runs the wrong code. Always specify `--manifest-path` or `cd` to worktree first.

## Current State

- **Tests:** 242 Rust running + 22 ignored + 49 TS = **291 total** (260 running + 22 debt + 49 TS)
- **Build:** `cargo build --release` clean (both `pice` and `pice-daemon` binaries)
- **Lint/Types:** fmt, clippy, eslint, tsc all clean, zero warnings
- **Phase 0 progress:** **32/32 tasks complete**. Branch pushed.

## Context for Next Session

Phase 0 is done. The branch has 20 commits and is pushed to origin. The immediate action is to merge it into `main` (PR or direct merge). After that, Phase 1 begins: porting the 11 stub handlers to real logic, starting with the simplest (`status`, `init`) and working up to streaming commands (`plan`, `execute`) and dual-model evaluation. The 22 ignored integration tests are the checklist — re-enable each as its handler graduates.

**Recommended first action:**
```bash
cd /Users/jacobmolz/code/pice-framework/.worktrees/phase-0-daemon
git log --oneline main..HEAD | wc -l  # expect: 20
cargo test 2>&1 | grep "test result" | awk '{s+=$4} END {print s}'  # expect: 242
# Then: create PR or merge to main
```

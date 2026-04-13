# Handoff: v0.2 Daemon Architecture — Phase 0 + Phase 1 Complete

**Date:** 2026-04-13
**Branch:** `main`

## Goal

PICE v0.2 daemon architecture is in place. Phase 0 (crate split into `pice-cli` + `pice-daemon` + `pice-core`) and Phase 1 (porting all 11 handler stubs to real implementations) are complete and merged to main.

## Recently Completed (This Session)

- [x] **Template sync** — 9 embedded template files in `templates/claude/` synced with root `.claude/` (threshold 7→8, worktree awareness, PICE-specific workflows)
- [x] **Stale artifact cleanup** — removed 46 untracked files (per-crate `.claude/` test artifacts + reference plan)
- [x] **Test count updates** — README badge (320), CONTRIBUTING.md (271), commit-and-deploy.md (271) all reflect current counts
- [x] **CONTRIBUTING.md structure** — updated project structure to include `pice-daemon` and `pice-core` crates

## In Progress / Next Steps

- [ ] **File-based daemon logging** — `logging.rs` still uses stderr (Phase 0 T11 stub). Replace with `tracing_appender::rolling::daily("~/.pice/logs", "daemon.log")` so `pice daemon logs` works against a real file.
- [ ] **`pice daemon start` binary discovery** — currently uses PATH lookup. Should check adjacent to the CLI binary first (for npm installs where both binaries are in the same package dir).
- [ ] **PRDv2 Phase 1: Layer Detection** — next major feature phase. Read `PRDv2.md` and `.claude/rules/stack-loops.md` before starting.

## Key Decisions

- **`StreamSink` = `Arc<dyn StreamSink>` (aliased as `SharedSink`)** — forced by `NotificationHandler = Box<dyn Fn + Send>` being `'static`.
- **`CommandResponse` struct variants (not newtype)**: serde's `#[serde(tag = "type")]` cannot serialize tagged newtypes wrapping primitives.
- **No `DaemonTransport` trait** — deferred; two impls mirror API shape, trait falls out naturally later.

## Dead Ends (Don't Repeat These)

- **`use pice_core::X` inside pice-core itself** — use `crate::`.
- **Newtype variants in `#[serde(tag = "type")]` enums wrapping primitives** — use struct variants.
- **macOS fd-inheritance race in stale-socket tests** — must stay in separate integration test binary.
- **`DaemonClient` doesn't impl `Debug`** — inner types (`UnixConnection`) don't. Use `result.is_err()` pattern in tests.

## Current State

- **Tests:** 271 Rust (1 ignored) + 49 TS = 320 total
- **Build:** `cargo build --release` clean (both `pice` and `pice-daemon` binaries)
- **Lint/Types:** fmt, clippy, eslint, tsc all clean, zero warnings

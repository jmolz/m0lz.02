---
paths:
  - "crates/pice-daemon/**"
  - "crates/pice-core/**"
  - "crates/pice-cli/src/adapter/**"
  - "crates/pice-cli/src/commands/daemon.rs"
---

# Daemon Architecture Rules (v0.2+)

See `PRDv2.md` → "Architectural Pivot: Headless Daemon + Adapters" for the full design rationale. This file captures the invariants and patterns.

## The split

- `pice-cli` — short-lived CLI adapter. Parses args, renders TTY output, sends daemon RPCs, exits.
- `pice-daemon` — long-lived daemon process. Owns orchestration, provider processes, state, and SQLite.
- `pice-core` — shared logic. Both crates depend on it. Never duplicate parsing/validation between CLI and daemon.

## Crate boundary (hard rule)

If the CLI needs to preview what the daemon will execute, put the logic in `pice-core`:
- Config TOML parsing → `pice-core::config`
- Layers TOML parsing → `pice-core::layers`
- Workflow YAML parsing + validation → `pice-core::workflow`
- Manifest schema + helpers → `pice-core::manifest`
- Daemon RPC types → `pice-core::protocol`
- Seam check trait + default library → `pice-core::seam`
- SPRT/ADTS/VEC algorithms → `pice-core::adaptive`

`pice-core` has **zero async** and **zero network** dependencies. Pure logic + data types.

## Transport

- macOS/Linux: Unix domain socket at `~/.pice/daemon.sock` (override via `PICE_DAEMON_SOCKET`)
- Windows: named pipe at `\\.\pipe\pice-daemon`
- Abstract behind a `DaemonTransport` trait. Per-platform impls in `pice-daemon/src/server/`.
- Framing: newline-delimited JSON-RPC 2.0 (`\n`-separated messages)
- Benchmarked before release: Windows named pipe parity with Unix socket must be verified in CI.

## Authentication

- Bearer token stored in `~/.pice/daemon.token`, file permissions 0600 (owner read/write only)
- Token is 32 random bytes, hex-encoded
- Rotated on every daemon start
- CLI reads the token at startup and includes it in every RPC as a top-level `auth` field (not inside `params`)
- Daemon rejects any request without a valid token with error code `-32002`
- Never log the token. Never send it to providers. Never pass it as a process argument.

## Auto-start behavior

- `pice <command>` checks if the daemon is running (via `daemon/health` RPC, 100ms timeout)
- If not running, CLI starts `pice-daemon` as a detached background process, waits for socket to become available (up to 2s), then retries the RPC
- First-run auto-start latency target: < 500ms end-to-end
- Warm CLI command latency target: < 50ms
- `pice daemon start` explicitly starts the daemon (for shell init scripts)
- `pice daemon stop` sends `daemon/shutdown` RPC; `pice daemon restart` = stop + start
- `pice daemon status` prints PID, uptime, active evaluations, socket path

## Graceful shutdown

- On SIGTERM, daemon enters shutdown mode:
  1. Stop accepting new RPCs
  2. Wait for in-flight RPCs to complete (up to 10s)
  3. Flush all manifests to disk (atomic rename)
  4. Close provider processes with `shutdown` RPC
  5. Close socket, remove socket file, exit
- On SIGKILL, the daemon cannot clean up. The CLI detects stale socket on next connect attempt and removes it.
- On daemon crash mid-evaluation, the manifest survives (every `manifest/event` is persisted atomically). On restart, the daemon reads active manifests and marks them `failed-interrupted` unless the last checkpoint was a clean state transition.

## Inline mode (debugging)

- `PICE_DAEMON_INLINE=1` bypasses the daemon and runs the orchestrator in-process in the CLI
- Disables background mode and concurrent evaluations
- Used for: CI diagnosis of daemon-related failures, debugging orchestrator logic without the IPC layer
- Must be kept working — the test suite runs against both daemon mode and inline mode

## Verification manifest — source of truth

- Location: `~/.pice/state/{feature-id}.manifest.json` (namespaced by `project_root_hash`)
- Schema versioned (`schema_version: "0.2"`); daemon refuses to read incompatible versions
- Writes are atomic: write to `.tmp` + rename
- Single-writer-per-manifest enforced by daemon's internal lock map
- All adapters (CLI, dashboard, CI) observe the same manifest
- Never build parallel state stores. Never write manifest data to SQLite and treat SQLite as authoritative — SQLite is for metrics aggregation and audit trail. The manifest is for current evaluation state.

## Watchdog

- Daemon health check endpoint at `daemon/health` returns `{ status, version, uptime_s }` in <5ms
- CLI supervisor retries on hang: if `daemon/health` times out twice in a row, auto-restart with warning
- Memory limit: configurable via `config.toml`, default unlimited. Daemon exits cleanly on OOM with last-manifest-flush.
- Long-running session logs flush to `~/.pice/logs/daemon.log` via tracing_appender with daily rotation

## Multi-daemon prevention

- Only one daemon per user per machine (single socket). Second daemon fails to bind and exits with error.
- The socket file itself is the lock. Stale sockets (after unclean shutdown) are detected via `connect()` test — if connection fails with ECONNREFUSED, socket is stale, daemon removes and recreates.

## Windows considerations

- Named pipes do not use filesystem permissions. Access control is via Windows ACL.
- Default: pipe is owner-only (same effect as 0600 on Unix)
- The `DaemonTransport` trait abstraction must hide platform differences from the orchestrator and RPC handlers
- Run the full acceptance suite on Windows in CI before shipping v0.2

## What the CLI must NOT do directly

- Spawn provider processes (daemon owns the provider host)
- Write to SQLite metrics (daemon owns writes; CLI may read for reporting)
- Write to verification manifests (daemon owns writes; CLI may read via `manifest/get`)
- Run the adaptive algorithms (pure functions live in `pice-core`, but execution happens in the daemon)
- Create/remove git worktrees (daemon owns worktree lifecycle)

All of these go through the daemon. The CLI is an adapter, not a participant.

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

- Location: `~/.pice/state/{project_hash_12chars}/{feature-id}.manifest.json` (namespaced by project root SHA-256 hash to prevent cross-repo collisions)
- Schema versioned (`schema_version: "0.2"`); daemon refuses to read incompatible versions
- Writes are crash-safe: write to `.tmp` + fsync + rename + fsync parent directory
- Persisted incrementally: initial checkpoint, per-layer checkpoint, final checkpoint
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
- Embed or extract templates (daemon owns `rust-embed` and the init handler; CLI delegates via adapter)
- Run metrics aggregation queries (daemon owns `metrics::aggregator`; CLI dispatches `pice metrics` to daemon)

All of these go through the daemon. The CLI is an adapter, not a participant.

## Streaming and JSON mode

- Daemon handlers receive a `&dyn StreamSink` for streaming output.
- In inline mode, `TerminalSink` writes chunks to stdout and events to stderr.
- In socket mode, `NullSink` is used (temporary — socket-side stream relay is Phase 2 work).
- **Streaming handlers MUST gate on `!req.json`**: never install `streaming_handler()` or use `to_shared_sink()` when JSON mode is active. Stream chunks on stdout corrupt the JSON response.
- Capture handlers (commit, handoff) that use `run_session_and_capture()` should use `NullSink` as the shared sink in JSON mode.

## Channel ownership invariant (Phase 6+)

**Interactive prompt text is CLI-owned and written to stderr**; daemon-emitted streaming text and normal command output go to stdout (unchanged). This preserves the stdout-as-JSON invariant in `--json` mode — a concurrent `pice evaluate --json` run is parseable because prompt bytes never touch stdout.

Concrete consequences:
- The review-gate box-drawing prompt is produced by the pure helper `crates/pice-cli/src/input/decision_source.rs::render_prompt(body, details)` and written to `std::io::stderr()` by the CLI. The daemon never sees or emits prompt bytes.
- Production prompt call sites (`crates/pice-cli/src/commands/review_gate.rs::prompt_tty_for_decision` + `crates/pice-cli/src/commands/evaluate.rs::prompt_decision_for_gate`) read stdin directly via `std::io::stdin().read_line(...)`. Phase 6 initially shipped a `DecisionSource` trait abstraction, but `StdinLock: !Send` blocked it from being wired through the async handler path — the Pass-3 review removed the trait as unused scaffolding (only `render_prompt` survives). If Phase 7 re-introduces an input abstraction (e.g. `tokio::task::spawn_blocking`-wrapped TTY source for the PTY test harness), do it once a real consumer exists — don't ship the trait ahead of a user.
- The daemon's `ReviewGate::Decide` handler NEVER reads environment variables for the reviewer name. `ReviewGateSubcommand::Decide.reviewer` is resolved CLI-side (`$USER` / `$USERNAME` / `unknown` fallback) and threaded through the RPC.

## Structured JSON failure responses

`CommandResponse` has two exit variants. They are NOT interchangeable:

- `Exit { code, message }` — human-readable failure. Renderer writes `message` to **stderr** and exits nonzero.
- `ExitJson { code, value }` — structured `--json`-mode failure. Renderer writes `serde_json::to_string_pretty(&value)` to **stdout** and exits nonzero. Used by `pice validate --json` so CI pipelines like `pice validate --json && deploy` fail closed while the machine caller still gets a parseable report on the expected channel.

**Rules:**
- Never return `Exit { message: <stringified JSON> }`. String-sniffing to route JSON to stdout is ambiguous (a plain-text error that happens to parse as JSON would be misrouted) and was removed.
- A JSON-mode success emits `Json { value }` (stdout, exit 0). A JSON-mode failure emits `ExitJson { code: 1|2, value }` (stdout, exit 1 or 2). Text-mode failures use `Exit` (stderr).
- The renderer is in `crates/pice-cli/src/commands/mod.rs::render_response`. Every `CommandResponse` variant must have a dedicated arm — no catch-all string heuristics.
- Daemon RPC roundtrip: `ExitJson` serializes as `{"type":"exit-json","code":N,"value":...}` (kebab-case internally-tagged enum). Both pice-cli and pice-daemon depend on the enum in `pice-core::cli`; divergence is a bug.
- **(Phase 3+) `ExitJson.value.status` discriminants are typed.** The `value` JSON object carries a `"status"` field whose value MUST come from `pice_core::cli::ExitJsonStatus` via `.as_str()` — never a raw string literal. The enum has `#[serde(rename_all = "kebab-case")]` and a hand-written `as_str()` method; a unit test (`exit_json_status_as_str_matches_serde_kebab_case`) locks the two in sync. When adding a new structured failure path: add a variant to `ExitJsonStatus`, implement it in the handler via `ExitJsonStatus::NewVariant.as_str()`, and add a CLI binary integration test in `crates/pice-cli/tests/evaluate_integration.rs` that asserts the wire string against `ExitJsonStatus::NewVariant.as_str()`.

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

## Phase 7 invariants (background execution + subscribe streams)

These invariants are codified in the Phase 7 implementation and enforced by tests in `crates/pice-daemon/tests/background_dispatch_integration.rs`, `concurrent_features_integration.rs`, `subscribe_snapshot_response_integration.rs`, and the daemon `handlers::subscribe` + `handlers::status` + `handlers::logs` inline test modules.

- **`manifest/subscribe` + `logs/stream` are SINGLE-REQUEST-RESPONSE RPCs** whose response body carries the initial snapshot. Subsequent notifications (`manifest/event`, `logs/chunk`) stream on the SAME connection until the CLI closes it. There is NO `unsubscribe` RPC — **connection close IS unsubscribe**. `handlers/subscribe.rs` uses `tokio::select!` between an inbound-read arm (clean EOF or error → break) and the broadcast receiver; loop exit drops the receiver, and the broadcast channel's subscriber count decrements automatically. No explicit `SubscriptionRegistry`.
- **Connection-close cleanup is automatic.** Per-connection `broadcast::Receiver` drop on `select!` exit decrements the channel's subscriber count. The asymptotic invariant is "idle daemon has zero broadcast subscribers" — holds immediately on Unix clean-EOF; on Windows `kill -9` may leave the server-side read blocked until OS pipe timeout (acceptable per plan). The `connection_drop_cleans_up` integration test validates the Unix path end-to-end.
- **Background dispatch-return time is a measured-p95 SLO < 500ms**, NOT a hard per-call bound. The hard invariant is "dispatch returns BEFORE the first provider RPC" — the orchestrator future is `tokio::spawn`ed, and the handler writes the Queued manifest + returns the `BackgroundDispatched` response immediately. Verified by `fifty_dispatches_meet_p95_slo`.
- **Two semaphores govern concurrency.** `max_parallelism` (per-feature cohort, clamped ≤ 16) and `max_global_provider_concurrency` (global provider sessions, clamped ≤ 32) are independent; operators size them independently. With global=2 and 3 concurrent features, at most 2 features are past the permit-acquire barrier at any instant (`global_semaphore_bounds_concurrent_provider_holds`). **CRITICAL for test authors:** bind the `OwnedSemaphorePermit` parameter with a non-underscore name (`permit` or `let _hold = permit`) — an `_permit` parameter pattern tells rustc the value is unused and drops it immediately, releasing the semaphore slot before the future's critical section.
- **`ManifestStatus::Queued` is only ever observed on disk for fresh blank dispatches**; the orchestrator transitions the dispatch marker to `InProgress` as its first action after acquiring the global provider permit. A dispatch-time crash before transition leaves a blank `Queued` manifest; startup reconciliation treats these as DELETE (system-level state repair, not an audit-worthy interrupt). Resume dispatches that preserve existing layers or review gates use `Pending`, and startup reconciliation defensively rewrites any `Queued` manifest with layers/gates to `Pending` instead of deleting the audit source of truth.
- **Jobs capture a `JobEnv` snapshot at spawn** (`state_dir`, `project_root`, `workflow_snapshot`, `contracts`, env overrides). Runtime env mutation does NOT affect already-running jobs. Locks down the plan's "background tasks snapshot env at spawn, not on every read" invariant.
- **Lifecycle integration tests must isolate the daemon project root from the caller's checkout.** `lifecycle::run_with_paths_for_project` exists so socket/token tests can serve a temp project root instead of `std::env::current_dir()`. Merge validation for Phase 7 caught `tests/lifecycle.rs::all_command_types_dispatch_successfully` invoking the commit provider because the merge index had staged changes; daemon tests that dispatch git-sensitive commands must not depend on the user's staged/unstaged state.
- **`FeatureJobManager` panics emit `Cancelled { reason: "panic" }`** via the supervisor task, then remove the feature from the DashMap. The panicked feature's manifest is left as-is and picked up by the next startup reconciliation as an `InProgress` → `Failed(failed-interrupted)` rewrite.
- **Desktop notifications are non-load-bearing.** `notifications::notify` logs failures via `tracing::debug!` + falls back to a terminal BEL (`\x07[pice] ...`) on stderr. Never surfaces errors to the user; CI runners without a notification daemon must exit cleanly. Config is floor-merged (user floor + project → `user && project`); the project may DISABLE any `on_*` flag but never ENABLE one the user disabled.
- **`SubscribedGateSource` is a concrete struct, not a trait.** No `DecisionSource` trait exists in Phase 7. Introduce a trait only when a second consumer shape materializes (e.g. dashboard gate UI in v0.3). The struct owns the reviewer name snapshotted at construction; each `handle_gate_requested` call opens a SEPARATE `DaemonClient` for the `ReviewGate::Decide` dispatch because the subscribe stream's client is busy reading notifications (bearer-token auth permits concurrent connections).
- **All daemon-side state-transition saves go through `ManifestSaver::save_and_emit(intent)`.** Raw `VerificationManifest::save` calls are banned at grep-assertion level across `orchestrator/`, `handlers/review_gate.rs`, and `handlers/evaluate.rs`; only `events/saver.rs` retains the raw call.
- **`PICE_DAEMON_INLINE=1` behavior for Phase 7 commands:**
  - `pice evaluate --background` / `pice execute --background` → reject with `ExitJsonStatus::InlineModeBackgroundUnsupported` (exit 1). Inline mode has no daemon process to own the background task.
  - `pice status --follow` → daemon handler rejects follow/wait at `cli/dispatch` with a structured `Exit` naming `manifest/subscribe` as the correct route. Inline mode has no router-level subscribe path.
  - `pice logs --follow` → daemon handler rejects follow at `cli/dispatch` with a structured `Exit` naming `logs/stream` as the correct route.

## Streaming and JSON mode (Phase 7 extensions)

Phase 7 introduces two new output shapes — `--stream-json` NDJSON and the background-wait outcome envelope. They coexist with the pre-Phase-7 `--json` single-object mode under strict channel-ownership rules.

- **`--json` remains single-JSON-object mode.** It is REJECTED with a clap error when paired with `--follow` (a streaming endpoint can't produce a single object). The error message names the conflicting flag and suggests `--stream-json` (pinned by `stream_json_flag_validation.rs`).
- **`--stream-json` is the streaming NDJSON mode.** Each line of stdout is one JSON object. For `pice status --follow --stream-json` the wire shape is a heterogeneous `StreamJsonFrame` envelope: `{"kind":"snapshot",...} | {"kind":"event",...} | {"kind":"terminal","exit_code":N}`. Consumers pattern-match on `kind`. First line is always `snapshot`; zero or more middle lines are `event`; final line before exit is `terminal`. For `pice logs --follow --stream-json` the envelope is `{"kind":"log-chunk","chunk":<LogChunk>}` plus a `terminal` frame on stream close.
- **Channel ownership — stdout vs stderr.** Stdout carries the VISIBLE FRAME: `--stream-json` NDJSON lines, follow-mode human render, daemon-emitted streaming text, JSON responses. Stderr carries: CLI prompts (review-gate box, `SubscribedGateSource` prompt), control events (terminal marker when NOT in `--stream-json`, disconnect notices, timeouts), and `tracing` output. A consumer piping `pice status --follow --stream-json | jq` NEVER sees prompt bytes on stdout.
- **`SubscribedGateSource` prompts go to stderr** — the Phase 6 channel-ownership invariant extends unchanged. Under `--stream-json` OR piped stdin (non-TTY), the follow loop does NOT prompt; the reviewer is expected to run `pice review-gate` separately. TTY detection uses `std::io::IsTerminal` on BOTH stdin and stderr.

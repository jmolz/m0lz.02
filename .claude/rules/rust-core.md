---
paths:
  - "crates/**"
  - "Cargo.toml"
  - "Cargo.lock"
---

# Rust Core Rules

## Crate Organization

### v0.1 (current, shipped)

- `pice-cli` — binary crate, depends on `pice-protocol`. Owns everything: state machine, provider host, metrics, templates.
- `pice-protocol` — library crate, zero external dependencies beyond serde. Shared contract types for core↔provider JSON-RPC.

### v0.2+ (post-refactor — see `PRDv2.md` and `.claude/rules/daemon.md`)

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

- Template files from `templates/` are embedded using `rust-embed` or `include_str!`.
- Test that embedded templates match the actual template files in CI.

## Provider Resolution

- `ProviderHost::spawn(command, args)` launches a provider as a tokio child process.
- `registry::resolve(name, config)` maps provider names to commands. It locates provider binaries by walking up from the pice binary's own location looking for `packages/`.
- Notifications received during `request()` are forwarded to an optional `NotificationHandler` callback (set via `on_notification()`). Phase 2 streaming depends on this.
- `ProviderHost::shutdown(timeout)` splits the timeout budget: `min(timeout, 5s)` for the shutdown RPC, remainder for process exit wait.

## Session Runner

- `engine/session.rs` provides `run_session()` and `run_session_and_capture()`. All provider-backed commands use these — never duplicate the session lifecycle.
- `streaming_handler()` creates the standard notification handler for text-mode streaming. Use it instead of inline closures.
- The always-shutdown pattern: `let result = session::run_session(...); orchestrator.shutdown(); result?;` — the provider shuts down even on failure.

## Contract Parsing

- `plan_parser.rs` detects `## Contract` headings using line-level matching (`find_h2_heading`), not substring search.
- Only level-2 headings (`##`) are matched. `###` and deeper headings are rejected. Up to 3 leading spaces are allowed per CommonMark.
- If `## Contract` exists but has no ` ```json ` fence, the parser returns an error (not `Ok(None)`). Half-written contracts must be surfaced, not silently ignored.
- `status.rs` includes malformed plans in output with a `parse_error` field rather than silently dropping them.

## CLI Conventions

- Use `clap` derive macros for arg parsing.
- Every command has a `--json` flag for machine-readable output. When `--json` is active, suppress `println!` messages and emit a single JSON object to stdout. In JSON mode, capture/suppress subprocess stdout (use `output()` not `status()`) to keep stdout as valid JSON.
- Exit codes: 0 = success, 1 = failure, 2 = evaluation failed (contract criteria not met).
- Phase-N scaffolding uses `#[allow(dead_code)]` with a `///` doc comment explaining which phase uses the code.

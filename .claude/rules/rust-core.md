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
- JSON-mode failure responses use `CommandResponse::ExitJson { code, value }`, not `Exit { message: <stringified json> }`. See `.claude/rules/daemon.md` → "Structured JSON failure responses".
- Phase-N scaffolding uses `#[allow(dead_code)]` with a `///` doc comment explaining which phase uses the code.

## Schema Hardening

- **Every serde-derived config struct that represents a user-editable file MUST carry `#[serde(deny_unknown_fields)]`.** TOML and YAML readers silently drop unknown keys by default — a renamed or deprecated field in a stale config will then be silently ignored at runtime. This class of bug is invisible from the user's perspective: the workflow "runs" but respects no override. `deny_unknown_fields` converts that into a parse error with the bad key name.
- The rule applies to: `pice-core::config::PiceConfig`, `pice-core::layers::LayersConfig`, all of `pice-core::workflow::schema::*`, and any future `.pice/*.{toml,yaml}` schema types. It does NOT apply to internal-only types (JSON-RPC wire types, manifest records that may be forward-extended) where unknown fields are expected during version drift.
- Add a test that asserts a stale/misspelled top-level field produces a parse error whose message names the bad field. See `crates/pice-core/src/workflow/loader.rs::load_project_rejects_unknown_top_level_fields` for the pattern.

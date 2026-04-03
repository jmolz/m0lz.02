# CLAUDE.md

This file provides guidance to Claude Code when working with this repository.

## Project Overview

PICE CLI is an open-source workflow orchestrator for structured AI coding. It implements the Plan-Implement-Contract-Evaluate (PICE) methodology as a Rust-powered CLI with a TypeScript provider bridge, orchestrating AI coding sessions through a formal lifecycle with dual-model adversarial evaluation and quality metrics. Architecture uses a JSON-RPC provider protocol (inspired by MCP) enabling community-built providers for any AI coding tool.

See `.claude/PRD.md` for the full product requirements document.

---

## Tech Stack

| Technology | Purpose |
|------------|---------|
| Rust (stable) | Core CLI binary — arg parsing, state machine, config, metrics, templates, provider host |
| TypeScript 5.x | Provider implementations — Claude Code SDK bridge, Codex evaluator |
| Node.js 22+ LTS | Runtime for TypeScript providers |
| clap 4.x | CLI framework (args, help, shell completions) |
| tokio 1.x | Async runtime for parallel provider process management |
| rusqlite (SQLite 3) | Local metrics storage |
| serde / serde_json | Serialization for JSON-RPC protocol and TOML config |
| rust-embed | Embed template files in binary at build time |
| @anthropic-ai/claude-agent-sdk | Claude Agent SDK (workflow + evaluation provider). Requires `ANTHROPIC_API_KEY` — subscription/OAuth auth is NOT available for third-party SDK consumers. |
| OpenAI SDK | Codex/GPT adversarial evaluation provider. Requires `OPENAI_API_KEY`. |
| pnpm | TypeScript workspace manager |

---

## Commands

```bash
# Development — Rust
cargo build                    # Build CLI binary
cargo test                     # Run Rust tests
cargo clippy                   # Lint Rust code
cargo fmt --check              # Check Rust formatting

# Development — TypeScript
pnpm install                   # Install TS dependencies
pnpm build                     # Build all TS packages
pnpm test                      # Run TS tests
pnpm lint                      # Lint TS code (eslint)
pnpm typecheck                 # Type check (tsc --noEmit)

# Full Validation (run before every commit)
cargo fmt --check && cargo clippy -- -D warnings && cargo test && pnpm lint && pnpm typecheck && pnpm test && pnpm build && cargo build --release
```

**Expected baseline:** 121 Rust tests, 49 TypeScript tests, 0 lint errors, 0 warnings, clean release build.

---

## Project Structure

```
pice/
├── crates/                    # Rust packages
│   ├── pice-cli/              # Main binary crate
│   │   └── src/
│   │       ├── main.rs        # Entry point
│   │       ├── commands/      # One module per CLI command
│   │       ├── engine/        # PICE loop state machine, lifecycle
│   │       ├── config/        # .pice/ and .claude/ management
│   │       ├── metrics/       # SQLite store, aggregation, telemetry
│   │       ├── templates/     # Scaffolding, file generation
│   │       └── provider/      # Provider host, JSON-RPC, process mgmt
│   └── pice-protocol/         # Shared JSON-RPC protocol types
│       └── src/lib.rs
├── packages/                  # TypeScript packages
│   ├── provider-protocol/     # JSON-RPC types (TS side, published)
│   ├── provider-base/         # Shared provider utilities
│   ├── provider-claude-code/  # Claude Code SDK provider
│   └── provider-codex/        # Codex/OpenAI evaluator provider
├── templates/                 # Files embedded in binary for `pice init`
│   ├── claude/                # .claude/ directory template
│   └── pice/                  # .pice/ directory template
├── docs/                      # PICE methodology (readable on GitHub)
│   ├── methodology/           # Core PICE concepts
│   ├── guides/                # Playbook, brownfield, greenfield
│   └── providers/             # Provider development docs
├── npm/                       # NPM distribution packages
│   ├── pice/                  # Main package (binary resolver)
│   └── pice-{platform}/      # Platform-specific binary packages
├── tests/                     # Integration tests (cross-crate/cross-package)
├── Cargo.toml                 # Rust workspace root
├── package.json               # TS workspace root
└── pnpm-workspace.yaml
```

---

## Architecture

The CLI follows a **Provider Architecture** pattern:

```
pice (Rust binary)
├── Core engine ──── state machine, lifecycle, config
├── Metrics engine ── SQLite + telemetry
├── Template engine ── scaffolding, file generation
├── Provider host ── spawns and manages provider processes
│    ↕ JSON-RPC over stdio
└── Providers (TS) ── Claude Code, Codex, future community providers
```

**Data flow:** User command → Rust core (parse, config, state) → JSON-RPC → Provider (SDK session) → JSON-RPC → Rust core (metrics, output)

**Evaluation flow (Tier 2+):** Rust core launches Claude + Codex providers in parallel via tokio. Claude grades contract criteria. Codex challenges the approach. Core synthesizes a unified report.

**Provider contract:** Providers declare capabilities (`workflow`, `evaluation`, or both) during `initialize`. The core routes commands accordingly — workflow to Claude Code, evaluation to both.

---

## Code Patterns

### Naming
- Rust files: `snake_case.rs`
- Rust types/structs: `PascalCase`
- Rust functions/methods: `snake_case`
- Rust constants: `SCREAMING_SNAKE_CASE`
- TS files: `kebab-case.ts`
- TS types/interfaces: `PascalCase`
- TS functions: `camelCase`
- CLI commands: `kebab-case` (e.g., `pice plan-feature` if multi-word)

### Imports
- Rust: Use `use` with explicit imports, avoid glob imports (`use crate::config::*`)
- TS: Named imports, no default exports. Use `@pice/` workspace aliases.

### Error Handling
- Rust: Use `thiserror` for library errors, `anyhow` for CLI-level errors. Never `unwrap()` in library code. `unwrap()` only in tests or where a panic is the correct behavior.
- TS: Typed errors via discriminated unions. Never swallow errors silently. Provider errors must be surfaced through JSON-RPC error responses.
- Provider failures are non-fatal for the core — the CLI must gracefully degrade (e.g., single-model eval when adversarial provider fails).

### CLI Output
- Every command supports `--json` for machine-readable output. When `--json` is passed, suppress human-friendly messages (`println!`) and emit a single JSON object to stdout.
- Exit codes: 0 = success, 1 = failure, 2 = evaluation failed (contract criteria not met).

### Session Lifecycle
- All provider-backed commands use `session::run_session()` or `session::run_session_and_capture()` from `engine/session.rs`. Never duplicate the create → send → destroy lifecycle in command files.
- For streaming output in text mode, use `session::streaming_handler()` — not inline notification handler closures.
- Commands that need the AI's response text (commit, handoff) use `run_session_and_capture()`. Commands that only stream (prime, review, plan, execute) use `run_session()`.
- The caller registers the notification handler before calling `run_session()`. The session module owns the handler for `run_session_and_capture()`.

### Git Index Safety
- Commands that auto-stage (`git add -u`) must track whether they did so and restore the index (`git reset`) on all non-commit exit paths (dry-run, errors, empty messages).
- Never generate commit messages from a diff that includes files outside the staged set. Stage first, then build the prompt from `get_staged_diff()`.
- Check `git status` exit code before inspecting stdout — non-git directories return empty stdout but non-zero exit.

### Phase Scaffolding
- Code intended for future phases uses `#[allow(dead_code)]` with a comment explaining which phase will use it (e.g., `/// Used by interactive sessions in Phase 3+.`). This keeps the codebase warning-free while allowing architectural scaffolding.

### Async Commands
- All CLI commands are `async fn` and run on the tokio runtime (`#[tokio::main]`). Phase 1/3/4 commands that don't need async yet are trivially async (same body, `async fn` signature).

### Logging
- Rust: Use `tracing` with structured fields. Levels: `error` (user-facing failures), `warn` (degraded behavior), `info` (lifecycle events), `debug` (detailed flow), `trace` (protocol messages). Tracing output goes to stderr via `.with_writer(std::io::stderr)`.
- TS: Use `console.error` for provider-side errors (stderr, not stdout — stdout is the JSON-RPC channel).

---

## Testing

- **Rust framework**: Built-in `#[test]` + `cargo test`
- **TS framework**: Vitest
- **Rust test location**: `src/` inline `#[cfg(test)]` modules for unit tests, `tests/` for integration tests
- **TS test location**: `__tests__/` directories alongside source, or `*.test.ts` co-located
- **Run**: `cargo test && pnpm test`
- **Minimum coverage**: Each new feature needs: 1 happy path, 1 edge case, 1 error case per public function
- **Provider testing**: Use the stub/echo provider for integration tests — never depend on live API calls in CI
- **JSON-RPC protocol tests**: Both sides (Rust + TS) must have serialization roundtrip tests for every message type

---

## Dual-Model Adversarial Evaluation

This project uses **dual-model adversarial evaluation** as a core feature, not an add-on. During `pice evaluate`:

- **Tier 1** (Tier 1 changes): Single Claude evaluator — contract grading only
- **Tier 2** (new features): Claude evaluator + Codex adversarial review in parallel
- **Tier 3** (architectural): Claude agent team (4 evaluators) + Codex xhigh adversarial review

Evaluators see ONLY: contract JSON, git diff, CLAUDE.md. Never implementation context.

Users configure models via `.pice/config.toml` `[evaluation]` section. Both API key and subscription auth are supported.

---

## Validation (Pre-Commit)

Run these before every commit:

```bash
# Rust
cargo fmt --check
cargo clippy -- -D warnings
cargo test

# TypeScript
pnpm lint
pnpm typecheck
pnpm test

# Build
cargo build --release
pnpm build
```

---

## On-Demand Context

When working on specific areas, read the corresponding reference:

| Area | File | When |
|------|------|------|
| Rust core | `.claude/rules/rust-core.md` | Working on crates/ |
| TypeScript providers | `.claude/rules/providers.md` | Working on packages/ |
| JSON-RPC protocol | `.claude/rules/protocol.md` | Changing provider contract |
| Metrics & telemetry | `.claude/rules/metrics.md` | Working on metrics engine |
| Templates & scaffolding | `.claude/rules/templates.md` | Changing `pice init` output |

For deep architecture reference: `.claude/docs/`
For PICE methodology: `docs/methodology/`

---

## Key Rules

- **Never `unwrap()` in library code** — use `?` operator with proper error types. Panics in the CLI core are bugs.
- **stdout is the JSON-RPC channel for providers** — all provider logging goes to stderr. Writing to stdout breaks the protocol.
- **Provider failures must not crash the CLI** — gracefully degrade (single-model eval, warning messages) instead of panicking.
- **Evaluation sessions are context-isolated** — evaluator prompts must NEVER include implementation conversation or planning rationale. Only: contract, diff, CLAUDE.md.
- **Templates are embedded at build time** — changes to `templates/` require a rebuild. Use `rust-embed` or `include_str!`.
- **JSON-RPC protocol changes require both Rust and TS updates** — `pice-protocol` crate and `@pice/provider-protocol` package must stay in sync. Add roundtrip serialization tests for every new message type.
- **Never commit API keys or secrets** — auth is handled via environment variables or subscription OAuth flows, never hardcoded.
- **All CLI commands go through the provider protocol** — no direct SDK calls from Rust. The protocol IS the abstraction boundary.

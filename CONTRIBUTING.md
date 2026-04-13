# Contributing to PICE CLI

Thank you for your interest in contributing. This guide covers setup, project layout, contribution boundaries, and quality expectations.

## Development Setup

### Prerequisites

- Rust (stable toolchain)
- Node.js 22+ LTS
- pnpm 9+

### Clone and Build

```bash
git clone https://github.com/jacobmolz/pice.git
cd pice
cargo build
pnpm install
pnpm build
```

### Verify

```bash
cargo test
pnpm test
```

## Project Structure

```
crates/pice-cli/              Thin CLI adapter (arg parsing, terminal rendering, daemon RPC)
crates/pice-daemon/           Headless daemon (orchestrator, provider host, metrics, templates)
crates/pice-core/             Shared library (config, protocol types, pure logic — zero async)
crates/pice-protocol/         Shared JSON-RPC types for core↔provider communication
packages/provider-protocol/   Shared JSON-RPC types (TypeScript side)
packages/provider-base/       Provider utilities
packages/provider-claude-code/ Claude Code SDK provider
packages/provider-codex/      Codex/GPT evaluator provider
packages/provider-stub/       Echo provider for testing
templates/                    Files embedded in binary for pice init
```

## Contribution Boundaries

| Area | Directories | Language |
|------|-------------|----------|
| CLI adapter, daemon, core logic | `crates/`, `templates/` | Rust |
| Providers | `packages/` | TypeScript |
| JSON-RPC protocol | `crates/pice-protocol/` AND `packages/provider-protocol/` | Both |

**Protocol changes are the exception.** Any modification to JSON-RPC message types must be made on both the Rust and TypeScript sides, with roundtrip serialization tests added to each.

## Validation

Run the full validation suite before opening a PR. Every check must pass.

```bash
cargo fmt --check && cargo clippy -- -D warnings && cargo test && \
pnpm lint && pnpm typecheck && pnpm test && pnpm build && \
cargo build --release
```

Expected baseline: 271 Rust tests (1 ignored), 49 TypeScript tests, zero lint errors, zero warnings, clean release build.

## Testing

### Rust

- Unit tests live in inline `#[cfg(test)]` modules alongside the code they test.
- Integration tests live in `tests/`.
- Framework: built-in `#[test]` + `cargo test`.

### TypeScript

- Tests live in `__tests__/` directories or co-located `*.test.ts` files.
- Framework: Vitest.

### Coverage expectations

Every new public function needs at minimum:

1. One happy-path test
2. One edge-case test
3. One error-case test

### Provider testing

Provider tests must use the stub provider (`packages/provider-stub/`). Never depend on live API calls in tests or CI.

## Code Style

Follow the conventions documented in `CLAUDE.md`. The key points:

- **Rust**: `snake_case` files and functions, `PascalCase` types, `SCREAMING_SNAKE_CASE` constants.
- **TypeScript**: `kebab-case` files, `camelCase` functions, `PascalCase` types.
- **No `unwrap()` in library code** -- use the `?` operator with proper error types. `unwrap()` is acceptable only in tests.
- **stdout is the JSON-RPC channel for providers** -- all provider logging must go to stderr.
- **Provider failures must not crash the CLI** -- degrade gracefully instead of panicking.
- **Error handling**: Rust uses `thiserror` for library errors, `anyhow` for CLI-level errors. TypeScript uses typed errors via discriminated unions.

## Building Providers

Providers communicate with the PICE core over JSON-RPC via stdio. A provider declares its capabilities (`workflow`, `evaluation`, or both) during the `initialize` handshake.

To study the provider contract, see:

- `crates/pice-protocol/src/lib.rs` (Rust protocol types)
- `packages/provider-protocol/` (TypeScript protocol types)
- `packages/provider-stub/` (minimal reference implementation)

## Pull Request Process

1. **Branch from `main`.** Use a descriptive branch name (e.g., `fix/provider-timeout`, `feat/csv-export`).
2. **Keep PRs focused.** One logical change per PR.
3. **Pass the full validation suite** listed above.
4. **Write a descriptive title and summary.** Explain what changed and why.
5. **Link to an issue** if one exists.
6. **Protocol changes require both sides.** If your PR touches `pice-protocol` or `provider-protocol`, it must update both packages with matching roundtrip serialization tests.

## Commit Messages

Use conventional-style messages that describe the change:

- `feat:` for new features
- `fix:` for bug fixes
- `docs:` for documentation
- `refactor:` for restructuring without behavior change
- `test:` for test-only changes
- `chore:` for build, CI, or dependency updates

## License

By contributing, you agree that your contributions will be licensed under the same terms as this project.

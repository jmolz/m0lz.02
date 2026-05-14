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

### Local CI Preflight

For a Linux CI-equivalent run from macOS or Linux with Docker installed:

```bash
scripts/ci/local-linux.sh
```

The container uses Node 22, pnpm 9, Rust stable, `rustfmt`, and `clippy`.
It bind-mounts the checkout, puts the local-CI debug binary directory on
`PATH`, serializes daemon-spawning Rust tests with `RUST_TEST_THREADS=1`,
mounts temporary host-owned `node_modules` directories so Linux optional
dependencies do not overwrite the host install, writes Linux build artifacts
under `target/local-ci-*`, and runs the Linux Rust, TypeScript, Phase 8
acceptance, release-smoke, and README media gates. It defaults to `linux/amd64`
to match GitHub's `linux-x64` runner. Set
`PICE_LOCAL_CI_PLATFORM=linux/arm64` when local ARM speed matters more than
x64 parity.

Windows behavior cannot be reproduced by Linux Docker containers. To validate
Windows named-pipe, `.cmd`, PowerShell, and path behavior before a release tag,
run this script on a Windows VM, physical Windows host, or self-hosted runner:

```powershell
scripts/ci/windows-smoke.ps1
```

The same Windows smoke path is available as a manual GitHub Actions workflow:
`Windows Smoke`.

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

For Phase 8 release-readiness work, also run the acceptance and benchmark
gates before final review:

```bash
cargo test -p pice-daemon --test parallel_cohort_speedup_assertion -- --nocapture
cargo bench -p pice-daemon --bench parallel_cohort_speedup
node scripts/acceptance/metrics-schema-inventory.mjs
node scripts/acceptance/phase8-reference-projects.mjs
tar -czf /private/tmp/pice-release-smoke-local.tar.gz -C target/release pice pice-daemon
PICE_ARTIFACT_ARCHIVE=/private/tmp/pice-release-smoke-local.tar.gz PICE_NPM_PACK_SMOKE=1 node scripts/acceptance/release-artifact-smoke.mjs
node scripts/acceptance/readme-media-audit.mjs
```

Expected release baseline from the May 14, 2026 v0.8.2 validation run:

- Rust: `cargo test --workspace --all-targets` passed 1262 tests.
- Rust docs: `cargo test --workspace --doc` passed with 1 ignored documentation example.
- TypeScript: `pnpm test` passed 123 tests.
- Lint/typecheck/build: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `pnpm lint`, `pnpm typecheck`, `pnpm build`, and `cargo build --release` passed.
- Local Linux Docker preflight: `scripts/ci/local-linux.sh` passed end-to-end on `linux/amd64`.
- Release/CI tripwires: `release-workflow-policy`, `release-artifact-smoke`, and `local-ci-policy` Vitest suites passed.
- Windows validation: `Rust (windows-latest)` passed in main CI, and `Smoke x86_64-pc-windows-msvc` passed in the v0.8.2 release workflow.
- Phase 8 acceptance: metrics inventory, five-reference-project harness,
  npm pack artifact smoke, README media audit, speedup assertion, and Criterion
  benchmark passed. See `docs/releases/validation-evidence.json`.

Do not update these counts from memory or stale CI output.

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

Follow the conventions documented in repo guidance such as `AGENTS.md`. The key points:

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

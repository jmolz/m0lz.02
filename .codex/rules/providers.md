---
paths:
  - "packages/**"
---

# TypeScript Provider Rules

## Package Organization

- `@pice/provider-protocol` — JSON-RPC types. Published to npm. No runtime dependencies.
- `@pice/provider-base` — shared utilities (JSON-RPC message parsing, stdio transport, error formatting). Depends on `provider-protocol`.
- `@pice/provider-claude-code` — Claude Code SDK provider. Capabilities: `workflow` + `evaluation`.
- `@pice/provider-codex` — Codex/OpenAI provider. Capabilities: `evaluation` only.

## Provider Lifecycle

1. Rust core spawns the provider as a child process
2. Core sends `initialize` with config
3. Provider responds with `capabilities`
4. Core sends commands, provider responds via JSON-RPC
5. Core sends `shutdown`, provider exits cleanly

## stdio Protocol

- **stdin**: JSON-RPC messages from core (one JSON object per line)
- **stdout**: JSON-RPC responses and notifications TO the core
- **stderr**: Provider logging (never JSON-RPC). Use `console.error()` for all logging.
- NEVER write non-JSON-RPC data to stdout. This breaks the protocol.

## Auth

Providers handle their own authentication:
- Claude Code: SDK handles API key (`ANTHROPIC_API_KEY`) or OAuth (Max/Pro subscription)
- Codex: OpenAI API key (`OPENAI_API_KEY`) or existing Codex CLI subscription auth
- Providers must surface auth failures as JSON-RPC errors, not crash.

## BaseProvider API

- `BaseProvider<TConfig = unknown>` is generic on config type. Subclasses can declare their config shape: `class MyProvider extends BaseProvider<MyConfig>`.
- `requireInitialized()` takes no arguments. Call it at the start of any method handler that requires initialization.
- Shutdown uses `setImmediate(() => process.exit(0))` after returning the response, ensuring the stdout write flushes before the process exits.
- The transport preserves `.code` from thrown Error objects. If a handler throws `Object.assign(new Error('msg'), { code: -32000 })`, the client receives error code `-32000`, not `-32603`.

## Error Handling

- All errors surfaced as JSON-RPC error responses with error code and message.
- Provider crash = the core detects process exit and degrades gracefully.
- Network/API errors should include retry-after headers when available.
- Use `Object.assign(new Error('message'), { code: ERROR_CODE })` to throw errors with specific JSON-RPC error codes. The transport catch block extracts the code automatically.

## Testing

- Unit test JSON-RPC message serialization/deserialization roundtrips.
- Integration test against the stub/echo provider (no live API calls in CI).
- Provider-specific integration tests can use live APIs in a separate test target (not default CI).
- Both sync (`std::process`) and async (`tokio::process`) integration tests exist for the stub provider.

## CI Build Order Dependency

- **`pnpm install && pnpm build` must run before `cargo test`** in all CI workflows. Rust integration tests spawn `provider-stub` which requires compiled JS at `packages/provider-stub/dist/bin.js`. If this file doesn't exist, all provider integration tests fail with `MODULE_NOT_FOUND`.
- This applies to both `ci.yml` (main/PR) and `release.yml` (tag push). Both workflows have been fixed for this — do not reorder their steps.

---
paths:
  - "crates/pice-protocol/**"
  - "crates/pice-core/src/protocol/**"
  - "packages/provider-protocol/**"
---

# JSON-RPC Protocol Rules

## Two separate protocols (from v0.2 onward)

PICE v0.2+ has **two** JSON-RPC 2.0 protocols. Do not conflate them.

| Protocol | Scope | Transport | Crate | Consumers |
|----------|-------|-----------|-------|-----------|
| **Provider protocol** | Rust core ↔ AI provider | stdio of spawned child process | `pice-protocol` (Rust) + `@pice/provider-protocol` (TS) | Daemon ↔ Claude Code provider, Daemon ↔ Codex provider, Daemon ↔ community providers |
| **Daemon RPC** | CLI adapter ↔ daemon | Unix socket (macOS/Linux) / named pipe (Windows) | `pice-core::protocol` | CLI ↔ daemon, future dashboard adapter ↔ daemon, future CI adapter ↔ daemon |

The method namespaces do NOT overlap. `session/create` is a provider protocol method (core asks provider to create a session). `execute/create` is a daemon RPC method (CLI asks daemon to start an execution run). Never add daemon RPC methods to `pice-protocol`, and never add provider methods to `pice-core::protocol`.

## Provider protocol basics

- JSON-RPC 2.0 over stdio (stdin/stdout)
- One JSON object per line (newline-delimited)
- Requests have `id` (core waits for response). Notifications have no `id` (fire-and-forget).

## Sync Requirement

The Rust `pice-protocol` crate and the TS `@pice/provider-protocol` package define the SAME types. They MUST stay in sync:

- Every message type change requires updating BOTH packages
- CI must include a roundtrip test: serialize in Rust → deserialize in TS, and vice versa
- Version numbers of both packages must match

## Adding a New Method

1. Define the request/response types in `pice-protocol` (Rust)
2. Mirror the types in `@pice/provider-protocol` (TS)
3. Add roundtrip serialization tests in both languages
4. Update the provider capability declaration if the method requires a new capability
5. Update `@pice/provider-base` handler routing
6. Implement in at least one provider
7. Update `docs/providers/protocol.md`

## Streaming

Streaming responses use JSON-RPC notifications (no `id`):
- `response/chunk` — text chunks from AI session
- `evaluate/result` — evaluation findings

The core accumulates chunks via the `ProviderHost::on_notification()` callback. In the Rust host, notifications received during `request()` are forwarded to the registered handler. Without a handler, notifications are logged at `debug` level.

On the TS side, `StdioTransport.onNotification` is an optional callback for incoming notifications. Notifications are identified by the absence of the `id` field per JSON-RPC 2.0 spec.

## Error Codes

| Code | Meaning |
|------|---------|
| -32700 | Parse error (invalid JSON) |
| -32600 | Invalid request |
| -32601 | Method not found |
| -32602 | Invalid params |
| -32603 | Internal error |
| -32000 | Provider not initialized |
| -32001 | Session not found |
| -32002 | Authentication failed |
| -32003 | Rate limited (include retry-after in data) |
| -32004 | Model not available |

## v0.2 Provider Protocol Extensions (additive, backwards compatible)

v0.2 extends the provider protocol without breaking v0.1 providers. All additions are opt-in via capabilities.

### Extended `initialize` response

```jsonc
{
  "capabilities": {
    "workflow": true,
    "evaluation": true,
    "agentTeams": true,
    "layerAware": true,           // v0.2: provider understands layer-scoped sessions
    "seamChecks": ["schema_match", "openapi_compliance"],  // v0.2: seam check IDs the provider can run
    "models": ["claude-opus-4-6", "claude-sonnet-4-6"],
    "defaultEvalModel": "claude-opus-4-6"
  }
}
```

### Extended `session/create` params

```jsonc
{
  "workingDirectory": "/abs/path/to/worktree",     // v0.2: worktree path, not project root
  "layer": "backend",                                // v0.2: layer name
  "layerPaths": ["src/server/**", "lib/**"],        // v0.2: file globs scoped to this layer
  "contractPath": ".pice/contracts/backend.toml"   // v0.2: per-layer contract file
}
```

### Extended `evaluate/create` params

```jsonc
{
  "layer": "backend",
  "contract": { /* parsed layer contract */ },
  "diff": "...",
  "seamChecks": [                                   // v0.2: seam check specs for this layer's boundaries
    {
      "id": "schema_match",
      "boundary": "backend↔database",
      "config": { /* ... */ }
    }
  ]
}
```

### New notification: `manifest/event`

Providers emit structured events the daemon aggregates into the verification manifest.

```jsonc
{
  "jsonrpc": "2.0",
  "method": "manifest/event",
  "params": {
    "featureId": "auth-feature-20260410",
    "eventType": "pass_complete",  // layer_started | pass_complete | confidence_updated | seam_finding | gate_requested | layer_complete
    "layer": "backend",
    "data": { /* event-specific payload */ },
    "timestamp": "2026-04-10T10:23:11Z"
  }
}
```

### New method: `layer/detect` (optional)

Provider-contributed layer detection hints. The daemon's core detector calls this if the provider declares it. Used for framework-specific signals that belong to the provider (e.g., Rails-specific Active Record detection).

### Backwards compatibility

- A provider that does NOT declare `layerAware: true` is driven in "single virtual layer" fallback mode. The daemon sends `session/create` without `layer`/`layerPaths`/`contractPath`. Seam checks are skipped.
- The daemon synthesizes `manifest/event` from command boundaries when the provider doesn't emit them.
- v0.1 providers keep working without modification.

## Daemon RPC Methods (v0.2)

Daemon RPC is newline-delimited JSON-RPC 2.0 over Unix socket (`~/.pice/daemon.sock`) or named pipe (`\\.\pipe\pice-daemon`). See `.claude/rules/daemon.md` for transport, auth, and lifecycle rules.

| Method | Purpose |
|--------|---------|
| `daemon/health` | Liveness probe + version string |
| `daemon/shutdown` | Request orderly shutdown |
| `daemon/reload-config` | Re-read config files from disk |
| `execute/create` | Start a layer-aware execution run |
| `evaluate/create` | Start an evaluation run (optionally background) |
| `manifest/get` | Fetch full manifest for a feature |
| `manifest/list` | List all features with summary |
| `manifest/subscribe` / `manifest/unsubscribe` | Stream manifest events |
| `gate/list` | List pending gates |
| `gate/decide` | Submit gate decision (approve/reject/skip) |
| `worktree/list` / `worktree/prune` | Worktree management |
| `logs/stream` | Stream live log output |
| `validate/workflow` | Validate `.pice/workflow.yaml` |

**Authentication**: CLI reads bearer token from `~/.pice/daemon.token` (0600 permissions) and includes it in every request under a top-level `auth` field (not inside `params`). Daemon rejects any request without a valid token.

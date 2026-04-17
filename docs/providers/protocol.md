# PICE Provider Protocol Specification

This document specifies the JSON-RPC 2.0 protocol between the PICE CLI core (Rust) and provider processes (TypeScript). The protocol runs over stdio -- the core writes JSON-RPC messages to the provider's stdin and reads from the provider's stdout.

---

## Transport

- **Wire format**: Newline-delimited JSON. One JSON object per line.
- **stdin** (core to provider): JSON-RPC requests.
- **stdout** (provider to core): JSON-RPC responses and notifications.
- **stderr** (provider logging): All logging. Never write non-JSON-RPC data to stdout.

The core spawns providers via `tokio::process::Command` with piped stdin/stdout and inherited stderr.

---

## JSON-RPC 2.0 Envelope

### Request (Core to Provider)

Requests have an `id` field. The core waits for a response with the matching `id`.

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "session/create",
  "params": { "workingDirectory": "/path/to/project" }
}
```

The `id` is a number or string. The `params` field is omitted when a method takes no parameters.

### Response (Provider to Core)

```json
{"jsonrpc":"2.0","id":1,"result":{"sessionId":"abc-123"}}
```

Error response (the `id` is `null` when the request could not be parsed):

```json
{"jsonrpc":"2.0","id":1,"error":{"code":-32001,"message":"session not found: xyz-456","data":null}}
```

### Notification (Provider to Core)

Notifications have no `id` field. They are fire-and-forget.

```json
{"jsonrpc":"2.0","method":"response/chunk","params":{"sessionId":"abc-123","text":"## Plan\n"}}
```

---

## Core to Provider Requests

### `initialize`

Initialize the provider and declare capabilities. Always the first message sent.

| Param | Type | Required | Description |
|-------|------|----------|-------------|
| `config` | `object` | Yes | Provider config from `.pice/config.toml` |

**Result:** `{ capabilities: ProviderCapabilities, version: string }`

**ProviderCapabilities:**

| Field | Type | Description |
|-------|------|-------------|
| `workflow` | `boolean` | Supports workflow sessions (plan, execute, commit) |
| `evaluation` | `boolean` | Supports evaluation (contract grading) |
| `agentTeams` | `boolean` | Supports agent team evaluation (Tier 3) |
| `models` | `string[]` | Supported model identifiers |
| `defaultEvalModel` | `string?` | Default model for evaluation |

```json
// Request
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"config":{"defaultModel":"claude-opus-4-6"}}}

// Response
{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"workflow":true,"evaluation":true,"agentTeams":false,"models":["claude-opus-4-6","claude-sonnet-4-6"],"defaultEvalModel":"claude-opus-4-6"},"version":"0.1.0"}}
```

### `shutdown`

Gracefully shut down the provider. The provider cleans up and exits after sending the response. The core closes stdin after receiving the response. If the provider does not exit within the timeout, the core kills the process.

**Params:** None. **Result:** `null`

```json
{"jsonrpc":"2.0","id":5,"method":"shutdown"}
{"jsonrpc":"2.0","id":5,"result":null}
```

### `capabilities`

Query capabilities without re-initializing. **Params:** None. **Result:** `ProviderCapabilities`.

### `session/create`

Create a new AI session.

| Param | Type | Required | Description |
|-------|------|----------|-------------|
| `workingDirectory` | `string` | Yes | Absolute path to the project directory |
| `model` | `string` | No | Model override |
| `systemPrompt` | `string` | No | System prompt override |

**Result:** `{ sessionId: string }`

```json
{"jsonrpc":"2.0","id":2,"method":"session/create","params":{"workingDirectory":"/home/user/project","model":"claude-opus-4-6"}}
{"jsonrpc":"2.0","id":2,"result":{"sessionId":"claude-session-1"}}
```

### `session/send`

Send a message to an existing session. The provider calls the AI SDK, streams results as notifications, and returns `{ ok: true }` when complete.

| Param | Type | Required | Description |
|-------|------|----------|-------------|
| `sessionId` | `string` | Yes | Target session |
| `message` | `string` | Yes | Prompt or message text |

**Result:** `{ ok: boolean }`

During processing, the provider emits `response/chunk`, `response/tool_use`, and `response/complete` notifications before returning:

```json
{"jsonrpc":"2.0","id":3,"method":"session/send","params":{"sessionId":"claude-session-1","message":"Plan a JWT auth feature."}}
{"jsonrpc":"2.0","method":"response/chunk","params":{"sessionId":"claude-session-1","text":"## Authentication Plan\n\n"}}
{"jsonrpc":"2.0","method":"response/chunk","params":{"sessionId":"claude-session-1","text":"### 1. Token Generation\n"}}
{"jsonrpc":"2.0","method":"response/tool_use","params":{"sessionId":"claude-session-1","toolName":"Read","toolInput":{"path":"/home/user/project/src/auth.ts"}}}
{"jsonrpc":"2.0","method":"response/complete","params":{"sessionId":"claude-session-1","result":{"completed":true}}}
{"jsonrpc":"2.0","id":3,"result":{"ok":true}}
```

### `session/destroy`

Destroy a session and release resources.

| Param | Type | Required | Description |
|-------|------|----------|-------------|
| `sessionId` | `string` | Yes | Session to destroy |

**Result:** `null`

```json
{"jsonrpc":"2.0","id":4,"method":"session/destroy","params":{"sessionId":"claude-session-1"}}
{"jsonrpc":"2.0","id":4,"result":null}
```

### `evaluate/create`

Create an evaluation session. Evaluation sessions are context-isolated -- they receive only the contract, diff, and CLAUDE.md, never implementation conversation.

| Param | Type | Required | Description |
|-------|------|----------|-------------|
| `contract` | `object` | Yes | Contract JSON from the plan file |
| `diff` | `string` | Yes | Git diff of changes to evaluate |
| `claudeMd` | `string` | Yes | Project CLAUDE.md contents |
| `model` | `string` | No | Model override |
| `effort` | `string` | No | Effort level (e.g., `"high"`, `"xhigh"`) |
| `seamChecks` | `SeamCheckSpec[]` | No | Seam checks to run for this layer's boundaries (v0.2+) |
| `passIndex` | `number` | No | 0-indexed pass number within the adaptive loop (v0.4+). The stub provider uses this to index `PICE_STUB_SCORES`. Loop iterates passes 1..=N internally but wires 0..=N-1 for array compatibility. |
| `freshContext` | `boolean` | No | Recreate provider session, drop prior conversation (ADTS Level 1+, v0.4+) |
| `effortOverride` | `string` | No | Per-pass effort override (e.g. `"xhigh"` for ADTS Level 2, v0.4+). Takes precedence over `effort` when both are set. |

Each `SeamCheckSpec` is `{ id: string, boundary?: string, args?: object }`.
Providers that don't declare `seamChecks` capability should ignore this
field; the daemon runs seam checks in-process using its built-in registry
when the provider omits support. See [Seam Verification](../methodology/evaluate.md#seam-verification-v02)
and [Authoring Seam Checks](../guides/authoring-seam-checks.md).

**Result:** `{ sessionId: string, costUsd?: number, confidence?: number }`

Cost and confidence (v0.4+) are optional self-reports from the provider.
The daemon uses `costUsd` for budget enforcement and `confidence` for
adaptive halting. Providers without cost telemetry should omit these
fields — the daemon falls back to its own posterior estimate.

```json
{"jsonrpc":"2.0","id":4,"method":"evaluate/create","params":{"contract":{"criteria":[{"name":"Tests pass","threshold":7},{"name":"No unwrap in lib","threshold":8}]},"diff":"+fn new_feature() -> Result<()> {\n+    Ok(())\n+}","claudeMd":"# Rules\n- Never unwrap in lib code"}}
{"jsonrpc":"2.0","id":4,"result":{"sessionId":"claude-eval-1"}}
```

### `evaluate/score`

Trigger scoring on an evaluation session. Results arrive via the `evaluate/result` notification.

| Param | Type | Required | Description |
|-------|------|----------|-------------|
| `sessionId` | `string` | Yes | Evaluation session to score |

**Result:** `{ ok: boolean }`

---

## Provider to Core Notifications

Notifications have no `id` and receive no response.

### `response/chunk`

Streaming text chunk from an AI session.

| Param | Type | Description |
|-------|------|-------------|
| `sessionId` | `string` | Source session |
| `text` | `string` | Text chunk (may be partial words, lines, or paragraphs) |

### `response/complete`

Session finished producing output.

| Param | Type | Description |
|-------|------|-------------|
| `sessionId` | `string` | Completed session |
| `result` | `object` | Provider-specific result data |

### `response/tool_use`

AI used a tool during the session.

| Param | Type | Description |
|-------|------|-------------|
| `sessionId` | `string` | Session where the tool was used |
| `toolName` | `string` | Tool name (e.g., `"Read"`, `"Bash"`, `"Edit"`) |
| `toolInput` | `object` | Input parameters |
| `toolResult` | `object?` | Result of tool execution (optional) |

### `evaluate/result`

Evaluation scores for a session created with `evaluate/create`.

| Param | Type | Description |
|-------|------|-------------|
| `sessionId` | `string` | Evaluation session |
| `scores` | `CriterionScore[]` | Per-criterion scores |
| `passed` | `boolean` | Overall pass/fail |
| `summary` | `string?` | Human-readable summary (optional) |

**CriterionScore:** `{ name: string, score: number (0-10), threshold: number, passed: boolean, findings?: string }`

```json
{"jsonrpc":"2.0","method":"evaluate/result","params":{"sessionId":"claude-eval-1","scores":[{"name":"Tests pass","score":9,"threshold":7,"passed":true,"findings":"All 42 tests pass."},{"name":"No unwrap in lib","score":8,"threshold":8,"passed":true,"findings":"All error paths use the ? operator."}],"passed":true,"summary":"All criteria met."}}
```

### `metrics/event`

Metrics event for telemetry tracking.

| Param | Type | Description |
|-------|------|-------------|
| `type` | `string` | Event type identifier |
| `data` | `object` | Event-specific data |

```json
{"jsonrpc":"2.0","method":"metrics/event","params":{"type":"evaluation_complete","data":{"tier":2,"passed":true,"score_avg":8.5}}}
```

---

## Error Codes

### Standard JSON-RPC 2.0

| Code | Constant | Description |
|------|----------|-------------|
| `-32700` | `PARSE_ERROR` | Invalid JSON |
| `-32600` | `INVALID_REQUEST` | Missing or invalid `jsonrpc` version |
| `-32601` | `METHOD_NOT_FOUND` | Method does not exist |
| `-32602` | `INVALID_PARAMS` | Invalid or missing parameters |
| `-32603` | `INTERNAL_ERROR` | Unhandled provider error |

### PICE-Specific (-32000 to -32099)

| Code | Constant | Description |
|------|----------|-------------|
| `-32000` | `PROVIDER_NOT_INITIALIZED` | Method called before `initialize` |
| `-32001` | `SESSION_NOT_FOUND` | Session ID does not exist |
| `-32002` | `AUTH_FAILED` | Authentication failed (invalid key, expired token) |
| `-32003` | `RATE_LIMITED` | Rate limit exceeded; include `retryAfter` in `data` |
| `-32004` | `MODEL_NOT_AVAILABLE` | Requested model unavailable |

Rate-limiting errors should include retry guidance in the `data` field:

```json
{"jsonrpc":"2.0","id":3,"error":{"code":-32003,"message":"rate limited","data":{"retryAfter":30}}}
```

---

## Protocol Lifecycle

### Workflow Session

```
Core                              Provider
  |  initialize                      |
  |--------------------------------->|
  |  { capabilities, version }       |
  |<---------------------------------|
  |  session/create                  |
  |--------------------------------->|
  |  { sessionId }                   |
  |<---------------------------------|
  |  session/send                    |
  |--------------------------------->|
  |    response/chunk (notification) |
  |<---------------------------------|
  |    response/complete (notif.)    |
  |<---------------------------------|
  |  { ok: true }                    |
  |<---------------------------------|
  |  session/destroy                 |
  |--------------------------------->|
  |  null                            |
  |<---------------------------------|
  |  shutdown                        |
  |--------------------------------->|
  |  null                            |
  |<---------------------------------|
  |  [stdin closed, provider exits]  |
```

### Evaluation Session

```
Core                              Provider
  |  initialize                      |
  |--------------------------------->|
  |  { capabilities, version }       |
  |<---------------------------------|
  |  evaluate/create                 |
  |--------------------------------->|
  |  { sessionId }                   |
  |<---------------------------------|
  |  evaluate/score                  |
  |--------------------------------->|
  |    evaluate/result (notification)|
  |<---------------------------------|
  |  { ok: true }                    |
  |<---------------------------------|
  |  shutdown                        |
  |--------------------------------->|
  |  null                            |
  |<---------------------------------|
```

For dual-model adversarial evaluation (Tier 2+), the core launches two providers in parallel and merges their `evaluate/result` notifications into a unified report.

---

## Implementation Notes

- **Sync requirement**: The Rust `pice-protocol` crate and TypeScript `@pice/provider-protocol` package define identical types. Both must be updated together with roundtrip serialization tests.
- **Notification handling**: On the Rust side, `ProviderHost` forwards notifications to a `NotificationHandler` callback during `request()`. On the TS side, `StdioTransport.onNotification` is the callback.
- **Timeouts**: The core sets timeouts on all requests. The `shutdown` method splits its budget: up to 5s for the RPC, remainder for process exit. Hung providers are killed.
- **Graceful degradation**: Provider failures are non-fatal. If the adversarial provider fails, the core falls back to single-model evaluation with a warning.

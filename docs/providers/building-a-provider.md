# Building a PICE Provider

This guide walks through building a community provider for the PICE CLI. A provider translates the PICE JSON-RPC protocol into calls to an AI SDK.

---

## What Providers Do

`pice-daemon` handles state management, background jobs, config, metrics, templates, and provider lifecycle. It does not call AI SDKs directly. Instead, it spawns provider processes and communicates via JSON-RPC 2.0 on stdio. The `pice` CLI talks to the daemon over a separate socket protocol.

A provider is a standalone process that:

1. Reads JSON-RPC requests from stdin
2. Calls an AI SDK (Anthropic, OpenAI, or any other)
3. Streams results back as JSON-RPC notifications and responses on stdout
4. Logs to stderr (never stdout -- stdout is the JSON-RPC channel)

Reference implementations:

- `packages/provider-claude-code/` -- Workflow + evaluation using the Claude Agent SDK
- `packages/provider-codex/` -- Evaluation-only using the OpenAI SDK
- `packages/provider-stub/` -- Echo provider for testing (no AI SDK)

---

## Project Setup

Create a TypeScript package under `packages/` (structure: `src/index.ts`, `src/bin.ts`, `package.json`, `tsconfig.json`).

```json
{
  "name": "@pice/provider-myai",
  "version": "0.1.0",
  "type": "module",
  "main": "./dist/index.js",
  "bin": { "pice-provider-myai": "./dist/bin.js" },
  "scripts": { "build": "tsc", "typecheck": "tsc --noEmit" },
  "dependencies": {
    "@pice/provider-protocol": "workspace:*",
    "@pice/provider-base": "workspace:*",
    "myai-sdk": "^1.0.0"
  },
  "devDependencies": { "@types/node": "^22.0.0", "typescript": "^5.7.0" }
}
```

The two critical PICE dependencies:

- **`@pice/provider-protocol`** -- JSON-RPC type definitions (methods, param/result shapes, error codes). No runtime code.
- **`@pice/provider-base`** -- `BaseProvider` class and `StdioTransport` that handle JSON-RPC plumbing.

---

## Implementing the Provider

### Step 1: Declare Capabilities

Extend `BaseProvider` and implement `getCapabilities()`.

```typescript
// src/index.ts
import type { ProviderCapabilities } from '@pice/provider-protocol';
import { BaseProvider, StdioTransport } from '@pice/provider-base';

interface MyAIConfig {
  defaultModel?: string;
}

export class MyAIProvider extends BaseProvider<MyAIConfig> {
  constructor() {
    super('0.1.0');
  }

  getCapabilities(): ProviderCapabilities {
    return {
      workflow: true,
      evaluation: false,
      agentTeams: false,
      models: ['myai-large', 'myai-small'],
    };
  }

  protected registerHandlers(transport: StdioTransport): void {
    // Register method handlers (Steps 2-4)
  }
}
```

`BaseProvider<TConfig>` types your config object. The core passes config from `.pice/config.toml` during `initialize`. The base class automatically handles `initialize`, `shutdown`, and `capabilities` -- you implement everything else in `registerHandlers()`.

### Step 2: Handle Sessions

Register handlers for the session lifecycle. Call `this.requireInitialized()` first in every handler -- it throws a JSON-RPC error (`-32000`) if `initialize` has not been called.

```typescript
import type {
  SessionCreateParams,
  SessionSendParams,
  SessionDestroyParams,
} from '@pice/provider-protocol';
import { SESSION_NOT_FOUND } from '@pice/provider-protocol';

// Inside registerHandlers():

let nextId = 1;
const sessions = new Map<string, { id: string; model?: string; cwd: string }>();

transport.registerMethod('session/create', async (params: unknown) => {
  this.requireInitialized();
  const { workingDirectory, model } = params as SessionCreateParams;
  const sessionId = `myai-session-${nextId++}`;
  sessions.set(sessionId, {
    id: sessionId,
    model: model ?? this.config?.defaultModel,
    cwd: workingDirectory,
  });
  return { sessionId };
});

transport.registerMethod('session/send', async (params: unknown) => {
  this.requireInitialized();
  const { sessionId, message } = params as SessionSendParams;
  const session = sessions.get(sessionId);
  if (!session) {
    throw Object.assign(new Error(`session not found: ${sessionId}`), {
      code: SESSION_NOT_FOUND,
    });
  }

  // Call your AI SDK here (see Step 3 for streaming)
  const response = await callMyAI(message, session.model);

  transport.sendNotification('response/chunk', { sessionId, text: response.text });
  transport.sendNotification('response/complete', { sessionId, result: { completed: true } });
  return { ok: true };
});

transport.registerMethod('session/destroy', async (params: unknown) => {
  this.requireInitialized();
  const { sessionId } = params as SessionDestroyParams;
  sessions.delete(sessionId);
  return null;
});
```

### Step 3: Stream Responses

For real-time terminal output, send `response/chunk` notifications as the AI generates text.

```typescript
transport.registerMethod('session/send', async (params: unknown) => {
  this.requireInitialized();
  const { sessionId, message } = params as SessionSendParams;
  const session = sessions.get(sessionId);
  if (!session) {
    throw Object.assign(new Error(`session not found: ${sessionId}`), {
      code: SESSION_NOT_FOUND,
    });
  }

  const stream = await myAISDK.stream({ model: session.model, prompt: message });

  for await (const chunk of stream) {
    if (chunk.type === 'text') {
      transport.sendNotification('response/chunk', { sessionId, text: chunk.text });
    }
    if (chunk.type === 'tool_use') {
      transport.sendNotification('response/tool_use', {
        sessionId,
        toolName: chunk.toolName,
        toolInput: chunk.toolInput,
        toolResult: chunk.toolResult,
      });
    }
  }

  transport.sendNotification('response/complete', { sessionId, result: { completed: true } });
  return { ok: true };
});
```

Key points:
- Chunks can be any size. The core handles reassembly.
- Send `response/tool_use` when the AI invokes tools so the core can log activity.
- Always send `response/complete` before returning `{ ok: true }`.

### Step 4: Implement Evaluation (Optional)

Set `evaluation: true` in capabilities and implement `evaluate/create` and `evaluate/score`.

Evaluation sessions are context-isolated. They receive only contract JSON, filtered git diff, and project guidance text. Never include implementation conversation.
The `claudeMd` wire field is retained for v0.1 compatibility. v0.7.0 callers may populate it with generated `CLAUDE.md` contents or equivalent project guidance; providers should treat it as opaque guidance text.

```typescript
import type { EvaluateCreateParams, EvaluateScoreParams, CriterionScore } from '@pice/provider-protocol';
import { SESSION_NOT_FOUND, EVALUATE_RESULT } from '@pice/provider-protocol';

interface EvalSession {
  id: string;
  contract: unknown;
  diff: string;
  claudeMd: string;
  model: string;
}

const evalSessions = new Map<string, EvalSession>();

transport.registerMethod('evaluate/create', async (params: unknown) => {
  this.requireInitialized();
  const { contract, diff, claudeMd, model } = params as EvaluateCreateParams;
  const sessionId = `myai-eval-${nextId++}`;
  evalSessions.set(sessionId, {
    id: sessionId, contract, diff, claudeMd,
    model: model ?? this.config?.defaultModel ?? 'myai-large',
  });
  return { sessionId };
});

transport.registerMethod('evaluate/score', async (params: unknown) => {
  this.requireInitialized();
  const { sessionId } = params as EvaluateScoreParams;
  const session = evalSessions.get(sessionId);
  if (!session) {
    throw Object.assign(new Error(`eval session not found: ${sessionId}`), {
      code: SESSION_NOT_FOUND,
    });
  }

  const result = await myAISDK.evaluate({
    model: session.model,
    prompt: buildEvalPrompt(session.contract, session.diff, session.claudeMd),
  });

  const scores: CriterionScore[] = result.criteria.map((c) => ({
    name: c.name,
    score: c.score,
    threshold: c.threshold,
    passed: c.score >= c.threshold,
    findings: c.findings,
  }));

  transport.sendNotification(EVALUATE_RESULT, {
    sessionId,
    scores,
    passed: scores.every((s) => s.passed),
    summary: result.summary,
  });

  evalSessions.delete(sessionId);
  return { ok: true };
});
```

---

## Entry Point

```typescript
// src/bin.ts
#!/usr/bin/env node
import { MyAIProvider } from './index.js';

const provider = new MyAIProvider();
provider.start().catch((err) => {
  console.error('myai provider failed:', err);
  process.exit(1);
});
```

`start()` begins reading stdin and routing messages to handlers. The process runs until `shutdown` or stdin EOF.

---

## Error Handling

### JSON-RPC Error Codes

The transport extracts `.code` from thrown Error objects:

```typescript
import { SESSION_NOT_FOUND, AUTH_FAILED, RATE_LIMITED, MODEL_NOT_AVAILABLE } from '@pice/provider-protocol';

throw Object.assign(new Error(`session not found: ${id}`), { code: SESSION_NOT_FOUND });   // -32001
throw Object.assign(new Error('invalid API key'),          { code: AUTH_FAILED });          // -32002
throw Object.assign(new Error('rate limited'),             { code: RATE_LIMITED });         // -32003
throw Object.assign(new Error(`unavailable: ${model}`),    { code: MODEL_NOT_AVAILABLE });  // -32004
```

Without a `.code` property, the transport defaults to `-32603` (internal error).

### stdout Is Sacred

All logging goes to stderr. stdout is exclusively for JSON-RPC.

```typescript
console.error('connecting to MyAI API...');  // Correct
console.log('debug: something happened');    // Wrong -- breaks the protocol
```

### Cleanup on Shutdown

Override `onShutdown()` for resource cleanup. The base class calls `setImmediate(() => process.exit(0))` after the shutdown response flushes.

```typescript
protected async onShutdown(): Promise<void> {
  await this.sdkClient?.close();
}
```

---

## Testing

### Use the Stub Provider as Reference

`packages/provider-stub/` is a minimal echo provider. Study it to understand the protocol flow without AI SDK complexity.

### Unit Tests

```typescript
import { describe, it, expect } from 'vitest';

describe('MyAIProvider', () => {
  it('should declare correct capabilities', () => {
    const provider = new MyAIProvider();
    const caps = provider.getCapabilities();
    expect(caps.workflow).toBe(true);
    expect(caps.models).toContain('myai-large');
  });
});
```

### Integration Testing

Build the provider and test against the Rust host (`pnpm build && cargo test --test provider_integration`). Gate live API tests behind environment variables -- never call live APIs in default CI:

```typescript
const HAS_KEY = !!process.env.MYAI_API_KEY;
describe.skipIf(!HAS_KEY)('live API', () => { /* ... */ });
```

---

## Registration

Configure your provider in `.pice/config.toml`:

```toml
[provider]
name = "myai"
```

For evaluation providers:

```toml
[evaluation.adversarial]
provider = "myai"
model = "myai-large"
effort = "high"
enabled = true
```

The core resolves provider names to `pice-provider-{name}` binaries in the `packages/` directory relative to the PICE binary.

---

## Capability Matrix

| Capability | When to Enable | Methods to Implement |
|------------|---------------|---------------------|
| `workflow` | SDK supports interactive coding sessions | `session/create`, `session/send`, `session/destroy` |
| `evaluation` | SDK can grade code against criteria | `evaluate/create`, `evaluate/score` |
| `costTelemetry` | Provider can report real per-pass cost | `evaluate/create` result `costUsd` |
| `agentTeams` | SDK supports multi-agent orchestration | Required for Tier 3 evaluation |

v0.2 Stack Loops add optional `layer`, `layerPaths`, and `contractPath` fields
to `session/create`, plus `seamChecks` and adaptive pass fields to
`evaluate/create`. Providers that do not use these fields should ignore them;
the daemon owns manifests, seam execution fallback, review gates, and
`manifest/event` subscriptions.

A provider can support workflow only, evaluation only, or both.

---

## Checklist

- [ ] Extends `BaseProvider` and implements `getCapabilities()`
- [ ] All handlers call `this.requireInitialized()`
- [ ] Session errors use `SESSION_NOT_FOUND` (`-32001`)
- [ ] Auth errors use `AUTH_FAILED` (`-32002`)
- [ ] All logging goes to stderr
- [ ] `response/chunk` notifications stream during `session/send`
- [ ] `response/complete` sent before `session/send` returns
- [ ] `evaluate/result` sent before `evaluate/score` returns
- [ ] Evaluation sessions are context-isolated (contract + filtered diff + project guidance only)
- [ ] Entry point starts provider with `provider.start()`
- [ ] Tests cover happy path, edge cases, and error cases
- [ ] No live API calls in default test suite
- [ ] Compiles with `pnpm build` and passes `pnpm typecheck`

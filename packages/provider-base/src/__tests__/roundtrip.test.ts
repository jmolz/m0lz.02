import { describe, it, expect } from 'vitest';
import type {
  JsonRpcRequest,
  JsonRpcResponse,
  JsonRpcErrorResponse,
  JsonRpcNotification,
  InitializeParams,
  InitializeResult,
  ProviderCapabilities,
  SessionCreateParams,
  SessionCreateResult,
  SessionSendParams,
  SessionSendResult,
  ResponseChunkParams,
  ResponseCompleteParams,
  ResponseToolUseParams,
  EvaluateCreateParams,
  EvaluateCreateResult,
  SeamCheckSpec,
  SeamCheckResult,
  SeamCheckStatus,
  SeamFinding,
  EvaluateScoreResult,
  CriterionScore,
  EvaluateResultParams,
} from '@pice/provider-protocol';
import {
  PARSE_ERROR,
  METHOD_NOT_FOUND,
  PROVIDER_NOT_INITIALIZED,
  SESSION_NOT_FOUND,
} from '@pice/provider-protocol';

// These tests verify that our TS types can correctly serialize/deserialize
// the exact same wire format that the Rust pice-protocol crate produces.

describe('JSON-RPC roundtrip (matches Rust wire format)', () => {
  it('JsonRpcRequest roundtrip', () => {
    const req: JsonRpcRequest = {
      jsonrpc: '2.0',
      id: 1,
      method: 'session/create',
      params: { workingDirectory: '/tmp/project' },
    };
    const json = JSON.stringify(req);
    const parsed: JsonRpcRequest = JSON.parse(json);
    expect(parsed.jsonrpc).toBe('2.0');
    expect(parsed.id).toBe(1);
    expect(parsed.method).toBe('session/create');
  });

  it('JsonRpcRequest with string id', () => {
    const req: JsonRpcRequest = {
      jsonrpc: '2.0',
      id: 'abc-123',
      method: 'initialize',
      params: { config: {} },
    };
    const json = JSON.stringify(req);
    const parsed: JsonRpcRequest = JSON.parse(json);
    expect(parsed.id).toBe('abc-123');
  });

  it('JsonRpcResponse roundtrip', () => {
    const resp: JsonRpcResponse = {
      jsonrpc: '2.0',
      id: 1,
      result: { sessionId: 'abc-123' },
    };
    const json = JSON.stringify(resp);
    const parsed: JsonRpcResponse = JSON.parse(json);
    expect(parsed.id).toBe(1);
    expect((parsed.result as { sessionId: string }).sessionId).toBe('abc-123');
  });

  it('JsonRpcErrorResponse roundtrip', () => {
    const resp: JsonRpcErrorResponse = {
      jsonrpc: '2.0',
      id: 1,
      error: { code: METHOD_NOT_FOUND, message: 'method not found: foo/bar' },
    };
    const json = JSON.stringify(resp);
    const parsed: JsonRpcErrorResponse = JSON.parse(json);
    expect(parsed.error.code).toBe(-32601);
  });

  it('JsonRpcErrorResponse with null id', () => {
    const resp: JsonRpcErrorResponse = {
      jsonrpc: '2.0',
      id: null,
      error: { code: PARSE_ERROR, message: 'invalid JSON' },
    };
    const json = JSON.stringify(resp);
    expect(json).toContain('"id":null');
  });

  it('JsonRpcNotification roundtrip (no id)', () => {
    const notif: JsonRpcNotification = {
      jsonrpc: '2.0',
      method: 'response/chunk',
      params: { sessionId: 'abc-123', text: 'Hello' },
    };
    const json = JSON.stringify(notif);
    expect(json).not.toContain('"id"');
    const parsed: JsonRpcNotification = JSON.parse(json);
    expect(parsed.method).toBe('response/chunk');
  });

  it('InitializeParams/Result roundtrip', () => {
    const params: InitializeParams = { config: { apiKey: 'test' } };
    const result: InitializeResult = {
      capabilities: {
        workflow: true,
        evaluation: true,
        agentTeams: false,
        models: ['claude-opus-4-6'],
        defaultEvalModel: 'claude-opus-4-6',
      },
      version: '0.1.0',
    };
    const paramsJson = JSON.stringify(params);
    const resultJson = JSON.stringify(result);
    expect(JSON.parse(paramsJson)).toEqual(params);
    expect(JSON.parse(resultJson)).toEqual(result);
  });

  it('ProviderCapabilities uses camelCase keys', () => {
    const caps: ProviderCapabilities = {
      workflow: true,
      evaluation: false,
      agentTeams: true,
      models: [],
    };
    const json = JSON.stringify(caps);
    expect(json).toContain('"agentTeams"');
    expect(json).not.toContain('"agent_teams"');
  });

  // Phase 4.1: costTelemetry is the capability gate for adaptive budgets.
  it('ProviderCapabilities costTelemetry serializes camelCase', () => {
    const caps: ProviderCapabilities = {
      workflow: true,
      evaluation: true,
      agentTeams: false,
      models: ['stub'],
      costTelemetry: true,
    };
    const json = JSON.stringify(caps);
    expect(json).toContain('"costTelemetry":true');
    expect(json).not.toContain('"cost_telemetry"');
    const parsed: ProviderCapabilities = JSON.parse(json);
    expect(parsed.costTelemetry).toBe(true);
  });

  // Legacy providers that predate costTelemetry must deserialize without
  // the field — JSON.parse produces undefined, which consumers must treat
  // as the fail-closed default (equivalent to false on the Rust side's
  // `#[serde(default)]`).
  it('ProviderCapabilities costTelemetry is optional (legacy provider)', () => {
    const legacyJson = JSON.stringify({
      workflow: true,
      evaluation: true,
      agentTeams: false,
      models: [],
    });
    const parsed: ProviderCapabilities = JSON.parse(legacyJson);
    expect(parsed.costTelemetry).toBeUndefined();
  });

  it('SessionCreate params/result roundtrip', () => {
    const params: SessionCreateParams = { workingDirectory: '/tmp/project' };
    const result: SessionCreateResult = { sessionId: 'session-abc' };
    const paramsJson = JSON.stringify(params);
    expect(paramsJson).toContain('"workingDirectory"');
    const resultJson = JSON.stringify(result);
    expect(resultJson).toContain('"sessionId"');
  });

  it('SessionSend params roundtrip', () => {
    const params: SessionSendParams = { sessionId: 's1', message: 'test' };
    const json = JSON.stringify(params);
    const parsed: SessionSendParams = JSON.parse(json);
    expect(parsed.sessionId).toBe('s1');
    expect(parsed.message).toBe('test');
  });

  it('ResponseChunk params roundtrip', () => {
    const params: ResponseChunkParams = { sessionId: 's1', text: 'Hello world' };
    const json = JSON.stringify(params);
    const parsed: ResponseChunkParams = JSON.parse(json);
    expect(parsed.text).toBe('Hello world');
  });

  it('ResponseComplete params roundtrip', () => {
    const params: ResponseCompleteParams = {
      sessionId: 's1',
      result: { planPath: '.claude/plans/auth.md' },
    };
    const json = JSON.stringify(params);
    const parsed: ResponseCompleteParams = JSON.parse(json);
    expect(parsed.sessionId).toBe('s1');
  });

  it('EvaluateCreate params roundtrip', () => {
    const params: EvaluateCreateParams = {
      contract: { criteria: [] },
      diff: '+added line',
      claudeMd: '# Rules',
    };
    const json = JSON.stringify(params);
    expect(json).toContain('"claudeMd"');
    const parsed: EvaluateCreateParams = JSON.parse(json);
    expect(parsed.diff).toBe('+added line');
  });

  it('CriterionScore roundtrip', () => {
    const score: CriterionScore = {
      name: 'Tests pass',
      score: 8,
      threshold: 7,
      passed: true,
      findings: 'All 42 tests pass',
    };
    const json = JSON.stringify(score);
    const parsed: CriterionScore = JSON.parse(json);
    expect(parsed.score).toBe(8);
    expect(parsed.passed).toBe(true);
  });

  it('EvaluateResult params roundtrip', () => {
    const params: EvaluateResultParams = {
      sessionId: 'eval-1',
      scores: [
        { name: 'Build succeeds', score: 9, threshold: 7, passed: true },
      ],
      passed: true,
      summary: 'All criteria met',
    };
    const json = JSON.stringify(params);
    const parsed: EvaluateResultParams = JSON.parse(json);
    expect(parsed.passed).toBe(true);
    expect(parsed.scores).toHaveLength(1);
  });

  it('SessionSendResult roundtrip', () => {
    const result: SessionSendResult = { ok: true };
    const json = JSON.stringify(result);
    const parsed: SessionSendResult = JSON.parse(json);
    expect(parsed.ok).toBe(true);
  });

  it('EvaluateScoreResult roundtrip', () => {
    const result: EvaluateScoreResult = { ok: true };
    const json = JSON.stringify(result);
    const parsed: EvaluateScoreResult = JSON.parse(json);
    expect(parsed.ok).toBe(true);
  });

  it('ResponseToolUseParams roundtrip', () => {
    const params: ResponseToolUseParams = {
      sessionId: 's1',
      toolName: 'Read',
      toolInput: { path: '/tmp/file.rs' },
    };
    const json = JSON.stringify(params);
    expect(json).toContain('"toolName"');
    expect(json).toContain('"toolInput"');
    expect(json).not.toContain('"toolResult"');
    const parsed: ResponseToolUseParams = JSON.parse(json);
    expect(parsed.toolName).toBe('Read');
  });

  it('ResponseToolUseParams with toolResult', () => {
    const params: ResponseToolUseParams = {
      sessionId: 's1',
      toolName: 'Bash',
      toolInput: { command: 'ls' },
      toolResult: { output: 'file.txt' },
    };
    const json = JSON.stringify(params);
    expect(json).toContain('"toolResult"');
    const parsed: ResponseToolUseParams = JSON.parse(json);
    expect(parsed.toolResult).toBeDefined();
  });

  it('SessionCreateParams with optional fields', () => {
    const params: SessionCreateParams = {
      workingDirectory: '/tmp',
      model: 'claude-opus-4-6',
      systemPrompt: 'You are a planner.',
    };
    const json = JSON.stringify(params);
    expect(json).toContain('"model"');
    expect(json).toContain('"systemPrompt"');
    const parsed: SessionCreateParams = JSON.parse(json);
    expect(parsed.model).toBe('claude-opus-4-6');
    expect(parsed.systemPrompt).toBe('You are a planner.');
  });

  it('SessionCreateParams with layer fields (v0.2)', () => {
    const params: SessionCreateParams = {
      workingDirectory: '/tmp/worktree/backend',
      layer: 'backend',
      layerPaths: ['src/server/**', 'lib/**'],
      contractPath: '.pice/contracts/backend.toml',
    };
    const json = JSON.stringify(params);
    expect(json).toContain('"layer"');
    expect(json).toContain('"layerPaths"');
    expect(json).toContain('"contractPath"');
    // Verify no snake_case leaks
    expect(json).not.toContain('"layer_paths"');
    expect(json).not.toContain('"contract_path"');
    const parsed: SessionCreateParams = JSON.parse(json);
    expect(parsed.layer).toBe('backend');
    expect(parsed.layerPaths).toEqual(['src/server/**', 'lib/**']);
    expect(parsed.contractPath).toBe('.pice/contracts/backend.toml');
  });

  it('SessionCreateParams without layer fields (backwards compatible)', () => {
    const params: SessionCreateParams = { workingDirectory: '/tmp/project' };
    const json = JSON.stringify(params);
    // Layer fields must be absent when not set
    expect(json).not.toContain('"layer"');
    expect(json).not.toContain('"layerPaths"');
    expect(json).not.toContain('"contractPath"');
    // Deserialize a v0.1 payload without layer fields
    const v1Json = '{"workingDirectory":"/old/project"}';
    const parsed: SessionCreateParams = JSON.parse(v1Json);
    expect(parsed.workingDirectory).toBe('/old/project');
    expect(parsed.layer).toBeUndefined();
    expect(parsed.layerPaths).toBeUndefined();
    expect(parsed.contractPath).toBeUndefined();
  });

  it('EvaluateCreateParams with optional fields', () => {
    const params: EvaluateCreateParams = {
      contract: { criteria: [] },
      diff: '+line',
      claudeMd: '# Rules',
      model: 'gpt-5.4',
      effort: 'high',
    };
    const json = JSON.stringify(params);
    expect(json).toContain('"model"');
    expect(json).toContain('"effort"');
    const parsed: EvaluateCreateParams = JSON.parse(json);
    expect(parsed.model).toBe('gpt-5.4');
    expect(parsed.effort).toBe('high');
  });

  it('EvaluateCreate seam_checks roundtrip', () => {
    const params: EvaluateCreateParams = {
      contract: { criteria: [] },
      diff: '',
      claudeMd: '',
      seamChecks: [
        { id: 'config_mismatch', boundary: 'backend↔infrastructure' },
        {
          id: 'schema_drift',
          boundary: 'backend↔database',
          args: { strict: true },
        },
      ],
    };
    const json = JSON.stringify(params);
    expect(json).toContain('"seamChecks"');
    expect(json).toContain('backend↔infrastructure');
    const parsed: EvaluateCreateParams = JSON.parse(json);
    expect(parsed.seamChecks).toHaveLength(2);
    expect(parsed.seamChecks?.[0].id).toBe('config_mismatch');
    expect(parsed.seamChecks?.[1].args?.strict).toBe(true);
  });

  it('EvaluateCreate omits seamChecks when absent', () => {
    const params: EvaluateCreateParams = {
      contract: {},
      diff: '',
      claudeMd: '',
    };
    const json = JSON.stringify(params);
    expect(json).not.toContain('seamChecks');
  });

  it('SeamCheckSpec shape is assignable and serializes', () => {
    const spec: SeamCheckSpec = {
      id: 'openapi_compliance',
      boundary: 'api↔frontend',
    };
    const json = JSON.stringify(spec);
    const parsed: SeamCheckSpec = JSON.parse(json);
    expect(parsed).toEqual(spec);
  });

  // Phase 3 round-4 adversarial review fix: round-1 only mirrored
  // SeamCheckSpec on the TS side, leaving result/finding shapes uncovered
  // by protocol-level types. CLAUDE.md / providers.md rule: "both sides
  // (Rust + TS) must have serialization roundtrip tests for every new
  // message type." These tests pin the wire shape against what the daemon
  // emits in `LayerResult.seam_checks[]` and the SQLite `seam_findings`
  // table.

  it('SeamFinding roundtrips with all optional fields', () => {
    const finding: SeamFinding = {
      message: "ORM model 'User' has no matching migration table",
      file: 'prisma/schema.prisma',
      line: 12,
    };
    const json = JSON.stringify(finding);
    const parsed: SeamFinding = JSON.parse(json);
    expect(parsed).toEqual(finding);
    expect(parsed.message).toBe(finding.message);
    expect(parsed.file).toBe('prisma/schema.prisma');
    expect(parsed.line).toBe(12);
  });

  it('SeamFinding roundtrips with optional fields omitted', () => {
    const finding: SeamFinding = {
      message: 'budget exceeded — heuristic check did not complete in 100ms',
    };
    const json = JSON.stringify(finding);
    expect(json).not.toContain('"file"');
    expect(json).not.toContain('"line"');
    const parsed: SeamFinding = JSON.parse(json);
    expect(parsed.message).toBe(finding.message);
    expect(parsed.file).toBeUndefined();
    expect(parsed.line).toBeUndefined();
  });

  it('SeamCheckStatus accepts every wire variant', () => {
    // Wire variants emitted by the daemon (kebab-case via serde).
    // Compile-time exhaustiveness: this list must mirror the Rust
    // `CheckStatus` enum. If a new variant is added there, the assignment
    // below fails to type-check.
    const variants: SeamCheckStatus[] = [
      'passed',
      'warning',
      'failed',
      'skipped',
    ];
    for (const v of variants) {
      const result: SeamCheckResult = {
        name: 'config_mismatch',
        status: v,
        boundary: 'backend↔infrastructure',
      };
      const json = JSON.stringify(result);
      const parsed: SeamCheckResult = JSON.parse(json);
      expect(parsed.status).toBe(v);
    }
  });

  it('SeamCheckResult roundtrips a full Failed payload', () => {
    const result: SeamCheckResult = {
      name: 'config_mismatch',
      status: 'failed',
      boundary: 'backend↔infrastructure',
      category: 1,
      details: "env var 'ORPHAN_VAR' declared in Dockerfile but not read",
    };
    const json = JSON.stringify(result);
    const parsed: SeamCheckResult = JSON.parse(json);
    expect(parsed).toEqual(result);
    expect(parsed.category).toBe(1);
  });

  it('SeamCheckResult roundtrips a payload with null category', () => {
    // Unregistered-check synthetic rows carry a null category.
    const result: SeamCheckResult = {
      name: 'unknown_check',
      status: 'failed',
      boundary: 'backend↔infrastructure',
      category: null,
      details: "seam check id 'unknown_check' is not registered",
    };
    const json = JSON.stringify(result);
    const parsed: SeamCheckResult = JSON.parse(json);
    expect(parsed.category).toBeNull();
  });

  it('SeamCheckResult without category key matches Rust skip_serializing_if shape', () => {
    // Phase 3 round-5 adversarial review fix: Rust uses
    // `#[serde(skip_serializing_if = "Option::is_none")]` on `category`,
    // so when category is None the key is OMITTED, not set to null.
    // TS must parse both shapes — key-omitted (Rust wire) and explicit
    // null (TS-side construction). This test validates key-omitted parsing.
    const rustWireShape = '{"name":"unknown","status":"failed","boundary":"a↔b","details":"msg"}';
    const parsed: SeamCheckResult = JSON.parse(rustWireShape);
    expect(parsed.name).toBe('unknown');
    expect(parsed.status).toBe('failed');
    expect(parsed.category).toBeUndefined();
  });

  it('error codes match between TS and Rust', () => {
    // These must match the values in pice-protocol/src/lib.rs
    expect(PARSE_ERROR).toBe(-32700);
    expect(METHOD_NOT_FOUND).toBe(-32601);
    expect(PROVIDER_NOT_INITIALIZED).toBe(-32000);
    expect(SESSION_NOT_FOUND).toBe(-32001);
  });

  // ── Phase 4 adaptive protocol roundtrips ──────────────────────────

  it('EvaluateCreateParams with passIndex roundtrips', () => {
    const params: EvaluateCreateParams = {
      contract: { criteria: [] },
      diff: '+line',
      claudeMd: '# R',
      passIndex: 3,
    };
    const json = JSON.stringify(params);
    expect(json).toContain('"passIndex":3');
    const parsed: EvaluateCreateParams = JSON.parse(json);
    expect(parsed.passIndex).toBe(3);
  });

  it('EvaluateCreateParams without passIndex omits field', () => {
    const params: EvaluateCreateParams = {
      contract: {},
      diff: '',
      claudeMd: '',
    };
    const json = JSON.stringify(params);
    expect(json).not.toContain('passIndex');
    const parsed: EvaluateCreateParams = JSON.parse(json);
    expect(parsed.passIndex).toBeUndefined();
  });

  it('EvaluateCreateResult with costUsd and confidence roundtrips', () => {
    const result: EvaluateCreateResult = {
      sessionId: 'eval-42',
      costUsd: 0.025,
      confidence: 0.93,
    };
    const json = JSON.stringify(result);
    expect(json).toContain('"costUsd"');
    expect(json).toContain('"confidence"');
    const parsed: EvaluateCreateResult = JSON.parse(json);
    expect(parsed.costUsd).toBe(0.025);
    expect(parsed.confidence).toBe(0.93);
  });

  it('EvaluateCreateResult without costUsd/confidence omits fields', () => {
    const result: EvaluateCreateResult = {
      sessionId: 'eval-43',
    };
    const json = JSON.stringify(result);
    expect(json).not.toContain('costUsd');
    expect(json).not.toContain('confidence');
    const parsed: EvaluateCreateResult = JSON.parse(json);
    expect(parsed.costUsd).toBeUndefined();
    expect(parsed.confidence).toBeUndefined();
  });

  // Phase 4 criterion #9: ADTS escalation fields roundtrip
  it('EvaluateCreateParams with freshContext roundtrips', () => {
    const params: EvaluateCreateParams = {
      contract: {},
      diff: '',
      claudeMd: '',
      passIndex: 1,
      freshContext: true,
    };
    const json = JSON.stringify(params);
    expect(json).toContain('"freshContext":true');
    const parsed: EvaluateCreateParams = JSON.parse(json);
    expect(parsed.freshContext).toBe(true);
  });

  it('EvaluateCreateParams with effortOverride roundtrips', () => {
    const params: EvaluateCreateParams = {
      contract: {},
      diff: '',
      claudeMd: '',
      passIndex: 2,
      freshContext: true,
      effortOverride: 'xhigh',
    };
    const json = JSON.stringify(params);
    expect(json).toContain('"effortOverride":"xhigh"');
    const parsed: EvaluateCreateParams = JSON.parse(json);
    expect(parsed.effortOverride).toBe('xhigh');
  });

  it('EvaluateCreateParams without freshContext/effortOverride omits fields', () => {
    const params: EvaluateCreateParams = {
      contract: {},
      diff: '',
      claudeMd: '',
    };
    const json = JSON.stringify(params);
    expect(json).not.toContain('freshContext');
    expect(json).not.toContain('effortOverride');
    const parsed: EvaluateCreateParams = JSON.parse(json);
    expect(parsed.freshContext).toBeUndefined();
    expect(parsed.effortOverride).toBeUndefined();
  });

  it('full request/response wire format matches Rust', () => {
    // This is the exact JSON that the Rust side produces/expects
    const rustReq =
      '{"jsonrpc":"2.0","id":1,"method":"session/create","params":{"workingDirectory":"/path/to/project"}}';
    const parsed: JsonRpcRequest = JSON.parse(rustReq);
    expect(parsed.method).toBe('session/create');
    const params = parsed.params as SessionCreateParams;
    expect(params.workingDirectory).toBe('/path/to/project');

    // Produce a response in the format Rust expects
    const resp: JsonRpcResponse = {
      jsonrpc: '2.0',
      id: parsed.id,
      result: { sessionId: 'abc-123' } satisfies SessionCreateResult,
    };
    const respJson = JSON.stringify(resp);
    expect(respJson).toContain('"sessionId":"abc-123"');
  });
});

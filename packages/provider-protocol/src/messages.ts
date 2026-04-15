// ─── JSON-RPC 2.0 Core Types ────────────────────────────────────────────────

export type RequestId = number | string;

export interface JsonRpcRequest {
  jsonrpc: '2.0';
  id: RequestId;
  method: string;
  params?: unknown;
}

export interface JsonRpcResponse {
  jsonrpc: '2.0';
  id: RequestId;
  result: unknown;
}

export interface JsonRpcErrorResponse {
  jsonrpc: '2.0';
  id: RequestId | null;
  error: JsonRpcError;
}

export interface JsonRpcError {
  code: number;
  message: string;
  data?: unknown;
}

export interface JsonRpcNotification {
  jsonrpc: '2.0';
  method: string;
  params?: unknown;
}

// ─── Provider-Specific Message Types ─────────────────────────────────────────

export interface InitializeParams {
  config: unknown;
}

export interface InitializeResult {
  capabilities: ProviderCapabilities;
  version: string;
}

export interface ProviderCapabilities {
  workflow: boolean;
  evaluation: boolean;
  agentTeams: boolean;
  models: string[];
  defaultEvalModel?: string;
}

export interface SessionCreateParams {
  workingDirectory: string;
  model?: string;
  systemPrompt?: string;
  layer?: string;           // v0.2: layer name for stack loops
  layerPaths?: string[];    // v0.2: glob patterns for this layer's files
  contractPath?: string;    // v0.2: path to layer contract
}

export interface SessionCreateResult {
  sessionId: string;
}

export interface SessionSendParams {
  sessionId: string;
  message: string;
}

export interface SessionSendResult {
  ok: boolean;
}

export interface SessionDestroyParams {
  sessionId: string;
}

export interface ResponseToolUseParams {
  sessionId: string;
  toolName: string;
  toolInput: unknown;
  toolResult?: unknown;
}

export interface ResponseChunkParams {
  sessionId: string;
  text: string;
}

export interface ResponseCompleteParams {
  sessionId: string;
  result: unknown;
}

export interface EvaluateCreateParams {
  contract: unknown;
  diff: string;
  claudeMd: string;
  model?: string;
  effort?: string;
  /**
   * Seam check specs for this layer's boundaries. Omitted for v0.1 providers;
   * the daemon tolerates absence and defaults to no seam verification.
   */
  seamChecks?: SeamCheckSpec[];
}

/**
 * Per-boundary seam check specification. Mirrored from `pice-protocol` Rust
 * crate so the TS + Rust sides stay in sync (see `.claude/rules/protocol.md`).
 */
export interface SeamCheckSpec {
  id: string;
  boundary?: string;
  args?: Record<string, unknown>;
}

export interface EvaluateCreateResult {
  sessionId: string;
}

export interface EvaluateScoreParams {
  sessionId: string;
}

export interface EvaluateScoreResult {
  ok: boolean;
}

export interface CriterionScore {
  name: string;
  score: number;
  threshold: number;
  passed: boolean;
  findings?: string;
}

export interface EvaluateResultParams {
  sessionId: string;
  scores: CriterionScore[];
  passed: boolean;
  summary?: string;
}

// ─── Method Constants ────────────────────────────────────────────────────────

// Core → Provider requests
export const INITIALIZE = 'initialize';
export const SHUTDOWN = 'shutdown';
export const CAPABILITIES = 'capabilities';
export const SESSION_CREATE = 'session/create';
export const SESSION_SEND = 'session/send';
export const SESSION_DESTROY = 'session/destroy';
export const EVALUATE_CREATE = 'evaluate/create';
export const EVALUATE_SCORE = 'evaluate/score';

// Provider → Core notifications
export const RESPONSE_CHUNK = 'response/chunk';
export const RESPONSE_COMPLETE = 'response/complete';
export const RESPONSE_TOOL_USE = 'response/tool_use';
export const EVALUATE_RESULT = 'evaluate/result';
export const METRICS_EVENT = 'metrics/event';

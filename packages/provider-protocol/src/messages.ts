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

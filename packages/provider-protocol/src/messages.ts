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
  /**
   * Phase 4.1: whether the provider emits real per-pass `costUsd` on
   * `evaluate/create` responses. Adaptive evaluation with `budget_usd > 0`
   * REQUIRES this — otherwise budget enforcement is synthetic (seed-only)
   * and `final_total_cost_usd` misrepresents real spend. Default `false`;
   * providers must explicitly opt in.
   */
  costTelemetry?: boolean;
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
  /**
   * 0-indexed pass number within an adaptive evaluation loop. Advisory.
   * The stub provider uses this to index `PICE_STUB_SCORES`.
   */
  passIndex?: number;
  /**
   * ADTS Level 1+ signal: drop prior-pass context on this pass. Set on the
   * pass following a divergent-score detection so the evaluator re-examines
   * the contract from scratch rather than anchoring on the previous reply.
   * Phase 4 ADTS three-level escalation.
   */
  freshContext?: boolean;
  /**
   * ADTS Level 2 signal: override `effort` for this pass only (typically
   * `"xhigh"`). Takes precedence over the session-level `effort` when both
   * are set.
   */
  effortOverride?: string;
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

/**
 * A single observation from a seam check. Mirrors `SeamFinding` in the Rust
 * `pice-core::seam` crate. Phase 3 round-4 adversarial review fix: the
 * round-1 implementation only mirrored `SeamCheckSpec` on the TS side,
 * leaving result/finding shapes without protocol-level type coverage.
 */
export interface SeamFinding {
  message: string;
  /** Repository-relative path implicated by the finding, if any. */
  file?: string;
  /** 1-indexed line number within `file`, if known. */
  line?: number;
}

/**
 * Per-check status reported by the seam runner. Mirrors `CheckStatus` in
 * Rust (`pice-core::layers::manifest`). Wire form is kebab-case via serde.
 */
export type SeamCheckStatus = "passed" | "warning" | "failed" | "skipped";

/**
 * Result of running a single seam check on one boundary. Mirrors
 * `SeamCheckResult` in Rust (`pice-core::layers::manifest`). Carried inside
 * `LayerResult.seam_checks[]` and persisted to SQLite by the daemon.
 */
export interface SeamCheckResult {
  /** Check id (registry key, e.g. `"config_mismatch"`). */
  name: string;
  status: SeamCheckStatus;
  /** Canonical boundary string (`"a↔b"` with `a ≤ b` alphabetically). */
  boundary: string;
  /** PRDv2 category 1..=12, or null for unregistered-check synthetic rows. */
  category?: number | null;
  details?: string | null;
}

export interface EvaluateCreateResult {
  sessionId: string;
  /** Estimated cost in USD for this pass. Provider-reported. */
  costUsd?: number;
  /** Provider's own confidence estimate (0.0–1.0) for this pass. */
  confidence?: number;
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

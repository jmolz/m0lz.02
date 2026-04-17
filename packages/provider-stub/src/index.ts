import type { ProviderCapabilities, EvaluateCreateParams } from '@pice/provider-protocol';
import { BaseProvider, StdioTransport } from '@pice/provider-base';
import { parseStubScores, getStubEntry, type StubScoreEntry } from './deterministic.js';
import { appendFileSync } from 'node:fs';

let nextSessionId = 1;

/**
 * Stub/echo provider for testing the PICE protocol.
 *
 * - Responds to `session/create` with a fake session ID
 * - Responds to `session/send` by echoing the message back as
 *   `response/chunk` notifications followed by `response/complete`
 * - Declares no real capabilities (workflow: false, evaluation: false)
 */
/**
 * Per-session state for stub evaluations. The score used at `evaluate/score`
 * time depends on the pass index declared at `evaluate/create` time — so we
 * resolve the `PICE_STUB_SCORES` entry once at create and stash it here.
 */
interface StubEvalState {
  contract: unknown;
  /** 0-indexed pass position from `evaluate/create` params (defaults to 0). */
  passIndex: number;
  /**
   * Pre-resolved stub entry for this pass. `undefined` when `PICE_STUB_SCORES`
   * is unset — `evaluate/score` then falls back to `defaultScore = 8`.
   */
  stubEntry?: StubScoreEntry;
}

export class StubProvider extends BaseProvider {
  private evalContracts = new Map<string, StubEvalState>();
  private stubScores: StubScoreEntry[];
  /**
   * Phase 4 ADTS test harness: when set, the stub returns scores from this
   * list for models whose name contains `"adversarial"` (case-insensitive).
   * Primary providers keep using `PICE_STUB_SCORES`. This lets ADTS
   * integration tests drive divergent primary/adversarial score sequences
   * through one stub binary — the alternative of two separate stub binaries
   * with per-process env vars would need provider-host plumbing changes.
   */
  private adversarialScores: StubScoreEntry[];
  /**
   * Phase 4 context-isolation test harness: when `PICE_STUB_REQUEST_LOG`
   * points at a file path, every `evaluate/create` request's payload
   * (contract, diff, claudeMd, passIndex, freshContext, effortOverride,
   * model) is appended as one JSON line. Callers parse the file to verify
   * byte-identical prompts across passes.
   */
  private requestLogPath: string | undefined;

  constructor(version: string) {
    super(version);
    const raw = process.env['PICE_STUB_SCORES'];
    this.stubScores = raw ? parseStubScores(raw) : [];
    const advRaw = process.env['PICE_STUB_ADVERSARIAL_SCORES'];
    this.adversarialScores = advRaw ? parseStubScores(advRaw) : [];
    this.requestLogPath = process.env['PICE_STUB_REQUEST_LOG'] || undefined;
  }

  getCapabilities(): ProviderCapabilities {
    return {
      workflow: true,
      evaluation: true,
      agentTeams: false,
      models: ['stub-echo'],
    };
  }

  protected registerHandlers(transport: StdioTransport): void {
    transport.registerMethod('session/create', async (_params: unknown) => {
      this.requireInitialized();
      const sessionId = `stub-session-${nextSessionId++}`;
      return { sessionId };
    });

    transport.registerMethod('session/send', async (params: unknown) => {
      this.requireInitialized();
      const { sessionId, message } = params as { sessionId: string; message: string };

      // Echo the message back as a chunk notification
      transport.sendNotification('response/chunk', {
        sessionId,
        text: message,
      });

      // Send completion
      transport.sendNotification('response/complete', {
        sessionId,
        result: { echo: message },
      });

      return { ok: true };
    });

    transport.registerMethod('session/destroy', async (_params: unknown) => {
      this.requireInitialized();
      return null;
    });

    transport.registerMethod('evaluate/create', async (params: unknown) => {
      this.requireInitialized();
      const sessionId = `stub-eval-${nextSessionId++}`;
      const p = params as EvaluateCreateParams;

      // Role selection for ADTS: if the model string contains "adversarial"
      // (case-insensitive) AND PICE_STUB_ADVERSARIAL_SCORES is set, use that
      // list. Otherwise fall back to PICE_STUB_SCORES. Kept opt-in so legacy
      // tests that don't set the adversarial list behave identically.
      const isAdversarial =
        typeof p.model === 'string' && p.model.toLowerCase().includes('adversarial');
      const scoreList =
        isAdversarial && this.adversarialScores.length > 0
          ? this.adversarialScores
          : this.stubScores;

      const passIndex = p.passIndex ?? 0;
      const entry = getStubEntry(scoreList, passIndex);

      // Request log: one JSON line per call, for context-isolation tests.
      // Failures are logged to stderr but never propagated — telemetry writes
      // must never crash the provider (mirrors the daemon's metrics pattern).
      if (this.requestLogPath) {
        try {
          appendFileSync(
            this.requestLogPath,
            JSON.stringify({
              sessionId,
              model: p.model ?? null,
              effort: p.effort ?? null,
              effortOverride: p.effortOverride ?? null,
              freshContext: p.freshContext ?? null,
              passIndex,
              contract: p.contract,
              diff: p.diff,
              claudeMd: p.claudeMd,
            }) + '\n',
          );
        } catch (err) {
          console.error(`[stub] request log append failed: ${String(err)}`);
        }
      }

      this.evalContracts.set(sessionId, {
        contract: p.contract,
        passIndex,
        stubEntry: entry,
      });

      return {
        sessionId,
        ...(entry ? { costUsd: entry.cost, confidence: entry.score / 10.0 } : {}),
      };
    });

    transport.registerMethod('evaluate/score', async (params: unknown) => {
      this.requireInitialized();
      const { sessionId } = params as { sessionId: string };

      // Default score if `PICE_STUB_SCORES` is not configured. Kept at 8 for
      // backward compatibility with pre-Phase-4 tests; the Phase 4 adaptive
      // loop integration tests SHOULD set `PICE_STUB_SCORES` for determinism.
      const defaultScore = 8;
      const state = this.evalContracts.get(sessionId);
      // Use the per-pass stub score (rounded to nearest integer for the
      // 0–10 `CriterionScore.score` wire type) when set; else fall back.
      const rawScore = state?.stubEntry?.score ?? defaultScore;
      const passScore = Math.max(0, Math.min(10, Math.round(rawScore)));
      const contract = state?.contract as
        | { criteria?: Array<{ name: string; threshold: number }> }
        | undefined;
      const criteria = contract?.criteria ?? [];
      const scores = criteria.length > 0
        ? criteria.map((c: { name: string; threshold: number }) => ({
            name: c.name,
            score: passScore,
            threshold: c.threshold,
            passed: passScore >= c.threshold,
            findings: 'Stub evaluation — scored via PICE_STUB_SCORES or default',
          }))
        : [{
            name: 'stub-criterion',
            score: passScore,
            threshold: 7,
            passed: passScore >= 7,
            findings: 'Stub evaluation',
          }];

      transport.sendNotification('evaluate/result', {
        sessionId,
        scores,
        // `passed` reflects the effective pass score, not a hard-coded true.
        // Phase 4 tests that expect SPRT-rejected need this to swing false.
        passed: scores.every((s) => s.passed),
        summary: 'Stub evaluation complete',
      });

      this.evalContracts.delete(sessionId);
      return { ok: true };
    });
  }
}

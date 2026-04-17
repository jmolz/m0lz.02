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
  /**
   * Phase 4 Pass-3 Codex Critical #2 regression harness: when set, the stub
   * responds to `evaluate/create` with a JSON-RPC error whose message is
   * this value. The provider's `initialize`/`session/*` methods still work
   * normally, so the daemon sees a successful spawn + successful init, then
   * a runtime error on the first evaluation — exactly the shape that
   * distinguishes `LayerAdaptiveResult::RuntimeError` from
   * `::NotStarted`. Used only by `runtime_error_fails_layer_not_pending`.
   */
  private evaluateError: string | undefined;
  /**
   * Phase 4 Pass-4 Codex High regression harness: when set, the stub throws
   * the configured `evaluateError` only from pass `>=` this value (1-indexed,
   * matching `PassResult.index`). Lets tests exercise the mid-loop error
   * path where passes 1..N-1 succeed and pass N fails, so the adaptive loop
   * must preserve already-recorded passes + pass_events instead of
   * discarding them via the earlier `runtime_failed_layer_result` placeholder
   * path. Consumed only by
   * `mid_loop_runtime_error_preserves_prior_passes_and_costs`.
   */
  private evaluateErrorFromPass: number | undefined;
  /**
   * Phase 4 Pass-5 Codex Critical #2 regression harness: when set, the stub
   * emits an extra `evaluate/result` notification with THIS value as the
   * sessionId (a deliberately wrong one) BEFORE emitting the real result in
   * `evaluate/score`. A correct session-correlation filter on the Rust side
   * ignores the stale one and waits for the notification whose sessionId
   * matches the value returned by `evaluate/create`. A naive "take first
   * matching method" handler captures the stale notification and corrupts
   * the pass outcome — exactly the Critical #2 failure mode.
   */
  private staleResultSessionId: string | undefined;

  constructor(version: string) {
    super(version);
    const raw = process.env['PICE_STUB_SCORES'];
    this.stubScores = raw ? parseStubScores(raw) : [];
    const advRaw = process.env['PICE_STUB_ADVERSARIAL_SCORES'];
    this.adversarialScores = advRaw ? parseStubScores(advRaw) : [];
    this.requestLogPath = process.env['PICE_STUB_REQUEST_LOG'] || undefined;
    this.evaluateError = process.env['PICE_STUB_EVALUATE_ERROR'] || undefined;
    // PICE_STUB_INIT_ERROR is read inline in registerHandlers — it must
    // short-circuit BEFORE `this.initialized` is set by the base's default
    // handler. The StubProvider constructor body runs AFTER super(), so a
    // field set here wouldn't be visible when registerHandlers runs.
    const fromRaw = process.env['PICE_STUB_EVALUATE_ERROR_FROM_PASS'];
    this.evaluateErrorFromPass = fromRaw ? Number.parseInt(fromRaw, 10) : undefined;
    this.staleResultSessionId =
      process.env['PICE_STUB_INJECT_STALE_EVAL_RESULT'] || undefined;
  }

  getCapabilities(): ProviderCapabilities {
    // Phase 4.1: stub emits deterministic per-pass `costUsd` on
    // `evaluate/create` responses (see PICE_STUB_COSTS env var), so it
    // satisfies the adaptive-budget capability gate by default.
    //
    // Pass-6 Codex Critical #1 regression harness: setting
    // `PICE_STUB_COST_TELEMETRY_OFF=1` flips the capability to `false`,
    // simulating a legacy provider (e.g. production provider-claude-code
    // / provider-codex as of this commit) that does not emit `costUsd`.
    // The daemon's adaptive-budget gate in `stack_loops.rs` must fail
    // closed in that case rather than silently running on synthetic
    // `budget_usd / max_passes` seed spend.
    const telemetryOff = process.env['PICE_STUB_COST_TELEMETRY_OFF'] === '1';
    return {
      workflow: true,
      evaluation: true,
      agentTeams: false,
      models: ['stub-echo'],
      costTelemetry: !telemetryOff,
    };
  }

  protected registerHandlers(transport: StdioTransport): void {
    // Pass-4 Codex Critical regression harness: re-register `initialize` to
    // throw BEFORE the base marks the provider as initialized. The base's
    // registrar ran first in the BaseProvider constructor; transport's
    // registerMethod overwrites via a Map.set(). Production providers never
    // set PICE_STUB_INIT_ERROR, so this path stays inert.
    //
    // `registerHandlers` runs from `super(version)` BEFORE StubProvider's own
    // constructor body assigns `this.initError`, so the field is always
    // undefined here. Read the env var directly to avoid the JS class-init
    // ordering trap.
    const initErrorEnv = process.env['PICE_STUB_INIT_ERROR'];
    if (initErrorEnv) {
      const msg = initErrorEnv;
      transport.registerMethod('initialize', async (_params: unknown) => {
        throw Object.assign(new Error(msg), { code: -32000 });
      });
    }

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

      const p = params as EvaluateCreateParams;

      // Pass-3 regression harness: simulate a runtime RPC failure *after*
      // initialize/session succeeded. The daemon's `run_adaptive_passes`
      // propagates this to `try_run_layer_adaptive` which must route it to
      // `LayerAdaptiveResult::RuntimeError` → `LayerStatus::Failed` (exit 2),
      // NOT `NotStarted` (exit 0). Kept opt-in; production providers never
      // read this env var.
      //
      // Pass-4 extension: when `PICE_STUB_EVALUATE_ERROR_FROM_PASS` is set,
      // the error only fires once `passIndex+1 >= from` (the wire form is
      // 0-indexed; manifest indexing is 1-indexed). This lets tests cover
      // the mid-loop partial-passes case: passes 1..N-1 complete, pass N
      // throws — exercising the adaptive loop's new "preserve prior passes"
      // halt path.
      if (this.evaluateError) {
        const wireIndex = p.passIndex ?? 0;
        const manifestIndex = wireIndex + 1;
        const threshold = this.evaluateErrorFromPass;
        const shouldThrow = threshold === undefined || manifestIndex >= threshold;
        if (shouldThrow) {
          throw Object.assign(new Error(this.evaluateError), { code: -32000 });
        }
      }

      const sessionId = `stub-eval-${nextSessionId++}`;

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

      // Phase 4 Pass-5 Codex Critical #2 regression harness: emit a stale
      // notification FIRST with a deliberately wrong sessionId. A correct
      // session-correlation filter must ignore this and wait for the real
      // notification below. Only active when `PICE_STUB_INJECT_STALE_EVAL_RESULT`
      // is set.
      if (this.staleResultSessionId) {
        transport.sendNotification('evaluate/result', {
          sessionId: this.staleResultSessionId,
          scores: [{
            name: 'stale-stub-criterion',
            score: 0,
            threshold: 10,
            passed: false,
            findings: 'STALE — should be filtered by session-correlation',
          }],
          passed: false,
          summary: 'STALE — should be filtered by session-correlation',
        });
      }

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

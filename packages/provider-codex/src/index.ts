import type {
  ProviderCapabilities,
  EvaluateCreateParams,
  EvaluateScoreParams,
  CriterionScore,
} from '@pice/provider-protocol';
import { SESSION_NOT_FOUND, EVALUATE_RESULT } from '@pice/provider-protocol';
import { BaseProvider, StdioTransport } from '@pice/provider-base';
import { runAdversarialEvaluation, type AdversarialResultType } from './evaluator.js';

interface CodexConfig {
  defaultModel?: string;
  defaultEffort?: string;
}

interface EvalSession {
  id: string;
  contract: unknown;
  diff: string;
  claudeMd: string;
  model: string;
  effort: string;
}

let nextSessionId = 1;

export class CodexProvider extends BaseProvider<CodexConfig> {
  private evalSessions = new Map<string, EvalSession>();

  constructor() {
    super('0.1.0');
  }

  getCapabilities(): ProviderCapabilities {
    return {
      workflow: false,
      evaluation: true,
      agentTeams: false,
      models: ['gpt-5.5', 'gpt-4.1'],
      defaultEvalModel: 'gpt-5.5',
    };
  }

  protected registerHandlers(transport: StdioTransport): void {
    // evaluate/create — create an adversarial evaluation session
    transport.registerMethod('evaluate/create', async (params: unknown) => {
      this.requireInitialized();
      const { contract, diff, claudeMd, model, effort } = params as EvaluateCreateParams;
      const id = `codex-eval-${nextSessionId++}`;
      this.evalSessions.set(id, {
        id,
        contract,
        diff,
        claudeMd,
        model: model ?? this.config?.defaultModel ?? 'gpt-5.5',
        effort: effort ?? this.config?.defaultEffort ?? 'high',
      });
      return { sessionId: id };
    });

    // evaluate/score — run adversarial evaluation
    transport.registerMethod('evaluate/score', async (params: unknown) => {
      this.requireInitialized();
      const { sessionId } = params as EvaluateScoreParams;
      const session = this.evalSessions.get(sessionId);
      if (!session) {
        throw Object.assign(new Error(`evaluation session not found: ${sessionId}`), {
          code: SESSION_NOT_FOUND,
        });
      }

      const prompt = buildAdversarialPrompt(session.contract, session.diff, session.claudeMd);
      const result = await runAdversarialEvaluation(prompt, session.model, session.effort);

      // Map adversarial findings to CriterionScore format for unified reporting
      const scores: CriterionScore[] = mapToScores(result);

      // Determine overall pass — fails if any critical finding exists
      const hasCritical = result.designChallenges.some((c) => c.severity === 'critical');
      const passed = !hasCritical && !result.recommendsChanges;

      transport.sendNotification(EVALUATE_RESULT, {
        sessionId,
        scores,
        passed,
        summary: result.overallAssessment,
      });

      return { ok: true };
    });
  }
}

function mapToScores(result: AdversarialResultType): CriterionScore[] {
  return result.designChallenges.map((challenge) => {
    const score = challenge.severity === 'critical' ? 3 : challenge.severity === 'consider' ? 6 : 8;
    return {
      name: `Design: ${challenge.finding.slice(0, 50)}`,
      score,
      threshold: 5,
      passed: challenge.severity !== 'critical',
      findings: challenge.finding,
    };
  });
}

function buildAdversarialPrompt(contract: unknown, diff: string, claudeMd: string): string {
  return `You are a DESIGN CHALLENGER reviewing code changes.

## Contract
\`\`\`json
${JSON.stringify(contract, null, 2)}
\`\`\`

## Code Changes
\`\`\`diff
${diff}
\`\`\`

## Project Conventions
${claudeMd}

## Task
Challenge the APPROACH, not just correctness:
- Was this the right design? What assumptions does it depend on?
- Where could it fail under real-world conditions?
- What alternative approaches were overlooked?
Categorize findings as: critical, consider, or acknowledged.`;
}

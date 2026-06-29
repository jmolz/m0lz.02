import type {
  ProviderCapabilities,
  SessionCreateParams,
  SessionSendParams,
  SessionDestroyParams,
  EvaluateCreateParams,
  EvaluateScoreParams,
  CriterionScore,
} from '@pice/provider-protocol';
import {
  SESSION_NOT_FOUND,
  RESPONSE_CHUNK,
  RESPONSE_COMPLETE,
  RESPONSE_TOOL_USE,
  EVALUATE_RESULT,
} from '@pice/provider-protocol';
import { BaseProvider, StdioTransport } from '@pice/provider-base';
import { type ActiveSession, createQuery, createEvalQuery } from './session.js';

interface ClaudeCodeConfig {
  defaultModel?: string;
}

let nextSessionId = 1;

export class ClaudeCodeProvider extends BaseProvider<ClaudeCodeConfig> {
  private sessions = new Map<string, ActiveSession>();

  constructor() {
    super('0.1.0');
  }

  getCapabilities(): ProviderCapabilities {
    return {
      workflow: true,
      evaluation: true,
      agentTeams: true,
      models: ['claude-fable-5', 'claude-sonnet-4-6', 'claude-haiku-4-5'],
      defaultEvalModel: 'claude-fable-5',
    };
  }

  protected registerHandlers(transport: StdioTransport): void {
    // session/create — prepare a new session
    transport.registerMethod('session/create', async (params: unknown) => {
      this.requireInitialized();
      const { workingDirectory, model, systemPrompt } = params as SessionCreateParams;
      const id = `claude-session-${nextSessionId++}`;
      this.sessions.set(id, {
        id,
        config: { cwd: workingDirectory, model, systemPrompt },
      });
      return { sessionId: id };
    });

    // session/send — run a query and stream responses
    transport.registerMethod('session/send', async (params: unknown) => {
      this.requireInitialized();
      const { sessionId, message } = params as SessionSendParams;
      const session = this.sessions.get(sessionId);
      if (!session) {
        throw Object.assign(new Error(`session not found: ${sessionId}`), {
          code: SESSION_NOT_FOUND,
        });
      }

      const q = createQuery(message, session.config, true);
      session.activeQuery = q;

      let finalResult: unknown = null;

      for await (const msg of q) {
        if (msg.type === 'stream_event') {
          // Extract text deltas from streaming events
          const event = (msg as { event?: { type?: string; delta?: { type?: string; text?: string } } }).event;
          if (event?.type === 'content_block_delta' && event.delta?.type === 'text_delta' && event.delta.text) {
            transport.sendNotification(RESPONSE_CHUNK, {
              sessionId,
              text: event.delta.text,
            });
          }
        } else if (msg.type === 'assistant') {
          // Complete assistant message — check for tool use
          const content = (msg as { message?: { content?: Array<{ type: string; name?: string; input?: unknown }> } }).message?.content;
          if (content) {
            for (const block of content) {
              if (block.type === 'tool_use') {
                transport.sendNotification(RESPONSE_TOOL_USE, {
                  sessionId,
                  toolName: block.name ?? 'unknown',
                  toolInput: block.input ?? {},
                });
              }
            }
          }
        } else if (msg.type === 'result') {
          finalResult = (msg as { result?: unknown }).result ?? { completed: true };
        }
      }

      // Send completion notification
      transport.sendNotification(RESPONSE_COMPLETE, {
        sessionId,
        result: finalResult ?? { completed: true },
      });

      return { ok: true };
    });

    // session/destroy — clean up session
    transport.registerMethod('session/destroy', async (params: unknown) => {
      this.requireInitialized();
      const { sessionId } = params as SessionDestroyParams;
      this.sessions.delete(sessionId);
      return null;
    });

    // evaluate/create — create an isolated evaluation session
    transport.registerMethod('evaluate/create', async (params: unknown) => {
      this.requireInitialized();
      const { contract, diff, claudeMd, model } = params as EvaluateCreateParams;
      const id = `claude-eval-${nextSessionId++}`;
      const session: ActiveSession & { evalContext?: unknown } = {
        id,
        config: {
          cwd: process.cwd(),
          model: model ?? this.config?.defaultModel,
        },
      };
      session.evalContext = { contract, diff, claudeMd };
      this.sessions.set(id, session);
      return { sessionId: id };
    });

    // evaluate/score — run evaluation and return scores
    transport.registerMethod('evaluate/score', async (params: unknown) => {
      this.requireInitialized();
      const { sessionId } = params as EvaluateScoreParams;
      const session = this.sessions.get(sessionId) as
        | (ActiveSession & { evalContext?: { contract: unknown; diff: string; claudeMd: string } })
        | undefined;
      if (!session || !session.evalContext) {
        throw Object.assign(new Error(`evaluation session not found: ${sessionId}`), {
          code: SESSION_NOT_FOUND,
        });
      }

      const { contract, diff, claudeMd } = session.evalContext;
      const prompt = buildEvalPrompt(contract, diff, claudeMd);

      // Use structured output for evaluation scoring
      const q = createEvalQuery(prompt, session.config, EVALUATION_SCHEMA);

      let scores: CriterionScore[] = [];
      let passed = false;
      let summary = '';

      for await (const msg of q) {
        if (msg.type === 'result') {
          const structured = (msg as { structured_output?: unknown }).structured_output;
          if (structured && typeof structured === 'object') {
            const result = structured as {
              criteria?: CriterionScore[];
              overallPass?: boolean;
              summary?: string;
            };
            scores = result.criteria ?? [];
            passed = result.overallPass ?? false;
            summary = result.summary ?? '';
          }
        }
      }

      // Send evaluation result notification
      transport.sendNotification(EVALUATE_RESULT, {
        sessionId,
        scores,
        passed,
        summary: summary || undefined,
      });

      return { ok: true };
    });
  }
}

// JSON Schema for structured evaluation output
const EVALUATION_SCHEMA: Record<string, unknown> = {
  type: 'object',
  properties: {
    criteria: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          name: { type: 'string' },
          score: { type: 'number' },
          threshold: { type: 'number' },
          passed: { type: 'boolean' },
          findings: { type: 'string' },
        },
        required: ['name', 'score', 'threshold', 'passed', 'findings'],
      },
    },
    overallPass: { type: 'boolean' },
    summary: { type: 'string' },
  },
  required: ['criteria', 'overallPass', 'summary'],
};

function buildEvalPrompt(contract: unknown, diff: string, claudeMd: string): string {
  return `You are an ADVERSARIAL EVALUATOR grading code changes against a contract.

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

## Instructions
For EACH criterion in the contract:
1. Read the relevant code
2. Try to break it — edge cases, malformed inputs, concurrent access
3. Score it 1-10 with specific evidence
4. Set passed = true if score >= threshold

Output JSON matching the required schema.`;
}

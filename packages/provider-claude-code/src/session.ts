import { query, type Query } from '@anthropic-ai/claude-agent-sdk';

export interface SessionConfig {
  cwd: string;
  model?: string;
  systemPrompt?: string;
}

export interface ActiveSession {
  id: string;
  config: SessionConfig;
  activeQuery?: Query;
}

export function createQuery(
  prompt: string,
  config: SessionConfig,
  streaming: boolean,
): Query {
  return query({
    prompt,
    options: {
      model: config.model ?? 'claude-sonnet-4-6',
      cwd: config.cwd,
      allowedTools: ['Read', 'Write', 'Edit', 'Bash', 'Glob', 'Grep'],
      permissionMode: 'bypassPermissions',
      allowDangerouslySkipPermissions: true,
      includePartialMessages: streaming,
      maxTurns: 100,
      systemPrompt: config.systemPrompt,
    },
  });
}

export function createEvalQuery(
  prompt: string,
  config: SessionConfig,
  outputSchema?: Record<string, unknown>,
): Query {
  return query({
    prompt,
    options: {
      model: config.model ?? 'claude-opus-4-8',
      cwd: config.cwd,
      allowedTools: ['Read', 'Glob', 'Grep'],
      permissionMode: 'bypassPermissions',
      allowDangerouslySkipPermissions: true,
      persistSession: false,
      ...(outputSchema
        ? {
            outputFormat: { type: 'json_schema' as const, schema: outputSchema },
          }
        : {}),
    },
  });
}

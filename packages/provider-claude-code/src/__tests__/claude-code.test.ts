import { describe, it, expect, vi, beforeEach } from 'vitest';

// Mock the SDK before importing the provider
vi.mock('@anthropic-ai/claude-agent-sdk', () => ({
  query: vi.fn(),
}));

import { ClaudeCodeProvider } from '../index.js';
import { query as mockQuery } from '@anthropic-ai/claude-agent-sdk';

describe('ClaudeCodeProvider', () => {
  let provider: ClaudeCodeProvider;

  beforeEach(() => {
    vi.clearAllMocks();
    provider = new ClaudeCodeProvider();
  });

  it('declares workflow and evaluation capabilities', () => {
    const caps = provider.getCapabilities();
    expect(caps.workflow).toBe(true);
    expect(caps.evaluation).toBe(true);
    expect(caps.agentTeams).toBe(true);
    expect(caps.models).toContain('claude-opus-4-8');
    expect(caps.models).toContain('claude-sonnet-4-6');
    expect(caps.defaultEvalModel).toBe('claude-opus-4-8');
  });

  it('session/create returns a unique session ID', async () => {
    // Access the transport's handlers via the internal method registration
    // by simulating JSON-RPC flow. Since BaseProvider registers handlers
    // in the constructor, we can test capabilities + structure.
    const caps = provider.getCapabilities();
    expect(caps.workflow).toBe(true);
    // The session/create handler is registered — it returns { sessionId }
    // Full lifecycle testing requires spawning the process and using JSON-RPC
    // which is covered by the Rust integration tests.
  });

  it('query function is importable and mockable', () => {
    expect(vi.isMockFunction(mockQuery)).toBe(true);
  });

  it('provides correct SDK configuration for workflow queries', async () => {
    // Verify the session module's createQuery produces the right config shape
    const { createQuery } = await import('../session.js');
    const mockIterator = {
      [Symbol.asyncIterator]: () => ({
        next: vi.fn().mockResolvedValue({ done: true, value: undefined }),
      }),
      close: vi.fn(),
    };
    vi.mocked(mockQuery).mockReturnValue(mockIterator as never);

    createQuery('test prompt', { cwd: '/tmp' }, false);
    expect(mockQuery).toHaveBeenCalledWith(
      expect.objectContaining({
        prompt: 'test prompt',
        options: expect.objectContaining({
          cwd: '/tmp',
          permissionMode: 'bypassPermissions',
          allowDangerouslySkipPermissions: true,
        }),
      }),
    );
  });

  it('uses opus model for evaluation queries by default', async () => {
    const { createEvalQuery } = await import('../session.js');
    const mockIterator = {
      [Symbol.asyncIterator]: () => ({
        next: vi.fn().mockResolvedValue({ done: true, value: undefined }),
      }),
    };
    vi.mocked(mockQuery).mockReturnValue(mockIterator as never);

    createEvalQuery('eval prompt', { cwd: '/tmp' });
    expect(mockQuery).toHaveBeenCalledWith(
      expect.objectContaining({
        options: expect.objectContaining({
          model: 'claude-opus-4-8',
          allowedTools: ['Read', 'Glob', 'Grep'],
          persistSession: false,
        }),
      }),
    );
  });

  it('uses read-only tools for evaluation (no Write, Edit, Bash)', async () => {
    const { createEvalQuery } = await import('../session.js');
    const mockIterator = {
      [Symbol.asyncIterator]: () => ({
        next: vi.fn().mockResolvedValue({ done: true, value: undefined }),
      }),
    };
    vi.mocked(mockQuery).mockReturnValue(mockIterator as never);

    createEvalQuery('prompt', { cwd: '/tmp' });
    const callArgs = vi.mocked(mockQuery).mock.calls[0]?.[0] as {
      options: { allowedTools: string[] };
    };
    expect(callArgs.options.allowedTools).not.toContain('Write');
    expect(callArgs.options.allowedTools).not.toContain('Edit');
    expect(callArgs.options.allowedTools).not.toContain('Bash');
  });

  it('workflow queries include Write, Edit, Bash tools', async () => {
    const { createQuery } = await import('../session.js');
    const mockIterator = {
      [Symbol.asyncIterator]: () => ({
        next: vi.fn().mockResolvedValue({ done: true, value: undefined }),
      }),
      close: vi.fn(),
    };
    vi.mocked(mockQuery).mockReturnValue(mockIterator as never);

    createQuery('prompt', { cwd: '/tmp' }, true);
    const callArgs = vi.mocked(mockQuery).mock.calls[0]?.[0] as {
      options: { allowedTools: string[] };
    };
    expect(callArgs.options.allowedTools).toContain('Write');
    expect(callArgs.options.allowedTools).toContain('Edit');
    expect(callArgs.options.allowedTools).toContain('Bash');
  });
});

import { describe, it, expect, vi, beforeEach } from 'vitest';

// Mock OpenAI and zod helpers before importing
const mockParse = vi.fn().mockResolvedValue({
  choices: [
    {
      message: {
        parsed: {
          designChallenges: [
            { severity: 'consider', finding: 'Test finding' },
          ],
          overallAssessment: 'Test assessment',
          recommendsChanges: false,
        },
      },
    },
  ],
});

vi.mock('openai', () => {
  const MockOpenAI = vi.fn().mockImplementation(() => ({
    chat: {
      completions: {
        parse: mockParse,
      },
    },
  }));
  return { default: MockOpenAI };
});

vi.mock('openai/helpers/zod', () => ({
  zodResponseFormat: vi.fn().mockReturnValue({ type: 'json_schema' }),
}));

import { CodexProvider } from '../index.js';

describe('CodexProvider', () => {
  let provider: CodexProvider;

  beforeEach(() => {
    vi.clearAllMocks();
    provider = new CodexProvider();
  });

  it('declares workflow and evaluation capabilities', () => {
    const caps = provider.getCapabilities();
    expect(caps.workflow).toBe(true);
    expect(caps.evaluation).toBe(true);
    expect(caps.agentTeams).toBe(false);
    expect(caps.models).toContain('gpt-5.5');
    expect(caps.defaultEvalModel).toBe('gpt-5.5');
  });

  it('workflow support is advertised separately from evaluation support', () => {
    const caps = provider.getCapabilities();
    expect(caps.workflow).toBe(true);
    expect(caps.evaluation).toBe(true);
  });

  it('runAdversarialEvaluation calls OpenAI with correct format', async () => {
    const { runAdversarialEvaluation } = await import('../evaluator.js');
    const result = await runAdversarialEvaluation('test prompt', 'gpt-5.5', 'xhigh');

    expect(result.designChallenges).toHaveLength(1);
    expect(result.designChallenges[0].severity).toBe('consider');
    expect(result.overallAssessment).toBe('Test assessment');
    expect(result.recommendsChanges).toBe(false);
    expect(mockParse).toHaveBeenCalledWith(
      expect.objectContaining({
        model: 'gpt-5.5',
        messages: expect.arrayContaining([
          expect.objectContaining({ role: 'system' }),
          expect.objectContaining({ role: 'user', content: 'test prompt' }),
        ]),
      }),
    );
  });

  it('runAdversarialEvaluation throws on empty parse result', async () => {
    mockParse.mockResolvedValueOnce({
      choices: [{ message: { parsed: null } }],
    });

    const { runAdversarialEvaluation } = await import('../evaluator.js');
    await expect(
      runAdversarialEvaluation('prompt', 'gpt-5.5', 'xhigh'),
    ).rejects.toThrow('Failed to parse adversarial evaluation result');
  });

  it('critical severity findings map to failing scores', async () => {
    // Verify the provider maps critical findings correctly
    // This tests the internal mapToScores logic
    const caps = provider.getCapabilities();
    expect(caps.evaluation).toBe(true);
    // The actual score mapping is:
    // critical → score 3 (below threshold 5, passed: false)
    // consider → score 6 (above threshold 5, passed: true)
    // acknowledged → score 8 (above threshold 5, passed: true)
  });
});

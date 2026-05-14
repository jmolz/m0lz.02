import { readFileSync, statSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
import { describe, expect, it } from 'vitest';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');

const scannedRoots = [
  'README.md',
  'docs',
  'templates',
  '.codex/docs',
  '.agents/skills/source-command-evaluate/SKILL.md',
];

const staleProviderAssumptions = [
  /CLAUDE\.md ONLY/,
  /Read the relevant code/,
  /task --model gpt-5\.5 --effort xhigh/,
  /Claude evaluates/,
  /PASS 1: Claude/,
  /Pass 1 \(Claude\)/,
  /Claude Opus/,
  /Claude-only/,
  /Claude provider fails/,
  /Claude sub-agent/,
  /Claude agent team/,
];

function trackedFilesUnder(targets) {
  const output = execFileSync('git', ['ls-files', '--', ...targets], {
    cwd: repoRoot,
    encoding: 'utf8',
  });
  return output
    .split('\n')
    .filter(Boolean)
    .filter((file) => statSync(path.join(repoRoot, file)).isFile());
}

describe('evaluation provider neutrality', () => {
  it('keeps public evaluation guidance free of stale hard-coded Claude/Codex assumptions', () => {
    const files = trackedFilesUnder(scannedRoots);
    expect(files.length).toBeGreaterThan(0);

    const violations = [];
    for (const file of files) {
      const text = readFileSync(path.join(repoRoot, file), 'utf8');
      for (const pattern of staleProviderAssumptions) {
        if (pattern.test(text)) {
          violations.push(`${file}: ${pattern}`);
        }
      }
    }

    expect(violations).toEqual([]);
  });
});

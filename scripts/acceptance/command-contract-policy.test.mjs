import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');

function read(relativePath) {
  return readFileSync(path.join(repoRoot, relativePath), 'utf8').replace(/\r\n/g, '\n');
}

const fullDeployCommandSurfaces = [
  '.agents/skills/source-command-commit-and-deploy/SKILL.md',
  '.claude/commands/commit-and-deploy.md',
  'templates/codex/commands/commit-and-deploy.md',
  'templates/claude/commands/commit-and-deploy.md',
];

describe('deployment command contract policy', () => {
  it('requires README review and current evidence during every deployment', () => {
    for (const surface of fullDeployCommandSurfaces) {
      const command = read(surface);

      expect(command, surface).toContain('README release-readiness review');
      expect(command, surface).toContain('review `README.md` against the actual');
      expect(command, surface).toContain('Docker/Linux CI parity guidance');
      expect(command, surface).toContain('hosted Windows runner guidance');
      expect(command, surface).toContain('node scripts/acceptance/readme-media-audit.mjs');
      expect(command, surface).toContain('Never update README evidence from');
      expect(command, surface).toContain('memory; use current command output');
    }
  });

  it('keeps the Codex slash wrapper tied to the README review gate', () => {
    const wrapper = read('.codex/commands/commit-and-deploy.md');

    expect(wrapper).toContain('source-command-commit-and-deploy');
    expect(wrapper).toContain('mandatory README review/update gate');
    expect(wrapper).toContain('verify `README.md` freshness');
  });

  it('requires full releases for every deployment tag in all command mirrors', () => {
    for (const surface of fullDeployCommandSurfaces) {
      const command = read(surface);

      expect(command, surface).toContain('Every push to main gets a release');
      expect(command, surface).toContain('A `v*` tag is always a full release');
      expect(command, surface).toContain('publish the matching npm packages');
      expect(command, surface).toContain('pnpm exec vitest run scripts/acceptance/release-workflow-policy.test.mjs');
      expect(command, surface).not.toContain('Docs/chore changes -> lightweight release');
      expect(command, surface).not.toContain('Docs/chore changes → lightweight release');
      expect(command, surface).not.toContain('Documentation-only release (no binary changes).');
    }
  });
});

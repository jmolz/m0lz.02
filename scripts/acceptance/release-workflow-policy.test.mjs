import { readFileSync, readdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const workflow = readFileSync(path.join(repoRoot, '.github/workflows/release.yml'), 'utf8').replace(
  /\r\n/g,
  '\n'
);

function jobSection(name) {
  const pattern = new RegExp(`\\n  ${name}:\\n([\\s\\S]*?)(?=\\n  [a-zA-Z0-9_-]+:\\n|\\s*$)`);
  const match = workflow.match(pattern);
  if (!match) {
    throw new Error(`missing release workflow job: ${name}`);
  }
  return match[1];
}

function packageJson(relativePath) {
  return JSON.parse(readFileSync(path.join(repoRoot, relativePath), 'utf8'));
}

const releaseJob = jobSection('release');
const npmPublishJob = jobSection('npm-publish');

describe('release workflow policy', () => {
  it('requires npm publish before creating a tag-triggered GitHub Release', () => {
    expect(releaseJob).toContain('needs: [package-binaries, completions, smoke-test, npm-publish]');
    expect(releaseJob).toContain(
      "if: github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')"
    );
  });

  it('runs npm publish for v-tag releases and explicit manual publish dispatches only', () => {
    expect(npmPublishJob).toMatch(
      /github\.event_name == 'push' && startsWith\(github\.ref, 'refs\/tags\/v'\)/
    );
    expect(npmPublishJob).toMatch(
      /github\.event_name == 'workflow_dispatch' && inputs\.dry_run == false && inputs\.publish_npm == true/
    );
    expect(npmPublishJob).not.toMatch(/inputs\.dry_run == true[\s\S]*npm publish/);
  });

  it('fails closed when a tag release does not match the npm package version', () => {
    expect(npmPublishJob).toContain('Assert release tag matches package version');
    expect(npmPublishJob).toContain(
      'expected_tag="v$(node -p "require(\'./npm/pice/package.json\').version")"'
    );
    expect(npmPublishJob).toContain('Release tag ${GITHUB_REF_NAME} must match npm package version');
  });

  it('requires artifact smoke and an npm token before publishing packages', () => {
    expect(npmPublishJob).toContain('needs: [package-binaries, smoke-test]');
    expect(npmPublishJob).toContain('Assert NPM token is configured');
    expect(npmPublishJob).toContain('NPM_TOKEN is required for release npm publishing');
    expect(npmPublishJob).toContain('Local npm pack/install smoke');
    expect(npmPublishJob).toContain('PICE_NPM_PACK_SMOKE: "1"');
    expect(npmPublishJob).toContain('node scripts/acceptance/release-artifact-smoke.mjs');
  });

  it('publishes every platform package before the main npm wrapper package', () => {
    const expectedPlatformPackages = [
      'pice-darwin-arm64',
      'pice-darwin-x64',
      'pice-linux-arm64',
      'pice-linux-x64',
      'pice-win32-x64',
    ];

    for (const pkg of expectedPlatformPackages) {
      expect(npmPublishJob).toContain(pkg);
    }

    expect(npmPublishJob.indexOf('Publish platform packages')).toBeLessThan(
      npmPublishJob.indexOf('Publish main package')
    );
  });

  it('keeps all publishable package versions aligned with the Rust workspace version', () => {
    const cargo = readFileSync(path.join(repoRoot, 'Cargo.toml'), 'utf8');
    const workspaceVersion = cargo.match(/^version = "([^"]+)"$/m)?.[1];
    expect(workspaceVersion).toBeTruthy();

    const publishablePackages = [
      ...readdirSync(path.join(repoRoot, 'npm')).map((dir) => `npm/${dir}/package.json`),
      ...readdirSync(path.join(repoRoot, 'packages')).map((dir) => `packages/${dir}/package.json`),
    ];

    for (const pkgPath of publishablePackages) {
      expect(packageJson(pkgPath).version, pkgPath).toBe(workspaceVersion);
    }

    const wrapperPackage = packageJson('npm/pice/package.json');
    for (const [pkgName, version] of Object.entries(wrapperPackage.optionalDependencies ?? {})) {
      expect(version, `npm/pice optionalDependency ${pkgName}`).toBe(workspaceVersion);
    }
  });
});

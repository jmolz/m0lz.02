#!/usr/bin/env node
import { spawnSync } from 'node:child_process';

const MIN_VERSION = '0.130.0';
const REQUIRED_FLAGS = ['--json', '--cd', '--sandbox', '--output-last-message'];
const DEFAULT_PROBE_TIMEOUT_MS = 10_000;

function parseProbeTimeoutMs(raw) {
  const parsed = Number.parseInt(raw ?? '', 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : DEFAULT_PROBE_TIMEOUT_MS;
}

const PROBE_TIMEOUT_MS = parseProbeTimeoutMs(process.env.PICE_CODEX_CLI_PROBE_TIMEOUT_MS);

function spawnSpec(executable, args) {
  if (/\.(?:cjs|mjs|js)$/i.test(executable)) {
    return { command: process.execPath, args: [executable, ...args] };
  }
  return { command: executable, args };
}

function run(executable, args) {
  const spec = spawnSpec(executable, args);
  return spawnSync(spec.command, spec.args, {
    encoding: 'utf8',
    env: process.env,
    timeout: PROBE_TIMEOUT_MS,
  });
}

function parseVersion(raw) {
  const match = raw.match(/codex-cli\s+(\d+\.\d+\.\d+)/);
  if (!match) {
    throw new Error(`unable to parse Codex CLI version from: ${raw.trim()}`);
  }
  return match[1];
}

function compareSemver(a, b) {
  const pa = a.split('.').map(Number);
  const pb = b.split('.').map(Number);
  for (let i = 0; i < 3; i++) {
    const delta = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (delta !== 0) return delta;
  }
  return 0;
}

function fail(message, details = {}) {
  console.error(JSON.stringify({ status: 'failed', message, ...details }, null, 2));
  process.exit(1);
}

const executable = process.env.PICE_CODEX_CLI ?? 'codex';
const allowMissing = process.env.PICE_CODEX_SMOKE_ALLOW_MISSING === '1';

const versionRun = run(executable, ['--version']);
if (versionRun.error) {
  if (allowMissing && versionRun.error.code === 'ENOENT') {
    console.log(JSON.stringify({ status: 'skipped', reason: 'codex CLI not installed' }, null, 2));
    process.exit(0);
  }
  if (versionRun.error.code === 'ETIMEDOUT') {
    fail('codex CLI version probe timed out', { timeout_ms: PROBE_TIMEOUT_MS });
  }
  fail('codex CLI is not executable', { error: String(versionRun.error) });
}
if (versionRun.status !== 0) {
  fail('codex --version failed', { stdout: versionRun.stdout, stderr: versionRun.stderr });
}

let version;
try {
  version = parseVersion(`${versionRun.stdout}\n${versionRun.stderr}`);
} catch (error) {
  fail(error instanceof Error ? error.message : String(error));
}
if (compareSemver(version, MIN_VERSION) < 0) {
  fail(`unsupported codex CLI version ${version}; require >= ${MIN_VERSION}`);
}

const helpRun = run(executable, ['exec', '--help']);
if (helpRun.error) {
  if (helpRun.error.code === 'ETIMEDOUT') {
    fail('codex exec help probe timed out', { timeout_ms: PROBE_TIMEOUT_MS });
  }
  fail('codex exec --help failed to run', { error: String(helpRun.error) });
}
if (helpRun.status !== 0) {
  fail('codex exec --help failed', { stdout: helpRun.stdout, stderr: helpRun.stderr });
}
const help = `${helpRun.stdout}\n${helpRun.stderr}`;
const missing = REQUIRED_FLAGS.filter((flag) => !help.includes(flag));
if (missing.length > 0 || !help.includes('stdin')) {
  fail('codex exec help is missing required non-secret workflow support', {
    missing: [...missing, ...(help.includes('stdin') ? [] : ['stdin prompt support'])],
  });
}

console.log(
  JSON.stringify(
    {
      status: 'passed',
      executable,
      version,
      probe_timeout_ms: PROBE_TIMEOUT_MS,
      required_flags: REQUIRED_FLAGS,
      ask_for_approval_flag: help.includes('--ask-for-approval') ? 'available' : 'not-advertised',
    },
    null,
    2,
  ),
);

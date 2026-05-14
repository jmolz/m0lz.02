import { createInterface } from 'node:readline';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { spawn, type ChildProcess } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const MIN_CODEX_CLI_VERSION = '0.130.0';
const REQUIRED_EXEC_FLAGS = [
  '--json',
  '--cd',
  '--sandbox',
  '--output-last-message',
] as const;

export interface CodexCliContract {
  executable: string;
  version: string;
  supportsAskForApproval: boolean;
}

export interface CodexWorkflowConfig {
  sessionId: string;
  message: string;
  workingDirectory: string;
  model?: string;
  systemPrompt?: string;
  executable?: string;
}

export interface CodexWorkflowEvents {
  onChunk(text: string): void;
  onToolUse?(event: { toolName: string; toolInput: unknown; toolResult?: unknown }): void;
}

export interface CodexWorkflowResult {
  finalText: string;
}

interface SpawnOutput {
  stdout: string;
  stderr: string;
  code: number | null;
}

const activeChildrenBySession = new Map<string, Set<ChildProcess>>();
const activeChildren = new Set<ChildProcess>();
let signalHandlersInstalled = false;

function resolveCodexExecutable(explicit?: string): string {
  return explicit ?? process.env['PICE_CODEX_CLI'] ?? 'codex';
}

function spawnSpec(executable: string, args: string[]): { command: string; args: string[] } {
  if (/\.(?:cjs|mjs|js)$/i.test(executable)) {
    return { command: process.execPath, args: [executable, ...args] };
  }
  if (executable.startsWith('file://')) {
    return { command: process.execPath, args: [fileURLToPath(executable), ...args] };
  }
  return { command: executable, args };
}

async function collectCommand(executable: string, args: string[]): Promise<SpawnOutput> {
  const spec = spawnSpec(executable, args);
  const probeTimeoutMs = codexContractProbeTimeoutMs();
  return new Promise((resolve, reject) => {
    let settled = false;
    let timeout: NodeJS.Timeout | undefined;
    const finish = (fn: () => void) => {
      if (settled) return;
      settled = true;
      if (timeout) clearTimeout(timeout);
      fn();
    };
    const child = spawn(spec.command, spec.args, {
      stdio: ['ignore', 'pipe', 'pipe'],
      env: process.env,
    });
    timeout = setTimeout(() => {
      child.kill('SIGKILL');
      finish(() => {
        reject(
          Object.assign(
            new Error(
              `Codex CLI contract probe timed out after ${probeTimeoutMs}ms: ${[
                spec.command,
                ...spec.args,
              ].join(' ')}`,
            ),
            { code: -32000 },
          ),
        );
      });
    }, probeTimeoutMs);
    timeout.unref();
    let stdout = '';
    let stderr = '';
    if (!child.stdout || !child.stderr) {
      finish(() => reject(new Error('failed to open Codex CLI stdout/stderr pipes')));
      return;
    }
    child.stdout.on('data', (chunk: Buffer) => {
      stdout += chunk.toString();
    });
    child.stderr.on('data', (chunk: Buffer) => {
      stderr += chunk.toString();
    });
    child.on('error', (err) => finish(() => reject(err)));
    child.on('close', (code) => finish(() => resolve({ stdout, stderr, code })));
  });
}

function codexContractProbeTimeoutMs(): number {
  const raw = process.env['PICE_CODEX_CLI_PROBE_TIMEOUT_MS'];
  if (!raw) return 10_000;
  const parsed = Number(raw);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : 10_000;
}

function registerChild(sessionId: string, child: ChildProcess): void {
  activeChildren.add(child);
  const sessionChildren = activeChildrenBySession.get(sessionId) ?? new Set<ChildProcess>();
  sessionChildren.add(child);
  activeChildrenBySession.set(sessionId, sessionChildren);
}

function unregisterChild(sessionId: string, child: ChildProcess | undefined): void {
  if (!child) return;
  activeChildren.delete(child);
  const sessionChildren = activeChildrenBySession.get(sessionId);
  if (!sessionChildren) return;
  sessionChildren.delete(child);
  if (sessionChildren.size === 0) {
    activeChildrenBySession.delete(sessionId);
  }
}

export async function terminateChild(child: ChildProcess | undefined): Promise<void> {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;

  await new Promise<void>((resolve) => {
    let settled = false;
    let sigkillTimer: NodeJS.Timeout | undefined;
    let abandonTimer: NodeJS.Timeout | undefined;
    const finish = () => {
      if (settled) return;
      settled = true;
      if (sigkillTimer) clearTimeout(sigkillTimer);
      if (abandonTimer) clearTimeout(abandonTimer);
      resolve();
    };

    child.once('close', finish);
    sigkillTimer = setTimeout(() => {
      if (child.exitCode === null && child.signalCode === null) {
        child.kill('SIGKILL');
      }
    }, 1_000);
    abandonTimer = setTimeout(finish, 2_000);
    child.kill('SIGTERM');
  });
}

export async function terminateCodexWorkflowSession(sessionId: string): Promise<void> {
  const children = [...(activeChildrenBySession.get(sessionId) ?? [])];
  await Promise.all(children.map((child) => terminateChild(child)));
}

export async function terminateAllCodexWorkflowSessions(): Promise<void> {
  await Promise.all([...activeChildren].map((child) => terminateChild(child)));
}

function signalExitCode(signal: NodeJS.Signals): number {
  if (signal === 'SIGINT') return 130;
  if (signal === 'SIGTERM') return 143;
  if (signal === 'SIGHUP') return 129;
  return 1;
}

function terminateChildrenSync(): void {
  for (const child of activeChildren) {
    if (child.exitCode === null && child.signalCode === null) {
      child.kill('SIGTERM');
    }
  }
}

export function installCodexChildSignalHandlers(): void {
  if (signalHandlersInstalled) return;
  signalHandlersInstalled = true;

  const handleSignal = (signal: NodeJS.Signals) => {
    const abandon = setTimeout(() => process.exit(signalExitCode(signal)), 2_500);
    abandon.unref();
    terminateAllCodexWorkflowSessions().finally(() => process.exit(signalExitCode(signal)));
  };

  process.once('SIGTERM', handleSignal);
  process.once('SIGINT', handleSignal);
  process.once('SIGHUP', handleSignal);
  process.once('exit', terminateChildrenSync);
}

export const CODEX_EXEC_CONTRACT_NOTE =
  'codex-cli 0.130.0 does not advertise --ask-for-approval; PICE passes it only when the installed CLI supports it.';

function parseVersion(raw: string): string {
  const match = raw.match(/codex-cli\s+(\d+\.\d+\.\d+)/);
  if (!match) {
    throw Object.assign(new Error(`unable to parse Codex CLI version from: ${raw.trim()}`), {
      code: -32000,
    });
  }
  return match[1];
}

function compareSemver(a: string, b: string): number {
  const pa = a.split('.').map(Number);
  const pb = b.split('.').map(Number);
  for (let i = 0; i < 3; i++) {
    const delta = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (delta !== 0) return delta;
  }
  return 0;
}

export async function validateCodexCliContract(executable = resolveCodexExecutable()): Promise<CodexCliContract> {
  const versionOutput = await collectCommand(executable, ['--version']);
  if (versionOutput.code !== 0) {
    throw Object.assign(
      new Error(`Codex CLI version check failed: ${versionOutput.stderr || versionOutput.stdout}`),
      { code: -32000 },
    );
  }
  const version = parseVersion(`${versionOutput.stdout}\n${versionOutput.stderr}`);
  if (compareSemver(version, MIN_CODEX_CLI_VERSION) < 0) {
    throw Object.assign(
      new Error(`unsupported Codex CLI version ${version}; require >= ${MIN_CODEX_CLI_VERSION}`),
      { code: -32000 },
    );
  }

  const helpOutput = await collectCommand(executable, ['exec', '--help']);
  if (helpOutput.code !== 0) {
    throw Object.assign(
      new Error(`Codex CLI exec help failed: ${helpOutput.stderr || helpOutput.stdout}`),
      { code: -32000 },
    );
  }
  const help = `${helpOutput.stdout}\n${helpOutput.stderr}`;
  const missing = REQUIRED_EXEC_FLAGS.filter((flag) => !help.includes(flag));
  if (missing.length > 0 || !help.includes('stdin')) {
    throw Object.assign(
      new Error(
        `unsupported Codex CLI exec shape; missing ${[
          ...missing,
          ...(help.includes('stdin') ? [] : ['stdin prompt support']),
        ].join(', ')}`,
      ),
      { code: -32000 },
    );
  }

  return {
    executable,
    version,
    supportsAskForApproval: help.includes('--ask-for-approval'),
  };
}

/**
 * Extract text deltas from known Codex CLI JSONL event shapes.
 *
 * Consumed fields are intentionally narrow and fixture-backed:
 * - `{ "type": "response.output_text.delta", "delta": "..." }`
 * - `{ "type": "agent_message_delta", "delta": "..." }`
 * - `{ "type": "assistant_message_delta", "text": "..." }`
 * - `{ "type": "response.chunk", "text": "..." }`
 *
 * Unknown event objects are ignored so newer Codex CLI releases can add
 * telemetry/progress events without breaking the provider.
 */
export function extractTextDelta(event: unknown): string | undefined {
  if (!event || typeof event !== 'object') return undefined;
  const e = event as Record<string, unknown>;
  const type = typeof e.type === 'string' ? e.type : '';
  if (
    type === 'response.output_text.delta' ||
    type === 'agent_message_delta' ||
    type === 'assistant_message_delta' ||
    type === 'response.chunk'
  ) {
    if (typeof e.delta === 'string') return e.delta;
    if (typeof e.text === 'string') return e.text;
    if (
      e.delta &&
      typeof e.delta === 'object' &&
      typeof (e.delta as Record<string, unknown>).text === 'string'
    ) {
      return (e.delta as Record<string, string>).text;
    }
  }
  return undefined;
}

export function extractToolUse(event: unknown): { toolName: string; toolInput: unknown; toolResult?: unknown } | undefined {
  if (!event || typeof event !== 'object') return undefined;
  const e = event as Record<string, unknown>;
  const type = typeof e.type === 'string' ? e.type : '';
  if (type !== 'tool_use' && type !== 'response.tool_use' && type !== 'tool_call') {
    return undefined;
  }
  const toolName =
    typeof e.toolName === 'string'
      ? e.toolName
      : typeof e.name === 'string'
        ? e.name
        : typeof e.tool === 'string'
          ? e.tool
          : 'unknown';
  return {
    toolName,
    toolInput: e.input ?? e.arguments ?? {},
    ...(e.result !== undefined ? { toolResult: e.result } : {}),
  };
}

export async function runCodexWorkflowSession(
  config: CodexWorkflowConfig,
  events: CodexWorkflowEvents,
): Promise<CodexWorkflowResult> {
  const contract = await validateCodexCliContract(resolveCodexExecutable(config.executable));
  const tmp = await mkdtemp(join(tmpdir(), 'pice-codex-'));
  const outputPath = join(tmp, 'last-message.txt');
  const args = [
    'exec',
    '--json',
    '--cd',
    config.workingDirectory,
    '--sandbox',
    'workspace-write',
    ...(contract.supportsAskForApproval ? ['--ask-for-approval', 'never'] : []),
    '--output-last-message',
    outputPath,
    ...(config.model ? ['-m', config.model] : []),
    '-',
  ];

  const prompt = config.systemPrompt
    ? `${config.systemPrompt}\n\n${config.message}`
    : config.message;
  const spec = spawnSpec(contract.executable, args);
  let stderr = '';
  let streamedText = '';
  let child: ChildProcess | undefined;

  try {
    child = spawn(spec.command, spec.args, {
      stdio: ['pipe', 'pipe', 'pipe'],
      env: process.env,
    });
    registerChild(config.sessionId, child);
    if (!child.stdin || !child.stdout || !child.stderr) {
      throw Object.assign(new Error('failed to open Codex CLI stdio pipes'), { code: -32000 });
    }
    const runningChild = child;
    runningChild.stderr?.on('data', (chunk: Buffer) => {
      stderr += chunk.toString();
    });

    const exitPromise = new Promise<number | null>((resolve, reject) => {
      runningChild.on('error', reject);
      runningChild.on('close', resolve);
    });

    runningChild.stdin?.end(prompt);

    const rl = createInterface({ input: runningChild.stdout!, crlfDelay: Infinity });
    for await (const line of rl) {
      if (line.trim() === '') continue;
      let event: unknown;
      try {
        event = JSON.parse(line);
      } catch {
        throw Object.assign(new Error(`Codex CLI emitted non-JSONL stdout: ${line}`), {
          code: -32000,
        });
      }
      const chunk = extractTextDelta(event);
      if (chunk) {
        streamedText += chunk;
        events.onChunk(chunk);
      }
      const toolUse = extractToolUse(event);
      if (toolUse) {
        events.onToolUse?.(toolUse);
      }
    }

    const code = await exitPromise;
    if (code !== 0) {
      throw Object.assign(
        new Error(`Codex CLI exited with code ${code}: ${stderr.trim()}`),
        { code: -32000 },
      );
    }

    let finalText: string;
    try {
      finalText = await readFile(outputPath, 'utf8');
    } catch (err) {
      const detail = err instanceof Error ? err.message : String(err);
      throw Object.assign(
        new Error(`Codex CLI did not write --output-last-message: ${detail}`),
        { code: -32000 },
      );
    }
    if (finalText && streamedText !== finalText) {
      const completionSuffix =
        streamedText.length === 0
          ? finalText
          : finalText.startsWith(streamedText)
            ? finalText.slice(streamedText.length)
            : '';
      if (completionSuffix) {
        events.onChunk(completionSuffix);
      }
    }
    return { finalText };
  } finally {
    await terminateChild(child);
    unregisterChild(config.sessionId, child);
    await rm(tmp, { recursive: true, force: true });
  }
}

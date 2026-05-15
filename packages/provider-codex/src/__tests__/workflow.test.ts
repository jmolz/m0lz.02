import { afterEach, describe, expect, it, vi } from 'vitest';
import { access, chmod, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { setTimeout as delay } from 'node:timers/promises';
import {
  extractTextDelta,
  runCodexWorkflowSession,
  terminateCodexWorkflowSession,
  validateCodexCliContract,
} from '../workflow.js';

const HELP = `Run Codex non-interactively

Usage: codex exec [OPTIONS] [PROMPT]

Options:
  --json
  --cd <DIR>
  --sandbox <SANDBOX_MODE>
  --output-last-message <FILE>

If '-' is used, instructions are read from stdin.
`;

async function fakeCodex(): Promise<{ dir: string; path: string }> {
  const dir = await mkdtemp(join(tmpdir(), 'pice-fake-codex-'));
  const path = join(dir, 'fake-codex.mjs');
  await writeFile(
    path,
    `#!/usr/bin/env node
import { writeFileSync } from 'node:fs';

const args = process.argv.slice(2);

if (process.env.FAKE_CODEX_TERM_MARKER) {
  process.on('SIGTERM', () => {
    writeFileSync(process.env.FAKE_CODEX_TERM_MARKER, 'terminated');
    process.exit(0);
  });
}

if (args.includes('--version')) {
  if (process.env.FAKE_CODEX_HANG_VERSION === '1') {
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0);
  } else {
    console.log(process.env.FAKE_CODEX_VERSION ?? 'codex-cli 0.130.0');
    process.exit(0);
  }
}

if (args[0] === 'exec' && args.includes('--help')) {
  if (process.env.FAKE_CODEX_HANG_HELP === '1') {
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0);
  } else {
    console.log(process.env.FAKE_CODEX_HELP ?? ${JSON.stringify(HELP)});
    process.exit(0);
  }
}

if (args[0] !== 'exec') {
  console.error('unexpected command: ' + args.join(' '));
  process.exit(64);
}

if (process.env.FAKE_CODEX_STARTED_MARKER) {
  writeFileSync(process.env.FAKE_CODEX_STARTED_MARKER, 'started');
}

let stdin = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (chunk) => {
  stdin += chunk;
});
process.stdin.on('end', () => {
  if (process.env.FAKE_CODEX_ECHO_STDIN === '1') {
    console.log(JSON.stringify({ type: 'agent_message_delta', delta: stdin }));
  }
  const events = JSON.parse(process.env.FAKE_CODEX_EVENTS ?? '[]');
  for (const event of events) {
    if (typeof event === 'string') {
      console.log(event);
    } else {
      console.log(JSON.stringify(event));
    }
  }
  const outFlag = args.indexOf('--output-last-message');
  const shortOutFlag = args.indexOf('-o');
  const outPath =
    outFlag >= 0 ? args[outFlag + 1] : shortOutFlag >= 0 ? args[shortOutFlag + 1] : undefined;
  if (!outPath) {
    console.error('missing output file');
    process.exit(65);
  }
  writeFileSync(outPath, process.env.FAKE_CODEX_FINAL ?? 'fake final');
  if (process.env.FAKE_CODEX_STDERR) {
    console.error(process.env.FAKE_CODEX_STDERR);
  }
  if (process.env.FAKE_CODEX_STAY_ALIVE === '1') {
    setInterval(() => undefined, 1000);
    return;
  }
  process.exit(Number(process.env.FAKE_CODEX_EXIT ?? '0'));
});
`,
  );
  await chmod(path, 0o755);
  return { dir, path };
}

async function waitForFile(path: string, timeoutMs = 5_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      await access(path);
      return;
    } catch {
      await delay(50);
    }
  }
  throw new Error(`file was not created within ${timeoutMs}ms: ${path}`);
}

afterEach(() => {
  vi.unstubAllEnvs();
});

describe('validateCodexCliContract', () => {
  it('accepts codex-cli 0.130.0 with required non-secret exec flags', async () => {
    const fake = await fakeCodex();
    try {
      const contract = await validateCodexCliContract(fake.path);
      expect(contract.version).toBe('0.130.0');
      expect(contract.supportsAskForApproval).toBe(false);
    } finally {
      await rm(fake.dir, { recursive: true, force: true });
    }
  });

  it('fails fast when required exec flags are absent', async () => {
    const fake = await fakeCodex();
    vi.stubEnv('FAKE_CODEX_HELP', HELP.replace('--output-last-message <FILE>', ''));
    try {
      await expect(validateCodexCliContract(fake.path)).rejects.toThrow(
        /missing --output-last-message/,
      );
    } finally {
      await rm(fake.dir, { recursive: true, force: true });
    }
  });

  it('times out hung contract probes before model execution', async () => {
    const fake = await fakeCodex();
    vi.stubEnv('FAKE_CODEX_HANG_VERSION', '1');
    vi.stubEnv('PICE_CODEX_CLI_PROBE_TIMEOUT_MS', '100');
    try {
      await expect(validateCodexCliContract(fake.path)).rejects.toThrow(
        /contract probe timed out/,
      );
    } finally {
      await rm(fake.dir, { recursive: true, force: true });
    }
  });
});

describe('runCodexWorkflowSession', () => {
  it('streams Codex JSONL deltas as response chunks and avoids duplicate final text', async () => {
    const fake = await fakeCodex();
    const chunks: string[] = [];
    vi.stubEnv(
      'FAKE_CODEX_EVENTS',
      JSON.stringify([
        { type: 'response.output_text.delta', delta: 'Hello ' },
        { type: 'agent_message_delta', delta: 'world' },
        { type: 'unknown.future_event', ignored: true },
      ]),
    );
    vi.stubEnv('FAKE_CODEX_FINAL', 'Hello world');
    try {
      const result = await runCodexWorkflowSession(
        {
          sessionId: 's1',
          message: 'prompt',
          workingDirectory: fake.dir,
          executable: fake.path,
        },
        { onChunk: (text) => chunks.push(text) },
      );
      expect(result.finalText).toBe('Hello world');
      expect(chunks).toEqual(['Hello ', 'world']);
    } finally {
      await rm(fake.dir, { recursive: true, force: true });
    }
  });

  it('emits output-last-message as a final chunk when JSONL has no text', async () => {
    const fake = await fakeCodex();
    const chunks: string[] = [];
    vi.stubEnv('FAKE_CODEX_EVENTS', JSON.stringify([]));
    vi.stubEnv('FAKE_CODEX_FINAL', 'final only');
    try {
      await runCodexWorkflowSession(
        {
          sessionId: 's1',
          message: 'prompt',
          workingDirectory: fake.dir,
          executable: fake.path,
        },
        { onChunk: (text) => chunks.push(text) },
      );
      expect(chunks).toEqual(['final only']);
    } finally {
      await rm(fake.dir, { recursive: true, force: true });
    }
  });

  it('emits only the missing suffix when streamed text is a prefix of final text', async () => {
    const fake = await fakeCodex();
    const chunks: string[] = [];
    vi.stubEnv(
      'FAKE_CODEX_EVENTS',
      JSON.stringify([{ type: 'response.output_text.delta', delta: 'partial ' }]),
    );
    vi.stubEnv('FAKE_CODEX_FINAL', 'partial final');
    try {
      await runCodexWorkflowSession(
        {
          sessionId: 's1',
          message: 'prompt',
          workingDirectory: fake.dir,
          executable: fake.path,
        },
        { onChunk: (text) => chunks.push(text) },
      );
      expect(chunks).toEqual(['partial ', 'final']);
    } finally {
      await rm(fake.dir, { recursive: true, force: true });
    }
  });

  it('does not replay unrelated streamed text as the authoritative final message', async () => {
    const fake = await fakeCodex();
    const chunks: string[] = [];
    vi.stubEnv(
      'FAKE_CODEX_EVENTS',
      JSON.stringify([{ type: 'response.output_text.delta', delta: 'progress only' }]),
    );
    vi.stubEnv('FAKE_CODEX_FINAL', 'authoritative final');
    try {
      const result = await runCodexWorkflowSession(
        {
          sessionId: 's1',
          message: 'prompt',
          workingDirectory: fake.dir,
          executable: fake.path,
        },
        { onChunk: (text) => chunks.push(text) },
      );
      expect(result.finalText).toBe('authoritative final');
      expect(chunks).toEqual(['progress only']);
    } finally {
      await rm(fake.dir, { recursive: true, force: true });
    }
  });

  it('maps child-process failures into JSON-RPC-shaped provider errors', async () => {
    const fake = await fakeCodex();
    vi.stubEnv('FAKE_CODEX_EXIT', '7');
    vi.stubEnv('FAKE_CODEX_STDERR', 'model unavailable');
    try {
      await expect(
        runCodexWorkflowSession(
          {
            sessionId: 's1',
            message: 'prompt',
            workingDirectory: fake.dir,
            executable: fake.path,
          },
          { onChunk: () => undefined },
        ),
      ).rejects.toMatchObject({ code: -32000 });
    } finally {
      await rm(fake.dir, { recursive: true, force: true });
    }
  });

  it('rejects non-JSONL stdout instead of forwarding raw child output', async () => {
    const fake = await fakeCodex();
    vi.stubEnv('FAKE_CODEX_EVENTS', JSON.stringify(['raw text from child stdout']));
    try {
      await expect(
        runCodexWorkflowSession(
          {
            sessionId: 's1',
            message: 'prompt',
            workingDirectory: fake.dir,
            executable: fake.path,
          },
          { onChunk: () => undefined },
        ),
      ).rejects.toThrow(/non-JSONL stdout/);
    } finally {
      await rm(fake.dir, { recursive: true, force: true });
    }
  });

  it('terminates the Codex child when stdout parsing fails before child exit', async () => {
    const fake = await fakeCodex();
    const marker = join(fake.dir, 'terminated');
    vi.stubEnv('FAKE_CODEX_EVENTS', JSON.stringify(['raw text from child stdout']));
    vi.stubEnv('FAKE_CODEX_STAY_ALIVE', '1');
    vi.stubEnv('FAKE_CODEX_TERM_MARKER', marker);
    try {
      await expect(
        runCodexWorkflowSession(
          {
            sessionId: 's1',
            message: 'prompt',
            workingDirectory: fake.dir,
            executable: fake.path,
          },
          { onChunk: () => undefined },
        ),
      ).rejects.toThrow(/non-JSONL stdout/);
      await waitForFile(marker);
    } finally {
      await rm(fake.dir, { recursive: true, force: true });
    }
  });

  it('terminates an active Codex child by workflow session id', async () => {
    const fake = await fakeCodex();
    const started = join(fake.dir, 'started');
    const marker = join(fake.dir, 'terminated-by-session');
    vi.stubEnv('FAKE_CODEX_STAY_ALIVE', '1');
    vi.stubEnv('FAKE_CODEX_STARTED_MARKER', started);
    vi.stubEnv('FAKE_CODEX_TERM_MARKER', marker);
    vi.stubEnv('FAKE_CODEX_FINAL', 'final before cancellation');
    const promise = runCodexWorkflowSession(
      {
        sessionId: 's-cancel',
        message: 'prompt',
        workingDirectory: fake.dir,
        executable: fake.path,
      },
      { onChunk: () => undefined },
    );
    try {
      await waitForFile(started);
      await terminateCodexWorkflowSession('s-cancel');
      await waitForFile(marker);
      await promise.catch((err: unknown) => {
        expect(err).toMatchObject({ code: -32000 });
      });
    } finally {
      await rm(fake.dir, { recursive: true, force: true });
    }
  });
});

describe('extractTextDelta', () => {
  it('ignores unknown JSON objects', () => {
    expect(extractTextDelta({ type: 'future_event', text: 'not raw text' })).toBeUndefined();
  });
});

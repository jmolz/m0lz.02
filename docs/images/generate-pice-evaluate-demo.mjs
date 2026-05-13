#!/usr/bin/env node
import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import sharp from 'sharp';

const WIDTH = 960;
const HEIGHT = 540;
const LINE_START_Y = 138;
const LINE_HEIGHT = 27;
const PROGRESS_Y = HEIGHT - 58;
const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const OUTPUT = join(repoRoot, 'docs/images/pice-evaluate-demo.gif');
const FRAME_DIR = mkdtempSync(join(tmpdir(), 'pice-readme-demo-'));

const lines = [
  { text: '$ npm install -g @jacobmolz/pice', kind: 'cmd' },
  { text: 'pice and pice-daemon installed', kind: 'out' },
  { text: '$ pice init', kind: 'cmd' },
  { text: 'created .pice/workflow.yaml and .codex/', kind: 'ok' },
  { text: '$ pice layers detect --json', kind: 'cmd' },
  { text: 'detected infrastructure, database, api, frontend', kind: 'ok' },
  { text: '$ pice evaluate .codex/plans/stack-loops.md --background --wait', kind: 'cmd' },
  { text: 'run stack-loops-phase8 admitted', kind: 'out' },
  { text: 'infrastructure + database passed', kind: 'ok' },
  { text: 'api + frontend passed with isolated contracts', kind: 'ok' },
  { text: 'review gate approved and resumed', kind: 'ok' },
  { text: 'overall_status=passed confidence=0.94', kind: 'out' },
];

function escapeXml(value) {
  return value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

function colorFor(kind) {
  switch (kind) {
    case 'cmd':
      return '#f5f5f0';
    case 'ok':
      return '#9ad7b4';
    default:
      return '#b7b7ad';
  }
}

function lineSvg(line, index) {
  const y = LINE_START_Y + index * LINE_HEIGHT;
  return `<text x="64" y="${y}" fill="${colorFor(line.kind)}" font-family="SFMono-Regular, Menlo, Consolas, monospace" font-size="18">${escapeXml(line.text)}</text>`;
}

function frameSvg(visibleCount, cursorOn) {
  const visible = lines.slice(0, visibleCount);
  const cursorY = LINE_START_Y + Math.max(0, visible.length - 1) * LINE_HEIGHT;
  const progress = Math.round((visibleCount / lines.length) * 100);
  const progressWidth = Math.round((WIDTH - 128) * (visibleCount / lines.length));

  return `<svg width="${WIDTH}" height="${HEIGHT}" viewBox="0 0 ${WIDTH} ${HEIGHT}" xmlns="http://www.w3.org/2000/svg">
<rect width="${WIDTH}" height="${HEIGHT}" fill="#090909"/>
<rect x="34" y="34" width="${WIDTH - 68}" height="${HEIGHT - 68}" rx="12" fill="#111111" stroke="#2c2c2c" stroke-width="2"/>
<circle cx="66" cy="66" r="6" fill="#ff5f57"/>
<circle cx="88" cy="66" r="6" fill="#ffbd2e"/>
<circle cx="110" cy="66" r="6" fill="#28c840"/>
<text x="64" y="104" fill="#f5f5f0" font-family="Inter, Arial, sans-serif" font-size="27" font-weight="700">m0lz.02 Stack Loops</text>
<text x="${WIDTH - 170}" y="104" fill="#8f8f86" font-family="SFMono-Regular, Menlo, Consolas, monospace" font-size="16">${progress}%</text>
<rect x="64" y="${PROGRESS_Y}" width="${WIDTH - 128}" height="4" rx="2" fill="#2b2b2b"/>
<rect x="64" y="${PROGRESS_Y}" width="${progressWidth}" height="4" rx="2" fill="#f5f5f0"/>
${visible.map(lineSvg).join('\n')}
${cursorOn ? `<rect x="64" y="${cursorY + 9}" width="10" height="20" fill="#f5f5f0"/>` : ''}
</svg>`;
}

async function main() {
  const frames = [];
  for (let visible = 1; visible <= lines.length; visible += 1) {
    frames.push({ visible, cursorOn: true });
    frames.push({ visible, cursorOn: false });
  }
  for (let i = 0; i < 8; i += 1) {
    frames.push({ visible: lines.length, cursorOn: i % 2 === 0 });
  }

  try {
    for (let i = 0; i < frames.length; i += 1) {
      const name = join(FRAME_DIR, `frame-${String(i).padStart(3, '0')}.png`);
      await sharp(Buffer.from(frameSvg(frames[i].visible, frames[i].cursorOn)))
        .png()
        .toFile(name);
    }

    execFileSync(
      'ffmpeg',
      [
        '-y',
        '-framerate',
        '3',
        '-i',
        join(FRAME_DIR, 'frame-%03d.png'),
        '-vf',
        'fps=6,split[s0][s1];[s0]palettegen[p];[s1][p]paletteuse',
        OUTPUT,
      ],
      { stdio: 'ignore' },
    );
  } finally {
    rmSync(FRAME_DIR, { recursive: true, force: true });
  }
}

main().catch((err) => {
  console.error(err instanceof Error ? err.message : String(err));
  process.exit(1);
});

#!/usr/bin/env node
import { existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const readmePath = path.join(repoRoot, 'README.md');
const outPath =
  process.env.PICE_README_MEDIA_EVIDENCE ??
  path.join(repoRoot, 'docs/releases/readme-media-evidence.json');

const readme = readFileSync(readmePath, 'utf8');
const banned = [
  'Tests: 418',
  'single binary',
  'v0.1 ships',
  'pice seams',
  'pice affected',
  'CLAUDE.md',
  '217 tests',
];
const bannedAssetText = [
  'Rust binary with core engine',
  'pice (Rust binary)',
  'single Rust binary',
  'stdio-only',
  'GPT-5.4',
  'gpt-5.4',
  '.claude/plans/auth-plan.md',
  'Auth endpoints return 401',
  'Password hashing uses bcrypt',
  'No secrets in git history',
];

const failures = [];
for (const phrase of banned) {
  if (readme.includes(phrase)) {
    failures.push(`README contains stale phrase: ${phrase}`);
  }
}

function stripDecorators(raw) {
  return raw.trim().replace(/^['"]|['"]$/g, '').split(/\s+/)[0].split('#')[0].split('?')[0];
}

function isExternal(src) {
  return /^(https?:|data:|mailto:|#)/i.test(src);
}

const refs = [];
for (const match of readme.matchAll(/!\[([^\]]*)\]\(([^)]+)\)/g)) {
  refs.push({ kind: 'markdown-image', alt: match[1].trim(), src: stripDecorators(match[2]) });
}
for (const match of readme.matchAll(/<img\b[^>]*\bsrc=["']([^"']+)["'][^>]*>/gi)) {
  const tag = match[0];
  const alt = tag.match(/\balt=["']([^"']*)["']/i)?.[1]?.trim() ?? '';
  refs.push({ kind: 'html-img', alt, src: stripDecorators(match[1]) });
}
for (const match of readme.matchAll(/<source\b[^>]*\bsrcset=["']([^"']+)["'][^>]*>/gi)) {
  for (const part of match[1].split(',')) {
    refs.push({ kind: 'html-source', alt: '(source)', src: stripDecorators(part) });
  }
}

const checked = [];
for (const ref of refs) {
  if (!ref.src || isExternal(ref.src)) {
    checked.push({ ...ref, status: 'external-or-empty' });
    continue;
  }
  const abs = path.resolve(repoRoot, ref.src);
  if (!abs.startsWith(repoRoot)) {
    failures.push(`media path escapes repo: ${ref.src}`);
    continue;
  }
  if (!existsSync(abs)) {
    failures.push(`missing README media asset: ${ref.src}`);
    continue;
  }
  const stat = statSync(abs);
  if (!stat.isFile() || stat.size === 0) {
    failures.push(`README media asset is empty or not a file: ${ref.src}`);
  }
  if (ref.kind !== 'html-source' && ref.alt.length < 8) {
    failures.push(`README media alt text too short for ${ref.src}`);
  }
  if (path.extname(abs).toLowerCase() === '.svg') {
    const svg = readFileSync(abs, 'utf8');
    for (const phrase of bannedAssetText) {
      if (svg.includes(phrase)) {
        failures.push(`README SVG asset contains stale phrase ${JSON.stringify(phrase)}: ${ref.src}`);
      }
    }
  }
  checked.push({ ...ref, size_bytes: stat.size, status: 'ok' });
}

function collectFiles(dir, exts) {
  const found = [];
  if (!existsSync(dir)) {
    return found;
  }
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const abs = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      found.push(...collectFiles(abs, exts));
    } else if (entry.isFile() && exts.has(path.extname(entry.name).toLowerCase())) {
      found.push(abs);
    }
  }
  return found;
}

const firstPartyMediaSources = [
  ...collectFiles(path.join(repoRoot, 'docs/images'), new Set(['.svg', '.tape', '.sh', '.mjs'])),
  ...collectFiles(path.join(repoRoot, 'docs/diagrams'), new Set(['.excalidraw', '.svg'])),
];
const sourceTextChecks = [];
for (const abs of firstPartyMediaSources) {
  const rel = path.relative(repoRoot, abs);
  const text = readFileSync(abs, 'utf8');
  for (const phrase of bannedAssetText) {
    if (text.includes(phrase)) {
      failures.push(`first-party media source contains stale phrase ${JSON.stringify(phrase)}: ${rel}`);
    }
  }
  sourceTextChecks.push(rel);
}

const gifChecks = [];
for (const abs of collectFiles(path.join(repoRoot, 'docs/images'), new Set(['.gif']))) {
  const rel = path.relative(repoRoot, abs);
  const stat = statSync(abs);
  if (!stat.isFile() || stat.size === 0) {
    failures.push(`first-party GIF is empty or not a file: ${rel}`);
  }
  if (stat.size > 2 * 1024 * 1024) {
    failures.push(`first-party GIF exceeds 2 MiB release-doc budget: ${rel}`);
  }
  gifChecks.push({ src: rel, size_bytes: stat.size, status: 'ok' });
}

if (refs.length === 0) {
  failures.push('README has no media references to audit');
}

const evidence = {
  generated_at: new Date().toISOString(),
  readme: 'README.md',
  checked,
  first_party_media_sources_checked: sourceTextChecks,
  first_party_gifs_checked: gifChecks,
  stale_phrase_checks: banned.length,
  stale_asset_phrase_checks: bannedAssetText.length,
  failures,
};

mkdirSync(path.dirname(outPath), { recursive: true });
writeFileSync(outPath, `${JSON.stringify(evidence, null, 2)}\n`);

if (failures.length > 0) {
  console.error(failures.join('\n'));
  process.exit(1);
}

console.log(`README media audit passed (${checked.length} references)`);

#!/usr/bin/env node
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { DatabaseSync } from 'node:sqlite';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const outPath =
  process.env.PICE_METRICS_SCHEMA_EVIDENCE ??
  path.join(repoRoot, 'docs/releases/metrics-schema-evidence.json');

function exe(name) {
  return process.platform === 'win32' ? `${name}.exe` : name;
}

function run(cmd, args, options = {}) {
  return execFileSync(cmd, args, {
    cwd: options.cwd ?? repoRoot,
    env: { ...process.env, ...options.env },
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  });
}

function piceArgs(args, cwd, env) {
  const candidate = process.env.PICE_BIN ?? path.join(repoRoot, 'target/debug', exe('pice'));
  if (existsSync(candidate)) {
    return { cmd: candidate, args, cwd, env };
  }
  return { cmd: 'cargo', args: ['run', '-q', '-p', 'pice-cli', '--', ...args], cwd, env };
}

function runPice(args, cwd, env = {}) {
  const p = piceArgs(args, cwd, env);
  return run(p.cmd, p.args, { cwd: p.cwd, env: p.env });
}

function sqliteInventory(dbPath) {
  const db = new DatabaseSync(dbPath, { readOnly: true });
  const tables = {};
  try {
    const rows = db
      .prepare(
        "SELECT name, replace(sql, char(10), ' ') AS create_sql FROM sqlite_master WHERE type='table' ORDER BY name;"
      )
      .all();
    for (const row of rows) {
      tables[row.name] = row.create_sql ?? '';
    }
  } finally {
    db.close();
  }
  return { source: 'node:sqlite', tables };
}

function writePathEvidence() {
  const files = [
    'crates/pice-daemon/src/metrics/store.rs',
    'crates/pice-daemon/src/handlers/evaluate.rs',
    'crates/pice-daemon/src/orchestrator/adaptive_loop.rs',
  ];
  const haystack = files
    .map((file) => `${file}\n${readFileSync(path.join(repoRoot, file), 'utf8')}`)
    .join('\n');
  return {
    gate_decisions: /insert_gate_decision/.test(haystack),
    pass_events: /record_pass|INSERT INTO pass_events|pass_events/.test(haystack),
    seam_findings: /insert_seam_finding|seam_findings/.test(haystack),
    layer_runs: /insert_layer_run|layer_runs/.test(haystack),
    adaptive_halt_reasons: /halted_by|sprt_confidence_reached|vec_entropy|max_passes/.test(haystack),
  };
}

function extractStructFields(source, structName) {
  const match = source.match(new RegExp(`struct\\s+${structName}\\s*\\{([\\s\\S]*?)\\n\\}`));
  if (!match) {
    throw new Error(`could not find struct ${structName}`);
  }
  return [...match[1].matchAll(/pub\s+([a-zA-Z_][a-zA-Z0-9_]*)\s*:/g)].map((field) => field[1]);
}

function telemetryPrivacyEvidence() {
  const telemetryPath = 'crates/pice-daemon/src/metrics/telemetry.rs';
  const configPath = 'crates/pice-core/src/config/mod.rs';
  const templatePath = 'templates/pice/config.toml';
  const telemetrySource = readFileSync(path.join(repoRoot, telemetryPath), 'utf8');
  const configSource = readFileSync(path.join(repoRoot, configPath), 'utf8');
  const templateSource = readFileSync(path.join(repoRoot, templatePath), 'utf8');
  const eventFields = extractStructFields(telemetrySource, 'TelemetryEvent');
  const anonymizedBlock = telemetrySource.match(/struct\s+AnonymizedPayload\s*\{([\s\S]*?)\n\}/)?.[1] ?? '';
  const anonymizedFields = [...anonymizedBlock.matchAll(/^\s*([a-zA-Z_][a-zA-Z0-9_]*)\s*:/gm)].map(
    (field) => field[1]
  );
  const safeFields = ['event_type', 'tier', 'passed', 'score_avg', 'provider_type', 'timestamp'];
  const forbidden = {
    code: /\b(code|source|diff)\b/i.test(anonymizedBlock),
    prompts: /\b(prompt|contract_contents)\b/i.test(anonymizedBlock),
    paths: /\b(path|file|project_root)\b/i.test(anonymizedBlock),
    secrets: /\b(secret|token|key|password)\b/i.test(anonymizedBlock),
    pii: /\b(email|name|user|pii)\b/i.test(anonymizedBlock),
  };
  const evidence = {
    source: telemetryPath,
    default_config_enabled_false: /telemetry:\s*TelemetryConfig\s*\{[\s\S]*?enabled:\s*false/.test(
      configSource
    ),
    template_enabled_false: /\[telemetry\][\s\S]*?enabled\s*=\s*false/.test(templateSource),
    outbound_telemetry_opt_in: false,
    event_fields: eventFields,
    anonymized_payload_fields: anonymizedFields,
    anonymized_payload_exact_whitelist:
      anonymizedFields.length === safeFields.length &&
      anonymizedFields.every((field, index) => field === safeFields[index]),
    compile_time_new_field_guard: /let\s+TelemetryEvent\s*\{[\s\S]*event_type[\s\S]*timestamp[\s\S]*\}\s*=\s*event;/.test(
      telemetrySource
    ),
    tests_present: {
      anonymize_produces_separate_wire_type: telemetrySource.includes(
        'fn anonymize_produces_separate_wire_type'
      ),
      anonymize_wire_format_is_distinct_from_event: telemetrySource.includes(
        'fn anonymize_wire_format_is_distinct_from_event'
      ),
      jsonl_log_no_file_paths: telemetrySource.includes('fn jsonl_log_no_file_paths'),
    },
    excludes: Object.fromEntries(Object.entries(forbidden).map(([key, present]) => [key, !present])),
  };
  evidence.outbound_telemetry_opt_in =
    evidence.default_config_enabled_false && evidence.template_enabled_false;

  const failures = [];
  if (!evidence.outbound_telemetry_opt_in) {
    failures.push('telemetry is not disabled by default in config and template sources');
  }
  if (!evidence.anonymized_payload_exact_whitelist) {
    failures.push(`AnonymizedPayload fields are not the expected whitelist: ${anonymizedFields.join(', ')}`);
  }
  if (!evidence.compile_time_new_field_guard) {
    failures.push('anonymize() does not destructure TelemetryEvent as a compile-time field guard');
  }
  for (const [name, excluded] of Object.entries(evidence.excludes)) {
    if (!excluded) {
      failures.push(`AnonymizedPayload appears to include ${name}-shaped data`);
    }
  }
  for (const [name, present] of Object.entries(evidence.tests_present)) {
    if (!present) {
      failures.push(`missing telemetry privacy test: ${name}`);
    }
  }
  if (failures.length > 0) {
    throw new Error(`telemetry privacy evidence failed:\n${failures.join('\n')}`);
  }

  return evidence;
}

const work = path.join(tmpdir(), `pice-metrics-${process.pid}`);
rmSync(work, { recursive: true, force: true });
mkdirSync(work, { recursive: true });

try {
  run('git', ['init'], { cwd: work });
  runPice(['init', '--json'], work, {
    HOME: work,
    PICE_DAEMON_INLINE: '1',
  });

  const dbPath = path.join(work, '.pice/metrics.db');
  if (!existsSync(dbPath)) {
    throw new Error(`expected initialized metrics DB at ${dbPath}`);
  }

  const inventory = sqliteInventory(dbPath);
  const tables = inventory.tables;
  const surfaces = {
    gate_decisions: tables.gate_decisions ? 'exists' : 'not shipped',
    cost_events: tables.cost_events ? 'exists' : tables.pass_events ? 'implemented under pass_events.cost_usd' : 'not shipped',
    pass_events: tables.pass_events ? 'exists' : 'not shipped',
    seam_findings: tables.seam_findings ? 'exists' : 'not shipped',
    layer_runs: tables.layer_runs ? 'exists' : 'not shipped',
    adaptive_halt_reasons:
      tables.evaluations?.includes('halted_by') || tables.layer_runs?.includes('halted_by')
        ? 'exists'
        : 'not shipped',
  };
  const evidence = {
    generated_at: new Date().toISOString(),
    command: 'pice init --json',
    database: path.relative(repoRoot, dbPath),
    inventory_source: inventory.source,
    surfaces,
    write_paths: writePathEvidence(),
    telemetry_privacy: telemetryPrivacyEvidence(),
    tables,
  };

  for (const [name, status] of Object.entries(surfaces)) {
    if (status === 'not shipped' && name !== 'cost_events') {
      throw new Error(`missing required metrics surface: ${name}`);
    }
  }

  mkdirSync(path.dirname(outPath), { recursive: true });
  writeFileSync(outPath, `${JSON.stringify(evidence, null, 2)}\n`);
  console.log(`metrics schema inventory wrote ${path.relative(repoRoot, outPath)}`);
} finally {
  rmSync(work, { recursive: true, force: true });
}

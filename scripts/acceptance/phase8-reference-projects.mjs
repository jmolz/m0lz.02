#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { DatabaseSync } from 'node:sqlite';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const fixtureRoot = path.join(repoRoot, 'fixtures/reference-projects');
const outPath =
  process.env.PICE_PHASE8_REFERENCE_EVIDENCE ??
  path.join(repoRoot, 'docs/releases/phase8-reference-evidence.json');
const fixtures = ['next-prisma', 'fastapi-postgres', 'rails', 'express-mongo', 'sveltekit-supabase'];
const acceptanceCommands = [
  'pice init --json',
  'pice init --upgrade --json',
  'pice layers detect/check/list --json',
  'pice validate --json',
  'pice daemon start',
  'pice evaluate <plan> --background --wait --timeout-secs 30 --json',
  'pice status <feature-id> --follow --stream-json',
  'pice logs <feature-id> --follow --stream-json',
  'pice status <feature-id> --wait --timeout-secs 30 --json',
  'pice status <feature-id> --json',
  'pice logs <feature-id> --json',
  'pice review-gate --list --feature-id <feature-id> --json',
  'pice review-gate --gate-id <gate-id> --decision approve --json',
];

function exe(name) {
  return process.platform === 'win32' ? `${name}.exe` : name;
}

function run(cmd, args, options = {}) {
  const result = spawnSync(cmd, args, {
    cwd: options.cwd ?? repoRoot,
    env: { ...process.env, ...options.env },
    encoding: 'utf8',
    timeout: options.timeout ?? 60_000,
  });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== (options.status ?? 0)) {
    throw new Error(
      `${cmd} ${args.join(' ')} exited ${result.status}, expected ${options.status ?? 0}\nstdout:\n${result.stdout}\nstderr:\n${result.stderr}`
    );
  }
  return { stdout: result.stdout, stderr: result.stderr, status: result.status };
}

function removeTree(target) {
  rmSync(target, {
    recursive: true,
    force: true,
    maxRetries: process.platform === 'win32' ? 10 : 2,
    retryDelay: 250,
  });
}

function ensureBuiltBinaries() {
  const pice = process.env.PICE_BIN ?? path.join(repoRoot, 'target/debug', exe('pice'));
  const daemon = process.env.PICE_DAEMON_BIN ?? path.join(repoRoot, 'target/debug', exe('pice-daemon'));
  if (!process.env.PICE_BIN || !process.env.PICE_DAEMON_BIN) {
    run('cargo', ['build', '-p', 'pice-cli', '-p', 'pice-daemon'], { timeout: 180_000 });
  }
  if (!existsSync(pice) || !existsSync(daemon)) {
    throw new Error('expected debug pice and pice-daemon binaries after cargo build');
  }
  return { pice, daemon };
}

function ensureProviderStubBuilt() {
  const stub = path.join(repoRoot, 'packages/provider-stub/dist/bin.js');
  if (!existsSync(stub)) {
    throw new Error('packages/provider-stub/dist/bin.js is missing; run pnpm build before Phase 8 acceptance');
  }
}

function writeStubConfig(project) {
  writeFileSync(
    path.join(project, '.pice/config.toml'),
    `[provider]
name = "stub"

[evaluation.primary]
provider = "stub"
model = "stub-model"

[evaluation.adversarial]
provider = "stub"
model = "stub-model"
effort = ""
enabled = false

[evaluation.tiers]
tier1_models = ["stub-model"]
tier2_models = ["stub-model"]
tier3_models = ["stub-model"]
tier3_agent_team = false

[telemetry]
enabled = false
endpoint = ""

[metrics]
db_path = ".pice/metrics.db"
`
  );
}

function writeAcceptanceWorkflow(project, options = {}) {
  const review = options.reviewGate
    ? `review:
  enabled: true
  trigger: "layer == infrastructure"
  timeout_hours: 24
  on_timeout: reject
  retry_on_reject: 0
  notification: stdout
`
    : `review:
  enabled: false
`;
  writeFileSync(
    path.join(project, '.pice/workflow.yaml'),
    `schema_version: "0.2"
defaults:
  tier: 1
  min_confidence: 0.90
  max_passes: 8
  model: stub-model
  budget_usd: 2.0
  cost_cap_behavior: halt
  max_parallelism: 2
  max_global_provider_concurrency: 2
phases:
  evaluate:
    parallel: true
    seam_checks: true
layer_overrides:
  infrastructure:
    tier: 3
seams:
  "backend<->deployment":
    - cascade_timeout
${review}`
  );
}

function writePlan(project, name) {
  const planDir = path.join(project, '.claude/plans');
  mkdirSync(planDir, { recursive: true });
  const planPath = path.join(planDir, `${name}-phase8.md`);
  writeFileSync(
    planPath,
    `# Plan: ${name} Phase 8 acceptance

## Contract

\`\`\`json
{
  "feature": "${name}-phase8",
  "tier": 1,
  "pass_threshold": 8,
  "criteria": [
    {"name": "fixture validates", "threshold": 8, "validation": "pice validate --json"},
    {"name": "layer evaluation passes", "threshold": 8, "validation": "stub provider score"}
  ]
}
\`\`\`
`
  );
  return planPath;
}

function mutateFixture(project, name) {
  const filesByFixture = {
    'next-prisma': [
      'src/server/index.ts',
      'prisma/schema.prisma',
      'app/api/users/route.ts',
      'app/page.tsx',
      'infra/main.tf',
      'Dockerfile',
      'monitoring/alerts.yml',
    ],
    'fastapi-postgres': [
      'app/main.py',
      'migrations/001_init.sql',
      'app/db.py',
      'src/client/App.tsx',
      'infra/main.tf',
      'Dockerfile',
      'monitoring/alerts.yml',
    ],
    rails: [
      'app/controllers/users_controller.rb',
      'app/models/user.rb',
      'app/views/users/index.html.erb',
      'infra/main.tf',
      'Dockerfile',
      'monitoring/alerts.yml',
    ],
    'express-mongo': [
      'src/server.ts',
      'src/routes/users.ts',
      'src/models/user.ts',
      'src/client/App.tsx',
      'infra/main.tf',
      'Dockerfile',
      'monitoring/alerts.yml',
    ],
    'sveltekit-supabase': [
      'src/server/index.ts',
      'migrations/001_users.sql',
      'src/routes/api/users/+server.ts',
      'src/routes/+page.svelte',
      'infra/main.tf',
      'Dockerfile',
      'monitoring/alerts.yml',
    ],
  };
  for (const file of filesByFixture[name] ?? ['Dockerfile']) {
    const target = path.join(project, file);
    const current = readFileSync(target, 'utf8');
    const marker = file.endsWith('.ts') || file.endsWith('.tsx') ? '// phase8 acceptance mutation' : '# phase8 acceptance mutation';
    writeFileSync(target, `${current}\n${marker}\n`);
  }
}

function pice(bin, args, cwd, env = {}, options = {}) {
  return run(bin, args, { cwd, env, timeout: options.timeout ?? 90_000, status: options.status ?? 0 });
}

function socketPath(base, name) {
  if (process.platform === 'win32') {
    return `\\\\.\\pipe\\pice-phase8-${process.pid}-${name}`;
  }
  return path.join(base, `${name}.sock`);
}

function parseJson(stdout, context) {
  try {
    return JSON.parse(stdout);
  } catch (err) {
    throw new Error(`failed to parse JSON for ${context}: ${err.message}\n${stdout}`);
  }
}

function featureIdFrom(value, fallback) {
  return value.feature_id ?? value.featureId ?? value.feature ?? fallback;
}

function assertTerminalStream(result, context, expectedExitCode = 0) {
  const lines = result.stdout.trim().split(/\r?\n/).filter(Boolean);
  if (lines.length === 0) {
    throw new Error(`${context} emitted no stream-json frames`);
  }
  const frames = lines.map((line) => {
    try {
      return JSON.parse(line);
    } catch (err) {
      throw new Error(`${context} emitted invalid NDJSON: ${err.message}\n${line}`);
    }
  });
  const terminal = frames.find((frame) => frame.kind === 'terminal');
  if (!terminal) {
    throw new Error(`${context} did not emit a terminal frame`);
  }
  if (terminal.exit_code !== expectedExitCode) {
    throw new Error(
      `${context} terminal exit_code ${terminal.exit_code}, expected ${expectedExitCode}`
    );
  }
  const progressFrames = frames.filter((frame) => frame.kind !== 'terminal').length;
  if (progressFrames === 0) {
    throw new Error(`${context} emitted terminal without a progress/snapshot frame`);
  }
  return {
    frames: frames.length,
    progress_frames: progressFrames,
    terminal_frame: true,
    terminal_exit_code: terminal.exit_code,
    terminal_status: terminal.status ?? null,
  };
}

function runStream(bin, args, cwd, env, context, expectedExitCode = 0) {
  const result = pice(bin, args, cwd, env, { timeout: 45_000, status: expectedExitCode });
  return assertTerminalStream(result, context, expectedExitCode);
}

function assertPassedStatus(value, context, expectedLayerCount) {
  if (value.overall_status !== 'passed') {
    throw new Error(`${context} expected overall_status=passed, got ${value.overall_status}`);
  }
  const layers = value.layers ?? [];
  const badLayers = layers.filter((layer) => layer.status !== 'passed' && layer.status !== 'skipped');
  if (badLayers.length > 0) {
    throw new Error(`${context} has non-terminal/non-passing layers: ${JSON.stringify(badLayers)}`);
  }
  if (expectedLayerCount !== undefined && layers.length !== expectedLayerCount) {
    throw new Error(`${context} expected ${expectedLayerCount} runtime layers, got ${layers.length}`);
  }
  return {
    overall_status: value.overall_status,
    layer_count: layers.length,
    passed_layers: layers.filter((layer) => layer.status === 'passed').length,
    skipped_layers: layers.filter((layer) => layer.status === 'skipped').length,
  };
}

function assertAuditDecision(bin, project, env, featureId, gateId) {
  const audit = parseJson(
    pice(bin, ['audit', '--json', 'gates', '--feature', featureId], project, env).stdout,
    `${featureId} audit gates`
  );
  const row = audit.decisions?.find((decision) => decision.gate_id === gateId);
  if (!row) {
    throw new Error(`${featureId} expected audit row for gate ${gateId}`);
  }
  if (row.decision !== 'approve') {
    throw new Error(`${featureId} expected audit decision approve, got ${row.decision}`);
  }
  return {
    id: row.id,
    gate_id: row.gate_id,
    decision: row.decision,
    layer: row.layer,
    reviewer: row.reviewer ?? null,
  };
}

function sqliteScalar(dbPath, sql, ...params) {
  const db = new DatabaseSync(dbPath, { readOnly: true });
  try {
    const row = db.prepare(sql).get(...params);
    if (!row) {
      return '';
    }
    return Object.values(row)[0] ?? '';
  } finally {
    db.close();
  }
}

function assertBackgroundMetricsRows(project, featureId, expectGateDecision, expectedLayerRows) {
  const dbPath = path.join(project, '.pice/metrics.db');
  if (!existsSync(dbPath)) {
    throw new Error(`${featureId} expected metrics DB at ${dbPath}`);
  }
  const evaluations = Number(sqliteScalar(dbPath, 'SELECT COUNT(*) FROM evaluations;'));
  const passEvents = Number(sqliteScalar(dbPath, 'SELECT COUNT(*) FROM pass_events;'));
  const seamFindings = Number(sqliteScalar(dbPath, 'SELECT COUNT(*) FROM seam_findings;'));
  const layerRuns = Number(
    sqliteScalar(dbPath, 'SELECT COUNT(*) FROM layer_runs WHERE feature_id = ?;', featureId)
  );
  const distinctLayerRuns = Number(
    sqliteScalar(dbPath, 'SELECT COUNT(DISTINCT layer) FROM layer_runs WHERE feature_id = ?;', featureId)
  );
  const latestEvaluationId = Number(
    sqliteScalar(
      dbPath,
      'SELECT COALESCE(MAX(evaluation_id), 0) FROM layer_runs WHERE feature_id = ?;',
      featureId
    )
  );
  const latestLayerRuns =
    latestEvaluationId > 0
      ? Number(
          sqliteScalar(
            dbPath,
            'SELECT COUNT(*) FROM layer_runs WHERE feature_id = ? AND evaluation_id = ?;',
            featureId,
            latestEvaluationId
          )
        )
      : layerRuns;
  const gateDecisions = Number(
    sqliteScalar(dbPath, 'SELECT COUNT(*) FROM gate_decisions WHERE feature_id = ?;', featureId)
  );
  const infrastructureTier = Number(
    sqliteScalar(
      dbPath,
      "SELECT COALESCE(MAX(contract_tier), 0) FROM layer_runs WHERE feature_id = ? AND layer = 'infrastructure';",
      featureId
    )
  );

  const required = [
    ['evaluations', evaluations],
    ['pass_events', passEvents],
    ['seam_findings', seamFindings],
    ['layer_runs', layerRuns],
  ];
  for (const [table, count] of required) {
    if (count <= 0) {
      throw new Error(`${featureId} expected background evaluate to write ${table} rows`);
    }
  }
  if (expectedLayerRows !== undefined && distinctLayerRuns !== expectedLayerRows) {
    throw new Error(
      `${featureId} expected ${expectedLayerRows} distinct layer_runs layers, got ${distinctLayerRuns}`
    );
  }
  if (expectedLayerRows !== undefined && latestLayerRuns !== expectedLayerRows) {
    throw new Error(
      `${featureId} expected latest evaluation to write ${expectedLayerRows} layer_runs rows, got ${latestLayerRuns}`
    );
  }
  if (expectGateDecision && gateDecisions <= 0) {
    throw new Error(`${featureId} expected review-gate flow to write gate_decisions rows`);
  }
  if (infrastructureTier !== 3) {
    throw new Error(`${featureId} expected infrastructure layer_run contract_tier=3, got ${infrastructureTier}`);
  }

  return {
    status: 'verified',
    database: path.relative(project, dbPath),
    evaluations,
    pass_events: passEvents,
    seam_findings: seamFindings,
    layer_runs: layerRuns,
    distinct_layer_runs: distinctLayerRuns,
    latest_evaluation_id: latestEvaluationId || null,
    latest_evaluation_layer_runs: latestLayerRuns,
    gate_decisions: gateDecisions,
    infrastructure_contract_tier: infrastructureTier,
  };
}

function runFixture(name, binaries, workRoot) {
  const source = path.join(fixtureRoot, name);
  if (!existsSync(source)) {
    throw new Error(`missing fixture ${source}`);
  }

  const project = path.join(workRoot, name);
  cpSync(source, project, { recursive: true });
  run('git', ['init'], { cwd: project });
  run('git', ['config', 'user.email', 'phase8@example.invalid'], { cwd: project });
  run('git', ['config', 'user.name', 'Phase 8'], { cwd: project });
  run('git', ['add', '.'], { cwd: project });
  run('git', ['commit', '-m', 'fixture baseline'], { cwd: project });
  mutateFixture(project, name);

  const commonEnv = {
    HOME: path.join(workRoot, `${name}-home`),
    PICE_STATE_DIR: path.join(workRoot, `${name}-state`),
    PICE_STUB_SCORES: '9.5,0.001;9.5,0.001;9.5,0.001;9.5,0.001;9.5,0.001;9.5,0.001;9.5,0.001;9.5,0.001',
    PICE_STUB_LATENCY_MS: '100',
    USER: 'phase8-acceptance',
  };
  mkdirSync(commonEnv.HOME, { recursive: true });
  mkdirSync(commonEnv.PICE_STATE_DIR, { recursive: true });

  const init = parseJson(
    pice(binaries.pice, ['init', '--json'], project, { ...commonEnv, PICE_DAEMON_INLINE: '1' }).stdout,
    `${name} init`
  );
  writeStubConfig(project);
  const reviewGate = name === 'fastapi-postgres';
  writeAcceptanceWorkflow(project, { reviewGate });
  const plan = writePlan(project, name);
  const upgrade = parseJson(
    pice(binaries.pice, ['init', '--upgrade', '--json'], project, { ...commonEnv, PICE_DAEMON_INLINE: '1' }).stdout,
    `${name} init --upgrade`
  );
  const layers = parseJson(
    pice(binaries.pice, ['layers', '--json', 'detect'], project, { ...commonEnv, PICE_DAEMON_INLINE: '1' }).stdout,
    `${name} layers detect`
  );
  pice(binaries.pice, ['layers', '--json', 'check'], project, { ...commonEnv, PICE_DAEMON_INLINE: '1' });
  const listedLayers = parseJson(
    pice(binaries.pice, ['layers', '--json', 'list'], project, { ...commonEnv, PICE_DAEMON_INLINE: '1' }).stdout,
    `${name} layers list`
  );
  if ((listedLayers.layers?.length ?? 0) !== 7) {
    throw new Error(`${name} expected 7 configured Stack Loop layers, got ${listedLayers.layers?.length ?? 0}`);
  }
  pice(binaries.pice, ['validate', '--json'], project, { ...commonEnv, PICE_DAEMON_INLINE: '1' });

  const socket = socketPath(workRoot, name);
  const daemonEnv = {
    ...commonEnv,
    PICE_DAEMON_SOCKET: socket,
    PICE_DAEMON_BIN: binaries.daemon,
    PATH: `${path.dirname(binaries.daemon)}${path.delimiter}${process.env.PATH ?? ''}`,
  };
  pice(binaries.pice, ['daemon', 'start'], project, daemonEnv, { timeout: 20_000 });
  let featureId;
  let evaluateStatus;
  let evaluateWaitEvidence = null;
  let reviewEvidence = null;
  let statusStream;
  let logsStream;
  let finalStatusEvidence;

  if (reviewGate) {
    const pending = parseJson(
      pice(
        binaries.pice,
        ['evaluate', plan, '--background', '--wait', '--timeout-secs', '30', '--json'],
        project,
        daemonEnv,
        { timeout: 60_000, status: 3 }
      ).stdout,
      `${name} evaluate pending review`
    );
    featureId = featureIdFrom(pending, `${name}-phase8`);
    const gates = parseJson(
      pice(binaries.pice, ['review-gate', '--list', '--feature-id', featureId, '--json'], project, daemonEnv).stdout,
      `${name} review-gate list pending`
    );
    const gate = gates.gates?.[0];
    if (!gate?.id) {
      throw new Error(`${name} expected a pending review gate for ${featureId}`);
    }
    const decision = parseJson(
      pice(
        binaries.pice,
        ['review-gate', '--gate-id', gate.id, '--decision', 'approve', '--reason', 'Phase 8 acceptance', '--json'],
        project,
        daemonEnv
      ).stdout,
      `${name} review-gate approve`
    );
    if ((decision.audit_id ?? 0) <= 0) {
      throw new Error(`${name} review-gate approval did not return an audit_id`);
    }
    const resumed = parseJson(
      pice(
        binaries.pice,
        ['evaluate', plan, '--background', '--wait', '--timeout-secs', '30', '--json'],
        project,
        daemonEnv,
        { timeout: 60_000 }
      ).stdout,
      `${name} evaluate resume after gate approval`
    );
    const waited = parseJson(
      pice(
        binaries.pice,
        ['status', featureId, '--wait', '--timeout-secs', '30', '--json'],
        project,
        daemonEnv,
        { timeout: 45_000 }
      ).stdout,
      `${name} status --wait after gate approval`
    );
    const status = parseJson(
      pice(binaries.pice, ['status', featureId, '--json'], project, daemonEnv).stdout,
      `${name} final status`
    );
    finalStatusEvidence = assertPassedStatus(status, `${name} final status`, listedLayers.layers.length);
    const auditDecision = assertAuditDecision(binaries.pice, project, daemonEnv, featureId, gate.id);
    evaluateStatus = finalStatusEvidence.overall_status;
    reviewEvidence = {
      gate_id: gate.id,
      layer: gate.layer,
      decision: decision.decision,
      audit_id: decision.audit_id,
      resume_status: resumed.status ?? resumed.overall_status ?? null,
      wait_status: waited.status ?? null,
      audit_decision: auditDecision,
    };
    statusStream = runStream(
      binaries.pice,
      ['status', '--follow', featureId, '--stream-json'],
      project,
      daemonEnv,
      `${name} status follow stream-json`
    );
    logsStream = runStream(
      binaries.pice,
      ['logs', featureId, '--follow', '--stream-json'],
      project,
      daemonEnv,
      `${name} logs follow stream-json`
    );
  } else {
    const waited = parseJson(
      pice(
        binaries.pice,
        ['evaluate', plan, '--background', '--wait', '--timeout-secs', '30', '--json'],
        project,
        daemonEnv,
        { timeout: 60_000 }
      ).stdout,
      `${name} evaluate --background --wait`
    );
    featureId = featureIdFrom(waited, `${name}-phase8`);
    evaluateWaitEvidence = {
      status: waited.status ?? null,
      overall_status: waited.overall_status ?? null,
    };
    statusStream = runStream(
      binaries.pice,
      ['status', '--follow', featureId, '--stream-json'],
      project,
      daemonEnv,
      `${name} status follow stream-json`
    );
    logsStream = runStream(
      binaries.pice,
      ['logs', featureId, '--follow', '--stream-json'],
      project,
      daemonEnv,
      `${name} logs follow stream-json`
    );
    const status = parseJson(
      pice(binaries.pice, ['status', featureId, '--json'], project, daemonEnv).stdout,
      `${name} final status`
    );
    finalStatusEvidence = assertPassedStatus(status, `${name} final status`, listedLayers.layers.length);
    evaluateStatus = finalStatusEvidence.overall_status;
  }

  pice(binaries.pice, ['status', featureId, '--json'], project, daemonEnv);
  pice(binaries.pice, ['logs', featureId, '--json'], project, daemonEnv);
  const gates = parseJson(
    pice(binaries.pice, ['review-gate', '--list', '--feature-id', featureId, '--json'], project, daemonEnv).stdout,
    `${name} review-gate list`
  );
  const metricsRows = assertBackgroundMetricsRows(
    project,
    featureId,
    reviewGate,
    listedLayers.layers.length
  );
  pice(binaries.pice, ['daemon', 'stop'], project, daemonEnv, { timeout: 20_000 });

  return {
    fixture: name,
    init_created: init.totalCreated ?? init.created?.length ?? null,
    upgrade_created: upgrade.created?.length ?? null,
    detected_layers: layers.layers?.length ?? layers.detected?.length ?? null,
    configured_layers: listedLayers.layers.length,
    configured_layer_names: listedLayers.layers.map((layer) => layer.name),
    feature_id: featureId,
    evaluate_status: evaluateStatus,
    evaluate_wait: evaluateWaitEvidence,
    final_status: finalStatusEvidence,
    review_gate: reviewEvidence,
    status_follow_stream_json: statusStream,
    logs_follow_stream_json: logsStream,
    background_metrics_rows: metricsRows,
    pending_gates: gates.pending_gates?.length ?? gates.gates?.length ?? 0,
  };
}

function runFocusedRegressionSelectors() {
  if (process.env.PICE_PHASE8_SKIP_FOCUSED_TESTS === '1') {
    return { status: 'skipped by env' };
  }
  const selectors = [
    ['cargo', ['test', '-p', 'pice-cli', '--test', 'evaluate_background_wait_live_integration', 'evaluate_background_wait_json_uses_second_subscribe_until_feature_complete', '--', '--nocapture']],
    ['cargo', ['test', '-p', 'pice-cli', '--test', 'status_follow_live_stream_json_integration', 'status_follow_stream_json_drains_live_burst_and_terminal_status_alias', '--', '--nocapture']],
    ['cargo', ['test', '-p', 'pice-cli', '--test', 'terminal_short_circuit_live_cli_integration', 'logs_follow_stream_json_terminal_frame_carries_logs_stream_ended_status', '--', '--nocapture']],
    ['cargo', ['test', '-p', 'pice-daemon', '--test', 'review_gate_lifecycle_integration', 'scenario_4b_reject_without_retry_halts_with_gate_rejected', '--', '--nocapture']],
  ];
  for (const [cmd, args] of selectors) {
    run(cmd, args, { timeout: 180_000 });
  }
  return { status: 'passed', selectors: selectors.map(([cmd, args]) => `${cmd} ${args.join(' ')}`) };
}

function assertBackgroundWaitCoverage(results, commands) {
  if (!commands.includes('pice evaluate <plan> --background --wait --timeout-secs 30 --json')) {
    throw new Error('Phase 8 acceptance command list must include background wait evaluate');
  }
  if (commands.some((command) => command.includes('--background --json'))) {
    throw new Error('Phase 8 acceptance command list must not advertise non-wait background evaluate');
  }
  const missing = results.filter((result) => {
    if (result.review_gate) {
      return result.review_gate.resume_status !== 'passed' || result.review_gate.wait_status !== 'passed';
    }
    return result.evaluate_wait?.status !== 'passed' && result.evaluate_wait?.overall_status !== 'passed';
  });
  if (missing.length > 0) {
    throw new Error(
      `Phase 8 acceptance did not prove background wait evaluate for: ${missing
        .map((result) => result.fixture)
        .join(', ')}`
    );
  }
}

ensureProviderStubBuilt();
const binaries = ensureBuiltBinaries();
const workRoot = path.join(tmpdir(), `pice-phase8-reference-${process.pid}`);
removeTree(workRoot);
mkdirSync(workRoot, { recursive: true });

try {
  const results = fixtures.map((fixture) => runFixture(fixture, binaries, workRoot));
  assertBackgroundWaitCoverage(results, acceptanceCommands);
  const focused = runFocusedRegressionSelectors();
  const evidence = {
    generated_at: new Date().toISOString(),
    fixtures: results,
    focused_regressions: focused,
    commands: acceptanceCommands,
  };
  mkdirSync(path.dirname(outPath), { recursive: true });
  writeFileSync(outPath, `${JSON.stringify(evidence, null, 2)}\n`);
  console.log(`Phase 8 reference acceptance passed for ${results.length} fixtures`);
} finally {
  if (process.env.PICE_PHASE8_KEEP_WORK === '1') {
    console.error(`kept Phase 8 acceptance workdir at ${workRoot}`);
  } else {
    removeTree(workRoot);
  }
}

#!/usr/bin/env node
import { execFileSync, spawnSync } from 'node:child_process';
import { copyFileSync, cpSync, existsSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const outPath =
  process.env.PICE_RELEASE_SMOKE_EVIDENCE ??
  path.join(repoRoot, 'docs/releases/release-artifact-smoke-evidence.json');

function exe(name) {
  return process.platform === 'win32' ? `${name}.exe` : name;
}

function run(cmd, args, options = {}) {
  const result = spawnSync(cmd, args, {
    cwd: options.cwd ?? repoRoot,
    env: { ...process.env, ...options.env },
    encoding: 'utf8',
    timeout: options.timeout ?? 20_000,
  });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    throw new Error(
      `${cmd} ${args.join(' ')} exited ${result.status}\nstdout:\n${result.stdout}\nstderr:\n${result.stderr}`
    );
  }
  return { stdout: result.stdout, stderr: result.stderr };
}

function sleepMs(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

function isTransientWindowsRemoveError(err) {
  return process.platform === 'win32' && ['EBUSY', 'ENOTEMPTY', 'EPERM'].includes(err?.code);
}

function rmRetrySync(target, options = {}) {
  const attempts = process.platform === 'win32' ? 8 : 1;
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    try {
      rmSync(target, options);
      return;
    } catch (err) {
      if (!isTransientWindowsRemoveError(err) || attempt === attempts) {
        throw err;
      }
      sleepMs(Math.min(100 * 2 ** (attempt - 1), 2_000));
    }
  }
}

function winCmdQuote(value) {
  return `"${String(value).replace(/"/g, '""')}"`;
}

function runNpmBin(cmd, args, options = {}) {
  if (process.platform !== 'win32' || !/\.(cmd|bat)$/i.test(cmd)) {
    return run(cmd, args, options);
  }

  const commandLine = [winCmdQuote(cmd), ...args.map(winCmdQuote)].join(' ');
  return run(process.env.ComSpec || 'cmd.exe', ['/d', '/s', '/c', commandLine], options);
}

function relativeOrAbsolute(p) {
  const rel = path.relative(repoRoot, p);
  return rel && !rel.startsWith('..') && !path.isAbsolute(rel) ? rel : p;
}

function artifactDir() {
  if (process.env.PICE_ARTIFACT_DIR) {
    return path.resolve(process.env.PICE_ARTIFACT_DIR);
  }
  const releaseDir = path.join(repoRoot, 'target/release');
  if (existsSync(path.join(releaseDir, exe('pice'))) && existsSync(path.join(releaseDir, exe('pice-daemon')))) {
    return releaseDir;
  }
  if (process.env.PICE_RELEASE_SMOKE_ALLOW_DEBUG === '1') {
    return path.join(repoRoot, 'target/debug');
  }
  return releaseDir;
}

function psQuote(value) {
  return `'${value.replace(/'/g, "''")}'`;
}

function unpackArchive(archivePath) {
  const archive = path.resolve(archivePath);
  if (!existsSync(archive)) {
    throw new Error(`release archive does not exist: ${archive}`);
  }

  const work = path.join(tmpdir(), `pice-archive-smoke-${process.pid}`);
  rmRetrySync(work, { recursive: true, force: true });
  mkdirSync(work, { recursive: true });

  if (archive.endsWith('.tar.gz')) {
    run('tar', ['-xzf', archive, '-C', work]);
  } else if (archive.endsWith('.zip')) {
    if (process.platform === 'win32') {
      run('powershell', [
        '-NoProfile',
        '-Command',
        `Expand-Archive -LiteralPath ${psQuote(archive)} -DestinationPath ${psQuote(work)} -Force`,
      ]);
    } else {
      run('unzip', ['-q', archive, '-d', work]);
    }
  } else {
    rmRetrySync(work, { recursive: true, force: true });
    throw new Error(`unsupported release archive extension: ${archive}`);
  }

  return { dir: work, cleanup: () => rmRetrySync(work, { recursive: true, force: true }) };
}

function packLocalReleaseArchive() {
  const releaseDir = path.join(repoRoot, 'target/release');
  const pice = path.join(releaseDir, exe('pice'));
  const daemon = path.join(releaseDir, exe('pice-daemon'));
  if (!existsSync(pice) || !existsSync(daemon)) {
    return null;
  }

  const archive =
    process.platform === 'win32'
      ? path.join(tmpdir(), `pice-release-smoke-local-${process.pid}.zip`)
      : path.join(tmpdir(), `pice-release-smoke-local-${process.pid}.tar.gz`);
  rmRetrySync(archive, { force: true });
  if (process.platform === 'win32') {
    run('powershell', [
      '-NoProfile',
      '-Command',
      `Compress-Archive -LiteralPath ${psQuote(pice)},${psQuote(daemon)} -DestinationPath ${psQuote(archive)} -Force`,
    ]);
  } else {
    run('tar', ['-czf', archive, '-C', releaseDir, exe('pice'), exe('pice-daemon')]);
  }
  return { archive, cleanup: () => rmRetrySync(archive, { force: true }) };
}

function artifactInput() {
  if (process.env.PICE_ARTIFACT_ARCHIVE) {
    const archive = path.resolve(process.env.PICE_ARTIFACT_ARCHIVE);
    const unpacked = unpackArchive(archive);
    return {
      kind: 'archive',
      archive,
      dir: unpacked.dir,
      cleanup: unpacked.cleanup,
    };
  }
  const localArchive = packLocalReleaseArchive();
  if (localArchive) {
    const unpacked = unpackArchive(localArchive.archive);
    return {
      kind: 'local-release-archive',
      archive: localArchive.archive,
      dir: unpacked.dir,
      cleanup: () => {
        unpacked.cleanup();
        localArchive.cleanup();
      },
    };
  }
  if (process.env.PICE_RELEASE_SMOKE_ALLOW_DIR !== '1') {
    throw new Error(
      'PICE_ARTIFACT_ARCHIVE or target/release binaries are required for release smoke; set PICE_RELEASE_SMOKE_ALLOW_DIR=1 only for local binary-directory debugging'
    );
  }
  return {
    kind: 'directory',
    archive: null,
    dir: artifactDir(),
    cleanup: () => {},
  };
}

function smokeBinaries(dir) {
  const pice = path.join(dir, exe('pice'));
  const daemon = path.join(dir, exe('pice-daemon'));
  if (!existsSync(pice) || !existsSync(daemon)) {
    throw new Error(`expected pice and pice-daemon in ${dir}`);
  }

  const work = path.join(tmpdir(), `pice-artifact-smoke-${process.pid}`);
  rmSync(work, { recursive: true, force: true });
  mkdirSync(work, { recursive: true });

  const env = {
    HOME: work,
    USERPROFILE: work,
    PICE_DAEMON_SOCKET:
      process.platform === 'win32'
        ? `\\\\.\\pipe\\pice-artifact-smoke-${process.pid}`
        : path.join(work, 'daemon.sock'),
    PICE_STATE_DIR: path.join(work, 'state'),
    PICE_DAEMON_BIN: daemon,
  };

  try {
    const version = run(pice, ['--version'], { env }).stdout.trim();
    run(pice, ['--help'], { env });
    run(pice, ['init', '--json'], { cwd: work, env: { ...env, PICE_DAEMON_INLINE: '1' } });
    run(pice, ['validate', '--json'], { cwd: work, env: { ...env, PICE_DAEMON_INLINE: '1' } });
    JSON.parse(run(pice, ['status', '--json'], { cwd: work, env }).stdout);
    const status = run(pice, ['daemon', 'status'], { cwd: work, env }).stdout;
    run(pice, ['daemon', 'stop'], { cwd: work, env });
    const daemonStatus = status.trim();
    if (/not running/i.test(daemonStatus) || daemonStatus.length === 0) {
      throw new Error(`daemon status did not report a running daemon: ${daemonStatus}`);
    }
    run(pice, ['daemon', 'start'], { cwd: work, env });
    const explicitStatus = run(pice, ['daemon', 'status'], { cwd: work, env }).stdout.trim();
    run(pice, ['daemon', 'stop'], { cwd: work, env });
    if (/not running/i.test(explicitStatus) || explicitStatus.length === 0) {
      throw new Error(`explicit daemon start/status did not report a running daemon: ${explicitStatus}`);
    }
    return {
      version,
      auto_start_verified: true,
      daemon_status: daemonStatus,
      explicit_start_status: explicitStatus,
    };
  } finally {
    rmRetrySync(work, { recursive: true, force: true });
  }
}

function commandExists(cmd) {
  const checker = process.platform === 'win32' ? 'where' : 'command';
  const args = process.platform === 'win32' ? [cmd] : ['-v', cmd];
  return spawnSync(checker, args, { stdio: 'ignore', shell: process.platform !== 'win32' }).status === 0;
}

function smokeNpmPackedInstall(artifactDirForBinaries) {
  if (process.env.PICE_NPM_PACK_SMOKE !== '1') {
    return { status: 'not requested' };
  }
  if (!commandExists('npm')) {
    throw new Error('PICE_NPM_PACK_SMOKE=1 requires npm on PATH');
  }

  const platformKey = `${process.platform}-${process.arch}`;
  const platformPkg = {
    'darwin-arm64': 'pice-darwin-arm64',
    'darwin-x64': 'pice-darwin-x64',
    'linux-arm64': 'pice-linux-arm64',
    'linux-x64': 'pice-linux-x64',
    'win32-x64': 'pice-win32-x64',
  }[platformKey];
  if (!platformPkg) {
    return { status: 'unsupported platform for npm smoke', platform: platformKey };
  }

  let platformDir = path.join(repoRoot, 'npm', platformPkg);
  let mainDir = path.join(repoRoot, 'npm/pice');
  let stagedFromArtifact = false;
  const stagingRoot = path.join(tmpdir(), `pice-npm-stage-${process.pid}`);
  const platformBin = () => path.join(platformDir, exe('pice'));
  const platformDaemon = () => path.join(platformDir, exe('pice-daemon'));
  if (!existsSync(platformBin()) || !existsSync(platformDaemon())) {
    const artifactBin = path.join(artifactDirForBinaries, exe('pice'));
    const artifactDaemon = path.join(artifactDirForBinaries, exe('pice-daemon'));
    if (!existsSync(artifactBin) || !existsSync(artifactDaemon)) {
      throw new Error(`npm smoke requires copied binaries in ${platformDir} or built binaries in ${artifactDirForBinaries}`);
    }
    rmRetrySync(stagingRoot, { recursive: true, force: true });
    platformDir = path.join(stagingRoot, platformPkg);
    mainDir = path.join(stagingRoot, 'pice');
    cpSync(path.join(repoRoot, 'npm', platformPkg), platformDir, { recursive: true });
    cpSync(path.join(repoRoot, 'npm/pice'), mainDir, { recursive: true });
    copyFileSync(artifactBin, platformBin());
    copyFileSync(artifactDaemon, platformDaemon());
    stagedFromArtifact = true;
  }

  const work = path.join(tmpdir(), `pice-npm-smoke-${process.pid}`);
  rmRetrySync(work, { recursive: true, force: true });
  mkdirSync(work, { recursive: true });
  try {
    const platformTar = run('npm', ['pack', platformDir, '--pack-destination', work], { cwd: repoRoot }).stdout.trim().split(/\r?\n/).pop();
    const mainTar = run('npm', ['pack', mainDir, '--pack-destination', work], { cwd: repoRoot }).stdout.trim().split(/\r?\n/).pop();
    run('npm', ['init', '-y'], { cwd: work });
    run('npm', ['install', path.join(work, platformTar), path.join(work, mainTar)], { cwd: work, timeout: 60_000 });
    const piceBin = process.platform === 'win32'
      ? path.join(work, 'node_modules/.bin/pice.cmd')
      : path.join(work, 'node_modules/.bin/pice');
    const env = {
      HOME: path.join(work, 'home'),
      USERPROFILE: path.join(work, 'home'),
      PICE_DAEMON_SOCKET:
        process.platform === 'win32'
          ? `\\\\.\\pipe\\pice-npm-smoke-${process.pid}`
          : path.join(work, 'daemon.sock'),
      PICE_STATE_DIR: path.join(work, 'state'),
    };
    mkdirSync(env.HOME, { recursive: true });
    const version = runNpmBin(piceBin, ['--version'], { cwd: work, env }).stdout.trim();
    JSON.parse(runNpmBin(piceBin, ['status', '--json'], { cwd: work, env }).stdout);
    const status = runNpmBin(piceBin, ['daemon', 'status'], { cwd: work, env }).stdout.trim();
    runNpmBin(piceBin, ['daemon', 'stop'], { cwd: work, env });
    if (/not running/i.test(status) || status.length === 0) {
      throw new Error(`npm-installed daemon status did not report running: ${status}`);
    }
    runNpmBin(piceBin, ['daemon', 'start'], { cwd: work, env });
    const explicitStatus = runNpmBin(piceBin, ['daemon', 'status'], { cwd: work, env }).stdout.trim();
    runNpmBin(piceBin, ['daemon', 'stop'], { cwd: work, env });
    if (/not running/i.test(explicitStatus) || explicitStatus.length === 0) {
      throw new Error(`npm-installed explicit daemon start/status did not report running: ${explicitStatus}`);
    }
    return {
      status: 'passed',
      platform: platformKey,
      version,
      auto_start_verified: true,
      explicit_start_verified: true,
      staged_from_artifact: stagedFromArtifact,
    };
  } finally {
    rmRetrySync(work, { recursive: true, force: true });
    rmRetrySync(stagingRoot, { recursive: true, force: true });
  }
}

const input = artifactInput();
try {
  const evidence = {
    generated_at: new Date().toISOString(),
    artifact_kind: input.kind,
    artifact_archive: input.archive ? relativeOrAbsolute(input.archive) : null,
    artifact_dir: relativeOrAbsolute(input.dir),
    binaries: smokeBinaries(input.dir),
    npm_pack_local_install: smokeNpmPackedInstall(input.dir),
  };

  mkdirSync(path.dirname(outPath), { recursive: true });
  writeFileSync(outPath, `${JSON.stringify(evidence, null, 2)}\n`);
  console.log(
    `release artifact smoke passed using ${
      input.archive ? relativeOrAbsolute(input.archive) : relativeOrAbsolute(input.dir)
    }`
  );
} finally {
  input.cleanup();
}

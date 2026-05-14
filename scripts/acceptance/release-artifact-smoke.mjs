#!/usr/bin/env node
import { execFileSync, spawnSync } from 'node:child_process';
import { copyFileSync, cpSync, existsSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const outPath =
  process.env.PICE_RELEASE_SMOKE_EVIDENCE ??
  path.join(repoRoot, 'docs/releases/release-artifact-smoke-evidence.json');
const daemonCommandTimeout = process.platform === 'win32' ? 90_000 : 30_000;
const daemonStopTimeout = process.platform === 'win32' ? 45_000 : 20_000;

function exe(name) {
  return process.platform === 'win32' ? `${name}.exe` : name;
}

function run(cmd, args, options = {}) {
  const stdio = options.stdio ?? 'pipe';
  const result = spawnSync(cmd, args, {
    cwd: options.cwd ?? repoRoot,
    env: { ...process.env, ...options.env },
    encoding: 'utf8',
    timeout: options.timeout ?? 20_000,
    stdio,
  });
  const stdout = typeof result.stdout === 'string' ? result.stdout : '';
  const stderr = typeof result.stderr === 'string' ? result.stderr : '';
  if (result.error) {
    result.error.message = `${cmd} ${args.join(' ')} failed: ${result.error.message}\nstdout:\n${stdout}\nstderr:\n${stderr}`;
    throw result.error;
  }
  if (result.status !== 0) {
    throw new Error(
      `${cmd} ${args.join(' ')} exited ${result.status}\nstdout:\n${stdout}\nstderr:\n${stderr}`
    );
  }
  return { stdout, stderr };
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

function cleanupRetrySync(target, options = {}) {
  try {
    rmRetrySync(target, options);
  } catch (err) {
    if (!isTransientWindowsRemoveError(err)) {
      throw err;
    }
    console.warn(`warning: could not remove temporary path ${target}: ${err.code}`);
  }
}

function isWindowsDaemonStopDisconnect(err, platform = process.platform) {
  return (
    platform === 'win32' &&
    /failed to send shutdown|daemon closed connection during shutdown|pipe is being closed|os error 232|transport write .*failed/i.test(
      err?.message ?? ''
    )
  );
}

function stopDaemonAndWait(runner, cmd, cwd, env, platform = process.platform) {
  let stopError = null;
  const deadline = Date.now() + daemonStopTimeout;
  let nextStopAttemptAt = 0;
  let lastStatus = '';
  while (Date.now() < deadline) {
    if (Date.now() >= nextStopAttemptAt) {
      try {
        runner(cmd, ['daemon', 'stop'], { cwd, env, timeout: daemonStopTimeout });
      } catch (err) {
        if (!isWindowsDaemonStopDisconnect(err, platform)) {
          throw err;
        }
        stopError = err;
      }
      nextStopAttemptAt = Date.now() + (platform === 'win32' ? 1_000 : daemonStopTimeout);
    }

    lastStatus = runner(cmd, ['daemon', 'status'], { cwd, env, timeout: daemonCommandTimeout }).stdout.trim();
    if (/not running/i.test(lastStatus)) {
      if (platform === 'win32') {
        sleepMs(500);
      }
      return;
    }
    sleepMs(250);
  }

  const stopDetail = stopError ? `; stop error: ${stopError.message}` : '';
  throw new Error(`daemon did not stop within ${daemonStopTimeout / 1_000}s; last status: ${lastStatus}${stopDetail}`);
}

function stopDaemonBestEffort(runner, cmd, cwd, env) {
  try {
    stopDaemonAndWait(runner, cmd, cwd, env);
  } catch (err) {
    console.warn(`warning: could not stop smoke daemon: ${err.message}`);
  }
}

function runNpmBin(cmd, args, options = {}) {
  if (process.platform !== 'win32' || !/\.(cmd|bat)$/i.test(cmd)) {
    return run(cmd, args, options);
  }

  return run(process.env.ComSpec || 'cmd.exe', ['/d', '/c', cmd, ...args], options);
}

function runDaemonStart(runner, cmd, cwd, env) {
  runner(cmd, ['daemon', 'start'], {
    cwd,
    env,
    timeout: daemonCommandTimeout,
    // Windows release smoke runs under spawnSync capture. The daemon is a
    // long-lived grandchild, so avoid captured stdio handles on the start call
    // and verify readiness with a separate status probe that exits normally.
    stdio: process.platform === 'win32' ? 'ignore' : 'pipe',
  });
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

  return { dir: work, cleanup: () => cleanupRetrySync(work, { recursive: true, force: true }) };
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
  return { archive, cleanup: () => cleanupRetrySync(archive, { force: true }) };
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
  rmRetrySync(work, { recursive: true, force: true });
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

    let autoStartVerified = false;
    let daemonStatus;
    let explicitStatus;
    if (process.platform === 'win32') {
      runDaemonStart(run, pice, work, env);
      daemonStatus = run(pice, ['daemon', 'status'], { cwd: work, env, timeout: daemonCommandTimeout }).stdout.trim();
      stopDaemonAndWait(run, pice, work, env);
      explicitStatus = daemonStatus;
    } else {
      JSON.parse(run(pice, ['status', '--json'], { cwd: work, env, timeout: daemonCommandTimeout }).stdout);
      daemonStatus = run(pice, ['daemon', 'status'], { cwd: work, env, timeout: daemonCommandTimeout }).stdout.trim();
      stopDaemonAndWait(run, pice, work, env);
      run(pice, ['daemon', 'start'], { cwd: work, env, timeout: daemonCommandTimeout });
      explicitStatus = run(pice, ['daemon', 'status'], { cwd: work, env, timeout: daemonCommandTimeout }).stdout.trim();
      stopDaemonAndWait(run, pice, work, env);
      autoStartVerified = true;
    }
    if (/not running/i.test(daemonStatus) || daemonStatus.length === 0) {
      throw new Error(`daemon status did not report a running daemon: ${daemonStatus}`);
    }
    if (/not running/i.test(explicitStatus) || explicitStatus.length === 0) {
      throw new Error(`explicit daemon start/status did not report a running daemon: ${explicitStatus}`);
    }
    return {
      version,
      auto_start_verified: autoStartVerified,
      inline_validate_verified: true,
      daemon_lifecycle_verified: true,
      daemon_status: daemonStatus,
      explicit_start_status: explicitStatus,
    };
  } finally {
    stopDaemonBestEffort(run, pice, work, env);
    cleanupRetrySync(work, { recursive: true, force: true });
  }
}

function commandExists(cmd) {
  const checker = process.platform === 'win32' ? 'where' : 'command';
  const args = process.platform === 'win32' ? [cmd] : ['-v', cmd];
  return spawnSync(checker, args, { stdio: 'ignore', shell: process.platform !== 'win32' }).status === 0;
}

function npmCommand() {
  return process.platform === 'win32' ? 'npm.cmd' : 'npm';
}

function smokeNpmPackedInstall(artifactDirForBinaries) {
  if (process.env.PICE_NPM_PACK_SMOKE !== '1') {
    return { status: 'not requested' };
  }
  const npmCmd = npmCommand();
  if (!commandExists(npmCmd)) {
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
  let piceBin = null;
  let npmEnv = null;
  try {
    const platformTar = runNpmBin(npmCmd, ['pack', platformDir, '--pack-destination', work], { cwd: repoRoot }).stdout.trim().split(/\r?\n/).pop();
    const mainTar = runNpmBin(npmCmd, ['pack', mainDir, '--pack-destination', work], { cwd: repoRoot }).stdout.trim().split(/\r?\n/).pop();
    runNpmBin(npmCmd, ['init', '-y'], { cwd: work });
    runNpmBin(npmCmd, ['install', path.join(work, platformTar), path.join(work, mainTar)], { cwd: work, timeout: 60_000 });
    piceBin = process.platform === 'win32'
      ? path.join(work, 'node_modules/.bin/pice.cmd')
      : path.join(work, 'node_modules/.bin/pice');
    npmEnv = {
      HOME: path.join(work, 'home'),
      USERPROFILE: path.join(work, 'home'),
      PICE_DAEMON_SOCKET:
        process.platform === 'win32'
          ? `\\\\.\\pipe\\pice-npm-smoke-${process.pid}`
          : path.join(work, 'daemon.sock'),
      PICE_STATE_DIR: path.join(work, 'state'),
    };
    mkdirSync(npmEnv.HOME, { recursive: true });
    const version = runNpmBin(piceBin, ['--version'], { cwd: work, env: npmEnv }).stdout.trim();

    let autoStartVerified = false;
    let daemonStatus;
    let explicitStatus;
    if (process.platform === 'win32') {
      runDaemonStart(runNpmBin, piceBin, work, npmEnv);
      daemonStatus = runNpmBin(piceBin, ['daemon', 'status'], { cwd: work, env: npmEnv, timeout: daemonCommandTimeout }).stdout.trim();
      stopDaemonAndWait(runNpmBin, piceBin, work, npmEnv);
      explicitStatus = daemonStatus;
    } else {
      JSON.parse(runNpmBin(piceBin, ['status', '--json'], { cwd: work, env: npmEnv, timeout: daemonCommandTimeout }).stdout);
      daemonStatus = runNpmBin(piceBin, ['daemon', 'status'], { cwd: work, env: npmEnv, timeout: daemonCommandTimeout }).stdout.trim();
      stopDaemonAndWait(runNpmBin, piceBin, work, npmEnv);
      runNpmBin(piceBin, ['daemon', 'start'], { cwd: work, env: npmEnv, timeout: daemonCommandTimeout });
      explicitStatus = runNpmBin(piceBin, ['daemon', 'status'], { cwd: work, env: npmEnv, timeout: daemonCommandTimeout }).stdout.trim();
      stopDaemonAndWait(runNpmBin, piceBin, work, npmEnv);
      autoStartVerified = true;
    }
    if (/not running/i.test(daemonStatus) || daemonStatus.length === 0) {
      throw new Error(`npm-installed daemon status did not report running: ${daemonStatus}`);
    }
    if (/not running/i.test(explicitStatus) || explicitStatus.length === 0) {
      throw new Error(`npm-installed explicit daemon start/status did not report running: ${explicitStatus}`);
    }
    return {
      status: 'passed',
      platform: platformKey,
      version,
      auto_start_verified: autoStartVerified,
      daemon_lifecycle_verified: true,
      daemon_status: daemonStatus,
      explicit_start_status: explicitStatus,
      staged_from_artifact: stagedFromArtifact,
    };
  } finally {
    if (piceBin && npmEnv) {
      stopDaemonBestEffort(runNpmBin, piceBin, work, npmEnv);
    }
    cleanupRetrySync(work, { recursive: true, force: true });
    cleanupRetrySync(stagingRoot, { recursive: true, force: true });
  }
}

function main() {
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
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main();
}

export { isWindowsDaemonStopDisconnect, stopDaemonAndWait };

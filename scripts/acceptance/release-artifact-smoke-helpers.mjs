const defaultDaemonCommandTimeout = process.platform === 'win32' ? 90_000 : 30_000;
const defaultDaemonStopTimeout = process.platform === 'win32' ? 45_000 : 20_000;

function sleepMs(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

function isWindowsDaemonStopDisconnect(err, platform = process.platform) {
  return (
    platform === 'win32' &&
    /failed to send shutdown|daemon closed connection during shutdown|pipe is being closed|os error 232|transport write .*failed/i.test(
      err?.message ?? ''
    )
  );
}

function stopDaemonAndWait(
  runner,
  cmd,
  cwd,
  env,
  platform = process.platform,
  {
    daemonCommandTimeout = defaultDaemonCommandTimeout,
    daemonStopTimeout = defaultDaemonStopTimeout,
  } = {}
) {
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

export { isWindowsDaemonStopDisconnect, stopDaemonAndWait };

import { describe, expect, it } from 'vitest';

import {
  isWindowsDaemonStopDisconnect,
  stopDaemonAndWait,
} from './release-artifact-smoke.mjs';

describe('release artifact smoke daemon teardown', () => {
  it('recognizes the Windows named-pipe shutdown race', () => {
    const err = new Error(
      'failed to send shutdown to daemon: transport write (frame delimiter) failed: The pipe is being closed. (os error 232)'
    );

    expect(isWindowsDaemonStopDisconnect(err, 'win32')).toBe(true);
    expect(isWindowsDaemonStopDisconnect(err, 'linux')).toBe(false);
  });

  it('polls status after a Windows shutdown disconnect', () => {
    const calls = [];
    const runner = (_cmd, args) => {
      calls.push(args.join(' '));
      if (args.join(' ') === 'daemon stop') {
        throw new Error(
          'failed to send shutdown to daemon: transport write (frame delimiter) failed: The pipe is being closed. (os error 232)'
        );
      }
      return { stdout: 'daemon is not running\n', stderr: '' };
    };

    expect(() => stopDaemonAndWait(runner, 'pice', '/tmp/work', {}, 'win32')).not.toThrow();
    expect(calls).toEqual(['daemon stop', 'daemon status']);
  });

  it('keeps non-Windows stop failures fatal', () => {
    const runner = (_cmd, args) => {
      if (args.join(' ') === 'daemon stop') {
        throw new Error('failed to send shutdown to daemon');
      }
      return { stdout: 'daemon is not running\n', stderr: '' };
    };

    expect(() => stopDaemonAndWait(runner, 'pice', '/tmp/work', {}, 'linux')).toThrow(
      'failed to send shutdown'
    );
  });
});

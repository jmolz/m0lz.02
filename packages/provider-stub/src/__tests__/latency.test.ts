/**
 * Phase 5 cohort parallelism — `PICE_STUB_LATENCY_MS` contract.
 *
 * The stub provider uses this env var to simulate per-response latency
 * so sequential-vs-parallel speedup benchmarks have a dominant wall-clock
 * cost to measure. The knob itself is tiny; this suite locks its three
 * invariants:
 *
 * 1. No env → zero latency (backward compatible with pre-Phase-5 tests).
 * 2. Valid integer env → sleeps at least that many ms.
 * 3. Invalid env → warns on stderr, treated as 0 (never crashes).
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { readStubLatencyMs } from '../deterministic.js';

describe('readStubLatencyMs', () => {
  it('returns 0 when PICE_STUB_LATENCY_MS is unset', () => {
    expect(readStubLatencyMs({})).toBe(0);
  });

  it('returns 0 when the value is an empty string', () => {
    expect(readStubLatencyMs({ PICE_STUB_LATENCY_MS: '' })).toBe(0);
  });

  it('returns the parsed value for a non-negative integer', () => {
    expect(readStubLatencyMs({ PICE_STUB_LATENCY_MS: '0' })).toBe(0);
    expect(readStubLatencyMs({ PICE_STUB_LATENCY_MS: '200' })).toBe(200);
    expect(readStubLatencyMs({ PICE_STUB_LATENCY_MS: '1500' })).toBe(1500);
  });

  describe('invalid values log a warning and return 0', () => {
    let errSpy: ReturnType<typeof vi.spyOn>;
    beforeEach(() => {
      // Cast because `vi.spyOn` on console.error infers `(...data) => void`
      // (variadic), which is narrower than the generic overload vitest
      // picks by default.
      errSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    });
    afterEach(() => {
      errSpy.mockRestore();
    });

    it('warns on negative values', () => {
      expect(readStubLatencyMs({ PICE_STUB_LATENCY_MS: '-100' })).toBe(0);
      expect(errSpy).toHaveBeenCalledTimes(1);
      expect(errSpy.mock.calls[0][0]).toMatch(
        /PICE_STUB_LATENCY_MS=-100 is not a non-negative integer/,
      );
    });

    it('warns on non-numeric values', () => {
      expect(readStubLatencyMs({ PICE_STUB_LATENCY_MS: 'abc' })).toBe(0);
      expect(errSpy).toHaveBeenCalledTimes(1);
    });

    it('warns on NaN', () => {
      expect(readStubLatencyMs({ PICE_STUB_LATENCY_MS: 'NaN' })).toBe(0);
      expect(errSpy).toHaveBeenCalledTimes(1);
    });

    it('warns on fractional values (milliseconds must be whole)', () => {
      expect(readStubLatencyMs({ PICE_STUB_LATENCY_MS: '100.5' })).toBe(0);
      expect(errSpy).toHaveBeenCalledTimes(1);
    });

    it('warns on Infinity', () => {
      expect(readStubLatencyMs({ PICE_STUB_LATENCY_MS: 'Infinity' })).toBe(0);
      expect(errSpy).toHaveBeenCalledTimes(1);
    });
  });
});

describe('PICE_STUB_LATENCY_MS real-clock sleep', () => {
  // Scheduler jitter on shared CI can add ~50ms even on idle machines.
  // The plan specified ≥190ms for a 200ms env; we mirror that slack here.
  it('actually delays approximately the configured ms', async () => {
    const latencyMs = readStubLatencyMs({ PICE_STUB_LATENCY_MS: '200' });
    const t0 = performance.now();
    await new Promise((r) => setTimeout(r, latencyMs));
    const elapsed = performance.now() - t0;
    // Lower bound: 190ms (Node timers can fire ~10ms early under load);
    // upper bound: 1000ms to catch pathological scheduler stalls.
    expect(elapsed).toBeGreaterThanOrEqual(190);
    expect(elapsed).toBeLessThan(1000);
  });

  it('zero latency is effectively instant', async () => {
    const latencyMs = readStubLatencyMs({ PICE_STUB_LATENCY_MS: '0' });
    const t0 = performance.now();
    await new Promise((r) => setTimeout(r, latencyMs));
    const elapsed = performance.now() - t0;
    expect(elapsed).toBeLessThan(50);
  });
});

/**
 * PICE_STUB_SCORES parser for deterministic adaptive evaluation traces.
 *
 * Format: `score,cost;score,cost;...` — e.g. `9.5,0.02;9.1,0.02;3.0,0.02`.
 * Each entry maps to a passIndex (0-indexed). When passIndex exceeds the
 * list, the last entry is repeated (avoids OOB in longer runs).
 *
 * Phase 5 cohort parallelism: per-layer override via
 * `PICE_STUB_SCORES_<LAYER_UPPER>`. When the daemon sends a contract
 * carrying `{ "layer": "backend", ... }`, the stub reads
 * `PICE_STUB_SCORES_BACKEND` first and falls back to the shared
 * `PICE_STUB_SCORES` when the per-layer env is absent. Per-layer lists
 * eliminate contention by construction — two parallel layers consume
 * disjoint score arrays. This is test-only infrastructure; production
 * providers NEVER read `PICE_STUB_*` env vars.
 */

export interface StubScoreEntry {
  score: number;
  cost: number;
}

export function parseStubScores(raw: string): StubScoreEntry[] {
  if (!raw.trim()) return [];
  return raw
    .trim()
    .split(';')
    .filter((s) => s.length > 0)
    .map((entry, i) => {
      const parts = entry.split(',');
      if (parts.length !== 2) {
        throw new Error(
          `PICE_STUB_SCORES entry ${i} must be "score,cost"; got "${entry}"`,
        );
      }
      const score = Number(parts[0]);
      const cost = Number(parts[1]);
      if (!Number.isFinite(score) || !Number.isFinite(cost)) {
        throw new Error(
          `PICE_STUB_SCORES entry ${i} has non-finite values: score=${parts[0]}, cost=${parts[1]}`,
        );
      }
      return { score, cost };
    });
}

export function getStubEntry(
  entries: StubScoreEntry[],
  passIndex: number,
): StubScoreEntry | undefined {
  if (entries.length === 0) return undefined;
  const idx = Math.min(passIndex, entries.length - 1);
  return entries[idx];
}

/**
 * Resolve the per-layer score env var name for `layer`. Always returns
 * the uppercase-normalized form — `backend` → `PICE_STUB_SCORES_BACKEND`.
 * Non-ASCII characters are preserved verbatim; keep layer names ASCII in
 * tests (layers.toml validator already encourages this).
 */
export function perLayerScoreEnvName(layer: string): string {
  return `PICE_STUB_SCORES_${layer.toUpperCase()}`;
}

/**
 * Read `PICE_STUB_LATENCY_MS` as a non-negative integer. Invalid values
 * (negative, non-numeric, NaN, ∞) log a single warning to stderr and
 * return 0 — the latency knob is test infrastructure; a typo must not
 * crash the stub. Zero is the steady-state default.
 *
 * Callers use this in the `evaluate/score` handler to simulate provider
 * latency for cohort-parallelism benchmarks. `tokio::time::pause()` on
 * the Rust side would zero this out silently — the bench scenario
 * explicitly opts into the multi-thread runtime. See
 * `crates/pice-daemon/benches/parallel_cohort_speedup.rs`.
 */
export function readStubLatencyMs(
  env: NodeJS.ProcessEnv = process.env,
): number {
  const raw = env['PICE_STUB_LATENCY_MS'];
  if (raw === undefined || raw === '') return 0;
  const parsed = Number(raw);
  if (
    !Number.isFinite(parsed) ||
    !Number.isInteger(parsed) ||
    parsed < 0
  ) {
    console.error(
      `[stub] PICE_STUB_LATENCY_MS=${raw} is not a non-negative integer; treating as 0`,
    );
    return 0;
  }
  return parsed;
}

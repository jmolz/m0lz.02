# Migration Guide: PICE v0.1 To v0.2 Stack Loops

v0.2 changes PICE from a single foreground loop into a daemon-backed, layer-aware workflow. The core Plan -> Implement -> Contract-Evaluate loop remains, but evaluation can now run per layer, in the background, with seam checks, review gates, and persisted manifests.

## What Changes On Disk

`pice init` creates:

- `.claude/` public workflow scaffold and command templates by default
- `.codex/` public workflow scaffold plus root `AGENTS.md` when run with `--developer codex`
- `.pice/config.toml` provider, evaluation, telemetry, and metrics config
- `.pice/workflow.yaml` Stack Loops defaults

`pice init --upgrade` adds:

- `.pice/layers.toml` detected layer configuration
- `.pice/contracts/*.toml` layer contract templates

`[provider].name` in `.pice/config.toml` selects the primary developer for workflow commands. `[evaluation.primary]` and `[evaluation.adversarial]` select evaluators and can be mixed independently.

## Upgrade Steps

```bash
pice init                    # default Claude Code scaffold
# or: pice init --developer codex
pice init --upgrade
pice layers detect --json
pice layers check --json
pice layers graph
pice validate --json
```

Review `.pice/layers.toml` before committing it. The detector is a starting point, not an authority.

## Daemon And Inline Mode

The `pice` CLI talks to `pice-daemon` over a Unix socket on macOS/Linux or a named pipe on Windows. The CLI auto-starts the daemon when socket mode is needed.

Use `PICE_DAEMON_INLINE=1` for deterministic local validation where background subscriptions are not needed. Inline mode intentionally rejects or degrades `--background`, `--wait`, and follow streams because no long-lived daemon owns the job.

Useful isolation variables:

- `PICE_DAEMON_SOCKET`: daemon socket or pipe override
- `PICE_STATE_DIR`: manifest state directory override
- `PICE_DAEMON_BIN`: absolute daemon binary path, used by npm installs and artifact smoke tests

## Layer Review

Check these fields in `.pice/layers.toml`:

- `paths`: source, config, deployment, and observability globs
- `depends_on`: transitive layer dependencies
- `always_run`: infrastructure, deployment, and observability should remain always-run unless your project records an explicit exception
- `type`: meta-layers such as infrastructure can influence other layer contracts

Run `pice layers check --json` after adding new files so unlayered files do not silently escape evaluation.

## Workflow And Contracts

`.pice/workflow.yaml` controls:

- default tier, confidence, max passes, and parallelism
- `phases.evaluate.parallel`
- adaptive algorithm settings
- review gate policy
- model overrides and budget behavior

`.pice/contracts/*.toml` controls layer-specific criteria. Keep infrastructure, deployment, and observability criteria concrete; these are the checks that single feature-level contracts usually miss.

## Background Jobs

Run:

```bash
pice evaluate <plan.md> --background --wait --timeout-secs 120 --json
pice status <feature-id> --json
pice status <feature-id> --follow --stream-json
pice logs <feature-id> --json
pice logs <feature-id> --follow --stream-json
```

Manifests are persisted under `~/.pice/state/{project-hash}/{feature-id}.manifest.json` unless `PICE_STATE_DIR` is set.

## Review Gates

Enable gates in `.pice/workflow.yaml`:

```yaml
review:
  enabled: true
  trigger: "tier >= 3 OR layer == infrastructure"
  timeout_hours: 24
  on_timeout: reject
  retry_on_reject: 1
```

Operate gates with:

```bash
pice review-gate --list --feature-id <feature-id> --json
pice review-gate --gate-id <gate-id> --decision approve --reason "reviewed" --json
```

Every decision is written to the local `gate_decisions` audit table.

## Provider Compatibility

v0.1 providers can continue in single-layer mode. v0.2 providers may receive
optional layer scoping fields on `session/create` (`layer`, `layerPaths`,
`contractPath`) and optional seam/adaptive fields on `evaluate/create`
(`seamChecks`, `passIndex`, `freshContext`, `effortOverride`). Providers that
do not use those fields should ignore them; the daemon owns manifests, review
gates, and built-in seam execution fallback. Providers that report real
per-pass spend must explicitly declare `costTelemetry: true`.

## Rollback

To temporarily return to single-loop behavior, remove or rename `.pice/layers.toml` and disable review gates in `.pice/workflow.yaml`. To stop daemon state for a test run, use an isolated `PICE_STATE_DIR` and `PICE_DAEMON_SOCKET`, then `pice daemon stop`.

Do not delete `.pice/metrics.db` unless you intentionally want to discard local audit and evaluation history.

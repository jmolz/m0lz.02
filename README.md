<p align="center">
  <img src="branch-mark.svg" width="48" height="48" alt="m0lz.02 branch mark for the PICE CLI">
</p>

<h1 align="center">m0lz.02</h1>

<p align="center">
  PICE CLI: a Rust daemon and CLI adapter for structured AI coding workflows.
</p>

<p align="center">
  <a href="https://github.com/jmolz/m0lz.02/actions/workflows/ci.yml"><img alt="CI status" src="https://img.shields.io/github/actions/workflow/status/jmolz/m0lz.02/ci.yml?branch=main&style=flat-square&label=ci&labelColor=404040&color=171717"></a>
  &nbsp;
  <a href="https://github.com/jmolz/m0lz.02/releases"><img alt="Latest GitHub release" src="https://img.shields.io/github/v/release/jmolz/m0lz.02?style=flat-square&label=release&labelColor=404040&color=171717"></a>
  &nbsp;
  <a href="https://www.npmjs.com/package/@jacobmolz/pice"><img alt="npm package version" src="https://img.shields.io/npm/v/%40jacobmolz%2Fpice?style=flat-square&label=npm&labelColor=404040&color=171717"></a>
  &nbsp;
  <a href="./package.json"><img alt="Node.js version" src="https://img.shields.io/badge/node-22%2B-171717?style=flat-square&labelColor=404040"></a>
  &nbsp;
  <a href="./Cargo.toml"><img alt="Rust stable" src="https://img.shields.io/badge/rust-stable-171717?style=flat-square&labelColor=404040"></a>
  &nbsp;
  <a href="./LICENSE"><img alt="License: MIT" src="https://img.shields.io/github/license/jmolz/m0lz.02?style=flat-square&labelColor=404040&color=171717"></a>
</p>

<p align="center">
  <img src="docs/images/pice-evaluate-demo.gif" alt="Animated terminal demo showing m0lz.02 install, initialization, Stack Loops layer detection, background evaluation, and a passing status result" width="760">
</p>

## What It Does

m0lz.02 implements PICE: Plan, Implement, Contract-Evaluate. The current release line includes Stack Loops, which split a feature across technology layers, evaluate each layer against its own contract, run seam checks at integration boundaries, and keep background evaluations observable through status, logs, review gates, and audit data.

The shipped architecture is a CLI adapter plus a headless `pice-daemon`. The CLI handles arguments and terminal rendering. The daemon owns orchestration, background jobs, provider sessions, manifests, metrics, templates, and audit state. AI providers run out of process over the provider JSON-RPC protocol.

## How Work Stays Tied To The Spec

`prime` orients on the repository and recent state; it does not tie implementation back to a spec. `plan` turns the original request, supplied spec, or stable reference into an approved plan, a `## Spec Traceability` mapping, and a JSON contract. `execute` starts a fresh provider session from the approved plan and refuses contract-free plans before provider startup. `evaluate` grades the produced diff against the contract with isolated evaluators that see only the contract, filtered diff, and `AGENTS.md`.

Stack Loops extend the chain with per-layer contracts, seam checks, manifest state, trace metadata for the approved plan and contract, and review gates when the workflow requires human approval.

## Install

```bash
npm install -g @jacobmolz/pice
```

The npm package installs a platform package containing both `pice` and `pice-daemon`. The wrapper resolves both binaries and passes the daemon path to the CLI so background mode does not require a manual `PATH` edit.

From source:

```bash
cargo install pice-cli
```

Prebuilt archives are published from GitHub Releases when a tag is approved.

## Quick Start

```bash
pice init
pice init --developer codex
pice init --upgrade
pice layers detect --json
pice layers check --json
pice validate --json
pice plan "add account settings"
pice execute <plan.md>
pice evaluate <plan.md> --background --wait --timeout-secs 120
pice status <feature-id> --json
pice logs <feature-id> --json
pice review-gate --list --feature-id <feature-id> --json
```

`pice init` scaffolds public workflow files under `.claude/` and project config under `.pice/` by default. Use `pice init --developer codex` to scaffold `.codex/`, create root `AGENTS.md`, and set Codex as the primary developer provider.

## Stack Loops

Stack Loops turn one feature into layer-specific loops:

1. Detect layers from source, infrastructure, database, deployment, and observability files.
2. Cascade dependencies so upstream changes activate downstream checks.
3. Always run infrastructure, deployment, and observability layers unless the project explicitly overrides that policy.
4. Evaluate independent DAG cohorts concurrently when `phases.evaluate.parallel` is enabled.
5. Halt adaptively when confidence, budget, gate, cancellation, or max-pass rules decide the result.
6. Persist the manifest to `~/.pice/state/{project-hash}/{feature-id}.manifest.json`.

See [the Stack Loops guide](docs/guides/stack-loops.md) and [the v0.1 to v0.2 migration guide](docs/guides/migration-v01-to-v02.md).

## Commands

| Command | Purpose |
| --- | --- |
| `pice init` | Scaffold `.claude/` or `.codex/` developer files and `.pice/` config |
| `pice prime` | Orient on the current project |
| `pice plan <description>` | Create a plan and contract |
| `pice execute <plan>` | Implement from a plan in a fresh provider session |
| `pice evaluate <plan>` | Evaluate a plan contract, including background mode |
| `pice review` | Run code review and regression checks |
| `pice commit` | Create a standardized commit |
| `pice handoff` | Capture session state |
| `pice status [feature-id]` | Inspect manifests; `--follow --stream-json` tails live updates |
| `pice logs <feature-id>` | Inspect captured provider logs; `--follow --stream-json` tails live chunks |
| `pice metrics` | Aggregate local quality metrics |
| `pice benchmark` | Compare workflow effectiveness |
| `pice layers detect/list/check/graph` | Manage layer configuration |
| `pice validate` | Validate `.pice/` workflow, layer, and contract config |
| `pice daemon start/status/stop/restart/logs` | Manage the headless daemon |
| `pice audit` | Export gate and audit data |
| `pice review-gate` | List or decide pending human review gates |
| `pice completions <shell>` | Generate shell completions |

Every command that returns structured data supports `--json`; follow modes use newline-delimited `--stream-json` frames.

## Architecture

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/architecture-dark.svg">
  <img alt="m0lz.02 architecture showing CLI adapter, daemon, metrics, templates, provider host, and external providers connected by JSON-RPC" src="docs/images/architecture-light.svg" width="800">
</picture>

There are two JSON-RPC boundaries:

- CLI to daemon: socket transport for commands, background jobs, subscriptions, and daemon lifecycle.
- Daemon to provider: stdio transport for workflow and evaluation providers.

Provider failures are allowed to degrade evaluation, but they must not crash the CLI. Provider stdout is reserved for JSON-RPC; provider logs go to stderr.

## Evaluation

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/evaluation-tiers-dark.svg">
  <img alt="Evaluation tiers showing single-model contract grading, dual-model adversarial review, and higher-tier agent-team review" src="docs/images/evaluation-tiers-light.svg" width="800">
</picture>

Evaluators are context-isolated. A layer evaluator sees its layer contract, filtered diff, and evaluation guidance text carried on the compatibility `claudeMd` wire field. PICE reads `AGENTS.md` for evaluation guidance and excludes alternate workflow-guidance files from evaluator prompts. Evaluators do not receive implementation chat, plan rationale, sibling layer contracts, or unrelated findings.

Tier 2 runs primary contract grading plus adversarial review. Tier 3 adds agent-team evaluation. Adaptive evaluation respects the correlated-evaluator confidence ceiling documented in [convergence analysis](docs/research/convergence-analysis.md).

## Configuration

Project config lives in `.pice/config.toml`; Stack Loops behavior lives in `.pice/workflow.yaml` and `.pice/layers.toml`.

`[provider].name` selects the primary developer for workflow commands such as `prime`, `plan`, `execute`, `review`, `commit`, and `handoff`. `[evaluation.primary]` and `[evaluation.adversarial]` select the evaluators used by `pice evaluate`; they are independent of the workflow provider.

Claude-primary complete config:

```toml
[provider]
name = "claude-code"

[evaluation.primary]
provider = "claude-code"
model = "claude-opus-4-6"

[evaluation.adversarial]
provider = "codex"
model = "gpt-5.5"
effort = "xhigh"
enabled = true

[evaluation.tiers]
tier1_models = ["claude-opus-4-6"]
tier2_models = ["claude-opus-4-6", "gpt-5.5"]
tier3_models = ["claude-opus-4-6", "gpt-5.5"]
tier3_agent_team = true

[telemetry]
enabled = false
endpoint = "https://telemetry.pice.dev/v1/events"

[metrics]
db_path = ".pice/metrics.db"

[init]
project_type = "auto"
```

Codex-primary with dual-model evaluation complete config:

```toml
[provider]
name = "codex"

[evaluation.primary]
provider = "claude-code"
model = "claude-opus-4-6"

[evaluation.adversarial]
provider = "codex"
model = "gpt-5.5"
effort = "xhigh"
enabled = true

[evaluation.tiers]
tier1_models = ["claude-opus-4-6"]
tier2_models = ["claude-opus-4-6", "gpt-5.5"]
tier3_models = ["claude-opus-4-6", "gpt-5.5"]
tier3_agent_team = true

[telemetry]
enabled = false
endpoint = "https://telemetry.pice.dev/v1/events"

[metrics]
db_path = ".pice/metrics.db"

[init]
project_type = "auto"
```

Codex-primary workflow with Codex adversarial evaluation disabled complete config:

```toml
[provider]
name = "codex"

[evaluation.primary]
provider = "claude-code"
model = "claude-opus-4-6"

[evaluation.adversarial]
provider = "codex"
model = "gpt-5.5"
effort = "xhigh"
enabled = false

[evaluation.tiers]
tier1_models = ["claude-opus-4-6"]
tier2_models = ["claude-opus-4-6", "gpt-5.5"]
tier3_models = ["claude-opus-4-6", "gpt-5.5"]
tier3_agent_team = true

[telemetry]
enabled = false
endpoint = "https://telemetry.pice.dev/v1/events"

[metrics]
db_path = ".pice/metrics.db"

[init]
project_type = "auto"
```

Required environment variables depend on the providers you enable:

| Variable | Used by |
| --- | --- |
| `ANTHROPIC_API_KEY` | Claude Code workflow and evaluation sessions through the Claude Agent SDK |
| `OPENAI_API_KEY` | Codex adversarial evaluation through the OpenAI SDK |

Codex workflow sessions use the installed Codex CLI through `codex exec`. Run `codex login` first, or otherwise configure the auth method supported by your Codex CLI. This is separate from `OPENAI_API_KEY`, which is only used by the OpenAI SDK-backed adversarial evaluator.

For Codex-primary projects, the scaffold includes `.codex/commands/self-heal.md`. Run self-heal manually after a feature worktree has been merged into `main` to capture durable lessons into rules, docs, commands, and tripwires; it is not run automatically by execute, evaluate, or merge.

## Metrics And Telemetry

Metrics are local SQLite data in `.pice/metrics.db`. The current metrics schema records evaluation rows, pass events with cost fields, seam findings, layer runs, and gate decisions. The release inventory script writes fresh schema evidence to `docs/releases/metrics-schema-evidence.json`; the Phase 8 reference harness writes runtime row-count evidence to `docs/releases/phase8-reference-evidence.json`.

Telemetry is opt-in and disabled by default. Public telemetry claims are limited to aggregate workflow events; local metrics can include project-specific identifiers, but outbound telemetry must not send code, prompts, file paths, secrets, or PII.

## Release Evidence

Reference release evidence for the v0.8.9 validation cycle was verified on May 15, 2026 from commit `bb3d364`. Release workflow artifacts are generated for every later tag; refresh this table whenever release validation materially changes. Historical v0.7.0 evidence is recorded in [docs/releases/v0.7.0.md](docs/releases/v0.7.0.md).

Recorded release evidence:

| Check | Result |
| --- | --- |
| Local Linux Docker preflight | `scripts/ci/local-linux.sh` is the required Linux CI parity gate before every deployment push/tag; it runs Rust, TypeScript, Phase 8 acceptance, release-smoke, npm pack smoke, and README media gates in Docker |
| Rust lint/tests/build | `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --all-targets`, and `cargo build --release` passed in local Docker, main CI, and release validation |
| TypeScript lint/typecheck/tests/build | `pnpm lint`, `pnpm typecheck`, `pnpm test`, and `pnpm build` passed; current local `pnpm test` passed 128 tests |
| Phase 8 acceptance | Metrics inventory, five-reference-project harness, release artifact smoke, npm pack smoke, and README media audit passed |
| Hosted Windows pre-tag smoke | GitHub Actions run `25937214270` passed on `main` at commit `bb3d364` before the `v0.8.9` release tag; `scripts/ci/windows-smoke.ps1` ran native Windows build/test/release-smoke coverage |
| Windows validation | `Rust (windows-latest)` passed in main CI; `Smoke x86_64-pc-windows-msvc` passed in the release workflow |
| Remote CI | GitHub Actions run `25937187663` passed on `main`; includes `Phase 8 acceptance (linux-x64)`, `Rust (windows-latest)`, TypeScript, and Rust release-build coverage |
| Release workflow | GitHub Actions run `25940205779` passed for tag `v0.8.9` |
| NPM publish | `@jacobmolz/pice@0.8.9` and platform packages published from the release workflow; `npm view @jacobmolz/pice version` returned `0.8.9` |
| GitHub release | [`v0.8.9`](https://github.com/jmolz/m0lz.02/releases/tag/v0.8.9) published with five platform archives and shell completions |

For a Linux CI-equivalent local preflight, run:

```bash
scripts/ci/local-linux.sh
```

For release, CI, command/template, validation, test-policy, CLI-runtime, or follow-up Windows-failure changes, do not substitute one platform gate for the other. Use the Docker preflight above for Linux CI parity before pushing, then verify the exact pushed `main` commit on the hosted Windows runner before tagging:

```bash
gh workflow run windows-smoke.yml --ref main
gh run watch <windows-smoke-run-id> --exit-status
```

The Linux Docker gate is the authoritative local parity check for the Linux CI environment, including scheduler-sensitive performance assertions. The hosted Windows runner is the authoritative pre-tag check for Windows CLI behavior such as path normalization, `.cmd` execution, PowerShell, archive smoke, and daemon named-pipe behavior.

The Phase 8 acceptance suite inside that preflight is:

```bash
node scripts/acceptance/metrics-schema-inventory.mjs
node scripts/acceptance/phase8-reference-projects.mjs
tar -czf /private/tmp/pice-release-smoke-local.tar.gz -C target/release pice pice-daemon
PICE_ARTIFACT_ARCHIVE=/private/tmp/pice-release-smoke-local.tar.gz PICE_NPM_PACK_SMOKE=1 node scripts/acceptance/release-artifact-smoke.mjs
node scripts/acceptance/readme-media-audit.mjs
```

## Provider Development

Providers declare `workflow`, `evaluation`, and optional telemetry capabilities during `initialize`. Protocol changes must update both Rust and TypeScript types and add roundtrip tests on both sides.

Read [building a provider](docs/providers/building-a-provider.md) and [the provider protocol](docs/providers/protocol.md).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Full validation includes Rust format, clippy, tests, TypeScript lint/typecheck/tests/build, release build, Phase 8 acceptance harnesses, artifact smoke, the local Linux Docker preflight for CI or release changes, and hosted Windows Smoke before release tags when the change can affect Windows CLI/runtime behavior or follows a Windows CI failure.

## Writing

- [m0lz-02-stack-loops](https://m0lz.dev/writing/m0lz-02-stack-loops)

# PICE CLI

Structured AI coding workflow orchestrator -- Plan, Implement, Contract-Evaluate.

[![CI](https://github.com/jmolz/pice-framework/actions/workflows/ci.yml/badge.svg)](https://github.com/jmolz/pice-framework/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

## What is PICE?

PICE is a methodology for structured AI coding that breaks work into three formal phases: **Plan** (research, design, and contract negotiation), **Implement** (code generation from a plan), and **Contract-Evaluate** (adversarial grading of the implementation against the contract). The CLI orchestrates this lifecycle -- it manages the state, the prompts, and the measurement while an AI assistant does the actual coding.

The key differentiator is **dual-model adversarial evaluation**. Instead of asking the same AI that wrote the code to judge it, PICE runs parallel evaluations from independent models -- Claude grades contract criteria while GPT-5.4 challenges the approach as an adversary. This eliminates the single-model blind spots that plague self-review workflows.

PICE CLI is the outer loop. It spawns AI providers over a JSON-RPC protocol, feeds them scoped context, captures structured output, and stores quality metrics locally in SQLite. The AI does the coding; PICE makes sure it is doing it well.

## Installation

### npm (recommended)

```bash
npm install -g pice
```

### Cargo

```bash
cargo install pice-cli
```

### GitHub Releases

Download a prebuilt binary for your platform from [GitHub Releases](https://github.com/jacobmolz/pice/releases), extract it, and place it on your `PATH`.

## Quick Start

```bash
# Scaffold PICE framework files in your project
pice init

# Orient on the codebase and get recommended next actions
pice prime

# Research, plan, and generate a contract for a feature
pice plan "add user auth"

# Implement the plan in a fresh AI session
pice execute .claude/plans/auth-plan.md

# Run dual-model adversarial evaluation against the contract
pice evaluate .claude/plans/auth-plan.md

# Code review with regression checks
pice review

# Create a standardized git commit
pice commit
```

## Commands

| Command | Description |
|---------|-------------|
| `pice init` | Scaffold `.claude/` and `.pice/` directories with framework files |
| `pice prime` | Orient on the codebase and get recommended next actions |
| `pice plan <description>` | Research, plan, and generate a contract for a feature or change |
| `pice execute <plan>` | Implement from a plan file in a fresh AI session |
| `pice evaluate <plan>` | Run adversarial evaluation against a plan's contract |
| `pice review` | Code review and regression suite |
| `pice commit` | Create a standardized git commit |
| `pice handoff` | Capture session state for the next session or agent |
| `pice status` | Display active plans and workflow state |
| `pice metrics` | Aggregate and display quality metrics |
| `pice benchmark` | Before/after workflow effectiveness comparison |
| `pice completions <shell>` | Generate shell completions (bash, zsh, fish, powershell) |

All commands support `--json` for machine-readable output.

## Architecture

PICE CLI uses a **provider architecture** that separates the Rust core from AI provider implementations:

```
pice (Rust binary)
  Core engine --------- state machine, lifecycle, config
  Metrics engine ------- SQLite storage + telemetry
  Template engine ------ scaffolding, file generation
  Provider host -------- spawns and manages provider processes
       |  JSON-RPC over stdio
  Providers (TypeScript) -- Claude Code, Codex, community providers
```

The Rust core handles argument parsing, state management, configuration, metrics, and process orchestration. AI providers are separate TypeScript processes that communicate over JSON-RPC on stdio. This design allows community-built providers for any AI coding tool without modifying the core binary.

For provider development, see [`docs/providers/`](docs/providers/).

## Dual-Model Adversarial Evaluation

Evaluation scales with the significance of the change:

| Tier | Scope | Models | Behavior |
|------|-------|--------|----------|
| Tier 1 | Minor changes | Claude Opus | Single evaluator, contract grading only |
| Tier 2 | New features | Claude Opus + GPT-5.4 | Parallel evaluation with adversarial review |
| Tier 3 | Architectural | Claude Opus team (4) + GPT-5.4 | Agent team evaluation + high-effort adversarial review |

Evaluators are **context-isolated** -- they see only the contract JSON, the git diff, and the project's `CLAUDE.md`. They never see the implementation conversation or planning rationale.

## Configuration

PICE stores project configuration in `.pice/config.toml`, created by `pice init`:

```toml
[provider]
name = "claude-code"

[evaluation.primary]
provider = "claude-code"
model = "claude-opus-4-6"

[evaluation.adversarial]
provider = "codex"
model = "gpt-5.4"
effort = "high"
enabled = true

[telemetry]
enabled = false

[metrics]
db_path = ".pice/metrics.db"
```

Key settings:
- **`provider.name`** -- The AI provider for workflow commands (plan, execute, review, commit).
- **`evaluation.primary`** -- Model for contract grading.
- **`evaluation.adversarial`** -- Model for adversarial review. Set `enabled = false` to use single-model evaluation only.
- **`telemetry.enabled`** -- Opt-in anonymous telemetry (see below).

### Environment Variables

| Variable | Required for |
|----------|-------------|
| `ANTHROPIC_API_KEY` | Claude Code provider (workflow + evaluation) |
| `OPENAI_API_KEY` | Codex provider (adversarial evaluation) |

## Shell Completions

Generate completions for your shell and add them to your profile:

**Bash:**
```bash
pice completions bash > ~/.local/share/bash-completion/completions/pice
```

**Zsh:**
```bash
pice completions zsh > ~/.zfunc/_pice
# Ensure ~/.zfunc is in your fpath before compinit
```

**Fish:**
```bash
pice completions fish > ~/.config/fish/completions/pice.fish
```

## Telemetry

Telemetry is **opt-in** and **off by default**. When enabled, PICE collects anonymous usage metrics (command frequency, evaluation pass rates, workflow timing) to improve the tool. No code, prompts, or personally identifiable information is collected.

Telemetry data is fully inspectable in `.pice/telemetry-log.jsonl` before any data leaves your machine. To enable:

```toml
# .pice/config.toml
[telemetry]
enabled = true
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, coding standards, and contribution guidelines.

### Development

```bash
# Rust
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt --check

# TypeScript
pnpm install
pnpm build
pnpm test
pnpm lint
pnpm typecheck
```

## License

MIT -- see [LICENSE](LICENSE) for details.

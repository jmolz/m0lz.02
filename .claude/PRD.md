# Product Requirements Document: PICE CLI

## Executive Summary

PICE CLI is an open-source workflow orchestrator for structured AI coding. It implements the Plan-Implement-Contract-Evaluate (PICE) methodology as a Rust-powered command-line tool that drives AI coding sessions through a formal lifecycle, collects quality metrics, and proves the methodology works with data.

The tool is the outer loop. The AI coding assistant (Claude Code first, others later via a provider architecture) is the engine. PICE CLI manages the lifecycle, state, and measurement — the assistant does the coding.

The repo serves dual purpose: it documents the PICE methodology (readable on GitHub without installing anything) and ships a CLI that productizes it. The architecture uses a JSON-RPC provider protocol inspired by MCP, enabling community-built providers for any AI coding tool.

**MVP Goal:** Ship an 11-command CLI that orchestrates the complete PICE workflow via Claude Code, scaffolds projects, and collects quality metrics with opt-in anonymous telemetry — proving that structured AI coding workflows produce measurably better code.

---

## Target Users

### Primary Persona: AI-Assisted Developer (Claude Code User)

- **Who:** Software developer using Claude Code for daily coding tasks
- **Technical Level:** Developer — comfortable with CLI tools, git workflows, and AI coding assistants
- **Key Need:** A structured workflow that turns ad-hoc AI coding ("vibing") into a repeatable, measurable process
- **Pain Point:** No way to enforce discipline in AI coding sessions. No metrics proving whether structured workflows actually improve output quality. No easy way to set up the PICE methodology in new projects.

### Secondary Persona: AI Coding Tool User (Future)

- **Who:** Developer using Cursor, Copilot, Windsurf, or other AI coding assistants
- **Technical Level:** Developer
- **Key Need:** Same structured workflow, different AI engine
- **Pain Point:** Same as primary, but also: no cross-tool workflow standard exists

### Tertiary Persona: Open-Source Contributor

- **Who:** Developer interested in AI coding methodology or Rust/TypeScript tooling
- **Technical Level:** Developer
- **Key Need:** Clear contribution boundaries — Rust devs work on the core, TS devs build providers
- **Pain Point:** Most AI coding tools are closed-source. No open-source project is measuring AI coding workflow effectiveness at scale.

---

## MVP Scope

### In Scope

**Core Functionality**
- [ ] `pice init` — scaffold `.claude/` directory with all PICE framework files (commands, templates, hooks, docs, rules)
- [ ] `pice prime` — orchestrate codebase orientation session via Claude Code SDK
- [ ] `pice plan <description>` — orchestrate planning + contract negotiation session
- [ ] `pice execute <plan-path>` — orchestrate implementation from a plan file
- [ ] `pice evaluate <plan-path>` — run dual-model adversarial evaluation against the plan's contract (configurable evaluator models)
- [ ] `pice review` — orchestrate code review + regression suite
- [ ] `pice commit` — orchestrate standardized git commit
- [ ] `pice handoff` — orchestrate session state capture
- [ ] `pice status` — display active plans, contract status, evaluation results
- [ ] `pice metrics` — aggregate and display quality data across PICE loops
- [ ] `pice benchmark` — before/after workflow effectiveness comparison with git stats, test coverage deltas, and CI data

**Technical**
- [ ] Rust core binary (CLI parsing, state machine, config engine, metrics store, template engine, provider host)
- [ ] JSON-RPC over stdio provider protocol (formal, documented contract between core and providers)
- [ ] `@pice/provider-claude-code` — TypeScript provider using `@anthropic-ai/claude-code` SDK
- [ ] `@pice/provider-codex` — TypeScript provider for OpenAI Codex/GPT models (adversarial evaluator)
- [ ] `@pice/provider-protocol` — TypeScript types for the provider contract
- [ ] `@pice/provider-base` — shared utilities for building providers
- [ ] Multi-model evaluation engine — orchestrates parallel evaluator sessions across different providers/models
- [ ] SQLite-based local metrics store
- [ ] Opt-in anonymous telemetry client
- [ ] Shell completions (bash, zsh, fish)

**Distribution**
- [ ] Pre-built binaries via GitHub Releases (macOS arm64/x64, Linux arm64/x64, Windows x64)
- [ ] `npm install -g pice` — NPM wrapper that downloads platform-specific binary
- [ ] `cargo install pice` — compile from source
- [ ] CI/CD pipeline for automated builds and releases

**Documentation (in-repo)**
- [ ] PICE methodology docs (readable on GitHub without installing)
- [ ] Playbook, brownfield guide, agent teams playbook
- [ ] Provider development guide
- [ ] Contributing guide with clear Rust/TS contribution boundaries

### Out of Scope

- Providers for other AI coding workflow tools (Cursor, Copilot, Windsurf) — architecture supports them, MVP doesn't ship them. Note: this is distinct from evaluator providers — the MVP ships both Claude Code (workflow + evaluation) and Codex (evaluation only)
- Web dashboard for telemetry visualization — anonymous data collected, dashboard is post-MVP
- GUI or TUI interface — CLI only for MVP
- IDE extensions (VS Code, JetBrains) — post-MVP
- Hosted/cloud version — local CLI only
- `pice upgrade` or self-update mechanism — users use their package manager

---

## User Stories

1. As a developer starting a new project, I want to run `pice init` so that my project is immediately set up with the PICE workflow (commands, templates, hooks, rules).
2. As a developer starting a coding session, I want to run `pice prime` so that an AI agent orients on my codebase and gives me a summary of current state and recommended next actions.
3. As a developer adding a feature, I want to run `pice plan "add user auth"` so that an AI agent researches my codebase, creates a detailed plan with a contract, and presents it for my approval.
4. As a developer with an approved plan, I want to run `pice execute auth-plan.md` so that an AI agent implements the plan in a fresh session with full context from the plan file.
5. As a developer who just finished implementation, I want to run `pice evaluate auth-plan.md` so that multiple adversarial AI agents from different model families grade my implementation against the contract criteria in parallel — eliminating single-model blind spots.
11. As a developer, I want to configure which models run evaluation (e.g., Claude Opus for contract grading, GPT-5.5 xhigh for design challenge) so that I can choose the evaluation rigor and cost trade-off that fits my needs.
6. As a developer who wants to ship, I want to run `pice review` so that code review and regression tests run before I commit.
7. As a developer, I want to run `pice metrics` so that I can see aggregate quality scores, pass rates, and trends across all my PICE loops — proving the methodology works.
8. As an open-source contributor, I want to build a provider for my preferred AI tool by implementing the JSON-RPC protocol, without needing to understand or modify the Rust core.
9. As a team lead, I want to run `pice benchmark` so that I can show before/after data comparing structured vs. unstructured AI coding workflows.
10. As a developer between sessions, I want to run `pice status` to see my active plans, which have been evaluated, and what needs attention.

---

## Tech Stack

| Technology | Purpose | Version |
|------------|---------|---------|
| Rust | Core CLI binary (arg parsing, state machine, config, metrics, templates, provider host) | Latest stable |
| TypeScript | Provider implementations, SDK bridge | 5.x |
| Node.js | Runtime for TypeScript providers | 22+ LTS |
| clap | Rust CLI framework (arg parsing, help, completions) | 4.x |
| SQLite (rusqlite) | Local metrics storage | 3.x |
| serde / serde_json | Rust serialization for JSON-RPC protocol and config | 1.x |
| tokio | Async runtime for provider process management | 1.x |
| @anthropic-ai/claude-code | Claude Code SDK for workflow orchestration + Claude evaluator | Latest |
| OpenAI SDK / Codex CLI | Adversarial evaluator provider (GPT-5.5 / user-configurable) | Latest |
| npm | Distribution channel (binary wrapper pattern) | 10+ |
| GitHub Actions | CI/CD for cross-platform builds and releases | N/A |

---

## Architecture

### Directory Structure (Monorepo)

```
pice/
├── docs/                          # PICE methodology (readable on GitHub)
│   ├── methodology/               # Core PICE concepts
│   │   ├── overview.md
│   │   ├── plan.md
│   │   ├── implement.md
│   │   ├── contract.md
│   │   └── evaluate.md
│   ├── guides/                    # Practical guides
│   │   ├── playbook.md
│   │   ├── brownfield.md
│   │   ├── greenfield.md
│   │   ├── agent-teams.md
│   │   └── wisc-context.md
│   └── providers/                 # Provider development docs
│       ├── protocol.md
│       └── building-a-provider.md
├── crates/                        # Rust packages
│   ├── pice-cli/                  # Main binary
│   │   └── src/
│   │       ├── main.rs
│   │       ├── commands/          # One module per command
│   │       ├── engine/            # State machine, lifecycle
│   │       ├── config/            # .pice/ and .claude/ management
│   │       ├── metrics/           # SQLite store, aggregation, telemetry
│   │       ├── templates/         # Scaffolding, file generation
│   │       └── provider/          # Provider host, JSON-RPC, process mgmt
│   └── pice-protocol/             # Shared protocol types (Rust side)
│       └── src/
│           └── lib.rs             # JSON-RPC message types, provider trait
├── packages/                      # TypeScript packages
│   ├── provider-protocol/         # JSON-RPC types (TS side, published)
│   │   └── src/
│   │       └── index.ts
│   ├── provider-base/             # Shared provider utilities
│   │   └── src/
│   │       └── index.ts
│   ├── provider-claude-code/      # Claude Code SDK provider (workflow + evaluation)
│   │   └── src/
│   │       ├── index.ts           # Provider entry point
│   │       ├── session.ts         # SDK session management
│   │       └── handlers/          # JSON-RPC method handlers
│   └── provider-codex/            # Codex/OpenAI provider (evaluation only)
│       └── src/
│           ├── index.ts           # Provider entry point
│           ├── evaluator.ts       # Adversarial evaluation logic
│           └── handlers/          # JSON-RPC method handlers
├── templates/                     # Files that `pice init` scaffolds
│   ├── claude/                    # .claude/ directory template
│   │   ├── commands/
│   │   ├── templates/
│   │   ├── docs/
│   │   ├── hooks/
│   │   └── rules/
│   └── pice/                      # .pice/ directory template
│       └── config.toml
├── npm/                           # NPM distribution packages
│   ├── pice/                      # Main package (binary resolver)
│   ├── pice-darwin-arm64/         # Platform-specific binary packages
│   ├── pice-darwin-x64/
│   ├── pice-linux-arm64/
│   ├── pice-linux-x64/
│   └── pice-win32-x64/
├── .github/
│   └── workflows/
│       ├── ci.yml                 # Lint, test, build on every PR
│       └── release.yml            # Cross-compile + publish on tag
├── Cargo.toml                     # Rust workspace
├── package.json                   # TS workspace (pnpm)
├── pnpm-workspace.yaml
├── LICENSE                        # MIT
├── README.md
└── CLAUDE.md
```

### Data Flow

#### Workflow Commands (plan, execute, prime, etc.)

```
User runs `pice plan "add auth"`
        │
        ▼
┌─────────────────┐
│  Rust Core       │
│  (pice-cli)      │
│                  │
│  1. Parse args   │
│  2. Load config  │
│  3. Load plan    │
│     template     │
│  4. Start        │
│     provider     │
│     process      │
└────────┬────────┘
         │ JSON-RPC over stdio
         ▼
┌─────────────────┐
│  Provider        │
│  (claude-code)   │
│                  │
│  1. Create SDK   │
│     session      │
│  2. Send plan    │
│     prompt       │
│  3. Stream       │
│     responses    │
│  4. Return       │
│     structured   │
│     results      │
└────────┬────────┘
         │ JSON-RPC response
         ▼
┌─────────────────┐
│  Rust Core       │
│                  │
│  5. Parse result │
│  6. Write plan   │
│     file         │
│  7. Store metrics│
│  8. Display to   │
│     user         │
└─────────────────┘
```

#### Evaluation Command (dual-model, Tier 2 shown)

```
User runs `pice evaluate plan.md`
        │
        ▼
┌──────────────────────┐
│  Rust Core            │
│  1. Parse contract    │
│  2. Determine tier    │
│  3. Gather context    │
│     (diff, CLAUDE.md) │
│  4. Launch providers  │
│     IN PARALLEL       │
└───┬──────────┬───────┘
    │          │
    ▼          ▼
┌────────┐  ┌────────────┐
│Claude  │  │ Codex      │
│Provider│  │ Provider   │
│        │  │            │
│Contract│  │ Design     │
│grading │  │ challenge  │
│(scores │  │(approach   │
│per     │  │ critique,  │
│criteria│  │ assumptions│
│1-10)   │  │ tradeoffs) │
└───┬────┘  └─────┬──────┘
    │             │
    ▼             ▼
┌──────────────────────┐
│  Rust Core            │
│  5. Collect results   │
│  6. Synthesize report │
│  7. Store metrics     │
│  8. Display unified   │
│     evaluation report │
│                       │
│  Contract: 8/10 PASS  │
│  Design:  2 Consider  │
│  Overall: PASS        │
└──────────────────────┘
```

### Key Design Decisions

- **JSON-RPC over stdio for provider protocol** — Same IPC pattern as MCP servers. Familiar to the AI tooling community. Language-agnostic. Providers are independently testable processes.
- **Dual-model adversarial evaluation as a first-class concept** — Different model families have different blind spots. Claude evaluates contract criteria formally (structured grading). A second model (GPT-5.5 by default) challenges the approach itself — design tradeoffs, assumptions, failure modes. The provider protocol distinguishes between `workflow` and `evaluation` capabilities so providers can declare what they support. The Rust core orchestrates both in parallel.
- **User-configurable evaluation models** — The evaluation config is not hardcoded. Users choose which models run each evaluator role. This future-proofs against model deprecation and lets users optimize for cost vs. rigor. The tiered system (1/2/3) provides sensible defaults while allowing full override.
- **Graceful degradation for evaluation** — If only one provider is configured (e.g., no OpenAI key), evaluation falls back to single-model mode with a clear warning. The tool never fails because an optional evaluator is missing.
- **SQLite for metrics** — Zero-config, file-based, embedded in the Rust binary. No external database dependency. Portable across platforms. Sufficient for local metrics aggregation.
- **Monorepo with Cargo + pnpm workspaces** — Single clone for contributors. Rust workspace for crates, pnpm workspace for TS packages. CI tests both in one pipeline.
- **NPM binary distribution pattern** — Platform-specific packages contain pre-built binaries. The main `pice` package resolves the right one. Same pattern as Biome, Turbo, esbuild.
- **Templates embedded in binary** — `pice init` scaffolding files are compiled into the Rust binary at build time (via `include_str!` or `rust-embed`). No runtime dependency on the repo.

---

## Core Features

### Feature 1: Project Scaffolding (`pice init`)

Initializes a project with the complete PICE framework. Creates `.claude/` directory with all commands, templates, hooks, docs, and rules. Creates `.pice/` directory for local config and metrics. Detects existing `.claude/` and offers to merge rather than overwrite. Detects project type (language, framework) and customizes templates accordingly.

**Key behaviors:**
- Idempotent — safe to run multiple times
- Detects existing PICE setup and offers upgrade path
- Creates `.pice/config.toml` with default settings (provider, telemetry opt-in, etc.)
- Initializes SQLite metrics database at `.pice/metrics.db`

### Feature 2: Workflow Orchestration (`pice plan`, `pice execute`, `pice evaluate`, `pice prime`, `pice review`, `pice commit`, `pice handoff`)

Each command orchestrates a Claude Code session via the provider protocol. The Rust core:
1. Assembles the prompt/instruction set (from the corresponding command template)
2. Launches the provider process
3. Sends JSON-RPC requests to create and manage the AI session
4. Streams responses to the user's terminal
5. Captures structured results (plan files, evaluation scores, etc.)
6. Stores metrics data

**Session lifecycle:** The core manages context resets between phases. `pice plan` runs in one session, `pice execute` in a fresh session (critical for the PICE methodology — execution should not be biased by planning context).

**Contract negotiation in `pice plan`:** The plan command includes an interactive phase where the user negotiates the evaluation contract. The provider streams the AI's proposed contract, the user can adjust criteria and thresholds, and the final contract is saved in the plan file.

**Dual-model adversarial evaluation in `pice evaluate`:** The evaluate command implements the methodology's tiered evaluation system:

- **Tier 1** (bug fixes, simple changes): Single Claude evaluator session — grades contract criteria formally.
- **Tier 2** (new features, integrations): Claude evaluator + parallel adversarial review from a second model (default: GPT-5.5 via Codex provider). The second model challenges the *approach* — design tradeoffs, assumptions, failure modes. Different model families have different blind spots.
- **Tier 3** (architectural changes): Claude agent team (contract evaluator + convention auditor + regression hunter + edge case breaker) + parallel adversarial review from a second model at maximum reasoning depth.

All evaluators see ONLY the contract, code diff, and CLAUDE.md — never the planning or implementation context. This eliminates self-evaluation bias. The Rust core orchestrates multiple provider sessions in parallel and synthesizes results into a unified report.

**User-configurable models:** Users select which models to use for each evaluator role via `.pice/config.toml`. The provider architecture supports any model through any provider — Claude via the Claude Code provider, GPT via the Codex provider, or any future model via community providers. The default configuration ships with Claude + GPT-5.5 for dual-model coverage.

### Feature 3: Workflow Status (`pice status`)

Displays the current state of PICE workflows in the project:
- Active plans (path, creation date, contract tier, status)
- Evaluation results (pass/fail, scores per criterion)
- Pending actions (plans awaiting execution, implementations awaiting evaluation)
- Recent activity timeline

Data sourced from `.pice/metrics.db` and filesystem scan of `.claude/plans/`.

### Feature 4: Quality Metrics (`pice metrics`)

Aggregates quality data across all PICE loops in the project:
- Total loops completed
- Average evaluation score
- Contract pass rate (first-pass vs. after fixes)
- Score trends over time
- Most common failure criteria
- Evaluation tier distribution

Reads from `.pice/metrics.db`. Output formats: terminal table (default), JSON (`--json`), CSV (`--csv`).

### Feature 5: Benchmarking (`pice benchmark`)

Before/after comparison proving structured workflow effectiveness:
- **Git integration:** Commit frequency, revert rate, average PR size, time between commits
- **Test coverage:** Coverage deltas correlated with PICE loop usage
- **Evaluation scores:** Quality trend analysis across project lifetime
- **CI integration (optional):** Build success rate, deployment frequency
- Comparative analysis between PICE-managed features and non-PICE features in the same repo

Outputs a report with visualizations (terminal charts via ASCII) and exportable data.

### Feature 6: Telemetry (`pice metrics --telemetry`)

Opt-in anonymous aggregate data collection:
- Users explicitly opt in during `pice init` or via `pice config set telemetry true`
- Data is anonymized — no code, no file paths, no project names
- Collects: evaluation scores, pass rates, tier distribution, loop completion rates, provider type
- Feeds a future public dashboard showing PICE methodology effectiveness at scale
- Full transparency: telemetry payload is logged locally and viewable via `pice telemetry show`

---

## API Specification

### Provider Protocol (JSON-RPC over stdio)

The provider protocol is the contract between the Rust core and any provider implementation.

#### Core Methods (Core → Provider)

| Method | Description |
|--------|-------------|
| `initialize` | Set up provider with config (API keys, model preferences) |
| `session/create` | Create a new AI coding session |
| `session/send` | Send a message/prompt to the active session |
| `session/stream` | Stream responses from the session |
| `session/destroy` | End and clean up a session |
| `evaluate/create` | Create an adversarial evaluation session (isolated context: contract + diff + CLAUDE.md only) |
| `evaluate/score` | Request structured scoring against contract criteria |
| `capabilities` | Query what the provider supports (workflow, evaluation, or both) |

#### Notifications (Provider → Core)

| Method | Description |
|--------|-------------|
| `response/chunk` | Streamed response text chunk |
| `response/tool_use` | Tool use event from the AI session |
| `response/complete` | Session response complete with structured result |
| `evaluate/result` | Evaluation complete with per-criterion scores and findings |
| `metrics/event` | Provider-side metrics event |

#### Provider Capabilities

Providers declare their capabilities during `initialize`. This allows the core to route evaluation work appropriately:

```jsonc
{
  "capabilities": {
    "workflow": true,          // Can orchestrate coding sessions (plan, execute, etc.)
    "evaluation": true,        // Can run adversarial evaluation
    "agentTeams": true,        // Can spawn parallel sub-agents (Tier 3)
    "models": ["claude-opus-4-8", "claude-sonnet-4-6"],  // Available models
    "defaultEvalModel": "claude-opus-4-8"
  }
}
```

The Codex provider declares `evaluation: true` but `workflow: false` — it's an evaluator only. The Claude Code provider declares both. This lets the core route: workflow commands → Claude Code provider, evaluation → both providers in parallel.

#### Example Exchange

```jsonc
// Core → Provider: create session
{"jsonrpc": "2.0", "id": 1, "method": "session/create", "params": {"workingDirectory": "/path/to/project"}}

// Provider → Core: session created
{"jsonrpc": "2.0", "id": 1, "result": {"sessionId": "abc-123"}}

// Core → Provider: send planning prompt
{"jsonrpc": "2.0", "id": 2, "method": "session/send", "params": {"sessionId": "abc-123", "message": "..."}}

// Provider → Core: streamed response chunks
{"jsonrpc": "2.0", "method": "response/chunk", "params": {"sessionId": "abc-123", "text": "## Plan..."}}

// Provider → Core: response complete
{"jsonrpc": "2.0", "method": "response/complete", "params": {"sessionId": "abc-123", "result": {"planPath": ".claude/plans/auth.md", "contractTier": 2}}}
```

---

## Security & Configuration

### Authentication

No authentication for the CLI itself. Provider authentication is handled by each provider, supporting **both API keys and subscription-based auth**:

- **Claude Code provider:** Delegates to the Claude Code SDK's existing auth. Supports API key (`ANTHROPIC_API_KEY`) OR subscription auth (Claude Code Max monthly/annual plans via OAuth). The SDK handles this transparently — the provider passes through whichever auth method the user has configured.
- **Codex provider:** Supports OpenAI API key (`OPENAI_API_KEY`) OR existing Codex CLI subscription auth. Users with active Codex or OpenAI subscriptions can authenticate through their existing login without needing a separate API key.

The CLI should never force users into API-key-only auth. Many developers use their Max/Pro/subscription plans for daily work and shouldn't need to provision separate API keys to use PICE.

### Environment Variables

| Variable | Purpose | Required |
|----------|---------|----------|
| `PICE_PROVIDER` | Override default workflow provider | No (default: `claude-code`) |
| `PICE_CONFIG_DIR` | Override `.pice/` location | No (default: `.pice/`) |
| `PICE_TELEMETRY` | Enable/disable telemetry | No (default from config) |
| `ANTHROPIC_API_KEY` | Claude Code SDK auth via API key | No (alternative: subscription auth via Claude Code Max/Pro) |
| `OPENAI_API_KEY` | OpenAI/Codex auth via API key | No (alternative: subscription auth via Codex/OpenAI plan) |
| `PICE_EVAL_MODEL` | Override evaluation model | No (default from config) |
| `PICE_ADVERSARIAL_MODEL` | Override adversarial evaluator model | No (default from config) |

### Configuration (`.pice/config.toml`)

```toml
[provider]
name = "claude-code"         # Primary provider for workflow orchestration
# Provider-specific settings passed during initialize

[evaluation]
# Dual-model adversarial evaluation configuration
# Users can override which models/providers run evaluation

[evaluation.primary]
provider = "claude-code"     # Contract evaluator — formal grading
model = "claude-opus-4-8"   # User-selectable model

[evaluation.adversarial]
provider = "codex"           # Design challenge evaluator — different model family
model = "gpt-5.5"           # User-selectable model
effort = "xhigh"              # Reasoning effort: "low", "high", "xhigh"
enabled = true               # Can be disabled for Tier 1 or cost savings

[evaluation.tiers]
# Override default tier behavior
tier1_models = ["claude-opus-4-8"]                     # Single evaluator
tier2_models = ["claude-opus-4-8", "gpt-5.5"]          # Dual-model
tier3_models = ["claude-opus-4-8", "gpt-5.5"]          # Dual-model + agent team
tier3_agent_team = true                                 # Enable 4-agent team for Tier 3

[telemetry]
enabled = false              # Opt-in anonymous metrics
endpoint = "https://telemetry.pice.dev/v1/events"

[metrics]
db_path = ".pice/metrics.db"

[init]
# Default template customizations
project_type = "auto"        # auto-detect or manual override
```

### Deployment

This is a local CLI tool. No server deployment. Distribution via:
- GitHub Releases (pre-built binaries for all platforms)
- NPM (`npm install -g pice`)
- Cargo (`cargo install pice`)

---

## Implementation Phases

### Phase 1: Foundation

**Goal:** Rust CLI skeleton, provider protocol, and `pice init`

- [ ] Rust workspace setup (Cargo.toml, crate structure)
- [ ] CLI framework with clap (arg parsing, help, shell completions for all 11 commands)
- [ ] `.pice/` config management (read/write TOML config)
- [ ] Template engine — embed `.claude/` framework files, scaffold on `pice init`
- [ ] JSON-RPC protocol types (Rust crate `pice-protocol` + TS package `@pice/provider-protocol`)
- [ ] Provider host — spawn, manage, and communicate with provider processes
- [ ] `@pice/provider-base` — TS utilities for building providers
- [ ] Stub provider (echo provider for testing the protocol without Claude Code)

**Validation:** `pice init` creates a correct `.claude/` directory. `pice --help` shows all commands. Provider host can spawn and communicate with the stub provider.

### Phase 2: Providers + Core Orchestration

**Goal:** Working `pice plan`, `pice execute`, `pice evaluate` with dual-model adversarial evaluation

- [ ] `@pice/provider-claude-code` — full implementation using Claude Code SDK (workflow + evaluation)
- [ ] `@pice/provider-codex` — Codex/OpenAI provider for adversarial evaluation
- [ ] Session management (create, send, stream, destroy) for both providers
- [ ] `pice plan` — assembles planning prompt, orchestrates interactive session, captures plan + contract
- [ ] `pice execute` — launches fresh session with plan context, streams implementation
- [ ] `pice evaluate` — dual-model adversarial evaluation engine:
  - Contract parsing and tier detection from plan files
  - Tier 1: single Claude evaluator session (contract grading)
  - Tier 2: Claude evaluator + parallel Codex adversarial review (design challenge)
  - Tier 3: Claude agent team (4 evaluators) + parallel Codex adversarial review at xhigh effort
  - Parallel provider orchestration (Rust core runs both providers concurrently via tokio)
  - Unified evaluation report synthesis (contract scores + design challenge findings)
- [ ] User-configurable model selection via `.pice/config.toml`
- [ ] Graceful degradation — if adversarial provider is unavailable, fall back to single-model evaluation with a warning
- [ ] Terminal output formatting (streamed responses, progress indicators, evaluation tables)

**Validation:** Full PICE loop works end-to-end: `pice plan "add health check endpoint"` → `pice execute plan.md` → `pice evaluate plan.md`. Tier 2 evaluation runs Claude + Codex in parallel and produces a unified report. Evaluation works with only Claude configured (graceful degradation).

### Phase 3: Workflow Commands

**Goal:** Complete workflow lifecycle commands

- [ ] `pice prime` — orchestrate codebase orientation
- [ ] `pice review` — orchestrate code review + regression suite
- [ ] `pice commit` — orchestrate standardized git commit
- [ ] `pice handoff` — orchestrate session state capture
- [ ] `pice status` — display active plans and workflow state
- [ ] Context isolation between phases (ensure fresh sessions where the methodology requires them)

**Validation:** All 8 orchestration commands work. A developer can run the complete PICE workflow from `pice prime` through `pice commit` using only CLI commands.

### Phase 4: Metrics, Benchmarking & Telemetry

**Goal:** Quality measurement and the open-source differentiator

- [ ] SQLite metrics store (schema, read/write operations)
- [ ] Metrics collection hooks in all orchestration commands (automatic, passive)
- [ ] `pice metrics` — aggregation queries, terminal output, JSON/CSV export
- [ ] `pice benchmark` — git integration, trend analysis, comparative reports
- [ ] Opt-in telemetry client (anonymous, transparent, logged locally)
- [ ] Terminal chart rendering for benchmark reports

**Validation:** After running several PICE loops, `pice metrics` shows accurate aggregate data. `pice benchmark` produces a meaningful before/after report. Telemetry payload is logged and inspectable.

### Phase 5: Distribution & Polish

**Goal:** Installable, documented, open-source ready

- [ ] GitHub Actions CI (lint, test, build for Rust + TS)
- [ ] GitHub Actions release pipeline (cross-compile for all platforms)
- [ ] NPM distribution packages (platform-specific binary resolver)
- [ ] `cargo install` support
- [ ] README.md with installation, quickstart, and architecture overview
- [ ] CLAUDE.md for the repo itself
- [ ] Contributing guide (Rust core vs. TS providers boundary)
- [ ] Provider development guide with examples
- [ ] Methodology docs migrated and expanded from current `.claude/docs/`
- [ ] LICENSE (MIT)

**Validation:** `npm install -g pice && pice init && pice --help` works on macOS, Linux, and Windows. A contributor can clone, build, and run tests in under 5 minutes. Methodology docs are readable on GitHub.

---

## Success Criteria

- [ ] All 11 commands implemented and working end-to-end
- [ ] Full PICE loop (plan → execute → evaluate) completes successfully via Claude Code SDK
- [ ] Dual-model evaluation (Claude + Codex) runs in parallel for Tier 2+ contracts and produces unified report
- [ ] Evaluation works with only one provider configured (graceful degradation)
- [ ] Users can configure evaluator models via `.pice/config.toml`
- [ ] Provider protocol is documented and a second provider can be built from the docs alone
- [ ] `pice metrics` produces meaningful quality data after 5+ PICE loops
- [ ] `pice benchmark` shows measurable difference between structured and unstructured workflows
- [ ] Installs cleanly via `npm install -g pice` on macOS, Linux, and Windows
- [ ] Repository is public, MIT licensed, with clear contribution guidelines
- [ ] Methodology docs are complete and readable without installing the CLI
- [ ] Rust binary starts in under 100ms (excluding provider startup)
- [ ] Anonymous telemetry is opt-in, transparent, and privacy-respecting

---

## Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Claude Code SDK API changes break the provider | High | Medium | Pin SDK version, abstract behind provider protocol, test against SDK in CI |
| JSON-RPC protocol design doesn't cover all provider needs | High | Medium | Design protocol with extensibility (custom methods), validate by building a second stub provider |
| Cross-platform Rust builds fail for some targets | Medium | Medium | Use `cross` for cross-compilation, test in CI on all platforms before release |
| SQLite metrics DB corruption or migration issues | Medium | Low | Use WAL mode, schema versioning with migrations, backup before schema changes |
| Telemetry privacy concerns hurt adoption | High | Medium | Opt-in only, fully transparent payload, local log of everything sent, no PII, open-source the telemetry endpoint |
| MVP scope is too large for initial release | High | Medium | Phases are ordered by dependency — Phase 1-2 alone is a usable tool. Phase 3-5 can be iterative releases |
| Provider process management is complex across platforms | Medium | Medium | Use tokio process management with proper signal handling. Test on all platforms in CI |
| Dual-model evaluation requires two API keys and costs more | Medium | High | Graceful degradation to single-model eval. Clear docs on cost implications. Tier 1 defaults to single-model. Users opt into dual-model via config |
| OpenAI/Codex API changes or rate limits affect adversarial evaluation | Medium | Medium | Abstract behind provider protocol. Codex provider can be swapped for any OpenAI-compatible endpoint. Timeout + retry logic in provider |

---

## Assumptions

1. **Claude Code SDK (`@anthropic-ai/claude-code`) provides sufficient programmatic control** — session creation, message sending, response streaming, and tool result access are all available through the SDK. If the SDK is limited, the provider may need to fall back to CLI subprocess mode.
2. **JSON-RPC over stdio is sufficient for provider IPC** — streaming, bidirectional communication, and structured data all work. If latency or throughput is an issue, the protocol can be upgraded to Unix domain sockets.
3. **The existing `.claude/` command prompts translate cleanly to orchestrated sessions** — the current slash commands were written for interactive use. Orchestration may require prompt adjustments for programmatic consumption.
4. **SQLite is sufficient for metrics storage at project scale** — individual developer metrics on a single project. If team-scale metrics are needed, this would need a different storage backend.
5. **Anonymous telemetry collection is technically feasible without a hosted backend in the MVP** — the telemetry client collects and stores locally. The actual endpoint and public dashboard are post-MVP.
6. **Pre-built Rust binaries via GitHub Actions work reliably for all target platforms** — macOS arm64/x64, Linux arm64/x64, Windows x64. ARM Linux may need cross-compilation tooling.
7. **OpenAI/Codex API provides sufficient programmatic control for adversarial evaluation** — text generation with structured output, configurable reasoning effort. The Codex provider may use the OpenAI SDK directly or the Codex CLI companion script pattern from the existing framework.
8. **Parallel provider orchestration via tokio is reliable** — running two provider processes simultaneously with concurrent JSON-RPC communication. Timeout and error handling must be robust to prevent one provider's failure from blocking the other.

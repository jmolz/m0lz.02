# Product Requirements Document: PICE CLI v0.2+ (PRDv2)

> **Document scope:** Post-v0.1 roadmap (v0.2 through v0.5).
> **Relationship to v0.1:** This document assumes the v0.1 PRD (`.claude/PRD.md`) as historical baseline — the plan/implement/contract-evaluate lifecycle, the provider protocol, SQLite metrics, and the Claude Code + Codex providers all remain in place. PRDv2 extends, replaces, and in two places pivots v0.1 architecture.
> **Status:** Draft — supersedes the planning sketch at `.claude/plans/pice-ux-patterns-prd.md`.
> **Grounding:** Every design decision in this document traces to either the published roadmap (`docs/roadmap.md`) or the research library (`docs/research/`). Key figures are embedded inline; full derivations are linked.

---

## Executive Summary

PICE v0.1 shipped the core loop: Plan → Implement → Contract-Evaluate with dual-model adversarial grading, orchestrated by a Rust CLI over a JSON-RPC provider protocol. It proved the methodology is buildable and measurable. 217 tests pass. The loop works on single-component features.

**PRDv2 is about what v0.1 cannot do.** Empirical research across Google SRE, Adyen, ICSE, and AWS postmortems shows that software breaks overwhelmingly at the **boundaries between components, not inside them** — 68% of outages trigger at integration points. SWE-Bench Pro demonstrates the same pattern for AI coding agents: models that score >70% on single-file tasks drop to ~23% on multi-file tasks. v0.1's feature-level contracts cannot detect this class of failure. Contract says "does the feature work?" — but never asks "is it actually shipped, and do the seams between layers hold under composition?"

**v0.2 — Stack Loops** reshapes the PICE loop to run per-layer across a technology stack, with seam verification at every boundary, adaptive evaluation pass allocation grounded in the correlated Condorcet Jury Theorem, a committable workflow pipeline, human-in-the-loop approval gates, parallel execution via git worktree isolation, and a headless daemon architecture that makes v0.3's web dashboard trivial to add.

**v0.3 — Arch Experts + Web Dashboard** introduces dynamically generated specialist agents inferred from project architecture files, and a browser-based visualization layer for evaluation progress, confidence curves, seam maps, and review gate actioning.

**v0.4 — Implicit Contract Inference** closes the "unknown unknown" gap: automated discovery of cross-component assumption asymmetries from static analysis and distributed traces, using Honeycomb-style observability-driven development to derive seam contracts that the developer never wrote.

**v0.5 — Self-Evolving Verification** closes the outer loop: PICE learns from its own execution history. Using predictive test selection techniques proven at Meta, Google, and Netflix, every evaluation makes the next one smarter, more targeted, and cheaper. Checks with zero historical hit rate are pruned; checks that repeatedly catch real bugs are boosted. Confidence targets adapt to project risk history.

**MVP Goal (v0.2):** Ship per-layer PICE loops with seam verification, adaptive pass allocation, committable workflow pipeline, parallel execution, approval gates, and a headless daemon architecture — proving that structured AI coding verification scales to full stacks and composes across boundaries.

---

## Relationship to the v0.1 PRD

The v0.1 PRD (`.claude/PRD.md`) remains the authoritative document for:
- The provider protocol (JSON-RPC over stdio, capabilities, session lifecycle)
- The 12 MVP commands (`init`, `prime`, `plan`, `execute`, `evaluate`, `review`, `commit`, `handoff`, `status`, `metrics`, `benchmark`, `completions`)
- Tier 1/2/3 evaluation defaults and dual-model adversarial architecture
- Existing `.pice/config.toml` surface for `[provider]`, `[evaluation.primary]`, `[evaluation.adversarial]`, `[telemetry]`, `[metrics]`
- NPM binary distribution pattern
- Rust core + TypeScript providers split

PRDv2 **extends** v0.1 in these areas:
- Adds layer-aware per-component loops on top of the existing feature-level loop
- Adds `.pice/layers.toml`, `.pice/workflow.yaml`, `.pice/contracts/*.toml` to the configuration surface
- Adds seam verification as a first-class concept alongside contract grading
- Adds adaptive pass allocation on top of the existing fixed-tier defaults
- Adds 7 new commands (`pice layers`, `pice seam`, `pice validate`, `pice review-gate`, `pice logs`, `pice dashboard`, `pice clean`)
- Expands the provider protocol with layer-scoped session creation, seam check requests, and manifest event streaming

PRDv2 **pivots** v0.1 in these areas:
- **Architecture**: The Rust core becomes a headless daemon. The CLI becomes the first of several adapters. v0.1's "CLI is the binary" assumption is dropped. (See [Architectural Pivot](#architectural-pivot-headless-daemon--adapters).)
- **Evaluation state**: Evaluations become long-lived stateful entities (the "verification manifest"), not single in-process command runs. This enables background execution, multi-adapter interaction, and the gate-pause-resume lifecycle.

PRDv2 does **not** modify:
- The plan/implement/contract-evaluate phase names or semantics
- The context-isolation rule for evaluators (still contract + diff + CLAUDE.md only)
- The provider protocol's stdio-based JSON-RPC for core↔provider communication (a new socket-based protocol is introduced for CLI↔daemon, but the provider protocol is untouched)
- The Tier 1/2/3 naming convention or dual-model adversarial architecture
- SQLite as the metrics store

**Public narrative compatibility**: `docs/roadmap.md` is the public-facing narrative of where PICE is going. PRDv2 is the internal implementation spec. Where the two disagree, PRDv2 wins and `docs/roadmap.md` should be updated to match — but roadmap updates are out of scope for this document.

---

## Target Users

### Primary Persona (unchanged from v0.1): AI-Assisted Developer

- **Who:** Software developer using Claude Code for daily coding tasks, already running PICE v0.1
- **New pain point in v0.2:** v0.1's single-loop evaluation catches per-feature contract violations but repeatedly ships features that "work" in isolation and break at deployment — missing env vars, unverified Docker builds, untested infrastructure changes, schema drift between layers. The user has learned the hard way that "feature complete" ≠ "production ready."
- **What v0.2+ gives them:** Stack Loops force every layer to pass before a feature can ship. Seam checks catch the cross-layer assumptions that v0.1's contracts ignore. Adaptive pass allocation reduces evaluation cost while increasing confidence.

### Primary Persona NEW in v0.2: Team Lead / Staff Engineer

- **Who:** Technical lead responsible for an AI-assisted development team's velocity and production reliability
- **Technical Level:** Senior developer with architectural judgment
- **Key Need:** A way to enforce consistent verification standards across the team without manually reviewing every PR. A way to answer "are we actually shipping what we think we're shipping?" from data, not vibes.
- **Pain Point:** Different team members run different informal AI coding workflows. Infrastructure and deployment checks get skipped inconsistently. Review gates are tribal knowledge. There's no audit trail for which AI-assisted changes were actually verified and to what rigor.
- **What v0.2+ gives them:** `.pice/workflow.yaml` codifies the verification pipeline as a committable file. Approval gates let them insert human review at configurable points (Tier 3 changes, infrastructure layers, deployment layers). The headless daemon architecture and v0.3 dashboard give team-wide visibility. The SQLite audit trail captures every gate decision, evaluation result, and reviewer identity.

### Secondary Persona (unchanged from v0.1): Open-Source Contributor

- **Who:** Developer building a new provider for a different AI coding tool, or extending PICE for a custom verification need
- **Technical Level:** Developer comfortable with JSON-RPC, TypeScript, or Rust
- **New v0.2 contribution surface:**
  - Arch Expert generators (v0.3) — take a project's architecture files, output specialist agent definitions
  - Seam check modules — protocol-specific verification libraries (gRPC, GraphQL, REST, Kafka) pluggable at layer boundaries
  - Adapter implementations — new interfaces to the headless daemon (VS Code extension, Slack bot, GitHub Action) via the daemon's Unix socket protocol
- **Pain Point addressed:** v0.1 contributors could only build AI providers. v0.2's adapter architecture and seam check plugin surface open three additional extension points without touching the Rust core.

### Tertiary Persona NEW in v0.4: Production SRE

- **Who:** Site reliability engineer responsible for production observability
- **Technical Level:** Senior operations engineer
- **Key Need:** Closing the loop from production incidents back to verification. When a seam fails in production, the same seam should never fail a second time.
- **Pain Point:** Traditional contract testing catches structural issues but misses behavioral ones. Post-incident reviews lead to "add a test" action items that rarely get systematically connected to the verification pipeline.
- **What v0.4+ gives them:** Implicit contract inference derives seam contracts from distributed traces. Tracetest-style "turn production signals into assertions" becomes native. v0.5's self-evolving verification boosts historically-triggering checks automatically.

---

## MVP Scope (v0.2)

### In Scope — v0.2 "Stack Loops"

**Core Methodology**
- [ ] Per-layer PICE loops — a feature is PASS only when every layer passes
- [ ] Layer detection via six-level heuristic stack (manifest → directory → framework → config → imports → override)
- [ ] `.pice/layers.toml` format with `depends_on`, `always_run`, `paths`, `type = "meta"` for IaC
- [ ] File-level layer tagging for fullstack-in-one frameworks (Next.js, Remix, SvelteKit, Nuxt)
- [ ] Seam verification at every layer boundary, targeting the 12 empirically validated failure categories (Google SRE + Adyen + ICSE data)
- [ ] Layer-specific contract templates (`.pice/contracts/{layer}.toml`) for infrastructure, deployment, observability, database, API, frontend, backend
- [ ] Environment-specific contract properties (`[contract.api.environments.production]`)
- [ ] Always-run layers (infrastructure, deployment, observability) that never get skipped regardless of change scope

**Adaptive Evaluation**
- [ ] Bayesian-SPRT (Sequential Probability Ratio Test) for adaptive halting — stop passes when posterior confidence exceeds target OR accumulated evidence rules out the target
- [ ] ADTS (Adversarial Divergence-Triggered Scaling) — when Claude and Codex diverge beyond threshold, escalate to next tier or additional passes
- [ ] VEC (Verification Entropy Convergence) — track the per-pass entropy reduction; halt when marginal entropy drops below a configurable floor
- [ ] Honor the mathematically derived ~96.6% confidence ceiling (correlated Condorcet with ρ ≈ 0.35) — do not pretend more passes can breach it
- [ ] Cost tracking per evaluation pass in SQLite; surface `$X.XX` in all evaluation output

**Orchestration Pipeline**
- [ ] `.pice/workflow.yaml` defines the evaluation pipeline (phases, parallelism, retry, review, layer overrides, budgets)
- [ ] Workflow inheritance: framework defaults → project workflow → user workflow (`~/.pice/workflow.yaml`), with floor-based merge semantics
- [ ] `pice validate` command — schema-checks workflow.yaml, layers.toml, and per-layer contracts
- [ ] Parallel execution of independent layers via git worktree isolation (`git worktree add` into `.pice/worktrees/{feature-id}/{layer}`)
- [ ] Sequential execution of dependency-ordered layers
- [ ] Worktree lifecycle management: create, merge back on pass, preserve on fail (configurable), clean via `pice clean`

**Human-in-the-Loop**
- [ ] Review gates triggered by configurable conditions (`tier >= 3 OR layer == infrastructure OR confidence < 0.95`)
- [ ] CLI gate interaction: approve / reject / details / skip
- [ ] Background gate deferral: gates written to verification manifest, actionable via `pice review-gate {feature} --layer {layer}`
- [ ] Timeout behavior: reject / approve / skip, configurable per gate
- [ ] Full audit trail of gate decisions in SQLite (reviewer identity, timestamp, decision, reason)

**Headless Daemon Architecture**
- [ ] Rust core becomes a daemon process (`pice-daemon` binary, or `pice daemon start`)
- [ ] Daemon owns: state machine, verification manifest, SQLite metrics, provider process lifecycle, adaptive algorithms, gate state
- [ ] CLI becomes an adapter that communicates with the daemon via Unix domain socket using newline-delimited JSON-RPC
- [ ] Daemon auto-starts on first CLI command if not already running (zero-config startup)
- [ ] Daemon socket authentication via filesystem permissions + generated token
- [ ] Verification manifest (`~/.pice/state/{feature-id}.manifest.json`) is the source of truth for evaluation state — both CLI and future dashboard adapters read from it
- [ ] Windows compatibility: named pipes instead of Unix sockets (same JSON-RPC framing)

**Background Execution**
- [ ] `pice evaluate --background` returns immediately, daemon runs evaluation
- [ ] `pice status` shows all active, pending, completed evaluations with layer progress bars and confidence curves
- [ ] `pice logs {feature} --follow` streams real-time output (layer completions, confidence updates, seam check results)
- [ ] Desktop notifications on completion (macOS `osascript`, Linux `notify-send`, Windows toast, fallback to bell + stdout)
- [ ] Multiple concurrent background evaluations with isolated worktrees and manifests

**Provider Protocol Extensions (v0.2)**
- [ ] `session/create` adds `layer: string` and `workingDirectory` (worktree path) parameters
- [ ] `evaluate/create` adds `seamChecks: SeamCheckSpec[]` parameter
- [ ] New `manifest/event` notification: providers emit structured events (layer-started, pass-complete, confidence-updated, gate-requested)
- [ ] New `layer/detect` method for v0.2 provider-side layer detection hints (framework-specific signals)
- [ ] Backwards compatible: v0.1 providers continue to work in single-layer fallback mode

### Out of Scope (v0.2)

- ❌ **Web dashboard** — deferred to v0.3. Architecture supports it (headless daemon, manifest-based state), but v0.2 ships only the CLI adapter.
- ❌ **Arch Experts** — deferred to v0.3. v0.2 uses static per-layer evaluator agents; v0.3 adds dynamically generated specialists inferred from architecture files.
- ❌ **Implicit contract inference** — deferred to v0.4. v0.2 requires developers to write contracts manually (or use defaults from templates).
- ❌ **Self-evolving verification** — deferred to v0.5. v0.2 collects the metrics needed (per-check hit rates, false positive rates, cost per true positive) but does not yet act on them.
- ❌ **Cross-repo seam checks (polyrepo support)** — deferred to v0.4. v0.2 works within a single repository. `.pice/external-contracts.toml` is the manual workaround.
- ❌ **CI/CD gate actioning via PR reviews** — deferred to v0.3. v0.2 supports CLI and manifest-based gates; webhook integration comes later.
- ❌ **Archon-style "magic workflow routing"** — explicitly rejected as unauditable and incompatible with PICE's committable-pipeline philosophy.
- ❌ **Single-model evaluation as default** — the dual-model adversarial architecture is core to PICE; single-model is a graceful-degradation fallback only.
- ❌ **Monolithic codebase treatment** — every feature in v0.2 presupposes layer awareness. A codebase without detectable layers degrades to v0.1 single-loop behavior, with a warning.
- ❌ **Static workflow definitions beyond YAML** — programmatic workflow construction (e.g., JavaScript workflow authoring) is out of scope. YAML + layer/user overrides is the full surface for v0.2.
- ❌ **IDE extensions** — remain post-v0.5.
- ❌ **Hosted cloud version** — remains explicitly out of scope for all versions.

---

## User Stories (v0.2)

1. As a developer adding a multi-layer feature, I want to run `pice plan "add user auth"` so that PICE generates a layer-aware plan with per-layer contracts and seam checks, not a single flat contract.
2. As a developer running `pice execute`, I want independent layers (backend, frontend) to implement in parallel worktrees so I'm not waiting for sequential execution.
3. As a developer running `pice evaluate`, I want adaptive pass allocation to stop as soon as confidence exceeds my target — so a high-confidence layer costs $0.08 instead of $0.40.
4. As a developer with a CSS-only change, I want `pice evaluate` to skip the database layer but NOT skip the infrastructure layer, because that's where forgotten env vars live.
5. As a team lead, I want to commit `.pice/workflow.yaml` to the repo so every team member runs the same verification pipeline with the same tier thresholds, budgets, and review gates.
6. As a team lead, I want infrastructure and deployment layers to pause for my approval before the evaluation can proceed, regardless of AI confidence.
7. As a developer kicking off a 7-layer evaluation that will take 4 minutes, I want to run it in the background so I can keep working, and get a desktop notification when it completes.
8. As a developer, I want `pice status` to show all my active evaluations with per-layer confidence bars and current pending gates.
9. As a developer debugging a failed layer, I want the worktree preserved so I can `cd` into it and inspect what the AI actually wrote.
10. As a developer reviewing a gated layer in a background evaluation, I want `pice review-gate auth-feature --layer infrastructure` to show me the same gate prompt as if it had run in the foreground.
11. As a developer running concurrent evaluations on multiple features, I want the daemon to manage them independently without conflicts, each in its own worktree and manifest.
12. As an open-source contributor, I want to build a Slack adapter that talks to the PICE daemon via its documented Unix socket protocol, without modifying the Rust core.
13. As a developer in CI, I want `pice evaluate --background` + `pice status --wait {feature}` to block until completion with proper exit codes for my pipeline.
14. As a developer adopting v0.2, I want to run `pice init --upgrade` in my existing v0.1 project and have it generate a proposed `.pice/layers.toml` from layer detection, which I review and commit.
15. As a developer with a Next.js app, I want PICE to correctly tag `pages/api/users.ts` as belonging to the API, frontend, AND database layers simultaneously, and evaluate each lens independently.

---

## Tech Stack

| Technology | Purpose | Version | Notes |
|------------|---------|---------|-------|
| Rust (stable) | Core CLI, daemon, state machine, config, metrics, adaptive algorithms, provider host, seam checks | Latest stable | Unchanged from v0.1 |
| TypeScript 5.x | Provider implementations, workflow validator (optional), Arch Expert generators (v0.3) | 5.x | Unchanged from v0.1 |
| Node.js 22+ LTS | Runtime for TypeScript providers | 22+ | Unchanged from v0.1 |
| clap 4.x | CLI framework | 4.x | Unchanged from v0.1 |
| tokio 1.x | Async runtime, now with `UnixListener` / `NamedPipe` for daemon socket | 1.x | Extended usage |
| rusqlite | Local metrics storage, audit trail, gate state, manifest persistence | 3.x (embedded) | Schema extended in v0.2 |
| serde / serde_json | JSON-RPC + TOML + manifest serialization | 1.x | Unchanged |
| serde_yaml | `workflow.yaml` parsing | 0.9.x | **NEW in v0.2** |
| git2-rs | Worktree creation/removal from Rust | 0.18+ | **NEW in v0.2** |
| rust-embed | Template and contract default embedding | Unchanged | Extended |
| `@anthropic-ai/claude-agent-sdk` | Claude Code provider (workflow + evaluation, including subagent spawning for Arch Expert committees in v0.3) | Latest | Unchanged |
| OpenAI SDK | Codex / GPT adversarial evaluator | Latest | Unchanged |
| pnpm | TypeScript workspace manager | Unchanged | Unchanged |
| A lightweight HTTP server (axum or hyper) | Daemon health endpoint (v0.2) + web dashboard HTTP serving (v0.3) | Latest | **NEW in v0.2** (minimal), expanded v0.3 |
| tracing + tracing-subscriber | Structured logging (stderr for CLI, file-based for daemon) | 0.1.x | Unchanged but daemon adds file sinks |

**Deliberately NOT added:**
- No message queue (Redis, RabbitMQ, NATS) — the verification manifest in the filesystem + Unix socket is sufficient
- No external database — SQLite remains authoritative
- No container runtime dependency — worktrees are plain git worktrees, not containerized sandboxes
- No JavaScript workflow DSL — YAML + inheritance is the full surface
- No WebSocket library in v0.2 — v0.3 dashboard will add this

---

## Architectural Pivot: Headless Daemon + Adapters

> This is the single largest architectural change from v0.1. It is a firm design decision for v0.2.

### v0.1 Architecture (what we have)

```
User → pice CLI (Rust binary)
         ├── parses args
         ├── loads config
         ├── manages state in-process
         ├── spawns provider process
         ├── streams JSON-RPC over stdio
         ├── writes to SQLite
         ├── displays to terminal
         └── exits
```

Every `pice` invocation is a self-contained process. State lives in files (`.pice/metrics.db`) but there is no long-lived coordinator. This works for v0.1's single-loop, synchronous, foreground-only evaluation model.

### v0.2 Architecture (what we're building)

```
┌──────────────────────────────────────────────────────────────┐
│  pice-daemon (long-lived headless process)                    │
│                                                                │
│  ┌────────────────────────────────────────────────────────┐  │
│  │  Orchestrator                                           │  │
│  │  ├── Stack Loops engine (per-layer PICE execution)     │  │
│  │  ├── Adaptive algorithms (Bayesian-SPRT, ADTS, VEC)    │  │
│  │  ├── Gate state manager                                │  │
│  │  └── Worktree lifecycle manager                        │  │
│  └────────────────────────────────────────────────────────┘  │
│                                                                │
│  ┌────────────────────────────────────────────────────────┐  │
│  │  Provider Host (spawns TS providers, JSON-RPC stdio)   │  │
│  │  ├── Claude Code provider                              │  │
│  │  ├── Codex provider                                    │  │
│  │  └── (Community providers)                             │  │
│  └────────────────────────────────────────────────────────┘  │
│                                                                │
│  ┌────────────────────────────────────────────────────────┐  │
│  │  State layer                                            │  │
│  │  ├── Verification manifests (~/.pice/state/*.json)     │  │
│  │  ├── SQLite metrics + audit trail                      │  │
│  │  └── Gate state store                                  │  │
│  └────────────────────────────────────────────────────────┘  │
│                                                                │
│  ┌────────────────────────────────────────────────────────┐  │
│  │  Adapter Interface                                      │  │
│  │  ├── Unix socket (macOS/Linux)                         │  │
│  │  └── Named pipe (Windows)                              │  │
│  │  Protocol: newline-delimited JSON-RPC                  │  │
│  └────────────────────────────────────────────────────────┘  │
└───────────────┬────────────────────────────────┬─────────────┘
                │                                │
   ┌────────────▼───────────┐      ┌────────────▼───────────┐
   │  pice CLI adapter       │      │  Dashboard adapter     │
   │  (the `pice` binary)    │      │  (v0.3, HTTP + WS)     │
   │                         │      │                         │
   │  - Parses args          │      │  - Serves SPA at       │
   │  - Submits requests     │      │    :3141              │
   │  - Renders TTY output   │      │  - Real-time updates   │
   │  - Streams event feed   │      │  - Gate actioning      │
   └─────────────────────────┘      └─────────────────────────┘

             (Future adapters: CI, Slack, VS Code, webhooks)
```

### Why this pivot

1. **Background execution requires a long-lived process.** Any architecture where evaluation state lives in a short-lived CLI process cannot support "kick off an evaluation, keep working, get notified when done." The daemon IS the background.
2. **Multi-adapter visibility requires a shared state source.** v0.3's dashboard must read the same manifest the CLI reads. Without a daemon, two adapters would race for file locks, double-handle notifications, and diverge in state. With a daemon, both read from one source.
3. **Gate pause/resume requires durable state between interactions.** A developer can kick off evaluation, go to lunch, come back, and action the gate that fired while they were away. The manifest survives; the daemon serves it on request.
4. **The manifest-as-source-of-truth pattern is battle-tested.** MCP-Guard uses the same daemon+bridge+CLI architecture. Docker, Kubernetes, systemd, and nearly every multi-client local tool converge on this shape because the alternatives (file locking, multi-process state, SQLite write contention) are worse.
5. **Provider process reuse.** Starting a Claude Code provider subprocess takes ~800ms. For a 7-layer evaluation with 5 passes each, that's 35 provider spawns — 28 seconds of just startup overhead. The daemon keeps provider processes warm across layer boundaries.

### Trade-offs accepted

- **First-run latency tax**: The first `pice` command after a reboot pays ~400ms to auto-start the daemon. Subsequent commands are <50ms. Mitigation: `pice daemon start` at shell startup is optional for users who notice.
- **Operational surface**: A daemon can crash, leak memory, or hang. Mitigation: watchdog supervisor in the CLI adapter, `pice daemon restart` recovery command, exhaustive logging to `~/.pice/logs/daemon.log`.
- **Debug complexity**: Bugs that used to be "rerun the CLI" now may require "check the daemon log." Mitigation: `pice daemon logs --follow`, explicit error propagation from daemon → CLI with context preservation, `PICE_DAEMON_INLINE=1` env var to run the orchestrator in-process for debugging (bypasses daemon, disables background/concurrent features).
- **Windows compatibility work**: Unix sockets don't exist on Windows. Named pipes behave differently. Mitigation: abstract the transport behind a `DaemonTransport` trait; each platform gets its own impl.
- **Installation complexity**: `pice` was one binary. Now there's `pice` + `pice-daemon`. Mitigation: ship them as a single binary with a subcommand mode (`pice --daemon` runs daemon mode), same pattern as `docker` / `dockerd` → consolidated into `docker` in recent versions.

### What the v0.2 CLI still does

- Argument parsing (clap)
- Config discovery and validation
- Terminal rendering (progress bars, tables, colors, streaming chunks)
- Keyboard input for interactive gate prompts
- Desktop notification dispatch
- First-run onboarding (`pice init`, auto-detecting layers)
- Shell completions

### What moves to the daemon

- State machine and PICE loop execution
- Provider process spawning and JSON-RPC multiplexing
- Verification manifest CRUD
- SQLite writes
- Adaptive algorithm computation
- Gate state management
- Worktree lifecycle

### What's shared (neither CLI nor daemon owns exclusively)

- Config file parsing: the CLI reads `.pice/*.toml` and `.pice/workflow.yaml` to validate before dispatch; the daemon reads them at execution time for actual use. Both sides must agree on the parsed representation, so the parser lives in `pice-core` (new crate, see below).

### New crate: `pice-core`

v0.2 introduces a shared crate that both the CLI and daemon depend on:

```
crates/
├── pice-cli/         # CLI adapter (formerly the whole binary)
├── pice-daemon/      # Daemon binary (new in v0.2)
├── pice-core/        # Shared logic (new in v0.2)
│   ├── config/       # TOML + YAML parsing
│   ├── layers/       # Layer detection + layers.toml types
│   ├── workflow/     # workflow.yaml types + validation
│   ├── manifest/     # Verification manifest schema + CRUD helpers
│   ├── seam/         # Seam check types + default check library
│   ├── adaptive/     # Bayesian-SPRT, ADTS, VEC algorithms
│   └── protocol/     # Daemon RPC types (NOT provider protocol)
└── pice-protocol/    # Provider protocol (unchanged)
```

`pice-core` has zero async dependencies and zero network dependencies. It's pure logic + data types. Both the CLI (for validation preview) and daemon (for execution) consume it. This keeps the CLI → daemon split clean and prevents drift between what the CLI reports as valid and what the daemon accepts.

---

## Directory Structure (v0.2)

```
pice/
├── crates/
│   ├── pice-cli/              # CLI adapter binary
│   │   └── src/
│   │       ├── main.rs        # Entry point: dispatches to daemon
│   │       ├── commands/      # One module per command (expanded)
│   │       ├── adapter/       # Daemon socket client
│   │       ├── render/        # Terminal output (progress bars, tables)
│   │       └── notify/        # Desktop notifications
│   ├── pice-daemon/           # Daemon binary (NEW)
│   │   └── src/
│   │       ├── main.rs        # Daemon entry point
│   │       ├── server/        # Unix socket / named pipe listener
│   │       ├── orchestrator/  # Stack Loops engine
│   │       ├── provider/      # Provider process host (moved from cli)
│   │       ├── worktree/      # git worktree lifecycle
│   │       ├── gate/          # Gate state + interaction
│   │       ├── state/         # Manifest CRUD + SQLite
│   │       └── watchdog/      # Health checks + restart logic
│   ├── pice-core/             # Shared logic (NEW)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── config/        # TOML parsing (moved from cli)
│   │       ├── layers/        # Layer detection + layers.toml
│   │       ├── workflow/      # workflow.yaml schema
│   │       ├── manifest/      # Manifest types
│   │       ├── seam/          # Seam check types + defaults
│   │       ├── adaptive/      # SPRT, ADTS, VEC (pure functions)
│   │       └── protocol/      # Daemon RPC types
│   └── pice-protocol/         # Provider protocol (unchanged)
├── packages/                  # TypeScript providers (unchanged structure)
│   ├── provider-protocol/
│   ├── provider-base/
│   ├── provider-claude-code/  # Extended with v0.2 session/layer params
│   ├── provider-codex/        # Extended with layer-scoped evaluation
│   └── provider-stub/         # Updated for v0.2 protocol compliance
├── templates/
│   ├── claude/                # .claude/ template (unchanged)
│   └── pice/                  # .pice/ template (EXPANDED)
│       ├── config.toml
│       ├── layers.toml        # Generated skeleton (NEW)
│       ├── workflow.yaml      # Default pipeline (NEW)
│       └── contracts/         # Per-layer contract defaults (NEW)
│           ├── backend.toml
│           ├── database.toml
│           ├── api.toml
│           ├── frontend.toml
│           ├── infrastructure.toml
│           ├── deployment.toml
│           └── observability.toml
├── docs/                      # Unchanged structure, expanded content
│   ├── methodology/
│   ├── guides/
│   │   ├── stack-loops.md     # NEW — the v0.2 adoption guide
│   │   └── migration-v01-to-v02.md  # NEW
│   ├── providers/
│   └── research/              # Unchanged — research library
├── npm/                       # Distribution (unchanged, + new daemon binary)
│   ├── pice/
│   └── pice-{platform}/       # Now ships both pice and pice-daemon
├── tests/
└── Cargo.toml                 # Workspace adds pice-daemon and pice-core
```

---

## Data Flow (v0.2)

### Workflow command flow (example: `pice execute plan.md`)

```
User runs `pice execute auth-plan.md --background`
        │
        ▼
┌────────────────────────────┐
│ CLI adapter (pice-cli)      │
│ 1. Parse args               │
│ 2. Load + validate          │
│    workflow.yaml, layers    │
│ 3. Check daemon running?    │
│    → auto-start if not      │
│ 4. Send RPC: execute/create │
│    { plan, workflow, layers │
│      mode: background }     │
│ 5. Print feature-id,        │
│    return immediately       │
└────────────┬───────────────┘
             │ Unix socket / named pipe
             │ newline-delimited JSON-RPC
             ▼
┌────────────────────────────────────┐
│ Daemon (pice-daemon)                │
│                                     │
│ 1. Allocate feature-id              │
│ 2. Write manifest:                  │
│    status = planning-execution      │
│ 3. Build dependency DAG from        │
│    plan's layer list                │
│ 4. For each layer level:            │
│    a. Identify parallel cohort      │
│    b. For each layer in cohort:     │
│       i.   git worktree add         │
│       ii.  Spawn provider session   │
│            with {layer, cwd}        │
│       iii. Stream exec phase        │
│       iv.  Run evaluation passes    │
│            (adaptive SPRT loop)     │
│       v.   Run seam checks for      │
│            this layer's boundaries  │
│       vi.  Update manifest          │
│    c. Wait for cohort to complete   │
│    d. If fail: retry policy or halt │
│    e. Merge passing worktrees back  │
│ 5. Mark manifest status = complete  │
│ 6. Fire notification                │
└────────────────────────────────────┘
```

### Status streaming flow (example: `pice status --follow`)

```
User runs `pice status --follow`
        │
        ▼
┌────────────────────────┐
│ CLI adapter             │
│ 1. Connect to daemon    │
│ 2. Send RPC:            │
│    manifest/subscribe   │
│    { featureId: null }  │
│    (all features)       │
└──────┬──────────────────┘
       │
       ▼
┌────────────────────────────────┐
│ Daemon                          │
│ 1. Reads all manifests         │
│ 2. Sends initial snapshot      │
│ 3. Streams manifest/event      │
│    notifications as events fire│
└──────┬─────────────────────────┘
       │ notifications on same socket
       ▼
┌────────────────────────────────┐
│ CLI adapter                     │
│ - Renders layer progress bars  │
│ - Updates in place (TTY)       │
│ - Handles SIGINT to unsubscribe│
└────────────────────────────────┘
```

### Gate interaction flow

```
During background evaluation, daemon emits:
  manifest/event {
    type: "gate-requested",
    featureId: "auth-feature",
    layer: "infrastructure",
    context: { evaluation summary, confidence, findings }
  }

Manifest state updated: status = pending_review

Later, user runs: pice review-gate auth-feature --layer infrastructure
        │
        ▼
┌───────────────────────┐
│ CLI                    │
│ 1. Fetch manifest      │
│ 2. Find pending gate   │
│ 3. Render gate prompt  │
│ 4. Read user decision  │
│ 5. Send RPC:           │
│    gate/decide         │
│    { decision }        │
└──────┬─────────────────┘
       │
       ▼
┌────────────────────────────┐
│ Daemon                      │
│ 1. Log decision to audit   │
│ 2. Update manifest status  │
│ 3. Resume evaluation:      │
│    - approve → continue    │
│    - reject  → retry/halt  │
│    - skip    → continue    │
│                  with warn │
└────────────────────────────┘
```

### Evaluation data flow (Tier 2 with seam checks)

```
User runs `pice evaluate plan.md`
        │
        ▼
Daemon orchestrates:

For each layer L:
  └── Parallel:
      ├── Claude provider (workflow + evaluation)
      │    ├── Contract grading (per criterion scores 1–10)
      │    └── Layer-specific checks
      │
      └── Codex provider (evaluation only)
           └── Design challenge (adversarial critique)

After both return:
  └── Seam checks for L:
      └── For each boundary (L↔adjacent):
          └── Run seam check modules
              (schema match, protocol validate, etc.)

Adaptive controller decides:
  ├── Confidence > target? → next layer
  ├── ADTS divergence? → extra pass or tier escalation
  ├── VEC entropy floor? → halt
  └── else → another pass

Result per layer: { passes: N, confidence: P, cost: $X, seam_findings: [...] }

Aggregation: feature is PASS iff ALL layers PASS and ALL seams clean.
```

---

## Key Design Decisions (v0.2)

1. **Layer detection is heuristic + manual override** — no pure algorithmic layer detection is viable across the full project ecosystem. The six-level stack (manifest → directory → framework → config → imports → `.pice/layers.toml`) captures 80% of projects automatically, and the override file wins for the rest. Human makes the architectural judgment call, PICE automates verification.
2. **File-level layer tagging handles fullstack frameworks** — a single `pages/api/users.ts` can belong to `api`, `frontend`, and `database` layers simultaneously. Each layer's contract applies its own evaluation lens. This is non-negotiable for Next.js/Remix/SvelteKit adoption.
3. **Seams travel with interface definitions, not components** — the Synopsys/Cadence VIP analogy. Seam checks are protocol-specific verification modules (REST, gRPC, GraphQL, Kafka, SQL). They live alongside the boundary definitions, not inside individual services. This means a shared seam check library can evolve independently of the code it verifies.
4. **The ~96.6% confidence ceiling is honored, not fought** — adaptive algorithms stop at the practical ceiling. v0.2 does not try to add more passes beyond the point where they stop helping. Instead, the roadmap pushes confidence higher in v0.4 (implicit contracts add orthogonal signal) and v0.5 (self-evolving verification reduces systematic error).
5. **Workflow YAML, not programmatic DSL** — teams can diff, review, and commit pipeline changes as plain YAML. Code-based workflow definitions (like GitHub Actions JavaScript, Airflow DAGs in Python) are rejected for v0.2 because they introduce a second runtime environment.
6. **Floor-based override semantics** — user-level `~/.pice/workflow.yaml` can only *restrict* what the project workflow allows. A user can raise confidence thresholds, lower budgets, or disable specific features. A user cannot lower tier gates, skip review gates, or relax contract criteria. Prevents an individual dev from locally bypassing team-wide guardrails.
7. **Worktree isolation for parallel execution, not containers** — git worktrees are ~200ms to create, require no Docker dependency, and share git objects (disk efficient). Containerized sandboxes are overkill for the "prevent file edit conflicts" use case.
8. **Gates live in the manifest, not in-process** — gates can fire during background execution, survive daemon restart, and be actioned from any adapter. The manifest is the durable gate store; the daemon is a cache + executor on top.
9. **Audit trail is write-only in SQLite** — every gate decision, every evaluation result, every model invocation is logged. Deletion is explicit (`pice audit prune --before DATE`). This supports compliance use cases and enables v0.5's self-evolving loop.
10. **Provider protocol extends, not breaks** — v0.1 providers still work, degraded to single-layer mode. The v0.2 additions (`layer`, `seamChecks`, `manifest/event`) are additive. This preserves the contribution boundary — a community provider built for v0.1 keeps working.
11. **Layer evaluator context isolation is preserved** — each layer evaluator sees only: this layer's contract, this layer's diff (filtered by file tagging), CLAUDE.md. It does NOT see other layers' contracts, other layers' implementations, or the cross-layer plan rationale. This prevents context contamination between layer evaluations and preserves the core anti-self-grading principle.
12. **Seam checks are cheap before they are smart** — v0.2 ships ~30 static seam checks (schema match, env var match, env var reference, JSON shape, OpenAPI compliance). The LLM-based seam analysis (v0.4 implicit contract inference) is layered on top, not underneath.

---

## Core Features (v0.2)

### Feature 1: Headless Daemon + CLI Adapter

**What it does.** Splits the v0.1 monolithic `pice` binary into a long-running daemon (`pice-daemon`) and a short-lived CLI adapter (`pice`). The daemon owns all evaluation state, orchestration, provider processes, and the SQLite store. The CLI becomes a thin client that submits requests and renders responses.

**Key behaviors.**
- `pice` auto-starts the daemon if not running. First-run latency ~400ms; subsequent commands ~50ms.
- Daemon listens on `~/.pice/daemon.sock` (Unix) or `\\.\pipe\pice-daemon` (Windows).
- Socket authentication via filesystem permissions (0600, owner-only) + a 32-byte random token in `~/.pice/daemon.token`.
- Daemon communicates over newline-delimited JSON-RPC 2.0. This is a SEPARATE protocol from the provider protocol — different method names, different scope, different consumers.
- `pice daemon start | stop | restart | status | logs` for lifecycle management.
- Daemon watches for config file changes and reloads where safe (workflow.yaml, layers.toml); reconnects are explicit.
- Graceful shutdown: daemon completes in-flight RPC, flushes manifest, closes sockets. SIGTERM → orderly shutdown within 10s or SIGKILL.
- `PICE_DAEMON_INLINE=1` bypasses the daemon and runs the orchestrator in-process for debugging; disables background mode and concurrent evaluations. CI can use this to simplify diagnosis of daemon-related failures.

**Daemon RPC methods (non-exhaustive).**

| Method | Purpose |
|--------|---------|
| `execute/create` | Start a new layer-aware execution run |
| `execute/status` | Get snapshot of a feature's execution state |
| `evaluate/create` | Start an evaluation run (optionally background) |
| `manifest/get` | Fetch full manifest for a feature |
| `manifest/list` | List all features with summary |
| `manifest/subscribe` | Stream live manifest events |
| `manifest/unsubscribe` | Stop streaming |
| `gate/list` | List pending gates across all features |
| `gate/decide` | Submit a gate decision |
| `worktree/list` | List active worktrees |
| `worktree/prune` | Remove worktrees for completed features |
| `daemon/health` | Lightweight health check |
| `daemon/shutdown` | Request orderly shutdown |
| `daemon/reload-config` | Re-read config files from disk |

**Edge cases.**
- Daemon hung or unresponsive → CLI detects via `daemon/health` timeout, attempts restart with warning. User override: `pice daemon restart --force`.
- Daemon crashed mid-evaluation → manifest state survives; on restart, daemon resumes active manifests from their last checkpoint (or marks them as `failed-interrupted` if mid-provider-call).
- Socket permissions wrong (e.g., shared by root) → CLI refuses to connect, prints how to fix.
- Two daemons running → second daemon fails to bind socket, exits with clear error.
- Stale socket file after unclean shutdown → daemon detects with `connect()` test, removes, recreates.

**Acceptance criteria.**
- [ ] `pice daemon start` launches the daemon and returns when the socket is listening
- [ ] First `pice` command after reboot auto-starts the daemon in <500ms
- [ ] Subsequent `pice` commands reach the daemon in <50ms
- [ ] Daemon survives CLI crashes without losing active evaluation state
- [ ] Daemon RPC errors surface back to CLI with context preservation
- [ ] `pice daemon logs --follow` streams daemon logs
- [ ] `PICE_DAEMON_INLINE=1` runs the full pipeline without a daemon (for debugging and CI)
- [ ] Windows implementation uses named pipes and passes the same acceptance tests
- [ ] Stale socket files are detected and cleaned up automatically

---

### Feature 2: Verification Manifest (Persistent Evaluation State)

**What it does.** Introduces a durable, structured state file that represents the complete state of an active or completed evaluation. The manifest is the source of truth — the daemon reads and writes it; all adapters observe it.

**Manifest location.** `~/.pice/state/{feature-id}.manifest.json` (one file per feature, namespaced by project via `project_root_hash`).

**Manifest schema (abbreviated).**

```jsonc
{
  "schema_version": "0.2",
  "feature_id": "auth-feature-20260410-a3b2",
  "project_root": "/Users/jacobmolz/code/my-app",
  "project_root_hash": "sha256:...",
  "plan_path": ".claude/plans/auth-plan.md",
  "workflow_snapshot": { /* parsed workflow.yaml at run start */ },
  "layers": [
    {
      "name": "backend",
      "status": "passed",
      "worktree_path": ".pice/worktrees/auth-feature-20260410-a3b2/backend",
      "contract_tier": 2,
      "passes": [
        {
          "index": 1,
          "model": "claude-opus-4-6",
          "confidence": 0.88,
          "score": 8.2,
          "cost_usd": 0.04,
          "timestamp": "2026-04-10T10:23:11Z",
          "findings": [ /* ... */ ]
        },
        { /* pass 2, 3, ... */ }
      ],
      "seam_checks": [
        {
          "name": "backend↔database: schema match",
          "status": "pass",
          "details": "Migration matches ORM schema"
        }
      ],
      "halted_by": "sprt_confidence_reached",
      "final_confidence": 0.951,
      "total_cost_usd": 0.12
    },
    { /* other layers */ }
  ],
  "gates": [
    {
      "layer": "infrastructure",
      "status": "pending_review",
      "triggered_by": "trigger: layer == infrastructure",
      "summary": { /* human-readable context */ },
      "requested_at": "2026-04-10T10:31:02Z",
      "timeout_at": "2026-04-11T10:31:02Z",
      "decision": null
    }
  ],
  "overall_status": "pending_review",
  "created_at": "...",
  "last_event_at": "..."
}
```

**Key behaviors.**
- Manifest is append-only for history (passes, seam check results, gate decisions); current-state fields are updated in place.
- Writes are atomic (write to `.tmp` + rename).
- `~/.pice/state/` is namespaced by `project_root_hash` to avoid collisions when developers run PICE across multiple projects from the same home directory.
- Schema is versioned; daemon refuses to read incompatible versions and suggests upgrade.
- Manifest events are broadcast to all subscribed adapter sessions via the daemon socket.
- `pice status --json` emits the manifest directly for scripting.

**Edge cases.**
- Daemon crash during write → atomic rename prevents torn files
- Concurrent writes from multiple daemon tasks → single-writer-per-manifest enforced by daemon's internal lock map
- Manifest corruption → daemon quarantines to `.corrupted` and alerts
- Cross-project manifest contamination → `project_root_hash` mismatch rejects loads with clear error
- Disk full during manifest write → daemon marks the evaluation as `failed-io` and halts cleanly

**Acceptance criteria.**
- [ ] Manifests survive daemon restart and resume correctly
- [ ] Schema version mismatch is detected and reported
- [ ] `pice status --json` emits the full manifest
- [ ] Multiple concurrent evaluations maintain isolated manifests
- [ ] Events streamed to subscribers match what is written to disk
- [ ] Stale manifests (feature never finished, last event >30 days ago) are auto-pruned on `pice clean`

---

### Feature 3: Layer Detection + `.pice/layers.toml`

**What it does.** Implements the six-level heuristic layer detection stack: manifest → directory → framework → config → imports → override. Produces a proposed `.pice/layers.toml` on `pice init --upgrade` or `pice layers detect`. Developer reviews and commits.

**Detection stack (in order of authority, later levels override earlier).**

1. **Manifest files** — `package.json`, `pyproject.toml`, `Cargo.toml`, `go.mod`, `Gemfile` → runtime, framework, dependencies
2. **Directory patterns** — `app/`, `api/`, `infra/`, `deploy/`, `src/server/`, `src/client/`, `terraform/`, `helm/`
3. **Framework signals** — Next.js `app/` routes = frontend + API, Prisma schema = database, FastAPI = API + backend, Rails `app/controllers/` = API + backend
4. **Config files** — `Dockerfile`, `docker-compose.yml`, `terraform/`, `.github/workflows/`, `vercel.json`
5. **Import graph** — static analysis (`rust-analyzer`, `tsc`, `mypy` derived). Walks imports to classify which files depend on which, inferring architectural clusters.
6. **Override file** — `.pice/layers.toml` always wins. If present, detection is skipped and the file is used as-is.

**Fullstack-in-one handling.** Files can belong to multiple layers via glob pattern overlap. Example: `pages/api/users.ts` matches `api.paths = ["pages/api/**"]`, `frontend.paths = ["pages/**"]`, and `database.paths = ["**/*.ts[x]?"]` if it imports `@prisma/client`. The import-graph level adds database membership based on Prisma import detection.

**Monorepo handling.** Multi-service monorepos → each service is a stack. `.pice/layers.toml` supports `[stacks.{service}]` sections for multi-stack projects. Shared libraries are cross-stack dependencies with their own seam checks.

**Monorepo tools integration.** If `nx.json`, `turbo.json`, or `pnpm-workspace.yaml` are detected, the detector uses their project graphs as input rather than re-computing from scratch.

**Polyrepo — deferred to v0.4.** v0.2 operates within a single repository. `.pice/external-contracts.toml` supports manual declaration of external service contracts.

**Commands.**

| Command | Purpose |
|---------|---------|
| `pice layers detect` | Run detection, print proposed layers.toml to stdout |
| `pice layers detect --write` | Run detection, write to `.pice/layers.toml` (refuses to overwrite without `--force`) |
| `pice layers list` | Print current layer configuration with path counts and file examples |
| `pice layers check` | Warn about unlayered files (files not matching any layer's paths) |
| `pice layers graph` | ASCII diagram of layer dependencies |

**`.pice/layers.toml` format.**

```toml
[layers]
order = ["backend", "database", "api", "frontend", "infrastructure", "deployment", "observability"]

[layers.backend]
paths = ["src/server/**", "lib/**"]
always_run = false
contract = ".pice/contracts/backend.toml"

[layers.database]
paths = ["prisma/**", "migrations/**", "src/models/**"]
always_run = false
contract = ".pice/contracts/database.toml"
depends_on = []

[layers.api]
paths = ["src/server/routes/**", "pages/api/**"]
always_run = false
contract = ".pice/contracts/api.toml"
depends_on = ["backend", "database"]

[layers.frontend]
paths = ["pages/**", "src/client/**", "app/**"]
always_run = false
contract = ".pice/contracts/frontend.toml"
depends_on = ["api"]

[layers.infrastructure]
paths = ["terraform/**", "Dockerfile", "docker-compose.yml"]
always_run = true
type = "meta"
contract = ".pice/contracts/infrastructure.toml"

[layers.deployment]
paths = [".github/workflows/**", "deploy/**", "vercel.json"]
always_run = true
depends_on = ["infrastructure"]
environment_variants = ["staging", "production"]
contract = ".pice/contracts/deployment.toml"

[layers.observability]
paths = ["monitoring/**", "otel/**", "prometheus/**", "grafana/**"]
always_run = true
depends_on = ["deployment"]
contract = ".pice/contracts/observability.toml"

[external_contracts]
# Polyrepo workaround (v0.2)
api_gateway = { spec = "https://api.example.com/openapi.json", type = "openapi" }
```

**Acceptance criteria.**
- [ ] Detection produces a non-empty layers.toml for all framework templates (Next.js, FastAPI, Rails, Express, SvelteKit, Remix)
- [ ] Fullstack-in-one files appear in multiple layers when framework signals apply
- [ ] `pice layers check` reports unlayered files with suggestions
- [ ] `pice layers graph` renders correctly for cycle-free DAGs (and reports cycles as errors)
- [ ] Override file wins detection in all test cases
- [ ] Monorepo detection identifies multiple stacks correctly for Nx and Turborepo fixtures

---

### Feature 4: `.pice/workflow.yaml` — Committable Evaluation Pipeline

**What it does.** Introduces a committable pipeline definition that codifies which tiers run, when parallelism applies, where review gates fire, how retries work, and what budgets apply. Teams commit `workflow.yaml` to the repo; every team member runs the same pipeline.

**Schema (`schema_version: "0.2"`).**

```yaml
schema_version: "0.2"

defaults:
  tier: 2
  min_confidence: 0.90
  max_passes: 5
  model: sonnet
  budget_usd: 2.00
  cost_cap_behavior: halt  # halt | warn | continue

phases:
  plan:
    description: "Generate layer-aware plan from feature request"
    output: .claude/plans/{feature-id}.md

  execute:
    description: "Implement per layer in dependency order"
    parallel: true
    worktree_isolation: true
    retry:
      max_attempts: 3
      fresh_context: true

  evaluate:
    description: "Evaluate per layer with seam checks"
    parallel: true
    seam_checks: true
    adaptive_algorithm: bayesian_sprt  # bayesian_sprt | adts | vec | none
    model_override:
      infrastructure: opus
      frontend: haiku

  review:
    enabled: true
    trigger: "tier >= 3 OR layer == infrastructure OR layer == deployment"
    timeout_hours: 24
    on_timeout: reject  # reject | approve | skip
    notification: stdout

layer_overrides:
  infrastructure:
    tier: 3
    min_confidence: 0.95
    require_review: true
    max_passes: 7

  frontend:
    tier: 1
    max_passes: 2

  deployment:
    tier: 3
    min_confidence: 0.97
    require_review: true
    budget_usd: 4.00
```

**Inheritance.**
1. **Framework defaults** (shipped embedded in the binary) — sensible baseline for every phase
2. **Project workflow** (`.pice/workflow.yaml`, committed) — team-wide overrides
3. **User workflow** (`~/.pice/workflow.yaml`, NOT committed) — personal preferences

**Floor-based merge semantics.** User overrides can only *restrict* project defaults:
- ✅ User can raise `min_confidence` from 0.90 to 0.95
- ✅ User can lower `budget_usd` from $2.00 to $1.00
- ✅ User can raise `tier` from 2 to 3
- ✅ User can enable `require_review` that was false
- ❌ User cannot lower `tier` below project setting
- ❌ User cannot disable a review gate the project requires
- ❌ User cannot lower `min_confidence` below project setting
- ❌ User cannot raise `budget_usd` above project setting

Violations are reported at workflow load time with a clear error showing the specific field and the project floor.

**Validation.** `pice validate` checks:
- Schema compliance (required fields, type correctness)
- Trigger expression parsing (see below)
- Cross-reference integrity (layer_overrides point to layers that exist in layers.toml)
- Floor violations in user overrides
- Model names valid for the configured provider

**Trigger expression grammar.**
```
expression := term ( ('AND' | 'OR') term )*
term       := 'NOT'? primary
primary    := comparison | grouped | literal
comparison := identifier ( '==' | '>=' | '<=' | '>' | '<' | '!=' ) value
grouped    := '(' expression ')'
literal    := 'true' | 'false' | 'always'
identifier := 'tier' | 'layer' | 'confidence' | 'cost' | 'passes' | 'change_scope'
value      := integer | float | string
```

Examples:
- `tier >= 3`
- `layer == infrastructure OR layer == deployment`
- `confidence < 0.95 AND tier >= 2`
- `NOT (change_scope == css_only)`

**Edge cases.**
- Malformed YAML → `pice validate` reports line + column, refuses to start evaluation
- Unknown model name → validator queries provider capabilities and reports mismatch
- Layer override references nonexistent layer → hard error
- User workflow violates floor → hard error with specific diff
- Schema version mismatch → upgrade instructions emitted

**Acceptance criteria.**
- [ ] Default workflow ships with framework
- [ ] Project overrides take effect
- [ ] User overrides take effect when they respect the floor
- [ ] Floor violations are rejected with clear error
- [ ] `pice validate` catches schema errors, trigger syntax errors, reference errors
- [ ] Layer-level overrides propagate to evaluation correctly
- [ ] Parallel/retry/review settings are respected by the orchestrator

---

### Feature 5: Stack Loops Orchestration

**What it does.** Runs per-layer PICE loops in dependency order, with independent layers executing in parallel via worktree isolation. A feature passes only when all layers pass.

**Execution model.**

1. **Parse plan** → extract layer list, per-layer contracts, dependency edges
2. **Build DAG** from `layers.toml` + plan-declared dependencies
3. **Topological layer cohorts** — group layers with no pending dependencies
4. **For each cohort (sequentially)**:
   - For each layer in cohort (parallel up to `workflow.defaults.max_parallelism`, default = number of CPU cores):
     - Create git worktree
     - Spawn provider session (`session/create` with `layer` and `workingDirectory`)
     - Stream the `execute` phase
     - Run evaluation passes (adaptive loop — see Feature 7)
     - Run seam checks (see Feature 6)
     - Update manifest
5. **After each cohort**: merge passing worktrees, abort failing cohorts if retry policy allows, halt on unrecoverable failures.

**Parallelism rules.**
- Layers with no dependency edge between them run in parallel worktrees
- Layers with dependency edges wait for all upstream layers to pass
- `max_parallelism` is configurable (default = CPU count)
- Cost budgets are enforced per-layer AND globally (global cap stops spawning new parallel work)

**Retry policy.**
- `retry.max_attempts` per layer
- `retry.fresh_context: true` (default) destroys and recreates provider session between attempts — prevents evaluator contamination from the failed attempt
- Retries consume the layer's budget
- Exceeding max_attempts → layer marked failed → feature halts unless workflow allows proceeding

**Layer-scoped context isolation.** Each layer's evaluator sees only:
- The layer's contract
- The git diff filtered to the layer's tagged files
- `CLAUDE.md` (unchanged)

It does NOT see other layers' contracts, other layers' diffs, other layers' findings, or the cross-layer plan rationale. This is a hard rule — violating it recreates the self-grading bias that PICE was built to eliminate.

**Acceptance criteria.**
- [ ] Feature with 7 layers, dependency graph as shown in roadmap, completes end-to-end
- [ ] Parallel cohorts execute concurrently; sequential chains wait appropriately
- [ ] Worktree per parallel layer; cleanup on pass, preservation on fail when configured
- [ ] Layer context isolation is enforced — evaluator prompts do not leak cross-layer content (verified by test harness)
- [ ] Retry with fresh context works (evaluator has no memory of previous attempt)
- [ ] Budget enforcement halts new work when global cap is reached
- [ ] Layer failure propagation respects `always_run` (e.g., infrastructure still runs even when upstream application layers fail, unless explicitly halted)

---

### Feature 6: Seam Verification

**What it does.** Adds a seam verification pass after each layer's contract grading. Seam checks target the 12 empirically validated failure categories from the Seam Blindspot research. Each layer boundary in the DAG has associated seam check modules.

**The twelve failure categories (from research, verbatim).**

| # | Category | Empirical source | Example check |
|---|----------|------------------|---------------|
| 1 | Configuration / deployment mismatches | Google SRE: 31% of outage triggers | Env var declared in app but missing from deploy config |
| 2 | Binary / version incompatibilities | Google SRE: 37% of outage triggers | Producer schema version ≠ consumer schema version |
| 3 | Protocol / API contract violations | Adyen: 60K+ daily errors | OpenAPI spec vs. actual handler response shape |
| 4 | Authentication handoff failures | 4.55% of microservice issues | JWT secret propagation from infra → app layer |
| 5 | Cascading failures from dependencies | AWS 2025: 14hr cascade | Retry budget × timeout breach of downstream capacity |
| 6 | Retry storm / timeout conflicts | Netflix / Amazon / Uber docs | Upstream retries × downstream timeout > capacity |
| 7 | Service discovery failures | ICST 2025: >50% of practitioners | DNS name in config not matching deployed service |
| 8 | Health check blind spots | AWS 2021: self-masking outage | Health endpoint doesn't reflect dependency health |
| 9 | Serialization / schema drift | — | Nullable field always populated in practice |
| 10 | Cold start / ordering dependencies | — | Service B startup before Service A warm |
| 11 | Network topology assumptions | Deutsch's Fallacies (1994) | Code assumes single AZ; deploy uses multi-AZ |
| 12 | Resource exhaustion at boundaries | — | Thread pool exhaustion from slow downstream |

**Seam check types (v0.2 ships ~30 static checks).**

- **Static structural checks** (fast, deterministic): schema diff, env var cross-reference, OpenAPI compliance, Dockerfile ENV vs. app env reads, SQL migration vs. ORM schema, TypeScript type vs. API response shape
- **Config cross-reference checks**: Terraform outputs vs. application inputs, Docker Compose service names vs. app connection strings, Kubernetes service name vs. DNS references
- **Protocol validation checks**: gRPC proto compatibility, GraphQL schema diff, REST OpenAPI breaking change detection (via Buf-style rules)
- **Behavioral check stubs** (placeholders for v0.4 implicit contract inference): "did anyone call this endpoint with a null field in production?" — v0.2 emits TODO seam findings; v0.4 populates them

**Seam check extensibility.** Seam checks are a plugin point. A check is defined by a `SeamCheckSpec`:

```rust
pub trait SeamCheck {
    fn id(&self) -> &str;
    fn applies_to(&self, boundary: &LayerBoundary) -> bool;
    fn run(&self, ctx: &SeamContext) -> SeamResult;
}
```

Community checks can ship in a new crate `pice-seam-checks-{protocol}` (e.g., `pice-seam-checks-grpc`). Checks are discovered at daemon startup via a registry pattern.

**Per-boundary checks in layers.toml.**

```toml
[seams]
"backend↔database" = ["schema_match", "migration_ordering"]
"api↔frontend"     = ["response_shape", "openapi_compliance"]
"infra↔deploy"     = ["env_var_match", "secret_propagation"]
"app↔observability" = ["metric_name_match", "log_field_schema"]
```

**Acceptance criteria.**
- [ ] All 12 failure categories have at least one default seam check in v0.2
- [ ] Seam checks run after contract grading per layer
- [ ] Seam findings appear in manifest and evaluation output
- [ ] Seam check plugin crate pattern works (one community check is builable from the docs)
- [ ] Missing env var between infra and app is detected (end-to-end test)
- [ ] ORM schema vs. migration drift is detected (end-to-end test)
- [ ] OpenAPI spec vs. handler response shape mismatch is detected

---

### Feature 7: Adaptive Evaluation (Bayesian-SPRT, ADTS, VEC)

**What it does.** Replaces v0.1's fixed-per-tier pass count with adaptive allocation that halts as soon as evidence is sufficient. Grounded in the correlated Condorcet Jury Theorem and Chernoff bound analysis.

**The confidence ceiling (from research).**

| Passes | Effective N | Estimated confidence | Marginal gain | Cumulative improvement |
|--------|-------------|----------------------|---------------|-----------------------|
| 1      | 1.00        | 88.0%                | —             | 0% |
| 2      | 1.48        | 92.1%                | +4.1%         | 48% |
| 3      | 1.87        | 94.0%                | +1.9%         | 70% |
| 4      | 2.09        | 94.9%                | +0.9%         | 80% |
| 5      | 2.27        | 95.4%                | +0.5%         | 86% |
| 7      | 2.50        | 95.9%                | +0.25% avg    | 92% |
| 10     | 2.63        | 96.2%                | +0.10% avg    | 95% |
| 20     | 2.80        | 96.5%                | +0.03% avg    | 99% |
| ∞      | 2.86        | ~96.6%               | 0             | 100% (ceiling) |

Assumptions: `p = 0.88` per evaluator, `ρ = 0.35` inter-model correlation.
Source: [`docs/research/convergence-analysis.md`](docs/research/convergence-analysis.md)

**Practical implication**: 3 passes captures 70% of total achievable improvement. 5 passes captures 86%. Beyond 5, marginal gains < 0.5% per pass. v0.2 honors this floor and does not pretend more passes can breach it.

**Three algorithms (all ship in `pice-core::adaptive`).**

#### 7.1 — Bayesian-SPRT (default)

Sequential Probability Ratio Test with Bayesian posterior updates.

```
Maintain prior Beta(α, β) over "contract is met"
For each pass n:
  observe pass_result, update posterior
  posterior = Beta(α + successes, β + failures)
  likelihood_ratio = P(observations | H1) / P(observations | H0)
  if likelihood_ratio > A (accept) → halt, feature PASS
  if likelihood_ratio < B (reject) → halt, feature FAIL
  if n >= max_passes → halt, return current posterior
```

Where `A` and `B` are chosen based on the target false-positive and false-negative rates from workflow config (`min_confidence` maps to `A`).

#### 7.2 — ADTS (Adversarial Divergence-Triggered Scaling)

When Claude and Codex scoring diverge beyond threshold, escalate:

```
For each pass:
  claude_score = claude provider result
  codex_score  = codex provider result
  divergence = |claude_score - codex_score|

  if divergence > 2.0 (on 0–10 scale):
    → schedule an extra pass with a fresh context
    → if still divergent, escalate to next tier (Tier 2 → Tier 3)
    → if still divergent at Tier 3, request human review gate
```

Catches cases where "both models are confident but disagree" — a strong signal that the contract is underdetermined or the code has a subtle issue one model missed.

#### 7.3 — VEC (Verification Entropy Convergence)

Track the per-pass entropy of the posterior distribution. Halt when marginal entropy drops below a floor:

```
For each pass n:
  H_n = entropy(posterior after n passes)
  ΔH = H_{n-1} - H_n
  if ΔH < entropy_floor (default 0.01 bits) → halt
```

Complements SPRT when the posterior is neither strongly accepted nor rejected — stops the loop when additional passes are no longer reducing uncertainty.

**Adaptive algorithm selection.** `workflow.yaml` specifies which algorithm per phase. Default is `bayesian_sprt`. Teams can mix (e.g., SPRT for evaluate, ADTS for review gate triggering).

**Cost integration.** Every pass writes `cost_usd` to the manifest. The adaptive loop respects `budget_usd` cap — if next pass would exceed budget, halt with a warning regardless of confidence.

**Configuration surface (workflow.yaml).**

```yaml
evaluate:
  adaptive_algorithm: bayesian_sprt
  sprt:
    prior_alpha: 1.0
    prior_beta: 1.0
    accept_threshold: 19.0  # A: accept when LR > 19 (~95% confidence)
    reject_threshold: 0.053 # B: reject when LR < 1/19
  adts:
    divergence_threshold: 2.0
    max_divergence_escalations: 2
  vec:
    entropy_floor: 0.01
```

**Acceptance criteria.**
- [ ] SPRT halts on high-confidence layers before `max_passes`
- [ ] SPRT respects `min_confidence` target
- [ ] ADTS detects cross-model divergence and escalates
- [ ] VEC halts when posterior stabilizes
- [ ] Budget cap halts regardless of confidence state
- [ ] Adaptive output shows "halted by: sprt_confidence_reached / budget / max_passes / vec_entropy"
- [ ] Evaluation confidence never claimed above ~96.6% for dual-model correlated evaluators (validated by test)

---

### Feature 8: Worktree Isolation for Parallel Layers

**What it does.** Creates a git worktree per parallel layer so concurrent subagents don't conflict on file edits. Worktrees are created by the daemon, passed to provider sessions as `workingDirectory`, cleaned up after the layer passes (or preserved after failure, configurable).

**Lifecycle.**

```
1. Daemon determines parallel cohort from DAG
2. For each layer in cohort:
   a. git worktree add .pice/worktrees/{feature-id}/{layer} HEAD
   b. Spawn provider session with workingDirectory = worktree path
   c. Provider runs execute + evaluate inside worktree
3. On layer PASS:
   a. Merge worktree changes back to main worktree (squash commit or apply as working-dir changes)
   b. git worktree remove
4. On layer FAIL:
   a. If retry attempts remain: reset worktree, retry
   b. If no retries and preserve_failed_worktrees = true: keep worktree, note path in manifest
   c. If no retries and preserve_failed_worktrees = false: git worktree remove
```

**Merge strategy options (workflow.yaml).**

```yaml
execute:
  worktree_isolation: true
  merge_strategy: apply_to_main  # apply_to_main | squash_commit | branch_per_layer
  conflict_resolution: halt      # halt | abort_layer | manual_review
```

- `apply_to_main` (default) — applies worktree changes as unstaged working directory edits in the main worktree; user commits later
- `squash_commit` — each layer's changes become a squashed commit on a branch
- `branch_per_layer` — each layer gets its own branch off the feature branch; final assembly is user's responsibility

**Evaluation-only mode.** When running `pice evaluate` (not `pice execute`), subagents are read-only (`tools: [Read, Grep, Glob]`). Read-only agents cannot conflict, so worktrees are NOT created. Use `--isolate` to force worktree creation for evaluation too (e.g., when running experimental seam checks that write to the filesystem).

**Commands.**

| Command | Purpose |
|---------|---------|
| `pice worktree list` | Show active worktrees and their layer mapping |
| `pice worktree prune` | Remove worktrees for completed features |
| `pice clean` | Remove all worktrees for abandoned evaluations |

**Acceptance criteria.**
- [ ] Parallel layers run in separate worktrees
- [ ] Sequential layers reuse the main worktree
- [ ] Passing worktrees merge back cleanly
- [ ] Failed worktrees preserved when configured
- [ ] `pice clean` removes all evaluation worktrees
- [ ] `pice evaluate` (read-only) does not create worktrees by default
- [ ] `--isolate` flag creates worktrees for evaluation-only runs
- [ ] Merge conflicts halt the cohort with clear error

---

### Feature 9: Human-in-the-Loop Approval Gates

**What it does.** Adds configurable pause points where a human reviews before the evaluation proceeds. Gates fire based on trigger conditions in `workflow.yaml` and operate in both foreground and background mode.

**Gate lifecycle (state machine).**

```
[running] → evaluation proceeds
    │
    ▼  (trigger condition met)
[gate_requested] → manifest updated, event fired
    │
    ├─ foreground mode: CLI prompts immediately
    ├─ background mode: user runs `pice review-gate {feature} --layer {layer}`
    │
    ▼  (user decision)
[decision_recorded] → audit trail written
    │
    ├─ approve → [running] (next layer)
    ├─ reject  → [retrying] or [failed]
    ├─ skip    → [running] with warning
    └─ timeout → configured behavior (reject/approve/skip)
```

**Foreground gate prompt.**

```
╔═══════════════════════════════════════════════════════════╗
║  REVIEW GATE: Infrastructure layer                        ║
╠═══════════════════════════════════════════════════════════╣
║  Evaluation: PASS at 95.1% confidence (4 passes, $0.12)  ║
║  Seam checks: 3/3 verified                                ║
║                                                           ║
║  Findings:                                                ║
║  • JWT_SECRET added to RunPod env config                  ║
║  • Docker image builds with new auth dependencies         ║
║  • Cold start estimated at 3.2s (under 5s threshold)      ║
║                                                           ║
║  [a]pprove  [r]eject  [d]etails  [s]kip                  ║
╚═══════════════════════════════════════════════════════════╝
```

**Background gate deferral.**

```
$ pice evaluate plan.md --background
→ Evaluation started: auth-feature (7 layers, Tier 2)
  Run `pice status` to monitor progress

$ pice status
auth-feature   5/7 layers  ⏳ pending review: infrastructure

$ pice review-gate auth-feature --layer infrastructure
# Shows the same prompt as foreground mode

$ pice review-gate --list  # List all pending gates across all features
```

**Trigger expression (reuses workflow.yaml grammar).**

```yaml
review:
  enabled: true
  trigger: "tier >= 3 OR layer == infrastructure OR (layer == api AND confidence < 0.95)"
  timeout_hours: 24
  on_timeout: reject
```

**Audit trail.**

Every gate decision writes a row to SQLite:

```sql
CREATE TABLE gate_decisions (
    id INTEGER PRIMARY KEY,
    feature_id TEXT NOT NULL,
    layer TEXT NOT NULL,
    trigger_expression TEXT NOT NULL,
    decision TEXT NOT NULL,           -- approve | reject | skip | timeout_*
    reviewer TEXT,                    -- $USER by default
    reason TEXT,                      -- optional free text
    requested_at TEXT NOT NULL,
    decided_at TEXT NOT NULL,
    elapsed_seconds INTEGER NOT NULL
);
```

`pice audit gates [--feature F] [--since DATE]` exports the audit trail.

**Acceptance criteria.**
- [ ] Gate triggers match configured expression (all grammar operators tested)
- [ ] Foreground gate prompt accepts all four actions
- [ ] Background gate is actionable via `pice review-gate`
- [ ] Timeout behavior is configurable and enforced
- [ ] Audit trail is written for every decision including timeouts
- [ ] `pice status` shows pending gates
- [ ] Rejecting a gate with remaining retries triggers retry; without retries, halts the feature

---

### Feature 10: Background Execution + Status Monitoring

**What it does.** Enables `pice evaluate --background` and `pice execute --background` to return immediately, with the daemon running the evaluation asynchronously. Provides `pice status` and `pice logs` for monitoring.

**Command surface.**

```bash
# Fire and forget
pice evaluate plan.md --background
# Returns immediately with feature-id

# Monitor
pice status                       # Summary of all features
pice status auth-feature          # Detail for one feature
pice status --follow              # Live-updating status (streaming)
pice status --wait auth-feature   # Block until feature completes (for CI)

# Logs
pice logs auth-feature            # Print completed log output
pice logs auth-feature --follow   # Stream live (tail -f style)
pice logs auth-feature --layer backend  # Scoped to one layer
```

**`pice status` output (example).**

```
Feature: auth-feature                  Started 14:23   $0.34  Tier 2
████████████████░░░░░░░  5/7 layers   94.2% avg confidence

✅ backend        (4 passes, $0.08, 94.1%)
✅ database       (3 passes, $0.06, 93.8%)
✅ api            (5 passes, $0.11, 95.4%)
✅ frontend       (2 passes, $0.03, 91.2%)
⏸  infrastructure (4 passes, $0.06, 95.1%)  ← pending review
⬜ deployment     (pending)
⬜ observability  (pending)

Run `pice review-gate auth-feature --layer infrastructure` to action.
```

**`pice status --follow` behavior.** Uses a TTY live-update mode (erase + rewrite) when stdout is a terminal; falls back to append-only mode when piped. Updates triggered by `manifest/event` notifications from the daemon.

**Notifications on completion.**

- **macOS**: `osascript -e 'display notification ...'`
- **Linux**: `notify-send` if available, fallback to bell char + stdout
- **Windows**: Windows Toast via `PowerShell` or fallback to bell char + stdout
- **Configuration**:
  ```toml
  [notifications]
  on_complete = "terminal"  # terminal | none
  on_gate     = "terminal"
  on_failure  = "terminal"
  ```
- Future: `slack`, `webhook` — scaffolded but out of scope for v0.2

**Multiple concurrent evaluations.** The daemon runs N evaluations concurrently, each with its own manifest and worktree tree. `pice status` lists them all. Resource contention (provider rate limits, disk I/O) is managed by a global semaphore tuned by `workflow.defaults.max_parallelism`.

**CI mode.** `pice evaluate --background --wait` is equivalent to a synchronous run for scripting purposes — returns 0 on success, 2 on contract failure, 1 on system error. `--wait` blocks until manifest is in a terminal state.

**Acceptance criteria.**
- [ ] `pice evaluate --background` returns within 500ms of dispatch
- [ ] `pice status` accurately reflects manifest state
- [ ] `pice status --follow` updates live as events stream
- [ ] `pice logs --follow` streams real time
- [ ] Desktop notifications fire on completion (macOS, Linux verified; Windows best-effort)
- [ ] Multiple concurrent background evaluations don't conflict
- [ ] `pice evaluate --background --wait` has the same exit codes as synchronous mode

---

## Provider Protocol Evolution (v0.2)

The v0.1 provider protocol (`pice-protocol` + `@pice/provider-protocol`) is extended, not replaced. v0.1 providers continue to work, degraded to single-layer mode.

### Additions

**Extended `initialize` capabilities.**

```jsonc
{
  "capabilities": {
    "workflow": true,
    "evaluation": true,
    "agentTeams": true,
    "layerAware": true,           // v0.2: understands layer-scoped sessions
    "seamChecks": ["schema_match", "openapi_compliance"], // v0.2: supported seam check IDs
    "models": ["claude-opus-4-6", "claude-sonnet-4-6"],
    "defaultEvalModel": "claude-opus-4-6"
  }
}
```

**Extended `session/create`.**

```jsonc
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "session/create",
  "params": {
    "workingDirectory": "/abs/path/to/worktree",   // v0.2: worktree, not project root
    "layer": "backend",                              // v0.2: layer name
    "layerPaths": ["src/server/**", "lib/**"],      // v0.2: scoped file set
    "contractPath": ".pice/contracts/backend.toml" // v0.2: layer contract
  }
}
```

**Extended `evaluate/create`.**

```jsonc
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "evaluate/create",
  "params": {
    "layer": "backend",
    "contract": { /* parsed contract */ },
    "diff": "...",
    "seamChecks": [                                  // v0.2: seam check specs
      {
        "id": "schema_match",
        "boundary": "backend↔database",
        "config": { /* ... */ }
      }
    ]
  }
}
```

**New notification: `manifest/event`.**

```jsonc
{
  "jsonrpc": "2.0",
  "method": "manifest/event",
  "params": {
    "featureId": "auth-feature-20260410",
    "eventType": "layer_pass_complete",  // layer_started, pass_complete, confidence_updated, seam_finding, gate_requested, layer_complete
    "layer": "backend",
    "data": { /* event-specific payload */ },
    "timestamp": "2026-04-10T10:23:11Z"
  }
}
```

Providers emit events; daemon aggregates and broadcasts to subscribers. Enables the dashboard (v0.3) and `pice status --follow` (v0.2).

**New method: `layer/detect` (optional provider-side hint).**

Provider can contribute framework-specific layer detection heuristics (e.g., a Ruby on Rails provider could declare `app/controllers/` = API layer). Used by `pice layers detect` in addition to the core detector.

### Backwards compatibility

- A v0.1 provider that does NOT declare `layerAware: true` is driven by the daemon in a "single virtual layer" mode — the feature runs as one big layer covering all files. Seam checks are skipped.
- A v0.1 provider receives `session/create` params without `layer` / `layerPaths` / `contractPath`; it works the same way it did before.
- A v0.1 provider does not emit `manifest/event` notifications; the daemon synthesizes events from command boundaries (start, end, error).

This keeps the contribution surface stable and means early community providers don't have to chase v0.2 changes immediately.

---

## Success Criteria (v0.2)

### Functional

- [ ] `pice init --upgrade` on a v0.1 project produces a working `.pice/layers.toml` + `.pice/workflow.yaml`
- [ ] 7-layer Stack Loop (`backend` → `database` → `api` → `frontend` → `infrastructure` → `deployment` → `observability`) executes end-to-end on reference projects
- [ ] Parallel layers run concurrently in isolated worktrees; sequential layers respect dependency order
- [ ] Adaptive SPRT halts layers at configured confidence target; confidence reporting never exceeds the ~96.6% ceiling
- [ ] Seam checks detect all 12 failure categories on the reference test fixtures (at least one test per category)
- [ ] Review gates fire on configured triggers in both foreground and background mode
- [ ] `pice evaluate --background` returns in <500ms; `pice status --follow` streams live
- [ ] Headless daemon architecture: CLI + daemon split is complete, Windows named pipe parity verified
- [ ] Graceful degradation: v0.1 providers still work (single-layer mode)
- [ ] Graceful degradation: adversarial provider missing still completes (single-model eval with warning)
- [ ] Floor-based workflow.yaml override semantics enforced at load time
- [ ] Audit trail persists every gate decision

### Quantitative targets

| Metric | Target | Why |
|--------|--------|-----|
| Daemon cold start (auto-start) | < 500 ms | Matches v0.1's sub-100ms perceived startup once warm |
| Warm CLI command latency | < 50 ms | Fast enough to feel like a local binary, not a network call |
| Worktree creation | < 300 ms | From research: ~200ms observed |
| 7-layer Tier 2 evaluation (cached warm providers) | < 5 min | Reference project with typical layer sizes |
| Parallel speedup on 2-layer cohort | ≥ 1.6× sequential | Accounts for provider spawn + worktree overhead |
| Manifest write latency | < 20 ms | Atomic rename + fsync |
| Gate decision round-trip (background → CLI → decision → resume) | < 1 s | Excluding human thinking time |
| SPRT halt latency after threshold reached | < 1 pass (immediate on next tick) | No wasted passes after decision is clear |
| Cost reduction from adaptive pass allocation | ≥ 30% vs. fixed max_passes | Target based on high-confidence layer halting early |

### Qualitative

- [ ] A team can commit `.pice/workflow.yaml` and every member runs the same verification pipeline
- [ ] Infrastructure and deployment layers do not get skipped
- [ ] Developers can kick off evaluation and keep working (background mode feels natural)
- [ ] Review gates are intuitive: a developer who has never used PICE can action a gate prompt correctly
- [ ] `pice status` is readable at a glance
- [ ] Layer detection "just works" on the five reference framework templates (Next.js, FastAPI, Rails, Express, SvelteKit)
- [ ] A community contributor can build a seam check plugin crate from the docs alone

---

## Risks & Mitigations (v0.2)

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Daemon architecture introduces reliability regressions | High | Medium | Exhaustive watchdog, `PICE_DAEMON_INLINE=1` for debug, extensive integration tests across restart scenarios, graceful shutdown protocol |
| Layer detection is wrong on common project shapes | High | High | Ship detection against 5 reference templates in CI; mandatory manual override via `.pice/layers.toml`; `pice layers check` surfaces unlayered files |
| Seam check false positive rate > signal | High | Medium | Static checks are deterministic, not heuristic; LLM-based checks deferred to v0.4; tune defaults against fixture projects before release |
| Adaptive algorithms halt too early / too late | Medium | Medium | Default SPRT thresholds derived from research; `workflow.yaml` tuning surface; test harness validates confidence claims against known-good/known-bad fixtures |
| Worktree merge conflicts on real projects | Medium | Medium | Default to `apply_to_main` (manual resolution); document `branch_per_layer` for heavily-modified layers; halt-on-conflict behavior prevents silent corruption |
| Windows named pipes behave subtly differently from Unix sockets | Medium | High | Abstract transport behind `DaemonTransport` trait; run full acceptance suite on Windows in CI; named pipe-specific test suite |
| Headless daemon + Unix socket unfamiliar to contributors | Medium | Low | Copy MCP-Guard's patterns; document daemon architecture in `docs/architecture/daemon.md`; provide `PICE_DAEMON_INLINE` debug mode |
| v0.2 adoption breaks v0.1 users | High | Medium | `pice init --upgrade` is non-destructive; v0.1 providers still work; v0.1 `.pice/config.toml` is still respected; migration guide in `docs/guides/migration-v01-to-v02.md` |
| workflow.yaml grammar/validation is the wrong level of flexibility | Medium | Medium | Ship with 5 reference workflows (greenfield, brownfield, CI-only, strict, permissive); collect user feedback in v0.2.x before freezing schema |
| Daemon memory leaks during long-running sessions | Medium | Low | Daemon has a configurable idle timeout; restart-on-memory-limit; periodic manifest flush to prevent in-memory accumulation |
| Provider rate limits during parallel execution | Medium | High | Global semaphore per provider; exponential backoff on 429; surface provider rate limit errors in manifest |
| Security: socket token leakage | High | Low | Token is fs-permission-protected (0600), rotated on daemon restart, never logged or sent to providers |
| Budget overruns (multiple concurrent evaluations exceed team budget) | Medium | Medium | Global budget cap across daemon, not per-evaluation; `pice metrics cost --by-day` surfaces spend trends |

---

## v0.3 — Arch Experts + Web Dashboard

v0.3 extends v0.2 with two features that become possible once the headless daemon and per-layer execution model are in place.

### Feature A: Arch Experts (Dynamically Generated Specialist Agents)

**What it does.** v0.2 ships with static layer evaluators — the "backend evaluator" sees the same prompt regardless of whether the backend is Express+Postgres or Rails+MySQL. v0.3 introduces **Arch Experts**: dynamically generated specialist agents inferred from the project's architecture files.

**Why.** A generic backend evaluator can't verify Prisma migration correctness with the same rigor as a Prisma-specialist evaluator. A generic frontend evaluator doesn't know Tailwind's JIT compilation model. The specialist agent produces a materially higher-quality evaluation because its prompt embeds framework-specific knowledge.

**How it works.**

1. **Architecture extraction** — on `pice init` (and subsequently on demand), PICE scans the project for "architecture files": `prisma/schema.prisma`, `next.config.js`, `tsconfig.json`, `Dockerfile`, `docker-compose.yml`, `terraform/main.tf`, `tailwind.config.js`, etc.
2. **Specialist generation** — a meta-agent (Claude Code provider) reads the architecture files and produces specialist agent definitions with framework-specific evaluation prompts and tool subsets. Generated specialists are stored in `.pice/experts/{layer}-{framework}.toml`.
3. **Review gate** — developer reviews the generated specialists before committing. PICE never executes auto-generated agents without explicit user acknowledgment.
4. **Evaluation integration** — Stack Loops orchestrator uses specialists in place of generic layer evaluators when available. Falls back to generic evaluators when no specialist exists for a given framework.

**Example generated specialist.**

```toml
# .pice/experts/database-prisma.toml
[metadata]
name = "Database (Prisma) Specialist"
generated_at = "2026-05-12T09:14:22Z"
reviewed_by = "jacobmolz"
reviewed_at = "2026-05-12T09:30:00Z"
source_files = ["prisma/schema.prisma", "prisma/migrations/**"]
framework = "prisma"
runtime = "postgresql"

[prompt]
system = """
You are a database verification specialist for a Prisma + PostgreSQL stack.
You know:
- Prisma migration safety patterns (expand-and-contract, additive-only)
- PostgreSQL-specific constraints (unique indexes, partial indexes, CHECK constraints)
- Prisma Client vs. raw query trade-offs
- Connection pooling implications with Serverless runtimes
...
"""

[checks]
migration_safety = true
query_n_plus_one = true
index_coverage = true
unique_constraint_enforcement = true
```

**Originality.** Per `docs/research/originality-analysis.md`, "Arch Experts" as a dynamically generated specialist agent pattern is novel. No existing framework (CrewAI, AutoGen, LangGraph, MetaGPT) spawns architecture-derived specialists from static project files.

**Acceptance criteria (v0.3).**
- [ ] Architecture file scanning produces correct specialist proposals for 10+ common stacks
- [ ] Generated specialists are reviewable before use
- [ ] Specialist execution integrates with Stack Loops orchestrator
- [ ] Fallback to generic layer evaluator works when no specialist exists
- [ ] Specialists re-generate when architecture files change (with user confirmation)
- [ ] Audit trail records which specialist evaluated which layer

### Feature B: Web Dashboard

**What it does.** Browser-based visualization of evaluation progress, confidence curves, seam maps, cost tracking, review gate actioning, and (in v0.5) self-evolving verification metrics. Serves from the daemon — zero external dependencies, no build step, single-file SPA embedded in the binary.

**Why.** CLI output is sufficient for individuals but limited for:
- Team visibility (the person running `pice evaluate` isn't the person who approves the infra gate)
- Tier 3 evaluations with many layers, passes, and seams (hard to read in a terminal)
- Confidence curves (visual is much stronger than numeric)
- Seam maps (graph visualization is the natural form)
- Auditable review (stakeholders reviewing gate history want a searchable interface)

**Architecture.**

```
User runs `pice dashboard`
        │
        ▼
CLI sends RPC: dashboard/start
        │
        ▼
Daemon starts HTTP + WebSocket server on configurable port (default 3141)
        │
        ├── Serves embedded SPA (single HTML file, ~500 KB)
        ├── REST API: /api/features, /api/manifests/:id, /api/gates, /api/metrics
        ├── WebSocket: /ws → live manifest events
        └── Authentication: bearer token from ~/.pice/dashboard.token
        │
        ▼
Browser opens http://localhost:3141
        │
        ▼
SPA authenticates with token, subscribes to WebSocket, renders UI
```

**Core views (v0.3 shipping set).**

1. **Evaluation Overview** — List of all active, pending, completed features with layer progress bars, confidence, cost, status
2. **Layer Detail** — Per-pass confidence progression, per-criterion contract scores, seam check results, Arch Expert findings, model used per pass, cost per pass
3. **Confidence Curve** — Real-time chart of confidence per pass with the ~96.6% ceiling line and tier transition markers
4. **Seam Map** — Force-directed graph of layers with seam check status on each edge (green pass, red fail, yellow warning)
5. **Review Panel** — Pending gates across all features with approve/reject/skip actions. Supports "assignee" filtering for team use
6. **Audit Log** — Searchable history of gate decisions, evaluation results, cost by day/feature/layer

**Implementation notes.**
- SPA is built at pice release time and embedded via `rust-embed`
- No runtime Node.js dependency — the SPA is static HTML+JS+CSS
- Charts use a lightweight library (Chart.js or similar), embedded
- WebSocket framing is newline-delimited JSON matching the daemon's manifest/event schema
- Authentication: bearer token sent as `Authorization: Bearer {token}` header
- `pice dashboard-token` generates/rotates the token
- Token is stored with 0600 permissions in `~/.pice/dashboard.token`
- HTTP listens on `127.0.0.1` only by default; `--bind 0.0.0.0` requires explicit `--allow-network` flag with warning

**Configuration (workflow.yaml or config.toml).**

```toml
[dashboard]
enabled = true
port = 3141
bind = "127.0.0.1"
open_browser_on_start = true
token_rotation_hours = 168  # weekly
```

**Acceptance criteria (v0.3).**
- [ ] `pice dashboard` starts HTTP + WS server, opens browser, authenticates
- [ ] Evaluation Overview shows all features with accurate layer progress
- [ ] Layer Detail shows per-pass confidence, seam checks, contract results
- [ ] Confidence Curve renders with the ceiling line and tier markers
- [ ] Seam Map visualizes layer relationships with status colors
- [ ] Review gates are actionable from the browser; decisions persist to audit trail
- [ ] Dashboard works with zero external dependencies (no npm install, no build step)
- [ ] WebSocket disconnects gracefully and reconnects automatically
- [ ] Token rotation works without losing active browser sessions (grace period for old token)

---

## v0.4 — Implicit Contract Inference

### The problem

v0.1 and v0.2 require developers to write contracts manually (or accept the layer templates' defaults). But the most dangerous failures — per the Seam Blindspot research — are the ones no one wrote a contract for. "Component A assumes something about Component B that Component B never explicitly guarantees." Garlan et al. identified this 30 years ago as "architectural mismatch" and proposed better documentation. It didn't work.

### What v0.4 does

**Automated cross-component assumption asymmetry detection from code and traffic.** PICE derives seam contracts that the developer never wrote, using two sources:

1. **Static analysis of the code** — parse function signatures, type annotations, API call sites, environment variable reads, config references, and extract the IMPLICIT contract each side assumes about the other
2. **Distributed trace analysis** — connect to the project's observability stack (OTel, Honeycomb, Datadog) via existing traces and extract runtime behavior patterns (field presence rates, value distributions, call ordering)

The difference between what Component A assumes and what Component B actually provides is the asymmetry. PICE surfaces these as seam findings.

### Example

```
Static analysis of frontend code:
  - Calls API with { userId: string }
  - Expects response { email: string, name: string, phoneVerified: boolean }
  - Relies on phoneVerified always being present (no null check)

Static analysis of API code:
  - Returns { email, name, phoneVerified? }
  - phoneVerified only set if user went through verification flow

Trace analysis:
  - 100% of /api/users responses in staging have phoneVerified
  - 94% of production responses have phoneVerified
  - 6% of production responses return phoneVerified = undefined

Inferred seam finding:
  ⚠️ Seam API↔Frontend: phoneVerified asymmetry
     Frontend assumes always present (no null check)
     API provides conditionally (optional in type)
     Production: 6% of responses omit the field
     → NullPointerException candidate
```

### Architectural pieces

1. **Static analyzer crate** (`pice-static-analysis`) — language-agnostic interface with per-language impls (Rust, TS, Python, Go to start). Extracts "assumes" and "provides" per API call site and handler.
2. **Trace ingester** (`pice-trace-ingest`) — pluggable source for distributed traces. Initial support for OpenTelemetry JSON export files; later: Honeycomb API, Datadog API, Tracetest files. Reads traces and extracts field presence, value distributions, call orderings.
3. **Contract synthesizer** (`pice-core::contract::synthesize`) — combines static + trace analysis to produce candidate seam contracts. Developer reviews and commits (same pattern as layer detection and Arch Experts).
4. **Integration with v0.2 seam checks** — synthesized contracts become runnable seam checks that slot into the v0.2 seam verification pipeline.

### Prior art

- **Pact** — consumer-driven contract testing; structural only
- **Specmatic** — spec-as-contract; limited to what's in the spec
- **Tracetest** — turns production traces into test assertions (closest prior art)
- **Honeycomb ODD** — "your job isn't done until it's in production"
- **Harvard compositional specification research** — LLMs handle local specs but fail under composition

No existing tool synthesizes seam contracts from the combination of static code and distributed traces with LLM-based reasoning about asymmetry. This is the novel contribution.

### Success criteria (v0.4)

- [ ] Static analyzer supports Rust, TypeScript, Python, Go for extracting assumes/provides
- [ ] Trace ingester supports OTel JSON export at minimum
- [ ] Contract synthesizer produces reviewable seam contracts
- [ ] Inferred contracts catch at least 3 of the 12 seam failure categories that v0.2's static seam checks miss
- [ ] Integration with v0.2 seam verification pipeline is seamless (inferred checks use the same `SeamCheck` trait)
- [ ] Polyrepo seam checks become possible using traces as the cross-repo signal (closes the v0.2 polyrepo deferral)
- [ ] Developer workflow: run analysis, review synthesized contracts, commit or discard, re-run verification

### Acceptance criteria (v0.4)

- [ ] Static + trace analysis produces at least one correct seam contract on a multi-service reference project
- [ ] Synthesized contracts have < 20% false positive rate on reference fixtures (validated against known-good baselines)
- [ ] Review UI (CLI + dashboard) shows synthesized contracts with both static and trace evidence
- [ ] Inferred checks integrate into the v0.2 pipeline without special-casing
- [ ] Performance: trace ingestion for 10K traces completes in < 30s

### Open questions (to be resolved before v0.4 spec freeze)

- How does PICE handle traces with PII? (filtering rules? redaction?)
- What's the right cadence for re-running contract synthesis? (on every evaluation? on schedule? on demand?)
- How are inferred contracts versioned when they change? (new contract overrides old? append with deprecation?)

---

## v0.5 — Self-Evolving Verification

### The problem

Every verification run produces data: which checks fired, which caught real issues, which were false positives, which were redundant, how much each cost. v0.1–v0.4 collect this data in SQLite but don't act on it. Every run treats the checklist as static.

**Meta, Google, and Netflix have proven that acting on this data works at scale.** Meta's Predictive Test Selection catches >99.9% of regressions while running only one-third of tests. Google's TAP platform uses ML-driven test selection to reduce computational waste by >30% while maintaining 99.9% regression safety. Develocity saves Netflix 280,000 developer hours per year.

PICE has the same data. v0.5 closes the loop.

### What v0.5 does

**Every evaluation makes the next one smarter, more targeted, and cheaper.** Four mechanisms:

1. **Predictive check selection** — an ML model (gradient-boosted decision tree, similar to Meta's) predicts which checks are most likely to catch issues on a given change. Low-probability checks are skipped or deferred. High-probability checks run first, fail-fast.
2. **Check value scoring** — every check accumulates a historical record: hit rate (how often it flags), true positive rate (how often the flag is a real bug), false positive rate, cost per true positive, time-to-flag. Checks with low value are pruned or down-weighted automatically.
3. **Adaptive confidence targeting** — projects with high historical pass rates at 90% confidence drop to 88% target to save money. Projects with historical failures at 95% rise to 97% target. Confidence targets become project-specific and risk-history-aware.
4. **Observability feedback loop** — when production incidents occur, the incident → trace → seam contract pipeline (v0.4) generates a new check, which gets added to the verification pipeline. The check that would have caught the incident now runs on every future change.

### Supporting research (embedded key figures)

- **Meta PTS** (ICSE-SEIP 2019): >99.9% faulty-change detection with one-third of the test count (effective 3x infra efficiency)
- **Google TAP**: 30%+ computational waste reduction, 99.9% regression safety, 150M test executions daily
- **Netflix via Develocity**: 280,000 developer hours saved per year, 10+ min test runs → 1–2 min
- **Launchable**: running 20% of tests achieves 90% catch rate
- **Meta Sapienz**: 75% actionable report rate, 80% reduction in Android crashes

The feature set that drives these systems' predictions: which files changed, which tests historically fail on those files, recency of failures, developer identity, time of day, commit metadata. **This is precisely what PICE's SQLite metrics engine already collects.** v0.5 adds the ML model on top.

### Architectural pieces

1. **Metrics warehouse crate** (`pice-metrics-warehouse`) — extends v0.1's SQLite metrics to include per-check outcomes, file associations, true/false positive labels (from developer feedback), cost attribution
2. **Model trainer** (`pice-ml-trainer`) — trains a gradient-boosted tree model weekly (or on demand) on the warehouse data. Stores the trained model in `.pice/models/check_selector_{date}.bin`
3. **Predictive selector** (`pice-core::predict`) — at evaluation time, loads the latest model and scores each pending check. High-score checks run first; below-threshold checks are skipped with a "deselected by model" marker
4. **Value score aggregator** — computes per-check value scores from warehouse data. Surfaces in `pice metrics --check-values`
5. **Observability → verification bridge** — reuses v0.4's trace ingester. When an incident is logged (either manually via `pice incident log` or automatically from monitoring integration), the bridge generates a new seam check

### Safety and override

- **Never skip checks without developer visibility.** Every deselected check appears in `pice status` with a reason ("low predicted hit rate", "never flagged in 90 days", "redundant with X").
- **Manual override.** `pice evaluate --full` runs every check regardless of model predictions. `workflow.yaml` has a `predictive_selection: enabled|disabled` flag.
- **Regression safety** — the predictive model is validated against held-out historical data and must meet a minimum catch rate (default 99%). Below that, the model is rejected and selection falls back to running all checks.
- **Audit visibility** — every model-driven decision writes to audit trail with model version and prediction score.

### Success criteria (v0.5)

- [ ] Predictive check selection reduces evaluation cost by ≥ 30% on reference projects after 100+ historical runs
- [ ] Catch rate ≥ 99% on held-out test data (regression safety)
- [ ] Check value scoring surfaces top / bottom 10 checks accurately
- [ ] Adaptive confidence targeting tracks project risk history
- [ ] Observability → verification bridge generates at least one catching check from a simulated incident
- [ ] All model-driven decisions are auditable and override-able

### Acceptance criteria (v0.5)

- [ ] Model retrains automatically weekly (configurable)
- [ ] Trained model files are versioned and rolled forward on retrain
- [ ] `pice metrics check-values` shows per-check true positive rate, false positive rate, cost per TP
- [ ] `pice evaluate --full` overrides predictive selection
- [ ] Deselected checks appear in `pice status` with reason
- [ ] Audit trail captures model version per decision
- [ ] Held-out regression test validates catch rate before model deployment

### Open questions (to be resolved before v0.5 spec freeze)

- Which ML framework? (linfa for pure Rust? candle for ONNX? external Python call?)
- How do we handle the cold start — projects with < 100 historical runs have no data to train on
- Should the model be per-project or shared across projects (with telemetry opt-in)?
- How does the telemetry opt-in from v0.1 interact with model sharing?
- What is "developer feedback" input for labeling false positives? (explicit `pice feedback` command? inferred from post-evaluation commits?)

---

## API Specification (v0.2 additions)

### Daemon RPC (newline-delimited JSON-RPC 2.0 over Unix socket / named pipe)

This is a separate protocol from the provider protocol. It is consumed by the CLI adapter and (in v0.3) the dashboard.

| Method | Params | Result |
|--------|--------|--------|
| `daemon/health` | `{}` | `{ status, version, uptime_s }` |
| `daemon/shutdown` | `{}` | `{ success }` |
| `daemon/reload-config` | `{}` | `{ reloaded: [...] }` |
| `execute/create` | `{ planPath, workflowSnapshot, mode: sync\|background }` | `{ featureId }` |
| `evaluate/create` | `{ planPath, mode }` | `{ featureId }` |
| `manifest/get` | `{ featureId }` | `Manifest` |
| `manifest/list` | `{ filter?, limit? }` | `ManifestSummary[]` |
| `manifest/subscribe` | `{ featureId?: null-for-all }` | stream of `manifest/event` notifications |
| `manifest/unsubscribe` | `{ subscriptionId }` | `{ success }` |
| `gate/list` | `{ featureId? }` | `Gate[]` |
| `gate/decide` | `{ featureId, layer, decision, reason? }` | `{ success, newStatus }` |
| `worktree/list` | `{}` | `Worktree[]` |
| `worktree/prune` | `{ featureId? }` | `{ removed: N }` |
| `logs/stream` | `{ featureId, layer?, follow }` | stream of `logs/chunk` notifications |
| `validate/workflow` | `{ path }` | `{ valid, errors: [] }` |

### Notifications

| Method | Payload |
|--------|---------|
| `manifest/event` | `{ featureId, eventType, layer?, data, timestamp }` |
| `logs/chunk` | `{ featureId, layer, text, timestamp }` |
| `gate/requested` | `{ featureId, layer, gateId, summary }` |

### Environment Variables (additions)

| Variable | Purpose | Default |
|----------|---------|---------|
| `PICE_DAEMON_SOCKET` | Override daemon socket path | `~/.pice/daemon.sock` (Unix) / `\\.\pipe\pice-daemon` (Windows) |
| `PICE_DAEMON_INLINE` | Run orchestrator in-process (debug) | unset |
| `PICE_DAEMON_TOKEN_PATH` | Override token file location | `~/.pice/daemon.token` |
| `PICE_STATE_DIR` | Override manifest storage location | `~/.pice/state/` |
| `PICE_WORKFLOW_FILE` | Override workflow.yaml path | `.pice/workflow.yaml` |
| `PICE_USER_WORKFLOW_FILE` | Override user workflow.yaml path | `~/.pice/workflow.yaml` |
| `PICE_LOG_DIR` | Daemon log directory | `~/.pice/logs/` |
| `PICE_DASHBOARD_TOKEN_PATH` | Override dashboard token file | `~/.pice/dashboard.token` |

### CLI commands added in v0.2

| Command | Purpose |
|---------|---------|
| `pice daemon {start\|stop\|restart\|status\|logs}` | Daemon lifecycle |
| `pice layers {detect\|list\|check\|graph}` | Layer configuration |
| `pice validate` | Workflow + contract + layers validation |
| `pice worktree {list\|prune}` | Worktree management |
| `pice clean` | Remove abandoned worktrees and stale manifests |
| `pice review-gate [--list] {feature} --layer {layer}` | Action pending gates |
| `pice logs {feature} [--layer L] [--follow]` | Log streaming |
| `pice status --follow` | Live status streaming |
| `pice status --wait {feature}` | Block until feature completes |
| `pice audit gates [--feature F] [--since DATE]` | Gate decision audit trail |
| `pice metrics cost [--by-day\|--by-feature\|--by-layer]` | Cost reporting |

### CLI commands added in v0.3

| Command | Purpose |
|---------|---------|
| `pice dashboard [--port P] [--open\|--no-open]` | Start web dashboard |
| `pice dashboard-token [--rotate]` | Manage dashboard auth token |
| `pice experts {generate\|list\|review}` | Arch Expert management |

### CLI commands added in v0.4

| Command | Purpose |
|---------|---------|
| `pice contracts {synthesize\|review}` | Implicit contract inference |
| `pice traces ingest {path\|url}` | Ingest OTel traces for seam analysis |

### CLI commands added in v0.5

| Command | Purpose |
|---------|---------|
| `pice metrics check-values` | Per-check true/false positive rates and cost |
| `pice evaluate --full` | Override predictive check selection |
| `pice feedback {true-positive\|false-positive} {featureId} {checkId}` | Label check outcomes |
| `pice incident log` | Record production incident for bridge |
| `pice model {train\|rollback\|info}` | Predictive model management |

---

## Security & Configuration

### Authentication (unchanged from v0.1 for providers, new for daemon)

- **Provider auth** (Claude Code, Codex): unchanged from v0.1. API keys OR subscription auth delegated to each SDK.
- **Daemon socket auth (new in v0.2)**: bearer token in `~/.pice/daemon.token` with 0600 permissions. Token rotates on daemon restart. CLI adapter reads token at startup and sends with each RPC.
- **Dashboard auth (new in v0.3)**: separate bearer token in `~/.pice/dashboard.token`. Rotation on `pice dashboard-token --rotate` or weekly automatic. Token presented in `Authorization: Bearer` header over localhost HTTP.

### Configuration extensions

In addition to v0.1's `.pice/config.toml`:

```toml
# v0.2 additions
[daemon]
auto_start = true
idle_shutdown_minutes = 0    # 0 = never shut down due to idle
max_concurrent_evaluations = 4

[notifications]
on_complete = "terminal"     # terminal | none | slack (v0.2+) | webhook (v0.2+)
on_gate     = "terminal"
on_failure  = "terminal"

[worktrees]
preserve_failed_worktrees = true
parent_directory = ".pice/worktrees"

[audit]
retention_days = 365
```

```toml
# v0.3 additions
[dashboard]
enabled = true
port = 3141
bind = "127.0.0.1"
open_browser_on_start = true
token_rotation_hours = 168

[experts]
auto_generate_on_init = false
review_required = true
```

```toml
# v0.4 additions
[inference]
enabled = true
static_analysis = true
trace_analysis = true
trace_sources = ["otel-json:./otel-export.json"]
false_positive_review_required = true
```

```toml
# v0.5 additions
[predictive]
enabled = true
model_retrain_days = 7
minimum_training_runs = 100
regression_safety_threshold = 0.99
```

### Deployment

- v0.2 adds a `pice-daemon` binary shipped in the same NPM platform packages as `pice`.
- `npm install -g @jacobmolz/pice` installs both.
- `cargo install pice-cli` builds both binaries from source.
- No additional system dependencies — daemon uses standard library + tokio + sqlite only.

---

## Implementation Phases (v0.2 monolithic)

### Phase 0: Foundation Refactor

**Goal:** Split the v0.1 binary into `pice-cli` + `pice-daemon` + `pice-core` without changing user-visible behavior.

- [ ] Extract `pice-core` crate with config/layers/manifest/seam/adaptive/protocol modules
- [ ] Create `pice-daemon` crate with socket server + orchestrator skeleton
- [ ] Refactor `pice-cli` to communicate with daemon via socket
- [ ] Implement daemon auto-start, socket authentication, health check
- [ ] Implement `PICE_DAEMON_INLINE=1` bypass for debugging
- [ ] Windows named pipe transport
- [ ] All v0.1 commands work against the daemon with no behavior change

**Validation:** Full v0.1 test suite passes against the new daemon architecture. No regressions in functionality. Latency targets met.

### Phase 1: Layer Detection and Stack Loops Core

**Goal:** Per-layer PICE execution with `.pice/layers.toml`.

- [ ] Implement six-level layer detection stack
- [ ] `.pice/layers.toml` parser + validator
- [ ] `pice layers {detect\|list\|check\|graph}` commands
- [ ] File-level layer tagging for fullstack frameworks
- [ ] DAG construction + topological cohort identification
- [ ] Stack Loops orchestrator (sequential for now, no parallelism yet)
- [ ] Layer-specific contract templates
- [ ] Layer-scoped context isolation enforcement (test-driven)
- [ ] `pice init --upgrade` generates proposed layers.toml
- [ ] Migration guide for v0.1 → v0.2

**Validation:** 7-layer evaluation completes sequentially on the reference Next.js + Prisma + Terraform project. All layers get context-isolated evaluators.

### Phase 2: Workflow YAML and Validation

**Goal:** `.pice/workflow.yaml` defines the pipeline; validation catches errors.

- [ ] YAML schema + parser
- [ ] Framework defaults embedded in binary
- [ ] Project + user inheritance with floor-based merge
- [ ] Trigger expression grammar + parser
- [ ] `pice validate` command
- [ ] Layer override plumbing into Stack Loops orchestrator
- [ ] 5 reference workflow presets (greenfield/brownfield/CI/strict/permissive)

**Validation:** Workflow changes drive observable pipeline behavior. Floor violations are caught. Reference workflows all validate.

### Phase 3: Seam Verification

**Goal:** Seam checks run at every layer boundary.

- [ ] `SeamCheck` trait and registry
- [ ] ~30 default static seam checks covering all 12 failure categories
- [ ] Per-boundary check assignment in `layers.toml`
- [ ] Seam execution phase after per-layer contract grading
- [ ] Seam findings in manifest and evaluation output
- [ ] Plugin crate pattern documented
- [ ] Reference seam check plugin for one protocol (e.g., gRPC)

**Validation:** End-to-end test fires each of the 12 failure categories and verifies detection. Plugin crate builds and loads.

### Phase 4: Adaptive Evaluation

**Goal:** Bayesian-SPRT, ADTS, VEC ship with honest confidence claims.

- [ ] Pure-function SPRT implementation in `pice-core::adaptive`
- [ ] ADTS divergence detection and escalation
- [ ] VEC entropy computation and halting
- [ ] Integration into evaluation loop
- [ ] Cost tracking per pass + budget enforcement
- [ ] Adaptive halting metadata in manifest (`halted_by: ...`)
- [ ] Confidence ceiling validation test (never claim > 96.6%)
- [ ] Configuration surface in workflow.yaml

**Validation:** Adaptive tests show cost reduction vs. fixed pass counts on reference fixtures. Confidence claims track the correlated Condorcet predictions within ±2%.

### Phase 5: Worktree Isolation and Parallelism

**Goal:** Independent layers run in parallel worktrees.

- [ ] `git2-rs` integration for worktree CRUD
- [ ] Worktree lifecycle in daemon
- [ ] Parallel cohort execution in orchestrator
- [ ] Merge strategies (apply_to_main, squash_commit, branch_per_layer)
- [ ] `pice worktree {list\|prune}` and `pice clean`
- [ ] `preserve_failed_worktrees` behavior
- [ ] `pice evaluate --isolate` for read-only + worktree combo

**Validation:** Reference project runs parallel backend + frontend cohort in separate worktrees. Speedup ≥ 1.6× sequential.

### Phase 6: Review Gates

**Goal:** Human-in-the-loop gates work foreground and background.

- [ ] Gate trigger expression evaluator (reuse workflow grammar)
- [ ] Foreground gate prompt with keyboard input
- [ ] Background gate deferral via manifest
- [ ] `pice review-gate {feature} --layer {layer}` command
- [ ] Timeout behavior (reject/approve/skip)
- [ ] Audit trail SQLite schema + writes
- [ ] `pice audit gates` export

**Validation:** Gates fire on configured triggers. Foreground and background modes both work. Audit trail captures every decision.

### Phase 7: Background Execution

**Goal:** `pice evaluate --background` + `pice status --follow`.

- [ ] Daemon async execution of full pipeline
- [ ] `pice evaluate/execute --background` dispatch
- [ ] `pice status` + `pice status --follow` + `pice status --wait`
- [ ] `pice logs {feature} [--follow]` streaming
- [ ] Desktop notifications (macOS, Linux, Windows best-effort)
- [ ] Multiple concurrent evaluations (resource management + provider rate limiting)
- [ ] CI mode validation (`--background --wait`)

**Validation:** End-to-end background flow on a 7-layer feature, concurrent with a second feature, with live status updates and successful notification on completion.

### Phase 8: Polish, Documentation, Release

**Goal:** Ship v0.2.0.

- [ ] Migration guide (`docs/guides/migration-v01-to-v02.md`)
- [ ] Stack Loops adoption guide (`docs/guides/stack-loops.md`)
- [ ] Updated architecture docs (daemon split)
- [ ] Updated provider development guide (v0.2 protocol additions)
- [ ] All 5 reference framework projects tested end-to-end
- [ ] CI passes full acceptance suite on macOS arm64/x64, Linux arm64/x64, Windows x64
- [ ] Release notes with breaking changes called out (architectural pivot, new config surface)
- [ ] Telemetry schema extended for v0.2 metrics (cost tracking, adaptive halt reasons, gate decisions)
- [ ] v0.2.0 tag + release

**Validation:** Full PICE loop from `pice init --upgrade` through parallel Stack Loops execution to background-actioned gates completes on all platforms.

---

## Success Criteria (v0.2 complete)

### Methodology

- [ ] Stack Loops orchestration works end-to-end on 5 reference framework projects (Next.js+Prisma, FastAPI+Postgres, Rails, Express+Mongo, SvelteKit+Supabase)
- [ ] Layer-specific contracts catch infrastructure and deployment issues that v0.1's feature-level contracts miss (validated on fixture bugs)
- [ ] Seam checks detect at least one representative failure from each of the 12 empirical categories
- [ ] Adaptive evaluation reduces cost ≥ 30% vs. v0.1 fixed-tier pass counts while maintaining or increasing confidence
- [ ] Parallel execution achieves ≥ 1.6× speedup on 2-layer cohort vs. sequential
- [ ] Review gates are intuitive: a developer unfamiliar with PICE can action a foreground gate correctly
- [ ] Background execution is reliable: 100 concurrent evaluations in CI complete without conflicts

### Architecture

- [ ] Headless daemon + CLI adapter split is complete and production-ready
- [ ] Manifest-as-source-of-truth works correctly across daemon restarts
- [ ] All v0.1 providers continue to work in single-layer fallback mode (no community provider breakage)
- [ ] v0.2 provider protocol additions are documented and a reference community seam-check plugin is buildable from the docs alone
- [ ] Windows named pipe parity verified
- [ ] Daemon cold start < 500ms; warm CLI command < 50ms

### Ops

- [ ] `pice init --upgrade` is non-destructive on v0.1 projects
- [ ] Migration guide walks through every breaking change
- [ ] Audit trail captures every gate decision and cost event
- [ ] Telemetry is extended without breaking v0.1's opt-in privacy guarantees

### Quality

- [ ] Test count ≥ 400 (v0.1 = 217; v0.2 adds ~200 for new features)
- [ ] Clippy clean, fmt clean, eslint clean, tsc clean
- [ ] End-to-end acceptance suite runs on macOS arm64/x64, Linux arm64/x64, Windows x64 in CI
- [ ] Fuzz tests for workflow.yaml parser and daemon RPC
- [ ] Memory leak test: 24-hour daemon run with continuous evaluations shows no unbounded growth

---

## Assumptions

1. **The daemon architecture is the right call for v0.2.** Alternatives considered: shared SQLite with file locks (rejected: contention and no live event streaming); per-command subprocess with lock file (rejected: no background, no gate resumption); a full message bus (rejected: complexity, external dependency). The MCP-Guard-style daemon is the lightest weight way to get background execution + multi-adapter visibility + gate pause/resume.
2. **v0.1 providers still work without modification.** The protocol additions are opt-in via capabilities; single-layer fallback mode is sufficient for v0.1 providers to continue functioning. Validated by: keeping the existing provider-stub as a test harness for v0.1-only provider behavior.
3. **Git worktrees are safe for parallel AI execution.** Worktrees share git objects but have independent working directories, so concurrent file edits don't conflict. Tested informally in v0.1 Phase 5 work; needs explicit validation in v0.2 with parallel provider sessions.
4. **The ~96.6% confidence ceiling is real and enforced.** Based on the correlated Condorcet Jury Theorem and Kim et al. (ICML 2025) data. v0.2 does not attempt to claim higher confidence without adding orthogonal signal (which is v0.4's job, not v0.2's).
5. **Adaptive SPRT halt rules produce calibrated confidence.** Depends on the prior Beta(1,1) being an acceptable starting point. May need project-specific tuning; workflow.yaml exposes the knobs.
6. **Seam checks in v0.2 are "cheap static" — LLM-based seam analysis is v0.4.** Static checks are fast, deterministic, and cover the most common failure categories. LLM-based reasoning about assumption asymmetries is out of scope for v0.2.
7. **Gate interaction is primarily CLI-driven in v0.2.** Dashboard gate UI is v0.3. Webhook/Slack/email gate notifications are post-v0.3.
8. **Multiple concurrent background evaluations don't hit provider rate limits.** Depends on reasonable provider-side rate limits + global semaphore. If rate limits are hit, daemon backs off exponentially and retries.
9. **Rust crate splitting (`pice-core`) is worth the refactoring cost.** Alternatives considered: keeping everything in `pice-cli` and having `pice-daemon` depend on it (rejected: creates a dependency cycle once the CLI also depends on daemon types). The three-crate split is the clean way to share config/protocol types without cycles.
10. **Developer-labeled false positives are available for v0.5.** The `pice feedback` command requires developer discipline. If labels are sparse, predictive model quality will be limited — v0.5 may need to fall back to heuristic pruning. This is an open question for v0.5 spec freeze.
11. **Observability integration is feasible.** v0.4's trace ingester assumes projects have some form of OTel/Honeycomb/Datadog traces. Projects without any observability get no benefit from v0.4's implicit contract inference — they can still use v0.2's static seam checks.
12. **Windows named pipes + tokio handle the same workload as Unix sockets.** Needs explicit benchmarking in Phase 0 before the daemon architecture is finalized. If named pipes are insufficient, fallback is localhost TCP on a random port with token auth.
13. **The public roadmap and PRDv2 will be kept in sync by a separate effort.** PRDv2 supersedes the roadmap for implementation purposes, but roadmap updates are deliberately out of scope for this document.

---

## References

- [v0.1 PRD (`.claude/PRD.md`)](.claude/PRD.md) — baseline MVP document
- [Roadmap (`docs/roadmap.md`)](docs/roadmap.md) — public narrative
- [Seam Blindspot research (`docs/research/seam-blindspot.md`)](docs/research/seam-blindspot.md) — empirical basis for seam verification and the 12 failure categories
- [Convergence Analysis (`docs/research/convergence-analysis.md`)](docs/research/convergence-analysis.md) — mathematical basis for adaptive evaluation and the ~96.6% ceiling
- [Self-Evolving Verification research (`docs/research/self-evolving-verification.md`)](docs/research/self-evolving-verification.md) — empirical basis for v0.5 predictive selection
- [Originality Analysis (`docs/research/originality-analysis.md`)](docs/research/originality-analysis.md) — confirms Stack Loops and Arch Experts are novel contributions
- [v0.2 Gap Analysis (`docs/research/v02-gap-analysis.md`)](docs/research/v02-gap-analysis.md) — 37 gaps identified and resolved in v0.2 design
- [Glossary (`docs/glossary.md`)](docs/glossary.md) — term definitions
- [Source planning sketch (`.claude/plans/pice-ux-patterns-prd.md`)](.claude/plans/pice-ux-patterns-prd.md) — original Archon-inspired UX patterns document that seeded PRDv2

---

*PRDv2 — Draft 2026-04-10. Supersedes the planning sketch for Archon-inspired UX patterns and extends the v0.1 PRD with the full post-v0.1 roadmap (v0.2 Stack Loops, v0.3 Arch Experts + Dashboard, v0.4 Implicit Contract Inference, v0.5 Self-Evolving Verification).*

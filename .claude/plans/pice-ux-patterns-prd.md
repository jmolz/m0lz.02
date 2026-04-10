# PRD: PICE UX Pattern Adoption from Archon Analysis

## Context

Competitive analysis of Archon (archon.diy) — a trending workflow engine for AI coding — identified five UX patterns that PICE should adopt. These are execution and interface patterns, not verification architecture. PICE's core differentiators (adversarial evaluation, seam verification, adaptive convergence, self-evolution) remain untouched. This PRD adds the ergonomic layer that makes those differentiators usable in production.

**What we're NOT adopting:** Archon's magic workflow routing (opaque, unauditable), single-model review architecture (no adversarial evaluation), monolithic codebase treatment (no layer/seam awareness), or static workflow definitions (no self-evolution).

**Priority order:** Features are ordered by impact on v0.2 Stack Loops adoption. Items 1–3 are required for v0.2. Items 4–5 are v0.3+.

---

## Feature 1: Worktree Isolation for Parallel Stack Loop Layers

**Priority:** P0 — Required for v0.2
**Why:** Independent layers (backend and frontend) should evaluate concurrently. Without worktree isolation, parallel subagents would conflict on file reads/writes. Claude Code subagents already support `isolation: worktree` — the building block exists.

### Requirements

**1.1 — Automatic worktree creation per parallel layer evaluation**

When PICE determines that two or more layers have no dependency edge between them (per `.pice/layers.toml` `depends_on` graph), it should:

- Create a git worktree for each parallel layer: `.pice/worktrees/{feature-id}/{layer-name}/`
- Pass `isolation: worktree` in the `AgentDefinition` for each parallel subagent
- Clean up worktrees after all parallel layers complete (or on `pice clean`)

```
# Example: backend and frontend have no dependency edge
# They get parallel worktrees; API depends on both, so it waits

backend  ──┐ (worktree: .pice/worktrees/auth-feature/backend/)
            ├──→ API layer (runs after both complete, main worktree)
frontend ──┘ (worktree: .pice/worktrees/auth-feature/frontend/)
```

**1.2 — Worktree lifecycle management**

- `pice status` shows active worktrees and which layers are running in them
- `pice clean` removes all worktrees for completed or abandoned evaluations
- Worktrees are created from the current branch HEAD at evaluation start
- If a layer evaluation modifies files (implementation phase), changes are merged back to the main worktree after the layer passes
- If a layer fails, the worktree is preserved for debugging (configurable: `preserve_failed_worktrees = true` in `.pice/config.toml`)

**1.3 — Evaluation-only mode uses read-only access (no worktree needed)**

When running `pice evaluate` (not `pice execute`), subagents are read-only (`tools: [Read, Grep, Glob]`). Read-only agents don't need worktree isolation since they can't conflict. Worktrees are only created for `pice execute` with parallel layers, or when explicitly requested via `--isolate`.

### Implementation notes

- Use `git worktree add` / `git worktree remove` from PICE's Rust core
- The worktree path is passed to Claude Code via `--directory` flag on the subprocess
- The `AgentDefinition` for each subagent includes `isolation: "worktree"` when parallel execution is active
- Worktree creation adds ~200ms overhead — negligible vs. evaluation time

### Acceptance criteria

- [ ] Parallel layers with no dependency edge run concurrently in separate worktrees
- [ ] Sequential layers with dependency edges wait for upstream completion
- [ ] `pice status` shows worktree status for active evaluations
- [ ] `pice clean` removes all evaluation worktrees
- [ ] Read-only evaluation (`pice evaluate`) does not create worktrees unless `--isolate` is passed
- [ ] Failed layer worktrees are preserved when `preserve_failed_worktrees = true`

---

## Feature 2: Evaluation Workflow as Committable YAML

**Priority:** P0 — Required for v0.2
**Why:** PICE's evaluation pipeline (which tiers to use, which layers get human gates, when to escalate, retry limits) needs to be codified as a committable file so the entire team runs the same verification process. Currently `.pice/layers.toml` defines layers and `.pice/contracts/` defines criteria, but the orchestration logic (the "how") isn't codified.

### Requirements

**2.1 — `.pice/workflow.yaml` defines the evaluation pipeline**

```yaml
# .pice/workflow.yaml
schema_version: "0.2"

defaults:
  tier: 2
  min_confidence: 0.90
  max_passes: 5
  model: sonnet
  budget_usd: 2.00

phases:
  plan:
    description: "Generate layer-aware plan from feature request"
    output: .claude/plans/{feature-id}.md

  execute:
    description: "Implement per layer in dependency order"
    parallel: true                    # Run independent layers concurrently
    worktree_isolation: true          # Isolate parallel layers in worktrees
    retry:
      max_attempts: 3
      fresh_context: true             # Prevent evaluator contamination between retries

  evaluate:
    description: "Evaluate per layer with seam checks"
    parallel: true
    model_override:
      infrastructure: opus            # Complex layer gets stronger model
      frontend: haiku                 # Simple layer gets cheaper model
    seam_checks: true

  review:                             # Optional human gate
    enabled: true
    trigger: "tier >= 3 OR layer == infrastructure OR layer == deployment"
    description: "Human reviews before deployment-affecting layers proceed"
    timeout_hours: 24                 # Auto-reject if no response

layer_overrides:
  infrastructure:
    tier: 3                           # Always Tier 3 for infra
    min_confidence: 0.95
    require_review: true

  frontend:
    tier: 1                           # Tier 1 sufficient for frontend
    max_passes: 2
```

**2.2 — Workflow inheritance and override**

- Default workflow ships with the framework (sensible defaults for all phases)
- `.pice/workflow.yaml` in the project overrides defaults (committed to repo, team-wide)
- `~/.pice/workflow.yaml` provides user-level overrides (personal preferences, not committed)
- Override semantics follow the same floor-based merge as MCP-Guard configs: personal overrides can restrict (lower budgets, higher confidence thresholds) but never relax (can't lower tier or disable review gates)

**2.3 — Workflow validation**

- `pice validate` checks `.pice/workflow.yaml` against the schema and reports errors
- Invalid workflows block evaluation with a clear error message
- Schema is versioned (`schema_version: "0.2"`) for forward compatibility

### Implementation notes

- Parse YAML in Rust using `serde_yaml`
- The workflow file is read once at the start of each `pice` command and passed as configuration to the orchestrator
- The `phases` section maps directly to the existing Plan→Implement→Contract→Evaluate lifecycle
- The `review` phase is a new addition that pauses the pipeline and waits for human input

### Acceptance criteria

- [ ] `.pice/workflow.yaml` controls evaluation pipeline behavior
- [ ] Default workflow ships with framework; project/user overrides work
- [ ] Floor-based merge semantics: personal overrides restrict, never relax
- [ ] `pice validate` catches invalid workflow files with actionable errors
- [ ] Layer-level overrides (tier, model, confidence) work as specified
- [ ] Parallel and retry settings are respected by the orchestrator

---

## Feature 3: Human-in-the-Loop Approval Gates

**Priority:** P0 — Required for v0.2
**Why:** For production deployments, teams need configurable pause points where a human reviews before proceeding. The Bayesian-SPRT/ADTS algorithms determine when AI confidence is insufficient, but there should also be explicit gates for high-stakes layers (infrastructure, deployment) regardless of AI confidence.

### Requirements

**3.1 — Configurable review gates in workflow.yaml**

The `review` phase (see Feature 2) supports trigger conditions:

```yaml
review:
  enabled: true
  trigger: "tier >= 3 OR layer == infrastructure OR layer == deployment"
  timeout_hours: 24
  on_timeout: reject           # or: approve, skip
  notification: stdout         # future: slack, email, webhook
```

Trigger conditions support:
- `tier >= N` — gate fires when evaluation tier exceeds threshold
- `layer == name` — gate fires for specific layers
- `confidence < N` — gate fires when posterior confidence is below threshold
- `always` — gate fires on every evaluation
- Boolean combinators: `AND`, `OR`, `NOT`

**3.2 — Gate interaction via CLI**

When a gate fires, the evaluation pauses and prompts:

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

- `approve` — layer passes, downstream layers proceed
- `reject` — layer fails, evaluation stops (or triggers retry if attempts remain)
- `details` — show full evaluation output, seam check details, contract criteria
- `skip` — bypass gate with warning logged to audit trail

**3.3 — Background mode with deferred gates**

When running in background mode (`pice evaluate --background`), gates don't block the terminal. Instead:
- Gate state is written to the verification manifest
- `pice status` shows pending gates
- `pice review {feature-id} --layer infrastructure` opens the gate interaction
- Gates timeout according to `timeout_hours` setting

### Implementation notes

- Gates are implemented as a state in the verification manifest: `"status": "pending_review"`
- The CLI polls the manifest for pending gates when `pice review` is called
- In CI/CD mode (GitHub Actions), gates can be wired to PR review approvals via webhook (future, not v0.2)
- Gate decisions are logged to the SQLite audit trail with reviewer identity and timestamp

### Acceptance criteria

- [ ] Review gates pause evaluation when trigger conditions match
- [ ] CLI presents gate with evaluation summary and approve/reject/details/skip options
- [ ] Gates work in both foreground and background modes
- [ ] Gate decisions are logged to audit trail
- [ ] Timeout behavior (reject/approve/skip) is configurable and enforced
- [ ] `pice status` shows pending gates

---

## Feature 4: Background Execution with Notification

**Priority:** P1 — Target v0.2, defer if needed
**Why:** Stack Loop evaluation across 7 layers with seam checks can take minutes. Blocking the terminal for that duration is bad UX. "Fire and forget" — kick off evaluation, get notified when done — is the right model for anything longer than 30 seconds.

### Requirements

**4.1 — Background evaluation mode**

```bash
# Foreground (current behavior, stays default)
pice evaluate .claude/plans/auth-plan.md

# Background — returns immediately, evaluation runs in daemon
pice evaluate .claude/plans/auth-plan.md --background
# → Evaluation started: auth-feature (7 layers, Tier 2)
# → Run `pice status` to monitor progress
# → Run `pice logs auth-feature` for streaming output
```

**4.2 — Status monitoring**

```bash
pice status
# Shows all active, pending, and recently completed evaluations
# Including background evaluations with progress bars

pice logs auth-feature --follow
# Streams evaluation output in real-time (like `tail -f`)
# Shows layer completions, seam check results, confidence updates
```

**4.3 — Completion notification**

When a background evaluation completes:

```bash
# Terminal notification (default)
# → macOS: osascript notification
# → Linux: notify-send
# → Fallback: bell character + stdout message

# Configurable in .pice/config.toml:
[notifications]
on_complete = "terminal"        # terminal, none
# Future: slack, webhook, email
```

**4.4 — Multiple concurrent evaluations**

Multiple background evaluations can run simultaneously (different features, different branches). Each gets its own verification manifest and worktree space. `pice status` shows all of them.

### Implementation notes

- Background mode spawns the evaluation as a child process of the PICE daemon (not the CLI)
- The CLI writes the evaluation request to the daemon via the Unix socket (same architecture as MCP-Guard's daemon/bridge split)
- The daemon manages the evaluation lifecycle and writes results to the verification manifest
- `pice status` and `pice logs` read from the manifest and daemon logs respectively
- If the daemon isn't running, `pice evaluate --background` starts it automatically (zero-config start, same as MCP-Guard)

### Acceptance criteria

- [ ] `pice evaluate --background` returns immediately and runs evaluation in daemon
- [ ] `pice status` shows progress of all active evaluations
- [ ] `pice logs {feature} --follow` streams real-time output
- [ ] Terminal notification fires on completion (macOS + Linux)
- [ ] Multiple concurrent background evaluations work without conflict
- [ ] Daemon auto-starts if not running

---

## Feature 5: Web Dashboard for Evaluation Monitoring

**Priority:** P2 — Target v0.3
**Why:** For teams and for Tier 3 evaluations, a web dashboard provides better visibility than CLI output. Layer progress, seam check results, confidence curves, cost tracking, and the self-evolving metrics (v0.5) all benefit from visual presentation. This is not a v0.2 requirement — CLI is sufficient for initial adoption — but the architecture should be dashboard-ready from day one.

### Requirements

**5.1 — Dashboard serves from PICE daemon**

```bash
pice dashboard
# → Dashboard available at http://localhost:3141
# → Token: pice_dash_abc123...
```

The daemon serves a lightweight web UI on a configurable port. Authentication via a generated token (same pattern as MCP-Guard's `mcp-guard dashboard-token`).

**5.2 — Core views**

**Evaluation Overview** — All active, pending, and recent evaluations. Filterable by project, feature, status, date. Shows layer progress bars, overall confidence, and cost.

```
┌─────────────────────────────────────────────────────────────┐
│  Feature: Add user authentication          Tier 2  $0.47   │
│  ██████████░░░░  5/7 layers  •  4/6 seams  •  94.2%        │
│                                                             │
│  ✅ Backend (92.1%)  ✅ Database (93.4%)  ✅ API (95.1%)    │
│  ✅ Frontend (91.8%)  ⏳ Infrastructure...  ⬜ Deployment    │
│  ⬜ Observability                                           │
│                                                             │
│  Seam: API↔Frontend ⚠️ JWT handoff warning                  │
│  Gate: Infrastructure — pending human review                │
└─────────────────────────────────────────────────────────────┘
```

**Layer Detail** — Click into a layer to see: evaluation passes with confidence progression, seam check results, contract criteria with pass/fail per criterion, Arch Expert findings, model used and cost per pass.

**Confidence Curve** — Real-time chart showing confidence increasing per pass, with the correlated evaluator ceiling line at ~96.6% and the ADTS tier transition points marked.

**Seam Map** — Visual graph of layer→layer seam relationships with pass/fail/warning status on each edge. Click an edge to see the specific seam check results.

**Metrics Dashboard (v0.5)** — Check value scores, hit rates, false positive rates, cost per true positive, evaluation-to-production correlation. The self-evolving loop's data visualized over time.

**5.3 — Review gates in the dashboard**

When a review gate fires during a background evaluation, the dashboard shows a review panel with the same information as the CLI gate prompt. The reviewer can approve/reject/skip from the browser. This is especially useful for team workflows where the person running `pice evaluate` isn't the person who should approve the infrastructure layer.

### Implementation notes

- The dashboard is a static SPA (single HTML file with embedded JS/CSS) served by the daemon's HTTP server
- No build step required — the HTML is embedded in the PICE binary at compile time
- Data comes from the SQLite metrics engine and verification manifests via a simple JSON API
- WebSocket connection for real-time updates (layer completions, confidence changes, gate triggers)
- The confidence curve chart uses a lightweight charting library (Chart.js or similar, embedded)
- Authentication uses a bearer token generated by `pice dashboard` or `pice dashboard-token`

### Acceptance criteria

- [ ] `pice dashboard` starts web UI on configurable port with token auth
- [ ] Evaluation Overview shows all evaluations with layer progress and confidence
- [ ] Layer Detail shows per-pass confidence, seam checks, contract results
- [ ] Confidence Curve renders real-time with ceiling line and tier markers
- [ ] Seam Map visualizes layer relationships with status
- [ ] Review gates can be actioned from the dashboard
- [ ] Dashboard works with zero external dependencies (embedded SPA)

---

## Architecture Note: Headless Engine from Day One

All five features reinforce the same architectural principle: **PICE's Rust core is a headless orchestration engine, and all interfaces (CLI, dashboard, CI, future Slack/webhook) are thin adapters.**

The internal architecture should be:

```
┌──────────────────────────────────────────────────────┐
│  PICE Rust Core (headless engine)                     │
│                                                       │
│  Orchestrator → Stack Loops → Subagent spawning       │
│  Adaptive algorithms (SPRT, ADTS, VEC)                │
│  Verification manifest management                     │
│  SQLite metrics engine                                │
│  Gate state management                                │
│                                                       │
│  API: Unix socket (JSON-RPC or JSON-lines)            │
└───────────────────────┬──────────────────────────────┘
                        │
         ┌──────────────┼──────────────┐
         │              │              │
    ┌────▼────┐   ┌─────▼─────┐  ┌────▼────┐
    │  CLI    │   │ Dashboard │  │  CI     │
    │ adapter │   │ adapter   │  │ adapter │
    │         │   │ (HTTP+WS) │  │ (GH    │
    │ pice *  │   │ :3141     │  │ Action) │
    └─────────┘   └───────────┘  └─────────┘
```

This means:
- Background execution is natural — the daemon IS the engine, CLI just submits requests
- The dashboard reads the same data the CLI reads (verification manifests, SQLite)
- CI integration talks to the same API as the CLI
- Future adapters (Slack, webhook, VS Code extension) are trivial additions

**Do not build the CLI as the orchestrator with the dashboard bolted on.** Build the engine as a daemon, and build the CLI as the first adapter. This is the MCP-Guard architecture (daemon + bridge + CLI), and it's the right one for PICE too.

---

## Implementation Order

| Phase | Features | Target |
|-------|----------|--------|
| v0.2a | Feature 2 (workflow YAML) + Feature 3 (approval gates) | First — defines the pipeline structure everything else plugs into |
| v0.2b | Feature 1 (worktree isolation) | Second — enables parallel execution |
| v0.2c | Feature 4 (background execution) | Third — requires daemon architecture |
| v0.3  | Feature 5 (web dashboard) | After v0.2 ships and gets user feedback |

Feature 2 comes first because the workflow YAML is the configuration surface for Features 1, 3, and 4. The `parallel`, `worktree_isolation`, `review`, and background execution settings all live in the workflow file. Define the config surface, then implement the features it controls.

---

## Success Criteria (v0.2 complete)

- [ ] Teams can commit `.pice/workflow.yaml` and everyone runs the same evaluation pipeline
- [ ] Independent layers evaluate concurrently in isolated worktrees
- [ ] Infrastructure and deployment layers pause for human approval when configured
- [ ] `pice evaluate --background` returns immediately; `pice status` shows progress
- [ ] The Rust core is a headless daemon with CLI as an adapter, ready for dashboard addition in v0.3
- [ ] All features respect the adaptive algorithms (SPRT, ADTS, VEC) — UX patterns wrap the math, they don't replace it

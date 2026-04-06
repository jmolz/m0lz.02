# Claude Code Agent Teams: A Technical Deep-Dive for PICE

*PICE Research Library — Expanded research supporting the [PICE Roadmap](../roadmap.md)*

*For term definitions, see the [Glossary](../glossary.md).*

-----

## Executive Summary

Claude Code offers two distinct multi-agent systems that PICE can leverage — but with critical caveats. The stable **subagent system** (via the `Agent` tool) provides isolated, per-role context windows with configurable system prompts and model selection, making it directly applicable to Stack Loops and Arch Experts. The experimental **Agent Teams** feature (since v2.1.32, February 2026) adds peer-to-peer messaging and shared task lists but remains too unstable for production orchestration. For PICE, the recommended integration path is **CLI subprocess invocation** (`claude --bare -p`), which avoids SDK licensing concerns while providing full access to subagent orchestration via JSON-lines over stdio.

-----

## 1. Two Multi-Agent Systems, Not One

Claude Code contains two architecturally distinct delegation mechanisms that are often conflated in community discussions. Understanding the difference is essential for PICE's design.

### Subagents (stable, always available)

The workhorse system. When Claude invokes the `Agent` tool (renamed from `Task` in v2.1.63), it spawns an inline sub-process with its own isolated context window. Key characteristics:

- The subagent runs to completion; only the final result message returns to the parent
- Intermediate tool calls and reasoning stay encapsulated — the parent never sees them
- Subagents **cannot spawn their own subagents** (no recursive nesting)
- Three built-in types ship by default: `Explore` (read-only codebase search on Haiku), `Plan` (research-focused, inherits parent model), and a general-purpose agent with full tool access

The `Agent` tool's input schema:

```json
{
  "description": "string (3-5 word task description)",
  "prompt": "string (the task instructions)",
  "subagent_type": "string (agent definition name)",
  "model": "sonnet | opus | haiku (optional)",
  "run_in_background": "boolean (optional)",
  "resume": "string (agent ID, optional)"
}
```

### Agent Teams (experimental, unstable)

A fundamentally different architecture requiring `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` and Opus 4.6+. Each "teammate" is a **separate Claude Code instance** with its own full context window. A team lead spawns teammates, assigns tasks via a shared task list stored at `~/.claude/tasks/{team-name}/`, and teammates communicate through a **file-based mailbox system** (`~/.claude/teams/{teamName}/inboxes/{agentName}.json`) using JSON message queues with file-locking for concurrent safety.

Internal tools powering teams: `TeamCreateTool`, `TeammateTool`, `SendMessageTool`, `TaskCreateTool`, `TaskUpdateTool`.

### Comparison

| Feature | Subagents | Agent Teams |
|---|---|---|
| Status | **Stable**, always available | Experimental research preview |
| Communication | Parent→child→parent only | Peer-to-peer via mailbox |
| Context | Isolated; only final result returns | Fully independent per teammate |
| Custom system prompts | Yes, via `.claude/agents/` markdown files | Yes, via subagent type references |
| Per-agent model selection | Yes (`sonnet`/`opus`/`haiku`/full ID) | Yes, specified at spawn |
| Nesting | Subagents cannot spawn subagents | Teammates cannot spawn sub-teams |
| Token cost | Lower (1 additional context window per subagent) | **3–7× single session** (linear with team size) |
| Session resume | Supported | Not supported |
| Concurrent limit | Not formally documented; practical limit ~5–10 | **2–16 teammates** per team |

### Coordinator Mode

A third system, feature-flagged as `COORDINATOR_MODE`, also exists in the codebase. It creates an asymmetric architecture where the coordinator relinquishes all filesystem/shell tools and exclusively manages workers via `Agent` tool calls with `subagent_type: "worker"`. This pattern — a pure orchestrator with no direct execution capability — maps closely to PICE's proposed coordinator role.

### Why PICE uses subagents, not Agent Teams

Agent Teams' peer-to-peer messaging is unnecessary for PICE's hierarchical verification model. PICE's architecture is inherently parent→child: the Rust coordinator spawns each layer's evaluator, collects results, and makes decisions. Lateral communication between evaluators would compromise the context isolation that Stack Loops require.

More critically, Agent Teams have known stability issues:
- No session resume capability
- Task status lag between team lead and teammates
- Known race conditions (e.g., `getTeammateModeFromSnapshot called before capture`)
- Token cost 3–7× higher than subagent approach

These issues make Agent Teams unsuitable as a reliability layer in a production verification framework where correctness is the product.

-----

## 2. Custom Subagent Definition System

### Markdown files with YAML frontmatter

Custom subagent types are defined as Markdown files placed in `.claude/agents/` (project scope) or `~/.claude/agents/` (user scope). The frontmatter supports rich configuration:

```markdown
---
name: security-reviewer
description: Reviews code for security vulnerabilities
tools: Read, Grep, Glob, Bash
model: sonnet
permissionMode: default
maxTurns: 25
skills: [security-checklist]
mcpServers: [sentry]
isolation: worktree
---
You are a security expert. Focus on OWASP top 10...
```

### Key fields for PICE

**`model`** — Supports per-agent model assignment, including full model IDs like `claude-opus-4-6`. Critical for cost optimization: Haiku for simple layer checks ($0.001/pass), Sonnet for implementation review ($0.01/pass), Opus for complex coordination ($0.10/pass).

**`tools`** — Restricts what each agent can do. Critical for Stack Loop evaluation agents that should be read-only: `tools: [Read, Grep, Glob]` with no `Write`, `Edit`, or `Bash`. Prevents evaluators from modifying the code they're evaluating.

**`isolation: worktree`** — Gives agents their own git worktree, preventing file conflicts between concurrent agents. Useful if PICE ever adopts parallel layer execution.

**`maxTurns`** — Caps agent loop iterations. Essential for cost control — prevents a confused evaluator from running indefinitely. Recommended: 25 for standard evaluation, 10 for simple checks, 50 for complex Tier 3 analysis.

**`skills`** and **`mcpServers`** — Let each Arch Expert access domain-specific tools and knowledge bases. A RunPod expert could connect to a RunPod MCP server for real-time deployment status.

The body of the markdown file becomes the agent's **system prompt** — enabling the dynamic specialist contexts that Arch Experts require.

### Resolution priority

Subagent definitions follow a priority order: managed settings (org-wide) → `--agents` CLI flag → `.claude/agents/` → `~/.claude/agents/` → plugin directories. Custom agents can override built-in agents by sharing the same name — PICE could replace the default `Explore` agent with a domain-specialized version.

-----

## 3. The Claude Agent SDK

### Two SDKs, different licenses

The Claude Agent SDK exists in two language implementations with critically different licensing:

**Python SDK** (`claude-agent-sdk` on PyPI, repo `anthropics/claude-agent-sdk-python`) — Standard **MIT License**. The full permissive text granting rights to use, copy, modify, merge, publish, distribute, sublicense, and sell. Copyright 2025 Anthropic, PBC. PyPI classifies it as `OSI Approved :: MIT License`.

**TypeScript SDK** (`@anthropic-ai/claude-agent-sdk` on npm, repo `anthropics/claude-agent-sdk-typescript`) — **Proprietary**. npm license field reads "SEE LICENSE IN README.md." LICENSE.md contains a single line: `© Anthropic PBC. All rights reserved. Use is subject to Anthropic's Commercial Terms of Service.`

Both SDKs' README files contain identical language: use is governed by Anthropic's Commercial Terms of Service, except where a specific component's LICENSE file indicates otherwise. The Python wrapper code is MIT; the bundled CLI binary in both packages is proprietary.

> **Full licensing analysis:** [SDK Licensing](sdk-licensing.md)

### How the SDK works

Both SDKs **spawn the Claude Code CLI as a subprocess** and communicate via **JSON-lines over stdio** — newline-delimited JSON objects streaming bidirectionally. This is structurally similar to PICE's JSON-RPC over stdio provider architecture, though the wire format differs (JSON-lines vs. JSON-RPC 2.0 framing).

The TypeScript SDK's core `query()` function accepts inline agent definitions:

```typescript
import { query, AgentDefinition } from "@anthropic-ai/claude-agent-sdk";

const q = query({
  prompt: "Review the authentication module comprehensively",
  options: {
    allowedTools: ["Read", "Glob", "Grep", "Agent"],
    agents: {
      "security-reviewer": {
        description: "Security vulnerability specialist",
        prompt: "You are a security expert. Identify OWASP top 10 issues...",
        tools: ["Read", "Grep", "Glob"],
        model: "sonnet"
      },
      "arch-reviewer": {
        description: "Architecture pattern specialist",
        prompt: "You are an architecture expert. Evaluate SOLID principles...",
        tools: ["Read", "Grep", "Glob"],
        model: "opus"
      }
    }
  }
});
```

The Python SDK exposes an abstract `Transport` base class (`connect()`, `write()`, `read_messages()`, `close()`) that could bridge custom protocols.

-----

## 4. Integration Options for PICE

Four options exist, ranked by feasibility for an open-source project:

### Option A: TypeScript SDK directly

Import `@anthropic-ai/claude-agent-sdk` in PICE's TypeScript layer. Define `AgentDefinition` objects for each PICE role. Stream `SDKMessage` objects for real-time progress. Use hooks (`PreToolUse`, `PostToolUse`, `SubagentStart`, `SubagentStop`) for control flow.

**Pros:** Richest API, typed interfaces, lifecycle hooks.
**Cons:** Proprietary license. Bundling or depending on this package creates licensing concerns for an open-source project.
**Verdict:** Not recommended for PICE.

### Option B: CLI subprocess (recommended)

Spawn `claude --bare -p` as a subprocess from Rust. Use:
- `--output-format stream-json` for streaming JSON-lines output
- `--agents <json>` for dynamically generated agent definitions
- `--resume <session_id>` for session continuity

**Pros:** No compile-time dependency on proprietary packages. Same integration as any program invoking any CLI tool. The TypeScript SDK itself does this internally.
**Cons:** PICE must parse JSON-lines output directly. No typed interfaces.
**Verdict:** Recommended. Cleanest licensing posture for open-source distribution.

### Option C: Python SDK bridge

Use the MIT-licensed `claude-agent-sdk` Python package. Declare as an optional dependency (`pip install pice[claude]`). The Python SDK's `Transport` base class could bridge Rust↔Claude Code over a custom protocol.

**Pros:** MIT license is compatible with any open-source license. Full SDK capabilities.
**Cons:** Requires a Python bridge layer in a Rust project. The bundled CLI binary is still proprietary.
**Verdict:** Good alternative if PICE adds a Python layer. Declare as optional dependency.

### Option D: MCP Server Mode

Run `claude mcp serve` to expose Claude Code's tools via JSON-RPC 2.0 over stdio — directly compatible with PICE's architecture. However, this mode does not support agent team orchestration and doesn't pass through Claude Code's own MCP server connections.

**Pros:** Native JSON-RPC 2.0 compatibility.
**Cons:** No subagent orchestration. No agent definitions.
**Verdict:** Too limited for PICE's needs.

-----

## 5. Concrete Integration Architecture

### How PICE orchestrates a Stack Loop evaluation

```
PICE Rust Core (sole orchestrator)
│
├─ Spawn: claude --bare -p --output-format stream-json --agents <json>
│  │
│  ├─ Subagent: Backend evaluator
│  │  ├─ tools: [Read, Grep, Glob]  (read-only)
│  │  ├─ model: haiku               (cost-appropriate)
│  │  ├─ maxTurns: 25               (cost-capped)
│  │  └─ prompt: "Evaluate backend layer against these criteria..."
│  │  └─ Returns: PASS/FAIL + findings (JSON)
│  │
│  ├─ Seam check: Backend↔Database
│  │  └─ prompt: "Verify integration contracts..."
│  │
│  ├─ Subagent: Database evaluator
│  │  └─ (same pattern, database-specific contract)
│  │
│  └─ Subagent: Infrastructure evaluator
│     ├─ model: sonnet              (more complex analysis)
│     └─ prompt: includes Arch Expert + seam criteria
│
├─ [Claude-side evaluation complete]
│
├─ Spawn: OpenAI API call (GPT adversarial evaluator)
│  └─ Same contract criteria, independent context
│
├─ ADTS: Compute divergence between Claude and GPT results
│  ├─ D < τ_low  → HALT (Tier 1)
│  ├─ D moderate → Additional targeted passes (Tier 2)
│  └─ D > τ_high → Escalate + VEC (Tier 3)
│
└─ PICE merges results → layer verdict + seam verdict + confidence
```

### How PICE spawns Arch Experts

PICE constructs `AgentDefinition` objects at runtime from architecture discovery results:

```
pice plan "add user auth"
│
├─ Architecture Discovery
│  └─ Scan project files → detect technologies
│
├─ Expert Generation
│  └─ Construct JSON agent definitions dynamically:
│     {
│       "runpod-expert": {
│         "description": "RunPod Serverless specialist",
│         "prompt": "<dynamically generated from runpod.toml + handler.py>",
│         "tools": ["Read", "Grep", "Glob"],
│         "model": "sonnet"
│       }
│     }
│
├─ Pass to Claude Code via --agents <json>
│  └─ No .claude/agents/*.md files written (ephemeral, clean)
│
└─ Evaluation proceeds via Stack Loop
```

The `--agents` CLI flag and SDK `agents` parameter offer agent definition without filesystem mutation — cleaner for ephemeral evaluation contexts where agents should not persist between runs.

### Streaming output parsing

PICE parses the JSON-lines stream from Claude Code to:
- Track subagent progress in real-time
- Extract intermediate results for the ADTS divergence calculation
- Detect completion and collect final verdicts
- Monitor token usage for cost tracking
- Capture timing data for the metrics engine

Each line is a self-contained JSON object. PICE filters for result messages, tool use events, and error conditions.

### Hooks for control flow

The Claude Code hook system provides programmatic control points:

- **`PreToolUse`** — Before each tool invocation. PICE can use this to enforce read-only constraints.
- **`PostToolUse`** — After each tool invocation. PICE can capture tool results for seam analysis.
- **`SubagentStart`** — When a subagent is spawned. PICE logs start time and configuration.
- **`SubagentStop`** — When a subagent completes. PICE captures the result and updates the Bayesian posterior.
- **`TaskCompleted`** — When the overall task finishes. PICE triggers the ADTS decision logic.

When using CLI subprocess integration (Option B), hooks are configured via `.claude/settings.json` or command-line flags rather than programmatic callbacks.

-----

## 6. What PICE Can and Cannot Do with Claude Code

### Can do: Stack Loops

Subagents deliver the isolation Stack Loops require:
- Each spawned agent gets a fresh, isolated context window
- Only the final result returns — intermediate reasoning stays encapsulated
- Layer N evaluator cannot be contaminated by layer N-1 reasoning
- `tools` restriction enables read-only evaluation
- `maxTurns` caps runaway evaluation loops
- `model` enables cost-appropriate model selection per layer

### Can do: Arch Experts

The custom agent definition system serves Arch Experts well:
- Dynamic `AgentDefinition` objects constructed at runtime from architecture discovery
- Per-expert system prompts with project-specific context
- Per-expert model assignment (Haiku for simple, Sonnet for complex)
- Per-expert tool restrictions (read-only for evaluation, full for implementation)
- `skills` and `mcpServers` for domain-specific tools and knowledge

### Cannot do: Cross-provider adversarial evaluation

The Agent SDK only supports Anthropic models. There is no cross-provider support through the subagent system. PICE must orchestrate dual-model adversarial evaluation at its own layer:
- Claude-side evaluation via Claude Code subagents
- GPT-side evaluation via separate OpenAI API connection
- PICE's Rust core merges results and runs ADTS/VEC algorithms

This is architecturally clean — it keeps cross-provider logic independent of any single vendor's agent system. PICE is the decision engine; Claude Code is one of its execution substrates.

### Cannot do: Recursive nesting

Subagents cannot spawn their own subagents. All agent orchestration must happen at a single level — PICE's coordinator must be the sole parent. This is not a limitation for Stack Loops (which require flat orchestration) but would prevent, e.g., an Arch Expert from delegating sub-tasks to its own specialist agents.

### Cannot do: Agent Teams for reliability

Agent Teams' instability (no session resume, race conditions, task status lag) makes them unsuitable for a production verification framework. If PICE needs lateral agent communication in the future, it should implement this at its own orchestration layer.

-----

## 7. Community Patterns Validating PICE's Architecture

### The "plan → parallelize" two-step

The dominant workflow in the Claude Code community. Use plan mode first (read-only analysis), then hand the plan to agents for execution. This maps directly to Stack Loops: verify the plan at one layer before committing tokens to implementation at the next.

John Kim's "30 Tips for Claude Code Agent Teams" (March 2026): "If you try to do security, performance, and test coverage all in the same context, the agent gets biased by whatever it finds first." This **bias isolation** property is exactly what Stack Loops deliver through subagent context isolation.

### Domain-based agent specialization

Already standard practice. PubNub's production pipeline uses three sequential agents:
- `pm-spec` (read-heavy, produces specifications)
- `architect-review` (produces ADRs)
- `implementer` (gets write tools)

Each with scoped tool access. The community's `awesome-claude-code-subagents` repository contains 100+ specialized agents installable as plugins. Architecture-specific agents already exist: the `feature-dev` plugin ships `code-explorer`, `code-architect`, and `code-reviewer` agents with distinct prompts and tool restrictions.

### Token cost as primary constraint

Community measurements show 3–7× token usage for agent teams vs. single sessions, with some workflows hitting 15× standard usage. The March 2026 "quota exhaustion crisis" (Reddit threads with 330+ comments about "20× max usage gone in 19 minutes") demonstrates that cost control is essential, not optional.

### The 3–5 teammate sweet spot

Community consensus on diminishing returns. John Kim: "Anything more than three feels like overkill." Official docs recommend 5–6 tasks per teammate. For PICE, this aligns with the convergence analysis: the Krogh-Vedelsby decomposition shows ensemble improvement requires diversity, not count, and the correlated evaluator ceiling caps effective independent evaluators at ~3.

-----

## 8. Cost Control Strategy

PICE enforces cost discipline through multiple mechanisms:

**Model tiering.** Match model capability to task complexity:
- Haiku (~$0.001/pass): Simple layer checks, syntax validation, basic seam checks
- Sonnet (~$0.01/pass): Implementation review, Arch Expert evaluation, complex seam analysis
- Opus (~$0.10/pass): Coordination, complex Tier 3 analysis, adversarial assumption mining

**ADTS-driven pass allocation.** The three-tier architecture naturally optimizes:
- ~70% of evaluations: 2 passes (Tier 1) — $0.002 with Haiku
- ~25% of evaluations: 3–5 passes (Tier 2) — $0.04 with Sonnet
- ~5% of evaluations: 5+ passes (Tier 3) — $0.70 with Opus
- Expected: ~$0.046/evaluation

**`maxTurns` per subagent.** Hard cap on agent iterations. Default 25; configurable per layer.

**Check value scoring.** The self-evolving loop (v0.5) automatically deprioritizes low-value, high-cost checks and promotes high-value, low-cost ones.

**Budget guardrails.** Alert at 50%, 90%, 100% of per-feature evaluation budget. Configurable in `.pice/config.toml`.

-----

## 9. Future: Conditional Agent Teams Adoption

If Claude Code's Agent Teams feature stabilizes — specifically:
- ✅ Session resume capability
- ✅ Race condition fixes
- ✅ Consistent task status propagation
- ✅ Production-grade reliability

Then PICE could adopt Agent Teams for **parallel Tier 3 layer evaluation** — running multiple layer evaluators concurrently rather than sequentially. This would reduce wall-clock time for full-stack evaluation without changing the architecture (each teammate is still an isolated evaluator, results still merge at the PICE coordinator).

This is a watch-and-wait item, not a planned integration. Monitor the `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS` feature flag and Claude Code release notes for stabilization signals.

-----

*See also: [SDK Licensing](sdk-licensing.md) | [Seam Blindspot](seam-blindspot.md) | [Convergence Analysis](convergence-analysis.md) | [Glossary](../glossary.md)*

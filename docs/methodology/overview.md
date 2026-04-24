# PICE Methodology Overview

## What is PICE?

PICE is a structured methodology for AI-assisted coding. It replaces ad-hoc prompting
with a formal lifecycle that produces measurably better code. The acronym stands for
the four phases every change passes through:

1. **Plan** -- Research the codebase, design the approach, negotiate a contract with
   testable success criteria.
2. **Implement** -- Execute the plan in a fresh session with strict context isolation.
3. **Contract** -- The machine-readable agreement embedded in every plan that defines
   what "done" looks like.
4. **Evaluate** -- Grade the implementation against the contract using dual-model
   adversarial evaluation.

Each phase feeds the next. The plan produces the contract. The contract governs
implementation. The evaluation grades against the contract. Nothing is implicit.

## Why Structured AI Coding Matters

Unstructured AI coding -- sometimes called "vibing" -- works like this: open a chat,
describe what you want, accept whatever comes back, fix the obvious errors, repeat.
It produces working code often enough to feel productive. It also produces:

- Implementations that drift from the original intent
- Inconsistent architecture across features
- No way to measure whether the AI helped or hurt
- Self-evaluation bias when the same model checks its own work

PICE addresses each of these. The plan anchors the intent. Context isolation prevents
the implementation session from being biased by planning rationale. The contract
makes success criteria explicit and testable. Dual-model evaluation eliminates
single-model blind spots.

The result is a workflow where every feature has a paper trail: what was planned, what
was built, and how it scored against objective criteria.

## The Four Phases

### Plan

The developer describes what they want to build. An AI agent researches the codebase,
maps dependencies, identifies affected files, and produces a detailed plan. The plan
includes a JSON contract with scored criteria that define the evaluation bar.

The planning session ends when the developer approves the plan and its contract.

See [Plan Phase](plan.md) for details.

### Implement

A fresh AI session receives the plan file, the project's CLAUDE.md, and access to the
codebase. It does not receive the planning conversation. This context isolation is
deliberate -- implementation should follow the plan, not the reasoning that produced it.

The AI implements the plan step by step, running validation (tests, lints, type checks)
after each change.

See [Implement Phase](implement.md) for details.

### Contract

The contract is a JSON structure embedded in the plan file under a `## Contract`
heading. It specifies the feature name, evaluation tier, pass threshold, and an array
of criteria -- each with a name, numeric threshold, and a validation command.

The contract is written during planning and enforced during evaluation. It is the
single source of truth for what the implementation must achieve.

See [Contract Format](contract.md) for details.

### Evaluate

Evaluation runs one or more AI models against the implementation. The evaluators see
only three things: the contract JSON, the git diff, and the project's CLAUDE.md. They
never see the planning conversation or implementation session.

PICE uses dual-model adversarial evaluation as a key differentiator. Claude grades each
contract criterion on a 1-10 scale. A second model from a different family -- GPT-5.5
by default -- challenges the approach itself: design tradeoffs, unstated assumptions,
failure modes. Different model families have different blind spots, so cross-model
evaluation catches issues that single-model review misses.

The Rust core recomputes pass/fail from raw scores. It does not trust any provider's
verdict. This separation of scoring and enforcement is intentional.

See [Evaluation System](evaluate.md) for details.

## Dual-Model Adversarial Evaluation

This is the mechanism that distinguishes PICE from simple AI code review:

- **Contract grading** (Claude): Formal, per-criterion scoring against the contract.
  Each criterion gets a 1-10 score. The Rust core checks whether every score meets its
  threshold.
- **Design challenge** (GPT-5.5 / Codex): An independent model critiques the approach
  itself. It looks for questionable design decisions, unstated assumptions, missed edge
  cases, and better alternatives. This is not scored against the contract -- it surfaces
  issues the contract might not cover.

The two evaluators run in parallel. Their results are synthesized into a unified report.
If the adversarial provider fails (missing API key, network error, timeout), evaluation
degrades gracefully to single-model mode with a warning.

## WISC Context Management

PICE uses the WISC framework to manage AI context windows effectively:

- **Write** -- Capture key context in persistent files (plans, contracts, CLAUDE.md)
  rather than relying on conversation history.
- **Isolate** -- Each phase runs in its own session. Planning context does not leak
  into implementation. Implementation context does not leak into evaluation.
- **Select** -- Give each session only the context it needs. Evaluators get the
  contract, diff, and CLAUDE.md -- nothing else.
- **Compress** -- Handoff files capture session state in a compact form that the next
  session can consume without replaying the full conversation.

WISC is not a tool feature. It is a discipline that PICE enforces through its session
lifecycle.

## Evaluation Tiers

The tier system scales evaluation rigor to match the scope of the change:

| Tier | Scope | Evaluation |
|------|-------|------------|
| 1 | Bug fixes, simple changes | Single Claude evaluator |
| 2 | New features, integrations | Claude + Codex in parallel |
| 3 | Architectural changes | Claude agent team (4 evaluators) + Codex at maximum depth |

Tiers are declared in the contract during planning. The developer and AI negotiate the
appropriate tier based on the scope of the change.

## Further Reading

- [Plan Phase](plan.md) -- Research, design, and contract negotiation
- [Implement Phase](implement.md) -- Context-isolated execution from plan files
- [Contract Format](contract.md) -- JSON contract structure and criteria design
- [Evaluation System](evaluate.md) -- Dual-model adversarial evaluation in depth

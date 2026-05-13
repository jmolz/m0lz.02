# Plan Phase

## Purpose

The Plan phase is where a feature goes from a one-line description to a detailed,
actionable specification with machine-readable success criteria. The output is a plan
file -- a Markdown document that contains everything the implementation session needs
to build the feature, plus a JSON contract that defines how the result will be
evaluated.

Planning and implementation happen in separate sessions. This is a core PICE principle:
the session that designs the approach must not be the same session that implements it.
Context isolation prevents the implementation from being biased by the planning
rationale instead of following the plan itself.

## What Makes a Good Plan

A good plan is self-contained. A developer (or AI) reading only the plan file, the
project's project guidance, and the codebase should have everything needed to implement the
feature correctly. The plan should include:

### Research

- Which files are affected and why
- How the existing code is structured in the relevant area
- What patterns the codebase already uses that the implementation should follow
- Dependencies between the changes (what must be built first)

### File Analysis

- Specific files to create, modify, or delete
- For modifications: which functions, structs, or modules are affected
- Import and dependency changes required

### Dependency Mapping

- Internal dependencies between the planned changes
- External dependencies (new crates, packages, APIs)
- Order of operations: what must be built before what

### Implementation Steps

- Numbered, concrete steps that can be followed sequentially
- Each step should be small enough to validate independently
- Validation commands after each step (run tests, type check, lint)

## Contract Negotiation

Every plan includes a JSON contract under a `## Contract` heading. The contract is
negotiated during the planning session:

1. The AI proposes a contract based on the feature scope
2. The developer reviews the criteria, thresholds, and tier
3. Either party can adjust criteria, add validation commands, or change thresholds
4. The final contract is saved in the plan file

The contract negotiation is the most important part of planning. Vague criteria like
"code quality is good" are useless. Good criteria are specific, testable, and tied to
validation commands that can be run automatically.

See [Contract Format](contract.md) for the full contract specification.

## Tier Selection

The tier determines how rigorously the implementation will be evaluated. It is declared
in the contract and should match the scope of the change:

| Tier | When to Use | Evaluation Depth |
|------|-------------|------------------|
| 1 | Bug fixes, config changes, simple refactors | Single Claude evaluator -- contract grading only |
| 2 | New features, integrations, non-trivial changes | Claude evaluator + Codex adversarial review in parallel |
| 3 | Architectural changes, core refactors, new subsystems | Claude agent team (4 evaluators) + Codex at maximum reasoning depth |

Tier 1 is fast and cheap. Tier 3 is thorough and expensive. Most feature work is
Tier 2. When in doubt, tier up rather than down -- under-evaluating a complex change
costs more than the extra evaluation time.

## Fresh Session Principle

The plan is created in one AI session. Implementation happens in a separate, fresh
session. This is non-negotiable in PICE.

Why? Because the planning session accumulates reasoning context: alternatives
considered, tradeoffs discussed, dead ends explored. If this context leaks into
implementation, the AI may take shortcuts based on the discussion rather than following
the plan. Worse, if the same session later evaluates the work, it already "knows" why
decisions were made -- eliminating the independent judgment that evaluation requires.

PICE enforces this by running `pice plan` and `pice execute` as separate commands,
each of which creates and destroys its own provider session.

## Example Plan Structure

````markdown
# Feature: Add Rate Limiting to API

## Overview

Add configurable rate limiting to all API endpoints using a token bucket
algorithm. Rate limits are defined per-endpoint in the configuration file.

## Research

- `src/server/middleware.rs` -- existing middleware chain, new limiter slots in here
- `src/config/mod.rs` -- configuration loading, needs new `[rate_limit]` section
- `Cargo.toml` -- may need `governor` crate for token bucket implementation

## Implementation Steps

1. Add `governor` dependency to Cargo.toml
2. Create `src/server/rate_limit.rs` with token bucket middleware
3. Add `[rate_limit]` section to config schema in `src/config/mod.rs`
4. Wire rate limit middleware into the server middleware chain
5. Add unit tests for token bucket logic
6. Add integration test for rate-limited endpoint
7. Update project guidance with rate limiting documentation

## Contract

```json
{
  "feature": "API Rate Limiting",
  "tier": 2,
  "pass_threshold": 7,
  "criteria": [
    {
      "name": "Tests pass",
      "threshold": 7,
      "validation": "cargo test"
    },
    {
      "name": "Rate limit middleware exists",
      "threshold": 7,
      "validation": "test -f src/server/rate_limit.rs"
    },
    {
      "name": "Configuration support",
      "threshold": 7,
      "validation": "cargo test config::tests::rate_limit"
    },
    {
      "name": "No lint warnings",
      "threshold": 8,
      "validation": "cargo clippy -- -D warnings"
    }
  ]
}
```
````

Note: The contract JSON must be inside a fenced code block (` ```json `) under a
level-2 `## Contract` heading. The PICE CLI parser looks for exactly this structure.

## Running the Plan Phase

```bash
pice plan "add rate limiting to the API"
```

This launches a Claude Code session via the provider protocol. The AI researches the
codebase, drafts the plan, negotiates the contract with the developer, and writes the
final plan file. The developer can steer the plan interactively before approving it.

## What Happens Next

Once the plan is approved and saved, the developer runs `pice execute <plan-path>` to
begin the [Implement Phase](implement.md). The plan file is the handoff artifact
between the two phases -- nothing else carries over.

## Further Reading

- [PICE Overview](overview.md) -- The full lifecycle
- [Contract Format](contract.md) -- Contract JSON specification
- [Implement Phase](implement.md) -- How plans are executed
- [Evaluation System](evaluate.md) -- How implementations are graded

---
description: Capture durable workflow lessons after accepted feature work lands
argument-hint: <plan-file-or-feature>
---

# Self-Heal: Post-Merge Workflow Hardening

## Objective

After a feature branch or worktree has been merged into `main`, update durable
project guidance from accepted evidence. This command is retrospective hygiene,
not part of implementation or evaluation.

## Preconditions

- The feature worktree has been merged into `main`.
- The merge validation or release gate has passed, or the remaining limitation
  is explicitly documented.
- You are working from the main checkout, not an unaccepted feature worktree.

## Process

1. Read the named plan, handoff, review notes, and recent git history for:
   `$ARGUMENTS`
2. Identify lessons that should persist across future work:
   - command instructions that were stale or ambiguous;
   - AGENTS.md guidance that did not match the accepted implementation;
   - tests, fixtures, or tripwires that would have caught the issue earlier;
   - docs that users or maintainers would reasonably trust.
3. Propose the smallest durable updates needed.
4. Apply only non-production project-file changes after reviewing the diff.
5. Run the focused validation that proves the guidance or tripwire is accurate.

## Guardrails

- Do not run this before merge; unaccepted branch behavior is not durable truth.
- Do not commit, push, deploy, publish, rotate secrets, or change production
  configuration without explicit user approval.
- Do not copy local command wrappers into public templates if they depend on
  private skills or machine-specific paths.
- Prefer executable tripwires over prose when a regression can be mechanically
  detected.

## Output

Report:

- what accepted evidence drove the update;
- files changed;
- validation run and result;
- any follow-up that still needs human approval.

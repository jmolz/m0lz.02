---
description: Retrospective on the most recent plan and sessions, harvesting durable lessons into rules, tripwires, commands, and workflows
argument-hint: [plan-file-path-or-feature-name]
---

# Self-Heal: Capability Retrospective

## Mission

Look at the work just completed: the plan, implementation, adversarial review
cycles, `/review`, `/evaluate`, validation, and any live debugging. Ask what
the repo learned and how to make that lesson stick.

`/self-heal` is not a code review. `/review` and `/evaluate` already do that.
This command captures durable workflow improvements:

1. Rule updates that prevent recurrence.
2. Tripwire tests or grep checks for missed failure modes.
3. Command or skill updates that close process gaps.
4. Documentation drift fixes tied to current code or incident evidence.
5. Process patterns worth preserving for future PICE CLI and workflow-orchestrator work.

## When to run

- After a multi-cycle `/evaluate` or adversarial review.
- After a live publishing, update, unpublish, benchmark, or release incident.
- After `/review` catches a class of defect, not just one instance.
- After a feature touches multiple subsystems and exposes workflow drift.
- Periodically on the most recent active plan.

Do not run `/self-heal` for trivial fixes, one-pass clean evaluations, or docs
changes with no broader lesson.

## Step 1: Identify Source Material

If `$ARGUMENTS` is provided, use it as the plan file path or feature name.
If not provided, default to the most recently modified plan across Claude and
Codex plan folders:

```bash
ls -t .claude/plans/*.md .codex/plans/*.md 2>/dev/null | head -1
```

Read the source plan in full. Pay attention to:

- Contract criteria and thresholds.
- Adversarial review findings.
- Each `/evaluate` or review cycle, especially resolved HIGH/MEDIUM items.
- Commit messages or plan cycle notes that identify defect classes.
- Runtime logs, publishing state, smoke notes, or DB checks that changed the diagnosis.

Use current checkout facts, not stale plan assumptions.

## Step 2: Extract Patterns

For each defect class or workflow miss, classify it:

| Class | Question | Typical Output |
| --- | --- | --- |
| Greppable code pattern | Could a regex or structural test catch this next time? | New regression/tripwire test |
| Test-quality issue | Did a passing test fail to assert the real contract? | Update `/review` checks or add test coverage |
| Workflow drift | Did the agent skip a repo-specific procedure? | Update `.claude/commands`, `.codex/commands`, or `.agents/skills/source-command-*` |
| Subsystem rule | Is the lesson scoped to CLI, DB, publishing, update, unpublish, benchmark, or evaluation? | Update `.claude/rules/*` and `.codex/rules/*` |
| Cross-cutting rule | Does it affect every agent session? | Update `AGENTS.md` and mirror to `CLAUDE.md` if present |
| Contract design gap | Did the contract miss what the evaluator caught? | Update planning/evaluation command guidance |

Every lesson must have a forensic anchor:

- File and line, route, test, log string, query, or run id.
- The cycle or incident that exposed it.
- One sentence explaining what the rule prevents.

Avoid aspirational rules. If the lesson cannot be anchored to evidence, do not
promote it into durable guidance.

## Step 3: Decide Where It Lives

Use this precedence:

1. Mechanical tripwire or regression test when the failure is machine-checkable.
2. Command or skill update when the failure was process drift.
3. `.claude/rules/*` and `.codex/rules/*` when the lesson is subsystem-scoped.
4. `AGENTS.md` and `CLAUDE.md` only for cross-cutting rules that should load in
   every session.

Keep Claude and Codex surfaces synchronized. If you update a Claude command or
rule, mirror the durable content into the Codex wrapper/source skill/rule unless
there is a deliberate tool-specific difference.

## Step 4: Write The Changes

Make the retrospective changes now while the evidence is fresh. Do not defer
durable process fixes to a follow-up unless user judgment is required.

For each change:

| Change Type | Required |
| --- | --- |
| New tripwire test | Add focused coverage and register it in review guidance if it is part of the review harness |
| New cross-cutting rule | Add forensic anchor and keep `AGENTS.md`/`CLAUDE.md` concise |
| Rule update | Mirror between `.claude/rules/*` and `.codex/rules/*` when both exist |
| Command update | Keep `.claude/commands/*`, `.codex/commands/*`, and `.agents/skills/source-command-*` aligned |
| Documentation correction | Cite the current file, route, migration, test, log, or query that proves the correction |

Do not commit, push, deploy, or mutate production configuration from this
command unless the user explicitly asks.

## Step 5: Verify

Run the narrow validation that matches the changes:

- Command/rule-only changes: verify file presence, frontmatter, and parity.
- New tests: run the new test file and any directly related suite.
- Review-harness changes: run the command's own inventory checks where feasible.
- Repo-wide behavioral changes: use the validation chain required by the active
  plan or `AGENTS.md`.

If a new tripwire fails on day zero, either fix the live violation or document a
specific allowlist rationale. Do not leave a vacuous test that scans no files.

## Step 6: Report

Return a concise report:

```markdown
## /self-heal: <feature or plan>

### Source Material
- Plan/run/logs reviewed:
- Cycles reviewed:
- Defect classes captured:

### Changes Made
- Tripwire tests:
- Rule updates:
- Command/skill updates:
- Documentation corrections:

### Open Follow-Ups
- Items needing user judgment:

### Validation
- Commands run:
- Result:
```

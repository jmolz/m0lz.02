---
description: Implement a feature from a plan file
argument-hint: <path-to-plan.md>
---

# Execute: Implement from Plan

## Plan File

Read the entire plan at: `$ARGUMENTS`

---

## Step 1: Read and Understand

Read the ENTIRE plan before writing any code. Understand:

- All tasks and their dependency order
- Files to create vs modify
- Validation commands to run
- Testing strategy and expected outcomes
- Environment requirements

**Do NOT start implementing until you have the full picture.**

---

## Step 2: Set Up Worktree

All feature execution happens in an isolated git worktree. Check the plan's `## Worktree` section for the branch name and path.

```bash
git branch --show-current
git worktree list
```

**If NOT already in the correct worktree**, create one:

```bash
# From the main repo directory
git worktree add {worktree-path} -b {branch-name}
cd {worktree-path}
```

**If a worktree already exists** for this feature (e.g., resuming work), navigate to it:

```bash
cd {worktree-path}
```

### Verify Preconditions

```bash
git status
git branch --show-current
```

Check:

- You are in the worktree directory, on the feature branch
- Working tree is clean (or only has expected changes)
- Any required env vars are set (check `.env` or `.env.example`)
- Dependencies are installed (run the install command — worktrees share `node_modules` but may need setup)

---

## Step 3: Execute Tasks in Order

Work through each task sequentially, respecting dependency ordering.

### For each task:

1. **Read** the target file(s) before modifying — never edit blind
2. **Read** any "Must-Read Files" referenced in the plan
3. **Implement** the change following project patterns from CLAUDE.md
4. **Verify** after each file change:
   - Syntax is correct
   - Imports resolve
   - Types check (if applicable)
   - No obvious regressions

Fix errors immediately — do not accumulate them.

---

## Step 4: Run Validation (per plan)

Execute ALL validation commands from the plan in the specified order.

For each level:

1. Run the command
2. If it fails: read the error, fix the root cause, re-run
3. Only proceed to the next level when the current one passes

---

## Step 5: End-to-End Testing

If the plan includes E2E or manual validation steps:

1. Start the application
2. Walk through every user journey described
3. Verify expected outcomes
4. Document any deviations

---

## Step 6: Completion Report

```markdown
## Execution Report: {Feature Name}

### Tasks Completed

- [x] Task 1: {description} — {files changed}
- [x] Task 2: {description} — {files changed}

### Files Created

- `path/to/file` — {purpose}

### Files Modified

- `path/to/file` — {what changed}

### Validation Results

- Lint: PASS / FAIL
- Types: PASS / FAIL
- Unit tests: PASS / FAIL ({N} passed)
- Integration: PASS / FAIL
- E2E: PASS / FAIL

### Manual Verification

{What was tested manually and results}

### Notes

{Any deviations from plan, unexpected findings, follow-up work}
```

## Step 7: Contract Evaluation

If the plan contains a `## Contract` section, prompt the user to run adversarial evaluation:

```
Implementation complete. This plan has a Tier {N} contract with {X} criteria.

Next step: /evaluate {path-to-plan.md}

This will spawn a fresh evaluator sub-agent that grades the implementation
against the contract criteria. The evaluator does NOT see this conversation —
only the contract, code diff, and CLAUDE.md.
```

Do not run `/evaluate` automatically — the user may want to do manual testing first.

---

## Rules

- Follow project conventions from CLAUDE.md at all times
- If a task is ambiguous, use existing codebase patterns as guidance
- If you must deviate from the plan, document why
- Never skip validation steps
- Fix failing tests by fixing implementation, not by weakening tests

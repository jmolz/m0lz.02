---
name: "source-command-handoff"
description: "Capture session state for the next agent or session to continue"
---

# source-command-handoff

Use this skill when the user asks to run the migrated source command `handoff`.

## Command Template


# Handoff: Write Session State

## Relationship to `/prime`

`/prime` starts sessions. `/handoff` ends them. They are a pair:

1. New session begins -> `/prime` reads HANDOFF.md (among other things) and orients
2. Session does work
3. Session ending -> `/handoff` writes HANDOFF.md for the next `/prime` to pick up

Not every session needs a handoff. If the work is self-contained and fully committed, there's nothing to hand off. Use `/handoff` when there's meaningful in-progress state the next session needs.

## When to Use

- Before ending a long session where work will continue
- Before hitting context limits (be proactive, not reactive)
- When switching from one phase to another
- Instead of relying on `/compact` for critical ongoing work
- **Not needed** when work is fully committed and there's no pending state

## Process

### 1. Reconcile with Existing State

**If a HANDOFF.md already exists**, read it first. This is the previous session's understanding of what was in-progress and what came next. You'll reconcile it against reality in the next step.

If no HANDOFF.md exists, skip to Step 2.

### 2. Gather State & Cross-Reference Git

Run these to understand what's actually happened:

```bash
git status
git diff --stat HEAD
git log --oneline -20
git branch --show-current
git worktree list
```

**If a previous HANDOFF.md existed**, cross-reference its "In Progress / Next Steps" items against the git log:

- Walk through each `- [ ]` item from the old handoff
- Check if commits in the git log address that item (look at commit messages, files changed)
- Mark items as **completed** if the git log shows the work was done
- Mark items as **stale/irrelevant** if the codebase has moved past them (e.g., the feature was scrapped, the approach changed, the file was deleted)
- Keep items as **still in-progress** only if they genuinely remain unfinished

This is the critical step. The goal is to produce a truthful snapshot, not to accumulate a backlog.

### 3. Analyze This Session

Review everything that happened in the current session:

- What was the original goal?
- What has been completed?
- What is still in progress or blocked?
- What key decisions were made and WHY?
- What files were created or modified?
- What errors were encountered and resolved?
- What dead ends were explored?

### Worktree State

If working in a git worktree, capture:

- **Worktree path**: output of `pwd`
- **Feature branch**: output of `git branch --show-current`
- **Main repo path**: first line of `git worktree list`
- **Merge status**: is the feature ready to merge, or still in progress?

This is critical — the next session needs to know where to `cd` to resume work.

### 4. Write HANDOFF.md

Save to `HANDOFF.md` in the project root (the main repo, not the worktree). Use this template as the structure — fill in every section with real data from reconciliation + current session:

@.codex/templates/HANDOFF-template.md

**Reconciliation rules for each section:**

- **Recently Completed (Last Session):** Items finished in THIS session only. Do not carry forward completed items from prior handoffs — they live in git history now. Cap at ~5 most relevant items. If nothing was completed this session, omit this section.
- **In Progress / Next Steps:** Only items that are genuinely unfinished. If the old handoff had 8 next steps and 6 were committed, those 6 disappear. The 2 remaining carry forward alongside any new items from this session.
- **Dead Ends:** Carry forward from previous handoff if still relevant (the next agent still needs to avoid them). Drop dead ends about approaches to problems that have since been solved.
- **Key Decisions:** Carry forward decisions that still constrain future work. Drop decisions about completed features unless they set precedent for ongoing work.

### 5. Confirm

1. Confirm file path
2. Suggest the next session's first prompt:
   ```
   Read HANDOFF.md and continue from where the previous session left off.
   ```
3. If uncommitted changes exist, suggest `/commit` first

## Quality Criteria

A good handoff should:

- Let a fresh agent continue without clarifying questions
- Be under 100 lines
- Reflect the **current** state, not a cumulative log of all sessions
- Include enough "why" that the next agent makes the same decisions
- Explicitly list dead ends to prevent wasted work
- Have a concrete "first action"
- Have zero completed items in the "In Progress" section

## Anti-patterns

- Don't include full file contents — reference paths
- Don't include conversation transcripts — summarize
- Don't be vague ("fix the bug") — be specific ("fix SSE reconnection in `src/hooks/useSSE.ts`")
- Don't skip Dead Ends — this prevents the most common wasted effort
- Don't carry forward completed tasks — if git shows it's done, it's done
- Don't accumulate "In Progress" items across sessions without verifying they're still relevant
- Don't keep dead ends about problems that have been solved

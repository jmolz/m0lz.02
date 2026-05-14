---
description: Orient on the codebase before starting any work
---

# Prime: Load Project Context

## Objective

Build a mental model of this codebase so you can make informed decisions about what to build next.

## Process

### 1. Read Core Documentation

Read these files (skip any that don't exist):

- `AGENTS.md` — global rules and conventions
- `PRD.md` or `.codex/PRD.md` — product requirements
- `README.md` — project overview
- Any architecture docs referenced in AGENTS.md

### 2. Explore Project Structure

```bash
tree -L 3 -I 'node_modules|__pycache__|.git|dist|build|.next|venv*|.venv' 2>/dev/null || find . -maxdepth 3 -not -path '*/node_modules/*' -not -path '*/.git/*' -not -path '*/dist/*' -not -name '*.pyc' | head -80
```

Read key config files:

- `package.json`, `pyproject.toml`, `Cargo.toml` (whichever exists)
- `tsconfig.json`, `vite.config.*`, `next.config.*` (if present)
- Database config/schema files

### 3. Check Git History (Long-Term Memory)

```bash
git log -15 --oneline 2>/dev/null
git status 2>/dev/null
git branch --show-current 2>/dev/null
git worktree list 2>/dev/null
```

### 4. Identify Key Entry Points

Based on structure, read the first 50-80 lines of:

- Main entry points (index.ts, main.py, app.py, etc.)
- Core route/handler files
- Key model/schema definitions

### 5. Check for Pending Work

Look for:

- `HANDOFF.md` — previous session left notes
- `.codex/plans/*.md` — pending implementation plans
- Open TODOs in recent commits

**If HANDOFF.md exists**, cross-reference its open items against `git log` to verify they're still relevant. The `vet-handoff` hook may have already flagged stale items at session start — check for "HANDOFF.md Vet" warnings in the session transcript and act on them. Remove or mark completed any items that git history shows have been addressed. If all items are resolved, note that the handoff is fully resolved and can be deleted.

## Output Report

Summarize in under 300 words with bullet points:

### Project Overview

- What this project does
- Tech stack and key libraries
- Current state (early, mid-build, mature)

### Architecture

- How code is organized
- Key patterns and conventions
- Important files and directories

### Current State

- Active branch and recent work
- Any uncommitted changes
- Active worktrees and their feature branches (if any)
- Pending plans or handoffs
- Handoff status: which items are resolved vs. still open, whether HANDOFF.md should be cleaned up or deleted

### Recommended Next Action

- What phase from the PRD comes next (if greenfield)
- What the most impactful next task is
- Any blockers or setup needed first

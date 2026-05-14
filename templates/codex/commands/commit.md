---
description: Create a standardized, detailed git commit
---

# Commit Changes

Your git log is long-term memory. Future agent sessions will read these commits to understand project history. Make them count.

## Process

### 1. Review Changes

```bash
git status
git diff --stat HEAD
git diff HEAD
git ls-files --others --exclude-standard
```

### 2. Stage Relevant Files

Add changed and new files relevant to the current work.

**Do NOT stage:**
- `.env` or credential files
- Large binary files
- Files unrelated to the current task
- IDE config or OS files

### 3. Create Commit Message

**Format:**
```
tag(scope): concise description of WHAT changed

WHY this change was made. Include context that isn't obvious
from the diff — the reasoning, the alternatives considered,
the user impact.

[Optional: Context section for AI layer changes]
[Optional: Fixes #123, Closes #456]
```

**Tags:**
- `feat:` — new capability or feature
- `fix:` — bug fix
- `refactor:` — code restructure, no behavior change
- `docs:` — documentation only
- `test:` — test additions or fixes
- `chore:` — build, CI, tooling, dependency updates
- `perf:` — performance improvement
- `style:` — formatting, no logic change

**Scope** (optional): the primary area affected
```
feat(auth): add OAuth2 login flow
fix(api): handle null response from payment gateway
refactor(db): extract query builder into utility module
```

### 4. Include AI Layer Changes

If ANY AI context files were modified, add a `Context:` section:

```
feat(dashboard): add real-time analytics widget

Added WebSocket-based live metrics with 5-second refresh.
Chose recharts over d3 for consistency with existing charts.

Context:
- Updated AGENTS.md with WebSocket patterns
- Added .codex/rules/websocket.md for connection conventions
- Created .codex/docs/analytics-architecture.md
```

**What counts as AI context changes:**
- `AGENTS.md` — global rules
- `.codex/rules/` — on-demand rules
- `.codex/commands/` — slash commands
- `.codex/docs/` — reference documents
- `.codex/plans/` — implementation plans

### 5. Commit

```bash
git add <files>
git commit -m "message"
```

## Why This Matters

- Future `/prime` commands read `git log` to understand recent work
- Standardized tags make history scannable
- The WHY section prevents re-discovery of dead ends
- Context section tracks AI layer evolution alongside code

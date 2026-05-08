---
name: debug
description: "Use this skill when diagnosing bugs, errors, test failures, or unexpected behavior. Triggers: 'debug', 'fix bug', 'not working', 'error', 'failing test', 'broken', 'unexpected behavior', 'investigate issue', 'stack trace', 'crash'. Provides a systematic debugging methodology to find root causes efficiently."
---

# Debug Skill

## When to Use
The agent should read this skill when troubleshooting any bug, error, or unexpected behavior.

## Debugging Methodology

### Step 1: Reproduce
Before fixing anything, confirm you can see the problem:

```bash
# Run the failing test
{test command} -- --grep "test name"

# Start the app and trigger the error
{dev command}
# Then reproduce the exact steps

# Check logs
tail -f logs/*.log 2>/dev/null
```

If you can't reproduce it, you don't understand it yet. Gather more information.

### Step 2: Read the Error

Actually read the full error:
- **Error message**: What does it literally say?
- **Stack trace**: Which file and line threw? Trace the call chain.
- **Error type**: TypeError? NetworkError? ValidationError? Each means something different.
- **Context**: What was the input? What state was the app in?

### Step 3: Form a Hypothesis

Based on the error, hypothesize the root cause. Common patterns:

| Symptom | Likely Cause |
|---------|-------------|
| `undefined is not a function` | Wrong import, missing method, typo |
| `null reference` | Async timing, optional field not checked |
| `connection refused` | Service not running, wrong port/host |
| `401/403` | Auth token expired, wrong credentials, missing header |
| `404` | Wrong URL, route not registered, typo in path |
| `500` | Unhandled exception in handler, DB query failure |
| Test passes alone, fails in suite | Shared state, mock pollution, test ordering |
| Works locally, fails in CI | Environment difference, missing env var, different DB state |
| Intermittent failure | Race condition, timing dependency, flaky external service |

### Step 4: Narrow Down

Use binary search — don't change 10 things at once:

```bash
# Check if the issue is in a specific file
git stash           # Remove your changes
# Test — does the bug exist in clean state?
git stash pop       # Bring changes back

# Check when it broke
git log --oneline -20
git bisect start
git bisect bad      # Current commit is broken
git bisect good abc123  # This commit was working
# Git walks you to the breaking commit
```

### Step 5: Fix the Root Cause

- Fix the actual problem, not the symptom
- Don't add a try/catch to hide an error — understand why it happens
- If the fix is in one place but the same pattern exists elsewhere, check those too
- If the bug came from a missing guard, consider if other callers have the same gap

### Step 6: Verify and Prevent

After fixing:
1. Confirm the original reproduction case now passes
2. Add a test that would have caught this bug
3. Check if AGENTS.md or rules should be updated to prevent this pattern
4. Run full validation to ensure no regressions

## Anti-Patterns

- ❌ Changing random things until it works (shotgun debugging)
- ❌ Adding `console.log` everywhere without a hypothesis
- ❌ Fixing the symptom without understanding the cause
- ❌ Disabling a test that fails instead of fixing the code
- ❌ Catching and swallowing exceptions to make errors disappear

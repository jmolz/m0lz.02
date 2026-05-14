# Agent Teams Playbook

Prompts for orchestrating multi-agent parallel work. Each prompt creates a team of specialists that work simultaneously.

**Cost awareness:** Agent teams use significantly more tokens than single sessions. Each teammate is a separate AI instance. Use them when parallel work genuinely saves time -- not for simple sequential tasks.

**Best practices:**

- Start with 3-5 teammates (more adds coordination overhead without proportional benefit)
- Tell the lead to wait for teammates before synthesizing
- Each teammate should own different files to avoid conflicts
- Monitor and steer -- check in with teammates and redirect if needed

## Adversarial Evaluation

### Tier 2: Dual-Model Adversarial Review

Use for Tier 2 contracts -- new features, integrations, schema changes. Runs the configured primary evaluator and configured adversarial review in parallel. See [evaluation methodology](../methodology/evaluate.md) for details on the tier system.

```
We just finished implementing [FEATURE]. The contract is in [PLAN PATH].

1. Launch the adversarial review in the background
2. Spawn the configured primary evaluator as the contract evaluator:
   Read the contract JSON from [PLAN PATH]. For EACH criterion, run the
   validation command, try to break the feature, and score 1-10. You are NOT
   the implementer. Do NOT be generous. A 7 means functional but insufficient.
3. Collect adversarial results
4. Synthesize both into a single evaluation report
```

### Tier 3: Full Agent Team Review

Use for Tier 3 contracts -- architectural changes. Spawns separate evaluators that challenge the implementation from different angles, plus a parallel adversarial review for cross-model coverage.

Spawn four teammates:

1. **Contract evaluator** -- formal scoring against criteria
2. **Convention auditor** -- check project guidance and rules compliance
3. **Regression hunter** -- run full test suite, check callers of changed files
4. **Edge case breaker** -- find ways to break the feature with malformed inputs, missing auth, boundary values

Rules: evaluators work independently, read only contract + diff + evaluation guidance, and share findings at the end.

## Code Review

### Multi-Angle PR Review

Spawn three reviewers working independently:

1. **Security reviewer** -- auth gaps, injection risks, exposed secrets
2. **Performance reviewer** -- N+1 queries, missing pagination, bundle size
3. **Correctness reviewer** -- logic errors, edge cases, race conditions

Have them share findings and resolve contradictions before synthesizing.

### Pre-Deploy Review

Spawn four teammates:

1. **Test coverage analyst** -- run suite, identify untested paths
2. **Security scanner** -- full scan on recent changes
3. **Dependency checker** -- outdated packages, known CVEs
4. **Config validator** -- env vars, build config, deployment settings

Output a go/no-go deployment decision.

## Feature Implementation

### Full-Stack Feature Build

Spawn three teammates:

1. **Backend developer** -- API endpoints, database, server logic
2. **Frontend developer** -- UI components, state, API integration
3. **Test engineer** -- unit, component, and integration tests

Rules: agree on API contract before implementing, no overlap in file ownership, tests written as features are built.

### Parallel Module Development

For independent modules, assign one teammate per module. They share a common types/interfaces file -- coordinate on that first, then build independently.

## Debugging

### Competing Hypotheses

For hard-to-diagnose bugs, spawn 4 teammates each investigating a different hypothesis:

1. Data issue (wrong records, corruption)
2. Timing/race condition (async operations, stale cache)
3. Auth/permissions (wrong token, expired session)
4. Recent regression (git bisect the last 5 commits)

Have them debate each other. When one finds evidence disproving another's theory, they share it.

## Architecture & Design

### Architecture Review

Spawn four teammates:

1. **Dependency analyst** -- map module dependencies, find circular imports
2. **Complexity assessor** -- find hotspots (highest complexity, most-changed files)
3. **Pattern consistency checker** -- compare how different parts handle the same concerns
4. **Scalability reviewer** -- identify bottlenecks at 10x load

### Technology Evaluation

Spawn three teammates, each advocating for a different option plus a devil's advocate. Each advocate must honestly acknowledge weaknesses. Output a decision matrix.

## Template

```
Create an agent team to [OBJECTIVE]. Spawn [N] teammates:

1. [Role A] -- [responsibilities, owned files, first action]
2. [Role B] -- [responsibilities, owned files, first action]
3. [Role C] -- [responsibilities, owned files, first action]

Coordination rules:
- [Who goes first / dependencies]
- [How they communicate findings]
- [File ownership (no overlap)]
- [When to require plan approval]
- [Validation to run at the end]

Wait for all teammates to finish before synthesizing.
```

### Key Principles

1. **Each teammate owns different files** -- two teammates editing the same file causes overwrites
2. **Start with research, then implement** -- teams work best when exploration adds value
3. **3-5 teammates is the sweet spot** -- more adds overhead without proportional benefit
4. **Tell the lead to wait** -- otherwise it starts implementing instead of delegating
5. **Give enough context in the prompt** -- teammates do not inherit your conversation history

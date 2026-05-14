# Agent Teams Playbook

Prompts for orchestrating multi-agent parallel work in Claude Code. Each prompt creates a team of specialists that work simultaneously and communicate with each other.

**Prerequisites:** Agent teams must be enabled in your `~/.codex/settings.json` (already done if you followed the install guide). Restart Claude Code after enabling.

**Cost awareness:** Agent teams use significantly more tokens than single sessions. Each teammate is a separate Claude instance. Use them when parallel work genuinely saves time — not for simple sequential tasks.

**Best practices across all scenarios:**

- Start with 3-5 teammates (more adds coordination overhead without proportional benefit)
- Tell the lead to wait for teammates before synthesizing
- Each teammate should own different files to avoid conflicts
- Use `Shift+Down` to cycle between teammates and interact directly

---

## Adversarial Evaluation

### Dual-Model Adversarial Review (Tier 2)

Use for Tier 2 contracts — new features, integrations, schema changes. Runs the configured primary evaluator and configured adversarial review in parallel for cross-model coverage.

```
We just finished implementing [FEATURE]. The contract is in [PLAN PATH].

Step 1: If `[evaluation.adversarial].enabled = true`, read `[evaluation.adversarial]` and launch the configured adversarial review in the background first:
node "$HOME/.codex/plugins/cache/openai-codex/codex/1.0.4/scripts/codex-companion.mjs" \
  task --background --model {model} --effort {effort} \
  "Adversarially evaluate against the contract in [PLAN PATH]. Use only the contract, diff/status, and AGENTS.md."

Step 2: While the adversarial review runs, spawn the configured primary evaluator:
Read the contract JSON from [PLAN PATH]. For EACH criterion, run the
validation command, try to break the feature, and score 1-10. You are NOT
the implementer. Do NOT be generous. A 7 means "meets the bar" — not
"pretty good." Score lower when in doubt.

Step 3: Collect the adversarial task result

Step 4: Synthesize both into a single evaluation report:
- Contract Evaluation (primary): {N}/{total} criteria passed (list each with score)
- Design Challenge ({provider} {model} {effort}): Critical / Consider / Acknowledged findings
- Overall: PASS / FAIL
```

### Contract-Based Adversarial Review (Tier 3)

Use this for Tier 3 contracts — architectural changes, new pipeline phases, or complex features where maximum evaluation rigor is needed. Spawns separate configured primary evaluators that challenge the implementation from different angles, **plus** a parallel configured adversarial review for cross-model coverage.

```
We just finished implementing [FEATURE]. The contract is in [PLAN PATH].

Step 1: If `[evaluation.adversarial].enabled = true`, read `[evaluation.adversarial]` and launch the configured adversarial review in the background first:
node "$HOME/.codex/plugins/cache/openai-codex/codex/1.0.4/scripts/codex-companion.mjs" \
  task --background --model {model} --effort {effort} \
  "Adversarially evaluate against the contract in [PLAN PATH]. Use only the contract, diff/status, and AGENTS.md."

Step 2: Create a configured primary evaluator team to perform an adversarial evaluation.
Spawn four teammates:

1. Contract evaluator — read the contract JSON from the plan file. For EACH
   criterion, run the validation command, try to break the feature, and score
   1-10. You are NOT the implementer. Do NOT be generous. A 7 means "meets
   the bar" — not "pretty good." Score lower when in doubt.
2. Convention auditor — use AGENTS.md and the supplied diff/status. Check
   changed files against project conventions. Flag pattern violations,
   naming inconsistencies, missing auth guards, and incorrect error handling.
   Do not report speculative issues — only real violations.
3. Regression hunter — run the full test suite and regression tests. Check
   that no existing functionality is broken. Read the git diff and identify
   every file that changed — then find all callers/consumers of those files
   and verify they still work correctly.
4. Edge case breaker — your ONLY job is to find ways to break the feature.
   Malformed inputs, missing auth, concurrent requests, empty states,
   boundary values, null data. Try every edge case you can think of against
   the running application. Report specific reproduction steps for failures.

Rules:
- Evaluators work independently — no shared context from implementation
- Each evaluator reads only: the contract, git diff, and AGENTS.md
- The contract evaluator's pass/fail on each criterion is the authority
- Edge case breaker should actively TRY to make things fail, not just
  read code and speculate
- All evaluators share findings at the end for cross-referencing

Step 3: Collect the adversarial task result

Step 4: Synthesize ALL findings into a single evaluation report:
- Contract Evaluation (primary): {N}/{total} criteria passed (list each with score)
- Convention Audit (primary): {N} violations found (Critical/Warning/Suggestion)
- Regression Status (primary): {N} tests passing, {N} failing
- Edge Cases (primary): {N} failures found (with reproduction steps)
- Design Challenge ({provider} {model} {effort}): Critical / Consider / Acknowledged findings
- Overall: PASS / FAIL

Note: The configured adversarial review challenges the APPROACH — was this the
right design? What assumptions does it depend on? This is complementary to
the primary team's execution-level evaluation. A critical design challenge
that cannot be justified = overall FAIL.
```

---

## Code Review

### Multi-Angle PR Review

```
Create an agent team to review the changes since the last commit. Spawn three reviewers:

1. Security reviewer — focus on auth gaps, injection risks, exposed secrets,
   missing input validation, and unsafe data handling
2. Performance reviewer — focus on N+1 queries, unnecessary re-renders,
   missing pagination, expensive computations, and bundle size impact
3. Correctness reviewer — focus on logic errors, edge cases, error handling,
   race conditions, and whether the code actually does what it claims

Have each reviewer work independently, then share findings with each other.
If one reviewer's finding contradicts another's, they should discuss and
reach consensus. Wait for all three to finish before synthesizing a
final prioritized review.
```

### Pre-Deploy Review

```
Create an agent team for a pre-deployment review. Spawn four teammates:

1. Test coverage analyst — run the test suite, identify untested code paths
   in recently changed files, and flag any tests that are skipped or flaky
2. Security scanner — use the security-scan agent's checklist to do a full
   scan focused on anything touched in the last 5 commits
3. Dependency checker — check for outdated packages, known vulnerabilities,
   and any dependency changes that could affect production
4. Config validator — verify environment variables, build config, deployment
   settings, and that .env.example matches what the code references

Each teammate reports findings independently. Synthesize into a
go/no-go deployment decision with specific blockers if any exist.
```

---

## Feature Implementation

### Full-Stack Feature Build

```
Create an agent team to implement [FEATURE DESCRIPTION]. Spawn three teammates:

1. Backend developer — owns the API endpoints, database schema changes,
   and server-side logic. Start with the data model and work up to the API.
2. Frontend developer — owns the UI components, state management, and
   API integration. Start with the component structure and mock the API
   until the backend teammate confirms the endpoints are ready.
3. Test engineer — owns all tests. Write unit tests for backend logic,
   component tests for frontend, and integration tests for the API.
   Coordinate with both teammates on interfaces and expected behaviors.

Rules:
- Backend and frontend teammates must agree on the API contract before
  implementing. Share the request/response shapes early.
- No teammate edits files owned by another teammate.
- Test engineer writes tests as the other two build, not after.
- Require plan approval before any teammate starts implementing.
- Wait for all teammates to finish before reporting.
```

### Parallel Module Development

```
Create an agent team to build these three independent modules in parallel:

1. [Module A] — [description, key files, what it does]
2. [Module B] — [description, key files, what it does]
3. [Module C] — [description, key files, what it does]

Each teammate owns one module. They share a common types/interfaces file —
coordinate on that first before building independently. If any teammate
needs something from another's module, they message each other directly
rather than implementing a workaround.

Run the test suite after all three finish to verify nothing conflicts.
```

---

## Debugging

### Competing Hypotheses Investigation

```
We have a bug: [DESCRIBE THE BUG, SYMPTOMS, ERROR MESSAGES].

Create an agent team with 4 teammates to investigate different hypotheses
in parallel. Each teammate should:

1. State their hypothesis clearly
2. Gather evidence for and against it
3. Actively try to disprove their own hypothesis
4. Share findings with other teammates

Hypotheses to investigate:
- Teammate 1: This is a data issue (wrong data in the database,
  missing records, data corruption)
- Teammate 2: This is a timing/race condition issue (async operations,
  stale cache, event ordering)
- Teammate 3: This is an auth/permissions issue (wrong token, expired
  session, missing middleware)
- Teammate 4: This is a recent regression (something in the last 5
  commits broke it — use git bisect)

Have them debate each other. When one finds evidence that disproves
another's theory, they should share it. The surviving hypothesis with
the strongest evidence wins. Then implement the fix.
```

### Cross-Stack Debug

```
The [FEATURE] is broken. It spans frontend, backend, and database layers.

Create an agent team with 3 teammates:

1. Frontend debugger — trace the issue from the UI. Check network requests,
   response data, component state, error boundaries. Identify whether the
   frontend is sending the right request and handling the response correctly.
2. Backend debugger — trace the issue from the API. Check request parsing,
   service logic, database queries, response formatting. Add logging if needed.
3. Database investigator — check the data layer. Verify schema matches code
   expectations, check for missing indexes, run the relevant queries directly
   and compare results to what the API returns.

Each teammate traces the data flow from their layer's perspective. They
should share what they find at each boundary point (frontend→API, API→DB)
so the team can identify exactly where the data goes wrong.
```

---

## Architecture & Design

### Architecture Review

```
Create an agent team to review our current architecture. Spawn four teammates:

1. Dependency analyst — map all module dependencies, find circular imports,
   identify tightly coupled components, and visualize the dependency graph.
   Flag any module that imports from too many others.
2. Complexity assessor — find the most complex files (highest cyclomatic
   complexity), longest functions, deepest nesting, and most-changed files
   in git history. These are the maintenance hotspots.
3. Pattern consistency checker — compare how different parts of the codebase
   handle the same concerns (error handling, logging, auth, validation).
   Find inconsistencies and recommend standardization.
4. Scalability reviewer — identify bottlenecks that would break under 10x
   load. Check database queries, API pagination, caching strategy, and
   connection pooling.

Have them share findings with each other — the dependency analyst's
coupling issues may explain the complexity assessor's hotspots.
Synthesize into an architecture health report with prioritized
recommendations.
```

### Technology Evaluation

```
We need to decide on [TECHNOLOGY DECISION — e.g., "a message queue for
background jobs", "a caching layer", "a search engine"].

Create an agent team with 3 teammates, each advocating for a different option:

1. Advocate for [Option A — e.g., "Redis + BullMQ"] — research the pros,
   document the integration effort, estimate costs, find gotchas
2. Advocate for [Option B — e.g., "AWS SQS"] — same analysis
3. Devil's advocate — challenge both options. Find failure modes, scaling
   limits, migration costs, and vendor lock-in risks. Propose a third
   option if one exists.

Each advocate must honestly acknowledge their option's weaknesses, not
just sell the strengths. The devil's advocate should be genuinely adversarial.
Synthesize into a decision matrix with clear recommendation.
```

### New Project Scaffolding

```
Create an agent team to scaffold a new [PROJECT TYPE] project. Spawn 3 teammates:

1. Project architect — set up the directory structure, base configuration
   files (tsconfig, package.json, eslint, prettier), and core utilities
   (logging, error handling, env config). Follow conventions from AGENTS.md.
2. Infrastructure engineer — set up Docker, CI/CD (GitHub Actions),
   deployment config, database migrations, and environment variable management.
3. Documentation writer — create README.md with setup instructions,
   AGENTS.md with project conventions, and scoped rule files for
   the major subsystems.

Architect goes first. Infrastructure and docs teammates wait for the
base structure before starting. All three coordinate on the directory
layout and naming conventions.
```

---

## Refactoring

### Large-Scale Refactor

```
Create an agent team to refactor [WHAT NEEDS REFACTORING]. Spawn 4 teammates:

1. Planner — read the affected code, map all dependencies, and create a
   step-by-step refactoring plan. Each step must leave the codebase in a
   working state. Get plan approved before anyone starts implementing.
2. Implementer A — owns refactoring [AREA A / files A]. Follows the plan
   step by step, running tests after each change.
3. Implementer B — owns refactoring [AREA B / files B]. Same approach.
4. Validator — runs the full test suite after each major milestone.
   Flags any regressions immediately to the relevant implementer.
   Also verifies the refactored code follows the planned patterns.

Rules:
- No implementer starts until the planner's plan is approved.
- Each implementer commits after each successful step.
- Validator runs tests continuously, not just at the end.
- If a test breaks, the relevant implementer stops and fixes it immediately.
```

### Codebase Modernization

```
Create an agent team to modernize [SPECIFIC AREA]. Spawn 3 teammates:

1. Pattern migrator — update code patterns: [e.g., "convert class
   components to functional components with hooks", "migrate from
   callbacks to async/await", "replace moment.js with date-fns"]
2. Type safety improver — add TypeScript strict mode compliance,
   remove any types, add proper generics, fix type assertions
3. Test updater — update tests to match the new patterns, replace
   deprecated testing utilities, improve assertion specificity

Each teammate works on different files. Run the full test suite
after each major batch of changes. Coordinate on shared utilities
that multiple areas depend on.
```

---

## Testing

### Test Coverage Sprint

```
Create an agent team to improve test coverage. Spawn 4 teammates:

1. Coverage analyst — run the coverage report, identify the least-tested
   files, and prioritize by risk (most changed files with lowest coverage
   are highest priority). Create a task list for the test writers.
2. Unit test writer — write unit tests for core business logic, utilities,
   and service functions. Follow existing test patterns.
3. Integration test writer — write integration tests for API endpoints,
   database operations, and cross-module interactions.
4. Edge case hunter — review the codebase for unhandled edge cases: null
   inputs, empty arrays, boundary values, concurrent access, timeout
   scenarios. Write tests that verify these are handled.

Coverage analyst creates the plan first. The three test writers then
work in parallel on different files. Run the full suite at the end to
verify no conflicts.
```

### Pre-Release Test Blitz

```
Create an agent team to do a comprehensive test pass before release.
Spawn 5 teammates:

1. Happy path tester — test every core user journey end to end
2. Error path tester — test every failure scenario (network errors,
   invalid input, expired sessions, missing data)
3. Boundary tester — test limits (max input lengths, pagination edges,
   rate limits, file size limits, concurrent requests)
4. Auth tester — test every auth flow (login, logout, token refresh,
   permission denied, role-based access)
5. Data integrity tester — test that data operations are consistent
   (create then read, update then verify, delete then confirm gone,
   concurrent writes)

Each tester works independently. Share any bugs found with the whole
team so others can check if the bug affects their test area too.
Compile a final test report.
```

---

## Security

### Comprehensive Security Audit

```
Create an agent team for a full security audit. Spawn 4 teammates:

1. Secrets scanner — check for hardcoded credentials, leaked API keys,
   tracked .env files, private keys, and tokens in code or git history.
   Check git log for accidentally committed secrets that were later removed.
2. Injection auditor — check for SQL injection, XSS, command injection,
   path traversal, and SSRF vulnerabilities. Test every user input path.
3. Auth/authz auditor — verify every API endpoint has proper auth checks,
   test for privilege escalation, check session management, verify token
   handling, and test CORS configuration.
4. Dependency auditor — run vulnerability scans on all dependencies,
   check for known CVEs, verify packages are from trusted sources,
   and flag any dependencies that are unmaintained (no commits in 12+ months).

Each auditor should challenge the others' findings. A false positive
from one auditor should be caught by another. Compile a severity-ranked
report with specific remediation steps.
```

---

## Performance

### Full Performance Audit

```
Create an agent team for a performance audit. Spawn 4 teammates:

1. Frontend performance analyst — check bundle size, code splitting,
   lazy loading, image optimization, unnecessary re-renders, and
   client-side caching. Run lighthouse-style checks if tools are available.
2. API performance analyst — profile API endpoints for slow responses.
   Check for N+1 queries, missing pagination, unnecessary data fetching,
   and missing caching headers.
3. Database performance analyst — analyze slow queries, missing indexes,
   table scan patterns, connection pool sizing, and query plan analysis.
   Check schema design for read-heavy vs write-heavy optimization.
4. Infrastructure analyst — check deployment config for resource allocation,
   scaling settings, CDN configuration, compression, and caching layers.

Each analyst focuses on their layer but shares findings that cross
boundaries (e.g., a slow API caused by a missing DB index).
Prioritize findings by user impact. Synthesize into a performance
improvement roadmap ordered by effort vs impact.
```

---

## Documentation

### Documentation Overhaul

```
Create an agent team to overhaul our project documentation. Spawn 4 teammates:

1. Doc auditor — use the doc-auditor agent to catalog every .md file
   and verify accuracy against the actual codebase. Flag stale,
   inaccurate, and redundant docs.
2. README writer — rewrite the main README.md with accurate setup
   instructions, architecture overview, and contribution guide.
   Test every command in the README to make sure it works.
3. API documenter — generate accurate API documentation from the actual
   route files. Include request/response examples, auth requirements,
   and error formats for every endpoint.
4. Rules updater — update AGENTS.md and scoped rule files to match
   current codebase patterns. Remove outdated conventions and add any
   missing ones discovered during the audit.

Doc auditor goes first to identify what's wrong. The other three
then work in parallel to fix their respective areas. Cross-reference
each other's work to ensure consistency.
```

---

## Database

### Schema Migration Planning

```
Create an agent team to plan a database schema migration for [DESCRIBE CHANGE].
Spawn 3 teammates:

1. Schema designer — design the new schema. Consider data types, indexes,
   constraints, relationships, and backward compatibility. Write the
   migration SQL.
2. Migration risk assessor — analyze the impact. How many rows are affected?
   Will the migration lock tables? What's the rollback plan? How long will
   it take? Can it run without downtime?
3. Code impact analyst — find every file that references the affected tables
   or columns. Map exactly what code needs to change after the migration.
   Estimate the blast radius.

Require plan approval from all three before any implementation begins.
```

---

## Incident Response

### Post-Incident Analysis

```
We just had an incident: [DESCRIBE WHAT HAPPENED].

Create an agent team with 4 teammates for a post-mortem:

1. Timeline builder — reconstruct what happened from git history, deploy
   logs, and code changes. Build a minute-by-minute timeline.
2. Root cause analyst — trace the chain of events to find the underlying
   cause. Don't stop at the proximate cause — find the systemic issue.
3. Blast radius assessor — determine the full impact. What users were
   affected? What data was impacted? What systems were degraded?
   Check logs and data for secondary effects.
4. Prevention planner — based on findings from the other three, recommend:
   immediate fixes, short-term mitigations, and long-term systemic changes.
   Include specific tests, monitoring, and guardrails that would have
   caught this before it reached production.

Have all four share findings throughout. The prevention planner should
challenge the root cause analyst: "If we fix that, could this still
happen a different way?" Compile into a post-mortem document.
```

---

## Sprint & Project Planning

### Technical Breakdown

```
We need to implement [FEATURE/PROJECT]. Before we start coding,
create an agent team to do a thorough technical breakdown. Spawn 3 teammates:

1. Requirements analyst — break down the feature into specific, testable
   requirements. Identify ambiguities, edge cases, and decisions that
   need to be made before implementation.
2. Architecture planner — given the requirements, design how this fits
   into the existing codebase. What files change? What new modules are
   needed? What are the dependencies? Draw the data flow.
3. Effort estimator — given the architecture plan, break the work into
   tasks sized for individual PICE loops. Estimate complexity (low/med/high)
   for each task. Identify what can be parallelized and what's sequential.
   Flag risks that could blow up estimates.

Requirements analyst goes first. Architect works from the requirements.
Estimator works from the architecture. Each challenges the previous
teammate's work. Output a structured plan ready for execution.
```

### Dependency Upgrade Sprint

```
Create an agent team to handle a dependency upgrade sprint. Spawn 3 teammates:

1. Audit & prioritize — run pnpm outdated, check for vulnerabilities,
   and create a prioritized list: security fixes first, then breaking
   changes, then minor updates. Check changelogs for breaking changes.
2. Upgrade executor A — work through the first half of the priority list.
   Update one package at a time, run tests after each. Document any
   breaking changes encountered and how they were resolved.
3. Upgrade executor B — work through the second half. Same process.

Both executors should share breaking change findings — if one discovers
a pattern in how a library changed, the other should know. Run the
full test suite after both finish.
```

---

## Tips for Custom Scenarios

### Template for Any Agent Team Prompt

```
Create an agent team to [OBJECTIVE]. Spawn [N] teammates:

1. [Role A] — [specific responsibilities, files they own, what they do first]
2. [Role B] — [specific responsibilities, files they own, what they do first]
3. [Role C] — [specific responsibilities, files they own, what they do first]

Coordination rules:
- [Who goes first / dependencies between teammates]
- [How they communicate findings]
- [What files each teammate owns (no overlap)]
- [When to require plan approval]
- [What validation to run at the end]

Wait for all teammates to finish before synthesizing results.
```

### Key Principles

1. **Each teammate owns different files** — two teammates editing the same file causes overwrites
2. **Start with research/planning, then implementation** — agent teams are best when exploration adds value
3. **3-5 teammates is the sweet spot** — more adds overhead without proportional benefit
4. **Tell the lead to wait** — otherwise it starts implementing instead of delegating
5. **Use plan approval for risky tasks** — "Require plan approval before making any changes"
6. **Give enough context in the prompt** — teammates don't inherit your conversation history
7. **Monitor and steer** — check in with `Shift+Down`, redirect teammates that are off track

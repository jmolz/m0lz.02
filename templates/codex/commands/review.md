---
description: Review code changes for bugs, security issues, and improvements — includes cumulative regression suite
---

# Code Review

Perform a thorough code review of the current changes AND run the cumulative regression suite to ensure all previously built features still work.

## Phase 0: Contract Check

Before starting the standard review, check if the most recent plan has a contract:

```bash
# Find the most recently modified plan file
ls -t .codex/plans/*.md 2>/dev/null | head -1
```

If a plan file exists, read its `## Contract` section. If a contract is found:

1. Note the tier and criteria in the review output
2. After Phase 3 (Code Review), add a **Phase 3.5: Contract Evaluation** that spawns a fresh sub-agent to grade the implementation against the contract (see `/evaluate` for the full evaluator protocol)
3. Include the contract evaluation results in the final output

Phase 3.5 must follow `/evaluate`'s Validation Proof Discipline: a validation command passing is not enough unless the evaluator identifies the assertion, request-log check, fixture, or observable output that would fail if the criterion regressed. If the named validation target is too broad or does not directly cover the criterion, report the coverage gap instead of treating the criterion as proven.

If no contract exists, skip this and proceed normally. The contract evaluation is additive — it does not replace the standard code review phases.

---

## Phase 0.5: Database Migration Check

Before running tests, verify that database migrations are up to date AND applied. Schema changes without applied migrations cause runtime failures that tests won't catch.

<!-- CUSTOMIZE: Replace the commands below with your project's ORM/migration tool.
     Common examples:
       Drizzle:  pnpm db:generate --check / pnpm db:migrate
       Prisma:   npx prisma migrate status / npx prisma migrate deploy
       Django:   python manage.py showmigrations --plan | grep '\[ \]' / python manage.py migrate
       Rails:    bin/rails db:migrate:status | grep 'down' / bin/rails db:migrate
       Alembic:  alembic check / alembic upgrade head
       Knex:     npx knex migrate:status / npx knex migrate:latest
-->

### Step 1: Check for schema drift

```bash
# Check for new/untracked migration files
git status --short -- '{migrations-directory}' 2>/dev/null
# Check for uncommitted schema changes that might need a migration
git diff HEAD --name-only -- '{schema-files}' 2>/dev/null
```

If schema files were modified but no new migration file exists, run `{migration-generate-command}` to create one. Flag as **Critical** in the review output.

### Step 2: Apply migrations

If there are new migration files (untracked `??` or modified in the migrations directory), **run the migrations directly** — do NOT skip this or defer to the user:

```bash
{migration-apply-command}
```

The command must succeed before proceeding. If it fails, flag as **Critical**.

## Phase 1: Regression Suite

Run these tests FIRST to verify that all previously shipped features are intact. This suite grows with every feature — when you ship a feature, add its tests here. If any fail, flag them as **Critical** and investigate before proceeding with the code review.

<!-- CUSTOMIZE: Replace the test runner and file list below with your actual tests.
     This section should be maintained as a living document — every time you ship
     a feature or fix a bug, add its test(s) to the command below and document
     them in the table.

     Example test runners:
       npx jest tests/foo.test.ts tests/bar.test.ts --no-coverage
       npx vitest run tests/foo.test.ts tests/bar.test.ts
       pytest tests/test_foo.py tests/test_bar.py -v
       cargo test --test foo --test bar
-->

```bash
# Run all regression suite tests
{test-runner} \
  {test-file-1} \
  {test-file-2} \
  {test-file-3} \
  {additional-flags}
```

### What each test covers

<!-- CUSTOMIZE: Add a new section header for each feature milestone or sprint.
     Each row documents one test file so anyone can understand what breaks if it fails.
     The table format matches the Bloom project pattern for consistency.

     Example section:

     **Initial feature set (commit abc1234)**

     | Test File | Feature | What It Validates |
     | --------- | ------- | ----------------- |
     | `auth.signup.test.ts` (5 tests) | User signup | Email validation, password hashing, duplicate detection, welcome email, session creation |
     | `api.customers.test.ts` (8 tests) | Customer CRUD | Create, read, update, delete, list with pagination, search, org scoping, auth guards |
-->

**{Milestone or sprint name} ({commit hash or date})**

| Test File                 | Feature        | What It Validates                                  |
| ------------------------- | -------------- | -------------------------------------------------- |
| `{test-file}` ({N} tests) | {Feature name} | {Brief description of what behaviors are verified} |

### Source files these tests protect

<!-- CUSTOMIZE: List every source file that the regression tests exercise.
     This makes it easy to check: "I changed foo.ts — is it regression-protected?"

     Example:
     - `app/api/auth/route.ts` — signup, login, session management
     - `lib/email.ts` — transactional email sending
     - `lib/db/queries.ts` — customer CRUD queries
-->

- `{source-file}` — {what it does}

### Expected results

All tests should pass. If any fail after your changes:

1. Check if you modified the source files listed above
2. Read the failing test to understand what behavior it expects
3. Fix your code to preserve the expected behavior, or update the test if the behavior change is intentional

### Updating the regression suite

After running the regression suite and before finishing the review, check if any test files touched in this session are NOT already in the suite above. To find them:

```bash
# Compare test files modified in uncommitted changes against the suite list
git diff --name-only HEAD -- '{test-directories-glob}'
```

<!-- CUSTOMIZE: Replace the glob with your test file patterns.
     Examples:
       'tests/*.test.ts' '__tests__/**/*.test.ts'
       'tests/test_*.py'
       'spec/**/*_spec.rb'
       'tests/**/*.rs'
-->

For each test file that exercises a newly shipped or migrated feature and is NOT already in the regression suite:

1. **Add it to the test runner command** in the bash block above
2. **Add a row to the "What each test covers" table** with: file name, test count, feature name, what it validates
3. **Add any new source files to the "Source files these tests protect" list**
4. **Add a line to the output format** checklist in Phase 4

Also check all test directories — test files may live in multiple locations (e.g., `tests/`, `__tests__/`, `spec/`, `e2e/`).

This ensures the suite is always exhaustive: every feature we ship gets regression-protected automatically.

## Phase 2: Full Validation

After regression tests pass, run the full suite:

<!-- CUSTOMIZE: Replace with your project's actual validation commands.
     These should match the commands in your AGENTS.md or /validate command.
-->

```bash
{lint-command}
{test-command}
{build-command}
```

<!-- CUSTOMIZE: Set your expected baselines so drift is immediately visible.
     Example: "Expected baseline: 0 lint errors (12 pre-existing warnings), 847 tests passing, clean build."
-->

Expected baseline: {describe your known-good state — lint error count, test count, build status}

## Phase 3: Code Review of Current Changes

```bash
git branch --show-current
git status
```

**If on a feature branch (worktree)**, diff against main to see the full feature scope:

```bash
git diff main...HEAD
git diff main...HEAD --stat
```

**If on main**, diff against the last commit:

```bash
git diff HEAD
```

If reviewing a specific commit, check it out or diff against it.

### Focus Areas

1. **Logic errors** and incorrect behavior
2. **Edge cases** that aren't handled
3. **Null/undefined reference** issues
4. **Race conditions** or concurrency issues
5. **Security vulnerabilities**
6. **Resource management** — leaks, unclosed connections
7. **API contract violations**
8. **Caching bugs** — staleness, bad keys, invalid invalidation, ineffective caching
9. **Pattern violations** — check AGENTS.md and .codex/rules/ for project conventions

### Rules

- Use sub-agents to explore the codebase in parallel for efficiency
- Report pre-existing bugs found near the changed code — code quality matters everywhere
- Do NOT report speculative or low-confidence issues — conclusions must be based on actual code understanding
- If reviewing a specific git commit, note that local code may differ from that commit

## Phase 4: Output Format

### Migration Status

```
Schema Drift: NONE / DETECTED (tables/columns affected)
New Migrations: [list files] or NONE
Action: Run `{migration-generate-command}` then `{migration-apply-command}` or N/A
```

### Regression Suite Results

```
Regression Suite: PASS / FAIL

{Milestone name}:
  - {Feature name} ({N} tests): ✓ / ✗
  - {Feature name} ({N} tests): ✓ / ✗

Full Suite: X passing, Y failing
Lint: {error count} errors, {warning count} warnings
Build: PASS / FAIL
```

<!-- CUSTOMIZE: As you add tests to the regression suite in Phase 1,
     add a corresponding status line here so the output stays in sync.

     Example:
     Initial feature set:
       - User signup (5 tests): ✓ / ✗
       - Customer CRUD (8 tests): ✓ / ✗
       - Campaign email (6 tests): ✓ / ✗

     AI migration:
       - Chat integration (3 tests): ✓ / ✗
       - Command extraction (10 tests): ✓ / ✗
-->

### Contract Evaluation (if applicable)

```
Contract: {feature name} — Tier {N}
Evaluator: Isolated sub-agent (no implementation context)

| Criterion | Threshold | Score | Pass |
|-----------|-----------|-------|------|
| {name} | {T}/10 | {S}/10 | YES/NO |

Overall: PASS / FAIL
```

If no contract was found in the plan, output: `Contract: N/A — no contract in plan`

### Code Review Findings

Group findings by severity:

**Critical** — Must fix before merge (bugs, security, data loss)

- `file:line` — description of the issue and recommended fix

**Warning** — Should fix (performance, maintainability, pattern violations)

- `file:line` — description and suggestion

**Suggestion** — Consider improving (readability, minor optimizations)

- `file:line` — description and suggestion

**Positive** — What's done well (reinforce good patterns)

- Description of what was done right

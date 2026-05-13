# Brownfield Development Guide

Working with an existing codebase is different from starting fresh. The code already has patterns, conventions, tech debt, and implicit knowledge. This guide covers how to set up the PICE workflow in a project that already has code.

## Step 1: Reverse-Engineer the AI Layer

### Generate Rules

Run `pice init` in the project root to scaffold the PICE framework files. Then review and customize the generated project guidance to match your project's actual conventions.

Go through the generated rules and check:

- Are the detected patterns actually correct?
- Are there conventions the analysis missed?
- Are there "wrong" patterns in the codebase that should not be replicated?
- Does the architecture description match reality?
- Are all the important commands listed?

### Create On-Demand Rules

For large codebases, extract subsystem-specific conventions into `.claude/rules/`:

```
.claude/rules/
  frontend.md      -- paths: ["src/frontend/**", "**/*.tsx"]
  api.md           -- paths: ["src/api/**", "src/routes/**"]
  database.md      -- paths: ["src/db/**", "migrations/**"]
  auth.md          -- paths: ["src/auth/**", "src/middleware/auth*"]
  testing.md       -- paths: ["**/*.test.*", "**/*.spec.*"]
```

### Create Reference Docs

For complex areas, create deep-dive docs in `.claude/docs/`:

- Architecture deep dive
- Data model reference
- Integration guide for external services
- Deployment procedures

These stay out of the main context but are available to sub-agent scouts.

## Step 2: Build a Regression Test Harness

Before adding new features, establish a safety net.

### Catalog Existing Tests

Find all test files and run a coverage report to identify gaps. Ask yourself: what are the 5-10 things that absolutely cannot break? For each one, ensure there is at least an integration or E2E test. If not, create one before doing any new work.

### Create a Validation Workflow

Set up a full validation command that covers:

1. Lint + type check
2. Unit tests
3. Integration tests
4. E2E smoke tests for critical paths
5. Build verification

Run this after every PICE loop to catch regressions.

## Step 3: Work in PICE Loops

The same workflow works for brownfield -- but with more emphasis on codebase research and [contract-based evaluation](../methodology/contract.md).

### Enhanced Priming

For brownfield, `pice prime` should:

- Read project guidance and on-demand rules
- Check git history for recent patterns
- Look at the specific area you are about to modify
- Understand the existing tests around that area

### Planning with Contracts

When running `pice plan`, sub-agents should:

- Deep-dive into the affected files
- Find ALL callers and consumers of code you will change
- Check for existing patterns to follow
- Identify potential regression points

The [plan](../methodology/plan.md) includes a contract -- structured success criteria negotiated before implementation. For brownfield work, contracts are especially valuable because they force explicit agreement on:

- Convention adherence (patterns already in the codebase)
- Regression boundaries (what must NOT break)
- Negative criteria (unauthorized access, data leakage)

**Tier selection for brownfield**: Default to Tier 2 for most brownfield work. Existing codebases have more surface area for subtle breakage. Use Tier 1 only for isolated, low-risk changes.

### Execution

When running `pice execute`:

- Read existing code before modifying (never edit blind)
- Maintain existing patterns even if you would choose differently
- Run the full validation suite, not just new tests
- Pay attention to import patterns, error handling, and naming

### Adversarial Evaluation

After execution, run `pice evaluate` against the contract. The [evaluator](../methodology/evaluate.md) is especially valuable for brownfield because:

- It checks convention compliance against project guidance without being primed by implementation decisions
- It can catch leaks by testing negative criteria
- It runs validation commands against the real codebase, not against assumptions

## Step 4: Evolve Incrementally

After each PICE loop, improve the AI layer:

| Problem | Solution |
|---------|----------|
| Agent uses wrong import style | Add import patterns to project guidance |
| Agent does not know about legacy code | Create `.claude/docs/legacy-systems.md` |
| Agent breaks adjacent features | Add regression tests to validation |
| Agent picks wrong file to modify | Add architecture section to project guidance |
| Agent misunderstands auth flow | Create `.claude/rules/auth.md` |

## Key Differences from Greenfield

| Aspect | Greenfield | Brownfield |
|--------|-----------|------------|
| Rules source | Generated from PRD | Reverse-engineered from code |
| Patterns | You choose | You follow what exists |
| Testing | Build from scratch | Catalog existing + fill gaps |
| Risk | Wrong architecture | Breaking existing features |
| Prime focus | What are we building? | How does this codebase work? |
| Contract tier | Tier 1-2 for most features | Default to Tier 2 |

## Anti-Patterns

- Do not rewrite the AI layer from scratch -- evolve it incrementally
- Do not impose new patterns over existing conventions (unless deliberately refactoring)
- Do not skip the regression harness -- it is your safety net
- Do not trust generated rules blindly -- always review against actual code
- Do not make the agent read the entire codebase -- use layered context

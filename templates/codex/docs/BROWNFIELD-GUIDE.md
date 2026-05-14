# Brownfield Development Guide

Working with an existing codebase is different from starting fresh. The code already has patterns, conventions, tech debt, and implicit knowledge. Your job is to reverse-engineer those patterns into the AI layer so the agent can work reliably within the existing system.

---

## Step 1: Reverse-Engineer the AI Layer

### Generate AGENTS.md

Start Codex in the project root and run:

```
/create-rules
```

This analyzes the existing codebase and extracts patterns. But don't blindly trust it — the generated rules need your review.

### Review and Customize

Go through the generated AGENTS.md and check:

- [ ] Are the detected patterns actually correct?
- [ ] Are there conventions the analysis missed?
- [ ] Are there "wrong" patterns in the codebase that shouldn't be replicated?
- [ ] Does the architecture description match reality?
- [ ] Are all the important commands listed?

### Create On-Demand Rules

For large codebases, extract subsystem-specific conventions into `.codex/rules/`:

```
.codex/rules/
├── frontend.md       ← paths: ["src/frontend/**", "**/*.tsx"]
├── api.md            ← paths: ["src/api/**", "src/routes/**"]
├── database.md       ← paths: ["src/db/**", "migrations/**"]
├── auth.md           ← paths: ["src/auth/**", "src/middleware/auth*"]
└── testing.md        ← paths: ["**/*.test.*", "**/*.spec.*"]
```

### Create Reference Docs

For complex areas, create deep-dive docs in `.codex/docs/`:

- Architecture deep dive
- Data model reference
- Integration guide for external services
- Deployment procedures

These stay out of the main context but are available to sub-agent scouts.

---

## Step 2: Build a Regression Test Harness

Before adding new features, establish a safety net.

### Catalog Existing Tests
```bash
# Find all test files
find . -name "*.test.*" -o -name "*.spec.*" -o -name "test_*" | head -30
```

Then ask Codex:
```
Catalog all existing tests in this project. Tell me what test coverage tooling exists, run a coverage report, and show me where the gaps are.
```

Codex will check your `package.json` scripts, pytest config, CI workflows, etc. and run whatever coverage tool is already set up. If none exists, it'll tell you and can set one up.

### Identify Critical User Journeys

Ask yourself: what are the 5-10 things that absolutely cannot break?

For each one, ensure there's at least an integration or E2E test. If not, create one before doing any new work.

### Create a Validate Command

Use `/validate` or create a custom one that covers:

1. Lint + type check
2. Unit tests
3. Integration tests
4. E2E smoke tests for critical paths
5. Build verification

Run this after every PICE loop to catch regressions.

---

## Step 3: Work in PICE Loops

The same greenfield process works for brownfield — but with more emphasis on codebase research and contract-based evaluation.

### Enhanced Prime

For brownfield, your `/prime` should:
- Read AGENTS.md and on-demand rules
- Check git history for recent patterns
- Look at the specific area you're about to modify
- Understand the existing tests around that area

### Planning with Codebase Intelligence + Contracts

When running `/plan-feature`, sub-agents should:
- Deep-dive into the affected files
- Find ALL callers/consumers of code you'll change
- Check for existing patterns to follow
- Identify potential regression points

The plan now includes a **contract** — structured success criteria negotiated with you before implementation. For brownfield work, contracts are especially valuable because they force explicit agreement on:
- Multi-tenancy scoping (org isolation, RBAC checks)
- Convention adherence (patterns already in the codebase)
- Regression boundaries (what must NOT break)
- Negative criteria (unauthorized access, data leakage)

**Tier selection for brownfield**: Default to Tier 2 for most brownfield work — existing codebases have more surface area for subtle breakage. Use Tier 1 only for isolated, low-risk changes.

### Execution with Extra Caution

When running `/execute`:
- Read existing code before modifying (never edit blind)
- Maintain existing patterns even if you'd choose differently
- Run the full validation suite, not just new tests
- Pay attention to import patterns, error handling, and naming

### Adversarial Evaluation

After execution, run `/evaluate` against the contract. The evaluator sub-agent is especially valuable for brownfield because:
- It checks convention compliance against AGENTS.md without being primed by implementation decisions
- It can catch multi-tenancy leaks by testing negative criteria (accessing another org's data)
- It runs validation commands against the real codebase, not against assumptions

---

## Step 4: Evolve Incrementally

### The "Boy Scout Rule"

Leave the code (and AI layer) better than you found it. After each PICE loop:

1. Did the agent make any mistakes due to missing rules? → Update AGENTS.md
2. Did it use wrong patterns in a specific area? → Create an on-demand rule
3. Did a regression slip through? → Add a test and update the validate command
4. Did research waste time on irrelevant files? → Create a specialized prime command

### Common Brownfield AI Layer Improvements

| Problem | Solution |
|---------|----------|
| Agent uses wrong import style | Add import patterns to AGENTS.md |
| Agent doesn't know about legacy code | Create `.codex/docs/legacy-systems.md` |
| Agent breaks adjacent features | Add regression tests to validate |
| Agent picks wrong file to modify | Add architecture section to AGENTS.md |
| Agent misunderstands auth flow | Create `.codex/rules/auth.md` |
| Agent generates inconsistent styles | Create `.codex/rules/frontend.md` |

### Specialized Prime Commands

As you learn which areas need special attention, create targeted primes:

```
.codex/commands/
├── prime.md            ← full codebase overview
├── prime-backend.md    ← backend-specific orientation
├── prime-frontend.md   ← frontend-specific orientation
├── prime-database.md   ← schema + migration focus
└── prime-auth.md       ← auth system deep dive
```

---

## Key Differences from Greenfield

| Aspect | Greenfield | Brownfield |
|--------|-----------|------------|
| Rules source | Generated from PRD + research | Reverse-engineered from code |
| Patterns | You choose | You follow what exists |
| Testing | Build from scratch | Catalog existing + fill gaps |
| Risk | Wrong architecture | Breaking existing features |
| Prime focus | "What are we building?" | "How does this codebase work?" |
| Plan focus | "How should we build it?" | "How does it fit with what's here?" |
| Validation | Feature works | Feature works AND nothing broke |
| Contract tier | Tier 1-2 for most features | Default to Tier 2 (more regression surface) |
| Contract criteria | Functional + UX | Functional + convention + negative (multi-tenancy, auth) |

---

## Anti-Patterns

- **Don't rewrite the AI layer from scratch** — evolve it incrementally
- **Don't impose new patterns** over existing conventions (unless deliberately refactoring)
- **Don't skip the regression harness** — it's your safety net
- **Don't trust generated rules blindly** — always review against actual code
- **Don't make the agent read the entire codebase** — use layered context

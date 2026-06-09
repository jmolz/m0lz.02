# CLAUDE.md

This file provides guidance to Claude Code when working with this repository.

## Project Overview

<!-- What is this project? One paragraph. -->

{Project description and purpose}

---

## Tech Stack

| Technology | Purpose |
|------------|---------|
| {language/runtime} | {primary language} |
| {framework} | {web framework, UI library, etc.} |
| {database} | {data storage} |
| {testing} | {test framework} |
| {build tool} | {bundler, compiler, etc.} |

---

## Commands

```bash
# Development
{dev-command}              # Start dev server

# Build
{build-command}            # Production build

# Test
{test-command}             # Run test suite
{test-watch-command}       # Watch mode (if available)

# Lint & Format
{lint-command}             # Check for issues
{format-command}           # Auto-format

# Database (if applicable)
{migrate-command}          # Run migrations
{seed-command}             # Seed data

# Full Validation (run before every commit)
{validate-command}         # Lint + types + tests + build
```

---

## Project Structure

```
{root}/
├── src/                   # Source code
│   ├── {dir}/             # {description}
│   ├── {dir}/             # {description}
│   └── {dir}/             # {description}
├── tests/                 # Test files (mirrors src/ structure)
├── public/                # Static assets (if applicable)
└── {config files}         # Configuration
```

---

## Architecture

<!-- How does data flow? What are the key patterns? -->

{Describe the architectural approach: layered, component-based, event-driven, etc.}

{Describe the data flow: request → handler → service → database, etc.}

---

## Code Patterns

### Naming
- Files: `{convention}` (e.g., kebab-case, PascalCase)
- Functions: `{convention}`
- Types/Interfaces: `{convention}`
- Constants: `{convention}`

### Imports
- {Import style: relative, absolute, aliases}

### Error Handling
- {Pattern: try/catch with typed errors, Result types, error boundaries, etc.}

### Logging
- {Strategy: structured logging with levels, console in dev, etc.}

---

## Testing

- **Framework**: {jest, pytest, vitest, etc.}
- **Location**: `{tests/ or __tests__/ or *.test.ts}`
- **Run**: `{test-command}`
- **Minimum coverage**: {each new feature needs: 1 happy path, 1 edge case, 1 error case}
- **Patterns**: {describe mocking approach, fixtures, test data}

---

## Adversarial Evaluation (Codex Plugin)

This project uses **dual-model adversarial evaluation** through PICE evaluation providers. GPT-5.5 with xhigh reasoning acts as a peer-level adversary to Fable 5, challenging design decisions and assumptions from a different model family's perspective.

- **Config**: `.pice/config.toml` — configure `[evaluation.primary]`, `[evaluation.adversarial]`, and `[evaluation.tiers]`
- **All tiers**: Run `/evaluate <plan>`; it launches formal contract grading plus a GPT-5.5 xhigh design challenge
- **Tier 3 architectural changes**: Configure the adversarial provider with `effort = "xhigh"` for maximum reasoning depth
- **Design challenges from Codex are complementary** — they question the approach, not just the implementation

---

## Validation (Pre-Commit)

Run these before every commit:

```bash
{lint-command}
{type-check-command}
{test-command}
{build-command}
```

---

## On-Demand Context

When working on specific areas, read the corresponding reference:

| Area | File | When |
|------|------|------|
| Frontend components | `.claude/rules/frontend.md` | Working on UI code |
| API endpoints | `.claude/rules/api.md` | Building/modifying APIs |
| Database | `.claude/rules/database.md` | Schema changes, queries |
| {custom area} | `.claude/rules/{name}.md` | {when} |

For deep architecture reference: `.claude/docs/`

---

## Key Rules

<!-- The most important constraints — things the agent should NEVER violate -->

- {Rule 1: e.g., "Never use `any` type without a justification comment"}
- {Rule 2: e.g., "All API endpoints must validate input with Zod schemas"}
- {Rule 3: e.g., "Database queries must use parameterized queries, never string interpolation"}
- {Rule 4: e.g., "Never commit .env files or hardcoded secrets"}

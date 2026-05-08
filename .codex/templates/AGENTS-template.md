# AGENTS.md

This file provides guidance to Codex when working with this repository.

## Project Overview

{Project description and purpose}

## Tech Stack

| Technology | Purpose |
| ---------- | ------- |
| {language/runtime} | {primary language} |
| {framework} | {web framework, UI library, etc.} |
| {database} | {data storage} |
| {testing} | {test framework} |
| {build tool} | {bundler, compiler, etc.} |

## Commands

```bash
# Development
{dev-command}

# Build
{build-command}

# Test
{test-command}
{test-watch-command}

# Lint and format
{lint-command}
{format-command}

# Database
{migrate-command}
{seed-command}

# Full validation
{validate-command}
```

## Project Structure

```text
{root}/
├── src/
├── tests/
├── public/
└── {config files}
```

## Architecture

{Describe architectural approach and data flow.}

## Code Patterns

### Naming

- Files: `{convention}`
- Functions: `{convention}`
- Types/interfaces: `{convention}`
- Constants: `{convention}`

### Imports

{Import style and alias conventions.}

### Error Handling

{Error handling pattern.}

### Logging

{Logging strategy.}

## Testing

- Framework: {framework}
- Location: {path/pattern}
- Run: `{test-command}`
- Minimum coverage for new behavior: happy path, edge case, and error case where applicable.
- Patterns: {mocking, fixtures, assertion style}

## Validation

Run before commit:

```bash
{lint-command}
{type-check-command}
{test-command}
{build-command}
```

## On-Demand Context

| Area | File | When |
| ---- | ---- | ---- |
| Frontend | `.codex/rules/frontend.md` | UI work |
| API | `.codex/rules/api.md` | Endpoint work |
| Database | `.codex/rules/database.md` | Schema, query, migration work |
| Testing | `.codex/rules/testing.md` | Test additions or changes |

Deep reference material belongs in `.codex/docs/`.

## Key Rules

- {Rule 1}
- {Rule 2}
- {Rule 3}
- Never commit `.env` files or hardcoded secrets.

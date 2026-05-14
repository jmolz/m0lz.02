---
description: Run comprehensive validation across the entire codebase
---

# Validate: Full Codebase Check

Run every available validation tool to ensure the codebase is healthy.

## Process

### 1. Discover Available Tools

Check what validation tools exist in this project:

```bash
# Check for package.json scripts
cat package.json 2>/dev/null | grep -A 50 '"scripts"' | head -60

# Check for Python project tools
cat pyproject.toml 2>/dev/null | head -40

# Check for Makefile
cat Makefile 2>/dev/null | grep '^[a-z]' | head -20

# Check for CI config
ls .github/workflows/ 2>/dev/null
cat .github/workflows/*.yml 2>/dev/null | head -80
```

### 2. Run Validation Pyramid

Execute each level in order. Fix failures before proceeding.

#### Level 1: Linting
Run the project's linter (eslint, ruff, clippy, etc.)
```bash
# Examples — use whatever this project has:
# npm run lint
# ruff check .
# cargo clippy
```

#### Level 2: Type Checking
Run the type checker if available:
```bash
# Examples:
# npx tsc --noEmit
# mypy .
# cargo check
```

#### Level 3: Formatting
Check code formatting:
```bash
# Examples:
# npx prettier --check .
# ruff format --check .
# cargo fmt --check
```

#### Level 4: Unit Tests
Run the test suite:
```bash
# Examples:
# npm test
# pytest
# cargo test
```

#### Level 5: Build Check
Verify the project builds:
```bash
# Examples:
# npm run build
# uv run python -m py_compile main.py
# cargo build
```

#### Level 6: Integration / E2E (if available)
Run any integration or end-to-end tests:
```bash
# Examples:
# npm run test:e2e
# pytest tests/integration/
```

### 3. Report Results

```markdown
## Validation Report

| Check | Status | Details |
|-------|--------|---------|
| Lint | ✅/❌ | {error count or "clean"} |
| Types | ✅/❌ | {error count or "clean"} |
| Format | ✅/❌ | {files needing format or "clean"} |
| Tests | ✅/❌ | {passed/failed/skipped counts} |
| Build | ✅/❌ | {success or error summary} |
| E2E | ✅/❌/⏭️ | {results or "not configured"} |

### Issues Found
{List any failures with file paths and brief descriptions}

### Recommended Fixes
{For each issue: what to change and where}
```

## Notes

- Adapt to whatever tools this project actually uses
- Don't run tools that aren't configured (no phantom validation)
- If a check doesn't exist yet, note it as a gap
- Fix issues in order: lint → types → format → tests → build

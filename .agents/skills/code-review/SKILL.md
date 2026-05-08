---
name: code-review
description: "Use this skill when reviewing code changes, pull requests, or when asked to check code quality. Triggers: 'review', 'code review', 'check this code', 'PR review', 'look over my changes', 'audit'. Provides a structured review checklist covering correctness, security, performance, readability, and testing."
---

# Code Review Skill

## When to Use
The agent should read this skill when reviewing code changes, auditing existing code, or evaluating a PR.

## Review Process

### 1. Understand the Change
- Read the diff or files being reviewed
- Understand the intent — what problem does this solve?
- Check if there's a related plan, issue, or PR description

### 2. Correctness
- Does the code do what it claims to do?
- Are edge cases handled? (null, empty, boundary values, concurrent access)
- Are error paths handled gracefully? (no swallowed errors, meaningful messages)
- Does it match the patterns in AGENTS.md?

### 3. Security
- No hardcoded secrets, API keys, or credentials
- User input is validated and sanitized
- Database queries are parameterized (no string interpolation)
- Authentication/authorization checks are in place where needed
- No sensitive data in logs or error messages
- Dependencies are from trusted sources

### 4. Performance
- No N+1 query patterns
- No unnecessary re-renders (frontend)
- No blocking operations on the main thread
- Large data sets are paginated or streamed
- Expensive computations are memoized where appropriate

### 5. Readability
- Names are descriptive and consistent with codebase conventions
- Functions are focused (single responsibility)
- No overly clever code — prefer clarity over brevity
- Comments explain WHY, not WHAT (the code should explain what)
- No dead code or commented-out blocks

### 6. Testing
- New logic has corresponding tests
- Tests cover happy path, edge cases, and error cases
- Tests are independent (no shared mutable state between tests)
- Mocks are appropriate (not mocking the thing being tested)
- Assertions are specific (not just "it didn't throw")

### 7. Architecture
- Changes respect existing module boundaries
- No circular dependencies introduced
- New abstractions are justified (not premature)
- Public API surface is minimal

## Output Format

Provide findings grouped by severity:

**Critical** — Must fix before merge (bugs, security issues, data loss risks)
**Warning** — Should fix (performance, maintainability, pattern violations)  
**Suggestion** — Consider improving (readability, minor optimizations)
**Positive** — What's done well (reinforces good patterns)

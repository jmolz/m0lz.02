---
name: refactor
description: "Use this skill when restructuring, simplifying, or improving existing code without changing behavior. Triggers: 'refactor', 'simplify', 'clean up', 'reorganize', 'extract', 'consolidate', 'reduce duplication', 'too complex', 'tech debt', '/simplify'. Provides safe refactoring patterns that preserve behavior while improving structure."
---

# Refactor Skill

## When to Use
The agent should read this skill when restructuring code to improve quality without changing external behavior.

## Golden Rule
**Tests must pass before AND after refactoring.** If there are no tests, write them first. Refactoring without tests is just changing code and hoping.

## Pre-Refactor Checklist

1. Run the full test suite — confirm everything passes
2. Commit current state — create a clean rollback point
3. Identify the specific problem (don't just "clean up")
4. Plan the changes before making them

## Common Refactoring Patterns

### Extract Function
**When:** A block of code does one distinct thing inside a larger function.
```
# Before: 50-line function doing 3 things
# After: 3 focused functions, each 15-20 lines
```

### Extract Module/File
**When:** A file has grown past 300-400 lines or handles multiple concerns.
- Group related functions and types
- Move to a new file with a clear name
- Update imports throughout the codebase
- Verify nothing broke

### Consolidate Duplication
**When:** Similar logic exists in 3+ places.
- Identify the shared pattern
- Create a single source of truth (utility, hook, base class)
- Replace duplicates with calls to the shared version
- **Don't** consolidate things that are only superficially similar — if they'll diverge, keep them separate

### Simplify Conditionals
**When:** Nested if/else blocks are hard to follow.
- Early returns to reduce nesting
- Guard clauses at the top of functions
- Lookup objects/maps instead of long switch statements
- Named boolean variables for complex conditions

### Rename for Clarity
**When:** Names don't communicate intent.
- Functions: verb + noun (`getUserById`, not `getData`)
- Booleans: `isActive`, `hasPermission`, `shouldRetry`
- Use your IDE's rename tool, not find-and-replace
- Check that all callers updated correctly

### Flatten Hierarchy
**When:** Too many layers of abstraction for no benefit.
- If a wrapper just passes through to one function, remove the wrapper
- If an interface has only one implementation and no plans for more, consider inlining
- Premature abstraction is worse than duplication

## Refactoring Workflow

```
1. Identify → What specifically is the problem?
2. Test     → Ensure existing tests pass (write them if missing)
3. Commit   → Clean rollback point
4. Change   → Make ONE refactoring move at a time
5. Test     → Run tests after each move
6. Commit   → Checkpoint after each successful move
7. Repeat   → Next refactoring move
```

**Small steps, frequent tests.** Never make multiple structural changes between test runs.

## When NOT to Refactor

- ❌ Right before a deadline (refactor after shipping)
- ❌ Code you don't understand yet (understand first, then refactor)
- ❌ Code that has no tests and is working (add tests first)
- ❌ For aesthetic reasons alone (must improve maintainability)
- ❌ Everything at once (pick the highest-impact area)

## Post-Refactor

1. Run full test suite + validation
2. Check that no public API changed (unless intentional)
3. Update any documentation that references changed structure
4. Consider updating AGENTS.md or rules if patterns changed

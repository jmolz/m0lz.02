# Contract Format

## Purpose

The contract is a machine-readable agreement embedded in every PICE plan file. It
defines what the implementation must achieve, how it will be measured, and how
rigorously it will be evaluated. The contract is written during the
[Plan Phase](plan.md) and enforced during the [Evaluation System](evaluate.md).

Without a contract, evaluation is subjective. With a contract, it is deterministic:
every criterion has a threshold, every threshold has a validation command, and the
Rust core recomputes pass/fail from raw scores independently of any provider's verdict.

## Structure

The contract is a JSON object inside a fenced code block under a level-2
`## Contract` heading in the plan file. The PICE CLI parser requires exactly this
structure -- `### Contract` or other heading levels will not be recognized.

```json
{
  "feature": "API Rate Limiting",
  "tier": 2,
  "pass_threshold": 7,
  "criteria": [
    {
      "name": "Tests pass",
      "threshold": 7,
      "validation": "cargo test"
    },
    {
      "name": "Rate limit middleware exists",
      "threshold": 7,
      "validation": "test -f src/server/rate_limit.rs"
    },
    {
      "name": "No lint warnings",
      "threshold": 8,
      "validation": "cargo clippy -- -D warnings"
    }
  ]
}
```

## Fields

### `feature` (string, required)

A short, human-readable name for the feature. Used in evaluation reports and metrics.
Keep it under 60 characters.

### `tier` (integer, required)

The evaluation tier, determining how many evaluators run and at what depth.

| Tier | Scope | Evaluation |
|------|-------|------------|
| 1 | Bug fixes, config changes, simple refactors | Single primary evaluator |
| 2 | New features, integrations, multi-file changes | Primary evaluator + configured adversarial provider |
| 3 | Architectural changes, new subsystems, core refactors | Primary evaluator team + configured adversarial provider |

When uncertain, tier up rather than down.

### `pass_threshold` (integer, required)

The minimum aggregate score (1-10) for the contract to pass overall. Currently,
pass/fail is computed per-criterion (all must meet their individual threshold). This
field exists for future weighted scoring. Set to 7 for most contracts.

### `criteria` (array, required)

An array of criterion objects. Each defines one measurable aspect of the
implementation.

#### Criterion Fields

**`name`** (string, required)

A short label for the criterion. Appears in evaluation reports. Examples: "Tests pass",
"No lint warnings", "API endpoint responds correctly", "Migration is reversible".

**`threshold`** (integer, required)

The minimum score (1-10) this criterion must receive to pass. The evaluator AI scores
each criterion, and the Rust core checks whether each score meets its threshold.

Threshold guidelines:

| Score | Meaning |
|-------|---------|
| 1-3 | Fundamentally broken or missing |
| 4-6 | Partial implementation, significant issues |
| 7 | Meets the bar -- functional, correct, follows conventions |
| 8 | Good -- clean implementation, handles edge cases |
| 9-10 | Excellent -- exceeds expectations, exemplary code |

Most criteria should use threshold 7. Use 8 or higher for critical requirements where
"good enough" is not good enough (security, data integrity, public API design). Use
lower thresholds only when a criterion is aspirational rather than required.

**`validation`** (string, required)

A shell command that the evaluator can reference when scoring. This command should
test the specific thing the criterion measures. Examples:

- `cargo test` -- all tests pass
- `cargo clippy -- -D warnings` -- no lint warnings
- `test -f src/server/rate_limit.rs` -- file exists
- `cargo test auth::tests` -- specific test module passes
- `pnpm typecheck` -- TypeScript types are correct

Validation commands should be runnable from the project root and exit 0 on success.
The evaluator uses these as evidence when scoring, not as the sole determinant.

## Writing Good Criteria

## Layer Contracts In Stack Loops

v0.2 keeps the same JSON contract shape, but the daemon can bind contracts to
layers. Layer contracts should test the responsibilities of that layer rather
than repeat a broad feature checklist. For example, an infrastructure layer
contract should name environment variables, secrets, network boundaries, and
resource limits; a deployment contract should name rollout, rollback, and health
check evidence.

Layer contracts are evaluated with filtered diffs. A database evaluator should
not receive unrelated frontend changes, and a frontend evaluator should not
receive sibling layer findings.

### Be Specific

Bad: "Code quality is acceptable"
Good: "No clippy warnings with `-D warnings`"

Bad: "Feature works correctly"
Good: "POST /api/users returns 201 with valid payload and 400 with missing fields"

### Be Testable

Every criterion needs a validation command that can be run mechanically. If you cannot
write a command that checks the criterion, the criterion is too vague.

### Cover Different Dimensions

A good contract covers multiple aspects of the implementation:

- **Correctness**: Tests pass, expected behavior verified
- **Quality**: Lint clean, type safe, no warnings
- **Completeness**: All planned files exist, all endpoints implemented
- **Conventions**: Follows project patterns documented in project guidance

### Match Thresholds to Importance

Not every criterion is equally important. A test suite passing is non-negotiable
(threshold 7 minimum). Code style consistency is important but less critical (threshold
7). Performance characteristics might be aspirational (threshold 6 for a stretch goal).

## Contract Parsing

The PICE CLI parser looks for a level-2 `## Contract` heading (not `###` or deeper),
finds the ` ```json ` fenced code block within it, and parses the JSON as a
`PlanContract` struct. If `## Contract` exists but the JSON fence is missing or
malformed, the parser returns an error -- half-written contracts are surfaced, not
silently ignored. Plans without a `## Contract` heading are treated as contract-free.

## Example: Tier 1 Contract (Bug Fix)

```json
{
  "feature": "Fix login redirect bug",
  "tier": 1,
  "pass_threshold": 7,
  "criteria": [
    {
      "name": "Tests pass",
      "threshold": 7,
      "validation": "cargo test"
    },
    {
      "name": "Redirect works",
      "threshold": 7,
      "validation": "cargo test auth::tests::login_redirect"
    }
  ]
}
```

## Example: Tier 3 Contract (Architectural)

A Tier 3 contract uses more criteria with higher thresholds to match the rigor of the
full evaluator team:

```json
{
  "feature": "WebSocket Event Streaming",
  "tier": 3,
  "pass_threshold": 8,
  "criteria": [
    { "name": "All tests pass", "threshold": 7, "validation": "cargo test" },
    { "name": "Lifecycle tests", "threshold": 8, "validation": "cargo test websocket::tests" },
    { "name": "No lint warnings", "threshold": 8, "validation": "cargo clippy -- -D warnings" },
    { "name": "Graceful disconnect", "threshold": 8, "validation": "cargo test websocket::tests::disconnect" }
  ]
}
```

## Further Reading

- [PICE Overview](overview.md) -- The full lifecycle
- [Plan Phase](plan.md) -- Where contracts are negotiated
- [Implement Phase](implement.md) -- Executing against the contract
- [Evaluation System](evaluate.md) -- How contracts are enforced

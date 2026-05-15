# Implement Phase

## Purpose

The Implement phase turns an approved plan into working code. A fresh AI session
receives the plan file, the project's workflow guidance, and access to the codebase. It does
not receive the planning conversation, the developer's rationale, or any other context
from the Plan phase.

This context isolation is the defining characteristic of the Implement phase. The plan
is the single source of truth. If the plan is good, the implementation will be good.
If the plan is missing something, the implementation will expose that gap -- which is
useful feedback for improving future plans.

## Context Isolation

When `pice execute <plan-path>` runs, the CLI:

1. Starts a new provider process (separate from any previous session)
2. Creates a fresh AI session with no conversation history
3. Sends the plan file content and workflow guidance as the initial context
4. The AI reads the codebase directly through tool use

The implementation session never sees:

- The planning conversation or reasoning
- Alternative approaches that were considered and rejected
- The developer's verbal instructions from the planning session
- Any evaluation results from previous iterations

This isolation serves two purposes. First, it tests the plan's completeness -- if the
plan lacks critical information, the implementation will struggle, revealing a planning
gap. Second, it prevents the AI from taking shortcuts based on context that would not
be available to a different developer reading the same plan.

## The Plan as Single Source of Truth

During implementation, the AI follows the plan's numbered steps sequentially. It uses
the plan's research section to understand which files to modify and why. It refers to
the plan's dependency mapping to determine the order of operations.

If the plan says "add a rate limiter to the middleware chain," the AI adds a rate
limiter to the middleware chain. It does not decide that a different approach would be
better, even if it could argue for one. The plan was negotiated and approved. The
implementation phase executes it.

This discipline matters because PICE evaluation grades the implementation against the
plan's contract. An implementation that deviates from the plan -- even if technically
superior -- may fail criteria that assume the planned approach. Deviations belong in
the next planning cycle, not in the middle of implementation.

## Execution from Plan Files

The implementation command takes a path to a plan file:

```bash
pice execute plans/rate-limiting.md
```

The CLI parses the plan file, extracts the contract, refuses contract-free plans before
provider startup, records plan trace metadata for background runs, and assembles the
execution prompt. The prompt includes:

- The full plan content (research, steps, contract)
- The project's workflow guidance (coding standards, project structure, conventions)
- Instructions to follow the plan steps sequentially
- Instructions to run validation after each step

The configured workflow provider controls which guidance file is preferred.
`[provider].name = "claude-code"` prefers `CLAUDE.md`; `[provider].name = "codex"`
prefers `AGENTS.md`. If the preferred file is absent, PICE falls back to the other
file so existing projects continue to work.

The AI then works through the plan, using its tools to read files, write code, and
run commands. The developer watches the streaming output and can intervene if needed.

## How Work Stays Tied To The Spec

`prime` orients on the repository and current state. It does not bind implementation
back to the original request. The binding starts when `plan` converts the original
request, supplied spec, or stable reference into an approved plan, a `## Spec
Traceability` mapping, and a contract. `execute` uses that approved plan and contract
as the source of truth for a fresh session and refuses contract-free plans. `evaluate`
then grades the diff against the contract with isolated evaluators. Stack Loops add
per-layer contracts, seam checks, manifest state, and review gates.

## Validation During Implementation

Good plans include validation commands after each step. The AI should run these as it
goes:

- After adding a new file: check that it compiles (`cargo check` or equivalent)
- After modifying tests: run the relevant test suite
- After changing configuration: verify the config parses correctly
- After each logical group of changes: run the full test suite

This incremental validation catches problems early. An error in step 3 is easier to
fix before steps 4 through 7 build on top of it.

The contract's `validation` fields define the final validation commands that evaluation
will use. Running these during implementation (not just at the end) ensures the
implementation stays on track.

## What the AI Can and Cannot Do

During implementation, the AI has full access to the codebase through its tools. It
can:

- Read any file in the project
- Create new files
- Modify existing files
- Run shell commands (tests, builds, lints)
- Search the codebase for patterns and references

It cannot:

- Access the internet (unless the provider supports it and the project allows it)
- See the planning conversation
- Modify the plan file itself
- Skip plan steps without the developer's approval

## Handling Plan Gaps

Sometimes the implementation session reveals that the plan is incomplete. A dependency
was missed, a file structure changed since planning, or a step is ambiguous.

In these cases, the AI should:

1. Note the gap explicitly in its output
2. Make a reasonable interpretation based on the plan's intent
3. Flag the deviation for the developer's review

The developer can then either approve the deviation or stop the session, update the
plan, and restart. PICE does not mandate restarting -- minor gaps are normal and
expected. Major gaps (wrong architecture, missing subsystem) should trigger a plan
revision.

## Session Lifecycle

Under the hood, `pice execute` follows the standard PICE session lifecycle managed by
the Rust core:

1. **Resolve provider** -- Look up the configured workflow provider from
   `[provider].name` (default: Claude Code; Codex is available with
   `pice init --developer codex` or `name = "codex"`)
2. **Spawn process** -- Start the provider as a child process communicating over
   JSON-RPC via stdio
3. **Initialize** -- Send provider configuration
4. **Create session** -- Start a new AI session in the provider
5. **Send prompt** -- Transmit the plan content and execution instructions
6. **Stream output** -- Display the AI's responses as they arrive
7. **Destroy session** -- Clean up the AI session
8. **Shutdown provider** -- Terminate the provider process

This lifecycle is managed by `session::run_session()` in the engine module. Individual
commands never duplicate this sequence.

## What Happens Next

After implementation is complete, the developer runs `pice evaluate <plan-path>` to
grade the work against the contract. The [Evaluation System](evaluate.md) uses
context-isolated evaluators that see only the contract, the git diff, and evaluation
guidance from `AGENTS.md` -- not the implementation session.

## Further Reading

- [PICE Overview](overview.md) -- The full lifecycle
- [Plan Phase](plan.md) -- How plans are created
- [Contract Format](contract.md) -- The success criteria the implementation must meet
- [Evaluation System](evaluate.md) -- How implementations are graded

---
name: "source-command-evaluate"
description: "Grade implementation against a plan's contract using an isolated adversarial evaluator"
---

# source-command-evaluate

Use this skill when the user asks to run the migrated source command `evaluate`. Original argument hint: `<path-to-plan.md>`.

## Command Template


# Evaluate: Contract-Based Adversarial Review

## Mission

Grade the implementation against the contract defined in the plan file. The evaluation is performed by a **fresh sub-agent** that sees ONLY the contract, the code diff, and AGENTS.md — never the planning conversation or implementation rationale. This separation eliminates self-evaluation bias.

**Core Principle**: The evaluator's job is to find failures, not confirm success. A passing score must be earned.

---

## Step 1: Load the Contract

Read the plan file at: `{user-provided arguments}`

Extract the `## Contract` section. If no contract exists, stop and tell the user:

```
No contract found in this plan. Run /plan-feature to create a plan with a contract,
or add a ## Contract section manually with JSON criteria.
```

Parse the contract JSON to get:

- **Tier** (1, 2, or 3) — determines number of evaluation passes
- **Criteria** — each with name, threshold, and validation method
- **Pass threshold** — default 8/10

---

## Step 2: Gather Evaluation Context

Collect ONLY what the evaluator needs — no implementation rationale:

```bash
# What changed since the plan was created
git diff HEAD~$(git log --oneline --since="$(stat -f %Sm -t '%Y-%m-%d' {user-provided arguments} 2>/dev/null || date -r $(stat -c %Y {user-provided arguments} 2>/dev/null || echo 0) '+%Y-%m-%d')" | wc -l | tr -d ' ')..HEAD --stat
git diff HEAD~$(git log --oneline --since="$(stat -f %Sm -t '%Y-%m-%d' {user-provided arguments} 2>/dev/null || date -r $(stat -c %Y {user-provided arguments} 2>/dev/null || echo 0) '+%Y-%m-%d')" | wc -l | tr -d ' ')..HEAD
```

If the diff approach doesn't work cleanly, fall back to:

```bash
git diff HEAD
git status
```

Also gather:

- The project's AGENTS.md (for convention checking)

If `AGENTS.md` is missing from the current git toplevel and the toplevel path contains `/.worktrees/`, use the sibling main checkout `AGENTS.md` above the `.worktrees` directory instead. Report the resolved AGENTS path in the evaluation output so a worktree cannot silently evaluate without project conventions.

---

## Step 3: Run Evaluation Pass(es)

Evaluation uses a **dual-model adversarial** approach. The configured primary evaluator grades contract criteria formally. For Tier 2+, a parallel configured adversarial review challenges the design approach itself.

### Step 3a: Launch Configured Adversarial Review (if enabled)

If `.pice/config.toml` has `[evaluation.adversarial].enabled = true`, read `[evaluation.adversarial]` and launch the configured `provider`, `model`, and `effort` in the background **before** running the configured primary evaluator. Do not substitute hard-coded defaults when the config names a different provider, model, or effort.

If `[evaluation.adversarial].enabled = false`, do not launch the background review. Record `Adversarial review: NO (disabled in config)` in the final report and proceed with primary-only evaluation.

Run the configured provider path via `Bash` with `run_in_background: true` (so primary evaluation can proceed in parallel). For the bundled Codex adversarial provider (`provider = "codex"`), run:

```bash
node "$HOME/.codex/plugins/cache/openai-codex/codex/1.0.4/scripts/codex-companion.mjs" \
  task --background --model {model} --effort {effort} \
  "Adversarially evaluate against this contract: {paste contract criteria names and thresholds}. Use only the contract JSON, git diff/status, and AGENTS.md. Challenge design assumptions, failure modes, and production risks; do not edit files."
```

For the bundled Codex provider, do not use `adversarial-review --effort`; the installed companion does not parse effort flags for that subcommand.

If `provider` is not `codex`, invoke that provider's documented adversarial path with the configured model and effort. If the provider implementation is unavailable, record `Adversarial review: NO (configured provider unavailable: {provider})` and continue with primary-only evaluation.

If the adversarial provider fails, note the error and continue with primary-only evaluation — do not block the entire evaluation.

The adversarial review challenges the *approach* — was this the right design? What assumptions does it depend on? Where could it fail under real-world conditions? This is complementary to formal contract grading.

#### Rate-Limit Fallback (ChatGPT Team → OpenAI API key)

The Codex CLI authenticates via the user's ChatGPT Team session by default. When the configured adversarial provider is `codex` and that session hits its rate limit, fall back to a direct OpenAI Responses API call with the configured adversarial `model` and `effort` — do **not** run `codex login --api-key`, as that would overwrite the ChatGPT Team session (making recovery manual once the rate limit lifts).

**Fallback key location**: `~/.codex/.openai-fallback-key` — single line containing an OpenAI API key, `chmod 600`. If absent, skip fallback and report the rate-limit error verbatim.

**Rate-limit detection** (in collected Codex output, case-insensitive): any of `rate limit`, `rate_limit_exceeded`, `429`, `too many requests`, `usage cap`, `quota exceeded`.

**Fallback invocation** (only on detection, only if the key file exists):

```bash
OPENAI_FALLBACK_KEY=$(cat "$HOME/.codex/.openai-fallback-key")
FALLBACK_MODEL="{model}"
FALLBACK_EFFORT="{effort}"
cat > /tmp/codex-fallback-request.json <<'JSON'
{
  "model": "__MODEL__",
  "reasoning": { "effort": "__EFFORT__" },
  "input": "__PROMPT__"
}
JSON
# Replace __MODEL__, __EFFORT__, and __PROMPT__ with the configured values (jq or sed; escape JSON properly).
curl -sS https://api.openai.com/v1/responses \
  -H "Authorization: Bearer $OPENAI_FALLBACK_KEY" \
  -H "Content-Type: application/json" \
  --data-binary @/tmp/codex-fallback-request.json
```

Reserve sufficient output budget for the configured model and effort. Optionally pass `"max_output_tokens": 32000` and handle `status: "incomplete"` with `incomplete_details.reason === "max_output_tokens"` by retrying with a larger budget. Extract the visible answer from `response.output[].content[].text` (or `response.output_text`).

The `__PROMPT__` must include: the same focus text passed to `codex-companion task`, the contract criteria JSON, the full diff, and AGENTS.md — i.e., the same context Codex would have received. Construct the prompt string explicitly rather than relying on Codex's internal prompt templates (which are not accessible outside the CLI).

Treat the extracted text as the adversarial review output. Label it clearly in the final report as `{provider} {model} {effort} (OpenAI API fallback — ChatGPT Team rate-limited)`.

Once the ChatGPT Team rate limit lifts, no action is required: the primary Codex path resumes on the next invocation, and the fallback triggers only on failure.

### Step 3b: Run Configured Primary Evaluator Pass(es)

For each primary evaluation pass (1 for Tier 1, 1 for Tier 2, 3 for Tier 3 agent team), spawn a **fresh evaluator session** using `[evaluation.primary]` provider/model from `.pice/config.toml` with the following prompt.

### Evaluator Sub-Agent Prompt

```
You are an ADVERSARIAL EVALUATOR. Your job is to find failures, not confirm success.

## Calibration — READ THIS FIRST

Do NOT be generous. Your natural inclination will be to praise the work. Resist this.
When in doubt, score LOWER, not higher. An 8 means "meets the bar" — not "pretty good."
A 7 means "functional but not production-ready — missing edge cases or robustness."
A 6 means "almost there but not reliable enough to ship." Do not round up.

You are NOT the implementer. You did NOT write this code. You have no stake in it passing.
Your reputation depends on catching problems, not on approving work.

## What You Are Grading

Contract:
{paste the full contract JSON here}

## What Changed

{paste the full git diff here}

## Evaluation Guidance

{paste AGENTS.md contents here if present; otherwise state that no evaluator guidance file exists}

## Your Task

For EACH criterion in the contract:

1. **Inspect the supplied diff/status** — identify the changed files and hunks relevant to this criterion without opening additional repository files
2. **Run the validation** — execute the validation command or check the observable behavior
3. **Try to break it from the supplied evidence** — think of edge cases, malformed inputs, missing auth, concurrent access
4. **Score it 1-10** with specific evidence:
   - 1-3: Fundamentally broken or missing
   - 4-5: Partially works but has significant gaps
   - 6-7: Functional but insufficient — missing edge cases, weak validation, or convention drift
   - 8: Meets the bar — correct, robust, follows conventions, handles edge cases
   - 9: Exceeds expectations — well-tested, defensive, production-hardened
   - 10: Exceptional — comprehensive error handling, security-aware, zero gaps found

## Validation Proof Discipline

Before scoring a criterion as passing, confirm that the validation command directly exercises the behavior named by that criterion. A broad suite, integration target, or grep command exiting 0 is not sufficient by itself; identify the assertion, request-log check, fixture, or observable output that would fail if the criterion regressed. If no such proof exists, score the criterion below threshold or call out the missing coverage even when the command passes.

For this repo, cargo validation that spawns `pice-daemon` should run with `PATH="$PWD/target/debug:$PATH"` before classifying missing-daemon or socket-startup errors as product failures. Workspace-wide Rust regression should also use `RUST_TEST_THREADS=1` to avoid socket-race noise. Self-heal anchor: the 2026-05 spec-traceability cycle initially passed `cargo test -p pice-daemon --test parallel_cohort_integration` before the added `feature_contract_fallback_is_used_when_layer_contracts_are_absent` assertion directly proved the Stack Loops feature-contract fallback.

## Output Format

For each criterion, output:

### {Criterion Name}
- **Score**: {N}/10 (threshold: {T})
- **Pass**: YES / NO
- **Evidence**: {What you found — specific file:line references}
- **Issues**: {What's wrong or missing — be specific}
- **Validation Result**: {Output of running the validation command}

Then output a summary:

### Summary
- **Overall**: PASS / FAIL
- **Passed**: {N}/{total} criteria met threshold
- **Lowest Score**: {criterion name} at {score}/10
- **Critical Issues**: {List any criterion that scored below threshold}

If ANY criterion scores below its threshold, the overall result is FAIL.
```

### Between Passes (Tier 2-3 only)

If Pass 1 fails, present the evaluator's feedback to the user:

```
## Evaluation Pass {N} — {PASS/FAIL}

{evaluator's full output}

Options:
1. Fix the issues and re-evaluate (remaining passes: {N})
2. Accept the current state and skip remaining passes
3. Adjust the contract (lower thresholds or remove criteria)
```

If the user chooses to fix:

- Fix the issues identified by the evaluator
- Run the next evaluation pass with a NEW sub-agent that sees:
  - The original contract
  - The NEW diff (including fixes)
  - The PREVIOUS evaluator's feedback (so it can verify fixes addressed the issues)
  - AGENTS.md

The new evaluator does NOT see the implementation conversation — only prior evaluation feedback.

---

## Step 4: Collect Adversarial Findings

If a configured adversarial review was launched in Step 3a, collect its results now. The background Bash task should have completed (or will complete shortly) — wait for the completion notification if it hasn't arrived yet, then read the full output.

If the background task is still running after all primary evaluation passes are complete, wait up to 5 minutes. If it times out or errored, note this in the final report and proceed with primary-only results.

**Before proceeding**, if the configured provider was Codex, scan the collected Codex output for rate-limit markers (see Step 3a → Rate-Limit Fallback). If any are present:

1. If `~/.codex/.openai-fallback-key` exists → run the fallback curl invocation from Step 3a and substitute the fallback output for the Codex output.
2. If the key file is missing → report to the user:
   `Codex adversarial review was rate-limited. Paste your OpenAI API key into ~/.codex/.openai-fallback-key (chmod 600) to enable fallback, then re-run /evaluate. Proceeding with primary-only evaluation for now.`

The adversarial review output challenges design decisions and assumptions — it does NOT score against the contract. Treat its findings as a separate evaluation dimension.

---

## Step 5: Final Report

After all passes complete (or the user stops early), output:

```markdown
## Evaluation Report: {Feature Name}

### Contract

- Tier: {N}
- Primary evaluator passes completed: {N}/{max}
- Adversarial review: YES ({provider} {model} {effort}) / NO ({reason})

### Results by Criterion (Primary Evaluator)

| Criterion | Threshold | Score  | Pass   |
| --------- | --------- | ------ | ------ |
| {name}    | {T}/10    | {S}/10 | YES/NO |
| ...       | ...       | ...    | ...    |

### Design Challenge Findings ({provider} {model} {effort})

{Paste adversarial review findings verbatim. These challenge the approach
itself — design tradeoffs, assumptions, and alternative approaches. Categorize as:}

- **Critical** — design issues that could cause real-world failures
- **Consider** — valid alternative approaches worth acknowledging
- **Acknowledged** — tradeoffs the team accepts knowingly

### Overall: {PASS / FAIL}

A FAIL from the configured primary evaluator (any criterion below threshold) = overall FAIL.
Critical design challenges from the configured adversarial review that the team cannot justify = overall FAIL.

### Issues to Address (if FAIL)

1. {criterion}: {specific issue and suggested fix}
2. ...

### What Passed Well

- {criterion}: {why it scored well}
```

---

## Rules

- **Never evaluate your own work in the same context** — always use a fresh sub-agent
- **The evaluator never sees implementation rationale** — only contract, diff, and conventions
- **Do not weaken criteria to make things pass** — if the implementation doesn't meet the bar, it fails
- **Run validation commands for real** — don't just read the code and guess
- **Prove validation coverage** — passing output is not enough unless the command contains an assertion or observable check for the criterion's behavior
- **Between passes, the user decides** — fix, accept, or adjust. Never auto-retry without user input
- **Kill background processes** before outputting results to prevent session hangs

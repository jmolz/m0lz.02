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
- Any on-demand rules in `.codex/rules/` relevant to changed files

If `AGENTS.md` is missing from the current git toplevel and the toplevel path contains `/.worktrees/`, use the sibling main checkout `AGENTS.md` above the `.worktrees` directory instead. Report the resolved AGENTS path in the evaluation output so a worktree cannot silently evaluate without project conventions.

---

## Step 3: Run Evaluation Pass(es)

Evaluation uses a **dual-model adversarial** approach. The Claude evaluator grades contract criteria formally. For Tier 2+, a parallel GPT-5.5 adversarial review challenges the design approach itself.

### Step 3a: Launch Codex Adversarial Review (all tiers)

For every tier (1, 2, and 3), launch a Codex adversarial review in the background **before** running the Claude evaluator. This runs GPT-5.5 xhigh in parallel.

Run the following via `Bash` with `run_in_background: true` (so Claude evaluation can proceed in parallel):

```bash
node "$HOME/.codex/plugins/cache/openai-codex/codex/1.0.4/scripts/codex-companion.mjs" \
  task --background --model gpt-5.5 --effort xhigh \
  "Adversarially evaluate against this contract: {paste contract criteria names and thresholds}. Use only the contract JSON, git diff/status, AGENTS.md, and relevant .codex/rules. Challenge design assumptions, failure modes, and production risks; do not edit files."
```

All tiers use `task --model gpt-5.5 --effort xhigh` for maximum reasoning depth. Do not use `adversarial-review --effort`; the installed companion does not parse effort flags for that subcommand.

If the script fails (e.g., Codex not configured), note the error and continue with Claude-only evaluation — do not block the entire evaluation.

The Codex review challenges the *approach* — was this the right design? What assumptions does it depend on? Where could it fail under real-world conditions? This is complementary to the Claude evaluator's formal contract grading.

#### Rate-Limit Fallback (ChatGPT Team → OpenAI API key)

The Codex CLI authenticates via the user's ChatGPT Team session by default. When that session hits its rate limit, fall back to a direct OpenAI Responses API call — do **not** run `codex login --api-key`, as that would overwrite the ChatGPT Team session (making recovery manual once the rate limit lifts).

**Fallback key location**: `~/.codex/.openai-fallback-key` — single line containing an OpenAI API key, `chmod 600`. If absent, skip fallback and report the rate-limit error verbatim.

**Rate-limit detection** (in collected Codex output, case-insensitive): any of `rate limit`, `rate_limit_exceeded`, `429`, `too many requests`, `usage cap`, `quota exceeded`.

**Fallback invocation** (only on detection, only if the key file exists):

```bash
OPENAI_FALLBACK_KEY=$(cat "$HOME/.codex/.openai-fallback-key")
EFFORT="xhigh"   # All tiers (Responses API supports xhigh for gpt-5.5)
cat > /tmp/codex-fallback-request.json <<'JSON'
{
  "model": "gpt-5.5",
  "reasoning": { "effort": "__EFFORT__" },
  "input": "__PROMPT__"
}
JSON
# Replace __EFFORT__ and __PROMPT__ with the actual values (jq or sed; escape JSON properly).
curl -sS https://api.openai.com/v1/responses \
  -H "Authorization: Bearer $OPENAI_FALLBACK_KEY" \
  -H "Content-Type: application/json" \
  --data-binary @/tmp/codex-fallback-request.json
```

Reserve sufficient output budget (OpenAI recommends ≥25k tokens for reasoning + output on gpt-5.5 at `xhigh`). Optionally pass `"max_output_tokens": 32000` and handle `status: "incomplete"` with `incomplete_details.reason === "max_output_tokens"` by retrying with a larger budget. Extract the visible answer from `response.output[].content[].text` (or `response.output_text`).

The `__PROMPT__` must include: the same focus text passed to `codex-companion task`, the contract criteria JSON, the full diff, and AGENTS.md — i.e., the same context Codex would have received. Construct the prompt string explicitly rather than relying on Codex's internal prompt templates (which are not accessible outside the CLI).

Treat the extracted text as the adversarial review output. Label it clearly in the final report as `Codex GPT-5.5 (OpenAI API fallback — ChatGPT Team rate-limited)`.

Once the ChatGPT Team rate limit lifts, no action is required: the primary Codex path resumes on the next invocation, and the fallback triggers only on failure.

### Step 3b: Run Claude Evaluator Pass(es)

For each Claude evaluation pass (1 for Tier 1, 1 for Tier 2, 3 for Tier 3 agent team), spawn a **fresh Claude opus 4.7 adaptive sub-agent** (`model: "opus"`) with the following prompt.

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

## Project Conventions

{paste AGENTS.md contents here}

## Your Task

For EACH criterion in the contract:

1. **Read the relevant code** — find the files that implement this criterion
2. **Run the validation** — execute the validation command or check the observable behavior
3. **Try to break it** — think of edge cases, malformed inputs, missing auth, concurrent access
4. **Score it 1-10** with specific evidence:
   - 1-3: Fundamentally broken or missing
   - 4-5: Partially works but has significant gaps
   - 6-7: Functional but insufficient — missing edge cases, weak validation, or convention drift
   - 8: Meets the bar — correct, robust, follows conventions, handles edge cases
   - 9: Exceeds expectations — well-tested, defensive, production-hardened
   - 10: Exceptional — comprehensive error handling, security-aware, zero gaps found

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

## Step 4: Collect Codex Findings (all tiers)

A Codex adversarial review was launched in Step 3a regardless of tier; collect its results now. The background Bash task should have completed (or will complete shortly) — wait for the completion notification if it hasn't arrived yet, then read the full output.

If the background task is still running after all Claude evaluation passes are complete, wait up to 5 minutes. If it times out or errored, note this in the final report and proceed with Claude-only results.

**Before proceeding**, scan the collected Codex output for rate-limit markers (see Step 3a → Rate-Limit Fallback). If any are present:

1. If `~/.codex/.openai-fallback-key` exists → run the fallback curl invocation from Step 3a and substitute the fallback output for the Codex output.
2. If the key file is missing → report to the user:
   `Codex adversarial review was rate-limited. Paste your OpenAI API key into ~/.codex/.openai-fallback-key (chmod 600) to enable fallback, then re-run /evaluate. Proceeding with Claude-only evaluation for now.`

The Codex review output challenges design decisions and assumptions — it does NOT score against the contract. Treat its findings as a separate evaluation dimension.

---

## Step 5: Final Report

After all passes complete (or the user stops early), output:

```markdown
## Evaluation Report: {Feature Name}

### Contract

- Tier: {N}
- Claude passes completed: {N}/{max}
- Codex adversarial review: YES (all tiers — GPT-5.5 xhigh)

### Results by Criterion (Claude Evaluator)

| Criterion | Threshold | Score  | Pass   |
| --------- | --------- | ------ | ------ |
| {name}    | {T}/10    | {S}/10 | YES/NO |
| ...       | ...       | ...    | ...    |

### Design Challenge Findings (Codex GPT-5.5 xhigh — all tiers)

{Paste Codex adversarial review findings verbatim. These challenge the approach
itself — design tradeoffs, assumptions, and alternative approaches. Categorize as:}

- **Critical** — design issues that could cause real-world failures
- **Consider** — valid alternative approaches worth acknowledging
- **Acknowledged** — tradeoffs the team accepts knowingly

### Overall: {PASS / FAIL}

A FAIL from the Claude evaluator (any criterion below threshold) = overall FAIL.
Critical design challenges from Codex that the team cannot justify = overall FAIL.

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
- **Between passes, the user decides** — fix, accept, or adjust. Never auto-retry without user input
- **Kill background processes** before outputting results to prevent session hangs

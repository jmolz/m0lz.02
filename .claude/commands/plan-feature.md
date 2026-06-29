---
description: Research and create a comprehensive implementation plan for a feature
argument-hint: <feature-description>
---

# Plan Feature: $ARGUMENTS

## Mission

Create a detailed, actionable implementation plan through systematic research, codebase analysis, and strategic thinking. We do NOT write code in this phase. The plan must contain ALL context needed for one-pass implementation success.

**Core Principle**: Context is king. The execution agent gets only this plan — no conversation history, no prior research. Everything it needs must be in the document.

---

## Phase 1: Feature Understanding

Restate the feature in your own words. Identify:

1. **Problem being solved** — what user pain point or capability gap?
2. **Success criteria** — what does "done" look like?
3. **Scope boundaries** — what's explicitly in/out of scope?
4. **Affected areas** — which parts of the codebase change?

Create a user story:

```
As a <user type>
I want to <action>
So that <benefit>
```

---

## Phase 2: Codebase Intelligence

**Use sub-agents for parallel research.** Spawn separate agents for:

**Sub-agent A — Affected code deep-dive:**
Read all relevant source files. Map the current data flow. Identify every file that needs to change.

**Sub-agent B — Pattern recognition:**
Find similar implementations in the codebase. Extract naming conventions, error handling patterns, logging patterns, import styles. Check CLAUDE.md for project rules.

**Sub-agent C — Test patterns:**
Find existing test files near the affected area. Understand mocking patterns, assertion style, test organization.

**Sub-agent D — Recent history:**

```bash
git log --oneline -15
```

Read recent commits touching relevant files to understand change patterns.

Synthesize findings into a brief summary of: current state, gaps, constraints.

---

## Phase 3: External Research (if needed)

If the feature involves new APIs, libraries, or unfamiliar patterns, research:

- Official documentation (with specific section links)
- Known gotchas and version incompatibilities
- Community patterns for the problem domain

---

## Phase 4: Strategic Thinking

Before writing tasks, reason through:

- **Where does this logic belong?** Apply SRP.
- **What's the dependency order?** Types before implementations, backend before frontend.
- **What could go wrong?** Edge cases, race conditions, error states.
- **How will we validate?** Be specific — exact commands, URLs, expected outputs.
- **What environment setup is needed?** New env vars, migrations, seeds.

---

## Phase 5: Write the Plan

Save to: `.claude/plans/{kebab-case-name}.md`

Use this template as the output structure — fill in every section with real data from the research:

@.claude/templates/plan-template.md

### Worktree Execution (REQUIRED)

All new features MUST be executed in a git worktree to isolate work from main. The plan must include a `## Worktree` section specifying:

```markdown
## Worktree

- **Branch**: `feature/{kebab-case-name}`
- **Path**: `.worktrees/{kebab-case-name}`
```

This ensures:
- Main stays clean and deployable at all times
- No risk of regressing or overwriting previously completed work
- Clean merge history when the feature is complete
- Parallel features can be developed in separate worktrees without conflict

---

## Phase 5.5: Adversarial Plan Review

Plans suffer the same confirmation bias that `/evaluate` was built to break: when you ask the agent that wrote the plan whether the plan looks good, you get reassurance, not critique. This phase subjects the draft plan to review from agents that did NOT write it, attacking the specific failure modes LLMs consistently fall into.

### Philosophical foundation

- **Karpathy's four LLM failure modes** (from [andrej-karpathy-skills](https://github.com/forrestchang/andrej-karpathy-skills)): plans drift into (1) hidden assumptions, (2) overcomplication, (3) orthogonal scope creep, and (4) aspirational success criteria. Adversaries attack each explicitly.
- **PoetiQ's refinement loop** (ARC-AGI-2 winning architecture, Dec 2025): a plan is not complete until an independent verifier confirms it. The planner's confidence is not the signal; surviving critique is. Iterative refinement beats single-pass reasoning.

### The Critique Prompt

The same prompt is used by all adversaries — every tier (Tier 1, Tier 2, Tier 3) runs both adversaries in parallel: a fresh Claude Fable 5 xhigh adaptive sub-agent AND a Codex GPT-5.5 xhigh task. It must attack six dimensions:

```
You are reviewing an implementation plan BEFORE code is written. You did NOT write this plan and have no stake in its approval. Your job is to find weaknesses along six dimensions. Be specific. Cite plan sections by heading. Reject generic concerns.

1. HIDDEN ASSUMPTIONS (Karpathy "Think Before Coding"): What does the plan silently assume about the system, user intent, or environment? Where were multiple interpretations possible but one was picked without surfacing the alternative? Name each assumption and its likelihood of being wrong.

2. OVERCOMPLICATION (Karpathy "Simplicity First"): What is speculative, bloated, or added "for flexibility" without being requested? Which specific parts would a senior engineer flag as over-engineered? What abstraction exists for single-use code? What error handling covers scenarios that can't happen?

3. SCOPE DRIFT (Karpathy "Surgical Changes"): Does every task trace directly to the user's feature request? What orthogonal improvements, refactors, or cleanups are sneaking in? What dead code is being deleted that wasn't asked for?

4. WEAK SUCCESS CRITERIA (Karpathy "Goal-Driven Execution"): Are the success criteria transformed into verifiable goals with explicit checks (tests, commands, observable outputs)? Which criteria are aspirational ("works well", "is clean") and must be rewritten as concrete assertions? Can an executor loop independently against these criteria, or will it stall waiting for clarification?

5. VERIFICATION-LOOP READINESS (PoetiQ refinement): For each task, does the validation step let an executor know "done" without human input? Trace one execution path: where does the executor stall? What is the shortest feedback signal for each task?

6. REASONING DIVERGENCE (PoetiQ cross-model verification): Setting aside the plan's internal logic, what conclusions would you reach independently about the best approach? Where does your reasoning diverge from the plan's? Name at least one genuine alternative path the plan did not consider.

Output format:

### Critical (plan must change before proceeding)
- {dimension}: {specific weakness}. Section: {heading}. Suggested resolution: {change}

### Consider (valid concern for user to weigh)
- {dimension}: {specific weakness}. Section: {heading}

### Reasoning divergence (alternative paths not taken)
- {alternative approach and why it might be better}

Do NOT output praise. Do NOT pad with generic concerns. Specificity is the quality bar — not volume.
```

The adversary receives: the plan file, `CLAUDE.md`, and `git log --oneline -15` — nothing else. It does NOT see the planning conversation, Phase 1–4 research, or any prior reasoning.

### All Tiers: Parallel Adversarial Streams

Run two independent adversaries **in parallel** (not sequentially) for every tier — Tier 1, Tier 2, and Tier 3. Self-critique by the planning agent is no longer sufficient at any tier; bias from the planner is too strong. Tier only changes the Claude sub-agent type and the refinement-cycle ceiling — both adversaries always run.

**Stream A — Fresh Claude Fable 5 xhigh adaptive sub-agent.** Spawn via `Task` tool with `model: "claude-fable-5"` (Claude Fable 5 xhigh adaptive):
- Tier 1 → `subagent_type: "general-purpose"`
- Tier 2 → `subagent_type: "general-purpose"`
- Tier 3 → `subagent_type: "architect"`
- Pass the critique prompt with plan + CLAUDE.md + git log appended.

**Stream B — Codex GPT-5.5 xhigh task** (runs in background via `Bash` with `run_in_background: true`). Always `--model gpt-5.5 --effort xhigh` regardless of tier:

```bash
node "$HOME/.codex/plugins/cache/openai-codex/codex/1.0.4/scripts/codex-companion.mjs" \
  task --background --model gpt-5.5 --effort xhigh \
  "{the critique prompt above, with plan + CLAUDE.md + git log appended}"
```

#### Rate-Limit Fallback

Inherits the fallback from `/evaluate`. If Stream B output contains rate-limit markers (`rate limit`, `rate_limit_exceeded`, `429`, `too many requests`, `usage cap`, `quota exceeded`), read `~/.codex/.openai-fallback-key` and retry via direct OpenAI Responses API:

- `model: "gpt-5.5"`, `reasoning.effort: "xhigh"` (all tiers)
- `max_output_tokens: 32000`; on `status: "incomplete"` with `reason: "max_output_tokens"`, retry with larger budget
- Do NOT run `codex login --api-key` (overwrites ChatGPT Team session)

See `/evaluate` Step 3a for the exact curl recipe.

### Refinement Loop

PoetiQ's core insight: iterative refinement beats single-pass reasoning. Refinement cycles per tier — Tier 1: 1 cycle, Tier 2: 1 cycle, Tier 3: up to 2 cycles:

**Cycle 1** — Run both streams, merge findings.
- Zero Critical findings → proceed to persistence.
- Critical findings exist → present to user:
  ```
  Adversarial review found {N} critical issues. Options:
  1. Revise the plan to address them (one more refinement cycle)
  2. Accept and mark as Acknowledged with explicit reasoning
  3. Mixed — revise some, acknowledge others
  ```

**Cycle 2 (Tier 3 only)** — If user chose revise: rewrite affected plan sections, then re-run both streams with the NEW plan + the previous critique (so reviewers can verify fixes addressed the concerns). After Cycle 2, any remaining Critical findings must be Acknowledged by the user with explicit reasoning — no silent dismissal.

Tier 1 and Tier 2 run a single cycle; skip Cycle 2. After Cycle 1, any remaining Critical findings must be Acknowledged by the user with explicit reasoning — no silent dismissal.

### Self-Auditing Stop Condition

Stop refinement when any of:
- Zero Critical findings remain, or
- All Critical findings Acknowledged with explicit reasoning, or
- Tier refinement-cycle limit reached (Tier 1: 1, Tier 2: 1, Tier 3: 2)

Do not over-refine. A plan that survives informed critique is ready.

### Persist Findings in the Plan File

Append findings to the plan file under a new `## Adversarial Review` section (the template reserves this slot above `## Notes`):

```markdown
## Adversarial Review

**Tier**: {N}
**Reviewers**: Claude Fable 5 xhigh adaptive sub-agent + Codex GPT-5.5 xhigh
**Refinement cycles**: {N}
**Attack framework**: Karpathy four principles + PoetiQ cross-model verification

### Critical (resolved before proceeding)
1. {finding} — section: {heading} — resolution: {revised text or Acknowledged with reasoning}

### Consider (flagged, not acted on)
1. {finding} — section: {heading}

### Reasoning divergence (alternative paths not taken)
1. {alternative} — why not: {reasoning}
```

### Feed into Phase 6

Findings feed contract negotiation: Critical items become non-negotiable contract criteria (elevated thresholds); Consider items become negotiable additions the user can accept or reject during Phase 6. The adversarial reviewers often surface criteria the planner missed — the user's Phase 6 contract should reflect that enriched picture.

---

## Phase 6: Contract Negotiation

After the adversarial review (Phase 5.5) completes, draft a **contract** — the structured success criteria that `/evaluate` will grade against after implementation. Incorporate adversarial findings:

- Each **Critical** finding resolved via revision should be reflected in contract criteria (the weakness the adversary caught becomes a criterion to prove fixed)
- Each **Critical** finding marked Acknowledged should appear as a contract criterion with an explicit pass-condition the user accepts
- **Consider** findings can optionally become new criteria the user agrees to test against

This keeps the contract honest: it grades against the plan as challenged, not the plan as originally self-assessed.


### Draft the Contract

Based on the plan's success criteria and implementation tasks, write a JSON contract block in the plan file's `## Contract` section:

1. **Set the tier** based on scope (determines Claude evaluator passes; the dual Claude Fable 5 xhigh adaptive + GPT-5.5 xhigh adversarial review runs at every tier):
   - **Tier 1** (1 Claude pass + Claude Fable 5 xhigh adaptive sub-agent + GPT-5.5 xhigh adversarial review): Bug fixes, simple endpoints, UI tweaks
   - **Tier 2** (1 Claude pass + Claude Fable 5 xhigh adaptive sub-agent + GPT-5.5 xhigh adversarial review): New features touching multiple domains, integrations, schema changes
   - **Tier 3** (Claude agent team + Claude Fable 5 xhigh adaptive architect sub-agent + GPT-5.5 xhigh adversarial review): New pipeline phases, agent types, architectural changes

2. **Write criteria** — each must be:
   - Independently testable (no "works well" or "looks good")
   - Paired with a validation method (command, API call, or observable behavior)
   - Assigned a threshold between 8 and 10, inclusive. **The minimum allowed threshold is 8.** Use 8 for standard functionality and UX criteria, 9 for correctness or regression-safety criteria, and 10 for security, data integrity, secret handling, and multi-tenancy concerns. Any criterion below 8 is not allowed — if something feels like it should be threshold 7, the criterion is either too vague to test or shouldn't be in the contract at all.

3. **Include at least one negative criterion** — something that should NOT happen (e.g., "Returns 403 when accessing another org's data", "Does not expose user email in public API response")

4. **Validate before presenting** — before presenting the contract to the user, scan every `threshold` value. If any is below 8, raise it or remove the criterion. The negotiation step assumes the floor is already respected; the user should not have to remind you.

### Present for Negotiation

Present the draft contract to the user with:

```
## Proposed Contract

Tier: {N} ({N} evaluation pass(es) after implementation)

Criteria:
1. {criterion} — threshold: {N}/10 — validation: {method}
2. {criterion} — threshold: {N}/10 — validation: {method}
...

Does this contract look right? You can:
- Raise/lower the tier
- Add criteria I missed (especially domain-specific edge cases)
- Adjust thresholds
- Change validation methods
```

**Do NOT proceed to save the final plan until the user approves the contract.** The user knows domain constraints the agent cannot infer — multi-tenancy scoping, RBAC rules, pipeline ordering, integration quirks. Their input during negotiation is what makes the contract valuable.

Once approved, save the contract into the plan file's `## Contract` section.

---

## Output

1. Save the plan file with its full path
2. Print a summary: number of tasks, affected files, complexity (low/med/high)
3. Print the approved contract summary (tier + criteria count)
4. Flag any risks or open questions
5. Rate confidence for one-pass execution success (X/10)

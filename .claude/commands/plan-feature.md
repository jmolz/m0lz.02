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

---

## Phase 6: Contract Negotiation

After writing the plan, draft a **contract** — the structured success criteria that `/evaluate` will grade against after implementation.

### Draft the Contract

Based on the plan's success criteria and implementation tasks, write a JSON contract block in the plan file's `## Contract` section:

1. **Set the tier** based on scope (determines both Claude evaluator passes AND whether GPT-5.4 adversarial review runs):
   - **Tier 1** (Claude only, 1 pass): Bug fixes, simple endpoints, UI tweaks
   - **Tier 2** (1 Claude pass + GPT-5.4 adversarial review): New features touching multiple domains, integrations, schema changes
   - **Tier 3** (Claude agent team + GPT-5.4 xhigh adversarial review): New pipeline phases, agent types, architectural changes

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

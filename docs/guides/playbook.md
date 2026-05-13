# AI Coding Playbook

A quick reference for running the PICE workflow. Print it, pin it, or keep it open in a tab.

## The PICE Loop

```
1. pice prime           Orient on codebase
2. Discuss              Talk through the feature
3. pice plan "..."      Research + plan + CONTRACT
      |  (negotiate contract with user)
   CONTEXT RESET (new session)
      |
4. pice execute <plan>  Implement from plan
5. pice evaluate <plan> Adversarial eval vs contract
      |  (fix if FAIL, re-evaluate per tier)
6. pice review          Code review + regressions
7. Human review         Manual testing
8. pice commit          Standardized commit
```

See [methodology overview](../methodology/overview.md) for a deeper explanation of each phase.

## Evaluation Tiers (Dual-Model Adversarial)

| Tier | Claude Evaluator | Codex Adversarial (GPT-5.5) | Use When |
|------|------------------|------------------------------|----------|
| 1 | 1 pass | -- | Bug fixes, simple endpoints, UI tweaks |
| 2 | 1 pass | 1 design challenge (high) | New features, integrations, schema changes |
| 3 | Claude agent team | 1 design challenge (xhigh) | Architectural changes, new pipeline phases |

Claude evaluates contract criteria formally (scores, pass/fail). GPT-5.5 challenges whether the approach itself is sound. Different model families have different blind spots, so running both in parallel maximizes coverage.

Both evaluators are adversarial by design -- they never see implementation reasoning, only the contract, code diff, and project guidance.

See [evaluation deep-dive](../methodology/evaluate.md) for details on the tier system and contract enforcement.

## WISC Context Management

| Letter | Strategy | Key Idea |
|--------|----------|----------|
| **W**rite | Externalize memory | Git commits, handoffs, plan files |
| **I**solate | Sub-agents for research | Keep main context clean |
| **S**elect | Layered context | Load just-in-time, not just-in-case |
| **C**ompress | Last resort | Compact with focus, or handoff |

## Context Budget Rules of Thumb

| Tokens | Status | Action |
|--------|--------|--------|
| < 100K | Green | Full speed ahead |
| 100-250K | Yellow | Be mindful, avoid unnecessary reads |
| 250K+ | Red | Compact or handoff immediately |

## Greenfield Checklist

1. Brain dump conversation about the idea
2. Ask the agent to ask YOU questions (reduces assumptions)
3. Generate a PRD from the conversation
4. Review the PRD thoroughly
5. Generate project guidance rules from the codebase
6. Set up environment variables
7. For each phase in the PRD: run a PICE loop
8. After Phase 1: set up a regression test harness

## Brownfield Checklist

1. Generate project guidance by reverse-engineering existing code
2. Review and customize the generated rules
3. Create on-demand rules for major subsystems
4. Run `pice prime` to verify agent understanding
5. Use PICE loops for new features
6. Build regression tests from existing functionality

See the [brownfield guide](brownfield.md) for a detailed walkthrough.

## Commands

| Command | When to Use |
|---------|-------------|
| `pice prime` | Start of every session |
| `pice plan <desc>` | Start of every PICE loop (includes contract) |
| `pice execute <path>` | After planning, in a fresh session |
| `pice evaluate <path>` | After execution -- dual-model adversarial eval |
| `pice review` | After evaluation -- regressions + code review |
| `pice commit` | After every successful implementation |
| `pice handoff` | When session is getting long |
| `pice status` | Check active plans and evaluation state |
| `pice metrics` | View quality data across PICE loops |
| `pice benchmark` | Before/after effectiveness comparison |

## Five Golden Rules

1. **Context is precious** -- keep it lean, reset between phases
2. **Commandify everything** -- if you do it twice, make it a command
3. **Git log = long-term memory** -- detailed, standardized commits
4. **Evolve the AI layer** -- bugs are opportunities to improve rules
5. **Separate evaluation from generation** -- never grade your own work, and use multiple model families for adversarial review

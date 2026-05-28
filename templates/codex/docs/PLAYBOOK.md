# AI Coding Playbook — Quick Reference

Print this. Pin it. Tape it to your monitor.

---

## The PICE Loop (Plan → Implement → Contract-Evaluate)

```
┌──────────────────────────────────────────────────┐
│  1. /prime           Orient on codebase           │
│  2. Conversation     Discuss the feature           │
│  3. /plan-feature    Research + plan + CONTRACT     │
│         ↓  (negotiate contract with user)          │
│    CONTEXT RESET (new session)                     │
│         ↓                                          │
│  4. /execute <plan>  Implement from plan            │
│  5. /evaluate <plan> Adversarial eval vs contract  │
│         ↓  (fix if FAIL, re-evaluate per tier)     │
│  6. /review          Standard review + regressions │
│  7. Human review     Manual testing                │
│  8. /commit          Standardized commit           │
│  9. Evolve           Update AI layer if needed     │
└──────────────────────────────────────────────────┘
```

### Evaluation Tiers (Dual-Model Adversarial)

| Tier | Primary Evaluator | Configured Adversarial | Use When |
|------|-------------------|--------------------------|----------|
| 1 | 1 Opus 4.8 pass | 1 Codex task | Bug fixes, simple endpoints, UI tweaks |
| 2 | 1 pass | 1 configured design challenge | New features, integrations, schema changes |
| 3 | primary evaluator team | 1 configured design challenge | Architectural changes, new pipeline phases |

**Dual-model rationale:** The configured primary evaluator scores contract criteria formally. The configured adversarial provider challenges whether the *approach itself* is right — questioning design tradeoffs, assumptions, and alternative approaches. Different model families have different blind spots, so running both in parallel maximizes coverage.

Both evaluators are adversarial by design — they never see implementation reasoning, only the contract, code diff, and AGENTS.md. This eliminates self-evaluation bias.

## WISC Context Management

| Letter | Strategy | Key Idea |
|--------|----------|----------|
| **W**rite | Externalize memory | Git commits, handoffs, plan files |
| **I**solate | Sub-agents for research | Keep main context clean |
| **S**elect | Layered context | Load just-in-time, not just-in-case |
| **C**ompress | Last resort | `/compact` with focus, or `/handoff` |

## Context Budget Rules of Thumb

| Tokens | Status | Action |
|--------|--------|--------|
| < 100K | Green | Full speed ahead |
| 100–250K | Yellow | Be mindful, no unnecessary reads |
| 250K+ | Red | Compact or handoff immediately |

## Greenfield Checklist

- [ ] Brain dump conversation about the idea
- [ ] Ask agent to give YOU questions (reduces assumptions)
- [ ] `/create-prd` — generate PRD from conversation
- [ ] Review PRD thoroughly
- [ ] `/create-rules` — generate AGENTS.md
- [ ] Set up `.env.example` and actual `.env`
- [ ] For each phase in PRD: run a PICE loop
- [ ] After Phase 1: set up regression test harness

## Brownfield Checklist

- [ ] `/create-rules` — reverse-engineer from existing code
- [ ] Review + customize generated AGENTS.md
- [ ] Create on-demand rules for major subsystems
- [ ] `/prime` to verify agent understanding
- [ ] Use PICE loops for new features
- [ ] Build regression tests from existing functionality

## Commands Cheatsheet

| Command | When to Use |
|---------|-------------|
| `/prime` | Start of every session |
| `/create-prd` | Once, at project inception |
| `/create-rules` | Once, then evolve manually |
| `/plan-feature <desc>` | Start of every PICE loop (includes contract) |
| `/execute <path>` | After planning, in fresh session |
| `/evaluate <path>` | After execution — dual-model adversarial eval vs contract |
| `/review` | After evaluation — regressions + code review |
| `/commit` | After every successful implementation |
| `/handoff` | When session is getting long |
| `/validate` | Before shipping, after major changes |
| `/context` | Anytime — monitor token usage |
| `/compact "focus on X"` | When approaching 250K tokens |
| `/btw` | Quick questions without context cost |

## Five Golden Rules

1. **Context is precious** — keep it lean, reset between phases
2. **Commandify everything** — if you do it twice, make it a `/command`
3. **Git log = long-term memory** — detailed, standardized commits
4. **Evolve the AI layer** — bugs are opportunities to improve rules
5. **Separate evaluation from generation** — never grade your own work, and use multiple model families for adversarial review

---
name: agent-teams
description: "Use this skill when coordinating parallel work, creating agent teams, or when the task would benefit from multiple specialists working simultaneously. Triggers: 'agent team', 'parallel', 'spawn teammates', 'create a team', 'work in parallel', 'multi-agent', 'review from multiple angles', 'competing hypotheses', 'split the work', 'team of agents', or when a task naturally decomposes into independent parallel workstreams like full-stack features, code reviews, security audits, debugging investigations, refactors, or test coverage sprints."
---

# Agent Teams: Patterns and Prompts

Use agent teams when parallel exploration or implementation adds real value. Each teammate is a separate Codex instance with its own context window. They communicate directly with each other and coordinate through a shared task list.

## When to Use Agent Teams vs Sub-Agents

- **Agent teams**: teammates need to communicate, challenge each other, or coordinate on shared artifacts
- **Sub-agents**: focused tasks where only the result matters (research, exploration, analysis)
- **Single session**: sequential work, quick changes, or tasks with heavy dependencies between steps

## Key Rules

1. **3-5 teammates** is the sweet spot — more adds overhead without proportional benefit
2. **Each teammate owns different files** — two editing the same file causes overwrites
3. **Tell the lead to wait** — otherwise it starts implementing instead of delegating
4. **Use plan approval for risky tasks** — "Require plan approval before making changes"
5. **Give context in the prompt** — teammates don't inherit your conversation history

## Prompt Template

```
Create an agent team to [OBJECTIVE]. Spawn [N] teammates:

1. [Role] — [responsibilities, files they own, what they do first]
2. [Role] — [responsibilities, files they own, what they do first]
3. [Role] — [responsibilities, files they own, what they do first]

Coordination rules:
- [Who goes first / dependencies]
- [How they share findings]
- [File ownership — no overlap]
- [Validation to run at the end]

Wait for all teammates to finish before synthesizing results.
```

## Ready-to-Use Patterns

### Code Review (3 teammates)

Spawn a security reviewer, performance reviewer, and correctness reviewer. Each applies a different lens to the same changes. They share findings and challenge each other. Synthesize a prioritized final review.

### Full-Stack Feature (3 teammates)

Spawn a backend developer, frontend developer, and test engineer. Backend and frontend agree on API contract first. Test engineer writes tests as the others build. No teammate edits files owned by another.

### Competing Hypotheses Debug (4 teammates)

Spawn 4 investigators, each with a different theory about the bug. They actively try to disprove each other's hypotheses. The surviving theory with the strongest evidence wins.

### Architecture Review (4 teammates)

Spawn a dependency analyst, complexity assessor, pattern consistency checker, and scalability reviewer. Each examines the codebase from a different angle. Cross-reference findings — coupling issues often explain complexity hotspots.

### Security Audit (4 teammates)

Spawn a secrets scanner, injection auditor, auth/authz auditor, and dependency auditor. Each focuses on a different attack surface. They challenge each other's findings to reduce false positives.

### Pre-Release Test Blitz (5 teammates)

Spawn happy path tester, error path tester, boundary tester, auth tester, and data integrity tester. Each works independently on different test categories. Share bugs found so others can check for related issues.

### Large-Scale Refactor (4 teammates)

Spawn a planner, two implementers (each owning different files), and a validator who runs tests continuously. Planner creates the plan first, gets approval, then implementers execute in parallel.

### Performance Audit (4 teammates)

Spawn frontend, API, database, and infrastructure performance analysts. Each profiles their layer. Share findings that cross boundaries — a slow API often traces to a missing DB index.

### Documentation Overhaul (4 teammates)

Spawn a doc auditor, README writer, API documenter, and rules updater. Auditor goes first to identify problems. The other three fix their respective areas in parallel.

### Post-Incident Analysis (4 teammates)

Spawn a timeline builder, root cause analyst, blast radius assessor, and prevention planner. They share findings throughout. Prevention planner challenges: "If we fix that, could this still happen differently?"

### Technical Breakdown for Planning (3 teammates)

Spawn a requirements analyst, architecture planner, and effort estimator. Work sequentially: requirements first, then architecture, then estimates. Each challenges the previous teammate's work.

## Managing Teams

- `Shift+Down` to cycle between teammates
- Talk to any teammate directly by cycling to them and typing
- `Ctrl+T` to toggle the task list
- Ask the lead to "shut down the team" when done
- If the lead starts doing work itself: "Wait for your teammates to complete their tasks"

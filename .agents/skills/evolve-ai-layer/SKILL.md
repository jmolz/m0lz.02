---
name: evolve-ai-layer
description: "Use this skill when improving the AI layer — updating AGENTS.md, creating or editing rules, adding skills, refining commands, or diagnosing why the agent made a mistake. Triggers: 'update rules', 'improve AGENTS.md', 'add a rule', 'create a skill', 'why did you do that wrong', 'the agent keeps making this mistake', 'evolve', 'meta-reasoning'. Provides patterns for systematically improving the AI context system."
---

# Evolve AI Layer Skill

## When to Use
The agent should read this skill when asked to improve the AI context layer — rules, commands, skills, on-demand docs — or when diagnosing repeated mistakes.

## Diagnosis: Why Did the Agent Make a Mistake?

When something went wrong, categorize the root cause:

| Root Cause | Fix Location |
|-----------|-------------|
| Wrong coding pattern used | `AGENTS.md` code patterns section |
| Wrong pattern in specific area | `.Codex/rules/{area}.md` |
| Didn't know about a library gotcha | `.Codex/docs/{library}.md` |
| Made an assumption about requirements | Improve `/plan-feature` question phase |
| Forgot a step in a workflow | `.Codex/commands/{workflow}.md` |
| Repeated a dead-end approach | Add to `/handoff` or commit message |
| Didn't validate properly | Update `/validate` or `/execute` validation |

## Updating AGENTS.md

### When to Update
- A pattern was wrong or missing
- Commands changed (new dev/test/build commands)
- Architecture changed (new modules, changed data flow)
- A convention should be enforced project-wide

### Rules for AGENTS.md
- Keep it under 500 lines — this loads EVERY session
- Be specific: "Use `zod` for API input validation" not "validate inputs"
- Include the WHY when a rule isn't obvious
- If a rule only applies to one area, move it to `.Codex/rules/` instead

## Creating On-Demand Rules

### When to Create
- Conventions specific to one part of the codebase
- Detailed patterns too long for AGENTS.md
- Guidelines that only matter when working in certain files

### Structure
```markdown
---
paths:
  - "src/area/**"
  - "**/*.pattern.ts"
---

# {Area} Conventions

## Key Patterns
{The most important things to know}

## Do / Don't
- ✅ Do this
- ❌ Don't do that

## Examples
{Actual code examples from this codebase}
```

## Creating Skills

### When to Create
- The agent needs specialized knowledge for a type of task
- A process has multiple steps that should be followed consistently
- Knowledge that applies across projects (debugging, reviewing, researching)

### Structure
```
.Codex/skills/{skill-name}/
└── SKILL.md
```

The `description` in frontmatter is what the agent sees first. Make it keyword-rich so the agent knows when to load the full skill.

## Creating Reference Docs

### When to Create
- Deep technical documentation (architecture, data model, integrations)
- Content too heavy for rules (200+ lines)
- Knowledge that sub-agents should scout before the main agent loads

### Location
`.Codex/docs/{topic}.md`

Start with a clear header summarizing what the doc covers and when it's relevant — sub-agents read this to decide whether to load the full document.

## Updating Commands

### When to Update
- The workflow has a step that keeps getting missed
- Validation needs to be more thorough
- A new project pattern should be part of the standard flow

### Remember
- Commands are what YOU invoke (`/commit`, `/prime`)
- Skills are what the AGENT decides to read
- Rules auto-load based on file paths
- Docs are loaded by scout sub-agents

## After Evolving

1. Test the change — run `/prime` and check the agent's understanding
2. Include AI layer changes in your next `/commit` (the Context section)
3. Note the change in your git log so future sessions understand the evolution

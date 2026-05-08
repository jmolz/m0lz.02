---
name: research
description: "Use this skill when performing research tasks — codebase analysis, web research, documentation gathering, technology evaluation, or library comparison. Triggers: 'research', 'investigate', 'explore', 'find out', 'compare options', 'what are best practices for', 'look into'. Provides structured research patterns for both codebase and external research."
---

# Research Skill

## When to Use
The agent should read this skill when doing any research task — whether exploring a codebase, researching libraries, or gathering documentation.

## Codebase Research

### Finding Patterns
```bash
# Find all files matching a pattern
find . -name "*.ts" -not -path "*/node_modules/*" | head -30

# Search for a specific pattern in code
grep -rn "pattern" --include="*.ts" src/

# Find usage of a function or type
grep -rn "functionName" --include="*.ts" --include="*.tsx" .

# Find recent changes to a file or area
git log --oneline -10 -- path/to/file
git log --oneline -10 -- "src/area/**"

# See who changed what recently
git log --oneline --all --since="2 weeks ago" | head -20
```

### Mapping Dependencies
- Read import statements to understand module relationships
- Check package.json/pyproject.toml for external dependencies
- Trace data flow: entry point → handler → service → storage
- Identify shared utilities and types

### Understanding Architecture
- Start with entry points (main, index, app)
- Follow the request/response path
- Identify boundaries between modules/packages
- Look for patterns: MVC, layered, event-driven, etc.

## External Research

### Library Evaluation
When comparing libraries or tools, assess:

1. **Maintenance**: Last commit, open issues, release frequency
2. **Adoption**: GitHub stars, npm/pip weekly downloads
3. **Compatibility**: Works with current tech stack versions
4. **Bundle size**: Impact on build (for frontend)
5. **API quality**: Clean, well-typed, good docs
6. **Migration path**: Easy to adopt incrementally

### Documentation Gathering
When collecting docs for a plan or implementation:

- Find the **official docs** first (not blog posts or tutorials)
- Link to **specific sections**, not just the homepage
- Note the **version** the docs apply to
- Capture **gotchas and known issues** from GitHub issues
- Look for **migration guides** if upgrading

## Research Output

Always structure research findings as:

```markdown
## Research: {Topic}

### Key Findings
- {Finding 1 with source}
- {Finding 2 with source}

### Recommendation
{What to do and why}

### Risks / Gotchas
- {Risk 1}
- {Risk 2}

### Sources
- [Source 1](url) — {what we got from it}
- [Source 2](url) — {what we got from it}
```

## As a Sub-Agent

When running as a sub-agent for research:
- Be thorough in exploration but concise in reporting
- Read broadly, summarize tightly
- Include specific file paths, line numbers, and URLs
- Flag anything uncertain or that needs human decision
- Your summary is all the main agent will see — make it count

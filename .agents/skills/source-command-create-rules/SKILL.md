---
name: "source-command-create-rules"
description: "Generate CLAUDE.md global rules from codebase analysis"
---

# source-command-create-rules

Use this skill when the user asks to run the migrated source command `create-rules`.

## Command Template


# Create Global Rules

Analyze the codebase and generate a AGENTS.md file with project-specific constraints and conventions.

## Phase 1: Discover

### Identify What Exists

Check for existing documentation:
- `PRD.md`, `.codex/PRD.md` — product spec
- `README.md` — project overview
- `package.json`, `pyproject.toml`, `Cargo.toml` — dependencies
- `tsconfig.json`, `vite.config.*`, `next.config.*` — build config
- Existing test files and patterns
- CI/CD workflows (`.github/workflows/`)
- Docker files

### Identify Project Type

| Type | Indicators |
|------|-----------|
| Full-stack web app | Separate client/server, API routes |
| Frontend SPA | React/Vue/Svelte, no server |
| API/Backend | Express/FastAPI/etc, no frontend |
| Library/Package | `exports` in package.json, publishable |
| CLI Tool | `bin` in package.json |
| Monorepo | Multiple packages, workspaces |

### Map Structure

```bash
tree -L 3 -I 'node_modules|__pycache__|.git|dist|build|.next|venv*' 2>/dev/null || find . -maxdepth 3 -type f -not -path '*/node_modules/*' -not -path '*/.git/*' | head -60
```

## Phase 2: Analyze

### Extract Patterns (use sub-agents for research if needed)

From existing code, identify:
- **Naming**: files, functions, classes, variables
- **Structure**: how code is organized within files
- **Errors**: how errors are created and handled
- **Types**: how types/interfaces are defined
- **Imports**: relative vs absolute, named vs namespace
- **Testing**: framework, structure, patterns
- **Logging**: strategy and format

### Research Best Practices

If this is a new project with a PRD but no code yet, spin up sub-agents to research:
- Testing strategy for the tech stack
- Logging best practices
- Common patterns for the framework
- Component/module organization conventions

## Phase 3: Generate AGENTS.md

Write to `AGENTS.md` in the project root. **Keep it under 500 lines.**

Use this template as the output structure — fill in every section with real data from the codebase analysis:

@.codex/templates/CLAUDE-template.md

### What NOT to include
- Exhaustive API documentation (put in `.codex/docs/`)
- Framework-specific guides (put in `.codex/rules/`)
- Full dependency lists (that's what package.json is for)
- Anything that isn't needed every single session

## Phase 4: Create On-Demand Context (if patterns identified)

If the project has distinct subsystems, create on-demand rule files:

```
.codex/rules/
├── frontend.md      ← component patterns, styling conventions
├── api.md           ← endpoint patterns, middleware, auth
├── database.md      ← query patterns, migrations, schema
└── testing.md       ← mock patterns, fixtures, test utilities
```

Each file should have path-scoped frontmatter:
```yaml
---
paths:
  - "src/frontend/**"
  - "src/components/**"
---
```

## Output

Confirm:
1. `AGENTS.md` created with line count
2. Any `.codex/rules/` files created
3. Any `.codex/docs/` files created
4. What to review and customize

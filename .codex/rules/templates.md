---
paths:
  - "templates/**"
  - "crates/pice-daemon/src/templates/**"
---

# Template & Scaffolding Rules

## Ownership

- `templates/claude/` — files scaffolded by `pice init` into `.codex/`
- `templates/pice/` — files scaffolded by `pice init` into `.pice/`
- `crates/pice-daemon/src/templates/mod.rs` — `rust-embed` embedding + extraction logic
- The **daemon** owns template extraction (init handler). The CLI delegates via adapter.

## Template Drift (CRITICAL)

`templates/claude/` and the root `.codex/` can drift when methodology improvements (threshold changes, worktree awareness, workflow updates) are made to the root `.codex/` commands without syncing back to `templates/`.

**Rule: when you update a file in `.codex/commands/` or `.codex/templates/`, check if the same file exists in `templates/claude/` and sync the change.**

**Carve-out for project-materialized commands:** `.codex/commands/review.md` is a self-customizing command — its regression-suite table, source-file inventory, and baseline test counts are materialized to the PICE project (e.g., 811 Rust + 78 TS, specific Phase 4 daemon test files). Those project-specific blocks MUST NOT be synced into `templates/claude/commands/review.md`, which ships to new projects with placeholder text (`{test-runner}`, `{test-file-1}`, `{lint-command}`, etc.). When updating `review.md`, classify the change first:

- **Methodology change** (new phase added, output format changed, contract-evaluation flow updated, new validation rule) → sync to template (preserving placeholders).
- **Project state change** (new test file added to the regression suite, baseline count bumped, new source file inventoried) → root only.

When in doubt, look at the placeholder syntax in the template: anything inside `{curly-braces}` or `<!-- CUSTOMIZE: ... -->` blocks is meant to be customized per project. Never replace those with PICE-specific values upstream — that would make every new `pice init` ship with PICE's test inventory baked in.

Files that must stay in sync:

| Root | Template |
|------|----------|
| `.codex/commands/evaluate.md` | `templates/claude/commands/evaluate.md` |
| `.codex/commands/execute.md` | `templates/claude/commands/execute.md` |
| `.codex/commands/plan-feature.md` | `templates/claude/commands/plan-feature.md` |
| `.codex/commands/commit-and-deploy.md` | `templates/claude/commands/commit-and-deploy.md` |
| `.codex/commands/empty-redeploy.md` | `templates/claude/commands/empty-redeploy.md` |
| `.codex/commands/review.md` | `templates/claude/commands/review.md` |
| `.codex/commands/handoff.md` | `templates/claude/commands/handoff.md` |
| `.codex/commands/prime.md` | `templates/claude/commands/prime.md` |
| `.codex/templates/plan-template.md` | `templates/claude/templates/plan-template.md` |

Files that exist only in root (project-specific, not scaffolded):
- `.codex/PRD.md`, `.codex/rules/*.md`, `.codex/docs/*.md`, `.codex/plans/*.md`, `.codex/settings.local.json`, `.codex/skills/`

## Contract Templates (v0.2+)

`templates/pice/contracts/` contains 7 default layer-specific contract templates. These are extracted by `pice init --upgrade` and `pice layers detect --write` into `.pice/contracts/`. They are NOT synced with the root project — they are defaults for new projects.

| Template | Layer | Purpose |
|----------|-------|---------|
| `templates/pice/contracts/backend.toml` | backend | Code quality, error handling, tests, performance |
| `templates/pice/contracts/database.toml` | database | Migration safety, schema consistency, query correctness |
| `templates/pice/contracts/api.toml` | api | Input validation, auth, response format, error responses |
| `templates/pice/contracts/frontend.toml` | frontend | Routes, components, accessibility, error handling |
| `templates/pice/contracts/infrastructure.toml` | infrastructure | Env vars, secrets, IaC builds, security |
| `templates/pice/contracts/deployment.toml` | deployment | CI pipeline, env config, rollback, health checks |
| `templates/pice/contracts/observability.toml` | observability | Logging, alerts, tracing, metrics |

## Build-Time Embedding

- Templates are embedded via `rust-embed` at compile time. Changes to `templates/` require `cargo build` to take effect.
- The `rust-embed` derive is in `pice-daemon/src/templates/mod.rs`.
- Existing tests verify template files are embedded and extractable. They check file existence, not content.

## Per-Crate .codex/ Artifacts

Running `pice init` inside a crate subdirectory (e.g., during testing) creates a `.codex/` directory there. These are test artifacts, not tracked code:
- `crates/*/.codex/` is gitignored
- If you see per-crate `.codex/` directories, delete them — they are stale copies of the templates

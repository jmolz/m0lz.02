# Template Drift Inventory

Status: intentional namespace split for v0.7.0, with opt-in Codex public scaffold added after the original release gate.

`pice init` scaffolds user-facing command files under `.claude/` by default.
`pice init --developer codex` scaffolds `.codex/` plus root `AGENTS.md` for
Codex-primary users. This repo's root `.codex/` directory still contains
maintainer-local wrappers and plans, so it is not a byte-for-byte source for the
public Codex template. The trees are allowed to differ when a file is
repo-specific, maintainer-only, or references local release plans.

## Drift Policy

- General PICE methodology belongs in `templates/claude/**` and `templates/codex/**`.
- Repo-specific release plans, private command wrappers, and local evaluator
  instructions stay under `.codex/**`.
- Public docs must say that `.claude/` is the default scaffold emitted by `pice init`
  and `.codex/` is emitted by `pice init --developer codex`.
- `.codex/rules/templates.md` is the source of truth for this split.

## Audit Commands

```bash
diff -qr .codex/commands templates/claude/commands || true
diff -qr templates/claude templates/codex || true
diff -q .codex/templates/plan-template.md templates/claude/templates/plan-template.md || true
cargo test -p pice-daemon templates:: -- --nocapture
cargo test -p pice-daemon handlers::init -- --nocapture
```

## Intentional Differences Recorded For v0.7.0

The current audit reports these expected differences:

| Diff output | Reason | Action |
| --- | --- | --- |
| `.codex/commands/commit-and-deploy.md` differs from `templates/claude/commands/commit-and-deploy.md` | Repo-local Codex wrapper delegates to migrated `source-command-*` skills; scaffold command remains standalone user-facing guidance. | Keep split. |
| `.codex/commands/commit.md` differs from `templates/claude/commands/commit.md` | Same wrapper-vs-scaffold distinction. | Keep split. |
| `.codex/commands/create-prd.md` differs from `templates/claude/commands/create-prd.md` | Same wrapper-vs-scaffold distinction. | Keep split. |
| `.codex/commands/create-rules.md` differs from `templates/claude/commands/create-rules.md` | Same wrapper-vs-scaffold distinction. | Keep split. |
| `.codex/commands/empty-redeploy.md` differs from `templates/claude/commands/empty-redeploy.md` | Same wrapper-vs-scaffold distinction. | Keep split. |
| `.codex/commands/evaluate.md` differs from `templates/claude/commands/evaluate.md` | Repo command resolves worktree `AGENTS.md` fallback for Codex; public scaffolds are now available for both Claude and Codex developer surfaces. | Keep split. |
| `.codex/commands/execute.md` differs from `templates/claude/commands/execute.md` | Same wrapper-vs-scaffold distinction. | Keep split. |
| `.codex/commands/handoff.md` differs from `templates/claude/commands/handoff.md` | Same wrapper-vs-scaffold distinction. | Keep split. |
| `.codex/commands/plan-feature.md` differs from `templates/claude/commands/plan-feature.md` | Same wrapper-vs-scaffold distinction. | Keep split. |
| `.codex/commands/prime.md` differs from `templates/claude/commands/prime.md` | Same wrapper-vs-scaffold distinction. | Keep split. |
| `.codex/commands/review.md` differs from `templates/claude/commands/review.md` | Same wrapper-vs-scaffold distinction. | Keep split. |
| `.codex/commands/validate.md` differs from `templates/claude/commands/validate.md` | Same wrapper-vs-scaffold distinction. | Keep split. |
| `.codex/commands/self-heal.md` differs from `templates/codex/commands/self-heal.md` | Repo-local command can delegate to private source-command skills; public Codex scaffold must be standalone and describe post-merge use. | Keep split. |
| `.codex/templates/plan-template.md` differs from `templates/claude/templates/plan-template.md` | Repo-local template contains Codex/PICE release-planning context; scaffold template must stay general and avoid repo-specific inventories. | Keep split; sync only general methodology changes. |

Unexpected differences found by the audit commands should either be moved into
the public scaffold or recorded here before a release tag is approved.

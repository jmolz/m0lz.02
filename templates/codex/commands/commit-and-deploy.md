---
description: Standard workflow for building, testing, committing, pushing, and releasing PICE framework changes
---

# Commit and Deploy Workflow

## Authorization (read first)

**Invoking `/commit-and-deploy` IS the user's pre-authorization for the entire flow described below**, including:

- Auto-staging and committing every uncommitted change in the worktree
- Merging the active feature branch into `main`
- Pushing `main` (and any new tag) to `origin`
- Creating a GitHub Release
- Triggering the release CI pipeline (cross-platform binary builds + NPM publish for code releases)

**Do NOT pause for confirmation** at merge, push, tag, or release steps — those are pre-approved by the slash command itself. You SHOULD pause and surface the situation only when one of these red flags fires:

- **Validation failed** (any test, lint, format, or build failure) → fix the underlying issue or report the failure; never paper over.
- **Merge conflict requires destructive choice** (e.g., favoring main would discard feature commits, or vice versa) → present the conflict and the two resolutions.
- **Working tree contains files that look unrelated to the feature** (uncommitted edits to crates the feature shouldn't touch, untracked secrets-shaped files, etc.) → list them and ask whether to include or stash.
- **A force-push or tag rewrite would be needed** (e.g., the next-tag computation collides with an existing remote tag) → never force-push without confirmation.
- **The diff vs `main` is unusually large** (>10k LoC or >25 commits) AND no plan in `.codex/plans/` describes the scope → call it out and continue, but include the scope summary in the release notes so it is visible to the user post-deploy. **Continue, do not block.**

A merge into `main`, a push, a tag, and a release are part of the normal `/commit-and-deploy` flow. Friction at those steps defeats the purpose of the command.

---

## Pre-Commit Validation

Run these checks in order. Fix failures before proceeding.

### 1. Rust checks

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

### 2. TypeScript checks

```bash
pnpm lint
pnpm typecheck
pnpm test
```

### 3. Full builds

```bash
cargo build --release
pnpm build
```

### 4. Local CI parity gate

Before any `main` push or release tag from this command, run the Linux
CI-equivalent Docker preflight:

```bash
scripts/ci/local-linux.sh
```

This gate is mandatory for code, release, CI, command/template, validation, or
test-policy changes. Do not substitute host macOS `cargo`/`pnpm` results for
this check. If it fails, fix the underlying issue and rerun this Docker
preflight before pushing or tagging.

**Expected baseline:** {RUST_TEST_COUNT} Rust tests ({IGNORED_COUNT} ignored), {TS_TEST_COUNT} TypeScript tests, 0 lint errors, 0 warnings, clean release build. Update this line and the matching baseline in `AGENTS.md` whenever you ship a feature that adds tests.

### 5. README release-readiness review

Before committing, pushing, or tagging, review `README.md` against the actual
deployment diff and the fresh evidence from this run. This gate is mandatory
even when no README edit is expected.

Check at minimum:

- Install, quickstart, command examples, provider/runtime requirements, and
  self-heal or workflow guidance touched by the change
- Test counts, benchmark/performance claims, badges, release evidence,
  npm/GitHub/tag references, and CI/run IDs
- Screenshots, images, GIFs, diagrams, captions, and linked media
- Docker/Linux CI parity guidance and hosted Windows runner guidance for
  Windows CLI/runtime behavior

If any README claim is stale or incomplete, update `README.md` in the same
deployment before pushing or tagging. If no README edit is needed, record the
README review evidence in the final response and release notes. Run
`node scripts/acceptance/readme-media-audit.mjs` after README/media edits and
include the result in validation evidence. Never update README evidence from
memory; use current command output, GitHub Actions, npm, or release artifacts.

## Determine Context (Worktree or Main)

```bash
git branch --show-current
git worktree list
```

Determine if you're in a **worktree** (feature branch) or on **main**. The remaining phases adapt based on this.

## Commit by Feature (CRITICAL)

**Do NOT create one giant commit.** Group changes by feature/purpose.

```bash
# Review everything
git status

# Stage and commit by logical group — examples:
git add crates/pice-cli/src/engine/*.rs
git commit -m "feat(engine): add session capture support"

git add packages/provider-claude-code/src/*.ts
git commit -m "feat(provider): implement streaming notifications"

git add templates/
git commit -m "chore(templates): update init scaffolding"

# Docs last, separately
git add docs/ README.md CONTRIBUTING.md
git commit -m "docs: update architecture diagrams"

# Plans separately
git add .codex/plans/
git commit -m "docs(plans): add feature plan"
```

**Commit tags:** `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `perf`

**Include scope:** `feat(engine)`, `fix(provider)`, `refactor(protocol)`, `docs(readme)`

If any AI layer files changed (AGENTS.md, .codex/rules/, .codex/commands/), add a `Context:` section to the commit body.

## Merge to Main (Worktree Only)

**Skip this phase if already on main.**

If you committed on a feature branch in a worktree, merge it into main:

```bash
FEATURE_BRANCH=$(git branch --show-current)
WORKTREE_PATH=$(pwd)
MAIN_REPO=$(git worktree list | head -1 | awk '{print $1}')
cd "$MAIN_REPO"
git checkout main
git pull origin main
git merge "$FEATURE_BRANCH"
```

If the merge has conflicts:

1. Resolve conflicts — favor the feature branch for new code, preserve main for unrelated changes
2. Run the full validation suite again after resolving
3. Commit the merge resolution

## Push

```bash
# Push from the main repo directory (not the worktree)
git push origin main
```

CI runs automatically via GitHub Actions (`.github/workflows/ci.yml`).

After pushing `main`, verify the exact pushed SHA before creating or pushing a
release tag:

```bash
gh run list --commit "$(git rev-parse HEAD)" --limit 5
gh workflow run windows-smoke.yml --ref main
gh run watch <windows-smoke-run-id> --exit-status
```

Do not create or push the release tag until GitHub CI for `HEAD` and the hosted
Windows smoke workflow have passed. If either fails, fix the root cause, rerun
the local Docker preflight, push the fix to `main`, and rerun hosted Windows
smoke before tagging. This Windows runner gate is required for code, release,
CI, command/template, validation, or test-policy changes and for any follow-up
to a Windows CI failure.

## Release (REQUIRED for every push)

Every push to main gets a release. A `v*` tag is always a full release: it must
build artifacts, pass artifact smoke, publish the matching npm packages, and
only then create the GitHub Release. Do not create tag-only or GitHub-only
lightweight releases; the release workflow fails closed when the tag does not
match `npm/pice/package.json`.

### Determine release type and version (deterministic)

```bash
LAST_TAG=$(git describe --tags --abbrev=0)
echo "Last release: $LAST_TAG"
git diff --name-only $LAST_TAG..HEAD
```

Apply the following rules in order — first match wins:

| Condition (checked vs `$LAST_TAG..HEAD`) | Tier | Bump |
|---|---|---|
| Any commit message starts with `feat!`, `fix!`, contains `BREAKING CHANGE:`, or removes/renames a public API in `crates/pice-protocol`, `crates/pice-core`, or `packages/provider-protocol` | **major** | `vX.0.0` |
| Any new file under `crates/pice-core/src/`, `crates/pice-daemon/src/orchestrator/`, `crates/pice-daemon/src/handlers/`, `packages/`, OR any commit message starts with `feat(` | **minor** | `v0.X.0` |
| Any modification to `crates/`, `packages/`, `templates/`, `npm/`, `Cargo.toml`, `Cargo.lock`, `package.json`, `pnpm-lock.yaml` (without matching the rules above) | **patch (code)** | `v0.X.Y` (full release) |
| Only files under `docs/`, `README.md`, `CONTRIBUTING.md`, `.codex/`, `.github/`, or other non-code paths changed | **chore** | `v0.X.Y` (full release) |

The version-bump heuristic is mechanical. If the diff scope hits "minor", the next tag is the next minor — do NOT downgrade to patch because "the change feels small." Phase milestones (e.g., a new orchestration capability) are minor releases by definition.

### All changes → full release (triggers binary builds + NPM publish)

1. Update version in `Cargo.toml` (`workspace.package.version`), all `npm/*/package.json` files, and `packages/*/package.json` files. Confirm with `grep -r '"version"' npm/ packages/ Cargo.toml` after.
2. Run the release-policy tripwire: `pnpm exec vitest run scripts/acceptance/release-workflow-policy.test.mjs`.
3. Commit the version bump: `git commit -am "chore: bump version to $NEXT_TAG"`
4. Run the local CI parity gate, push `main`, and verify GitHub CI + hosted
   Windows smoke for the exact version-bump commit as described above.
5. Tag and push the tag only after those gates pass:

```bash
git tag $NEXT_TAG
git push origin $NEXT_TAG
```

This triggers `.github/workflows/release.yml` which builds cross-platform binaries, creates a GitHub Release with assets, and publishes to NPM.

6. Verify the release pipeline and confirm `Release / Publish to NPM` was not skipped:

```bash
gh run list --workflow=release.yml --limit 1
```

## Clean Up Worktree (Worktree Only)

**Skip this phase if you were already on main.**

After a successful merge and push, remove the worktree and feature branch:

```bash
git worktree remove "$WORKTREE_PATH"
git branch -d "$FEATURE_BRANCH"
```

Verify cleanup:

```bash
git worktree list
git branch
git status
```

If `git branch -d` refuses (branch not fully merged), investigate — do NOT force-delete with `-D` without understanding why.

## Verify

```bash
git log --oneline -5
git status
# Expected: on main, clean tree, feature commits visible in log

gh release list --limit 3
# Expected: new release shows as "Latest"

gh run list --limit 1
# Expected: CI passing
```

---
description: Standard workflow for building, testing, committing, pushing, and releasing PICE framework changes
---

# Commit and Deploy Workflow

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

**Expected baseline:** 168 Rust tests, 49 TypeScript tests, 0 lint errors, 0 warnings, clean release build.

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
git add .claude/plans/
git commit -m "docs(plans): add feature plan"
```

**Commit tags:** `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `perf`

**Include scope:** `feat(engine)`, `fix(provider)`, `refactor(protocol)`, `docs(readme)`

If any AI layer files changed (CLAUDE.md, .claude/rules/, .claude/commands/), add a `Context:` section to the commit body.

## Push

```bash
git push origin main
```

CI runs automatically via GitHub Actions (`.github/workflows/ci.yml`).

## Release (REQUIRED for every push)

Every push to main gets a GitHub Release. The type depends on what changed.

### Determine release type

Check what was changed since the last release:

```bash
LAST_TAG=$(git describe --tags --abbrev=0)
echo "Last release: $LAST_TAG"
git diff --name-only $LAST_TAG..HEAD
```

**Code change** = any file in `crates/`, `packages/`, `templates/`, `npm/`, `Cargo.toml`, `Cargo.lock`, `package.json`, `pnpm-lock.yaml` was modified.

**Docs/chore change** = only files in `docs/`, `README.md`, `CONTRIBUTING.md`, `.claude/`, `.github/`, or other non-code paths were modified.

### Bump version number

Increment the patch version from the last tag:

```bash
# Example: v0.1.6 → v0.1.7
NEXT_TAG="v0.1.7"  # Adjust based on last tag
```

For code changes that add features, bump minor instead (`v0.2.0`). For breaking changes, bump major.

### Code changes → full release (triggers binary builds + NPM publish)

1. Update version in `Cargo.toml` (`workspace.package.version`), all `npm/*/package.json` files, and `packages/*/package.json` files
2. Commit the version bump: `git commit -am "chore: bump version to $NEXT_TAG"`
3. Tag and push:

```bash
git tag $NEXT_TAG
git push origin main --tags
```

This triggers `.github/workflows/release.yml` which builds cross-platform binaries, creates a GitHub Release with assets, and publishes to NPM.

4. Verify the release pipeline:

```bash
gh run list --workflow=release.yml --limit 1
```

### Docs/chore changes → lightweight release (no binaries)

No version bump needed. Create a lightweight GitHub Release directly:

```bash
NEXT_TAG="v0.1.7"  # Next patch after last tag
git tag $NEXT_TAG
git push origin $NEXT_TAG
gh release create $NEXT_TAG \
  --title "$NEXT_TAG — <short description>" \
  --notes "$(cat <<'EOF'
Documentation-only release (no binary changes).

## Changes
- <list changes>

**Full Changelog**: https://github.com/jmolz/pice-framework/compare/$LAST_TAG...$NEXT_TAG
EOF
)"
```

## Verify

```bash
git status
# Expected: "nothing to commit, working tree clean"

gh release list --limit 3
# Expected: new release shows as "Latest"

gh run list --limit 1
# Expected: CI passing
```

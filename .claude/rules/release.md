# Release Rules

These rules apply when changing `.github/workflows/release.yml`, version
manifests, `npm/**`, `packages/**`, release notes, or release command guidance.

## NPM publish is a release gate

Every `v*` tag release must publish the matching npm packages after artifact
smoke and before GitHub Release creation. The `release` job must depend on the
`npm-publish` job. Manual `workflow_dispatch` dry-runs may skip publishing, but
manual publishing must require `dry_run=false` and `publish_npm=true`.

Forensic anchor: on 2026-05-13, the `Release / Publish to NPM (push)` job was
skipped because tag pushes were not part of the job condition. The tripwire is
`scripts/acceptance/release-workflow-policy.test.mjs`.

## Version and tag alignment

Do not create a `v*` tag unless the tag name matches
`v$(npm/pice/package.json.version)`, and keep `Cargo.toml`, `npm/*/package.json`,
and `packages/*/package.json` on the same version. A tag/package mismatch must
fail closed before npm publish.

## Release validation

Before a release tag or manual npm publish:

- run the workflow-policy tripwire with
  `pnpm exec vitest run scripts/acceptance/release-workflow-policy.test.mjs`;
- keep the tripwire in `pnpm test` so CI catches future workflow drift;
- verify npm publication after the release workflow with `npm view` for the main
  package and every platform package.

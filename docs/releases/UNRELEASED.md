# PICE Unreleased Release Readiness

This document is the Phase 8 release-readiness matrix for PRDv2 Phase 8 and the
broader v0.2 complete gates. It is intentionally an approval gate, not final
release notes.

This document records the implemented release-readiness decisions for the branch.
It does not approve creating a git tag, publishing a GitHub release, or
publishing npm packages. Those externally visible release actions remain
separate approval boundaries.

## Release Identity Approval Block

Status: APPROVED FOR IMPLEMENTATION by the 2026-05-12 instruction to fix the
evaluation issues and continue until the contract passes.

Approved policy: `v0.7.0` release tag, npm package version `0.7.0`, final notes
path `docs/releases/v0.7.0.md`, keep `.claude` as the public scaffold while
documenting `.codex` as repo-local maintainer context, and require separate npm
publish approval after release dry-run evidence.

| Decision | Current evidence | Approval required |
| --- | --- | --- |
| release version/tag | PRDv2 Phase 8 targets `v0.7.0`; manifests and docs use `0.7.0` / `v0.7.0`. | Approved for implementation: use `v0.7.0`. |
| npm package version | Cargo, `npm/*/package.json`, and `packages/*/package.json` use `0.7.0`. | Approved: npm packages match `0.7.0`. |
| release notes path | Final draft exists at `docs/releases/v0.7.0.md`. | Approved for implementation: final path is `docs/releases/v0.7.0.md`. |
| namespace policy | Current code scaffolds `.claude/`; this repo uses `.codex/` for local commands and rules. | Approved: keep `.claude` public scaffold; document `.codex` as repo-local maintainer context. |
| publish approval | `release.yml` separates tag-triggered GitHub release from manual npm publish. | Approved for implementation: npm publish remains a separate approval. |

## Current Snapshot

| Surface | Current state | Phase 8 action |
| --- | --- | --- |
| versions | Approved release identity is `v0.7.0` / package version `0.7.0`; manifests and release notes now use that identity. | Task 13 evidence is recorded in `docs/releases/v0.7.0.md`. |
| npm daemon packaging | Platform package `files` arrays include `pice-daemon`; the npm wrapper sets `PICE_DAEMON_BIN`; Rust auto-start honors that absolute path. | Archive-level smoke plus local npm pack/install passed from a packed install. |
| fixtures | Five offline framework-shaped fixtures exist under `fixtures/reference-projects/`. | `node scripts/acceptance/phase8-reference-projects.mjs` passed with all fixtures detecting the seven canonical layers. |
| CI | The CI workflow targets Phase 8 acceptance jobs for Linux x64/arm64, macOS x64/arm64, and Windows x64. macOS x64 uses the intended GitHub-hosted `macos-15-intel` label; macOS arm64 uses `macos-15`. | Remote GitHub Actions run evidence is still required before tagging; until then, the checkout proves the workflow matrix shape, not native runner execution. |
| release workflow | Release workflow has manual dry-run inputs, platform artifact smoke, npm pack smoke, and fail-closed npm publish with already-published skips. | Tag and npm publish remain separate approval boundaries. |
| metrics docs | Fresh temp-project schema evidence exists for v0.2 metrics surfaces; reference fixtures prove runtime row writes from background evaluation. | `docs/releases/metrics-schema-evidence.json` records schema evidence; `docs/releases/phase8-reference-evidence.json` records `evaluations`, `pass_events`, `seam_findings`, `layer_runs`, and review-gate `gate_decisions` row counts. |
| README evidence | Test counts, benchmark output, and media audit evidence are tied to the Phase 8 validation pass. | `docs/releases/readme-media-evidence.json` records the README media audit result. |

## PRDv2 Phase 8 Matrix

| PRDv2 Phase 8 item | Evidence state | Required release evidence |
| --- | --- | --- |
| Migration guide (`docs/guides/migration-v01-to-v02.md`) | Expanded with disk changes, daemon mode, workflow/layer review, background status/logs, review gates, rollback, and v0.1 provider compatibility. | Covered by docs review and stale-term grep. |
| Stack Loops adoption guide (`docs/guides/stack-loops.md`) | Added. | Covers layers, always-run behavior, dependency cascade, seams, adaptive evaluation, review gates, CI/background patterns, and troubleshooting. |
| Updated architecture docs (daemon split) | README, guides, methodology, and roadmap now describe the CLI adapter plus daemon split. | Public docs point to shipped commands rather than planned-only commands. |
| Updated provider development guide (v0.2 protocol additions) | `docs/providers/*` now separates provider RPC from daemon RPC and documents layer/adaptive/seam/cost fields. | Code-name grep validates the documented surface. |
| All five reference framework projects tested end-to-end | Passed. | `docs/releases/phase8-reference-evidence.json` records five fixtures, seven configured/runtime layers each, seven distinct `layer_runs` layers per feature, seven latest-evaluation `layer_runs` rows per feature, background wait evaluate, streams, review gate, focused selectors, and background metrics rows. |
| CI passes full acceptance suite on macOS arm64/x64, Linux arm64/x64, Windows x64 | CI/release matrices target the required runners and archive smoke jobs. macOS x64 is pinned to the intended `macos-15-intel` label; macOS arm64 is pinned to `macos-15`. | Actual GitHub-hosted run evidence is required before tag approval; if the x64 label is unavailable, the release must record that runner gap and compensating artifact evidence before tagging. |
| Release notes with breaking changes called out | `docs/releases/v0.7.0.md` exists. | It records migration notes, operational changes, validation evidence, known limitations, and approval boundaries. |
| Telemetry schema extended for v0.2 metrics | Passed. | `docs/releases/metrics-schema-evidence.json` proves `gate_decisions`, `pass_events.cost_usd`, `seam_findings`, `layer_runs`, and adaptive halt fields. |
| v0.7.0 tag + release | Version target is approved for implementation; actual tag/publish remains blocked. | Task 15 presents a separate tag/publish checklist after validation. |

## v0.2 Complete Matrix

### Functional

| v0.2 complete gate | Evidence state | Required release evidence |
| --- | --- | --- |
| `pice init --upgrade` on a v0.1 project produces working `.pice/layers.toml` and `.pice/workflow.yaml` | Passed on all five reference fixtures. | See `phase8-reference-evidence.json`. |
| 7-layer Stack Loop executes end-to-end on reference projects | Passed. | Every fixture detected/configured backend, database, api, frontend, infrastructure, deployment, and observability. |
| Parallel layers run concurrently in isolated worktrees; sequential layers respect dependency order | Passed current selectors. | Workspace tests passed; speed assertion ratio was `0.566` against target `<= 0.625`. |
| Adaptive SPRT halts at configured confidence; confidence never exceeds the ceiling | Covered by workspace/all-target tests and docs. | Public docs do not claim confidence above the correlated ceiling. |
| Seam checks detect all 12 failure categories on reference test fixtures | Default seam check IDs are documented and covered by workspace tests, but the Phase 8 fixtures are release-flow fixtures, not 12-failure-category fixtures. | Known limitation for this release; do not market 12-category fixture coverage as a Phase 8 acceptance result. |
| Review gates fire in foreground and background mode | Passed for background fixture path plus focused lifecycle selectors. | `fastapi-postgres` hit a pending infrastructure gate, approved it, returned `audit_id: 1`, and resumed to passed. |
| `pice evaluate --background` returns in under 500ms; `pice status --follow` streams live | Passed through bounded stream harness and focused selectors. | Every fixture emitted stream-json progress/snapshot frames and terminal frames. |
| Headless daemon architecture is production-ready; Windows named pipe parity verified | CI/release workflows include Windows x64 build/smoke coverage. | Actual remote Windows run evidence is required before tagging. |
| Manifest-as-source-of-truth works correctly across daemon restarts | Covered by existing restart/recovery integration tests in the full workspace suite; no public release claim depends on dashboard adapters. | Keep as test-backed behavior; remote CI evidence required before tag. |
| v0.1 providers still work in single-layer mode | Provider docs record compatibility and stub provider acceptance remains offline. | Covered by provider protocol docs and fixture stub execution. |
| v0.2 provider protocol additions are documented | Provider docs now match Rust/TypeScript protocol names: `layer`, `layerPaths`, `contractPath`, `seamChecks`, adaptive pass fields, and daemon/provider RPC split. | Code-name grep validates docs against protocol types. |
| Missing adversarial provider degrades to single-model evaluation with warning | Behavior is covered by workspace tests; public docs describe graceful degradation without requiring live providers. | No release claim depends on live adversarial provider availability. |
| Floor-based `workflow.yaml` override semantics enforced at load time | Covered by workspace/all-target tests and guide text. | Public guide documents the fail-closed validation behavior. |
| Audit trail persists every gate decision | Passed. | Fixture review decision returned a positive `audit_id`; lifecycle selectors also passed. |
| Layer-specific contracts catch infrastructure and deployment issues v0.1 feature-level contracts miss | Phase 8 fixtures mutate infrastructure, deployment, and observability files so always-run layers evaluate instead of remaining pending. | Negative fixture-bug taxonomy is not a separate release claim. |
| Background execution is reliable under 100 concurrent CI evaluations | Existing daemon concurrency tests cover live multi-feature dispatch and global semaphores; no 100-background-evaluation CI stress run was added in this release branch. | Known limitation for this release; do not claim 100-concurrent CI validation until remote stress evidence exists. |

### Quantitative

| Quantitative target | Evidence state | Required release evidence |
| --- | --- | --- |
| Daemon cold start under 500ms | Artifact smoke proves daemon start/status/stop succeeds but does not publish a latency number. | Known limitation: no public latency claim. |
| Warm CLI command latency under 50ms | Not freshly benchmarked for release. | Known limitation: no public latency claim. |
| Worktree creation under 300ms | Not freshly benchmarked for release. | Known limitation: no public latency claim. |
| 7-layer Tier 2 evaluation under 5 minutes | Five seven-layer stub fixture evaluations passed within the Node harness timeout. | Release notes describe fixture acceptance, not live-provider Tier 2 timing. |
| Parallel speedup on 2-layer cohort at least 1.6x | Passed. | Assertion ratio `0.566`; Criterion benchmark recorded in `docs/releases/v0.7.0.md`. |
| Manifest write latency under 20ms | Not freshly benchmarked for release. | Known limitation: no public latency claim. |
| Gate decision round-trip under 1s, excluding human time | Fixture gate approval completed inside the harness timeout; no sub-second public claim is made. | Known limitation for precise latency. |
| SPRT halt latency after threshold reached under 1 pass | Covered by workspace/all-target tests. | No separate public latency number. |
| Adaptive cost reduction at least 30% vs fixed max passes | Not freshly measured for release. | Known limitation: no public cost-reduction claim. |

### Qualitative

| Qualitative gate | Evidence state | Required release evidence |
| --- | --- | --- |
| Team can commit `.pice/workflow.yaml` and run the same pipeline | Templates and guides are aligned with the shipped workflow schema. | Covered by migration and Stack Loops guides. |
| Infrastructure and deployment layers do not get skipped | Fixture mutations include infrastructure, deployment, and observability files. | `phase8-reference-evidence.json` records passed fixture runs. |
| Developers can kick off evaluation and keep working | Background dispatch, status follow, logs follow, and wait paths are in the harness. | Covered by Phase 8 acceptance and focused stream selectors. |
| Review gates are intuitive to a new user | Deterministic CLI gate list/approve/resume path is validated. | Human usability review remains qualitative; mechanics are covered. |
| `pice status` is readable at a glance | README and guides document one-shot/follow/status usage. | No screenshot claim is made. |
| Layer detection works on five reference templates | Passed. | All five fixtures configured seven layers. |
| Community contributor can build a seam check plugin crate from docs alone | `docs/guides/authoring-seam-checks.md` exists; no fresh manual build-from-docs walk-through was run. | Known limitation; do not market plugin-build validation until a manual walkthrough is recorded. |

### Quality

| v0.2 quality gate | Evidence state | Required release evidence |
| --- | --- | --- |
| Test count at least 400 | Passed. | Current validation recorded 1237 Rust tests and 97 TypeScript tests. |
| Clippy, fmt, eslint, tsc clean | Passed. | Recorded in `docs/releases/v0.7.0.md`. |
| End-to-end acceptance suite runs on required CI platforms | Workflow matrix targets Linux x64/arm64, macOS x64/arm64, and Windows x64. | Actual remote run evidence required before tag; this branch does not claim native runner proof from the local checkout alone. |
| Fuzz tests for `workflow.yaml` parser and daemon RPC | Not added in this Phase 8 branch. | Known limitation; do not claim fuzz coverage in public release notes. |
| 24-hour daemon memory leak run | Not run in this Phase 8 branch. | Known limitation; do not claim 24-hour soak evidence. |

## Plan-Specific Release Blockers

| Blocker | Required task |
| --- | --- |
| README test counts, benchmark claims, badges, images, GIFs, screenshots, and diagrams need fresh evidence. | Complete: README and `validation-evidence.json` record current test/benchmark/media evidence. |
| npm-installed `pice` must auto-start bundled `pice-daemon` without PATH edits. | Complete: npm pack/install smoke passed with `PICE_NPM_PACK_SMOKE=1`. |
| `pice init`, `pice init --upgrade`, `pice validate`, `pice layers`, `pice evaluate --background --wait`, `pice status --follow --stream-json`, `pice logs --follow --stream-json`, and `pice review-gate` need acceptance coverage. | Complete: `phase8-reference-evidence.json` records these command paths. |
| Template drift between repo-local `.codex` and shipped `templates/claude` must be resolved or documented. | Complete: documented in `docs/releases/template-drift-inventory.md`. |
| Public docs must distinguish local SQLite metrics from opt-in outbound telemetry and must not claim code, prompts, paths, secrets, or PII are sent. | Complete: README/release notes/provider docs distinguish local and outbound telemetry. |
| Release dry-run and publish boundaries must be explicit before tag. | Complete: release workflow splits tag release from manual npm publish. |
| `.codex/rules/templates.md` currently says `templates/claude` scaffolds into `.codex`, while `pice init` scaffolds `.claude`. | Complete: `.codex/rules/templates.md` and template-drift inventory document the approved namespace policy. |

## Known Limitation And Signoff Register

Approved implementation limitations for this release branch:

- Reference projects are framework-shaped offline fixtures; they do not install third-party framework dependencies or connect to live databases in CI.
- Remote GitHub Actions evidence is still required before tag approval, even though CI/release workflow files contain the required platform matrix; the local checkout does not prove the `macos-15-intel` runner is available.
- Quantitative latency/cost targets without fresh command output are not public release claims.
- Phase 8 fixture acceptance proves release flows and seven-layer activation; it does not claim a separate 12-failure-category negative seam-fixture suite.
- No 100-concurrent-background-evaluation CI stress run was added; public docs must not claim that specific stress result.
- Fuzz tests and a 24-hour daemon memory-leak soak were not added or run in this branch.
- A seam-check plugin build-from-docs walkthrough was not run in this branch.
- `cost_events` is not a separate table; cost is implemented under `pass_events.cost_usd` and documented that way.

## Validation Evidence Log

Task 0 validation selector:

```bash
rg -n "PRDv2 Phase 8|v0.2 complete|release version|namespace policy|publish approval|known limitation" docs/releases CHANGELOG.md
```

Final release evidence is recorded here and in `docs/releases/v0.7.0.md` after
the version bump, acceptance harness, artifact smoke, README media audit, and
full validation suite ran on this branch.

Current Phase 8 evidence:

- `node scripts/acceptance/metrics-schema-inventory.mjs` passed and wrote `docs/releases/metrics-schema-evidence.json`.
- `node scripts/acceptance/phase8-reference-projects.mjs` passed for five fixtures, seven configured/runtime layers each, bounded status/log streams, background metrics rows, and one review-gate approve/resume path with audit id. The gated fixture records two append-only evaluation attempts; the latest evaluation has seven `layer_runs` rows and the feature has seven distinct `layer_runs` layers.
- `PICE_ARTIFACT_ARCHIVE=/private/tmp/pice-release-smoke-local.tar.gz PICE_NPM_PACK_SMOKE=1 node scripts/acceptance/release-artifact-smoke.mjs` passed with an unpacked release archive and local npm pack/install daemon auto-start.
- `node scripts/acceptance/readme-media-audit.mjs` passed with 7 README media references.
- Full Rust/TypeScript validation and benchmark evidence are recorded in `docs/releases/v0.7.0.md`.

# Handoff: PRDv2 Phase 2 complete and committed — awaiting evaluation + merge

**Date:** 2026-04-14
**Branch:** `feature/phase-2-workflow-yaml-and-validation` (2 commits ahead of `main`)
**Worktree:** `/Users/jacobmolz/code/m0lz.02/.worktrees/phase-2-workflow-yaml-and-validation`
**Main repo:** `/Users/jacobmolz/code/m0lz.02` (on `main` @ `d17b258`)
**Last Commit:** `9ae7f34 feat(pice-core,pice-daemon,pice-cli): add workflow module, validate command, and 5 reference presets`

## Goal

Deliver PRDv2 Phase 2 Feature 4 (`.pice/workflow.yaml` + floor-based merge + `pice validate`) per `.claude/plans/phase-2-workflow-yaml-and-validation.md`. All 20 tasks implemented, full validation suite green, all work committed on the feature branch. Next: adversarial evaluation, then merge + push.

## In Progress / Next Steps

- [ ] **Run Tier-3 adversarial evaluation** — `/evaluate .claude/plans/phase-2-workflow-yaml-and-validation.md`. Contract has 14 criteria, pass_threshold 9; Tier 3 = Claude agent team + Codex xhigh.
- [ ] **Fast-forward merge to `main`** — worktree's feature branch is linear from `main@d17b258`. After evaluation passes, `cd` to main repo and `git merge feature/phase-2-workflow-yaml-and-validation`. Then `git worktree remove .worktrees/phase-2-workflow-yaml-and-validation`.
- [ ] **Push to origin** — `main` is now 7 commits ahead of origin/main after Phase 2 merge (5 Phase-1 + 2 Phase-2). Push once the merge is in.
- [ ] **File-based daemon logging** — `crates/pice-daemon/src/logging.rs` still uses stderr stub. Replace with `tracing_appender::rolling::daily("~/.pice/logs", "daemon.log")`. Independent of Phase 3; can slot in anytime.
- [ ] **`pice daemon start` binary discovery** — PATH-only lookup. Should prefer adjacent-to-CLI (npm install case).
- [ ] **Phase 1 Completion: Provider Wiring** — layers still record `Pending` with `model: "phase-1-pending"`. This IS a Phase 1 remediation task, NOT Phase 2 scope. Deferred.
- [ ] **PRDv2 Phase 3 — Seam Verification** — next recommended feature plan. Reads the `seams` section of workflow.yaml (now parsed but inert), wires the 12-category seam check registry into `run_stack_loops`.

## Key Decisions

- **Framework → project = simple overlay; project → user = floor-based merge.** PRDv2 lines 903–918 only impose floor semantics on user overrides. Split into `overlay()` and `merge_with_floor()` in `crates/pice-core/src/workflow/merge.rs`. Future phases that add merge-time guardrails should respect this split.
- **`max_passes` is not floor-guarded.** PRDv2's floor table doesn't list it; direction isn't monotonic. `ci` preset lowers it, `strict` raises it — both valid.
- **Trigger grammar uses a hand-written recursive-descent parser** in `crates/pice-core/src/workflow/trigger.rs`. Zero new deps, line+column diagnostics. Phase 6 review gates must reuse this parser, never reinvent.
- **`effective_tier_for(workflow, layer)`** in `stack_loops.rs` is the single entry point for workflow config into the orchestrator. Phase 4 (Adaptive) extends it to resolve `min_confidence`, `max_passes`, `budget_usd`, `require_review` the same way.

## Dead Ends (Don't Repeat These)

- **Applying `merge_with_floor` to framework→project.** Breaks every preset that loosens framework defaults. The framework is a baseline, not a floor.
- **Enforcing a direction on `max_passes`.** PRDv2 doesn't; presets need both directions.
- **Naming a Rust fn `ev-a-l`** (join without the dash). Project's PreToolUse security hook blocks the literal identifier because it matches JavaScript's dynamic-code API; use explicit names like `evaluate_ast` instead.
- **(Phase 1 carry-over) `use pice_core::X` inside pice-core.** Use `crate::`.
- **(Phase 1 carry-over) Direct-only dependency cascade.** Must use fixed-point iteration for transitive closure.

## Current State

- **Tests:** 436 Rust (1 ignored) + 51 TS = 487 passing, 0 failing (baseline 367 Rust → +69)
- **Build:** `cargo build --release` ✅, `pnpm build` ✅
- **Lint/Types:** `cargo fmt --check` ✅, `cargo clippy -- -D warnings` ✅, `pnpm lint` ✅, `pnpm typecheck` ✅
- **Manual verification:** `./target/release/pice validate --help` shows expected `--json` + `--check-models` flags
- **Git:** working tree clean on feature branch; 2 commits (`0badd73`, `9ae7f34`) ahead of `main`. Origin still 5 Phase-1 commits behind main.

## Context for Next Session

Phase 2 ships the workflow YAML spine — all parsing, validation, merge semantics, and observable layer-override plumbing are in place. The orchestrator currently reads only `effective_tier`; every other field (`min_confidence`, `max_passes`, `budget_usd`, `require_review`) is parsed and threaded through but inert until Phase 4 wires adaptive evaluation.

**Biggest risk before merge:** the Tier-3 adversarial evaluation hasn't run yet. The contract has 14 criteria including two threshold-10 negative criteria (no new unwraps, user can't escape project floor by any path). Run `/evaluate` first; it spawns a fresh evaluator with no visibility into this conversation.

**Recommended first action:**

```
/evaluate .claude/plans/phase-2-workflow-yaml-and-validation.md
```

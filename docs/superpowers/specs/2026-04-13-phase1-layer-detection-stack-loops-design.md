# Design: PRDv2 Phase 1 — Layer Detection + Stack Loops Core

> **Date:** 2026-04-13
> **Status:** Approved
> **PRDv2 Reference:** Lines 2029–2044 (Phase 1 checklist), Lines 747–836 (Feature 3: Layer Detection), Lines 601+ (Core Features)
> **Approach:** Bottom-up with early orchestrator skeleton (Approach C)

---

## Problem

PICE v0.1 evaluates features as monolithic units — one contract, one diff, one evaluation loop. This misses the 68% of production outages that trigger at integration points between components (Google SRE, Adyen, ICSE data). A feature can "pass" v0.1 evaluation while having broken infrastructure, missing env vars, or schema drift between layers.

Phase 1 introduces layer-aware evaluation: detect a project's architectural layers, run per-layer PICE loops with context-isolated evaluators, and verify that every layer passes before a feature is considered done.

## User Stories

1. As a developer running `pice evaluate`, I want PICE to evaluate each layer (backend, database, API, frontend, infrastructure, deployment, observability) independently with its own contract, so cross-layer blind spots are caught.
2. As a developer adopting v0.2, I want `pice init --upgrade` to generate a proposed `.pice/layers.toml` from layer detection, which I review and commit.
3. As a developer with a Next.js app, I want PICE to correctly tag `pages/api/users.ts` as belonging to the API, frontend, AND database layers simultaneously.
4. As a developer with a CSS-only change, I want `pice evaluate` to skip the database layer but NOT skip infrastructure (always-run).

## Scope

### In Scope (Phase 1)

- Six-level layer detection engine (manifest → directory → framework → config → imports-stub → override)
- `.pice/layers.toml` parser + validator with full schema (including `[seams]` section, parsed but not executed until Phase 3)
- Monorepo handling: `[stacks.{service}]` sections, `nx.json`/`turbo.json`/`pnpm-workspace.yaml` integration
- `pice layers {detect|list|check|graph}` command suite
- File-level layer tagging (files belong to multiple layers via glob overlap)
- DAG construction + topological cohort identification
- Stack Loops orchestrator (sequential execution only, no parallelism)
- Layer-specific contract templates (7 default `.pice/contracts/*.toml` files)
- Layer-scoped context isolation enforcement (test-driven)
- `pice init --upgrade` generates proposed `layers.toml` (workflow.yaml deferred to Phase 2)
- Diff filtering by layer glob patterns
- Per-layer manifest entries in verification manifest
- Provider protocol extensions (optional `layer`, `layerPaths`, `contractPath` fields on session/create)
- Migration guide for v0.1 → v0.2

### Out of Scope (Later Phases)

- Parallel cohort execution / worktree isolation (Phase 5)
- Adaptive pass allocation — SPRT, ADTS, VEC (Phase 4)
- Seam check execution (Phase 3 — schema parsed in Phase 1 but checks don't run)
- Review gates (Phase 6)
- Background execution (Phase 7)
- `workflow.yaml` schema + parser (Phase 2)
- Import graph analysis (Level 5 — stubbed in Phase 1, returns empty)
- Cross-repo / polyrepo seam checks (v0.4)

---

## Architecture

### Implementation Sequence (Approach C)

1. **Types + schemas** in `pice-core` — `LayersConfig`, `DetectedLayer`, `LayerDag`
2. **Detection engine** in `pice-core` — six-level heuristic stack, framework presets, monorepo detection
3. **Orchestrator skeleton** in `pice-daemon` — single-layer sequential path, context isolation, diff filtering
4. **`pice layers` commands** — detect, list, check, graph handlers + CLI args
5. **`pice init --upgrade`** — extend init handler
6. **Contract templates** — 7 embedded `.toml` files
7. **Context isolation test harness** — integration tests proving cross-layer leakage is impossible
8. **Migration guide** — `docs/guides/migration-v01-to-v02.md`

### Crate Boundaries

| Component | Crate | Rationale |
|-----------|-------|-----------|
| `LayersConfig`, `DetectedLayer`, `LayerDag`, detection engine, diff filtering, layer activation, DAG construction | `pice-core` | Pure logic, no async, no network. Both CLI and daemon depend on it. |
| Stack Loops orchestrator, `layers` command handler, init upgrade handler, contract templates | `pice-daemon` | Owns orchestration, provider sessions, template embedding. |
| `pice layers` subcommand args, CLI adapter dispatch | `pice-cli` | Thin adapter, arg parsing only. |
| `session/create` layer fields | `pice-protocol` + `@pice/provider-protocol` | Both sides of the JSON-RPC contract. |

---

## Data Model (`pice-core::layers`)

### `LayersConfig` — `.pice/layers.toml` Schema

```rust
pub struct LayersConfig {
    pub layers: LayerMap,  // BTreeMap<String, LayerDef>, order from `order` field
    pub order: Vec<String>,  // authoritative layer sequence
    pub seams: BTreeMap<String, Vec<String>>,  // "backend↔database" → check IDs (parsed, not executed until Phase 3)
    pub external_contracts: Option<BTreeMap<String, ExternalContract>>,  // v0.4, parse but ignore
    pub stacks: Option<BTreeMap<String, StackDef>>,  // monorepo multi-stack support
}

pub struct LayerDef {
    pub paths: Vec<String>,       // glob patterns
    pub always_run: bool,         // default false; true for infra/deploy/observability
    pub contract: Option<String>, // path to .pice/contracts/{layer}.toml
    pub depends_on: Vec<String>,  // DAG edges
    pub layer_type: Option<LayerType>,  // None or "meta" for IaC
    pub environment_variants: Option<Vec<String>>,  // deployment layer only
}

pub enum LayerType {
    Meta,  // IaC layers (Terraform/Pulumi/CDK) — provisioning-seam verification
}

pub struct StackDef {
    pub root: String,             // relative path to service root
    pub layers: Option<LayersConfig>,  // per-service layer overrides
}
```

**TOML format** follows PRDv2 lines 778–827 exactly. Validation rules:
- `depends_on` entries must reference layers in `order`
- Dependencies must form a DAG (no cycles) — detected by topological sort
- Overlapping `paths` globs are allowed (file-level multi-layer tagging)
- `contract` paths are relative to project root

### `DetectedLayers` — Detection Engine Output

```rust
pub struct DetectedLayers {
    pub layers: Vec<DetectedLayer>,
    pub stacks: Option<Vec<DetectedStack>>,  // monorepo multi-stack
}

pub struct DetectedLayer {
    pub name: String,
    pub paths: Vec<String>,
    pub detected_by: Vec<DetectionLevel>,  // which heuristic levels contributed
    pub always_run: bool,                   // inferred by layer category name
    pub depends_on: Vec<String>,            // from framework presets only, empty for non-preset
}

pub enum DetectionLevel {
    Manifest,
    Directory,
    Framework,
    Config,
    ImportGraph,  // stubbed in Phase 1
    Override,
}

pub struct DetectedStack {
    pub name: String,
    pub root: String,
    pub layers: Vec<DetectedLayer>,
    pub detected_by: MonorepoTool,  // Nx, Turborepo, PnpmWorkspace, DirectoryConvention
}
```

### `LayerDag` — Topological Ordering

```rust
pub struct LayerDag {
    pub cohorts: Vec<Vec<String>>,  // groups that can run in parallel (sequential in Phase 1)
    pub edges: Vec<(String, String)>,  // (dependency, dependent)
}
```

Constructed from `LayersConfig` by topological sort. Cycles → error with cycle path. Cohorts are groups with no pending upstream dependencies.

---

## Detection Engine (`pice-core::layers::detect`)

Pure function: `detect_layers(project_root: &Path) -> Result<DetectedLayers>`

### Six Levels

**Level 1 — Manifest files:** Scan for `package.json`, `Cargo.toml`, `pyproject.toml`, `go.mod`, `Gemfile`. Extract dependency names to identify frameworks and runtimes.

**Level 2 — Directory patterns:** Match against static lookup table. `src/server/`, `api/` → backend/API. `terraform/`, `infra/` → infrastructure. `deploy/`, `helm/` → deployment. `.github/workflows/` → deployment. `pages/`, `app/`, `src/client/` → frontend. Level 2 proposes *candidate* layers.

**Level 3 — Framework signals:** Uses Level 1 dependencies + Level 2 directories to apply framework-specific rules. Level 3 actively *promotes or reclassifies* Level 2 candidates. Next.js `app/` → frontend + API. Prisma `schema.prisma` → database. Rails `app/controllers/` → API + backend.

**Level 4 — Config files:** `Dockerfile`, `docker-compose.yml` → deployment. `vercel.json`, `netlify.toml` → deployment. `monitoring.yml`, `datadog.yaml` → observability.

**Level 5 — Import graph:** **Stubbed in Phase 1.** Returns empty results. Real import graph analysis (language-specific parsers) deferred. Detection produces useful results from Levels 1–4 and 6 without this.

**Level 6 — Override file:** If `.pice/layers.toml` exists, load it. **Detection is skipped entirely** (PRDv2 line 756). The override file IS the layer configuration. `pice layers check` can run detection on demand for comparison.

### Framework Presets

Static rules for known frameworks, providing sensible `depends_on` defaults and `always_run` flags:

- `NextJs` — frontend, api, database (if Prisma), infrastructure (if Terraform/Pulumi)
- `Remix` — frontend, api, backend
- `SvelteKit` — frontend, api
- `FastAPI` — api, backend, database
- `Rails` — api, backend, database
- `Express` — api, backend
- `RustCli` — backend (single layer, degrades to v0.1 behavior)

Non-preset projects get empty `depends_on` — the user edits `layers.toml` to add dependencies.

### `always_run` Inference

Inferred by layer category name convention, not detection level:
- Layer name contains "infrastructure" → `always_run = true`
- Layer name contains "deployment" → `always_run = true`
- Layer name contains "observability" → `always_run = true`
- All other layers → `always_run = false`

### Monorepo Detection

If `nx.json`, `turbo.json`, or `pnpm-workspace.yaml` detected:
1. Parse the monorepo tool's project graph to identify services
2. Each service becomes a `DetectedStack` with its own layer set
3. Output includes `stacks` field with per-service layers
4. Generated `layers.toml` uses `[stacks.{service}]` sections

If none of those files exist but multiple independent `package.json`/`Cargo.toml` files are found in subdirectories, fall back to directory-convention-based stack detection.

---

## Stack Loops Orchestrator (`pice-daemon::orchestrator`)

### Execution Flow

```
1. Check for .pice/layers.toml
   - Present → load it, skip detection
   - Absent → v0.1 single-loop behavior + warning suggesting `pice layers detect --write`
2. Filter layers to active set:
   a. Layers with changed files (git diff matches layer's paths globs)
   b. Always-run layers (infrastructure, deployment, observability)
   c. Direct dependency cascade: if layer B is active and layer A depends_on B, A is also active
      (direct only, NOT transitive — prevents activating entire stack on any change)
3. Build LayerDag → topological cohorts
4. For each cohort (sequential in Phase 1):
   For each layer in cohort (sequential in Phase 1):
     a. Filter git diff to files matching this layer's paths globs
     b. Load layer contract (.pice/contracts/{layer}.toml, or plan contract as fallback)
     c. Build context-isolated prompt: layer contract + filtered diff + CLAUDE.md ONLY
     d. Create provider session with optional `session/create` layer fields
     e. Run evaluation pass(es) via existing session lifecycle (run_session / run_session_and_capture)
     f. Record per-layer results to verification manifest
     // Phase 3: seam checks here
     // Phase 4: adaptive pass allocation here
     // Phase 6: review gate check here
5. Compute overall status: PASS only if every active layer PASS
```

### Integration with Existing Evaluate Handler

The existing `evaluate` handler in `pice-daemon/src/handlers/evaluate.rs` branches:
- If `.pice/layers.toml` exists → dispatch to stack loops orchestrator
- If absent → existing single-session evaluation (v0.1 behavior)

This branching is invisible to v0.1 users who haven't created `layers.toml`.

### Contract Sourcing (Two Modes)

1. **Per-layer contracts** (v0.2 path): `.pice/contracts/{layer}.toml` — layer-specific criteria
2. **Plan-level contract fallback** (v0.1 compatibility): if no per-layer contract exists for a layer, use the plan file's contract. This is the transition path for users who have plans with contracts but haven't written per-layer contracts yet.

### Context Isolation Enforcement

`context_filter.rs` in `pice-core` exposes:

```rust
pub fn build_layer_prompt(
    layer: &str,
    config: &LayersConfig,
    full_diff: &str,
    claude_md: &str,
) -> String
```

Each layer's evaluator sees ONLY:
- That layer's contract TOML
- Git diff filtered to files matching that layer's `paths` globs
- Project-level `CLAUDE.md`

Explicitly excluded:
- Other layers' contracts, diffs, or findings
- Cross-layer plan rationale
- Previous pass findings from other layers

### Diff Filtering

```rust
pub fn filter_diff_by_globs(full_diff: &str, globs: &[String]) -> String
```

Parses unified diff format, extracts per-file sections (split on `diff --git`), matches each file path against the glob set, reassembles only matching sections.

Edge cases requiring explicit handling:
- Binary files (no text hunks, just binary notice)
- Renames (old and new paths; match on new path)
- New files (`/dev/null` as old path)
- Deleted files (`/dev/null` as new path)
- File paths with spaces (quoted in diff headers)

### Layer Activation Logic

```rust
pub fn active_layers(config: &LayersConfig, changed_files: &[String]) -> Vec<String>
```

Rules (in order):
1. If layer's `paths` globs match any file in `changed_files` → active
2. If layer has `always_run = true` → active regardless of changed files
3. If layer B is active AND layer A has B in `depends_on` → A is active (direct only, not transitive)

### Provider Protocol Extension

`session/create` gains optional fields (backwards compatible via `#[serde(default)]`):

```json
{
  "method": "session/create",
  "params": {
    "mode": "evaluate",
    "layer": "backend",
    "layerPaths": ["src/server/**", "lib/**"],
    "contractPath": ".pice/contracts/backend.toml",
    "workingDirectory": "/path/to/project"
  }
}
```

v0.1 providers that don't understand these fields still work — single-layer fallback.

Both `pice-protocol` (Rust) and `@pice/provider-protocol` (TS) must be updated. Add roundtrip serialization tests for the new fields.

### Verification Manifest

Per-layer entries written to `~/.pice/state/{feature-id}.manifest.json`:

```json
{
  "schema_version": "0.2",
  "feature_id": "auth-feature-20260413-a3b2",
  "layers": [
    {
      "name": "backend",
      "status": "passed",
      "passes": [
        {
          "index": 1,
          "model": "claude-opus-4-6",
          "score": 8.2,
          "cost_usd": null,
          "timestamp": "2026-04-13T10:23:11Z",
          "findings": []
        }
      ],
      "seam_checks": [],
      "halted_by": null,
      "final_confidence": null,
      "total_cost_usd": null
    }
  ],
  "gates": [],
  "overall_status": "passed"
}
```

Fields for adaptive evaluation, seam checks, and gates are present but null/empty — filled by later phases.

---

## `pice layers` Command Suite

### CLI Structure

New `pice layers` parent command with 4 subcommands. New `LayersRequest` variant in `pice-core/src/cli/mod.rs`. New handler at `pice-daemon/src/handlers/layers.rs`.

All subcommands support `--json` for machine-readable output.

### `pice layers detect [--write] [--force] [--json]`

Runs six-level detection engine. Prints proposed `layers.toml` to stdout. With `--write`, writes to `.pice/layers.toml` (refuses if file exists unless `--force`). With `--json`, outputs structured detection results (layer names, paths, detection levels fired).

Local-only command — no provider session. Handler calls `pice_core::layers::detect_layers()` directly.

### `pice layers list [--json]`

Reads `.pice/layers.toml`, displays: layer names, path counts, always_run status, dependency edges. Errors if no `layers.toml` exists.

### `pice layers check [--json]`

Runs detection AND loads `.pice/layers.toml`. Compares detected vs. configured. Warns about: unlayered files (not matching any layer's globs), empty layers (zero matching files), differences between detection and configuration. Suggests additions.

### `pice layers graph [--json]`

Renders dependency DAG as ASCII art showing cohorts and edges. Detects and rejects cycles with clear error message including cycle path.

### `pice init --upgrade`

Extends existing `init` handler. In a v0.1 project (has `.pice/config.toml`):
1. Runs layer detection
2. Writes proposed `.pice/layers.toml` for review
3. Generates default per-layer contract templates in `.pice/contracts/`
4. Does NOT overwrite existing files without `--force`
5. `workflow.yaml` generation deferred to Phase 2

---

## Layer-Specific Contract Templates

7 default contract files embedded via `rust-embed`:

```
templates/pice/contracts/backend.toml
templates/pice/contracts/database.toml
templates/pice/contracts/api.toml
templates/pice/contracts/frontend.toml
templates/pice/contracts/infrastructure.toml
templates/pice/contracts/deployment.toml
templates/pice/contracts/observability.toml
```

Each defines layer-appropriate evaluation criteria. Generated into `.pice/contracts/` by `pice init --upgrade` and `pice layers detect --write`.

Custom layers (e.g., monorepo service names) get no default contract — user writes their own, or falls back to plan-level contract.

---

## Testing Strategy

### Unit Tests (`pice-core`)

**Detection engine:** Test each level independently with fixture directories via `tempfile::tempdir()`. ~6 tests per level, plus cross-level integration tests. Monorepo detection tests with mock `nx.json`/`turbo.json`.

**layers.toml parsing:** Roundtrip serialization. Invalid TOML. DAG cycle detection. Missing dependency references. Mirror `config/mod.rs` test patterns.

**Diff filtering:** Known unified diff inputs → expected filtered output. Edge cases: binary files, renames, new files, deleted files, paths with spaces.

**Layer activation:** Changed files + LayersConfig → expected active layers. Test always_run, direct dependency cascade, no-match scenarios.

**DAG construction:** Topological sort correctness. Cycle rejection with path. Cohort grouping.

### Integration Tests (`pice-daemon`)

**Stack Loops orchestrator:** End-to-end with stub provider. Multi-layer config → sequential evaluation → verify per-layer manifest output. Verify context isolation (each layer's session prompt contains only its own contract and filtered diff).

**`pice layers` commands:** Handler tests following existing pattern — call handler with `LayersRequest`, verify `CommandResponse`.

### Reference Fixtures

5-6 minimal project structures for framework detection:
- Next.js + Prisma + Terraform (7-layer reference from PRDv2 validation criteria)
- FastAPI + SQLAlchemy
- Rails
- Express
- SvelteKit
- Nx monorepo with 2 services

### Context Isolation Harness

Dedicated integration test that:
1. Constructs multi-layer scenario with unique marker strings per layer
2. Calls `build_layer_prompt` for each layer
3. Asserts each prompt contains its own markers and NONE of the other layers' markers
4. Verifies no cross-layer contract, diff, or finding leakage

---

## Design Decisions

1. **Level 5 (import graph) stubbed** — Returns empty. Levels 1–4 cover most real projects. Import graph analysis needs language-specific parsers and is a separate effort that doesn't block Phase 1 validation.

2. **Dependency cascade is direct only, not transitive** — Prevents a single database change from activating the entire stack. Users who want transitive activation set `always_run = true`.

3. **Override file skips detection** — PRDv2 line 756. No implicit detection when `layers.toml` exists. `pice layers check` runs detection on demand for comparison.

4. **No `layers.toml` → v0.1 behavior + warning** — Users must explicitly opt in to layer-aware evaluation. No auto-engagement.

5. **Plan-level contract fallback** — Users transitioning from v0.1 don't need per-layer contracts immediately. Their existing plan contracts work as a fallback applied to all layers.

6. **Seams section parsed but not executed** — `[seams]` in layers.toml is part of the schema and is parsed/validated in Phase 1, but seam check execution is Phase 3.

7. **`pice init --upgrade` generates layers.toml only** — `workflow.yaml` generation deferred to Phase 2 when the schema exists.

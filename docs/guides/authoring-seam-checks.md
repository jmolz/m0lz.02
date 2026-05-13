# Authoring seam checks

Seam checks are deterministic, static verifications that run between adjacent
layer boundaries during `pice evaluate`. PICE ships 12 default checks
covering the PRDv2 Feature 6 failure categories. This guide explains how to
author your own.

## When to write a seam check

Write a seam check when a specific class of cross-layer drift keeps biting
your team and can be detected by inspecting a filtered diff + a handful of
boundary-relevant files. Typical examples:

- Config field declared in one layer but consumed under a different name in
  another (`category 1: config_mismatch`).
- Schema column present in an ORM model but missing from a migration
  (`category 9: schema_drift`).
- Response field in an OpenAPI spec diverging from a handler return type
  (`category 3: openapi_compliance`).

If the drift is **dynamic** (e.g., runtime retry storms under load),
it belongs in v0.4 implicit-contract inference, not a static seam check.

## The trait

```rust
pub trait SeamCheck: Send + Sync {
    fn id(&self) -> &str;
    fn category(&self) -> u8;
    fn applies_to(&self, boundary: &LayerBoundary) -> bool;
    fn run(&self, ctx: &SeamContext<'_>) -> SeamResult;
}
```

| Method       | Contract                                                              |
|--------------|-----------------------------------------------------------------------|
| `id`         | Stable, unique identifier. Must match the file stem by convention.    |
| `category`   | PRDv2 failure category 1..=12. Plugin checks may return `0` via `None`.|
| `applies_to` | Cheap boundary filter. Return `false` if the check is out of scope.   |
| `run`        | Deterministic, <100ms, reads only `ctx.boundary_files`.               |

## The context

```rust
pub struct SeamContext<'a> {
    pub boundary: &'a LayerBoundary,
    pub filtered_diff: &'a str,     // unified diff restricted to boundary_files
    pub repo_root: &'a Path,
    pub boundary_files: &'a [PathBuf], // union of files on both sides
    pub args: Option<&'a BTreeMap<String, serde_json::Value>>,
}
```

**Context isolation is a hard rule.** The check only sees its own boundary.
Never reach outside `ctx.boundary_files` via `walk_dir` or similar. If a
check needs more data, declare it as a check arg (v0.3 plugin crate API).

## Result shape

- `SeamResult::Passed` — no finding.
- `SeamResult::Warning(findings)` — advisory. Does NOT fail the layer.
- `SeamResult::Failed(findings)` — fail-closed. Layer transitions to
  `Failed` with `halted_by="seam:<id>"`.

For v0.2, heuristic checks that extrapolate from static snapshots
(`cascade_timeout`, `cold_start_order`, `network_topology`, `retry_storm`)
must only ever emit `Warning`. Real runtime semantics land in v0.4.

## Performance budget

Every `run()` call has a **100ms wall-clock budget**. The seam runner
measures elapsed time post-hoc and downgrades overruns to `Warning` with a
budget-exceeded finding. If your check regularly exceeds the budget, shard it
into multiple IDs or move the heavy work into a precompute step.

## Example: skeleton check

```rust
use pice_core::seam::types::*;

pub struct MyCheck;

impl SeamCheck for MyCheck {
    fn id(&self) -> &str { "my_check" }
    fn category(&self) -> u8 { 7 }
    fn applies_to(&self, boundary: &LayerBoundary) -> bool {
        boundary.touches("backend")
    }
    fn run(&self, ctx: &SeamContext<'_>) -> SeamResult {
        let mut findings = Vec::new();
        for rel in ctx.boundary_files {
            let full = ctx.repo_root.join(rel);
            let Ok(content) = std::fs::read_to_string(&full) else { continue };
            if content.contains("FORBIDDEN_PATTERN") {
                findings.push(
                    SeamFinding::new("forbidden pattern detected")
                        .with_file(rel.clone()),
                );
            }
        }
        if findings.is_empty() { SeamResult::Passed } else { SeamResult::Failed(findings) }
    }
}
```

## Registering

v0.2: add your check to `pice-core::seam::defaults::register_defaults` and
submit a PR.

v0.3 (planned): plugin crates register via a discovery hook at daemon
startup. The trait is already `dyn`-compatible; only the loader is missing.

## Testing

Every default check ships with three tests:

1. **Happy path** — clean input, expect `Passed`.
2. **Negative** — crafted input producing the expected `Failed` or `Warning`.
3. **Out-of-scope** — `applies_to()` returns `false` for unrelated boundaries.

Plus one integration test in `crates/pice-daemon/tests/seam_integration.rs`
that exercises the full `run_stack_loops → run_seams_for_layer → manifest`
flow and asserts the `category`, `boundary`, and `halted_by` fields.

## See also

- `PRDv2.md` § Feature 6 — the 12-category taxonomy
- `.codex/rules/stack-loops.md` § Seam verification — source-tree invariants
- `docs/methodology/evaluate.md` — how seam findings surface in the evaluate
  report

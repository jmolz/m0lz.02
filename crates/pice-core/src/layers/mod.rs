//! Layer types, `.pice/layers.toml` parsing, validation, and DAG construction.
//!
//! This module defines the data model for PICE v0.2 Stack Loops: layers
//! represent architectural components of a project (backend, database, API,
//! frontend, infrastructure, deployment, observability). A feature is PASS
//! only when every active layer passes.
//!
//! The authoritative schema matches PRDv2 lines 778–827 and
//! `.claude/rules/stack-loops.md`.

pub mod detect;
pub mod filter;
pub mod manifest;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

// ─── Error types ────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum LayerError {
    #[error("dependency cycle detected: {0}")]
    CyclicDependency(String),
    #[error("layer '{0}' referenced in depends_on but not defined")]
    UndefinedDependency(String),
    #[error("order list is empty")]
    EmptyOrder,
    #[error("duplicate layer in order list: '{0}'")]
    DuplicateLayer(String),
    #[error("parse error: {0}")]
    ParseError(String),
}

// ─── Core types ─────────────────────────────────────────────────────────────

/// Top-level `.pice/layers.toml` configuration.
///
/// The `[layers]` table contains `order` and per-layer definitions.
/// Mirrors the PRDv2 reference format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayersConfig {
    pub layers: LayersTable,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seams: Option<BTreeMap<String, Vec<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_contracts: Option<BTreeMap<String, ExternalContract>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stacks: Option<BTreeMap<String, StackDef>>,
}

/// The `[layers]` table — contains `order` and flattened layer definitions.
///
/// TOML layout:
/// ```toml
/// [layers]
/// order = ["backend", "database", ...]
///
/// [layers.backend]
/// paths = ["src/server/**"]
/// ```
///
/// serde flattens the per-layer BTreeMap alongside the `order` field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayersTable {
    pub order: Vec<String>,
    #[serde(flatten)]
    pub defs: BTreeMap<String, LayerDef>,
}

/// Definition of a single layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerDef {
    pub paths: Vec<String>,
    #[serde(default)]
    pub always_run: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "type")]
    pub layer_type: Option<LayerType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_variants: Option<Vec<String>>,
}

/// Layer type — currently only `meta` for IaC layers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LayerType {
    Meta,
}

/// Monorepo stack definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackDef {
    pub root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layers: Option<LayersConfig>,
}

/// External contract reference (polyrepo workaround).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalContract {
    pub spec: String,
    #[serde(rename = "type")]
    pub contract_type: String,
}

/// Directed acyclic graph of layer dependencies, grouped into cohorts.
#[derive(Debug, Clone)]
pub struct LayerDag {
    /// Layers grouped by topological level — layers within a cohort have
    /// no pending dependencies and can execute in parallel.
    pub cohorts: Vec<Vec<String>>,
    /// Edges: (dependent, dependency) — "A depends_on B" → ("A", "B").
    pub edges: Vec<(String, String)>,
}

// ─── Implementation ─────────────────────────────────────────────────────────

impl LayersConfig {
    /// Load and validate a `.pice/layers.toml` file.
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read layers config from {}", path.display()))?;
        let config: LayersConfig = toml::from_str(&content)
            .with_context(|| format!("failed to parse layers config from {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    /// Validate layer definitions: check for empty order, undefined deps, cycles.
    pub fn validate(&self) -> Result<()> {
        if self.layers.order.is_empty() {
            return Err(LayerError::EmptyOrder.into());
        }

        // Check for duplicate layer names in order
        let mut seen = HashSet::new();
        for name in &self.layers.order {
            if !seen.insert(name.as_str()) {
                return Err(LayerError::DuplicateLayer(name.clone()).into());
            }
        }

        // Check that depends_on references exist
        for (name, def) in &self.layers.defs {
            for dep in &def.depends_on {
                if !self.layers.defs.contains_key(dep) {
                    return Err(LayerError::UndefinedDependency(format!(
                        "{dep} (referenced by {name})"
                    ))
                    .into());
                }
            }
        }

        // Check for cycles
        self.detect_cycle()?;

        Ok(())
    }

    /// Serialize back to TOML (for `detect --write`).
    pub fn to_toml_string(&self) -> Result<String> {
        toml::to_string_pretty(self).context("failed to serialize layers config to TOML")
    }

    /// Build a topological DAG from layer dependencies.
    pub fn build_dag(&self) -> Result<LayerDag> {
        self.detect_cycle()?;

        let mut edges = Vec::new();
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();

        // Initialize all layers with 0 in-degree
        for name in &self.layers.order {
            in_degree.entry(name.as_str()).or_insert(0);
            adj.entry(name.as_str()).or_default();
        }

        // Build edges from depends_on
        for (name, def) in &self.layers.defs {
            if !self.layers.order.contains(name) {
                continue;
            }
            for dep in &def.depends_on {
                if self.layers.order.contains(dep) {
                    edges.push((name.clone(), dep.clone()));
                    adj.entry(dep.as_str()).or_default().push(name.as_str());
                    *in_degree.entry(name.as_str()).or_insert(0) += 1;
                }
            }
        }

        // Kahn's algorithm for topological sort into cohorts
        let mut cohorts = Vec::new();
        let mut queue: VecDeque<&str> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(&name, _)| name)
            .collect();

        // Sort the initial queue for deterministic ordering
        let mut sorted_queue: Vec<&str> = queue.drain(..).collect();
        sorted_queue.sort();
        queue.extend(sorted_queue);

        while !queue.is_empty() {
            let cohort: Vec<String> = queue.drain(..).map(|s| s.to_string()).collect();
            let mut next_queue = Vec::new();

            for node in &cohort {
                if let Some(dependents) = adj.get(node.as_str()) {
                    for &dependent in dependents {
                        // Phase 4.1 Pass-6 C13: this `expect` is safe by DAG
                        // invariant — `dependent` came from `adj[node]` which
                        // was built from the same key set as `in_degree`.
                        // Grandfathered under `-D clippy::expect_used`.
                        #[allow(clippy::expect_used)]
                        let deg = in_degree.get_mut(dependent).expect("node in graph");
                        *deg -= 1;
                        if *deg == 0 {
                            next_queue.push(dependent);
                        }
                    }
                }
            }

            cohorts.push(cohort);
            next_queue.sort();
            queue.extend(next_queue);
        }

        Ok(LayerDag { cohorts, edges })
    }

    /// Detect cycles using DFS. Returns an error with the cycle path if found.
    fn detect_cycle(&self) -> Result<()> {
        #[derive(Clone, Copy, PartialEq)]
        enum Color {
            White,
            Gray,
            Black,
        }

        let mut color: HashMap<&str, Color> = HashMap::new();
        let mut parent: HashMap<&str, &str> = HashMap::new();

        for name in self.layers.defs.keys() {
            color.insert(name.as_str(), Color::White);
        }

        for start in self.layers.defs.keys() {
            if color.get(start.as_str()) != Some(&Color::White) {
                continue;
            }

            let mut stack = vec![(start.as_str(), false)];

            while let Some((node, backtrack)) = stack.pop() {
                if backtrack {
                    color.insert(node, Color::Black);
                    continue;
                }

                color.insert(node, Color::Gray);
                stack.push((node, true)); // push backtrack marker

                if let Some(def) = self.layers.defs.get(node) {
                    for dep in &def.depends_on {
                        let dep_str = dep.as_str();
                        match color.get(dep_str) {
                            Some(Color::Gray) => {
                                // Found a cycle — reconstruct path
                                let mut path = vec![dep_str];
                                let mut cur = node;
                                while cur != dep_str {
                                    path.push(cur);
                                    cur = parent.get(cur).copied().unwrap_or(dep_str);
                                }
                                path.push(dep_str);
                                path.reverse();
                                let cycle_str = path.join(" → ");
                                return Err(LayerError::CyclicDependency(cycle_str).into());
                            }
                            Some(Color::White) | None => {
                                parent.insert(dep_str, node);
                                stack.push((dep_str, false));
                            }
                            Some(Color::Black) => {} // already processed
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

impl Default for LayersConfig {
    fn default() -> Self {
        Self {
            layers: LayersTable {
                order: Vec::new(),
                defs: BTreeMap::new(),
            },
            seams: None,
            external_contracts: None,
            stacks: None,
        }
    }
}

/// Determine which layers are active given a set of changed files.
///
/// Activation rules:
/// 1. A layer is active if any changed file matches its path globs
/// 2. `always_run` layers are always active
/// 3. Transitive dependency cascade: if B is active and A depends_on B,
///    A becomes active. If C depends_on A, C also becomes active.
///    This ensures downstream consumers are verified when upstream changes.
pub fn active_layers(config: &LayersConfig, changed_files: &[String]) -> Vec<String> {
    let mut active = BTreeSet::new();

    // Step 1 + 2: glob match + always_run
    for name in &config.layers.order {
        if let Some(def) = config.layers.defs.get(name) {
            if def.always_run {
                active.insert(name.clone());
                continue;
            }
            for file in changed_files {
                if file_matches_globs(file, &def.paths) {
                    active.insert(name.clone());
                    break;
                }
            }
        }
    }

    // Step 3: transitive dependency cascade via fixed-point iteration.
    // If B is active and A depends_on B, then A becomes active.
    // If C depends_on A, C also becomes active (transitive closure).
    // This catches downstream breakage — exactly the class of failures
    // Stack Loops was built to detect.
    loop {
        let mut changed = false;
        for name in &config.layers.order {
            if active.contains(name) {
                continue;
            }
            if let Some(def) = config.layers.defs.get(name) {
                for dep in &def.depends_on {
                    if active.contains(dep) {
                        active.insert(name.clone());
                        changed = true;
                        break;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    // Return in order
    config
        .layers
        .order
        .iter()
        .filter(|n| active.contains(*n))
        .cloned()
        .collect()
}

/// Return all layers whose path globs match a given file path.
pub fn tag_file_to_layers(config: &LayersConfig, file_path: &str) -> Vec<String> {
    config
        .layers
        .order
        .iter()
        .filter(|name| {
            config
                .layers
                .defs
                .get(*name)
                .map(|def| file_matches_globs(file_path, &def.paths))
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

/// Check if a file path matches any of the given glob patterns.
fn file_matches_globs(file_path: &str, globs: &[String]) -> bool {
    for pattern_str in globs {
        if let Ok(pattern) = glob::Pattern::new(pattern_str) {
            if pattern.matches(file_path) {
                return true;
            }
        }
    }
    false
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Reference TOML from PRDv2 lines 778–827.
    const REFERENCE_TOML: &str = r#"
[layers]
order = ["backend", "database", "api", "frontend", "infrastructure", "deployment", "observability"]

[layers.backend]
paths = ["src/server/**", "lib/**"]
always_run = false
contract = ".pice/contracts/backend.toml"

[layers.database]
paths = ["prisma/**", "migrations/**", "src/models/**"]
always_run = false
contract = ".pice/contracts/database.toml"
depends_on = []

[layers.api]
paths = ["src/server/routes/**", "pages/api/**"]
always_run = false
contract = ".pice/contracts/api.toml"
depends_on = ["backend", "database"]

[layers.frontend]
paths = ["pages/**", "src/client/**", "app/**"]
always_run = false
contract = ".pice/contracts/frontend.toml"
depends_on = ["api"]

[layers.infrastructure]
paths = ["terraform/**", "Dockerfile", "docker-compose.yml"]
always_run = true
type = "meta"
contract = ".pice/contracts/infrastructure.toml"

[layers.deployment]
paths = [".github/workflows/**", "deploy/**", "vercel.json"]
always_run = true
depends_on = ["infrastructure"]
environment_variants = ["staging", "production"]
contract = ".pice/contracts/deployment.toml"

[layers.observability]
paths = ["monitoring/**", "otel/**", "prometheus/**", "grafana/**"]
always_run = true
depends_on = ["deployment"]
contract = ".pice/contracts/observability.toml"

[external_contracts]
api_gateway = { spec = "https://api.example.com/openapi.json", type = "openapi" }
"#;

    #[test]
    fn roundtrip_reference_format() {
        let config: LayersConfig = toml::from_str(REFERENCE_TOML).unwrap();

        assert_eq!(config.layers.order.len(), 7);
        assert_eq!(config.layers.order[0], "backend");
        assert_eq!(config.layers.order[6], "observability");
        assert_eq!(config.layers.defs.len(), 7);

        // Check specific fields
        let infra = &config.layers.defs["infrastructure"];
        assert!(infra.always_run);
        assert_eq!(infra.layer_type, Some(LayerType::Meta));

        let deploy = &config.layers.defs["deployment"];
        assert!(deploy.always_run);
        assert_eq!(deploy.depends_on, vec!["infrastructure"]);
        assert_eq!(
            deploy.environment_variants,
            Some(vec!["staging".to_string(), "production".to_string()])
        );

        let api = &config.layers.defs["api"];
        assert_eq!(api.depends_on, vec!["backend", "database"]);

        // External contracts
        let ext = config.external_contracts.as_ref().unwrap();
        assert_eq!(ext["api_gateway"].contract_type, "openapi");

        // Serialize → re-parse
        let toml_str = config.to_toml_string().unwrap();
        let reparsed: LayersConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(reparsed.layers.order.len(), 7);
        assert_eq!(reparsed.layers.defs.len(), 7);
    }

    #[test]
    fn cycle_detection_a_b_c_a() {
        let toml_str = r#"
[layers]
order = ["a", "b", "c"]

[layers.a]
paths = ["a/**"]
depends_on = ["c"]

[layers.b]
paths = ["b/**"]
depends_on = ["a"]

[layers.c]
paths = ["c/**"]
depends_on = ["b"]
"#;
        let config: LayersConfig = toml::from_str(toml_str).unwrap();
        let err = config.validate().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("cycle"), "should mention cycle, got: {msg}");
        // Should contain arrow notation with the cycle path
        assert!(
            msg.contains("→"),
            "should contain arrow notation, got: {msg}"
        );
        // The cycle path must name all three nodes and form a loop (start == end)
        let arrows: Vec<&str> = msg.split(" → ").collect();
        assert!(
            arrows.len() >= 3,
            "cycle path should have at least 3 nodes, got: {msg}"
        );
        assert_eq!(
            arrows.first().unwrap().split(": ").last().unwrap_or(""),
            *arrows.last().unwrap(),
            "cycle path must start and end with same node, got: {msg}"
        );
        // All three layer names must appear in the path
        for name in &["a", "b", "c"] {
            assert!(
                msg.contains(name),
                "cycle path should contain node '{name}', got: {msg}"
            );
        }
    }

    #[test]
    fn undefined_dependency_rejected() {
        let toml_str = r#"
[layers]
order = ["a"]

[layers.a]
paths = ["a/**"]
depends_on = ["nonexistent"]
"#;
        let config: LayersConfig = toml::from_str(toml_str).unwrap();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn empty_order_rejected() {
        let toml_str = r#"
[layers]
order = []
"#;
        let config: LayersConfig = toml::from_str(toml_str).unwrap();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn duplicate_layer_in_order_rejected() {
        let toml_str = r#"
[layers]
order = ["a", "a"]

[layers.a]
paths = ["a/**"]
"#;
        let config: LayersConfig = toml::from_str(toml_str).unwrap();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn dag_cohort_grouping() {
        // backend and database are independent → same cohort
        // api depends on both → next cohort
        // frontend depends on api → last cohort
        let config: LayersConfig = toml::from_str(REFERENCE_TOML).unwrap();
        let dag = config.build_dag().unwrap();

        assert!(!dag.cohorts.is_empty());

        // First cohort: backend, database (no deps) + infrastructure (no deps)
        let first = &dag.cohorts[0];
        assert!(first.contains(&"backend".to_string()));
        assert!(first.contains(&"database".to_string()));
        assert!(first.contains(&"infrastructure".to_string()));

        // api depends on backend + database → not in first cohort
        assert!(!first.contains(&"api".to_string()));
    }

    #[test]
    fn active_layers_glob_match() {
        let config: LayersConfig = toml::from_str(REFERENCE_TOML).unwrap();
        let changed = vec!["src/server/routes/auth.ts".to_string()];
        let active = active_layers(&config, &changed);

        // api matches (src/server/routes/**), always_run layers active
        assert!(active.contains(&"api".to_string()));
        assert!(active.contains(&"infrastructure".to_string()));
        assert!(active.contains(&"deployment".to_string()));
        assert!(active.contains(&"observability".to_string()));
    }

    #[test]
    fn active_layers_always_run_with_no_changes() {
        let config: LayersConfig = toml::from_str(REFERENCE_TOML).unwrap();
        let changed: Vec<String> = vec![];
        let active = active_layers(&config, &changed);

        // Only always_run layers
        assert!(active.contains(&"infrastructure".to_string()));
        assert!(active.contains(&"deployment".to_string()));
        assert!(active.contains(&"observability".to_string()));
        assert!(!active.contains(&"backend".to_string()));
    }

    #[test]
    fn active_layers_direct_dependency_cascade() {
        // If database is active (via glob match), api should become active
        // because api depends_on database.
        let config: LayersConfig = toml::from_str(REFERENCE_TOML).unwrap();
        let changed = vec!["prisma/schema.prisma".to_string()];
        let active = active_layers(&config, &changed);

        assert!(active.contains(&"database".to_string()));
        assert!(
            active.contains(&"api".to_string()),
            "api should cascade from database"
        );
    }

    #[test]
    fn active_layers_transitive_cascade() {
        // database active → api cascades (depends_on database)
        // frontend depends_on api → frontend ALSO cascades (transitive).
        // This ensures downstream consumers are verified when upstream changes.
        let config: LayersConfig = toml::from_str(REFERENCE_TOML).unwrap();
        let changed = vec!["prisma/schema.prisma".to_string()];
        let active = active_layers(&config, &changed);

        assert!(active.contains(&"database".to_string()));
        assert!(active.contains(&"api".to_string()));
        assert!(
            active.contains(&"frontend".to_string()),
            "frontend SHOULD transitively cascade from database via api"
        );
    }

    #[test]
    fn tag_file_to_multiple_layers() {
        let config: LayersConfig = toml::from_str(REFERENCE_TOML).unwrap();
        // pages/api/users.ts matches both api (pages/api/**) and frontend (pages/**)
        let layers = tag_file_to_layers(&config, "pages/api/users.ts");
        assert!(layers.contains(&"api".to_string()));
        assert!(layers.contains(&"frontend".to_string()));
    }

    #[test]
    fn load_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layers.toml");
        std::fs::write(&path, REFERENCE_TOML).unwrap();

        let config = LayersConfig::load(&path).unwrap();
        assert_eq!(config.layers.order.len(), 7);
    }

    #[test]
    fn load_nonexistent_returns_error() {
        let result = LayersConfig::load(&PathBuf::from("/nonexistent/layers.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn load_invalid_toml_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "this is not valid toml [[[").unwrap();
        let result = LayersConfig::load(&path);
        assert!(result.is_err());
    }

    // ─── Phase 5 build_dag tests ───────────────────────────────────────
    //
    // These cover multi-cohort topologies at the pure-function level.
    // Orchestrator-level behavior is already covered end-to-end by
    // `crates/pice-daemon/tests/parallel_cohort_integration.rs`, but
    // pure-function tests give faster feedback on DAG regressions
    // without spinning up the daemon stack.

    #[test]
    fn build_dag_diamond_topology_produces_three_cohorts() {
        // D → {B, C} → A → {}
        // Cohorts should be [[D], [B, C], [A]] — three distinct levels.
        let toml_str = r#"
[layers]
order = ["a", "b", "c", "d"]

[layers.a]
paths = ["a/**"]
depends_on = ["b", "c"]

[layers.b]
paths = ["b/**"]
depends_on = ["d"]

[layers.c]
paths = ["c/**"]
depends_on = ["d"]

[layers.d]
paths = ["d/**"]
"#;
        let config: LayersConfig = toml::from_str(toml_str).unwrap();
        let dag = config.build_dag().unwrap();

        assert_eq!(
            dag.cohorts.len(),
            3,
            "diamond: expected 3 cohorts (d → {{b,c}} → a); got {:?}",
            dag.cohorts
        );
        assert_eq!(dag.cohorts[0], vec!["d".to_string()]);
        // Cohort 1 has {b, c} — `build_dag` sorts the intra-cohort order
        // via `sort()` on next_queue, so lexicographic "b" < "c".
        assert_eq!(dag.cohorts[1], vec!["b".to_string(), "c".to_string()]);
        assert_eq!(dag.cohorts[2], vec!["a".to_string()]);
    }

    #[test]
    fn build_dag_all_independent_layers_produces_single_cohort() {
        // Five layers, zero edges → every layer is in the first cohort.
        // This is the maximum parallelism case — a 5-wide fan-out.
        let toml_str = r#"
[layers]
order = ["alpha", "bravo", "charlie", "delta", "echo"]

[layers.alpha]
paths = ["a/**"]

[layers.bravo]
paths = ["b/**"]

[layers.charlie]
paths = ["c/**"]

[layers.delta]
paths = ["d/**"]

[layers.echo]
paths = ["e/**"]
"#;
        let config: LayersConfig = toml::from_str(toml_str).unwrap();
        let dag = config.build_dag().unwrap();

        assert_eq!(
            dag.cohorts.len(),
            1,
            "all-independent: expected 1 cohort; got {:?}",
            dag.cohorts
        );
        assert_eq!(
            dag.cohorts[0],
            vec![
                "alpha".to_string(),
                "bravo".to_string(),
                "charlie".to_string(),
                "delta".to_string(),
                "echo".to_string()
            ]
        );
        assert_eq!(
            dag.edges.len(),
            0,
            "all-independent: expected zero edges; got {:?}",
            dag.edges
        );
    }

    #[test]
    fn build_dag_deterministic_across_runs() {
        // Two identical configs must produce byte-identical cohort vectors.
        // Nondeterminism here (e.g., HashMap iteration order leaking into
        // cohort assembly) would break Phase 5's "manifest layers[] order =
        // DAG topological order" invariant pinned by the orchestrator
        // `parallel_cohort_preserves_dag_order` test — pinning it at the
        // pure-function layer catches regressions faster.
        let toml_str = r#"
[layers]
order = ["backend", "database", "api", "frontend"]

[layers.backend]
paths = ["backend/**"]

[layers.database]
paths = ["db/**"]

[layers.api]
paths = ["api/**"]
depends_on = ["backend", "database"]

[layers.frontend]
paths = ["web/**"]
depends_on = ["api"]
"#;
        let config: LayersConfig = toml::from_str(toml_str).unwrap();
        let dag_a = config.build_dag().unwrap();
        let dag_b = config.build_dag().unwrap();
        assert_eq!(
            dag_a.cohorts, dag_b.cohorts,
            "two back-to-back build_dag calls must produce identical cohorts"
        );
        // Also verify cohort structure since we assert equality above:
        assert_eq!(
            dag_a.cohorts[0],
            vec!["backend".to_string(), "database".to_string()]
        );
        assert_eq!(dag_a.cohorts[1], vec!["api".to_string()]);
        assert_eq!(dag_a.cohorts[2], vec!["frontend".to_string()]);
    }

    #[test]
    fn build_dag_rejects_cycle() {
        // A → B → A forms a cycle. `build_dag` calls `detect_cycle` up
        // front and must return an error rather than emit partial cohorts.
        // This is the error-case companion to the happy-path tests above.
        let toml_str = r#"
[layers]
order = ["a", "b"]

[layers.a]
paths = ["a/**"]
depends_on = ["b"]

[layers.b]
paths = ["b/**"]
depends_on = ["a"]
"#;
        let config: LayersConfig = toml::from_str(toml_str).unwrap();
        let err = config.build_dag().unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("cycle"),
            "expected cycle error; got: {err}"
        );
    }

    #[test]
    fn seams_section_parsed() {
        let toml_str = r#"
[layers]
order = ["backend", "api"]

[layers.backend]
paths = ["src/**"]

[layers.api]
paths = ["api/**"]
depends_on = ["backend"]

[seams]
"backend↔api" = ["schema_match", "response_format"]
"#;
        let config: LayersConfig = toml::from_str(toml_str).unwrap();
        config.validate().unwrap();
        let seams = config.seams.as_ref().unwrap();
        assert_eq!(seams["backend↔api"].len(), 2);
    }
}

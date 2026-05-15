//! Phase 7 `JobEnv` — the immutable environment snapshot captured at
//! background-dispatch time and passed to the spawned orchestrator future.
//!
//! ## Why snapshot?
//!
//! Phase 7 dispatches `pice evaluate --background` and `pice execute
//! --background` as detached tokio tasks. The daemon is long-lived and its
//! process env can change between the time a job is dispatched and the time
//! the spawned future actually begins reading state. Without a snapshot,
//! feature A dispatched with `PICE_STATE_DIR=/a` could end up writing to
//! `/b` if an adjacent `PICE_STATE_DIR=/b` command mutates the process env
//! mid-flight.
//!
//! `JobEnv` captures every env-derived value at dispatch time. The spawned
//! future reads ONLY from the snapshot; it never re-reads `std::env::var`
//! or the daemon's live `PiceConfig`. This is load-bearing for contract
//! criterion #16 (`job_env_snapshot_integration.rs`).

use crate::plan_parser::PlanTrace;
use crate::workflow::schema::WorkflowConfig;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Immutable env snapshot passed to a background feature's orchestrator
/// closure. Built at dispatch time from the daemon's `DaemonContext` — never mutated,
/// always `Arc`-shared across the spawned future's lifetime.
///
/// `BTreeMap` (not `HashMap`) for `contracts` so iteration order is
/// deterministic — integration tests assert stable manifest shapes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobEnv {
    /// Resolved `~/.pice/state/{project_hash_12chars}` directory for the
    /// current project — the ONLY location the orchestrator writes
    /// manifests during the feature's lifetime, regardless of later
    /// process env mutations.
    pub state_dir: PathBuf,

    /// Project root (the git repository's working-tree path). Used by
    /// the orchestrator for filesystem-relative paths (layer globs,
    /// contract resolution).
    pub project_root: PathBuf,

    /// Full merged-and-validated workflow snapshot (framework + project +
    /// user layers). Cloned once at dispatch; the spawned future reads all
    /// workflow config from this snapshot, never from the daemon's live
    /// `PiceConfig`.
    pub workflow_snapshot: WorkflowConfig,

    /// Map of `layer name → absolute contract file path` captured at
    /// dispatch. Resolved against the project root so the spawned
    /// future can load contracts without re-running path discovery.
    pub contracts: BTreeMap<String, PathBuf>,

    /// Plan/contract trace metadata captured at dispatch time. Optional
    /// for backwards compatibility with older jobs/tests and for future
    /// non-plan background jobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_trace: Option<PlanTrace>,

    /// Captured value of `PICE_STATE_DIR` env var at dispatch time (or
    /// `None` if unset). Distinct from [`Self::state_dir`] which is the
    /// RESOLVED path; this field is the RAW env var for debugging /
    /// integration-test assertions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pice_state_dir_override: Option<PathBuf>,

    /// Captured value of `PICE_USER_WORKFLOW_FILE` env var at dispatch
    /// time — the `~/.pice/workflow.yaml` path the user-layer merge
    /// resolved against. Snapshotted so a rename of the user workflow
    /// file mid-run does not leak into already-dispatched features.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pice_user_workflow_file: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::schema::{CostCapBehavior, Defaults, WorkflowConfig};

    fn stub_workflow() -> WorkflowConfig {
        // Minimal `WorkflowConfig` — mirrors the `base()` fixture used
        // across `workflow::merge` tests. JobEnv holds the whole struct
        // by value so the spawned future sees a frozen view of the
        // workflow at dispatch time.
        WorkflowConfig {
            schema_version: "0.2".into(),
            defaults: Defaults {
                tier: 2,
                min_confidence: 0.90,
                max_passes: 5,
                model: "sonnet".into(),
                budget_usd: 2.0,
                cost_cap_behavior: CostCapBehavior::Halt,
                max_parallelism: None,
                max_global_provider_concurrency: None,
            },
            phases: Default::default(),
            layer_overrides: BTreeMap::new(),
            review: None,
            seams: None,
        }
    }

    #[test]
    fn job_env_roundtrip_minimal() {
        let env = JobEnv {
            state_dir: PathBuf::from("/home/u/.pice/state/abc123def456"),
            project_root: PathBuf::from("/home/u/code/foo"),
            workflow_snapshot: stub_workflow(),
            contracts: BTreeMap::new(),
            plan_trace: None,
            pice_state_dir_override: None,
            pice_user_workflow_file: None,
        };
        let wire = serde_json::to_string(&env).unwrap();
        assert!(!wire.contains(r#""pice_state_dir_override":"#));
        assert!(!wire.contains(r#""pice_user_workflow_file":"#));
        let back: JobEnv = serde_json::from_str(&wire).unwrap();
        assert_eq!(back.state_dir, env.state_dir);
        assert_eq!(back.project_root, env.project_root);
        assert_eq!(back.contracts, env.contracts);
        assert!(back.pice_state_dir_override.is_none());
    }

    #[test]
    fn job_env_roundtrip_with_contracts_and_overrides() {
        let mut contracts = BTreeMap::new();
        contracts.insert(
            "backend".to_string(),
            PathBuf::from("/repo/.pice/contracts/backend.toml"),
        );
        contracts.insert(
            "frontend".to_string(),
            PathBuf::from("/repo/.pice/contracts/frontend.toml"),
        );

        let env = JobEnv {
            state_dir: PathBuf::from("/tmp/a"),
            project_root: PathBuf::from("/repo"),
            workflow_snapshot: stub_workflow(),
            contracts,
            plan_trace: Some(PlanTrace {
                plan_path: ".codex/plans/trace.md".to_string(),
                plan_sha256: "a".repeat(64),
                contract_sha256: "b".repeat(64),
                contract_feature: "Trace".to_string(),
                contract_tier: 2,
                has_spec_traceability: true,
            }),
            pice_state_dir_override: Some(PathBuf::from("/tmp/a")),
            pice_user_workflow_file: Some(PathBuf::from("/home/u/.pice/workflow.yaml")),
        };
        let wire = serde_json::to_string(&env).unwrap();
        let back: JobEnv = serde_json::from_str(&wire).unwrap();
        assert_eq!(back.contracts.len(), 2);
        assert_eq!(
            back.contracts.get("backend").unwrap(),
            &PathBuf::from("/repo/.pice/contracts/backend.toml")
        );
        assert_eq!(back.pice_state_dir_override, Some(PathBuf::from("/tmp/a")));
        assert_eq!(
            back.plan_trace
                .as_ref()
                .map(|t| t.contract_feature.as_str()),
            Some("Trace")
        );
    }

    #[test]
    fn job_env_contracts_iteration_order_is_stable() {
        // BTreeMap guarantee — tests depend on deterministic serialized
        // order, e.g. `job_env_snapshot_integration.rs` will compare
        // snapshots byte-for-byte.
        let mut contracts = BTreeMap::new();
        contracts.insert("z-layer".to_string(), PathBuf::from("z"));
        contracts.insert("a-layer".to_string(), PathBuf::from("a"));
        contracts.insert("m-layer".to_string(), PathBuf::from("m"));

        let env = JobEnv {
            state_dir: PathBuf::from("/s"),
            project_root: PathBuf::from("/p"),
            workflow_snapshot: stub_workflow(),
            contracts,
            plan_trace: None,
            pice_state_dir_override: None,
            pice_user_workflow_file: None,
        };
        let wire = serde_json::to_string(&env).unwrap();
        let a_pos = wire.find("a-layer").unwrap();
        let m_pos = wire.find("m-layer").unwrap();
        let z_pos = wire.find("z-layer").unwrap();
        assert!(
            a_pos < m_pos && m_pos < z_pos,
            "BTreeMap keys must serialize in ascending order; got {wire}"
        );
    }
}

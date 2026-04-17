//! Workflow file loading — embedded defaults, project file, user file, resolve().
//!
//! Resolution order: framework → project → user. Each level is merged into the
//! previous via [`merge::merge_with_floor`]. Schema version `"0.2"` is checked
//! on every loaded config before any merge happens — a mismatch is a hard error.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

use crate::workflow::merge::{merge_with_floor, overlay as overlay_merge};
use crate::workflow::schema::WorkflowConfig;
use crate::workflow::SCHEMA_VERSION;

const FRAMEWORK_WORKFLOW_YAML: &str = include_str!("../../../../templates/pice/workflow.yaml");

/// The framework-default workflow, embedded in the binary at build time.
///
/// Panics only if the embedded YAML is malformed, which is asserted by the
/// `embedded_defaults_parses` unit test. This is the sole exception to the
/// no-unwrap rule in pice-core: the embedded string is build-time data.
pub fn embedded_defaults() -> WorkflowConfig {
    // Phase 4.1 Pass-6 C13: the embedded YAML is static build-time data;
    // `embedded_defaults_parses` asserts it parses. A panic here would be
    // a deterministic build failure, not a runtime surprise.
    // Grandfathered under `-D clippy::expect_used`.
    #[allow(clippy::expect_used)]
    {
        serde_yaml::from_str(FRAMEWORK_WORKFLOW_YAML)
            .expect("embedded framework workflow.yaml must parse (build-time asserted)")
    }
}

/// Load `<project_root>/.pice/workflow.yaml`. Returns `Ok(None)` if absent.
pub fn load_project(project_root: &Path) -> Result<Option<WorkflowConfig>> {
    let path = project_root.join(".pice").join("workflow.yaml");
    load_from_path_optional(&path)
}

/// Load `~/.pice/workflow.yaml`. Returns `Ok(None)` if absent.
pub fn load_user() -> Result<Option<WorkflowConfig>> {
    let home = home_dir()?;
    let path = home.join(".pice").join("workflow.yaml");
    load_from_path_optional(&path)
}

/// Resolve the effective workflow for a project by merging framework →
/// project → user. Framework defaults always apply; project and user layers
/// are optional. Floor violations at either merge step are returned as errors.
pub fn resolve(project_root: &Path) -> Result<WorkflowConfig> {
    let framework = embedded_defaults();
    let project = load_project(project_root)?;
    let user = load_user()?;

    // Framework → project: simple overlay. PRDv2 floor semantics apply only
    // to project → user (lines 903–918).
    let after_project = match project {
        Some(p) => overlay_merge(framework, p),
        None => framework,
    };

    let effective = match user {
        Some(u) => merge_with_floor(after_project, u).context("merging user workflow.yaml")?,
        None => after_project,
    };

    Ok(effective)
}

fn load_from_path_optional(path: &Path) -> Result<Option<WorkflowConfig>> {
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read workflow from {}", path.display()))?;
    let cfg: WorkflowConfig = serde_yaml::from_str(&content)
        .with_context(|| format!("failed to parse YAML at {}", path.display()))?;
    check_schema_version(&cfg, path)?;
    Ok(Some(cfg))
}

fn check_schema_version(cfg: &WorkflowConfig, path: &Path) -> Result<()> {
    if cfg.schema_version != SCHEMA_VERSION {
        return Err(anyhow!(
            "workflow at {} declares schema_version \"{}\"; expected \"{}\". \
             Upgrade the file by bumping `schema_version` and reviewing fields \
             against PRDv2 § Feature 4.",
            path.display(),
            cfg.schema_version,
            SCHEMA_VERSION
        ));
    }
    Ok(())
}

/// Resolve the user's home directory via environment variables.
/// Mirrors [`layers::manifest::home_dir`] — HOME on Unix, USERPROFILE on
/// Windows, with cross-fallback. Kept crate-local to avoid a public re-export
/// of an infrastructural helper.
fn home_dir() -> Result<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .context("could not determine home directory (neither HOME nor USERPROFILE is set)")
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(path: &Path, s: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, s).unwrap();
    }

    #[test]
    fn embedded_defaults_parses() {
        let cfg = embedded_defaults();
        assert_eq!(cfg.schema_version, "0.2");
        assert_eq!(cfg.defaults.tier, 2);
        assert!((cfg.defaults.min_confidence - 0.90).abs() < 1e-9);
    }

    #[test]
    fn load_project_absent_returns_none() {
        let tmp = tempdir().unwrap();
        let out = load_project(tmp.path()).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn load_project_valid() {
        let tmp = tempdir().unwrap();
        let yaml = r#"
schema_version: "0.2"
defaults:
  tier: 3
  min_confidence: 0.95
  max_passes: 7
  model: opus
  budget_usd: 4.0
  cost_cap_behavior: halt
"#;
        write(&tmp.path().join(".pice/workflow.yaml"), yaml);
        let cfg = load_project(tmp.path()).unwrap().unwrap();
        assert_eq!(cfg.defaults.tier, 3);
        assert_eq!(cfg.defaults.model, "opus");
    }

    #[test]
    fn load_project_bad_schema_version() {
        let tmp = tempdir().unwrap();
        let yaml = r#"
schema_version: "0.1"
defaults:
  tier: 2
  min_confidence: 0.9
  max_passes: 5
  model: sonnet
  budget_usd: 2.0
  cost_cap_behavior: halt
"#;
        write(&tmp.path().join(".pice/workflow.yaml"), yaml);
        let err = load_project(tmp.path()).unwrap_err().to_string();
        assert!(
            err.contains("0.1"),
            "error should cite the invalid version: {err}"
        );
        assert!(
            err.contains("0.2"),
            "error should cite the expected version: {err}"
        );
    }

    #[test]
    fn load_project_rejects_unknown_top_level_fields() {
        // Stale or misspelled field names must be rejected at parse time,
        // not silently dropped. `phases.review` was removed from the
        // schema; a workflow still carrying it is a stale config that must
        // not validate cleanly (runtime would silently ignore the setting).
        let tmp = tempdir().unwrap();
        let yaml = r#"
schema_version: "0.2"
defaults:
  tier: 2
  min_confidence: 0.9
  max_passes: 5
  model: sonnet
  budget_usd: 2.0
  cost_cap_behavior: halt
phases:
  review:
    enabled: true
    trigger: "always"
"#;
        write(&tmp.path().join(".pice/workflow.yaml"), yaml);
        let err = load_project(tmp.path())
            .expect_err("expected deny_unknown_fields to reject stale phases.review");
        // Use `{:#}` to include the full error chain — the serde detail is
        // wrapped by anyhow's top-level context.
        let chain = format!("{err:#}");
        assert!(
            chain.contains("review") || chain.contains("unknown"),
            "error chain should flag the unknown field: {chain}"
        );
    }

    #[test]
    fn load_project_malformed_yaml() {
        let tmp = tempdir().unwrap();
        write(
            &tmp.path().join(".pice/workflow.yaml"),
            "schema_version: \"0.2\"\ndefaults: [this is not a map",
        );
        let err = load_project(tmp.path()).unwrap_err().to_string();
        assert!(
            err.contains("parse"),
            "error should mention parse failure: {err}"
        );
    }

    #[test]
    fn load_user_missing_home_var_errors() {
        let prev_home = std::env::var_os("HOME");
        let prev_profile = std::env::var_os("USERPROFILE");
        // SAFETY: test mutates process env; serial only within this test.
        unsafe {
            std::env::remove_var("HOME");
            std::env::remove_var("USERPROFILE");
        }
        let err = load_user().unwrap_err().to_string();
        // SAFETY: restore env before other tests run.
        unsafe {
            if let Some(v) = prev_home {
                std::env::set_var("HOME", v);
            }
            if let Some(v) = prev_profile {
                std::env::set_var("USERPROFILE", v);
            }
        }
        assert!(err.contains("home"), "error should mention home: {err}");
    }

    #[test]
    fn resolve_no_overrides_returns_framework() {
        let tmp = tempdir().unwrap();
        let cfg = resolve(tmp.path()).unwrap();
        assert_eq!(cfg, embedded_defaults());
    }

    #[test]
    fn workflow_config_roundtrips_through_yaml() {
        // Contract criterion #1: serialize a fully-populated WorkflowConfig,
        // deserialize, assert equality. Catches `skip_serializing_if` data
        // loss and serde field-name drift across the full schema surface.
        use crate::adaptive::types::{AdtsConfig, SprtConfig, VecConfig};
        use crate::workflow::schema::{
            AdaptiveAlgo, CostCapBehavior, Defaults, EvaluatePhase, ExecutePhase, LayerOverride,
            OnTimeout, PhaseConfig, Phases, RetryConfig, ReviewConfig, WorkflowConfig,
        };
        use std::collections::BTreeMap;

        let mut layer_overrides: BTreeMap<String, LayerOverride> = BTreeMap::new();
        layer_overrides.insert(
            "backend".into(),
            LayerOverride {
                tier: Some(3),
                min_confidence: Some(0.97),
                max_passes: Some(8),
                budget_usd: Some(0.9),
                require_review: Some(true),
                trigger: Some("confidence < 0.95".into()),
                adaptive_algorithm: Some(AdaptiveAlgo::None),
            },
        );

        let mut model_override: BTreeMap<String, String> = BTreeMap::new();
        model_override.insert("backend".into(), "opus".into());

        let mut seams: BTreeMap<String, Vec<String>> = BTreeMap::new();
        seams.insert(
            "backend-frontend".into(),
            vec!["contract_type_sync".into(), "error_shape".into()],
        );

        let original = WorkflowConfig {
            schema_version: "0.2".into(),
            defaults: Defaults {
                tier: 3,
                min_confidence: 0.95,
                max_passes: 7,
                model: "opus".into(),
                budget_usd: 3.0,
                cost_cap_behavior: CostCapBehavior::Warn,
                max_parallelism: Some(4),
            },
            phases: Phases {
                plan: PhaseConfig {
                    description: Some("Planning".into()),
                    output: Some(".claude/plans/{feature}.md".into()),
                },
                execute: ExecutePhase {
                    description: Some("Executing".into()),
                    parallel: true,
                    worktree_isolation: true,
                    retry: RetryConfig {
                        max_attempts: 4,
                        fresh_context: true,
                    },
                },
                evaluate: EvaluatePhase {
                    description: Some("Evaluating".into()),
                    parallel: true,
                    seam_checks: true,
                    adaptive_algorithm: AdaptiveAlgo::Adts,
                    sprt: SprtConfig {
                        prior_alpha: 2.0,
                        prior_beta: 1.5,
                        accept_threshold: 20.0,
                        reject_threshold: 0.05,
                    },
                    adts: AdtsConfig {
                        divergence_threshold: 3.0,
                        max_divergence_escalations: 1,
                    },
                    vec: VecConfig {
                        entropy_floor: 0.02,
                    },
                    model_override,
                },
            },
            layer_overrides,
            review: Some(ReviewConfig {
                enabled: true,
                trigger: Some("tier >= 3 OR layer == infrastructure".into()),
                timeout_hours: 48,
                on_timeout: OnTimeout::Reject,
                notification: "stdout".into(),
            }),
            seams: Some(seams),
        };

        let yaml = serde_yaml::to_string(&original).expect("serialize");
        let roundtripped: WorkflowConfig = serde_yaml::from_str(&yaml).expect("deserialize");
        assert_eq!(
            original, roundtripped,
            "WorkflowConfig roundtrip lost data; yaml was:\n{yaml}"
        );
    }

    #[test]
    fn resolve_project_overrides_framework() {
        let tmp = tempdir().unwrap();
        // Project strictens every floor-guarded field (tier, min_confidence,
        // max_passes up; budget_usd down) — all valid direction.
        let yaml = r#"
schema_version: "0.2"
defaults:
  tier: 3
  min_confidence: 0.95
  max_passes: 7
  model: opus
  budget_usd: 1.5
  cost_cap_behavior: halt
"#;
        write(&tmp.path().join(".pice/workflow.yaml"), yaml);
        let cfg = resolve(tmp.path()).unwrap();
        assert_eq!(cfg.defaults.tier, 3);
        assert_eq!(cfg.defaults.model, "opus");
        assert!((cfg.defaults.budget_usd - 1.5).abs() < 1e-9);
    }
}

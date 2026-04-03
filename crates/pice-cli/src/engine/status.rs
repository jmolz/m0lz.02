use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;
use std::process::Command;

use super::plan_parser::ParsedPlan;
use crate::metrics::db::MetricsDb;
use crate::metrics::store;

#[derive(Debug, Clone, Serialize)]
pub struct PlanStatus {
    pub path: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<u8>,
    pub criteria_count: usize,
    pub has_contract: bool,
    /// Non-empty when the plan file exists but could not be fully parsed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_error: Option<String>,
    /// Latest evaluation result for this plan (if metrics DB is available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_evaluation: Option<LastEvaluation>,
}

/// Serializable summary of the most recent evaluation for a plan.
#[derive(Debug, Clone, Serialize)]
pub struct LastEvaluation {
    pub passed: bool,
    pub avg_score: f64,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectStatus {
    pub plans: Vec<PlanStatus>,
    pub branch: String,
    pub has_uncommitted_changes: bool,
    pub last_commit: String,
}

/// Scan the project root without metrics enrichment.
/// Used by tests and as a convenience wrapper.
#[allow(dead_code)]
pub fn scan_project(project_root: &Path) -> Result<ProjectStatus> {
    scan_project_with_metrics(project_root, None)
}

/// Scan with optional metrics DB for evaluation enrichment.
pub fn scan_project_with_metrics(
    project_root: &Path,
    metrics_db: Option<&MetricsDb>,
) -> Result<ProjectStatus> {
    let plans = scan_plans(project_root, metrics_db)?;
    let branch = get_branch_name(project_root);
    let has_uncommitted_changes = has_uncommitted_changes(project_root);
    let last_commit = get_last_commit(project_root);

    Ok(ProjectStatus {
        plans,
        branch,
        has_uncommitted_changes,
        last_commit,
    })
}

fn scan_plans(project_root: &Path, metrics_db: Option<&MetricsDb>) -> Result<Vec<PlanStatus>> {
    let plans_dir = project_root.join(".claude/plans");
    if !plans_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut plans = Vec::new();
    for entry in std::fs::read_dir(&plans_dir)
        .with_context(|| format!("failed to read {}", plans_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        match ParsedPlan::load(&path) {
            Ok(plan) => {
                let (tier, criteria_count, has_contract) = match &plan.contract {
                    Some(contract) => (Some(contract.tier), contract.criteria.len(), true),
                    None => (None, 0, false),
                };
                let plan_path = format!(
                    ".claude/plans/{}",
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown")
                );

                let last_evaluation = metrics_db
                    .and_then(|db| store::get_latest_evaluation(db, &plan_path).ok().flatten())
                    .map(|e| LastEvaluation {
                        passed: e.passed,
                        avg_score: e.avg_score,
                        timestamp: e.timestamp,
                    });

                plans.push(PlanStatus {
                    path: plan_path,
                    title: plan.title,
                    tier,
                    criteria_count,
                    has_contract,
                    parse_error: None,
                    last_evaluation,
                });
            }
            Err(e) => {
                let plan_path = format!(
                    ".claude/plans/{}",
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown")
                );
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "plan file has parse errors"
                );
                plans.push(PlanStatus {
                    path: plan_path,
                    title: path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    tier: None,
                    criteria_count: 0,
                    has_contract: false,
                    parse_error: Some(e.to_string()),
                    last_evaluation: None,
                });
            }
        }
    }

    plans.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(plans)
}

fn get_branch_name(project_root: &Path) -> String {
    Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(project_root)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_default()
}

fn has_uncommitted_changes(project_root: &Path) -> bool {
    Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(project_root)
        .output()
        .ok()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

fn get_last_commit(project_root: &Path) -> String {
    Command::new("git")
        .args(["log", "-1", "--oneline"])
        .current_dir(project_root)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_project_no_plans_dir() {
        let dir = tempfile::tempdir().unwrap();
        let status = scan_project(dir.path()).unwrap();
        assert!(status.plans.is_empty());
    }

    #[test]
    fn scan_project_with_plans() {
        let dir = tempfile::tempdir().unwrap();
        let plans_dir = dir.path().join(".claude/plans");
        std::fs::create_dir_all(&plans_dir).unwrap();
        std::fs::write(
            plans_dir.join("test-plan.md"),
            "# Feature: Test Plan\n\n## Overview\nA test.\n",
        )
        .unwrap();

        let status = scan_project(dir.path()).unwrap();
        assert_eq!(status.plans.len(), 1);
        assert_eq!(status.plans[0].title, "Feature: Test Plan");
        assert!(!status.plans[0].has_contract);
    }

    #[test]
    fn scan_project_with_contract() {
        let dir = tempfile::tempdir().unwrap();
        let plans_dir = dir.path().join(".claude/plans");
        std::fs::create_dir_all(&plans_dir).unwrap();
        std::fs::write(
            plans_dir.join("contract-plan.md"),
            r#"# Feature: Contract Plan

## Contract

```json
{
  "feature": "Test",
  "tier": 2,
  "pass_threshold": 7,
  "criteria": [
    { "name": "Tests pass", "threshold": 7, "validation": "cargo test" },
    { "name": "Lint clean", "threshold": 8, "validation": "cargo clippy" }
  ]
}
```
"#,
        )
        .unwrap();

        let status = scan_project(dir.path()).unwrap();
        assert_eq!(status.plans.len(), 1);
        assert!(status.plans[0].has_contract);
        assert_eq!(status.plans[0].tier, Some(2));
        assert_eq!(status.plans[0].criteria_count, 2);
    }

    #[test]
    fn scan_project_with_malformed_plan() {
        let dir = tempfile::tempdir().unwrap();
        let plans_dir = dir.path().join(".claude/plans");
        std::fs::create_dir_all(&plans_dir).unwrap();
        std::fs::write(
            plans_dir.join("bad-plan.md"),
            "# Bad Plan\n\n## Contract\n\n```json\n{invalid}\n```\n",
        )
        .unwrap();

        let status = scan_project(dir.path()).unwrap();
        assert_eq!(status.plans.len(), 1);
        assert!(status.plans[0].parse_error.is_some());
        assert_eq!(status.plans[0].title, "bad-plan");
    }

    #[test]
    fn scan_project_git_info() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@test.com",
                "commit",
                "--allow-empty",
                "-m",
                "initial commit",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let status = scan_project(dir.path()).unwrap();
        assert!(!status.branch.is_empty());
        assert!(!status.has_uncommitted_changes);
        assert!(status.last_commit.contains("initial commit"));
    }
}

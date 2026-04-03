use anyhow::Result;
use clap::Args;

use crate::engine::status;
use crate::metrics;

#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: &StatusArgs) -> Result<()> {
    let project_root = std::env::current_dir()?;

    // Open metrics DB if available (for evaluation enrichment)
    let db = metrics::open_metrics_db(&project_root).ok().flatten();
    let project_status = status::scan_project_with_metrics(&project_root, db.as_ref())?;

    if args.json {
        let output = serde_json::to_string_pretty(&project_status)?;
        println!("{output}");
    } else {
        // Branch and last commit
        if !project_status.branch.is_empty() {
            println!("Branch: {}", project_status.branch);
        }
        if !project_status.last_commit.is_empty() {
            println!("Last commit: {}", project_status.last_commit);
        }
        if project_status.has_uncommitted_changes {
            println!("Warning: uncommitted changes detected");
        }
        println!();

        // Plans table
        if project_status.plans.is_empty() {
            println!("No plans found.");
        } else {
            println!(
                "{:<40} {:<6} {:<10} {:<10}",
                "Plan", "Tier", "Criteria", "Last Eval"
            );
            println!("{}", "\u{2500}".repeat(66));
            for plan in &project_status.plans {
                if let Some(err) = &plan.parse_error {
                    println!(
                        "{:<40} {:<6} ERROR: {}",
                        truncate_str(&plan.path, 40),
                        "-",
                        truncate_str(err, 40),
                    );
                } else {
                    let tier = plan
                        .tier
                        .map(|t| t.to_string())
                        .unwrap_or_else(|| "-".to_string());
                    let eval_str = match &plan.last_evaluation {
                        Some(e) => {
                            let status = if e.passed { "PASS" } else { "FAIL" };
                            format!("{status} {:.1}", e.avg_score)
                        }
                        None => "--".to_string(),
                    };
                    println!(
                        "{:<40} {:<6} {:<10} {:<10}",
                        truncate_str(&plan.title, 40),
                        tier,
                        plan.criteria_count,
                        eval_str,
                    );
                }
            }
        }
    }

    Ok(())
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

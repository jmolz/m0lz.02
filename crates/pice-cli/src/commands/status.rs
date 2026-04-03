use anyhow::Result;
use clap::Args;

use crate::engine::status;

#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: &StatusArgs) -> Result<()> {
    let project_root = std::env::current_dir()?;
    let project_status = status::scan_project(&project_root)?;

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
            println!("{:<40} {:<6} {:<10}", "Plan", "Tier", "Criteria");
            println!("{}", "-".repeat(56));
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
                    println!(
                        "{:<40} {:<6} {:<10}",
                        truncate_str(&plan.title, 40),
                        tier,
                        plan.criteria_count,
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

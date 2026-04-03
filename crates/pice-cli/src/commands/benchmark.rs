use anyhow::Result;
use clap::Args;
use serde::Serialize;
use std::path::Path;
use std::process::Command;

use crate::metrics;

#[derive(Args, Debug)]
pub struct BenchmarkArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Serialize)]
struct BenchmarkReport {
    git: GitStats,
    pice: PiceStats,
    coverage_pct: f64,
}

#[derive(Debug, Serialize)]
struct GitStats {
    total_commits: u64,
    commits_30d: u64,
    avg_insertions: f64,
    avg_deletions: f64,
}

#[derive(Debug, Serialize)]
struct PiceStats {
    evaluations_30d: u64,
    plans_evaluated_30d: u64,
    evaluation_pass_rate: f64,
    avg_score: f64,
    plans_with_evaluations: u64,
    total_plans: u64,
}

pub async fn run(args: &BenchmarkArgs) -> Result<()> {
    let project_root = std::env::current_dir()?;

    let git = collect_git_stats(&project_root);
    let pice = collect_pice_stats(&project_root);
    // Coverage: distinct plans evaluated in the last 30 days / commits.
    // Use plans_evaluated (not raw evaluation count) to avoid inflation from retries.
    // Cap at 100% — multiple plans per commit is valid but coverage can't exceed full.
    let coverage_pct = if git.commits_30d > 0 {
        let raw = (pice.plans_evaluated_30d as f64 / git.commits_30d as f64) * 100.0;
        raw.min(100.0)
    } else {
        0.0
    };

    let report = BenchmarkReport {
        git,
        pice,
        coverage_pct,
    };

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_benchmark(&report);
    }

    Ok(())
}

fn collect_git_stats(project_root: &Path) -> GitStats {
    let total_commits = Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .current_dir(project_root)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .parse::<u64>()
                    .ok()
            } else {
                None
            }
        })
        .unwrap_or(0);

    // Last 30 days commits
    let commits_30d = Command::new("git")
        .args(["rev-list", "--count", "--since=30.days", "HEAD"])
        .current_dir(project_root)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .parse::<u64>()
                    .ok()
            } else {
                None
            }
        })
        .unwrap_or(0);

    // Shortstat for last 30 days
    let (avg_insertions, avg_deletions) = if commits_30d > 0 {
        let shortstat = Command::new("git")
            .args(["log", "--since=30.days", "--pretty=tformat:", "--shortstat"])
            .current_dir(project_root)
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();

        let (mut total_ins, mut total_del) = (0u64, 0u64);
        for line in shortstat.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Parse "N insertions(+), M deletions(-)"
            for part in line.split(',') {
                let part = part.trim();
                if part.contains("insertion") {
                    if let Some(n) = part
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse::<u64>().ok())
                    {
                        total_ins += n;
                    }
                } else if part.contains("deletion") {
                    if let Some(n) = part
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse::<u64>().ok())
                    {
                        total_del += n;
                    }
                }
            }
        }
        (
            total_ins as f64 / commits_30d as f64,
            total_del as f64 / commits_30d as f64,
        )
    } else {
        (0.0, 0.0)
    };

    GitStats {
        total_commits,
        commits_30d,
        avg_insertions,
        avg_deletions,
    }
}

fn collect_pice_stats(project_root: &Path) -> PiceStats {
    let db = match metrics::open_metrics_db(project_root).ok().flatten() {
        Some(db) => db,
        None => {
            return PiceStats {
                evaluations_30d: 0,
                plans_evaluated_30d: 0,
                evaluation_pass_rate: 0.0,
                avg_score: 0.0,
                plans_with_evaluations: 0,
                total_plans: 0,
            }
        }
    };

    let report = metrics::aggregator::aggregate(&db).unwrap_or_else(|_| {
        metrics::aggregator::MetricsReport {
            total_evaluations: 0,
            total_loops: 0,
            pass_rate: 0.0,
            avg_score: 0.0,
            last_30_days: metrics::aggregator::TrendData {
                evaluations: 0,
                distinct_plans: 0,
                pass_rate: 0.0,
                avg_score: 0.0,
            },
            tier_distribution: metrics::aggregator::TierDistribution {
                tier1: 0,
                tier2: 0,
                tier3: 0,
            },
            top_failing_criteria: Vec::new(),
        }
    });

    // Count total plans from filesystem
    let plans_dir = project_root.join(".claude/plans");
    let total_plans = if plans_dir.is_dir() {
        std::fs::read_dir(&plans_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("md"))
                    .count() as u64
            })
            .unwrap_or(0)
    } else {
        0
    };

    PiceStats {
        evaluations_30d: report.last_30_days.evaluations,
        plans_evaluated_30d: report.last_30_days.distinct_plans,
        evaluation_pass_rate: report.last_30_days.pass_rate,
        avg_score: report.last_30_days.avg_score,
        plans_with_evaluations: report.total_loops,
        total_plans,
    }
}

fn print_benchmark(report: &BenchmarkReport) {
    println!("PICE Benchmark Report");
    println!("\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}");
    println!();
    println!("Git Activity (last 30 days):");
    println!("  Commits:             {:>5}", report.git.commits_30d);
    println!(
        "  Avg insertions/commit: {:>3.0}",
        report.git.avg_insertions
    );
    println!("  Avg deletions/commit:  {:>3.0}", report.git.avg_deletions);
    println!();
    println!("PICE Metrics (last 30 days):");
    println!("  Evaluations:         {:>5}", report.pice.evaluations_30d);
    println!(
        "  Plans evaluated:     {:>5}",
        report.pice.plans_evaluated_30d
    );
    println!(
        "  Evaluation pass rate: {:>4.1}%",
        report.pice.evaluation_pass_rate
    );
    println!("  Average score:        {:>4.1}/10", report.pice.avg_score);
    println!(
        "  Plans with evaluations: {}/{}",
        report.pice.plans_with_evaluations, report.pice.total_plans
    );
    println!();
    println!("Coverage:");
    println!(
        "  Plans evaluated/commits: {}/{} ({:.1}%)",
        report.pice.plans_evaluated_30d, report.git.commits_30d, report.coverage_pct
    );
}

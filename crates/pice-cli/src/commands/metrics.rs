use anyhow::Result;
use clap::Args;

use crate::metrics;

#[derive(Args, Debug)]
pub struct MetricsArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Output as CSV
    #[arg(long)]
    pub csv: bool,
}

pub async fn run(args: &MetricsArgs) -> Result<()> {
    let project_root = std::env::current_dir()?;

    let db = match metrics::open_metrics_db(&project_root)? {
        Some(db) => db,
        None => {
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({"error": "no metrics database -- run `pice init` first"})
                    )?
                );
            } else {
                println!("No metrics database found. Run `pice init` to initialize.");
            }
            return Ok(());
        }
    };

    if args.csv {
        let csv = metrics::aggregator::format_csv(&db)?;
        print!("{csv}");
    } else {
        let report = metrics::aggregator::aggregate(&db)?;
        if args.json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            print!("{}", metrics::aggregator::format_table(&report));
        }
    }

    Ok(())
}

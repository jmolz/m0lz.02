use anyhow::Result;
use clap::Args;
use tracing::info;

use crate::engine::output;
use crate::metrics;
use pice_core::config::PiceConfig;
use pice_daemon::orchestrator::{session, ProviderOrchestrator};
use pice_daemon::prompt;

#[derive(Args, Debug)]
pub struct PlanArgs {
    /// Description of what to plan
    pub description: String,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: &PlanArgs) -> Result<()> {
    let project_root = std::env::current_dir()?;

    let config_path = project_root.join(".pice/config.toml");
    let config = PiceConfig::load(&config_path).unwrap_or_else(|_| PiceConfig::default());

    let plan_prompt = prompt::build_plan_prompt(&args.description, &project_root)?;

    if !args.json {
        println!("Planning: {}", args.description);
        println!();
    }

    info!(provider = %config.provider.name, "starting provider for planning");
    let mut orchestrator = ProviderOrchestrator::start(&config.provider.name, &config).await?;

    if !args.json {
        orchestrator.on_notification(session::streaming_handler(output::terminal_sink()));
    }

    let session_result = session::run_session(&mut orchestrator, &project_root, plan_prompt).await;
    if let Err(e) = orchestrator.shutdown().await {
        tracing::warn!("provider shutdown failed: {e}");
    }
    session_result?;

    // Record plan event (non-fatal)
    if let Ok(Some(db)) = metrics::open_metrics_db(&project_root) {
        if let Err(e) = metrics::store::record_loop_event(
            &db,
            "plan_created",
            None,
            Some(&serde_json::json!({ "description": args.description }).to_string()),
        ) {
            tracing::warn!("failed to record plan event: {e}");
        }
    }

    if args.json {
        let output = serde_json::json!({
            "status": "complete",
            "description": args.description,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("\n\nPlanning session complete.");
    }

    Ok(())
}

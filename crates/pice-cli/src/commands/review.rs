use anyhow::Result;
use clap::Args;
use tracing::info;

use crate::engine::output;
use pice_core::config::PiceConfig;
use pice_daemon::orchestrator::{session, ProviderOrchestrator};
use pice_daemon::prompt;

#[derive(Args, Debug)]
pub struct ReviewArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: &ReviewArgs) -> Result<()> {
    let project_root = std::env::current_dir()?;

    let config_path = project_root.join(".pice/config.toml");
    let config = PiceConfig::load(&config_path).unwrap_or_else(|_| PiceConfig::default());

    let review_prompt = prompt::build_review_prompt(&project_root)?;

    if !args.json {
        println!("Running code review...");
        println!();
    }

    info!(provider = %config.provider.name, "starting provider for review");
    let mut orchestrator = ProviderOrchestrator::start(&config.provider.name, &config).await?;

    if !args.json {
        orchestrator.on_notification(session::streaming_handler(output::terminal_sink()));
    }

    let session_result =
        session::run_session(&mut orchestrator, &project_root, review_prompt).await;
    if let Err(e) = orchestrator.shutdown().await {
        tracing::warn!("provider shutdown failed: {e}");
    }
    session_result?;

    if args.json {
        let output = serde_json::json!({ "status": "complete" });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("\n\nReview complete.");
    }

    Ok(())
}

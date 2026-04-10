use anyhow::Result;
use clap::Args;
use std::path::PathBuf;
use tracing::info;

use crate::engine::output;
use crate::metrics;
use pice_core::config::PiceConfig;
use pice_core::plan_parser;
use pice_daemon::orchestrator::{session, ProviderOrchestrator};
use pice_daemon::prompt;

#[derive(Args, Debug)]
pub struct ExecuteArgs {
    /// Path to the plan file to execute
    pub plan_path: PathBuf,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: &ExecuteArgs) -> Result<()> {
    let project_root = std::env::current_dir()?;

    let plan = plan_parser::ParsedPlan::load(&args.plan_path)?;

    if !args.json {
        println!("Executing: {}", plan.title);
        println!();
    }

    let config_path = project_root.join(".pice/config.toml");
    let config = PiceConfig::load(&config_path).unwrap_or_else(|_| PiceConfig::default());

    let exec_prompt = prompt::build_execute_prompt(&plan.content, &project_root)?;

    // Record execute_started event (non-fatal)
    let normalized_path = metrics::normalize_plan_path(&plan.path, &project_root);
    if let Ok(Some(db)) = metrics::open_metrics_db(&project_root) {
        if let Err(e) =
            metrics::store::record_loop_event(&db, "execute_started", Some(&normalized_path), None)
        {
            tracing::warn!("failed to record execute_started event: {e}");
        }
    }

    info!(provider = %config.provider.name, "starting provider for execution");
    let mut orchestrator = ProviderOrchestrator::start(&config.provider.name, &config).await?;

    if !args.json {
        orchestrator.on_notification(session::streaming_handler(output::terminal_sink()));
    }

    let session_result = session::run_session(&mut orchestrator, &project_root, exec_prompt).await;
    if let Err(e) = orchestrator.shutdown().await {
        tracing::warn!("provider shutdown failed: {e}");
    }
    session_result?;

    // Record execute_completed event (non-fatal)
    if let Ok(Some(db)) = metrics::open_metrics_db(&project_root) {
        if let Err(e) = metrics::store::record_loop_event(
            &db,
            "execute_completed",
            Some(&normalized_path),
            None,
        ) {
            tracing::warn!("failed to record execute_completed event: {e}");
        }
    }

    if args.json {
        let output = serde_json::json!({
            "status": "complete",
            "plan": plan.title,
            "planPath": plan.path,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("\n\nExecution complete.");
    }

    Ok(())
}

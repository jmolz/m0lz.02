use anyhow::Result;
use clap::Args;
use std::path::PathBuf;
use tracing::info;

use crate::engine::output;
use pice_core::config::PiceConfig;
use pice_daemon::orchestrator::{session, NullSink, ProviderOrchestrator, SharedSink};
use pice_daemon::prompt;
use std::sync::Arc;

#[derive(Args, Debug)]
pub struct HandoffArgs {
    /// Custom output path (default: HANDOFF.md in project root)
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: &HandoffArgs) -> Result<()> {
    let project_root = std::env::current_dir()?;

    let config_path = project_root.join(".pice/config.toml");
    let config = PiceConfig::load(&config_path).unwrap_or_else(|_| PiceConfig::default());

    let handoff_prompt = prompt::build_handoff_prompt(&project_root)?;

    if !args.json {
        println!("Generating session handoff...");
        println!();
    }

    info!(provider = %config.provider.name, "starting provider for handoff");
    let mut orchestrator = ProviderOrchestrator::start(&config.provider.name, &config).await?;

    // JSON mode: capture silently so stdout stays clean.
    // Text mode: stream to terminal AND capture for the handoff file.
    let sink: SharedSink = if args.json {
        Arc::new(NullSink)
    } else {
        output::terminal_sink()
    };
    let session_result =
        session::run_session_and_capture(&mut orchestrator, &project_root, handoff_prompt, sink)
            .await;
    if let Err(e) = orchestrator.shutdown().await {
        tracing::warn!("provider shutdown failed: {e}");
    }
    let captured = session_result?;

    // Write handoff file
    let output_path = args
        .output
        .clone()
        .unwrap_or_else(|| project_root.join("HANDOFF.md"));
    std::fs::write(&output_path, &captured)?;

    if args.json {
        let output = serde_json::json!({
            "status": "complete",
            "path": output_path.to_string_lossy(),
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("\n\nHandoff written to {}", output_path.display());
    }

    Ok(())
}

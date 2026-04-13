//! `pice handoff` handler — generate handoff document.

use anyhow::Result;
use pice_core::cli::{CommandResponse, HandoffRequest};
use serde_json::json;

use super::to_shared_sink;
use crate::orchestrator::session;
use crate::orchestrator::{NullSink, ProviderOrchestrator, SharedSink, StreamSink};
use crate::prompt::builders;
use crate::server::router::DaemonContext;

pub async fn run(
    req: HandoffRequest,
    ctx: &DaemonContext,
    sink: &dyn StreamSink,
) -> Result<CommandResponse> {
    let project_root = ctx.project_root();
    let config = ctx.config();
    let prompt = builders::build_handoff_prompt(project_root)?;

    let mut orchestrator = ProviderOrchestrator::start(&config.provider.name, config).await?;

    // Stream and capture: in text mode, handoff streams to the terminal while
    // collecting text. In JSON mode, use NullSink to keep stdout clean.
    let shared: SharedSink = if req.json {
        std::sync::Arc::new(NullSink)
    } else {
        // SAFETY INVARIANT: the session is awaited to completion before this
        // handler returns, so the Arc from to_shared_sink is dropped while
        // the borrowed `sink` is still alive.
        to_shared_sink(sink)
    };
    let captured =
        session::run_session_and_capture(&mut orchestrator, project_root, prompt, shared).await;
    orchestrator.shutdown().await.ok();
    let handoff_content = captured?;

    // Write handoff file
    let output_path = req
        .output
        .map(|p| {
            if p.is_absolute() {
                p
            } else {
                project_root.join(p)
            }
        })
        .unwrap_or_else(|| project_root.join("HANDOFF.md"));

    std::fs::write(&output_path, &handoff_content)?;

    let relative_path = output_path
        .strip_prefix(project_root)
        .unwrap_or(&output_path)
        .to_string_lossy()
        .to_string();

    if req.json {
        Ok(CommandResponse::Json {
            value: json!({"status": "complete", "path": relative_path}),
        })
    } else {
        Ok(CommandResponse::Text {
            content: format!("Handoff written to {relative_path}\n"),
        })
    }
}

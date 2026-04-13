//! `pice review` handler — AI code review of current changes.

use anyhow::Result;
use pice_core::cli::{CommandResponse, ReviewRequest};
use serde_json::json;

use super::to_shared_sink;
use crate::orchestrator::session::{self, streaming_handler};
use crate::orchestrator::{ProviderOrchestrator, StreamSink};
use crate::prompt::builders;
use crate::server::router::DaemonContext;

pub async fn run(
    req: ReviewRequest,
    ctx: &DaemonContext,
    sink: &dyn StreamSink,
) -> Result<CommandResponse> {
    let project_root = ctx.project_root();
    let config = ctx.config();
    let prompt = builders::build_review_prompt(project_root)?;

    let mut orchestrator = ProviderOrchestrator::start(&config.provider.name, config).await?;
    if !req.json {
        // SAFETY INVARIANT: session is awaited to completion before this handler
        // returns, so the Arc from to_shared_sink is dropped while `sink` is alive.
        orchestrator.on_notification(streaming_handler(to_shared_sink(sink)));
    }

    let result = session::run_session(&mut orchestrator, project_root, prompt).await;
    orchestrator.shutdown().await.ok();
    result?;

    if req.json {
        Ok(CommandResponse::Json {
            value: json!({"status": "complete"}),
        })
    } else {
        Ok(CommandResponse::Empty)
    }
}

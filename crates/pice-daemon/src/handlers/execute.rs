//! `pice execute` handler — run a plan through the provider.

use anyhow::Result;
use pice_core::cli::{CommandResponse, ExecuteRequest};
use pice_core::plan_parser::ParsedPlan;
use serde_json::json;

use super::to_shared_sink;
use crate::orchestrator::session::{self, streaming_handler};
use crate::orchestrator::{ProviderOrchestrator, StreamSink};
use crate::prompt::builders;
use crate::server::router::DaemonContext;

pub async fn run(
    req: ExecuteRequest,
    ctx: &DaemonContext,
    sink: &dyn StreamSink,
) -> Result<CommandResponse> {
    let project_root = ctx.project_root();
    let config = ctx.config();

    // Resolve plan path (relative paths are resolved against project root)
    let plan_path = if req.plan_path.is_absolute() {
        req.plan_path.clone()
    } else {
        project_root.join(&req.plan_path)
    };

    if !plan_path.exists() {
        return Ok(CommandResponse::Exit {
            code: 1,
            message: format!("plan file not found: {}", plan_path.display()),
        });
    }

    let plan = ParsedPlan::load(&plan_path)?;
    let prompt = builders::build_execute_prompt(&plan.content, project_root)?;

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
            value: json!({"status": "complete", "plan": plan.title}),
        })
    } else {
        Ok(CommandResponse::Empty)
    }
}

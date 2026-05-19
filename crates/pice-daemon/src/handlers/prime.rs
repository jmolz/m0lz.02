//! `pice prime` handler — orient AI on the codebase.

use anyhow::Result;
use pice_core::cli::{CommandResponse, PrimeRequest};
use serde_json::json;

use super::to_shared_sink;
use crate::memory::recall::{build_memory_recall, record_read_metrics, MemoryReadMetrics};
use crate::memory::store::MemoryPaths;
use crate::orchestrator::session::{self, streaming_handler};
use crate::orchestrator::{ProviderOrchestrator, StreamSink};
use crate::prompt::builders;
use crate::server::router::DaemonContext;

pub async fn run(
    req: PrimeRequest,
    ctx: &DaemonContext,
    sink: &dyn StreamSink,
) -> Result<CommandResponse> {
    let project_root = ctx.project_root();
    let config = ctx.config();
    let memory = if config
        .memory
        .policy()
        .can_read(pice_core::memory::MemoryConsumer::Prime)
    {
        let state_dir = pice_core::layers::manifest::VerificationManifest::state_dir()?;
        let paths = MemoryPaths::new(project_root, &state_dir);
        let recall = build_memory_recall(
            &paths,
            &config.memory,
            pice_core::memory::MemoryConsumer::Prime,
            None,
            None,
            &chrono::Utc::now().to_rfc3339(),
        )?;
        record_read_metrics(
            project_root,
            &paths,
            MemoryReadMetrics {
                consumer: pice_core::memory::MemoryConsumer::Prime,
                feature_id: None,
                plan_path: None,
                run_id: None,
                brief: recall.brief.as_ref(),
                warning: recall.warning,
            },
        );
        recall.brief
    } else {
        None
    };
    let prompt = builders::build_prime_prompt_with_memory(
        project_root,
        &config.provider.name,
        memory.as_ref(),
    )?;

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

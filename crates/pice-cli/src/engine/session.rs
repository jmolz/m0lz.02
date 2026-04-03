use anyhow::{Context, Result};
use pice_protocol::{methods, SessionCreateParams, SessionDestroyParams, SessionSendParams};
use std::path::Path;
use std::sync::{Arc, Mutex};

use super::orchestrator::ProviderOrchestrator;
use super::output;
use crate::provider::host::NotificationHandler;

/// Create a notification handler that prints response chunks to stdout.
/// Use this for commands that stream AI output in text mode.
pub fn streaming_handler() -> NotificationHandler {
    Box::new(|method, params| {
        if method == methods::RESPONSE_CHUNK {
            if let Some(params) = params {
                if let Some(text) = params.get("text").and_then(|t| t.as_str()) {
                    output::print_chunk(text);
                }
            }
        }
    })
}

/// Run a full session lifecycle: create → send prompt → destroy.
///
/// This is the common pattern used by prime, review, plan, and execute.
/// The caller is responsible for registering a notification handler
/// (for streaming) before calling this function.
pub async fn run_session(
    orchestrator: &mut ProviderOrchestrator,
    project_root: &Path,
    prompt: String,
) -> Result<()> {
    let create_params = serde_json::to_value(SessionCreateParams {
        working_directory: project_root.to_string_lossy().to_string(),
        model: None,
        system_prompt: None,
    })?;
    let create_result = orchestrator
        .request(methods::SESSION_CREATE, Some(create_params))
        .await?;
    let session_id = create_result["sessionId"]
        .as_str()
        .context("provider returned session/create without sessionId")?
        .to_string();

    let send_params = serde_json::to_value(SessionSendParams {
        session_id: session_id.clone(),
        message: prompt,
    })?;
    orchestrator
        .request(methods::SESSION_SEND, Some(send_params))
        .await?;

    let destroy_params = serde_json::to_value(SessionDestroyParams {
        session_id: session_id.clone(),
    })?;
    orchestrator
        .request(methods::SESSION_DESTROY, Some(destroy_params))
        .await?;

    Ok(())
}

/// Run a full session lifecycle and capture all response text.
///
/// Registers its own notification handler to collect `response/chunk` text.
/// When `print_chunks` is true, chunks are also printed to stdout in real time.
/// Returns the concatenated captured text.
pub async fn run_session_and_capture(
    orchestrator: &mut ProviderOrchestrator,
    project_root: &Path,
    prompt: String,
    print_chunks: bool,
) -> Result<String> {
    let chunks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let chunks_clone = Arc::clone(&chunks);

    orchestrator.on_notification(Box::new(move |method, params| {
        if method == methods::RESPONSE_CHUNK {
            if let Some(params) = params {
                if let Some(text) = params.get("text").and_then(|t| t.as_str()) {
                    if print_chunks {
                        output::print_chunk(text);
                    }
                    if let Ok(mut guard) = chunks_clone.lock() {
                        guard.push(text.to_string());
                    }
                }
            }
        }
    }));

    run_session(orchestrator, project_root, prompt).await?;

    let captured = chunks
        .lock()
        .map_err(|_| anyhow::anyhow!("failed to acquire chunk lock"))?
        .join("");

    Ok(captured)
}

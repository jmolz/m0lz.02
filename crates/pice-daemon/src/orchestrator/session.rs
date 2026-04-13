use anyhow::{Context, Result};
use pice_protocol::{methods, SessionCreateParams, SessionDestroyParams, SessionSendParams};
use std::path::Path;
use std::sync::{Arc, Mutex};

use super::stream::SharedSink;
use super::ProviderOrchestrator;
use crate::provider::host::NotificationHandler;

/// Create a notification handler that forwards response chunks to a [`SharedSink`].
///
/// The T12-era replacement for the v0.1 `streaming_handler()` that called
/// `pice_cli::engine::output::print_chunk` directly. The sink is captured by
/// move into the `'static` closure stored by `ProviderHost`, which is why we
/// require `SharedSink` (`Arc<dyn StreamSink>`) rather than `&dyn StreamSink`.
///
/// Use this for commands that stream AI output in text mode. Callers still
/// do two-step setup — install the handler, then call [`run_session`] — so
/// that commands which need a different handler shape (e.g., the capture
/// handler in [`run_session_and_capture`]) can install their own.
pub fn streaming_handler(sink: SharedSink) -> NotificationHandler {
    Box::new(move |method, params| {
        if method == methods::RESPONSE_CHUNK {
            if let Some(params) = params {
                if let Some(text) = params.get("text").and_then(|t| t.as_str()) {
                    sink.send_chunk(text);
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
        layer: None,
        layer_paths: None,
        contract_path: None,
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
/// Registers its own notification handler to collect `response/chunk` text
/// and forward each chunk to the supplied sink. The sink controls whether
/// chunks are user-visible — pass `Arc::new(NullSink)` for silent capture
/// (e.g., `pice commit` building a message from the model response) and a
/// `TerminalSink` for the stream-and-capture case (e.g., `pice handoff` in
/// text mode).
///
/// Returns the concatenated captured text regardless of what the sink does.
pub async fn run_session_and_capture(
    orchestrator: &mut ProviderOrchestrator,
    project_root: &Path,
    prompt: String,
    sink: SharedSink,
) -> Result<String> {
    let chunks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let chunks_clone = Arc::clone(&chunks);

    orchestrator.on_notification(Box::new(move |method, params| {
        if method == methods::RESPONSE_CHUNK {
            if let Some(params) = params {
                if let Some(text) = params.get("text").and_then(|t| t.as_str()) {
                    sink.send_chunk(text);
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

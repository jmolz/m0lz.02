use anyhow::{Context, Result};
use pice_protocol::{
    methods, ProviderCapabilities, SessionCreateParams, SessionDestroyParams, SessionSendParams,
};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::stream::SharedSink;
use super::ProviderOrchestrator;
use crate::provider::host::NotificationHandler;

fn ensure_workflow_capable(provider_name: &str, capabilities: &ProviderCapabilities) -> Result<()> {
    if capabilities.workflow {
        Ok(())
    } else {
        anyhow::bail!(
            "provider '{provider_name}' does not support workflow sessions; choose a provider with workflow=true for prime/plan/execute/review/commit/handoff"
        )
    }
}

/// Workflow sessions can legitimately run for a long time because the provider
/// may be driving an external coding CLI. Keep the Rust-side request deadline
/// above normal human coding-session duration so it does not fire while the
/// provider is still supervising its child process.
const WORKFLOW_SESSION_SEND_TIMEOUT: Duration = Duration::from_secs(4 * 60 * 60);

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
    ensure_workflow_capable(orchestrator.provider_name(), orchestrator.capabilities())?;

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
    let send_result = orchestrator
        .request_with_timeout(
            methods::SESSION_SEND,
            Some(send_params),
            WORKFLOW_SESSION_SEND_TIMEOUT,
        )
        .await;

    let destroy_params = serde_json::to_value(SessionDestroyParams {
        session_id: session_id.clone(),
    })?;
    let destroy_result = orchestrator
        .request(methods::SESSION_DESTROY, Some(destroy_params))
        .await;

    match (send_result, destroy_result) {
        (Ok(_), Ok(_)) => Ok(()),
        (Err(send_err), Ok(_)) => Err(send_err),
        (Ok(_), Err(destroy_err)) => Err(destroy_err),
        (Err(send_err), Err(destroy_err)) => Err(send_err).context(format!(
            "also failed to destroy workflow session: {destroy_err}"
        )),
    }?;

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
    let final_text: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let chunks_clone = Arc::clone(&chunks);
    let final_text_clone = Arc::clone(&final_text);

    orchestrator.on_notification(Box::new(move |method, params| {
        if let Some(params) = params {
            match method.as_str() {
                methods::RESPONSE_CHUNK => {
                    if let Some(text) = params.get("text").and_then(|t| t.as_str()) {
                        sink.send_chunk(text);
                        if let Ok(mut guard) = chunks_clone.lock() {
                            guard.push(text.to_string());
                        }
                    }
                }
                methods::RESPONSE_COMPLETE => {
                    if let Some(text) = params
                        .get("result")
                        .and_then(|result| result.get("finalText"))
                        .and_then(|text| text.as_str())
                    {
                        if let Ok(mut guard) = final_text_clone.lock() {
                            *guard = Some(text.to_string());
                        }
                    }
                }
                _ => {}
            }
        }
    }));

    run_session(orchestrator, project_root, prompt).await?;

    if let Some(captured) = final_text
        .lock()
        .map_err(|_| anyhow::anyhow!("failed to acquire final-text lock"))?
        .clone()
    {
        return Ok(captured);
    }

    let captured = chunks
        .lock()
        .map_err(|_| anyhow::anyhow!("failed to acquire chunk lock"))?
        .join("");

    Ok(captured)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(workflow: bool) -> ProviderCapabilities {
        ProviderCapabilities {
            workflow,
            evaluation: true,
            agent_teams: false,
            models: vec!["test".to_string()],
            default_eval_model: None,
            cost_telemetry: false,
        }
    }

    #[test]
    fn workflow_capability_guard_allows_workflow_provider() {
        ensure_workflow_capable("stub", &caps(true)).unwrap();
    }

    #[test]
    fn workflow_capability_guard_rejects_evaluation_only_provider() {
        let err = ensure_workflow_capable("eval-only", &caps(false)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("eval-only"));
        assert!(msg.contains("workflow sessions"));
    }

    #[test]
    fn workflow_session_send_timeout_allows_long_running_cli_work() {
        assert!(WORKFLOW_SESSION_SEND_TIMEOUT >= Duration::from_secs(60 * 60));
    }
}

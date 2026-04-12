//! Inline mode — runs a `CommandRequest` in-process without a socket.
//!
//! Used by:
//! - `pice-cli` when `PICE_DAEMON_INLINE=1` is set (regression-safe escape
//!   hatch for diagnosing daemon-related failures).
//! - Integration tests that want to exercise the handler chain without
//!   spawning a separate daemon subprocess.
//!
//! ## What inline mode skips
//!
//! - Socket binding and stale-socket cleanup
//! - Auth token generation and file I/O
//! - The JSON-RPC framing/routing layer
//! - The connection-per-request accept loop
//!
//! ## What inline mode keeps
//!
//! - The full handler dispatch chain ([`crate::handlers::dispatch`])
//! - Streaming output via the provided [`StreamSink`]
//! - `DaemonContext` (with an empty token since auth is bypassed)

use anyhow::Result;
use pice_core::cli::{CommandRequest, CommandResponse};

use crate::handlers;
use crate::orchestrator::StreamSink;
use crate::server::router::DaemonContext;

/// Run a command in-process, bypassing the socket transport and auth layer.
///
/// Creates a minimal [`DaemonContext`] (no auth token, no socket) and
/// dispatches to the same handler chain the daemon would use. Streaming
/// output is sent to `sink`.
///
/// This is the public entry point that `pice-cli` calls when
/// `PICE_DAEMON_INLINE=1` is set.
pub async fn run_command(req: CommandRequest, sink: &dyn StreamSink) -> Result<CommandResponse> {
    let ctx = DaemonContext::inline();
    handlers::dispatch(req, &ctx, sink).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::NullSink;
    use pice_core::cli::{InitRequest, StatusRequest};

    #[tokio::test]
    async fn inline_status_returns_stub_response() {
        let req = CommandRequest::Status(StatusRequest { json: false });
        let resp = run_command(req, &NullSink).await.expect("run_command");
        match resp {
            CommandResponse::Text { content } => {
                assert!(
                    content.contains("stub"),
                    "inline status should return stub, got: {content}"
                );
            }
            other => panic!("expected Text response, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn inline_json_mode_returns_json_variant() {
        let req = CommandRequest::Init(InitRequest {
            force: false,
            json: true,
        });
        let resp = run_command(req, &NullSink).await.expect("run_command");
        match resp {
            CommandResponse::Json { value } => {
                assert_eq!(value["status"], "stub");
                assert_eq!(value["command"], "init");
            }
            other => panic!("expected Json response, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn inline_streams_to_sink() {
        use std::sync::Mutex;

        struct CaptureSink {
            chunks: Mutex<Vec<String>>,
        }

        impl StreamSink for CaptureSink {
            fn send_chunk(&self, text: &str) {
                self.chunks.lock().unwrap().push(text.to_string());
            }
        }

        let sink = CaptureSink {
            chunks: Mutex::new(Vec::new()),
        };
        let req = CommandRequest::Status(StatusRequest { json: false });
        let _resp = run_command(req, &sink).await.expect("run_command");

        let chunks = sink.chunks.lock().unwrap();
        assert!(
            !chunks.is_empty(),
            "handler should have sent at least one chunk"
        );
    }
}

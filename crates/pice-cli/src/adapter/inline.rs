//! Inline-mode dispatch — runs a `CommandRequest` in-process without a socket.
//!
//! When `PICE_DAEMON_INLINE=1` is set, the CLI bypasses the daemon entirely
//! and calls `pice_daemon::inline::run_command` with a [`TerminalSink`] that
//! prints streaming output directly to stdout.
//!
//! ## What inline mode skips
//!
//! - Socket connection and auto-start
//! - Auth token read and validation
//! - JSON-RPC framing (request/response serialization)
//!
//! ## What inline mode keeps
//!
//! - The full handler dispatch chain (`pice_daemon::handlers::dispatch`)
//! - Streaming output via [`TerminalSink`]

use anyhow::Result;
use pice_core::cli::{CommandRequest, CommandResponse};
use pice_daemon::orchestrator::{StreamEvent, StreamSink};

/// Dispatch a command in inline mode (no socket, no daemon process).
///
/// Creates a [`TerminalSink`] for streaming output and delegates to
/// `pice_daemon::inline::run_command`.
pub async fn dispatch_inline(req: CommandRequest) -> Result<CommandResponse> {
    let sink = TerminalSink;
    pice_daemon::inline::run_command(req, &sink).await
}

/// A [`StreamSink`] that prints chunks to stdout and events to stderr.
///
/// Matches v0.1 CLI behavior: streaming model output goes to stdout,
/// advisory notices go to stderr alongside tracing output.
pub struct TerminalSink;

impl StreamSink for TerminalSink {
    fn send_chunk(&self, text: &str) {
        print!("{text}");
    }

    fn send_event(&self, event: StreamEvent) {
        if let StreamEvent::Notice { level, message } = event {
            eprintln!("[{level:?}] {message}");
        }
        // Future StreamEvent variants are silently ignored.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pice_core::cli::StatusRequest;
    use pice_daemon::orchestrator::NoticeLevel;

    #[tokio::test]
    async fn inline_dispatch_returns_text_response() {
        let req = CommandRequest::Status(StatusRequest { json: false });
        let resp = dispatch_inline(req).await.expect("dispatch_inline");
        match resp {
            CommandResponse::Text { content } => {
                assert!(
                    content.contains("PICE Status"),
                    "inline status should contain header, got: {content}"
                );
            }
            other => panic!("expected Text response, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn inline_dispatch_json_mode() {
        let req = CommandRequest::Status(StatusRequest { json: true });
        let resp = dispatch_inline(req).await.expect("dispatch_inline");
        match resp {
            CommandResponse::Json { value } => {
                assert!(
                    value["plans"].is_array(),
                    "status json should have plans array, got: {value}"
                );
            }
            other => panic!("expected Json response, got: {other:?}"),
        }
    }

    #[test]
    fn terminal_sink_send_chunk_does_not_panic() {
        let sink = TerminalSink;
        sink.send_chunk("hello ");
        sink.send_chunk("world\n");
    }

    #[test]
    fn terminal_sink_send_event_does_not_panic() {
        let sink = TerminalSink;
        sink.send_event(StreamEvent::Notice {
            level: NoticeLevel::Warn,
            message: "test warning".to_string(),
        });
    }
}

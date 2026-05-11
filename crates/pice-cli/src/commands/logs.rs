//! `pice logs <feature_id>` — inspect or tail captured provider
//! session logs for a background feature run.
//!
//! Phase 7 Task 13 shape:
//! - `pice logs <feature_id>`               → one-shot snapshot via
//!   `cli/dispatch::Logs` (daemon handler renders JSON or text).
//! - `pice logs <feature_id> --follow`      → router-level
//!   `logs/stream` RPC with `follow: true`; opens a
//!   [`crate::adapter::transport::DaemonClient::subscribe_stream`]
//!   connection and forwards every `logs/chunk` notification until a
//!   `LogChunk { terminal: true }` frame arrives or the connection
//!   closes.
//! - `--layer L` filters both modes.
//! - `--stream-json` (requires `--follow`) emits heterogeneous
//!   `StreamJsonFrame` NDJSON frames. `--json` (requires `--follow=false`)
//!   emits a single top-level `LogsStreamResponse` object.
//!
//! **Short-circuit on terminal-in-history (Codex Cycle 2 fix):** when the
//! `logs/stream` snapshot already contains a `LogChunk { terminal: true }`
//! frame (late subscribe to a completed feature), the CLI renders the
//! replay and exits IMMEDIATELY instead of hanging waiting for a live
//! terminal frame that will never fire.

use anyhow::{Context, Result};
use clap::Args;
use pice_core::cli::{CommandRequest, ExitJsonStatus, LogsRequest};
use pice_core::events::{LogChunk, StreamJsonFrame};
use pice_core::protocol::methods::{LOGS_CHUNK, LOGS_STREAM};
use pice_core::protocol::subscribe::{LogsStreamRequest, LogsStreamResponse};
use serde_json::json;

use crate::adapter::autostart::ensure_daemon_running;

#[derive(Args, Debug, Clone)]
pub struct LogsArgs {
    /// Feature id whose captured session logs to inspect.
    pub feature_id: String,

    /// Restrict output to a specific layer (filters buffered history
    /// and live chunks).
    #[arg(long)]
    pub layer: Option<String>,

    /// Tail live log chunks as they are emitted. Conflicts with
    /// `--json`; the follow path emits an NDJSON stream instead of a
    /// single top-level JSON object.
    #[arg(long, conflicts_with = "json")]
    pub follow: bool,

    /// Output as a single JSON object. Conflicts with `--follow`.
    #[arg(long)]
    pub json: bool,

    /// Emit heterogeneous `StreamJsonFrame` NDJSON frames. Requires
    /// `--follow`.
    #[arg(long, requires = "follow")]
    pub stream_json: bool,

    /// Include buffered history in the response. Default `true`; set
    /// `--no-include-history` when you only want live chunks going
    /// forward under `--follow`.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub include_history: bool,
}

impl From<LogsArgs> for LogsRequest {
    fn from(args: LogsArgs) -> Self {
        LogsRequest {
            feature_id: args.feature_id,
            layer: args.layer,
            follow: args.follow,
            json: args.json,
            stream_json: args.stream_json,
            include_history: args.include_history,
        }
    }
}

pub async fn run(args: &LogsArgs) -> Result<()> {
    if args.follow {
        return run_follow(args).await;
    }
    // Non-follow: dispatch through `cli/dispatch`, render the response.
    let req = CommandRequest::Logs(args.clone().into());
    let resp = crate::adapter::dispatch(req).await?;
    super::render_response(resp)
}

/// `pice logs <feature_id> --follow` — open `logs/stream` and tail.
///
/// The response body is a full [`LogsStreamResponse`] carrying the
/// history up to subscribe time; subsequent `logs/chunk` notifications
/// arrive on the same connection until a `LogChunk { terminal: true }`
/// is observed or the stream closes.
async fn run_follow(args: &LogsArgs) -> Result<()> {
    // Phase 7 Criterion 20: `PICE_DAEMON_INLINE=1` has no socket to
    // subscribe on. Graceful fallback: emit stderr notice, dispatch a
    // single-shot non-follow logs request, render, exit 0.
    if crate::adapter::is_inline_mode() {
        eprintln!(
            "pice: PICE_DAEMON_INLINE=1 — `--follow` streaming unavailable; \
             emitting buffered history snapshot instead"
        );
        let mut downgrade: LogsArgs = args.clone();
        downgrade.follow = false;
        downgrade.stream_json = false;
        let req = CommandRequest::Logs(downgrade.into());
        let resp = crate::adapter::dispatch(req).await?;
        return super::render_response(resp);
    }

    let feature_id = args.feature_id.clone();
    let layer = args.layer.clone();
    let stream_json = args.stream_json;

    let client = ensure_daemon_running()
        .await
        .context("failed to open subscribe connection for pice logs --follow")?;
    let params = LogsStreamRequest {
        feature_id: feature_id.clone(),
        layer: layer.clone(),
        follow: true,
        include_history: args.include_history,
    };
    let mut stream = client
        .subscribe_stream::<_, LogsStreamResponse>(LOGS_STREAM, params)
        .await
        .context("failed to open logs/stream subscribe connection")?;

    // Render buffered history.
    for chunk in &stream.snapshot.history {
        render_chunk(chunk, stream_json)?;
    }

    // Short-circuit: history already carries a terminal frame → exit
    // immediately (Codex Cycle 2 fix).
    if stream.snapshot.history.iter().any(|c| c.terminal) {
        stream.close().await;
        emit_logs_stream_terminal(stream_json, 0)?;
        return maybe_emit_logs_stream_ended(args.json);
    }

    loop {
        match stream.rx.recv().await {
            Some(notif) => {
                if notif.method != LOGS_CHUNK {
                    continue;
                }
                let Ok(chunk) = serde_json::from_value::<LogChunk>(notif.params) else {
                    continue;
                };
                // Apply client-side layer filter as defense-in-depth —
                // the daemon already filters, but a future wildcard
                // topic could slip through.
                if let Some(ref want) = layer {
                    if chunk.layer != *want && !chunk.terminal {
                        continue;
                    }
                }
                render_chunk(&chunk, stream_json)?;
                if chunk.terminal {
                    stream.close().await;
                    emit_logs_stream_terminal(stream_json, 0)?;
                    return maybe_emit_logs_stream_ended(args.json);
                }
            }
            None => {
                // Daemon closed the subscribe connection before a terminal
                // frame. Phase 7 semantics → exit 5.
                emit_daemon_disconnected(&feature_id, stream_json)?;
                std::process::exit(ExitJsonStatus::DaemonDisconnected.exit_code());
            }
        }
    }
}

fn render_chunk(chunk: &LogChunk, stream_json: bool) -> Result<()> {
    if stream_json {
        let frame = StreamJsonFrame::LogChunk {
            chunk: chunk.clone(),
        };
        println!("{}", serde_json::to_string(&frame)?);
    } else if chunk.terminal {
        // Stderr: a terminal marker is a control event for human
        // readers, not buffered output — written to stderr so callers
        // who pipe stdout into a log file do not get the sentinel mixed
        // in.
        eprintln!(
            "[{}] (terminal) reason={}",
            chunk.timestamp,
            chunk.reason.as_deref().unwrap_or("-")
        );
    } else {
        // Stdout: one chunk per `text`, as-is. Trailing newline
        // preserved (matches `cat`-like `tail -f` output).
        let text = chunk.text.trim_end_matches('\n');
        println!("[{}] [{}] {}", chunk.timestamp, chunk.layer, text);
    }
    Ok(())
}

fn maybe_emit_logs_stream_ended(json: bool) -> Result<()> {
    if json {
        // Structured success discriminant for `--json` consumers tailing
        // a completed feature. Exit 0 — stream-close was clean.
        let value = json!({
            "status": ExitJsonStatus::LogsStreamEnded.as_str(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
        );
    }
    Ok(())
}

fn emit_logs_stream_terminal(stream_json: bool, exit_code: i32) -> Result<()> {
    if stream_json {
        let frame = StreamJsonFrame::Terminal { exit_code };
        println!("{}", serde_json::to_string(&frame)?);
    }
    Ok(())
}

fn emit_daemon_disconnected(feature_id: &str, stream_json: bool) -> Result<()> {
    if stream_json {
        let value = json!({
            "kind": "terminal",
            "exit_code": ExitJsonStatus::DaemonDisconnected.exit_code(),
        });
        println!("{}", serde_json::to_string(&value)?);
    } else {
        eprintln!(
            "subscribe connection closed before feature {feature_id} reached \
             terminal frame"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_follow_args_route_through_cli_dispatch() {
        // The handler chooses its RPC based on `follow`. This test pins
        // the dispatch-method choice without invoking the async runtime —
        // if a future refactor accidentally routes non-follow through
        // `logs/stream`, the assertion fails at compile time (a
        // `LogsStreamRequest` does not fit `CommandRequest::Logs`).
        let args = LogsArgs {
            feature_id: "f".to_string(),
            layer: None,
            follow: false,
            json: false,
            stream_json: false,
            include_history: true,
        };
        let req: CommandRequest = CommandRequest::Logs(args.into());
        // CommandRequest tag is kebab-case "logs".
        let wire = serde_json::to_string(&req).unwrap();
        assert!(wire.contains(r#""command":"logs""#), "got {wire}");
    }

    #[test]
    fn follow_args_build_logs_stream_request() {
        // The follow path bundles a LogsStreamRequest (with follow=true).
        // Pin the wire shape so a rename in pice-core catches at this test.
        let params = LogsStreamRequest {
            feature_id: "f".to_string(),
            layer: Some("backend".to_string()),
            follow: true,
            include_history: false,
        };
        let wire = serde_json::to_string(&params).unwrap();
        assert!(wire.contains(r#""feature_id":"f""#));
        assert!(wire.contains(r#""follow":true"#));
        assert!(wire.contains(r#""include_history":false"#));
    }

    #[test]
    fn render_chunk_human_mode_prints_layer_and_timestamp() {
        // Smoke-test the human render: no panic on a well-formed chunk.
        // The println goes to stdout in the real CLI; this test only
        // asserts the function returns Ok.
        let chunk = LogChunk {
            feature_id: "f".to_string(),
            run_id: "r-1".to_string(),
            layer: "backend".to_string(),
            text: "compiling foo\n".to_string(),
            timestamp: "2026-04-21T10:00:00Z".to_string(),
            terminal: false,
            reason: None,
        };
        assert!(render_chunk(&chunk, false).is_ok());
    }

    #[test]
    fn render_chunk_stream_json_mode_emits_envelope() {
        let chunk = LogChunk {
            feature_id: "f".to_string(),
            run_id: "r-1".to_string(),
            layer: "backend".to_string(),
            text: "x\n".to_string(),
            timestamp: "2026-04-21T10:00:00Z".to_string(),
            terminal: false,
            reason: None,
        };
        assert!(render_chunk(&chunk, true).is_ok());
    }

    #[test]
    fn render_chunk_terminal_in_stream_json_mode() {
        let chunk = LogChunk {
            feature_id: "f".to_string(),
            run_id: "r-1".to_string(),
            layer: "".to_string(),
            text: "".to_string(),
            timestamp: "2026-04-21T10:04:00Z".to_string(),
            terminal: true,
            reason: Some("passed".to_string()),
        };
        assert!(render_chunk(&chunk, true).is_ok());
    }

    #[test]
    fn emit_logs_stream_terminal_is_available_for_stream_json_follow() {
        assert!(emit_logs_stream_terminal(true, 0).is_ok());
        assert!(emit_logs_stream_terminal(false, 0).is_ok());
    }
}

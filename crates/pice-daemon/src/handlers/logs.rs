//! `pice logs <feature_id>` handler (non-follow path).
//!
//! Phase 7 Task 13: the `--follow=false` CLI route dispatches through
//! `cli/dispatch` and lands here. `--follow=true` bypasses `cli/dispatch`
//! entirely and opens a router-level `logs/stream` RPC — see
//! `handlers/subscribe.rs::logs` for that flow.
//!
//! This handler is a thin layer over [`crate::logs::store::LogStore::snapshot`]:
//! it reads the buffered history for the requested `(feature_id, layer)`
//! tuple and renders it as JSON (structured) or text (one chunk per line).
//!
//! A missing feature (no buffered logs) is NOT a hard error — the log
//! store returns an empty vec. The handler surfaces that as an empty
//! history in JSON mode, or a friendly "no log chunks buffered" message
//! in text mode. A hard `FeatureNotFound` error would race badly against
//! the daemon's eventual-consistency semantics (a feature that just
//! dispatched may not have any buffered logs yet).

use anyhow::Result;
use pice_core::cli::{CommandResponse, LogsRequest};
use pice_core::protocol::subscribe::LogsStreamResponse;

use crate::orchestrator::StreamSink;
use crate::server::router::DaemonContext;

pub async fn run(
    req: LogsRequest,
    ctx: &DaemonContext,
    _sink: &dyn StreamSink,
) -> Result<CommandResponse> {
    // Follow-mode must not land here — the CLI is expected to route to
    // `logs/stream` directly. Defensive rejection so a routing bug
    // surfaces fast instead of silently dropping into the non-follow
    // path.
    if req.follow {
        return Ok(CommandResponse::Exit {
            code: 1,
            message: "pice logs --follow must route via logs/stream, not cli/dispatch \
                      (CLI routing bug)"
                .to_string(),
        });
    }

    let history = if req.include_history {
        ctx.logs()
            .snapshot(&req.feature_id, req.layer.as_deref())
            .await
    } else {
        Vec::new()
    };
    let run_id = ctx
        .jobs()
        .run_id_for(&req.feature_id)
        .map(|r| r.to_string())
        .or_else(|| history.iter().next_back().map(|c| c.run_id.clone()))
        .unwrap_or_default();

    if req.json {
        let body = LogsStreamResponse { history, run_id };
        return Ok(CommandResponse::Json {
            value: serde_json::to_value(&body)?,
        });
    }

    // Text mode: render one line per chunk, with a terminal marker when
    // the buffered history includes the end-of-stream frame.
    if history.is_empty() {
        return Ok(CommandResponse::Text {
            content: format!(
                "no log chunks buffered for feature '{}'. The feature may not \
                 have started, or may have exceeded the in-memory buffer cap.\n",
                req.feature_id
            ),
        });
    }

    let mut out = String::new();
    let _ = run_id; // run_id is surfaced in JSON mode only; text mode
                    // relies on the per-chunk `run_id` field which is
                    // already bundled with each log line.
    if let Some(layer) = &req.layer {
        out.push_str(&format!(
            "# logs for feature '{}' (layer filter: {})\n",
            req.feature_id, layer
        ));
    } else {
        out.push_str(&format!("# logs for feature '{}'\n", req.feature_id));
    }
    for chunk in &history {
        if chunk.terminal {
            out.push_str(&format!(
                "[{}] (terminal) reason={}\n",
                chunk.timestamp,
                chunk.reason.as_deref().unwrap_or("-")
            ));
        } else {
            // Strip trailing newline from `text` to avoid double-newlines
            // — `text` frequently ends with `\n` from the provider.
            let text = chunk.text.trim_end_matches('\n');
            out.push_str(&format!(
                "[{}] [{}] {}\n",
                chunk.timestamp, chunk.layer, text
            ));
        }
    }
    Ok(CommandResponse::Text { content: out })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::NullSink;
    use crate::server::router::DaemonContext;
    use pice_core::cli::LogsRequest;

    fn req(feature_id: &str, json: bool, layer: Option<&str>) -> LogsRequest {
        LogsRequest {
            feature_id: feature_id.to_string(),
            layer: layer.map(|s| s.to_string()),
            follow: false,
            json,
            stream_json: false,
            include_history: true,
        }
    }

    #[tokio::test]
    async fn non_follow_returns_empty_history_when_feature_unknown() {
        let ctx = DaemonContext::new_for_test("token");
        let resp = run(req("ghost-feature", true, None), &ctx, &NullSink)
            .await
            .expect("run");
        match resp {
            CommandResponse::Json { value } => {
                let body: LogsStreamResponse = serde_json::from_value(value).unwrap();
                assert!(body.history.is_empty());
                assert_eq!(body.run_id, "");
            }
            other => panic!("expected Json, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_follow_text_mode_prints_friendly_empty_message() {
        let ctx = DaemonContext::new_for_test("token");
        let resp = run(req("ghost-feature", false, None), &ctx, &NullSink)
            .await
            .expect("run");
        match resp {
            CommandResponse::Text { content } => {
                assert!(content.contains("no log chunks buffered"));
                assert!(content.contains("ghost-feature"));
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_follow_json_mode_returns_buffered_chunks() {
        let ctx = DaemonContext::new_for_test("token");
        ctx.logs()
            .append_chunk("feat-x", "r-1", "backend", "hello\n".to_string())
            .await;
        ctx.logs()
            .append_chunk("feat-x", "r-1", "backend", "world\n".to_string())
            .await;
        let resp = run(req("feat-x", true, None), &ctx, &NullSink)
            .await
            .expect("run");
        match resp {
            CommandResponse::Json { value } => {
                let body: LogsStreamResponse = serde_json::from_value(value).unwrap();
                assert_eq!(body.history.len(), 2);
                assert_eq!(body.history[0].text, "hello\n");
                assert_eq!(body.run_id, "r-1");
            }
            other => panic!("expected Json, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_follow_text_mode_renders_chunks_inline() {
        let ctx = DaemonContext::new_for_test("token");
        ctx.logs()
            .append_chunk("feat-x", "r-1", "backend", "compiling\n".to_string())
            .await;
        let resp = run(req("feat-x", false, None), &ctx, &NullSink)
            .await
            .expect("run");
        match resp {
            CommandResponse::Text { content } => {
                assert!(content.contains("# logs for feature 'feat-x'"));
                assert!(content.contains("compiling"));
                assert!(content.contains("[backend]"));
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_follow_layer_filter_restricts_output() {
        let ctx = DaemonContext::new_for_test("token");
        ctx.logs()
            .append_chunk("feat-x", "r-1", "backend", "b\n".to_string())
            .await;
        ctx.logs()
            .append_chunk("feat-x", "r-1", "frontend", "f\n".to_string())
            .await;
        let resp = run(req("feat-x", true, Some("backend")), &ctx, &NullSink)
            .await
            .expect("run");
        match resp {
            CommandResponse::Json { value } => {
                let body: LogsStreamResponse = serde_json::from_value(value).unwrap();
                assert_eq!(
                    body.history.len(),
                    1,
                    "layer filter should keep only backend"
                );
                assert_eq!(body.history[0].layer, "backend");
            }
            other => panic!("expected Json, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn follow_mode_rejected_at_dispatch() {
        let ctx = DaemonContext::new_for_test("token");
        let mut r = req("feat-x", false, None);
        r.follow = true;
        let resp = run(r, &ctx, &NullSink).await.expect("run");
        match resp {
            CommandResponse::Exit { code, message } => {
                assert_eq!(code, 1);
                assert!(message.contains("logs/stream"));
            }
            other => panic!("expected Exit, got {other:?}"),
        }
    }
}

//! Phase 7 Task 6: router-level subscribe handlers for `manifest/subscribe`
//! and `logs/stream`.
//!
//! These RPCs are NOT `cli/dispatch` variants — they live in their own branch
//! of the server router. Unlike `cli/dispatch` which is one-shot (read the
//! request, dispatch, write ONE response, done), subscribe handlers take over
//! the connection for the lifetime of the subscription:
//!
//! 1. Parse the typed request from `req.params`.
//! 2. Assemble a snapshot + write it as the normal `DaemonResponse` body.
//! 3. Acquire a `broadcast::Receiver` from the appropriate primitive
//!    (`EventBus` or `LogStore`).
//! 4. `tokio::select!` between:
//!    - The broadcast receiver's `.recv()` — writes a `DaemonNotification`
//!      back on the SAME connection.
//!    - `conn.read_request()` — any result (including `Ok(None)` clean EOF
//!      or `Err`) means the client disconnected; drop the receiver and exit.
//! 5. On loop exit, the receiver drops → channel subscriber count
//!    decrements → no explicit unsubscribe RPC, no `SubscriptionRegistry`.
//!
//! ## Lagged receivers
//!
//! The `broadcast::error::RecvError::Lagged(n)` case is treated as "close
//! subscription" per the plan — the CLI would observe a gap that cannot be
//! reconciled from a live stream, so we drop the connection with a warn log
//! rather than continue emitting stale-after-gap events. The client can
//! re-subscribe to replay the snapshot + any new events.
//!
//! ## State-dir snapshot
//!
//! The initial `SubscribeManifestResponse` scans
//! `VerificationManifest::state_dir()` (honoring `PICE_STATE_DIR`) for every
//! `{project_hash}/*.manifest.json` under the current project's hash. A
//! feature-scoped subscribe loads exactly one file; a wildcard subscribe
//! loads all manifests under the project namespace. Read errors on
//! individual files are logged + skipped (one corrupt manifest must not kill
//! the whole snapshot).

use std::collections::BTreeMap;

use anyhow::Result;
use pice_core::events::{LogChunk, ManifestEventPayload};
use pice_core::layers::manifest::{manifest_project_namespace, VerificationManifest};
use pice_core::protocol::{
    methods,
    subscribe::{
        LogsStreamRequest, LogsStreamResponse, SubscribeManifestRequest, SubscribeManifestResponse,
    },
    DaemonNotification, DaemonRequest, DaemonResponse,
};
use tokio::sync::broadcast::error::RecvError;

use crate::server::connection::DaemonConnection;
use crate::server::router::DaemonContext;

/// JSON-RPC error code for "invalid params" (matches router.rs).
const INVALID_PARAMS_CODE: i32 = -32602;

// ─── manifest/subscribe ─────────────────────────────────────────────────────

/// Handle a `manifest/subscribe` RPC. Owns the connection for the lifetime
/// of the subscription.
///
/// Flow:
/// 1. Parse `SubscribeManifestRequest` from `req.params`.
/// 2. Acquire a per-feature (or wildcard) event receiver.
/// 3. Scan the state dir for matching manifests → `snapshots`.
/// 4. Snapshot live `feature_id → run_id` state.
/// 5. Write the snapshot as the RPC response body.
/// 6. Loop: `tokio::select!` between inbound read + outbound event, writing
///    `manifest/event` notifications until one side closes.
pub async fn manifest(
    ctx: &DaemonContext,
    conn: &mut dyn DaemonConnection,
    req: DaemonRequest,
) -> Result<()> {
    let parsed: SubscribeManifestRequest = match serde_json::from_value(req.params.clone()) {
        Ok(p) => p,
        Err(e) => {
            let resp = DaemonResponse::error(
                req.id,
                INVALID_PARAMS_CODE,
                format!("failed to parse SubscribeManifestRequest: {e}"),
            );
            conn.write_response(&resp).await?;
            return Ok(());
        }
    };

    // Subscribe before the snapshot scan so terminal events emitted during
    // filesystem reads are queued in this receiver instead of lost.
    let mut rx = match parsed.feature_id.as_deref() {
        Some(feat) => ctx.events().subscribe_feature(feat),
        None => ctx.events().subscribe_wildcard(),
    };

    // Assemble the snapshot.
    let snapshots = match load_manifest_snapshots(ctx, parsed.feature_id.as_deref()) {
        Ok(v) => v,
        Err(e) => {
            let resp = DaemonResponse::error(
                req.id,
                -32603,
                format!("failed to scan manifest state dir: {e}"),
            );
            conn.write_response(&resp).await?;
            return Ok(());
        }
    };

    let run_ids: BTreeMap<String, String> = ctx.jobs().live_runs();

    let response_body = SubscribeManifestResponse { snapshots, run_ids };
    let value = serde_json::to_value(&response_body)?;
    let resp = DaemonResponse::success(req.id, value);
    conn.write_response(&resp).await?;

    // Stream loop.
    loop {
        tokio::select! {
            biased;

            // Client hangup detection. Any result (Ok(None) / Err) breaks.
            inbound = conn.read_request() => {
                match inbound {
                    Ok(None) => {
                        tracing::debug!("manifest/subscribe: client hung up (clean EOF)");
                    }
                    Ok(Some(_unexpected)) => {
                        // Per plan: the subscribe channel is one-shot for
                        // requests — any extra inbound frame is treated as
                        // client desire to close (and may be a bug). Log
                        // then exit; do NOT dispatch the extra request.
                        tracing::debug!(
                            "manifest/subscribe: unexpected inbound frame — \
                             closing subscription"
                        );
                    }
                    Err(e) => {
                        tracing::debug!("manifest/subscribe: read error: {e}");
                    }
                }
                break;
            }

            // Event fan-out.
            recv = rx.recv() => {
                match recv {
                    Ok(payload) => {
                        if let Err(e) = write_manifest_event(conn, &payload).await {
                            tracing::debug!("manifest/subscribe: write error: {e}");
                            break;
                        }
                    }
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(
                            lagged = n,
                            "manifest/subscribe: receiver lagged — closing subscription"
                        );
                        break;
                    }
                    Err(RecvError::Closed) => {
                        // The bus Sender dropping would be unusual (DaemonContext
                        // outlives every handler) but honor the EOF contract.
                        tracing::debug!("manifest/subscribe: bus closed");
                        break;
                    }
                }
            }
        }
    }

    // rx dropped on return; subscriber count decrements naturally.
    Ok(())
}

/// Scan the manifest state directory for the project namespace and load
/// every matching manifest. Errors on individual files are logged + skipped.
fn load_manifest_snapshots(
    ctx: &DaemonContext,
    feature_filter: Option<&str>,
) -> Result<Vec<VerificationManifest>> {
    let state_dir = VerificationManifest::state_dir()?;
    let namespace = manifest_project_namespace(ctx.project_root());
    let project_dir = state_dir.join(&namespace);

    // Non-existent project dir → empty snapshot (valid — no features
    // dispatched yet).
    if !project_dir.exists() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    let read = match std::fs::read_dir(&project_dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                dir = %project_dir.display(),
                error = %e,
                "manifest/subscribe: read_dir failed"
            );
            return Ok(Vec::new());
        }
    };

    for entry in read.flatten() {
        let path = entry.path();
        let file_name = match path.file_name().and_then(|s| s.to_str()) {
            Some(s) if s.ends_with(".manifest.json") => s.to_string(),
            _ => continue,
        };
        let feat = file_name.trim_end_matches(".manifest.json");
        if let Some(filter) = feature_filter {
            if feat != filter {
                continue;
            }
        }
        match VerificationManifest::load(&path) {
            Ok(m) => out.push(m),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "manifest/subscribe: skipping unreadable manifest"
                );
            }
        }
    }

    Ok(out)
}

async fn write_manifest_event(
    conn: &mut dyn DaemonConnection,
    payload: &ManifestEventPayload,
) -> Result<()> {
    let notif = DaemonNotification::new(methods::MANIFEST_EVENT, serde_json::to_value(payload)?);
    conn.write_notification(&notif).await
}

// ─── logs/stream ────────────────────────────────────────────────────────────

/// Handle a `logs/stream` RPC. When `follow: false` the handler returns
/// immediately after writing the history snapshot (one-shot mode — richer
/// than `cli/dispatch → Logs` because the response body carries the full
/// typed vector). When `follow: true` the handler takes over the connection
/// and streams live chunks until a terminal frame is observed OR the client
/// closes.
pub async fn logs(
    ctx: &DaemonContext,
    conn: &mut dyn DaemonConnection,
    req: DaemonRequest,
) -> Result<()> {
    let parsed: LogsStreamRequest = match serde_json::from_value(req.params.clone()) {
        Ok(p) => p,
        Err(e) => {
            let resp = DaemonResponse::error(
                req.id,
                INVALID_PARAMS_CODE,
                format!("failed to parse LogsStreamRequest: {e}"),
            );
            conn.write_response(&resp).await?;
            return Ok(());
        }
    };

    let rx = if parsed.follow {
        Some(ctx.logs().subscribe(&parsed.feature_id).await)
    } else {
        None
    };

    // Snapshot history (subject to `include_history` flag). Follow-mode
    // subscribes first so live chunks emitted during the snapshot read are
    // queued in the receiver and drained after the response body.
    let history = if parsed.include_history {
        ctx.logs()
            .snapshot(&parsed.feature_id, parsed.layer.as_deref())
            .await
    } else {
        Vec::new()
    };

    let run_id = ctx
        .jobs()
        .run_id_for(&parsed.feature_id)
        .map(|r| r.to_string())
        .or_else(|| history.iter().next_back().map(|c| c.run_id.clone()))
        .unwrap_or_default();

    let response_body = LogsStreamResponse { history, run_id };
    let value = serde_json::to_value(&response_body)?;
    let resp = DaemonResponse::success(req.id, value);
    conn.write_response(&resp).await?;

    if !parsed.follow {
        return Ok(());
    }

    let mut rx = rx.expect("follow=true initializes log receiver");

    loop {
        tokio::select! {
            biased;

            inbound = conn.read_request() => {
                match inbound {
                    Ok(None) => {
                        tracing::debug!("logs/stream: client hung up (clean EOF)");
                    }
                    Ok(Some(_unexpected)) => {
                        tracing::debug!(
                            "logs/stream: unexpected inbound frame — closing subscription"
                        );
                    }
                    Err(e) => {
                        tracing::debug!("logs/stream: read error: {e}");
                    }
                }
                break;
            }

            recv = rx.recv() => {
                match recv {
                    Ok(chunk) => {
                        // Apply layer filter if set.
                        if let Some(ref layer) = parsed.layer {
                            if chunk.layer != *layer && !chunk.terminal {
                                // Terminal frames with reason are always
                                // delivered regardless of layer filter so
                                // subscribers can unblock.
                                continue;
                            }
                        }
                        let terminal = chunk.terminal;
                        if let Err(e) = write_log_chunk(conn, &chunk).await {
                            tracing::debug!("logs/stream: write error: {e}");
                            break;
                        }
                        if terminal {
                            tracing::debug!("logs/stream: terminal frame — closing");
                            break;
                        }
                    }
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(
                            lagged = n,
                            "logs/stream: receiver lagged — closing subscription"
                        );
                        break;
                    }
                    Err(RecvError::Closed) => {
                        tracing::debug!("logs/stream: bus closed");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

async fn write_log_chunk(conn: &mut dyn DaemonConnection, chunk: &LogChunk) -> Result<()> {
    let notif = DaemonNotification::new(methods::LOGS_CHUNK, serde_json::to_value(chunk)?);
    conn.write_notification(&notif).await
}

// ─── Method-name dispatch helper ────────────────────────────────────────────

/// Returns `true` if `method` is a Phase 7 router-level subscribe method
/// that must bypass `cli/dispatch`. Exported so the connection accept loop
/// can branch before calling [`crate::server::router::route`].
pub fn is_subscribe_method(method: &str) -> bool {
    matches!(method, m if m == methods::MANIFEST_SUBSCRIBE || m == methods::LOGS_STREAM)
}

/// Dispatch a subscribe method to its handler. Assumes [`is_subscribe_method`]
/// returned true for the request's method name.
///
/// # Panics
/// Debug-asserts if called with a non-subscribe method — this is a bug in
/// the accept-loop branching. In release builds the default arm writes a
/// method-not-found response so the client doesn't hang.
pub async fn dispatch(
    ctx: &DaemonContext,
    conn: &mut dyn DaemonConnection,
    req: DaemonRequest,
) -> Result<()> {
    debug_assert!(
        is_subscribe_method(&req.method),
        "subscribe::dispatch called with non-subscribe method: {}",
        req.method
    );
    match req.method.as_str() {
        methods::MANIFEST_SUBSCRIBE => manifest(ctx, conn, req).await,
        methods::LOGS_STREAM => logs(ctx, conn, req).await,
        other => {
            let resp = DaemonResponse::error(req.id, -32601, format!("method not found: {other}"));
            conn.write_response(&resp).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::connection::test_support::{MemoryConnection, WireFrame};
    use crate::test_support::StateDirGuard;
    use pice_core::events::ManifestEvent;
    use pice_core::layers::manifest::VerificationManifest;
    use serde_json::json;

    // ─── is_subscribe_method ────────────────────────────────────────────

    #[test]
    fn is_subscribe_method_accepts_both_names() {
        assert!(is_subscribe_method(methods::MANIFEST_SUBSCRIBE));
        assert!(is_subscribe_method(methods::LOGS_STREAM));
    }

    #[test]
    fn is_subscribe_method_rejects_cli_dispatch() {
        assert!(!is_subscribe_method(methods::CLI_DISPATCH));
        assert!(!is_subscribe_method(methods::DAEMON_HEALTH));
        assert!(!is_subscribe_method(methods::DAEMON_SHUTDOWN));
        assert!(!is_subscribe_method("foo/bar"));
    }

    // ─── Helpers ────────────────────────────────────────────────────────

    fn subscribe_manifest_request(id: u64, feature_id: Option<&str>) -> DaemonRequest {
        let params = match feature_id {
            Some(f) => json!({ "feature_id": f }),
            None => json!({}),
        };
        DaemonRequest::new(id, methods::MANIFEST_SUBSCRIBE, "", params)
    }

    fn logs_stream_request(id: u64, feature_id: &str, follow: bool) -> DaemonRequest {
        DaemonRequest::new(
            id,
            methods::LOGS_STREAM,
            "",
            json!({ "feature_id": feature_id, "follow": follow }),
        )
    }

    /// Build a DaemonContext rooted at a tempdir with `PICE_STATE_DIR`
    /// set to a matching state dir (for `load_manifest_snapshots`).
    fn test_ctx_with_state(dir: &std::path::Path) -> (StateDirGuard<'static>, DaemonContext) {
        let guard = StateDirGuard::new(dir);
        let project_root = dir.join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let ctx = DaemonContext::new_for_test_with_root("t", project_root);
        (guard, ctx)
    }

    // ─── manifest/subscribe ─────────────────────────────────────────────

    #[tokio::test]
    async fn manifest_subscribe_empty_state_dir_returns_empty_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let (_guard, ctx) = test_ctx_with_state(dir.path());
        let (mut conn, in_tx, mut out_rx) = MemoryConnection::new();

        // Send the request + drop the sender to hang up immediately after.
        in_tx
            .send(subscribe_manifest_request(1, Some("f")))
            .unwrap();
        drop(in_tx);

        // Spawn the handler on the connection.
        let handler = tokio::spawn(async move {
            let req = subscribe_manifest_request(1, Some("f"));
            super::manifest(&ctx, &mut conn, req).await
        });

        // Expect the snapshot response first.
        let first = out_rx.recv().await.expect("response frame");
        match first {
            WireFrame::Response(r) => {
                assert_eq!(r.id, 1);
                assert!(r.error.is_none(), "snapshot should succeed");
                let body: SubscribeManifestResponse =
                    serde_json::from_value(r.result.unwrap()).unwrap();
                assert_eq!(body.snapshots.len(), 0);
                assert_eq!(body.run_ids.len(), 0);
            }
            other => panic!("expected Response, got {other:?}"),
        }

        handler.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn manifest_subscribe_drop_releases_receiver_within_one_yield() {
        let dir = tempfile::tempdir().unwrap();
        let (_guard, ctx) = test_ctx_with_state(dir.path());
        let ctx = std::sync::Arc::new(ctx);
        let (mut conn, in_tx, mut out_rx) = MemoryConnection::new();

        let handler_ctx = std::sync::Arc::clone(&ctx);
        let handler = tokio::spawn(async move {
            let req = subscribe_manifest_request(11, Some("drop-one-tick"));
            super::manifest(handler_ctx.as_ref(), &mut conn, req).await
        });

        let first = out_rx.recv().await.expect("response frame");
        assert!(matches!(first, WireFrame::Response(_)));
        assert_eq!(
            ctx.events().feature_receiver_count("drop-one-tick"),
            1,
            "snapshot response should leave one active receiver while the connection is open"
        );

        drop(in_tx);
        tokio::task::yield_now().await;

        assert!(
            handler.is_finished(),
            "in-memory connection close should let subscribe handler finish within one scheduler yield"
        );
        handler.await.unwrap().unwrap();
        assert_eq!(
            ctx.events().feature_receiver_count("drop-one-tick"),
            0,
            "receiver count should be zero immediately after one-yield handler cleanup"
        );
    }

    #[tokio::test]
    async fn manifest_subscribe_snapshot_returns_matching_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let (_guard, ctx) = test_ctx_with_state(dir.path());

        // Write a manifest under the project's namespace.
        let m = VerificationManifest::new("feat-x", ctx.project_root());
        let path = VerificationManifest::manifest_path_for("feat-x", ctx.project_root()).unwrap();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        m.save(&path).unwrap();

        let (mut conn, in_tx, mut out_rx) = MemoryConnection::new();

        // Drop the inbound sender immediately — after the response, the
        // subscribe loop will observe clean EOF and exit.
        drop(in_tx);

        let handler = tokio::spawn(async move {
            let req = subscribe_manifest_request(7, Some("feat-x"));
            super::manifest(&ctx, &mut conn, req).await
        });

        let first = out_rx.recv().await.expect("response frame");
        match first {
            WireFrame::Response(r) => {
                assert_eq!(r.id, 7);
                let body: SubscribeManifestResponse =
                    serde_json::from_value(r.result.unwrap()).unwrap();
                assert_eq!(body.snapshots.len(), 1);
                assert_eq!(body.snapshots[0].feature_id, "feat-x");
            }
            other => panic!("expected Response, got {other:?}"),
        }

        handler.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn manifest_subscribe_forwards_events_to_connection() {
        let dir = tempfile::tempdir().unwrap();
        let (_guard, ctx) = test_ctx_with_state(dir.path());
        let bus_handle = ctx.events().clone();

        // NOTE: we do NOT push an inbound frame — the handler is called
        // with the request as an argument, and the MemoryConnection's
        // inbound channel is reserved for subsequent "unexpected frame"
        // / hangup detection. Keeping `in_tx` alive without sending
        // makes `read_request()` block until we drop it at the end of
        // the test, which is how we exit the stream loop cleanly.
        let (mut conn, in_tx, mut out_rx) = MemoryConnection::new();

        let handler = tokio::spawn(async move {
            let req = subscribe_manifest_request(42, Some("feat-y"));
            super::manifest(&ctx, &mut conn, req).await
        });

        // Consume the initial snapshot response.
        let first = out_rx.recv().await.expect("response frame");
        match first {
            WireFrame::Response(r) => assert_eq!(r.id, 42),
            other => panic!("expected Response, got {other:?}"),
        }

        // Small delay so the subscribe receiver is registered BEFORE we
        // emit — the broadcast channel only delivers to receivers present
        // at send time.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        bus_handle.emit_layer_started("feat-y", "run-1", "backend");
        bus_handle.emit_layer_complete(
            "feat-y",
            "run-1",
            "backend",
            serde_json::json!({"status": "passed"}),
        );

        // Expect two manifest/event notifications in order.
        let notif1 = tokio::time::timeout(std::time::Duration::from_millis(500), out_rx.recv())
            .await
            .expect("first event within 500ms")
            .expect("frame");
        let notif2 = tokio::time::timeout(std::time::Duration::from_millis(500), out_rx.recv())
            .await
            .expect("second event within 500ms")
            .expect("frame");

        match (notif1, notif2) {
            (WireFrame::Notification(n1), WireFrame::Notification(n2)) => {
                assert_eq!(n1.method, methods::MANIFEST_EVENT);
                assert_eq!(n2.method, methods::MANIFEST_EVENT);
                let p1: ManifestEventPayload = serde_json::from_value(n1.params).unwrap();
                let p2: ManifestEventPayload = serde_json::from_value(n2.params).unwrap();
                assert_eq!(p1.event, ManifestEvent::LayerStarted);
                assert_eq!(p2.event, ManifestEvent::LayerComplete);
            }
            (a, b) => panic!("expected two notifications, got {a:?}, {b:?}"),
        }

        // Hang up — handler should exit cleanly.
        drop(in_tx);
        handler.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn manifest_subscribe_rejects_unknown_feature_filter_field() {
        let dir = tempfile::tempdir().unwrap();
        let (_guard, ctx) = test_ctx_with_state(dir.path());

        // Malformed params: the subscribe DTO has `deny_unknown_fields`.
        let req = DaemonRequest::new(
            1,
            methods::MANIFEST_SUBSCRIBE,
            "",
            json!({ "feature_id": "f", "bogus_field": 1 }),
        );

        let (mut conn, _in_tx, mut out_rx) = MemoryConnection::new();
        let handler = tokio::spawn(async move { super::manifest(&ctx, &mut conn, req).await });

        match out_rx.recv().await.expect("response") {
            WireFrame::Response(r) => {
                let err = r.error.expect("parse error expected");
                assert_eq!(err.code, INVALID_PARAMS_CODE);
            }
            other => panic!("expected error response, got {other:?}"),
        }

        handler.await.unwrap().unwrap();
    }

    // ─── logs/stream ────────────────────────────────────────────────────

    #[tokio::test]
    async fn logs_stream_one_shot_returns_snapshot_and_exits() {
        let dir = tempfile::tempdir().unwrap();
        let (_guard, ctx) = test_ctx_with_state(dir.path());

        // Seed the log store with two chunks for feat-a.
        ctx.logs()
            .append_chunk("feat-a", "run-1", "backend", "first\n".to_string())
            .await;
        ctx.logs()
            .append_chunk("feat-a", "run-1", "backend", "second\n".to_string())
            .await;

        let (mut conn, _in_tx, mut out_rx) = MemoryConnection::new();
        let handler = tokio::spawn(async move {
            let req = logs_stream_request(1, "feat-a", false);
            logs(&ctx, &mut conn, req).await
        });

        match out_rx.recv().await.expect("response") {
            WireFrame::Response(r) => {
                assert_eq!(r.id, 1);
                let body: LogsStreamResponse = serde_json::from_value(r.result.unwrap()).unwrap();
                assert_eq!(body.history.len(), 2);
                assert_eq!(body.history[0].text, "first\n");
                assert_eq!(body.run_id, "run-1");
            }
            other => panic!("expected response, got {other:?}"),
        }

        // With follow: false, no more frames. The handler exits after the
        // response — no inbound hangup needed.
        handler
            .await
            .expect("handler joins")
            .expect("handler Ok(())");
    }

    #[tokio::test]
    async fn logs_stream_follow_exits_on_terminal_frame() {
        let dir = tempfile::tempdir().unwrap();
        let (_guard, ctx) = test_ctx_with_state(dir.path());

        let store = ctx.logs().clone();

        let (mut conn, _in_tx, mut out_rx) = MemoryConnection::new();
        let handler = tokio::spawn(async move {
            let req = logs_stream_request(2, "feat-b", true);
            logs(&ctx, &mut conn, req).await
        });

        // Consume the initial snapshot response (empty).
        match out_rx.recv().await.expect("response") {
            WireFrame::Response(r) => assert_eq!(r.id, 2),
            other => panic!("expected response, got {other:?}"),
        }

        // Allow the subscribe to register.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        store
            .append_chunk("feat-b", "run-x", "backend", "live\n".to_string())
            .await;
        store
            .append_terminal_frame("feat-b", "run-x", "passed")
            .await;

        // Expect exactly one live chunk + one terminal frame, then handler
        // closes (loop exits on `terminal: true`).
        let live = tokio::time::timeout(std::time::Duration::from_millis(500), out_rx.recv())
            .await
            .expect("live chunk within 500ms")
            .expect("frame");
        let terminal = tokio::time::timeout(std::time::Duration::from_millis(500), out_rx.recv())
            .await
            .expect("terminal chunk within 500ms")
            .expect("frame");

        match (live, terminal) {
            (WireFrame::Notification(n1), WireFrame::Notification(n2)) => {
                assert_eq!(n1.method, methods::LOGS_CHUNK);
                assert_eq!(n2.method, methods::LOGS_CHUNK);
                let c1: LogChunk = serde_json::from_value(n1.params).unwrap();
                let c2: LogChunk = serde_json::from_value(n2.params).unwrap();
                assert_eq!(c1.text, "live\n");
                assert!(c2.terminal, "second frame must be terminal");
            }
            (a, b) => panic!("expected two notifications, got {a:?}, {b:?}"),
        }

        handler.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn logs_stream_include_history_false_omits_history() {
        let dir = tempfile::tempdir().unwrap();
        let (_guard, ctx) = test_ctx_with_state(dir.path());
        ctx.logs()
            .append_chunk("feat-c", "run-z", "backend", "skipme\n".to_string())
            .await;

        let req = DaemonRequest::new(
            3,
            methods::LOGS_STREAM,
            "",
            json!({"feature_id": "feat-c", "follow": false, "include_history": false}),
        );

        let (mut conn, _in_tx, mut out_rx) = MemoryConnection::new();
        let handler = tokio::spawn(async move { logs(&ctx, &mut conn, req).await });

        match out_rx.recv().await.expect("response") {
            WireFrame::Response(r) => {
                let body: LogsStreamResponse = serde_json::from_value(r.result.unwrap()).unwrap();
                assert_eq!(body.history.len(), 0, "include_history=false omits");
            }
            other => panic!("expected response, got {other:?}"),
        }

        handler.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn logs_stream_rejects_missing_feature_id() {
        let dir = tempfile::tempdir().unwrap();
        let (_guard, ctx) = test_ctx_with_state(dir.path());

        let req = DaemonRequest::new(4, methods::LOGS_STREAM, "", json!({}));
        let (mut conn, _in_tx, mut out_rx) = MemoryConnection::new();
        let handler = tokio::spawn(async move { logs(&ctx, &mut conn, req).await });

        match out_rx.recv().await.expect("response") {
            WireFrame::Response(r) => {
                let err = r.error.expect("parse error expected");
                assert_eq!(err.code, INVALID_PARAMS_CODE);
            }
            other => panic!("expected error response, got {other:?}"),
        }

        handler.await.unwrap().unwrap();
    }
}

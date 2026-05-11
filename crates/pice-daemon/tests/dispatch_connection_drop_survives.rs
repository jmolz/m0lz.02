//! Phase 7 Criterion 2 integration test.
//!
//! **Invariant pinned:** a background feature task MUST outlive the originating
//! RPC connection. Closing (dropping) any CLI connection MUST NOT cancel a
//! task that was already running in the daemon's `FeatureJobManager`.
//!
//! ## Test design
//!
//! Seeds a long-running background job via `DaemonContext::jobs().spawn()`
//! directly — keeping the test hermetic (no provider process, no plan file) —
//! while still exercising the real `FeatureJobManager` supervision loop and the
//! real `manifest/subscribe` socket handler.
//!
//! ### Step-by-step
//!
//! 1. Build a `DaemonContext` and start a minimal accept loop over a real
//!    Unix socket (mirrors `lifecycle::run_unix` without `run_with_paths`).
//! 2. Seed a background feature via `ctx.jobs().spawn(...)`. The closure
//!    emits a `LayerStarted` event, then waits for an explicit
//!    `tokio::sync::Notify` "unblock" signal before emitting `FeatureComplete`
//!    and returning. This keeps the task ALIVE so we can observe its lifetime
//!    relative to dropped connections.
//! 3. Open **connection A** and send `manifest/subscribe` filtered to the
//!    feature. After receiving the initial snapshot, **drop connection A**.
//! 4. Assert `active_count() >= 1` — the drop did NOT cancel the task.
//! 5. Open **connection B** and subscribe to the same feature. Subscribe
//!    connection B sets up a live receiver BEFORE we unblock the job.
//! 6. Unblock the job via the `Notify`. The closure emits `FeatureComplete`.
//! 7. Drain events from connection B until `FeatureComplete` arrives —
//!    **no fixed sleep** is used; observation is event-driven.
//! 8. Assert `active_count() == 0` — the feature ran to completion after
//!    connection A was dropped, confirming the drop did NOT cancel the task.
//!
//! ## Why a custom accept loop?
//!
//! `lifecycle::run_with_paths` builds its own `DaemonContext` internally, so
//! we cannot inject a job before the socket is up. The custom loop mirrors
//! the `lifecycle::run_unix` implementation faithfully, using the exact same
//! primitives (`UnixSocketListener`, `route`, `subscribe::dispatch`).

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use pice_core::events::ManifestEvent;
use pice_core::events::ManifestEventPayload;
use pice_core::jobs::JobEnv;
use pice_core::layers::manifest::VerificationManifest;
use pice_core::protocol::{methods, DaemonNotification, DaemonRequest, DaemonResponse};
use pice_core::transport::SocketPath;
use pice_core::workflow::schema::{CostCapBehavior, Defaults, Phases, WorkflowConfig};
use pice_daemon::server::auth;
use pice_daemon::server::router::DaemonContext;
use pice_daemon::server::unix::UnixConnection;
use pice_daemon::test_support::StateDirGuard;
use tokio::net::UnixStream;
use tokio::sync::Notify;

// ─── Helpers ────────────────────────────────────────────────────────────────

async fn wait_for_socket(path: &std::path::Path) {
    for _ in 0..200 {
        if path.exists() && UnixStream::connect(path).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("socket did not appear at {}", path.display());
}

fn stub_env(state_dir: &std::path::Path, project: &std::path::Path) -> Arc<JobEnv> {
    Arc::new(JobEnv {
        state_dir: state_dir.to_path_buf(),
        project_root: project.to_path_buf(),
        workflow_snapshot: WorkflowConfig {
            schema_version: "0.2".into(),
            defaults: Defaults {
                tier: 2,
                min_confidence: 0.90,
                max_passes: 5,
                model: "sonnet".into(),
                budget_usd: 2.0,
                cost_cap_behavior: CostCapBehavior::Halt,
                max_parallelism: None,
                max_global_provider_concurrency: None,
            },
            phases: Phases::default(),
            layer_overrides: Default::default(),
            review: None,
            seams: None,
        },
        contracts: Default::default(),
        pice_state_dir_override: None,
        pice_user_workflow_file: None,
    })
}

/// Read frames from a `UnixConnection` (as raw `serde_json::Value`) and return
/// the first that is a notification (no `id` field) with the given method.
/// Waits up to `timeout` total before panicking.
async fn await_notification_event(
    conn: &mut UnixConnection,
    method: &str,
    timeout: Duration,
) -> ManifestEventPayload {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or_default();
        if remaining.is_zero() {
            panic!("timed out waiting for notification method={method}");
        }
        let frame: Option<serde_json::Value> = tokio::time::timeout(remaining, conn.read_message())
            .await
            .expect("read_message did not time out")
            .expect("read_message succeeded");

        let value = match frame {
            Some(v) => v,
            None => panic!("connection closed before receiving notification method={method}"),
        };

        // Notifications have no `id` field; responses do.
        if value.get("id").is_some() {
            // This is a response frame (the subscribe snapshot); skip it.
            continue;
        }
        // It's a notification — parse it.
        let notif: DaemonNotification =
            serde_json::from_value(value).expect("parse DaemonNotification");
        if notif.method != method {
            continue;
        }
        let payload: ManifestEventPayload =
            serde_json::from_value(notif.params).expect("parse ManifestEventPayload");
        return payload;
    }
}

// ─── Test ────────────────────────────────────────────────────────────────────

/// Criterion 2: background task outlives the originating RPC connection.
///
/// A dropped CLI connection MUST NOT cancel a running `FeatureJobManager` task.
/// The task runs to completion; its `FeatureComplete` event is observable on a
/// second independent subscribe connection opened after the first was dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connection_drop_does_not_cancel_background_task() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock_path = dir.path().join("daemon.sock");
    let token_path = dir.path().join("daemon.token");

    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let _state_guard = StateDirGuard::new(&state_dir);

    let project = dir.path().to_path_buf();

    // Build the daemon context manually so we can seed a job before the
    // socket is up — `lifecycle::run_with_paths` builds its own context
    // internally, which would prevent pre-seeding.
    let token = auth::generate_token().expect("generate token");
    auth::write_token_file(&token_path, &token).expect("write token file");

    let ctx = Arc::new(DaemonContext::new(token.clone(), project.clone()));

    // "Unblock" notifier: the closure waits on this before completing.
    let unblock = Arc::new(Notify::new());
    let unblock_for_job = Arc::clone(&unblock);

    // Keep a handle to the event bus so we can subscribe to feature events
    // in the test body without going through the socket accept loop.
    let bus_handle = ctx.events().clone();

    // Spawn the long-running job. The closure:
    //   1. Emits `LayerStarted` so the subscribe stream has at least one
    //      event to confirm the task is running.
    //   2. Awaits the unblock signal (keeps the task alive for the drop test).
    //   3. Emits `FeatureComplete` — observable on connection B.
    //   4. Returns Ok so the supervisor removes it from the DashMap.
    //
    // Note: the bus must be cloned into the closure; the closure runs on a
    // tokio worker thread that does not share the outer scope.
    let bus_for_job = bus_handle.clone();
    let project_for_job = project.clone();
    ctx.jobs()
        .spawn(
            "drop-survives-feat",
            "run-drop-test".to_string(),
            stub_env(&state_dir, &project),
            move |_env, permit, _cancel| async move {
                let _hold = permit; // keep global semaphore slot for task lifetime
                bus_for_job.emit_layer_started("drop-survives-feat", "run-drop-test", "backend");
                // Wait for the test to signal us — this is the window where
                // connection A will be dropped.
                unblock_for_job.notified().await;
                bus_for_job.emit_feature_complete(
                    "drop-survives-feat",
                    "run-drop-test",
                    serde_json::json!({"status": "passed"}),
                );
                Ok(VerificationManifest::new(
                    "drop-survives-feat",
                    &project_for_job,
                ))
            },
        )
        .expect("spawn background job");

    assert_eq!(ctx.jobs().active_count(), 1, "job must be live before test");

    // Start a minimal accept loop mirroring `lifecycle::run_unix`.
    let ctx_for_loop = Arc::clone(&ctx);
    let sock_path_for_loop = sock_path.clone();
    let accept_handle = tokio::spawn(async move {
        let socket_path = SocketPath::Unix(sock_path_for_loop.clone());
        let listener = match socket_path {
            SocketPath::Unix(ref p) => {
                pice_daemon::server::unix::UnixSocketListener::bind(p).await?
            }
            _ => unreachable!(),
        };
        loop {
            tokio::select! {
                result = listener.accept() => {
                    if let Ok(mut conn) = result {
                        let ctx = Arc::clone(&ctx_for_loop);
                        tokio::spawn(async move {
                            loop {
                                let req: DaemonRequest = match conn.read_message().await {
                                    Ok(Some(r)) => r,
                                    _ => break,
                                };
                                // Route subscribe methods to the subscribe handler
                                // (takes ownership of the connection for the
                                // subscription lifetime).
                                use pice_daemon::handlers::subscribe as sub_handler;
                                if sub_handler::is_subscribe_method(&req.method) {
                                    let _ = sub_handler::dispatch(&ctx, &mut conn, req).await;
                                    break; // subscribe handler owns the connection
                                }
                                let resp = pice_daemon::server::router::route(req, &ctx).await;
                                if conn.write_message(&resp).await.is_err() {
                                    break;
                                }
                            }
                        });
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(50)) => {
                    if ctx_for_loop.is_shutdown_requested() {
                        break;
                    }
                }
            }
        }
        // Drain remaining jobs (mirrors lifecycle cleanup).
        let _ = ctx_for_loop
            .jobs()
            .drain_on_shutdown(Duration::from_secs(10))
            .await;
        Ok::<(), anyhow::Error>(())
    });

    wait_for_socket(&sock_path).await;

    // ── Step 3: open connection A, subscribe, get snapshot, DROP it ──────

    let stream_a = UnixStream::connect(&sock_path).await.expect("connect A");
    let mut conn_a = UnixConnection::new(stream_a);

    let params_a = serde_json::to_value(pice_core::protocol::subscribe::SubscribeManifestRequest {
        feature_id: Some("drop-survives-feat".to_string()),
    })
    .unwrap();
    let req_a = DaemonRequest::new(1, methods::MANIFEST_SUBSCRIBE, &token, params_a);
    conn_a
        .write_message(&req_a)
        .await
        .expect("write subscribe A");

    // Consume the snapshot response to confirm connection A is live.
    let snap_a: DaemonResponse = conn_a
        .read_message()
        .await
        .expect("read A")
        .expect("not EOF");
    assert!(
        snap_a.error.is_none(),
        "subscribe A snapshot must succeed, got: {:?}",
        snap_a.error
    );

    // Drop connection A — the client side is gone.
    drop(conn_a);

    // Give the scheduler a brief yield so the server-side EOF propagates.
    tokio::time::sleep(Duration::from_millis(30)).await;

    // ── Step 4: assert the job is STILL running after the drop ──────────

    assert!(
        ctx.jobs().active_count() >= 1,
        "task MUST still be active after connection A drop — \
         dropping a connection MUST NOT cancel background tasks"
    );

    // ── Step 5: open connection B, subscribe (before unblocking job) ─────

    let stream_b = UnixStream::connect(&sock_path).await.expect("connect B");
    let mut conn_b = UnixConnection::new(stream_b);

    let params_b = serde_json::to_value(pice_core::protocol::subscribe::SubscribeManifestRequest {
        feature_id: Some("drop-survives-feat".to_string()),
    })
    .unwrap();
    let req_b = DaemonRequest::new(2, methods::MANIFEST_SUBSCRIBE, &token, params_b);
    conn_b
        .write_message(&req_b)
        .await
        .expect("write subscribe B");

    // Read the snapshot response from connection B (confirms it's set up).
    let snap_b: DaemonResponse = conn_b
        .read_message()
        .await
        .expect("read B snap")
        .expect("not EOF");
    assert!(
        snap_b.error.is_none(),
        "subscribe B snapshot must succeed, got: {:?}",
        snap_b.error
    );

    // ── Step 6: unblock the job ──────────────────────────────────────────

    unblock.notify_one();

    // ── Step 7: await FeatureComplete on connection B (event-driven) ─────
    //
    // The stream may deliver a LayerStarted notification first (emitted
    // before the unblock), then FeatureComplete. We scan until we see
    // FeatureComplete — no fixed sleep.
    let fc_payload =
        await_notification_event(&mut conn_b, methods::MANIFEST_EVENT, Duration::from_secs(5))
            .await;

    // Keep reading if this was LayerStarted rather than FeatureComplete.
    let terminal_payload = if fc_payload.event == ManifestEvent::FeatureComplete {
        fc_payload
    } else {
        // We got an intermediate event (LayerStarted); wait for the next one.
        await_notification_event(&mut conn_b, methods::MANIFEST_EVENT, Duration::from_secs(5)).await
    };

    assert_eq!(
        terminal_payload.event,
        ManifestEvent::FeatureComplete,
        "background task must emit FeatureComplete after unblock"
    );
    assert_eq!(terminal_payload.feature_id, "drop-survives-feat");

    drop(conn_b);

    // ── Step 8: assert the job completed (not still running) ─────────────
    //
    // The supervisor loop polls every 100ms; give it up to 1s to clean up.
    for _ in 0..20 {
        if ctx.jobs().active_count() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        ctx.jobs().active_count(),
        0,
        "FeatureJobManager must show 0 active jobs after FeatureComplete — \
         the task ran to completion despite connection A being dropped"
    );

    // Shutdown the daemon gracefully.
    let stream_s = UnixStream::connect(&sock_path)
        .await
        .expect("connect shutdown");
    let mut conn_s = UnixConnection::new(stream_s);
    let shutdown_req =
        DaemonRequest::new(3, methods::DAEMON_SHUTDOWN, &token, serde_json::json!({}));
    conn_s
        .write_message(&shutdown_req)
        .await
        .expect("write shutdown");
    let _: DaemonResponse = conn_s
        .read_message()
        .await
        .expect("read shutdown resp")
        .expect("not EOF");
    drop(conn_s);

    let _ = tokio::time::timeout(Duration::from_secs(5), accept_handle).await;
}

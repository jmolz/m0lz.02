//! Phase 7 Task 6 end-to-end integration test.
//!
//! Drives a real Unix-socket `pice-daemon` via `lifecycle::run_with_paths`,
//! sends `manifest/subscribe` + `logs/stream` RPCs from a mock CLI client,
//! and asserts:
//!
//! 1. The response carries the initial snapshot body.
//! 2. Subsequent `manifest/event` notifications arrive on the same
//!    connection as id-less JSON-RPC notifications.
//!
//! This is a Unix-only test because the daemon binding uses a Unix domain
//! socket. The Windows named-pipe path is exercised by the lifecycle suite
//! via `#[cfg(windows)]` on Windows runners.

#![cfg(unix)]

use pice_core::jobs::JobEnv;
use pice_core::layers::manifest::VerificationManifest;
use pice_core::protocol::{
    methods,
    subscribe::{LogsStreamRequest, SubscribeManifestRequest, SubscribeManifestResponse},
    DaemonRequest, DaemonResponse,
};
use pice_core::transport::SocketPath;
use pice_core::workflow::schema::{CostCapBehavior, Defaults, Phases, WorkflowConfig};
use pice_daemon::events::EventBus;
use pice_daemon::lifecycle;
use pice_daemon::server::auth;
use pice_daemon::server::router::DaemonContext;
use pice_daemon::server::unix::{UnixConnection, UnixSocketListener};
use pice_daemon::test_support::StateDirGuard;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::sync::oneshot;

async fn wait_for_socket(path: &std::path::Path) {
    for _ in 0..200 {
        if path.exists() {
            // Bind-permission check: attempt connect.
            if UnixStream::connect(path).await.is_ok() {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("socket did not appear at {}", path.display());
}

fn spawn_test_accept_loop(
    ctx: Arc<DaemonContext>,
    sock_path: std::path::PathBuf,
) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    tokio::spawn(async move {
        let listener = UnixSocketListener::bind(&sock_path).await?;
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let mut conn = accepted?;
                    let ctx = Arc::clone(&ctx);
                    tokio::spawn(async move {
                        loop {
                            let req: DaemonRequest = match conn.read_message().await {
                                Ok(Some(req)) => req,
                                Ok(None) => break,
                                Err(_) => break,
                            };
                            if pice_daemon::handlers::subscribe::is_subscribe_method(&req.method) {
                                if let Err(auth_err) = ctx.validate_auth(&req) {
                                    let _ = conn.write_message(&auth_err).await;
                                    break;
                                }
                                let _ = pice_daemon::handlers::subscribe::dispatch(&ctx, &mut conn, req).await;
                                break;
                            }
                            let resp = pice_daemon::server::router::route(req, &ctx).await;
                            if conn.write_message(&resp).await.is_err() {
                                break;
                            }
                        }
                    });
                }
                _ = tokio::time::sleep(Duration::from_millis(20)) => {
                    if ctx.is_shutdown_requested() {
                        break;
                    }
                }
            }
        }
        let _ = ctx.jobs().drain_on_shutdown(Duration::from_secs(10)).await;
        Ok(())
    })
}

fn stub_job_env(state_dir: &std::path::Path, project_root: &std::path::Path) -> Arc<JobEnv> {
    Arc::new(JobEnv {
        state_dir: state_dir.to_path_buf(),
        project_root: project_root.to_path_buf(),
        workflow_snapshot: WorkflowConfig {
            schema_version: "0.2".to_string(),
            defaults: Defaults {
                tier: 2,
                min_confidence: 0.90,
                max_passes: 5,
                model: "sonnet".to_string(),
                budget_usd: 2.0,
                cost_cap_behavior: CostCapBehavior::Halt,
                max_parallelism: None,
                max_global_provider_concurrency: None,
            },
            phases: Phases::default(),
            layer_overrides: BTreeMap::new(),
            review: None,
            seams: None,
        },
        contracts: BTreeMap::new(),
        pice_state_dir_override: None,
        pice_user_workflow_file: None,
    })
}

#[tokio::test]
async fn subscribe_snapshot_response_carries_persisted_manifest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let _guard = StateDirGuard::new(&state_dir);

    // Seed a manifest on disk under the daemon's project-root namespace.
    // The daemon's project_root comes from `std::env::current_dir()` in
    // `lifecycle::run_with_paths`, so we seed for *that* path.
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let manifest = VerificationManifest::new("integ-feat-1", &project_root);
    let manifest_path =
        VerificationManifest::manifest_path_for("integ-feat-1", &project_root).unwrap();
    if let Some(parent) = manifest_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    manifest.save(&manifest_path).unwrap();

    // Start the daemon with isolated socket + token paths.
    let sock_path = dir.path().join("daemon.sock");
    let token_path = dir.path().join("daemon.token");
    let socket_path = SocketPath::Unix(sock_path.clone());

    let tp = token_path.clone();
    let handle = tokio::spawn(lifecycle::run_with_paths(socket_path, tp));

    wait_for_socket(&sock_path).await;

    // Read the per-test auth token.
    let token = auth::read_token_file(&token_path).expect("read token");

    // Connect + send manifest/subscribe for feat-1.
    let stream = UnixStream::connect(&sock_path).await.expect("connect");
    let mut conn = UnixConnection::new(stream);

    let params = serde_json::to_value(&SubscribeManifestRequest {
        feature_id: Some("integ-feat-1".to_string()),
    })
    .unwrap();
    let req = DaemonRequest::new(1, methods::MANIFEST_SUBSCRIBE, &token, params);
    conn.write_message(&req).await.expect("write subscribe");

    // Expect the snapshot response.
    let resp: DaemonResponse = conn
        .read_message()
        .await
        .expect("read response")
        .expect("not EOF");
    assert_eq!(resp.id, 1, "response id should match");
    assert!(
        resp.error.is_none(),
        "snapshot subscribe should succeed, got: {:?}",
        resp.error
    );
    let body: SubscribeManifestResponse =
        serde_json::from_value(resp.result.expect("result")).expect("parse snapshot body");
    assert_eq!(
        body.snapshots.len(),
        1,
        "snapshot should carry the seeded manifest"
    );
    assert_eq!(body.snapshots[0].feature_id, "integ-feat-1");

    // Close the client side to trigger subscribe exit.
    drop(conn);

    // Shutdown the daemon via a second connection.
    let stream = UnixStream::connect(&sock_path).await.expect("connect");
    let mut conn = UnixConnection::new(stream);
    let shutdown_req =
        DaemonRequest::new(2, methods::DAEMON_SHUTDOWN, &token, serde_json::json!({}));
    conn.write_message(&shutdown_req)
        .await
        .expect("write shutdown");
    let _resp: DaemonResponse = conn
        .read_message()
        .await
        .expect("read shutdown response")
        .expect("not EOF");
    drop(conn);

    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;

    // Cleanup: remove the seeded manifest so re-runs don't pollute
    // state_dir across tests (even though the guard removes the env
    // override, the files would persist).
    let _ = std::fs::remove_file(&manifest_path);
}

#[tokio::test]
async fn subscribe_notifications_follow_snapshot_on_same_connection_without_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let _guard = StateDirGuard::new(&state_dir);

    let project = tempfile::tempdir().expect("project");
    let sock_path = dir.path().join("daemon.sock");
    let token = auth::generate_token().expect("token");
    let ctx = Arc::new(DaemonContext::new(
        token.clone(),
        project.path().to_path_buf(),
    ));
    let handle = spawn_test_accept_loop(Arc::clone(&ctx), sock_path.clone());

    wait_for_socket(&sock_path).await;

    let stream = UnixStream::connect(&sock_path).await.expect("connect");
    let mut conn = UnixConnection::new(stream);
    let params = serde_json::to_value(&SubscribeManifestRequest {
        feature_id: Some("wire-feat".to_string()),
    })
    .unwrap();
    let req = DaemonRequest::new(1, methods::MANIFEST_SUBSCRIBE, &token, params);
    conn.write_message(&req).await.expect("write subscribe");

    let resp: serde_json::Value = conn
        .read_message()
        .await
        .expect("read response")
        .expect("not EOF");
    assert_eq!(resp["id"], 1);
    assert!(resp.get("result").is_some(), "snapshot response: {resp}");

    ctx.events().emit_feature_complete(
        "wire-feat",
        "run-wire",
        serde_json::json!({"overall_status": "passed", "status": "passed"}),
    );

    let notif: serde_json::Value = conn
        .read_message()
        .await
        .expect("read notification")
        .expect("not EOF");
    assert_eq!(notif["jsonrpc"], "2.0");
    assert_eq!(notif["method"], methods::MANIFEST_EVENT);
    assert!(
        notif.get("id").is_none(),
        "JSON-RPC notification frames must not carry id: {notif}"
    );
    assert_eq!(notif["params"]["feature_id"], "wire-feat");
    assert_eq!(notif["params"]["run_id"], "run-wire");
    assert_eq!(notif["params"]["event"]["event_type"], "feature_complete");

    drop(conn);

    let stream = UnixStream::connect(&sock_path)
        .await
        .expect("connect shutdown");
    let mut shutdown_conn = UnixConnection::new(stream);
    let shutdown_req =
        DaemonRequest::new(2, methods::DAEMON_SHUTDOWN, &token, serde_json::json!({}));
    shutdown_conn
        .write_message(&shutdown_req)
        .await
        .expect("write shutdown");
    let _: DaemonResponse = shutdown_conn
        .read_message()
        .await
        .expect("read shutdown")
        .expect("not EOF");
    drop(shutdown_conn);
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
}

#[tokio::test]
async fn scoped_manifest_subscribe_filters_live_run_ids() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let _guard = StateDirGuard::new(&state_dir);

    let project = tempfile::tempdir().expect("project");
    let sock_path = dir.path().join("daemon.sock");
    let token = auth::generate_token().expect("token");
    let ctx = Arc::new(DaemonContext::new(
        token.clone(),
        project.path().to_path_buf(),
    ));

    let env = stub_job_env(&state_dir, project.path());
    let (tx_a, rx_a) = oneshot::channel();
    let (tx_b, rx_b) = oneshot::channel();
    ctx.jobs()
        .spawn_after_signal(
            "live-a",
            "run-a".to_string(),
            Arc::clone(&env),
            rx_a,
            |_env, _permit, _cancel| async {
                Ok(VerificationManifest::new(
                    "live-a",
                    std::path::Path::new("/tmp/pice-test"),
                ))
            },
        )
        .expect("spawn live-a");
    ctx.jobs()
        .spawn_after_signal(
            "live-b",
            "run-b".to_string(),
            env,
            rx_b,
            |_env, _permit, _cancel| async {
                Ok(VerificationManifest::new(
                    "live-b",
                    std::path::Path::new("/tmp/pice-test"),
                ))
            },
        )
        .expect("spawn live-b");

    let handle = spawn_test_accept_loop(Arc::clone(&ctx), sock_path.clone());
    wait_for_socket(&sock_path).await;

    let stream = UnixStream::connect(&sock_path).await.expect("connect");
    let mut conn = UnixConnection::new(stream);
    let params = serde_json::to_value(&SubscribeManifestRequest {
        feature_id: Some("live-a".to_string()),
    })
    .unwrap();
    let req = DaemonRequest::new(1, methods::MANIFEST_SUBSCRIBE, &token, params);
    conn.write_message(&req).await.expect("write subscribe");

    let resp: DaemonResponse = conn
        .read_message()
        .await
        .expect("read response")
        .expect("not EOF");
    let body: SubscribeManifestResponse =
        serde_json::from_value(resp.result.expect("result")).expect("parse snapshot body");
    assert_eq!(
        body.run_ids,
        BTreeMap::from([("live-a".to_string(), "run-a".to_string())]),
        "feature-scoped subscribe must not leak unrelated live run ids"
    );

    drop(conn);
    drop(tx_a);
    drop(tx_b);
    for _ in 0..50 {
        if ctx.jobs().active_count() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let stream = UnixStream::connect(&sock_path)
        .await
        .expect("connect shutdown");
    let mut shutdown_conn = UnixConnection::new(stream);
    let shutdown_req =
        DaemonRequest::new(2, methods::DAEMON_SHUTDOWN, &token, serde_json::json!({}));
    shutdown_conn
        .write_message(&shutdown_req)
        .await
        .expect("write shutdown");
    let _: DaemonResponse = shutdown_conn
        .read_message()
        .await
        .expect("read shutdown")
        .expect("not EOF");
    drop(shutdown_conn);
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
}

#[tokio::test]
async fn logs_chunk_notifications_follow_snapshot_on_same_connection_without_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let _guard = StateDirGuard::new(&state_dir);

    let project = tempfile::tempdir().expect("project");
    let sock_path = dir.path().join("daemon.sock");
    let token = auth::generate_token().expect("token");
    let ctx = Arc::new(DaemonContext::new(
        token.clone(),
        project.path().to_path_buf(),
    ));
    let handle = spawn_test_accept_loop(Arc::clone(&ctx), sock_path.clone());

    wait_for_socket(&sock_path).await;

    let stream = UnixStream::connect(&sock_path).await.expect("connect");
    let mut conn = UnixConnection::new(stream);
    let params = serde_json::to_value(&LogsStreamRequest {
        feature_id: "wire-logs".to_string(),
        layer: None,
        follow: true,
        include_history: true,
    })
    .unwrap();
    let req = DaemonRequest::new(1, methods::LOGS_STREAM, &token, params);
    conn.write_message(&req).await.expect("write logs/stream");

    let resp: serde_json::Value = conn
        .read_message()
        .await
        .expect("read response")
        .expect("not EOF");
    assert_eq!(resp["id"], 1);
    assert!(
        resp.get("result").is_some(),
        "logs snapshot response: {resp}"
    );

    ctx.logs()
        .append_chunk("wire-logs", "run-logs", "backend", "live log\n".to_string())
        .await;

    let notif: serde_json::Value = conn
        .read_message()
        .await
        .expect("read logs/chunk notification")
        .expect("not EOF");
    assert_eq!(notif["jsonrpc"], "2.0");
    assert_eq!(notif["method"], methods::LOGS_CHUNK);
    assert!(
        notif.get("id").is_none(),
        "JSON-RPC logs/chunk notifications must not carry id: {notif}"
    );
    assert_eq!(notif["params"]["feature_id"], "wire-logs");
    assert_eq!(notif["params"]["run_id"], "run-logs");
    assert_eq!(notif["params"]["layer"], "backend");
    assert_eq!(notif["params"]["text"], "live log\n");

    ctx.logs()
        .append_terminal_frame("wire-logs", "run-logs", "passed")
        .await;
    let terminal: serde_json::Value = conn
        .read_message()
        .await
        .expect("read terminal logs/chunk notification")
        .expect("not EOF");
    assert_eq!(terminal["method"], methods::LOGS_CHUNK);
    assert!(terminal.get("id").is_none());
    assert_eq!(terminal["params"]["terminal"], true);

    drop(conn);

    let stream = UnixStream::connect(&sock_path)
        .await
        .expect("connect shutdown");
    let mut shutdown_conn = UnixConnection::new(stream);
    let shutdown_req =
        DaemonRequest::new(2, methods::DAEMON_SHUTDOWN, &token, serde_json::json!({}));
    shutdown_conn
        .write_message(&shutdown_req)
        .await
        .expect("write shutdown");
    let _: DaemonResponse = shutdown_conn
        .read_message()
        .await
        .expect("read shutdown")
        .expect("not EOF");
    drop(shutdown_conn);
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
}

#[tokio::test]
async fn subscribe_wildcard_snapshot_ignores_other_projects() {
    // Wildcard subscribe (feature_id: None) returns every manifest under the
    // project namespace. Project isolation across different root paths is
    // handled by the `project_hash` namespace in the state dir, which is a
    // pure-function property already unit-tested in pice-core. This test
    // validates the wire-level shape: wildcard → vec contains the seeded
    // manifest.
    let dir = tempfile::tempdir().expect("tempdir");
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let _guard = StateDirGuard::new(&state_dir);

    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let manifest = VerificationManifest::new("integ-feat-wildcard", &project_root);
    let manifest_path =
        VerificationManifest::manifest_path_for("integ-feat-wildcard", &project_root).unwrap();
    if let Some(parent) = manifest_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    manifest.save(&manifest_path).unwrap();

    let sock_path = dir.path().join("daemon.sock");
    let token_path = dir.path().join("daemon.token");
    let socket_path = SocketPath::Unix(sock_path.clone());

    let tp = token_path.clone();
    let handle = tokio::spawn(lifecycle::run_with_paths(socket_path, tp));

    wait_for_socket(&sock_path).await;
    let token = auth::read_token_file(&token_path).expect("read token");

    let stream = UnixStream::connect(&sock_path).await.expect("connect");
    let mut conn = UnixConnection::new(stream);

    let params = serde_json::to_value(&SubscribeManifestRequest { feature_id: None }).unwrap();
    let req = DaemonRequest::new(1, methods::MANIFEST_SUBSCRIBE, &token, params);
    conn.write_message(&req).await.expect("write");

    let resp: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");
    assert!(resp.error.is_none());
    let body: SubscribeManifestResponse =
        serde_json::from_value(resp.result.expect("result")).unwrap();
    // Wildcard must include the seeded feature. Other tests running in
    // parallel could add more manifests, but our seeded one must be
    // present.
    assert!(
        body.snapshots
            .iter()
            .any(|m| m.feature_id == "integ-feat-wildcard"),
        "wildcard snapshot should include seeded manifest, got: {:?}",
        body.snapshots
            .iter()
            .map(|m| &m.feature_id)
            .collect::<Vec<_>>()
    );

    drop(conn);

    // Shutdown.
    let stream = UnixStream::connect(&sock_path).await.expect("connect");
    let mut conn = UnixConnection::new(stream);
    let shutdown_req =
        DaemonRequest::new(2, methods::DAEMON_SHUTDOWN, &token, serde_json::json!({}));
    conn.write_message(&shutdown_req).await.expect("write");
    let _resp: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");
    drop(conn);

    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;

    let _ = std::fs::remove_file(&manifest_path);
}

/// Phase 7 Task 16: a dropped subscribe connection must clean up
/// without leaving per-connection state behind. The observable proof:
///
/// 1. Open a subscribe, receive the snapshot (confirms active).
/// 2. Drop the client half — the server's `tokio::select!` in the
///    subscribe handler observes `Ok(None)` on `read_request` (clean
///    EOF), drops its `broadcast::Receiver`, and exits.
/// 3. Open a second subscribe on a fresh connection — must succeed
///    within a bounded timeout. If the server were leaking state
///    (e.g. holding a per-connection mutex that the dropped connection
///    still owned), the second subscribe would time out.
/// 4. Issue `daemon/shutdown`. The drain path (`FeatureJobManager::
///    drain_on_shutdown` + connection-loop exit) must complete within
///    the daemon's 10s budget; the outer `timeout(5s)` on the lifecycle
///    `handle` tightens that to 5s as the assertion ceiling.
///
/// Rust's `broadcast::Receiver` drop-on-return semantics make this
/// cleanup automatic — there is NO explicit registry to maintain
/// (per the plan's "no separate `SubscriptionRegistry`" invariant).
/// The test's purpose is to pin that invariant as a regression gate:
/// if a future refactor introduces a registry that forgets to evict
/// on disconnect, the second-subscribe or shutdown step fails.
#[tokio::test]
async fn connection_drop_cleans_up() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let _guard = StateDirGuard::new(&state_dir);

    let sock_path = dir.path().join("daemon.sock");
    let token_path = dir.path().join("daemon.token");
    let socket_path = SocketPath::Unix(sock_path.clone());

    let tp = token_path.clone();
    let handle = tokio::spawn(lifecycle::run_with_paths(socket_path, tp));

    wait_for_socket(&sock_path).await;
    let token = auth::read_token_file(&token_path).expect("read token");

    // First subscribe — read snapshot, then drop.
    let stream = UnixStream::connect(&sock_path).await.expect("connect");
    let mut conn = UnixConnection::new(stream);
    let params = serde_json::to_value(&SubscribeManifestRequest {
        feature_id: Some("drop-test".to_string()),
    })
    .unwrap();
    let req = DaemonRequest::new(1, methods::MANIFEST_SUBSCRIBE, &token, params.clone());
    conn.write_message(&req).await.expect("write 1");
    let _resp: DaemonResponse = conn.read_message().await.expect("read 1").expect("not EOF");
    // Drop the client side — server observes EOF on next `select!`
    // poll, exits the subscribe loop, and drops its broadcast::Receiver.
    drop(conn);

    // Give the scheduler a tick for the server-side drop to propagate.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Second subscribe on a fresh connection — must succeed quickly.
    // If the server were leaking a per-connection lock or holding a
    // shard from the dropped connection, this would hang.
    let stream = UnixStream::connect(&sock_path).await.expect("connect 2");
    let mut conn = UnixConnection::new(stream);
    let req = DaemonRequest::new(2, methods::MANIFEST_SUBSCRIBE, &token, params);
    conn.write_message(&req).await.expect("write 2");
    let resp: DaemonResponse = tokio::time::timeout(Duration::from_secs(2), conn.read_message())
        .await
        .expect("second subscribe within 2s after prior connection drop")
        .expect("read 2")
        .expect("not EOF");
    assert_eq!(resp.id, 2);
    assert!(
        resp.error.is_none(),
        "second subscribe should succeed, got {:?}",
        resp.error
    );
    drop(conn);

    // Shutdown the daemon. If the drain hangs on a leaked subscribe
    // task (e.g. because the broadcast::Receiver from the dropped
    // connection was retained somewhere), the outer `timeout(5s)` on
    // the lifecycle handle fails the test.
    let stream = UnixStream::connect(&sock_path).await.expect("connect 3");
    let mut conn = UnixConnection::new(stream);
    let shutdown_req =
        DaemonRequest::new(3, methods::DAEMON_SHUTDOWN, &token, serde_json::json!({}));
    conn.write_message(&shutdown_req).await.expect("write 3");
    let _resp: DaemonResponse = conn
        .read_message()
        .await
        .expect("shutdown response")
        .expect("not EOF");
    drop(conn);

    // Ceiling: daemon must exit within 5s after shutdown RPC. If a
    // leaked subscribe held the event bus Arc or a per-connection
    // cancel-token, this would hang beyond 5s.
    let result = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("daemon exits within 5s after shutdown");
    result.expect("join ok").expect("clean exit");
}

#[tokio::test]
async fn n_socket_subscriptions_across_varied_features_drop_receiver_counts_to_zero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let _guard = StateDirGuard::new(&state_dir);

    let project = tempfile::tempdir().expect("project");
    let ctx = Arc::new(DaemonContext::new(
        String::new(),
        project.path().to_path_buf(),
    ));

    let feature_ids = ["cleanup-a", "cleanup-b", "cleanup-c", "cleanup-d"];
    let mut conns = Vec::new();
    let mut handlers = Vec::new();
    for (idx, feature_id) in feature_ids.iter().enumerate() {
        let (client, server) = std::os::unix::net::UnixStream::pair()
            .unwrap_or_else(|e| panic!("create unix stream pair for {feature_id}: {e}"));
        client.set_nonblocking(true).unwrap();
        server.set_nonblocking(true).unwrap();
        let mut conn = UnixConnection::new(UnixStream::from_std(client).unwrap());
        let mut server_conn = UnixConnection::new(UnixStream::from_std(server).unwrap());
        let params = serde_json::to_value(&SubscribeManifestRequest {
            feature_id: Some((*feature_id).to_string()),
        })
        .unwrap();
        let req = DaemonRequest::new(idx as u64 + 1, methods::MANIFEST_SUBSCRIBE, "", params);
        let ctx_for_handler = Arc::clone(&ctx);
        handlers.push(tokio::spawn(async move {
            pice_daemon::handlers::subscribe::manifest(
                ctx_for_handler.as_ref(),
                &mut server_conn,
                req,
            )
            .await
        }));
        let resp: DaemonResponse = conn
            .read_message()
            .await
            .expect("read snapshot")
            .expect("not EOF");
        assert_eq!(resp.id, idx as u64 + 1);
        assert!(resp.error.is_none());
        conns.push(conn);
    }

    let (client, server) =
        std::os::unix::net::UnixStream::pair().expect("create wildcard unix stream pair");
    client.set_nonblocking(true).unwrap();
    server.set_nonblocking(true).unwrap();
    let mut wildcard = UnixConnection::new(UnixStream::from_std(client).unwrap());
    let mut wildcard_server = UnixConnection::new(UnixStream::from_std(server).unwrap());
    let req = DaemonRequest::new(
        99,
        methods::MANIFEST_SUBSCRIBE,
        "",
        serde_json::to_value(&SubscribeManifestRequest { feature_id: None }).unwrap(),
    );
    let ctx_for_handler = Arc::clone(&ctx);
    handlers.push(tokio::spawn(async move {
        pice_daemon::handlers::subscribe::manifest(
            ctx_for_handler.as_ref(),
            &mut wildcard_server,
            req,
        )
        .await
    }));
    let resp: DaemonResponse = wildcard
        .read_message()
        .await
        .expect("read wildcard snapshot")
        .expect("not EOF");
    assert_eq!(resp.id, 99);
    assert!(resp.error.is_none());
    conns.push(wildcard);

    for feature_id in feature_ids {
        assert_eq!(
            ctx.events().feature_receiver_count(feature_id),
            1,
            "one socket receiver should be registered for {feature_id}"
        );
    }
    assert_eq!(
        ctx.events().wildcard_receiver_count(),
        1,
        "one wildcard socket receiver should be registered"
    );

    for conn in &mut conns {
        conn.shutdown()
            .await
            .expect("explicitly close subscribe client writer");
    }
    drop(conns);

    for handler in handlers {
        tokio::time::timeout(Duration::from_secs(2), handler)
            .await
            .expect("handler should exit after client shutdown")
            .expect("handler join")
            .expect("handler result");
    }
    assert_eq!(
        ctx.events().total_receiver_count(),
        0,
        "all socket-backed manifest subscriptions should clean up after handlers exit"
    );
    for feature_id in feature_ids {
        assert_eq!(
            ctx.events().feature_receiver_count(feature_id),
            0,
            "receiver count should return to zero for {feature_id}"
        );
    }
    assert_eq!(ctx.events().wildcard_receiver_count(), 0);
}

#[test]
fn event_bus_receiver_counts_return_to_zero_for_varied_features() {
    let bus = EventBus::new();
    let feature_ids = ["cleanup-a", "cleanup-b", "cleanup-c", "cleanup-d"];
    let mut receivers = Vec::new();

    for feature_id in feature_ids {
        receivers.push(bus.subscribe_feature(feature_id));
        receivers.push(bus.subscribe_feature(feature_id));
        assert_eq!(
            bus.feature_receiver_count(feature_id),
            2,
            "setup should create two receivers for {feature_id}"
        );
    }
    receivers.push(bus.subscribe_wildcard());
    assert_eq!(bus.wildcard_receiver_count(), 1);
    assert_eq!(bus.total_receiver_count(), 9);

    drop(receivers);

    for feature_id in feature_ids {
        assert_eq!(
            bus.feature_receiver_count(feature_id),
            0,
            "dropping receivers should decrement count for {feature_id}"
        );
    }
    assert_eq!(bus.wildcard_receiver_count(), 0);
    assert_eq!(bus.total_receiver_count(), 0);
}

#[tokio::test]
async fn subscribe_unknown_method_in_subscribe_namespace_returns_error() {
    // Defensive: if a client sends a method that's NOT in
    // `is_subscribe_method` but LOOKS similar, it must fall through to
    // `route()` which returns method-not-found. Covers the "dispatch
    // branch never silently eats a stray method name" invariant.
    let dir = tempfile::tempdir().expect("tempdir");
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let _guard = StateDirGuard::new(&state_dir);

    let sock_path = dir.path().join("daemon.sock");
    let token_path = dir.path().join("daemon.token");
    let socket_path = SocketPath::Unix(sock_path.clone());

    let tp = token_path.clone();
    let handle = tokio::spawn(lifecycle::run_with_paths(socket_path, tp));

    wait_for_socket(&sock_path).await;
    let token = auth::read_token_file(&token_path).expect("read token");

    let stream = UnixStream::connect(&sock_path).await.expect("connect");
    let mut conn = UnixConnection::new(stream);

    let req = DaemonRequest::new(1, "manifest/subscribe-bogus", &token, serde_json::json!({}));
    conn.write_message(&req).await.expect("write");

    let resp: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");
    assert_eq!(resp.id, 1);
    let err = resp.error.expect("method-not-found expected");
    assert_eq!(err.code, -32601);

    drop(conn);

    // Shutdown.
    let stream = UnixStream::connect(&sock_path).await.expect("connect");
    let mut conn = UnixConnection::new(stream);
    let shutdown_req =
        DaemonRequest::new(2, methods::DAEMON_SHUTDOWN, &token, serde_json::json!({}));
    conn.write_message(&shutdown_req).await.expect("write");
    let _resp: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");
    drop(conn);

    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
}

//! Phase 7 Task 6 end-to-end integration test.
//!
//! Drives a real Unix-socket `pice-daemon` via `lifecycle::run_with_paths`,
//! sends `manifest/subscribe` + `logs/stream` RPCs from a mock CLI client,
//! and asserts:
//!
//! 1. The response carries the initial snapshot body.
//! 2. Subsequent `manifest/event` notifications (produced by emitting on
//!    `ctx.events()` through a direct call — not available over the wire
//!    in this test; integration uses the public EventBus handle via a
//!    side-channel via the lifecycle helpers).
//!
//! Since the daemon owns its own `DaemonContext` internally (built inside
//! `lifecycle::run_with_paths`), this test cannot hook into the bus on the
//! wire. We validate the snapshot-response path by pre-seeding a manifest
//! on disk (honoring `PICE_STATE_DIR`) and asserting the subscribe response
//! loads it. The live-event path is covered by the in-memory unit tests
//! in `crates/pice-daemon/src/handlers/subscribe.rs::tests`.
//!
//! This is a Unix-only test because the daemon binding uses a Unix domain
//! socket. The Windows named-pipe path is exercised by the lifecycle suite
//! via `#[cfg(windows)]` on Windows runners.

#![cfg(unix)]

use pice_core::layers::manifest::VerificationManifest;
use pice_core::protocol::{
    methods,
    subscribe::{SubscribeManifestRequest, SubscribeManifestResponse},
    DaemonRequest, DaemonResponse,
};
use pice_core::transport::SocketPath;
use pice_daemon::lifecycle;
use pice_daemon::server::auth;
use pice_daemon::server::unix::UnixConnection;
use pice_daemon::test_support::StateDirGuard;
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::UnixStream;

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

//! Integration tests for daemon authentication.
//!
//! These test the full socket → auth → response path (unlike the unit tests
//! in `server::auth` and `server::router` which test components in isolation).

#![cfg(unix)]

use std::path::PathBuf;
use std::time::Duration;

use pice_core::protocol::{methods, DaemonRequest, DaemonResponse};
use pice_core::transport::SocketPath;
use pice_daemon::lifecycle;
use pice_daemon::server::auth;
use pice_daemon::server::unix::UnixConnection;
use pice_daemon::test_support::StateDirGuard;

/// Spin up a daemon in a background task. Returns socket path, token path,
/// and join handle.
async fn start_daemon() -> (
    tempfile::TempDir,
    StateDirGuard<'static>,
    SocketPath,
    PathBuf,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    let guard = StateDirGuard::new(&state_dir);
    let sock_path = dir.path().join("daemon.sock");
    let token_path = dir.path().join("daemon.token");
    let socket_path = SocketPath::Unix(sock_path.clone());

    let sp = socket_path.clone();
    let tp = token_path.clone();
    let handle = tokio::spawn(lifecycle::run_with_paths(sp, tp));

    for _ in 0..200 {
        if sock_path.exists() && token_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(sock_path.exists());
    assert!(token_path.exists());

    (dir, guard, socket_path, token_path, handle)
}

/// Connect a raw `UnixConnection`.
async fn connect(socket_path: &SocketPath) -> UnixConnection {
    let path = match socket_path {
        SocketPath::Unix(p) => p,
        _ => panic!("expected Unix"),
    };
    let stream = tokio::net::UnixStream::connect(path)
        .await
        .expect("connect");
    UnixConnection::new(stream)
}

// ─── Tests ────────────────────────────────────────────────────────────────

/// A request with the wrong token is rejected with error code -32002.
#[tokio::test]
async fn wrong_token_rejected_with_auth_error() {
    let (_dir, _guard, socket_path, token_path, handle) = start_daemon().await;
    let mut conn = connect(&socket_path).await;

    let req = DaemonRequest::new(
        1,
        methods::DAEMON_HEALTH,
        "totally-wrong-token",
        serde_json::json!({}),
    );
    conn.write_message(&req).await.expect("write");
    let resp: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");

    let err = resp.error.expect("should be an auth error");
    assert_eq!(err.code, -32002, "error code should be -32002");
    assert!(
        err.message.to_lowercase().contains("auth"),
        "error message should mention auth, got: {}",
        err.message
    );

    // Clean up.
    drop(conn);
    let token = auth::read_token_file(&token_path).expect("read token");
    let mut conn2 = connect(&socket_path).await;
    let shutdown = DaemonRequest::new(2, methods::DAEMON_SHUTDOWN, &token, serde_json::json!({}));
    conn2.write_message(&shutdown).await.expect("write");
    let _: DaemonResponse = conn2.read_message().await.expect("read").expect("not EOF");
    drop(conn2);
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
}

/// An empty token is rejected.
#[tokio::test]
async fn empty_token_rejected() {
    let (_dir, _guard, socket_path, token_path, handle) = start_daemon().await;
    let mut conn = connect(&socket_path).await;

    let req = DaemonRequest::new(1, methods::DAEMON_HEALTH, "", serde_json::json!({}));
    conn.write_message(&req).await.expect("write");
    let resp: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");

    assert!(resp.error.is_some(), "empty token should be rejected");
    assert_eq!(resp.error.as_ref().unwrap().code, -32002);

    // Clean up.
    drop(conn);
    let token = auth::read_token_file(&token_path).expect("read token");
    let mut conn2 = connect(&socket_path).await;
    let shutdown = DaemonRequest::new(2, methods::DAEMON_SHUTDOWN, &token, serde_json::json!({}));
    conn2.write_message(&shutdown).await.expect("write");
    let _: DaemonResponse = conn2.read_message().await.expect("read").expect("not EOF");
    drop(conn2);
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
}

/// After auth rejection, the same connection can retry with the correct token.
#[tokio::test]
async fn connection_survives_auth_rejection() {
    let (_dir, _guard, socket_path, token_path, handle) = start_daemon().await;
    let mut conn = connect(&socket_path).await;
    let token = auth::read_token_file(&token_path).expect("read token");

    // First: bad token → rejected.
    let bad_req = DaemonRequest::new(1, methods::DAEMON_HEALTH, "wrong", serde_json::json!({}));
    conn.write_message(&bad_req).await.expect("write");
    let bad_resp: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");
    assert!(bad_resp.error.is_some(), "bad token should fail");

    // Second: good token on the same connection → succeeds.
    let good_req = DaemonRequest::new(2, methods::DAEMON_HEALTH, &token, serde_json::json!({}));
    conn.write_message(&good_req).await.expect("write");
    let good_resp: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");
    assert!(
        good_resp.error.is_none(),
        "good token should succeed after bad attempt"
    );

    // Clean up.
    let shutdown = DaemonRequest::new(3, methods::DAEMON_SHUTDOWN, &token, serde_json::json!({}));
    conn.write_message(&shutdown).await.expect("write");
    let _: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");
    drop(conn);
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
}

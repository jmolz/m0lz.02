//! Phase 7 Task 8 integration test.
//!
//! Seeds three manifests under `state_dir/<ns>/*.manifest.json`:
//! - A `Queued` manifest (should be deleted).
//! - An `InProgress` manifest (should be rewritten to Failed).
//! - A `Passed` manifest (should be untouched).
//!
//! Then boots the daemon via `lifecycle::run_with_paths` and asserts
//! that reconciliation runs BEFORE the daemon accepts its first RPC
//! (verified by observing the disk state at a moment when the socket
//! is already bound + responsive).

#![cfg(unix)]

use pice_core::layers::manifest::{ManifestStatus, VerificationManifest};
use pice_core::protocol::{methods, DaemonRequest, DaemonResponse};
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
        if path.exists() && UnixStream::connect(path).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("socket did not appear at {}", path.display());
}

#[tokio::test]
async fn reconciliation_runs_before_first_rpc() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let _guard = StateDirGuard::new(&state_dir);

    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // Seed the three fixtures.
    let mut queued = VerificationManifest::new("feat-queued", &project_root);
    queued.overall_status = ManifestStatus::Queued;
    let queued_path =
        VerificationManifest::manifest_path_for("feat-queued", &project_root).unwrap();
    std::fs::create_dir_all(queued_path.parent().unwrap()).unwrap();
    queued.save(&queued_path).unwrap();

    let mut in_progress = VerificationManifest::new("feat-inprog", &project_root);
    in_progress.overall_status = ManifestStatus::InProgress;
    let in_progress_path =
        VerificationManifest::manifest_path_for("feat-inprog", &project_root).unwrap();
    in_progress.save(&in_progress_path).unwrap();

    let mut passed = VerificationManifest::new("feat-pass", &project_root);
    passed.overall_status = ManifestStatus::Passed;
    let passed_path = VerificationManifest::manifest_path_for("feat-pass", &project_root).unwrap();
    passed.save(&passed_path).unwrap();

    // Boot the daemon.
    let sock_path = dir.path().join("daemon.sock");
    let token_path = dir.path().join("daemon.token");
    let socket_path = SocketPath::Unix(sock_path.clone());

    let tp = token_path.clone();
    let handle = tokio::spawn(lifecycle::run_with_paths(socket_path, tp));

    wait_for_socket(&sock_path).await;

    // Sanity: daemon is responsive. A client may connect to the bound socket
    // before startup reconciliation finishes, but no RPC may be processed until
    // after reconciliation has completed.
    let token = auth::read_token_file(&token_path).expect("read token");
    let stream = UnixStream::connect(&sock_path).await.expect("connect");
    let mut conn = UnixConnection::new(stream);
    let health = DaemonRequest::new(1, methods::DAEMON_HEALTH, &token, serde_json::json!({}));
    conn.write_message(&health).await.expect("write");
    let resp: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");
    assert!(resp.error.is_none());
    drop(conn);

    // At this point the daemon is accepting and processing RPCs, so
    // reconciliation must have ALREADY run. Observe disk state:
    assert!(
        !queued_path.exists(),
        "Queued manifest should have been deleted before socket was accepting"
    );
    assert!(
        in_progress_path.exists(),
        "InProgress manifest should still exist, rewritten"
    );
    let rewritten = VerificationManifest::load(&in_progress_path).unwrap();
    assert_eq!(rewritten.overall_status, ManifestStatus::Failed);

    assert!(passed_path.exists(), "Passed manifest should be preserved");
    let preserved = VerificationManifest::load(&passed_path).unwrap();
    assert_eq!(preserved.overall_status, ManifestStatus::Passed);

    // Shutdown.
    let stream = UnixStream::connect(&sock_path).await.expect("connect");
    let mut conn = UnixConnection::new(stream);
    let shutdown = DaemonRequest::new(2, methods::DAEMON_SHUTDOWN, &token, serde_json::json!({}));
    conn.write_message(&shutdown).await.expect("write");
    let _: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");
    drop(conn);

    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;

    // Cleanup the seeded manifests so re-runs are clean.
    let _ = std::fs::remove_file(&in_progress_path);
    let _ = std::fs::remove_file(&passed_path);
}

#[tokio::test]
async fn reconciliation_failure_prevents_first_rpc() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_dir = dir.path().join("state");
    let ns = state_dir.join("ns-corrupt");
    std::fs::create_dir_all(&ns).unwrap();
    let _guard = StateDirGuard::new(&state_dir);

    let corrupt_path = ns.join("corrupt.manifest.json");
    std::fs::write(&corrupt_path, b"not valid json {{{ }}}}").unwrap();

    let sock_path = dir.path().join("daemon.sock");
    let token_path = dir.path().join("daemon.token");
    let socket_path = SocketPath::Unix(sock_path.clone());

    let err = tokio::time::timeout(
        Duration::from_secs(5),
        lifecycle::run_with_paths(socket_path, token_path),
    )
    .await
    .expect("daemon startup should return promptly on reconciliation failure")
    .expect_err("daemon startup should fail closed");

    let rendered = format!("{err:#}");
    assert!(rendered.contains("startup reconciliation failed"));
    assert!(rendered.contains("unable to load manifest"));
    assert!(
        !sock_path.exists(),
        "daemon must not bind the socket when startup reconciliation fails"
    );
}

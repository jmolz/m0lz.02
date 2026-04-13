//! Integration tests for the daemon lifecycle.
//!
//! These complement the unit tests in `lifecycle.rs` and `router.rs` by
//! exercising the full path: socket bind → accept → auth → route → handler
//! → response, with isolated paths in tempdirs. Each test spawns its own
//! daemon in a background tokio task, so tests run in parallel without
//! interfering.
//!
//! **What these tests cover that unit tests don't:**
//! - Multiple sequential RPCs on a single connection (connection reuse)
//! - Multiple independent clients dispatching concurrently
//! - Socket file cleanup after orderly shutdown
//! - Full command dispatch roundtrip through the handler layer

#![cfg(unix)]

use std::path::PathBuf;
use std::time::Duration;

use pice_core::cli::{CommandRequest, CommandResponse, StatusRequest};
use pice_core::protocol::{methods, DaemonRequest, DaemonResponse};
use pice_core::transport::SocketPath;
use pice_daemon::lifecycle;
use pice_daemon::server::auth;
use pice_daemon::server::unix::UnixConnection;

/// Spin up a daemon in a background task, returning the socket path, token
/// path, and join handle. Waits for the socket to appear before returning.
async fn start_daemon() -> (
    tempfile::TempDir,
    SocketPath,
    PathBuf,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock_path = dir.path().join("daemon.sock");
    let token_path = dir.path().join("daemon.token");
    let socket_path = SocketPath::Unix(sock_path.clone());

    let sp = socket_path.clone();
    let tp = token_path.clone();
    let handle = tokio::spawn(lifecycle::run_with_paths(sp, tp));

    // Wait for socket to appear.
    for _ in 0..200 {
        if sock_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(sock_path.exists(), "socket should exist after startup");

    (dir, socket_path, token_path, handle)
}

/// Connect a raw `UnixConnection` to the daemon, returning the connection
/// and the auth token.
async fn raw_connect(socket_path: &SocketPath, token_path: &PathBuf) -> (UnixConnection, String) {
    let path = match socket_path {
        SocketPath::Unix(p) => p,
        _ => panic!("expected Unix socket path"),
    };
    let stream = tokio::net::UnixStream::connect(path)
        .await
        .expect("connect");
    let conn = UnixConnection::new(stream);
    let token = auth::read_token_file(token_path).expect("read token");
    (conn, token)
}

/// Send a request and read the response on an existing connection.
async fn rpc(conn: &mut UnixConnection, id: u64, method: &str, token: &str) -> DaemonResponse {
    let req = DaemonRequest::new(id, method, token, serde_json::json!({}));
    conn.write_message(&req).await.expect("write");
    conn.read_message::<DaemonResponse>()
        .await
        .expect("read")
        .expect("not EOF")
}

// ─── Tests ────────────────────────────────────────────────────────────────

/// Full lifecycle: health → dispatch status → dispatch prime → shutdown.
///
/// Proves that a single connection can issue multiple sequential RPCs.
#[tokio::test]
async fn full_lifecycle_multiple_rpcs_on_one_connection() {
    let (_dir, socket_path, token_path, handle) = start_daemon().await;
    let (mut conn, token) = raw_connect(&socket_path, &token_path).await;

    // 1. Health check.
    let health = rpc(&mut conn, 1, methods::DAEMON_HEALTH, &token).await;
    assert!(health.error.is_none());
    let result = health.result.expect("health result");
    assert!(result["version"].as_str().is_some());
    assert!(result["uptime_seconds"].as_u64().is_some());

    // 2. Dispatch a status command (no provider needed).
    let status_req = CommandRequest::Status(StatusRequest { json: true });
    let params = serde_json::to_value(&status_req).expect("serialize");
    let dispatch_req = DaemonRequest::new(2, methods::CLI_DISPATCH, &token, params);
    conn.write_message(&dispatch_req).await.expect("write");
    let status_resp: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");
    assert!(
        status_resp.error.is_none(),
        "status dispatch should succeed"
    );
    let cmd_resp: CommandResponse =
        serde_json::from_value(status_resp.result.expect("result")).expect("deserialize");
    match cmd_resp {
        CommandResponse::Json { value } => {
            assert!(value["plans"].is_array(), "status json should have plans");
        }
        other => panic!("expected Json, got: {other:?}"),
    }

    // 3. Dispatch an evaluate command with missing plan (no provider needed).
    let eval_req = CommandRequest::Evaluate(pice_core::cli::EvaluateRequest {
        plan_path: "nonexistent.md".into(),
        json: false,
    });
    let params = serde_json::to_value(&eval_req).expect("serialize");
    let dispatch_req = DaemonRequest::new(3, methods::CLI_DISPATCH, &token, params);
    conn.write_message(&dispatch_req).await.expect("write");
    let eval_resp: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");
    assert!(eval_resp.error.is_none(), "eval dispatch should succeed (Exit response, not error)");

    // 4. Shutdown.
    let shutdown_resp = rpc(&mut conn, 99, methods::DAEMON_SHUTDOWN, &token).await;
    assert!(shutdown_resp.error.is_none());
    drop(conn);
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
}

/// Two clients dispatch concurrently — both should complete independently.
///
/// Proves the per-connection handler loop (tokio::spawn per accept) works.
#[tokio::test]
async fn concurrent_clients_complete_independently() {
    let (_dir, socket_path, token_path, handle) = start_daemon().await;

    // Open two independent connections.
    let (mut conn_a, token_a) = raw_connect(&socket_path, &token_path).await;
    let (mut conn_b, token_b) = raw_connect(&socket_path, &token_path).await;

    // Dispatch on both concurrently.
    let a = tokio::spawn(async move {
        let req = CommandRequest::Status(StatusRequest { json: false });
        let params = serde_json::to_value(&req).unwrap();
        let daemon_req = DaemonRequest::new(1, methods::CLI_DISPATCH, &token_a, params);
        conn_a.write_message(&daemon_req).await.unwrap();
        let resp: DaemonResponse = conn_a.read_message().await.unwrap().unwrap();
        assert!(resp.error.is_none(), "client A should succeed");
        conn_a
    });
    let b = tokio::spawn(async move {
        let req = CommandRequest::Status(StatusRequest { json: true });
        let params = serde_json::to_value(&req).unwrap();
        let daemon_req = DaemonRequest::new(2, methods::CLI_DISPATCH, &token_b, params);
        conn_b.write_message(&daemon_req).await.unwrap();
        let resp: DaemonResponse = conn_b.read_message().await.unwrap().unwrap();
        assert!(resp.error.is_none(), "client B should succeed");
        conn_b
    });

    let mut conn_a = a.await.expect("join A");
    let _conn_b = b.await.expect("join B");

    // Clean shutdown via client A.
    let token = auth::read_token_file(&token_path).expect("read token");
    let shutdown_resp = rpc(&mut conn_a, 99, methods::DAEMON_SHUTDOWN, &token).await;
    assert!(shutdown_resp.error.is_none());
    drop(conn_a);
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
}

/// After orderly shutdown, the socket file should be removed.
#[tokio::test]
async fn shutdown_removes_socket_file() {
    let (dir, socket_path, token_path, handle) = start_daemon().await;
    let sock_path = dir.path().join("daemon.sock");
    assert!(sock_path.exists(), "socket should exist before shutdown");

    let (mut conn, token) = raw_connect(&socket_path, &token_path).await;
    let resp = rpc(&mut conn, 1, methods::DAEMON_SHUTDOWN, &token).await;
    assert!(resp.error.is_none());
    drop(conn);

    // Wait for daemon to exit.
    let result = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("daemon exits within 5s")
        .expect("join handle");
    result.expect("clean exit");

    // Socket file should be gone (UnixSocketListener::drop removes it).
    assert!(
        !sock_path.exists(),
        "socket file should be removed after shutdown"
    );
}

/// Dispatch non-provider command types to verify handler wiring is complete.
///
/// Provider-dependent commands (prime, plan, review, handoff) are tested in
/// the integration test suite with the stub provider. Here we only test
/// commands that complete without spawning a provider process.
#[tokio::test]
async fn all_command_types_dispatch_successfully() {
    use pice_core::cli::*;

    let (_dir, socket_path, token_path, handle) = start_daemon().await;
    let (mut conn, token) = raw_connect(&socket_path, &token_path).await;

    // Commands that don't need a provider — should all return success responses.
    let no_provider_commands: Vec<(&str, CommandRequest)> = vec![
        (
            "init",
            CommandRequest::Init(InitRequest {
                force: false,
                json: false,
            }),
        ),
        (
            "execute-missing",
            CommandRequest::Execute(ExecuteRequest {
                plan_path: "nonexistent.md".into(),
                json: false,
            }),
        ),
        (
            "evaluate-missing",
            CommandRequest::Evaluate(EvaluateRequest {
                plan_path: "nonexistent.md".into(),
                json: false,
            }),
        ),
        (
            "commit-nothing-staged",
            CommandRequest::Commit(CommitRequest {
                message: None,
                dry_run: false,
                json: false,
            }),
        ),
        (
            "status",
            CommandRequest::Status(StatusRequest { json: false }),
        ),
        (
            "metrics",
            CommandRequest::Metrics(MetricsRequest {
                csv: false,
                json: false,
            }),
        ),
        (
            "benchmark",
            CommandRequest::Benchmark(BenchmarkRequest { json: false }),
        ),
    ];

    for (i, (name, cmd)) in no_provider_commands.into_iter().enumerate() {
        let params = serde_json::to_value(&cmd).expect("serialize");
        let req = DaemonRequest::new((i + 10) as u64, methods::CLI_DISPATCH, &token, params);
        conn.write_message(&req).await.expect("write");
        let resp: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");
        // All of these should complete without error — they either succeed
        // or return CommandResponse::Exit (which is still a success at the
        // RPC level, just an application-level failure).
        assert!(
            resp.error.is_none(),
            "{name} dispatch failed: {:?}",
            resp.error
        );
    }

    // Shutdown.
    let resp = rpc(&mut conn, 99, methods::DAEMON_SHUTDOWN, &token).await;
    assert!(resp.error.is_none());
    drop(conn);
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
}

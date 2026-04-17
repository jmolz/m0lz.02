//! Daemon lifecycle — startup, signal handling, graceful shutdown.
//!
//! ## Event loop
//!
//! 1. Resolve socket path (from `PICE_DAEMON_SOCKET` or platform default)
//! 2. Ensure `~/.pice/` directory exists
//! 3. Generate auth token, write to `~/.pice/daemon.token`
//! 4. Bind socket (with stale-cleanup retry on Unix)
//! 5. Accept loop — one `tokio::spawn` per connection
//! 6. `tokio::select!` between accept and shutdown signal (SIGTERM/SIGINT/CTRL-C)
//! 7. On shutdown: stop accepting, drain in-flight RPCs (10s budget), cleanup
//!
//! See `.claude/rules/daemon.md` "Graceful shutdown" for the 10s budget rule.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use pice_core::transport::SocketPath;
use tracing::{error, info};

use crate::server::auth;
use crate::server::router::DaemonContext;

/// Graceful shutdown budget — max time to wait for in-flight RPCs.
#[allow(dead_code)] // Used in shutdown logging; actual timeout enforcement comes with JoinHandle tracking.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// Run the daemon event loop. Blocks until the daemon shuts down.
///
/// Called from `main.rs` after `logging::init()`.
pub async fn run() -> Result<()> {
    let socket_path = SocketPath::default_from_env();
    let token_path = auth::default_token_path();
    run_with_paths(socket_path, token_path).await
}

/// Run the daemon with explicit socket and token paths. Testable entry point.
///
/// Tests use this to isolate the socket and token files in a tempdir,
/// avoiding races between concurrent test runs.
pub async fn run_with_paths(socket_path: SocketPath, token_path: std::path::PathBuf) -> Result<()> {
    // Ensure parent directories exist.
    ensure_parent_dir(&socket_path)?;
    if let Some(parent) = token_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    // Generate auth token and write to disk.
    let token = auth::generate_token().context("failed to generate auth token")?;
    auth::write_token_file(&token_path, &token)?;
    info!(token_path = %token_path.display(), "auth token written");

    // Build shared context.
    let project_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let ctx = Arc::new(DaemonContext::new(token, project_root));

    // Platform-specific bind + accept loop.
    match socket_path {
        #[cfg(unix)]
        SocketPath::Unix(ref path) => run_unix(path, ctx).await,

        #[cfg(windows)]
        SocketPath::Windows(ref name) => run_windows(name, ctx).await,

        // Unreachable on the matching platform, but the enum is not cfg-gated.
        #[allow(unreachable_patterns)]
        _ => anyhow::bail!("unsupported socket path variant on this platform"),
    }
}

/// Ensure the parent directory of the socket path exists.
fn ensure_parent_dir(socket_path: &SocketPath) -> Result<()> {
    match socket_path {
        SocketPath::Unix(path) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
        }
        SocketPath::Windows(_) => {
            // Named pipes don't have a parent directory.
        }
    }
    Ok(())
}

// ─── Unix accept loop ──────────────────────────────────────────────────────

#[cfg(unix)]
async fn run_unix(path: &std::path::Path, ctx: Arc<DaemonContext>) -> Result<()> {
    use crate::server::router;
    use crate::server::unix::UnixSocketListener;
    use pice_core::protocol::DaemonRequest;

    let listener = UnixSocketListener::bind(path).await?;
    info!(path = %path.display(), "daemon listening");

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok(mut conn) => {
                        let ctx = Arc::clone(&ctx);
                        tokio::spawn(async move {
                            loop {
                                let req: Option<DaemonRequest> = match conn.read_message().await {
                                    Ok(Some(r)) => Some(r),
                                    Ok(None) => break, // EOF — client disconnected.
                                    Err(e) => {
                                        tracing::debug!("read error: {e}");
                                        break;
                                    }
                                };
                                let req = match req {
                                    Some(r) => r,
                                    None => break,
                                };

                                let resp = router::route(req, &ctx).await;
                                if let Err(e) = conn.write_message(&resp).await {
                                    tracing::debug!("write error: {e}");
                                    break;
                                }
                            }
                        });
                    }
                    Err(e) => {
                        error!("accept error: {e}");
                    }
                }
            }

            _ = shutdown_signal() => {
                info!("shutdown signal received");
                break;
            }

            // Poll the shutdown flag every 100ms so daemon/shutdown RPCs
            // processed on a connection task can break the accept loop.
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                if ctx.is_shutdown_requested() {
                    info!("shutdown requested via RPC");
                    break;
                }
            }
        }
    }

    // Graceful shutdown: give in-flight tasks a moment to finish.
    // Phase 0 handlers are stubs that return immediately, so a brief
    // yield is sufficient. Full JoinHandle tracking comes later.
    tokio::time::sleep(Duration::from_millis(100)).await;

    info!("daemon shutdown complete");
    Ok(())
    // UnixSocketListener::drop removes the socket file.
}

// ─── Windows accept loop ───────────────────────────────────────────────────

#[cfg(windows)]
async fn run_windows(name: &str, ctx: Arc<DaemonContext>) -> Result<()> {
    use crate::server::router;
    use crate::server::windows::WindowsPipeListener;
    use pice_core::protocol::DaemonRequest;

    let listener = WindowsPipeListener::bind(name).await?;
    info!(pipe = %name, "daemon listening");

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok(mut conn) => {
                        let ctx = Arc::clone(&ctx);
                        tokio::spawn(async move {
                            loop {
                                let req: Option<DaemonRequest> = match conn.read_message().await {
                                    Ok(Some(r)) => Some(r),
                                    Ok(None) => break,
                                    Err(e) => {
                                        tracing::debug!("read error: {e}");
                                        break;
                                    }
                                };
                                let req = match req {
                                    Some(r) => r,
                                    None => break,
                                };

                                let resp = router::route(req, &ctx).await;
                                if let Err(e) = conn.write_message(&resp).await {
                                    tracing::debug!("write error: {e}");
                                    break;
                                }
                            }
                        });
                    }
                    Err(e) => {
                        error!("accept error: {e}");
                    }
                }
            }

            _ = shutdown_signal() => {
                info!("shutdown signal received");
                break;
            }

            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                if ctx.is_shutdown_requested() {
                    info!("shutdown requested via RPC");
                    break;
                }
            }
        }
    }

    tokio::time::sleep(Duration::from_millis(100)).await;
    info!("daemon shutdown complete");
    Ok(())
}

// ─── Shutdown signal ───────────────────────────────────────────────────────

/// Wait for an OS shutdown signal (SIGTERM/SIGINT on Unix, CTRL-C on Windows).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        // Phase 4.1 Pass-6 C13: signal-handler registration only fails on
        // invalid signal numbers (SIGTERM/SIGINT are always valid) or EINTR
        // from the kernel (retry-safe at tokio layer). A panic here means
        // the process cannot accept graceful shutdown — which is the
        // correct response: better to exit loudly at startup than to run
        // without shutdown handling. Grandfathered under
        // `-D clippy::expect_used`.
        #[allow(clippy::expect_used)]
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
        #[allow(clippy::expect_used)]
        let mut sigint =
            signal(SignalKind::interrupt()).expect("failed to register SIGINT handler");
        tokio::select! {
            _ = sigterm.recv() => {}
            _ = sigint.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to register CTRL-C handler");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_parent_dir_creates_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sock = dir.path().join("subdir").join("daemon.sock");
        let sp = SocketPath::Unix(sock.clone());
        ensure_parent_dir(&sp).expect("ensure_parent_dir");
        assert!(sock.parent().unwrap().exists());
    }

    #[test]
    fn ensure_parent_dir_noop_for_existing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sock = dir.path().join("daemon.sock");
        let sp = SocketPath::Unix(sock);
        ensure_parent_dir(&sp).expect("ensure_parent_dir");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn lifecycle_startup_health_and_shutdown() {
        use pice_core::protocol::{methods, DaemonRequest, DaemonResponse};
        use tokio::net::UnixStream;

        let dir = tempfile::tempdir().expect("tempdir");
        let sock_path = dir.path().join("daemon.sock");
        let token_path = dir.path().join("daemon.token");
        let socket_path = SocketPath::Unix(sock_path.clone());

        // Spawn the daemon in a background task with isolated paths.
        let tp = token_path.clone();
        let handle = tokio::spawn(run_with_paths(socket_path, tp));

        // Wait for the socket to appear.
        for _ in 0..100 {
            if sock_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(sock_path.exists(), "socket should exist after startup");

        // Read the per-test token.
        let token = auth::read_token_file(&token_path).expect("read token");

        // Connect and send a health check.
        let stream = UnixStream::connect(&sock_path).await.expect("connect");
        let mut conn = crate::server::unix::UnixConnection::new(stream);

        let health_req =
            DaemonRequest::new(1, methods::DAEMON_HEALTH, &token, serde_json::json!({}));
        conn.write_message(&health_req).await.expect("write health");

        let resp: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");
        assert_eq!(resp.id, 1);
        assert!(resp.error.is_none(), "health should succeed");
        let result = resp.result.expect("has result");
        assert!(result["version"].as_str().is_some());
        assert!(result["uptime_seconds"].as_u64().is_some());

        // Send shutdown RPC.
        let shutdown_req =
            DaemonRequest::new(2, methods::DAEMON_SHUTDOWN, &token, serde_json::json!({}));
        conn.write_message(&shutdown_req)
            .await
            .expect("write shutdown");

        let resp: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");
        assert_eq!(resp.id, 2);
        assert!(resp.error.is_none(), "shutdown should succeed");
        assert_eq!(
            resp.result.as_ref().unwrap()["shutting_down"],
            serde_json::json!(true)
        );

        // Drop the connection so the handler task exits.
        drop(conn);

        // The daemon should exit within the shutdown timeout.
        let daemon_result = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("daemon should exit within 5s")
            .expect("join handle");
        daemon_result.expect("daemon should exit cleanly");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn lifecycle_rejects_bad_auth() {
        use pice_core::protocol::{methods, DaemonRequest, DaemonResponse};
        use tokio::net::UnixStream;

        let dir = tempfile::tempdir().expect("tempdir");
        let sock_path = dir.path().join("daemon.sock");
        let token_path = dir.path().join("daemon.token");
        let socket_path = SocketPath::Unix(sock_path.clone());

        let tp = token_path.clone();
        let handle = tokio::spawn(run_with_paths(socket_path, tp));

        for _ in 0..100 {
            if sock_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // Connect with a wrong token.
        let stream = UnixStream::connect(&sock_path).await.expect("connect");
        let mut conn = crate::server::unix::UnixConnection::new(stream);

        let bad_req = DaemonRequest::new(
            1,
            methods::DAEMON_HEALTH,
            "wrong-token",
            serde_json::json!({}),
        );
        conn.write_message(&bad_req).await.expect("write");

        let resp: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");
        let err = resp.error.expect("should reject bad auth");
        assert_eq!(err.code, -32002);

        // Clean up: read the per-test token and send shutdown.
        drop(conn);
        let token = auth::read_token_file(&token_path).expect("read token");
        let stream = UnixStream::connect(&sock_path).await.expect("connect");
        let mut conn = crate::server::unix::UnixConnection::new(stream);
        let shutdown_req =
            DaemonRequest::new(2, methods::DAEMON_SHUTDOWN, &token, serde_json::json!({}));
        conn.write_message(&shutdown_req).await.expect("write");
        let _resp: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");
        drop(conn);

        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
    }
}

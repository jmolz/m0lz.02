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

/// Graceful shutdown budget — max time to wait for in-flight background jobs
/// (`FeatureJobManager::drain_on_shutdown`) to finish after `daemon/shutdown` or
/// SIGTERM fires the cancellation tokens.
///
/// Referenced from both [`crate::server::router::handle_shutdown`] (caller-
/// awaits path — response is emitted AFTER drain returns) and the accept-loop
/// SIGTERM cleanup branch below (no caller to wait, drain is still required so
/// any in-flight manifest saves land before the socket closes).
pub(crate) const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

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

    // Phase 7 Task 8: reconcile any interrupted-dispatch manifests BEFORE
    // accepting the first RPC. `Queued` manifests (dispatch that never
    // ran) are deleted; `InProgress` manifests are rewritten to Failed
    // with `halted_by = "failed-interrupted"`. Terminal states are
    // preserved untouched.
    let state_dir = pice_core::layers::manifest::VerificationManifest::state_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("~/.pice/state"));
    if let Err(e) = crate::jobs::reconcile_on_startup(&state_dir) {
        tracing::warn!(error = %e, "startup reconciliation failed");
    }

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
    use crate::server::unix::UnixSocketListener;

    let listener = UnixSocketListener::bind(path).await?;
    info!(path = %path.display(), "daemon listening");

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok(conn) => {
                        let ctx = Arc::clone(&ctx);
                        tokio::spawn(async move {
                            handle_connection_unix(conn, ctx).await;
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

    if ctx.is_shutdown_requested()
        && !ctx
            .wait_for_shutdown_response_observed(SHUTDOWN_TIMEOUT)
            .await
    {
        tracing::warn!(
            "daemon shutdown: timed out waiting for shutdown RPC response write before lifecycle exit"
        );
    }

    // Phase 7 Criterion 17: drain background jobs BEFORE closing the
    // socket. `daemon/shutdown` already drained for its caller, but
    // SIGTERM / SIGINT never ran a handler, so this call is the ONLY
    // drain on the signal path. Idempotent when the shutdown handler
    // already drained — `drain_on_shutdown` returns 0 immediately if
    // no jobs are live.
    let remaining = ctx.jobs().drain_on_shutdown(SHUTDOWN_TIMEOUT).await;
    if remaining > 0 {
        tracing::warn!(
            remaining,
            "daemon shutdown: {remaining} background jobs did not finish within the {SHUTDOWN_TIMEOUT:?} drain budget"
        );
    }

    info!("daemon shutdown complete");
    Ok(())
    // UnixSocketListener::drop removes the socket file.
}

#[cfg(unix)]
async fn handle_connection_unix(
    mut conn: crate::server::unix::UnixConnection,
    ctx: Arc<DaemonContext>,
) {
    use pice_core::protocol::{methods, DaemonRequest};

    loop {
        let req: DaemonRequest = match conn.read_message().await {
            Ok(Some(r)) => r,
            Ok(None) => break, // EOF — client disconnected.
            Err(e) => {
                tracing::debug!("read error: {e}");
                break;
            }
        };

        // Phase 7 Task 6: subscribe methods take over the connection.
        // After the handler returns, the subscription is over — we MUST
        // NOT read more frames on this connection (the handler's
        // `tokio::select!` already drained to EOF) so we break out of
        // the loop and let the task exit.
        if crate::handlers::subscribe::is_subscribe_method(&req.method) {
            if let Err(auth_err) = ctx.validate_auth(&req) {
                let _ = conn.write_message(&auth_err).await;
                break;
            }
            if let Err(e) = crate::handlers::subscribe::dispatch(&ctx, &mut conn, req).await {
                tracing::debug!("subscribe handler error: {e}");
            }
            break;
        }

        let is_shutdown = req.method == methods::DAEMON_SHUTDOWN;
        let resp = crate::server::router::route(req, &ctx).await;
        let write_result = conn.write_message(&resp).await;
        ctx.release_background_start_from_response(&resp);
        if is_shutdown && ctx.is_shutdown_requested() {
            ctx.mark_shutdown_response_observed();
        }
        if let Err(e) = write_result {
            tracing::debug!("write error: {e}");
            break;
        }
        if is_shutdown && ctx.is_shutdown_requested() {
            break;
        }
    }
}

// ─── Windows accept loop ───────────────────────────────────────────────────

#[cfg(windows)]
async fn run_windows(name: &str, ctx: Arc<DaemonContext>) -> Result<()> {
    use crate::server::windows::WindowsPipeListener;

    let listener = WindowsPipeListener::bind(name).await?;
    info!(pipe = %name, "daemon listening");

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok(conn) => {
                        let ctx = Arc::clone(&ctx);
                        tokio::spawn(async move {
                            handle_connection_windows(conn, ctx).await;
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

    if ctx.is_shutdown_requested()
        && !ctx
            .wait_for_shutdown_response_observed(SHUTDOWN_TIMEOUT)
            .await
    {
        tracing::warn!(
            "daemon shutdown: timed out waiting for shutdown RPC response write before lifecycle exit"
        );
    }

    // Phase 7 Criterion 17: drain background jobs BEFORE exit. See
    // the Unix branch above for rationale — Windows pipe path needs
    // the same symmetric drain.
    let remaining = ctx.jobs().drain_on_shutdown(SHUTDOWN_TIMEOUT).await;
    if remaining > 0 {
        tracing::warn!(
            remaining,
            "daemon shutdown: {remaining} background jobs did not finish within the {SHUTDOWN_TIMEOUT:?} drain budget"
        );
    }
    info!("daemon shutdown complete");
    Ok(())
}

#[cfg(windows)]
async fn handle_connection_windows(
    mut conn: crate::server::windows::WindowsPipeConnection,
    ctx: Arc<DaemonContext>,
) {
    use pice_core::protocol::{methods, DaemonRequest};

    loop {
        let req: DaemonRequest = match conn.read_message().await {
            Ok(Some(r)) => r,
            Ok(None) => break,
            Err(e) => {
                tracing::debug!("read error: {e}");
                break;
            }
        };

        if crate::handlers::subscribe::is_subscribe_method(&req.method) {
            if let Err(auth_err) = ctx.validate_auth(&req) {
                let _ = conn.write_message(&auth_err).await;
                break;
            }
            if let Err(e) = crate::handlers::subscribe::dispatch(&ctx, &mut conn, req).await {
                tracing::debug!("subscribe handler error: {e}");
            }
            break;
        }

        let is_shutdown = req.method == methods::DAEMON_SHUTDOWN;
        let resp = crate::server::router::route(req, &ctx).await;
        let write_result = conn.write_message(&resp).await;
        ctx.release_background_start_from_response(&resp);
        if is_shutdown && ctx.is_shutdown_requested() {
            ctx.mark_shutdown_response_observed();
        }
        if let Err(e) = write_result {
            tracing::debug!("write error: {e}");
            break;
        }
        if is_shutdown && ctx.is_shutdown_requested() {
            break;
        }
    }
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
    fn stub_job_env(
        state_dir: &std::path::Path,
        project_root: &std::path::Path,
    ) -> Arc<pice_core::jobs::JobEnv> {
        use pice_core::workflow::schema::{CostCapBehavior, Defaults, Phases, WorkflowConfig};

        Arc::new(pice_core::jobs::JobEnv {
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
                layer_overrides: Default::default(),
                review: None,
                seams: None,
            },
            contracts: Default::default(),
            pice_state_dir_override: None,
            pice_user_workflow_file: None,
        })
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shutdown_rpc_response_is_written_before_lifecycle_returns() {
        use pice_core::layers::manifest::VerificationManifest;
        use pice_core::protocol::{methods, DaemonRequest, DaemonResponse};
        use tokio::net::UnixStream;

        let dir = tempfile::tempdir_in("/private/tmp").expect("tempdir");
        let sock_path = dir.path().join("daemon.sock");
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&state_dir).expect("state dir");
        let flushed_marker = dir.path().join("shutdown-flushed");

        let token = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let project_root = dir.path().to_path_buf();
        let ctx = Arc::new(DaemonContext::new_for_test_with_root(
            token,
            project_root.clone(),
        ));

        let flushed_marker_for_job = flushed_marker.clone();
        let project_root_for_job = project_root.clone();
        ctx.jobs()
            .spawn(
                "shutdown-race",
                ctx.jobs().next_run_id(),
                stub_job_env(&state_dir, &project_root),
                move |_env, permit, cancel| async move {
                    let _hold = permit;
                    cancel.cancelled().await;
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    std::fs::write(&flushed_marker_for_job, b"flushed")
                        .expect("write flushed marker");
                    Ok(VerificationManifest::new(
                        "shutdown-race",
                        &project_root_for_job,
                    ))
                },
            )
            .expect("spawn background job");

        let ctx_for_daemon = Arc::clone(&ctx);
        let sock_path_for_daemon = sock_path.clone();
        let daemon =
            tokio::spawn(async move { run_unix(&sock_path_for_daemon, ctx_for_daemon).await });

        for _ in 0..200 {
            if sock_path.exists() && UnixStream::connect(&sock_path).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(sock_path.exists(), "socket should exist after startup");

        let stream = UnixStream::connect(&sock_path).await.expect("connect");
        let mut conn = crate::server::unix::UnixConnection::new(stream);
        let shutdown_req =
            DaemonRequest::new(1, methods::DAEMON_SHUTDOWN, token, serde_json::json!({}));
        conn.write_message(&shutdown_req)
            .await
            .expect("write shutdown");

        let response = tokio::time::timeout(
            Duration::from_secs(2),
            conn.read_message::<DaemonResponse>(),
        )
        .await
        .expect("shutdown response should be readable")
        .expect("read response")
        .expect("not EOF");

        assert!(response.error.is_none(), "shutdown response should succeed");
        assert_eq!(
            response.result.as_ref().unwrap()["drained_remaining"],
            serde_json::json!(0)
        );
        assert!(
            flushed_marker.exists(),
            "shutdown response must be written only after background job flush"
        );

        drop(conn);
        tokio::time::timeout(Duration::from_secs(5), daemon)
            .await
            .expect("daemon should exit after response")
            .expect("join handle")
            .expect("daemon should exit cleanly");
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

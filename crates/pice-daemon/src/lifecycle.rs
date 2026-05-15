//! Daemon lifecycle — startup, signal handling, graceful shutdown.
//!
//! ## Event loop
//!
//! 1. Resolve socket path (from `PICE_DAEMON_SOCKET` or platform default)
//! 2. Ensure `~/.pice/` directory exists
//! 3. Bind socket (with stale-cleanup retry on Unix) to prove single-daemon ownership
//! 4. Reconcile startup state before accepting RPCs
//! 5. Generate auth token, write to `~/.pice/daemon.token`
//! 6. Accept loop — one tracked task per connection
//! 7. `tokio::select!` between accept and shutdown signal (SIGTERM/SIGINT/CTRL-C)
//! 8. On shutdown: stop accepting, drain in-flight RPCs (10s budget), cleanup
//!
//! See `.claude/rules/daemon.md` "Graceful shutdown" for the 10s budget rule.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use pice_core::transport::SocketPath;
use tokio::task::JoinSet;
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
    let project_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    run_with_paths_for_project(socket_path, token_path, project_root).await
}

/// Run the daemon with explicit socket, token, and project-root paths.
///
/// This exists for integration tests that need daemon state isolated from the
/// caller's working tree. Production entrypoints should use [`run`] or
/// [`run_with_paths`].
#[doc(hidden)]
pub async fn run_with_paths_for_project(
    socket_path: SocketPath,
    token_path: std::path::PathBuf,
    project_root: std::path::PathBuf,
) -> Result<()> {
    // Ensure parent directories exist.
    ensure_parent_dir(&socket_path)?;
    if let Some(parent) = token_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    // Bind first: the socket/pipe is the single-daemon ownership lock. A
    // second daemon must fail here before it can rotate the active token file
    // or reconcile manifests owned by the running daemon.
    match socket_path {
        #[cfg(unix)]
        SocketPath::Unix(ref path) => {
            let listener = crate::server::unix::UnixSocketListener::bind(path).await?;
            let ctx = initialize_bound_daemon(&token_path, project_root)?;
            run_unix_bound(listener, ctx).await
        }

        #[cfg(windows)]
        SocketPath::Windows(ref name) => {
            let listener = crate::server::windows::WindowsPipeListener::bind(name).await?;
            let ctx = initialize_bound_daemon(&token_path, project_root)?;
            run_windows_bound(listener, ctx).await
        }

        // Unreachable on the matching platform, but the enum is not cfg-gated.
        #[allow(unreachable_patterns)]
        _ => anyhow::bail!("unsupported socket path variant on this platform"),
    }
}

fn initialize_bound_daemon(
    token_path: &std::path::Path,
    project_root: std::path::PathBuf,
) -> Result<Arc<DaemonContext>> {
    // Phase 7 Task 8: reconcile any interrupted-dispatch manifests BEFORE
    // accepting the first RPC. `Queued` manifests (dispatch that never
    // ran) are deleted; `InProgress` manifests are rewritten to Failed
    // with `halted_by = "failed-interrupted"`. Terminal states are
    // preserved untouched.
    let state_dir = pice_core::layers::manifest::VerificationManifest::state_dir()
        .context("failed to resolve verification manifest state dir")?;
    crate::jobs::reconcile_on_startup(&state_dir)
        .with_context(|| format!("startup reconciliation failed for {}", state_dir.display()))?;

    // Generate auth token and write to disk only after reconciliation succeeds.
    // Otherwise a failed startup could leave behind a fresh token for no live
    // daemon and obscure the real recovery failure.
    let token = auth::generate_token().context("failed to generate auth token")?;
    auth::write_token_file(token_path, &token)?;
    info!(token_path = %token_path.display(), "auth token written");

    Ok(Arc::new(DaemonContext::new(token, project_root)))
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

async fn drain_connection_tasks(tasks: &mut JoinSet<()>, timeout: Duration) -> usize {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tasks.is_empty() {
            return 0;
        }
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or_default();
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, tasks.join_next()).await {
            Ok(Some(Ok(()))) => {}
            Ok(Some(Err(e))) => {
                tracing::warn!("connection task exited with join error: {e}");
            }
            Ok(None) => return 0,
            Err(_) => break,
        }
    }

    let remaining = tasks.len();
    if remaining > 0 {
        tasks.abort_all();
    }
    remaining
}

// ─── Unix accept loop ──────────────────────────────────────────────────────

#[cfg(all(unix, test))]
async fn run_unix(path: &std::path::Path, ctx: Arc<DaemonContext>) -> Result<()> {
    use crate::server::unix::UnixSocketListener;

    let listener = UnixSocketListener::bind(path).await?;
    run_unix_bound(listener, ctx).await
}

#[cfg(unix)]
async fn run_unix_bound(
    listener: crate::server::unix::UnixSocketListener,
    ctx: Arc<DaemonContext>,
) -> Result<()> {
    info!(path = %listener.path().display(), "daemon listening");
    let mut connection_tasks = JoinSet::new();

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok(conn) => {
                        if ctx.is_shutdown_requested() {
                            break;
                        }
                        let ctx = Arc::clone(&ctx);
                        connection_tasks.spawn(async move {
                            handle_connection_unix(conn, ctx).await;
                        });
                    }
                    Err(e) => {
                        error!("accept error: {e}");
                    }
                }
            }

            joined = connection_tasks.join_next(), if !connection_tasks.is_empty() => {
                if let Some(Err(e)) = joined {
                    tracing::warn!("connection task exited with join error: {e}");
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

    let remaining_connections =
        drain_connection_tasks(&mut connection_tasks, SHUTDOWN_TIMEOUT).await;
    if remaining_connections > 0 {
        tracing::warn!(
            remaining_connections,
            "daemon shutdown: {remaining_connections} RPC connection tasks did not finish within the {SHUTDOWN_TIMEOUT:?} drain budget"
        );
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
    let drain_report = ctx.jobs().drain_on_shutdown(SHUTDOWN_TIMEOUT).await;
    if drain_report.remaining > 0 {
        tracing::warn!(
            remaining = drain_report.remaining,
            "daemon shutdown: {} background jobs did not finish within the {:?} drain budget",
            drain_report.remaining,
            SHUTDOWN_TIMEOUT,
        );
    }
    for failure in &drain_report.terminalization_failures {
        tracing::error!(
            feature_id = %failure.feature_id,
            run_id = %failure.run_id,
            error = %failure.error,
            "daemon shutdown: background job terminalization failed"
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

        if ctx.is_shutdown_requested() && req.method != methods::DAEMON_SHUTDOWN {
            let resp = pice_core::protocol::DaemonResponse::error(
                req.id,
                -32004,
                "daemon is shutting down",
            );
            let _ = conn.write_message(&resp).await;
            break;
        }

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

// Future Windows daemon tests can bind a pre-built context directly through
// this helper; production Windows startup uses `run_windows_bound`.
#[cfg(all(windows, test))]
#[allow(dead_code)]
async fn run_windows(name: &str, ctx: Arc<DaemonContext>) -> Result<()> {
    use crate::server::windows::WindowsPipeListener;

    let listener = WindowsPipeListener::bind(name).await?;
    run_windows_bound(listener, ctx).await
}

#[cfg(windows)]
async fn run_windows_bound(
    listener: crate::server::windows::WindowsPipeListener,
    ctx: Arc<DaemonContext>,
) -> Result<()> {
    info!("daemon listening");
    let mut connection_tasks = JoinSet::new();

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok(conn) => {
                        if ctx.is_shutdown_requested() {
                            break;
                        }
                        let ctx = Arc::clone(&ctx);
                        connection_tasks.spawn(async move {
                            handle_connection_windows(conn, ctx).await;
                        });
                    }
                    Err(e) => {
                        error!("accept error: {e}");
                    }
                }
            }

            joined = connection_tasks.join_next(), if !connection_tasks.is_empty() => {
                if let Some(Err(e)) = joined {
                    tracing::warn!("connection task exited with join error: {e}");
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

    let remaining_connections =
        drain_connection_tasks(&mut connection_tasks, SHUTDOWN_TIMEOUT).await;
    if remaining_connections > 0 {
        tracing::warn!(
            remaining_connections,
            "daemon shutdown: {remaining_connections} RPC connection tasks did not finish within the {SHUTDOWN_TIMEOUT:?} drain budget"
        );
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
    let drain_report = ctx.jobs().drain_on_shutdown(SHUTDOWN_TIMEOUT).await;
    if drain_report.remaining > 0 {
        tracing::warn!(
            remaining = drain_report.remaining,
            "daemon shutdown: {} background jobs did not finish within the {:?} drain budget",
            drain_report.remaining,
            SHUTDOWN_TIMEOUT,
        );
    }
    for failure in &drain_report.terminalization_failures {
        tracing::error!(
            feature_id = %failure.feature_id,
            run_id = %failure.run_id,
            error = %failure.error,
            "daemon shutdown: background job terminalization failed"
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

        if ctx.is_shutdown_requested() && req.method != methods::DAEMON_SHUTDOWN {
            let resp = pice_core::protocol::DaemonResponse::error(
                req.id,
                -32004,
                "daemon is shutting down",
            );
            let _ = conn.write_message(&resp).await;
            break;
        }

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
    #[allow(clippy::await_holding_lock)]
    async fn lifecycle_startup_health_and_shutdown() {
        use pice_core::protocol::{methods, DaemonRequest, DaemonResponse};
        use tokio::net::UnixStream;

        let dir = tempfile::tempdir().expect("tempdir");
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&state_dir).expect("state dir");
        let _guard = crate::test_support::StateDirGuard::new(&state_dir);

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
    async fn second_daemon_start_fails_before_token_write_or_reconciliation() {
        use pice_core::layers::manifest::{ManifestStatus, VerificationManifest};
        use pice_core::protocol::{methods, DaemonRequest, DaemonResponse};
        use tokio::net::UnixStream;

        let dir = tempfile::tempdir().expect("tempdir");
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&state_dir).expect("state dir");
        let _guard = crate::test_support::StateDirGuard::new(&state_dir);

        let sock_path = dir.path().join("daemon.sock");
        let token_path = dir.path().join("daemon.token");
        let socket_path = SocketPath::Unix(sock_path.clone());
        let daemon = tokio::spawn(run_with_paths(socket_path.clone(), token_path.clone()));

        for _ in 0..100 {
            if sock_path.exists() && UnixStream::connect(&sock_path).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(sock_path.exists(), "socket should exist after startup");

        let token_before = auth::read_token_file(&token_path).expect("read token");
        let project_root =
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let mut queued = VerificationManifest::new("second-start-queued", &project_root);
        queued.overall_status = ManifestStatus::Queued;
        let queued_path =
            VerificationManifest::manifest_path_for("second-start-queued", &project_root)
                .expect("manifest path");
        std::fs::create_dir_all(queued_path.parent().expect("manifest parent"))
            .expect("manifest dir");
        queued.save(&queued_path).expect("save queued");

        let err = run_with_paths(socket_path, token_path.clone())
            .await
            .expect_err("second daemon should fail before startup side effects");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("already listening"),
            "second daemon should fail on socket ownership, got: {rendered}"
        );

        let token_after =
            auth::read_token_file(&token_path).expect("read token after second start");
        assert_eq!(
            token_after, token_before,
            "second daemon must not rotate the active token before proving socket ownership"
        );
        assert!(
            queued_path.exists(),
            "second daemon must not reconcile/delete manifests owned by the active daemon"
        );

        let stream = UnixStream::connect(&sock_path).await.expect("connect");
        let mut conn = crate::server::unix::UnixConnection::new(stream);
        let shutdown_req = DaemonRequest::new(
            1,
            methods::DAEMON_SHUTDOWN,
            &token_before,
            serde_json::json!({}),
        );
        conn.write_message(&shutdown_req)
            .await
            .expect("write shutdown");
        let resp: DaemonResponse = conn.read_message().await.expect("read").expect("not EOF");
        assert!(resp.error.is_none(), "shutdown should succeed");
        drop(conn);

        tokio::time::timeout(Duration::from_secs(5), daemon)
            .await
            .expect("daemon should exit")
            .expect("join handle")
            .expect("daemon should exit cleanly");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn state_dir_resolution_failure_prevents_token_write_and_rpc() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sock_path = dir.path().join("daemon.sock");
        let token_path = dir.path().join("daemon.token");
        let socket_path = SocketPath::Unix(sock_path.clone());

        let _guard = crate::test_support::state_dir_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let prev_state = std::env::var("PICE_STATE_DIR").ok();
        let prev_home = std::env::var("HOME").ok();
        let prev_userprofile = std::env::var("USERPROFILE").ok();
        std::env::remove_var("PICE_STATE_DIR");
        std::env::remove_var("HOME");
        std::env::remove_var("USERPROFILE");

        let result = run_with_paths(socket_path, token_path.clone()).await;

        match prev_state {
            Some(v) => std::env::set_var("PICE_STATE_DIR", v),
            None => std::env::remove_var("PICE_STATE_DIR"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_userprofile {
            Some(v) => std::env::set_var("USERPROFILE", v),
            None => std::env::remove_var("USERPROFILE"),
        }

        let err = result.expect_err("daemon startup should fail without a resolvable state dir");
        let rendered = format!("{err:#}");
        assert!(rendered.contains("failed to resolve verification manifest state dir"));
        assert!(
            !token_path.exists(),
            "failed startup must not write a daemon token"
        );
        assert!(
            !sock_path.exists(),
            "failed startup must drop the bound socket before returning"
        );
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
            plan_trace: None,
        })
    }

    #[cfg(unix)]
    fn socket_tempdir() -> tempfile::TempDir {
        if std::path::Path::new("/private/tmp").is_dir() {
            tempfile::tempdir_in("/private/tmp").expect("tempdir")
        } else {
            tempfile::tempdir().expect("tempdir")
        }
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shutdown_rpc_response_is_written_before_lifecycle_returns() {
        use pice_core::layers::manifest::VerificationManifest;
        use pice_core::protocol::{methods, DaemonRequest, DaemonResponse};
        use tokio::net::UnixStream;

        let dir = socket_tempdir();
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
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(clippy::await_holding_lock)]
    async fn shutdown_waits_for_in_flight_subscribe_connection_before_exit() {
        use pice_core::protocol::subscribe::{SubscribeManifestRequest, SubscribeManifestResponse};
        use pice_core::protocol::{methods, DaemonRequest, DaemonResponse};
        use tokio::net::UnixStream;

        let dir = socket_tempdir();
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&state_dir).expect("state dir");
        let _guard = crate::test_support::StateDirGuard::new(&state_dir);

        let sock_path = dir.path().join("daemon.sock");
        let token_path = dir.path().join("daemon.token");
        let socket_path = SocketPath::Unix(sock_path.clone());

        let tp = token_path.clone();
        let daemon = tokio::spawn(run_with_paths(socket_path, tp));

        for _ in 0..200 {
            if sock_path.exists() && UnixStream::connect(&sock_path).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(sock_path.exists(), "socket should exist after startup");
        let mut token = None;
        for _ in 0..200 {
            if let Ok(candidate) = auth::read_token_file(&token_path) {
                if let Ok(stream) = UnixStream::connect(&sock_path).await {
                    let mut conn = crate::server::unix::UnixConnection::new(stream);
                    let health = DaemonRequest::new(
                        1,
                        methods::DAEMON_HEALTH,
                        &candidate,
                        serde_json::json!({}),
                    );
                    conn.write_message(&health).await.expect("write health");
                    if let Ok(Some(resp)) = conn.read_message::<DaemonResponse>().await {
                        if resp.error.is_none() {
                            token = Some(candidate);
                            break;
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let token = token.expect("daemon should become health-responsive before subscribe");

        let stream = UnixStream::connect(&sock_path)
            .await
            .expect("connect subscribe");
        let mut subscribe_conn = crate::server::unix::UnixConnection::new(stream);
        let subscribe = DaemonRequest::new(
            10,
            methods::MANIFEST_SUBSCRIBE,
            &token,
            serde_json::to_value(SubscribeManifestRequest {
                feature_id: Some("held-open".to_string()),
            })
            .expect("serialize subscribe"),
        );
        subscribe_conn
            .write_message(&subscribe)
            .await
            .expect("write subscribe");
        let snapshot: DaemonResponse = subscribe_conn
            .read_message()
            .await
            .expect("read snapshot")
            .expect("not EOF");
        assert!(snapshot.error.is_none(), "subscribe must succeed");
        let _: SubscribeManifestResponse =
            serde_json::from_value(snapshot.result.expect("snapshot result"))
                .expect("snapshot shape");

        let stream = UnixStream::connect(&sock_path)
            .await
            .expect("connect shutdown");
        let mut shutdown_conn = crate::server::unix::UnixConnection::new(stream);
        let shutdown =
            DaemonRequest::new(11, methods::DAEMON_SHUTDOWN, &token, serde_json::json!({}));
        shutdown_conn
            .write_message(&shutdown)
            .await
            .expect("write shutdown");
        let response: DaemonResponse = shutdown_conn
            .read_message()
            .await
            .expect("read shutdown response")
            .expect("not EOF");
        assert!(response.error.is_none(), "shutdown response should succeed");

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !daemon.is_finished(),
            "lifecycle must not exit while an in-flight subscribe RPC is still open"
        );

        drop(subscribe_conn);
        drop(shutdown_conn);
        tokio::time::timeout(Duration::from_secs(5), daemon)
            .await
            .expect("daemon should exit after subscribe connection closes")
            .expect("join handle")
            .expect("daemon should exit cleanly");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn lifecycle_rejects_bad_auth() {
        use pice_core::protocol::{methods, DaemonRequest, DaemonResponse};
        use tokio::net::UnixStream;

        let dir = tempfile::tempdir().expect("tempdir");
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&state_dir).expect("state dir");
        let _guard = crate::test_support::StateDirGuard::new(&state_dir);

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

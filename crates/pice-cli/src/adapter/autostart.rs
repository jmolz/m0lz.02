//! Daemon auto-start — ensures a running daemon before dispatching commands.
//!
//! The CLI calls [`ensure_daemon_running`] before every socket-mode dispatch.
//! If the daemon is already listening, this returns a connected client
//! immediately (~5ms). If not, it spawns `pice-daemon` as a detached child
//! and polls the socket until the daemon responds to a health check.
//!
//! ## Auto-start sequence
//!
//! 1. Try `daemon/health` RPC with 100ms timeout
//! 2. If healthy: return the active connection
//! 3. If not: spawn `pice-daemon` as a detached child process
//! 4. Poll the socket every 10ms for up to 2000ms
//! 5. Return the connection, or error with "daemon failed to start within 2s"

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use pice_core::transport::SocketPath;
use pice_daemon::server::auth;
use tracing::info;

use super::transport::DaemonClient;

/// Health-check timeout for the "is the daemon already running?" probe.
const HEALTH_TIMEOUT: Duration = Duration::from_millis(100);

/// Maximum time to wait for a freshly-spawned daemon to become ready.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(2);

/// Polling interval during daemon startup wait.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Ensure a running daemon and return a connected, health-checked client.
///
/// Resolves socket and token paths from environment/platform defaults.
pub async fn ensure_daemon_running() -> Result<DaemonClient> {
    let socket_path = SocketPath::default_from_env();
    let token_path = auth::default_token_path();
    ensure_daemon_running_with_paths(&socket_path, &token_path).await
}

/// Ensure a running daemon with explicit paths. Testable entry point.
///
/// Tests use this to isolate the socket and token files in a tempdir,
/// avoiding races between concurrent test runs.
pub async fn ensure_daemon_running_with_paths(
    socket_path: &SocketPath,
    token_path: &Path,
) -> Result<DaemonClient> {
    // Fast path: try to connect + health check within 100ms.
    if let Ok(Ok(client)) = tokio::time::timeout(
        HEALTH_TIMEOUT,
        try_connect_and_health(socket_path, token_path),
    )
    .await
    {
        return Ok(client);
    }

    // Daemon not running — spawn it.
    info!("daemon not running, starting...");
    spawn_daemon()?;

    // Poll for the daemon to become healthy.
    let deadline = tokio::time::Instant::now() + STARTUP_TIMEOUT;
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "daemon failed to start within {}s — check PICE_DAEMON_BIN or `pice-daemon` on PATH",
                STARTUP_TIMEOUT.as_secs()
            );
        }
        if let Ok(client) = try_connect_and_health(socket_path, token_path).await {
            info!("daemon started successfully");
            return Ok(client);
        }
    }
}

/// Attempt a single connect + health check. Returns the client on success.
async fn try_connect_and_health(
    socket_path: &SocketPath,
    token_path: &Path,
) -> Result<DaemonClient> {
    let mut client = DaemonClient::connect(socket_path, token_path).await?;
    client.health_check().await?;
    Ok(client)
}

/// Spawn `pice-daemon` as a detached child process.
///
/// The child inherits nothing from the CLI (stdin/stdout/stderr are all null).
/// The CLI does not wait on the child — it becomes a daemon that outlives
/// the CLI process.
///
/// `pub(crate)` because `commands::daemon::cmd_start` also uses this.
pub(crate) fn spawn_daemon() -> Result<()> {
    let mut command = std::process::Command::new(daemon_binary_path());
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{
            CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS,
        };

        // On Windows CI, callers frequently run `pice daemon start` with
        // stdout/stderr captured. The daemon must be fully detached so the
        // parent command can exit without waiting for inherited console/pipe
        // handles held by the long-running child process.
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    command
        .spawn()
        .context("failed to spawn pice-daemon — is it installed and in PATH?")?;
    Ok(())
}

fn daemon_binary_path() -> PathBuf {
    std::env::var_os("PICE_DAEMON_BIN")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("pice-daemon"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn daemon_binary_path_uses_env_override() {
        let _guard = env_lock().lock().expect("env lock");
        let previous = std::env::var_os("PICE_DAEMON_BIN");
        let override_path = if cfg!(windows) {
            r"C:\tools\pice-daemon.exe"
        } else {
            "/tmp/pice-daemon"
        };

        std::env::set_var("PICE_DAEMON_BIN", override_path);
        assert_eq!(daemon_binary_path(), PathBuf::from(override_path));

        match previous {
            Some(value) => std::env::set_var("PICE_DAEMON_BIN", value),
            None => std::env::remove_var("PICE_DAEMON_BIN"),
        }
    }

    #[test]
    fn daemon_binary_path_falls_back_to_path_lookup() {
        let _guard = env_lock().lock().expect("env lock");
        let previous = std::env::var_os("PICE_DAEMON_BIN");
        std::env::remove_var("PICE_DAEMON_BIN");
        assert_eq!(daemon_binary_path(), PathBuf::from("pice-daemon"));

        match previous {
            Some(value) => std::env::set_var("PICE_DAEMON_BIN", value),
            None => std::env::remove_var("PICE_DAEMON_BIN"),
        }
    }

    /// Test the "daemon already running" fast path: start a daemon in a
    /// background task, then call `ensure_daemon_running_with_paths`.
    #[cfg(unix)]
    #[tokio::test]
    async fn ensure_daemon_running_connects_to_existing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sock_path = dir.path().join("daemon.sock");
        let token_path = dir.path().join("daemon.token");
        let socket_path = SocketPath::Unix(sock_path.clone());
        let state_tmp = tempfile::tempdir().expect("state tempdir");
        let _state_guard = pice_daemon::test_support::StateDirGuard::new(state_tmp.path());

        // Start a daemon in a background task.
        let sp = socket_path.clone();
        let tp = token_path.clone();
        let handle = tokio::spawn(pice_daemon::lifecycle::run_with_paths(sp, tp));

        // Wait for socket + token; debug builds can take more than one
        // second on loaded CI workers.
        for _ in 0..500 {
            if sock_path.exists() && token_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // ensure_daemon_running should find the existing daemon.
        let mut client = ensure_daemon_running_with_paths(&socket_path, &token_path)
            .await
            .expect("should connect to existing daemon");

        // Dispatch should work through the returned client.
        let req = pice_core::cli::CommandRequest::Status(pice_core::cli::StatusRequest {
            json: false,
            ..Default::default()
        });
        let resp = client.dispatch(req).await.expect("dispatch");
        match resp {
            pice_core::cli::CommandResponse::Text { content } => {
                assert!(content.contains("PICE Status"));
            }
            other => panic!("expected Text, got: {other:?}"),
        }

        // Clean up: shutdown the daemon.
        client.shutdown().await.expect("shutdown");
        drop(client);
        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
    }

    /// Test that `ensure_daemon_running_with_paths` fails cleanly when no
    /// daemon is running and spawn_daemon would fail (pice-daemon not in PATH
    /// for this test, so it times out or fails to spawn).
    ///
    /// This test is intentionally NOT run in CI because it depends on
    /// `pice-daemon` not being in PATH. Instead, the auto-start path is
    /// covered by the integration test in T25.
    #[cfg(unix)]
    #[tokio::test]
    async fn ensure_fails_when_daemon_not_available() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sock_path = dir.path().join("daemon.sock");
        let token_path = dir.path().join("daemon.token");
        let socket_path = SocketPath::Unix(sock_path);

        // No daemon running, no binary to spawn — should fail.
        // The timeout is 2s, so we give it 3s to avoid flaky failures.
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            ensure_daemon_running_with_paths(&socket_path, &token_path),
        )
        .await;

        match result {
            Ok(Err(_)) => {} // Expected: spawn failure or timeout
            Ok(Ok(_)) => panic!("should not succeed without a running daemon"),
            Err(_) => {} // Outer timeout — spawn_daemon hung or polling took too long
        }
    }
}

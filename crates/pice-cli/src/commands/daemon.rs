//! `pice daemon` subcommand — manage the daemon process lifecycle.
//!
//! Unlike every other CLI command, these operations do NOT go through
//! `adapter::dispatch()`. They manage the daemon itself: starting, stopping,
//! querying status, and reading logs.  The daemon subcommand operates
//! entirely at the CLI layer.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use pice_core::transport::SocketPath;
use pice_daemon::server::auth;

use crate::adapter::autostart;
use crate::adapter::transport::DaemonClient;

/// Arguments for `pice daemon <action>`.
#[derive(Args, Debug)]
pub struct DaemonArgs {
    #[command(subcommand)]
    pub action: DaemonAction,
}

/// Daemon lifecycle actions.
#[derive(Subcommand, Debug)]
pub enum DaemonAction {
    /// Start the daemon (if not already running)
    Start,
    /// Stop a running daemon
    Stop,
    /// Show daemon status (version, uptime, socket path)
    Status,
    /// Restart the daemon (stop + start)
    Restart,
    /// Show daemon log output
    Logs {
        /// Follow the log file (like `tail -f`)
        #[arg(long)]
        follow: bool,
    },
}

/// Maximum time to wait for a freshly-spawned daemon during `start`.
const START_TIMEOUT: Duration = Duration::from_secs(5);

/// Polling interval during `start` wait.
const START_POLL: Duration = Duration::from_millis(50);

pub async fn run(args: &DaemonArgs) -> Result<()> {
    let socket_path = SocketPath::default_from_env();
    let token_path = auth::default_token_path();

    match &args.action {
        DaemonAction::Start => cmd_start(&socket_path, &token_path).await,
        DaemonAction::Stop => cmd_stop(&socket_path, &token_path).await,
        DaemonAction::Status => cmd_status(&socket_path, &token_path).await,
        DaemonAction::Restart => {
            // Stop first (ignore error if daemon wasn't running).
            let _ = cmd_stop(&socket_path, &token_path).await;
            cmd_start(&socket_path, &token_path).await
        }
        DaemonAction::Logs { follow } => cmd_logs(*follow),
    }
}

// ─── Subcommand implementations ───────────────────────────────────────────

/// `pice daemon start` — explicit start (override auto-start).
///
/// If the daemon is already running, prints a message and returns. Otherwise,
/// spawns `pice-daemon` as a detached child and polls until it responds to a
/// health check.
async fn cmd_start(socket_path: &SocketPath, token_path: &Path) -> Result<()> {
    // Check if already running.
    if try_health(socket_path, token_path).await.is_ok() {
        println!("daemon is already running");
        return Ok(());
    }

    println!("starting daemon...");
    autostart::spawn_daemon()?;

    // Poll until healthy.
    let deadline = tokio::time::Instant::now() + START_TIMEOUT;
    loop {
        tokio::time::sleep(START_POLL).await;
        if tokio::time::Instant::now() >= deadline {
            bail!("daemon failed to start within {}s", START_TIMEOUT.as_secs());
        }
        if try_health(socket_path, token_path).await.is_ok() {
            println!("daemon started");
            return Ok(());
        }
    }
}

/// `pice daemon stop` — send `daemon/shutdown` RPC.
///
/// Connects to the daemon, sends shutdown, and confirms. If the daemon is
/// not running, prints a message and returns success (idempotent).
async fn cmd_stop(socket_path: &SocketPath, token_path: &Path) -> Result<()> {
    let mut client = match DaemonClient::connect(socket_path, token_path).await {
        Ok(c) => c,
        Err(_) => {
            println!("daemon is not running");
            return Ok(());
        }
    };

    client
        .shutdown()
        .await
        .context("failed to send shutdown to daemon")?;
    println!("daemon stopped");
    Ok(())
}

/// `pice daemon status` — print daemon version, uptime, and socket path.
///
/// Queries `daemon/health` and formats the response. If the daemon is not
/// running, prints "not running".
async fn cmd_status(socket_path: &SocketPath, token_path: &Path) -> Result<()> {
    let mut client = match DaemonClient::connect(socket_path, token_path).await {
        Ok(c) => c,
        Err(_) => {
            println!("daemon is not running");
            return Ok(());
        }
    };

    match client.health_query().await {
        Ok(info) => {
            let version = info["version"].as_str().unwrap_or("unknown");
            let uptime = info["uptime_seconds"].as_u64().unwrap_or(0);
            println!("daemon is running (v{version})");
            println!("  uptime: {uptime}s");
            println!("  socket: {}", socket_path.display());
        }
        Err(e) => {
            println!("daemon is not healthy: {e}");
        }
    }
    Ok(())
}

/// `pice daemon logs` — read or tail `~/.pice/logs/daemon.log`.
///
/// Phase 0: the daemon still logs to stderr (T11 stub). This command
/// reports that the log file doesn't exist yet. When T21 switches to
/// `tracing_appender::rolling::daily`, this will work against the file.
fn cmd_logs(follow: bool) -> Result<()> {
    let log_path = default_log_path();

    if !log_path.exists() {
        println!(
            "log file not found: {}\n\nThe daemon logs to stderr until file-based logging is enabled.",
            log_path.display()
        );
        return Ok(());
    }

    let content = std::fs::read_to_string(&log_path)
        .with_context(|| format!("failed to read {}", log_path.display()))?;

    if follow {
        // Phase 0: simple one-shot read. A polling tail loop (or notify-based
        // watcher) will replace this when file-based logging lands.
        println!("{content}");
        println!("(--follow is not yet implemented; showing current contents)");
    } else {
        print!("{content}");
    }

    Ok(())
}

// ─── Helpers ──────────────────────────────────────────────────────────────

/// Try a single connect + health check. Returns `Ok(())` on success.
async fn try_health(socket_path: &SocketPath, token_path: &Path) -> Result<()> {
    let mut client = DaemonClient::connect(socket_path, token_path).await?;
    client.health_check().await
}

/// Resolve the default daemon log path: `~/.pice/logs/daemon.log`.
///
/// Derives the `~/.pice/` base from the token path's parent to stay
/// consistent with other default path resolution.
fn default_log_path() -> std::path::PathBuf {
    let base = auth::default_token_path();
    // token_path = ~/.pice/daemon.token → parent = ~/.pice/
    base.parent()
        .unwrap_or_else(|| Path::new("."))
        .join("logs")
        .join("daemon.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_log_path_ends_with_expected_suffix() {
        let p = default_log_path();
        let expected_suffix = Path::new(".pice").join("logs").join("daemon.log");
        assert!(
            p.ends_with(&expected_suffix),
            "expected path ending with {}, got {}",
            expected_suffix.display(),
            p.display()
        );
    }

    /// `status` against a non-existent socket should report "not running"
    /// without returning an error.
    #[cfg(unix)]
    #[tokio::test]
    async fn status_reports_not_running() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket_path = SocketPath::Unix(dir.path().join("no-such.sock"));
        let token_path = dir.path().join("daemon.token");
        std::fs::write(&token_path, "fake-token").expect("write token");

        // Should succeed (prints "not running") rather than error.
        let result = cmd_status(&socket_path, &token_path).await;
        assert!(result.is_ok());
    }

    /// `stop` against a non-existent socket should report "not running"
    /// without returning an error.
    #[cfg(unix)]
    #[tokio::test]
    async fn stop_reports_not_running() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket_path = SocketPath::Unix(dir.path().join("no-such.sock"));
        let token_path = dir.path().join("daemon.token");
        std::fs::write(&token_path, "fake-token").expect("write token");

        let result = cmd_stop(&socket_path, &token_path).await;
        assert!(result.is_ok());
    }

    /// Full cycle: spin up a daemon → status → stop.
    ///
    /// Uses `lifecycle::run_with_paths` directly (not `spawn_daemon`) so the
    /// test doesn't need `pice-daemon` on PATH.
    #[cfg(unix)]
    #[tokio::test]
    async fn status_and_stop_against_running_daemon() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sock_path = dir.path().join("daemon.sock");
        let token_path = dir.path().join("daemon.token");
        let socket_path = SocketPath::Unix(sock_path.clone());

        // Spawn daemon in background.
        let sp = socket_path.clone();
        let tp = token_path.clone();
        let handle = tokio::spawn(pice_daemon::lifecycle::run_with_paths(sp, tp));

        // Wait for socket.
        for _ in 0..100 {
            if sock_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(sock_path.exists(), "socket should appear");

        // Status should succeed and report version info via health_query.
        cmd_status(&socket_path, &token_path)
            .await
            .expect("status should succeed");

        // Stop should send shutdown.
        cmd_stop(&socket_path, &token_path)
            .await
            .expect("stop should succeed");

        // Daemon should exit cleanly.
        let daemon_result = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("daemon should exit within 5s")
            .expect("join handle");
        daemon_result.expect("daemon should exit cleanly");
    }

    /// `logs` when the log file doesn't exist should print a helpful message
    /// rather than error.
    #[test]
    fn logs_missing_file_is_not_an_error() {
        // cmd_logs uses default_log_path() which may not exist — that's fine.
        // We just verify it doesn't panic or return Err.
        let result = cmd_logs(false);
        assert!(result.is_ok());
    }
}

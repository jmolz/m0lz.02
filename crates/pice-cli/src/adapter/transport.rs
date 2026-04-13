//! Socket client for daemon RPC — connect, authenticate, dispatch.
//!
//! [`DaemonClient`] wraps a platform-specific socket connection (Unix domain
//! socket on macOS/Linux, named pipe on Windows) with the daemon's bearer
//! token. It provides [`DaemonClient::health_check`] and
//! [`DaemonClient::dispatch`] methods that handle the JSON-RPC 2.0
//! request/response framing.
//!
//! The CLI never constructs a `DaemonClient` directly in production — it goes
//! through [`super::autostart::ensure_daemon_running`], which handles
//! connection and auto-start. Tests use [`DaemonClient::connect`] directly
//! with isolated paths.

use std::path::Path;

use anyhow::{bail, Context, Result};
use pice_core::cli::{CommandRequest, CommandResponse};
use pice_core::protocol::{methods, DaemonRequest, DaemonResponse};
use pice_core::transport::SocketPath;
use pice_daemon::server::auth;
use serde::{de::DeserializeOwned, Serialize};

/// Type alias for the Windows client-side framed connection.
///
/// On Windows, the server-side `WindowsPipeConnection` wraps a
/// `NamedPipeServer`; the client side wraps a `NamedPipeClient` instead.
/// Both use `JsonLineFramed` for the same wire format.
#[cfg(windows)]
type WindowsClientFramed = pice_daemon::server::framing::JsonLineFramed<
    tokio::io::ReadHalf<tokio::net::windows::named_pipe::NamedPipeClient>,
    tokio::io::WriteHalf<tokio::net::windows::named_pipe::NamedPipeClient>,
>;

/// A framed, authenticated connection to the daemon.
///
/// Wraps the platform-specific connection and the bearer token so callers
/// can focus on request/response semantics without managing the transport
/// or auth layer.
pub struct DaemonClient {
    #[cfg(unix)]
    conn: pice_daemon::server::unix::UnixConnection,
    #[cfg(windows)]
    framed: WindowsClientFramed,
    token: String,
}

impl DaemonClient {
    /// Connect to the daemon at the given socket path and load the auth token.
    ///
    /// Fails if the socket doesn't exist (daemon not running), the token
    /// file is missing, or the connection is refused.
    pub async fn connect(socket_path: &SocketPath, token_path: &Path) -> Result<Self> {
        let token = auth::read_token_file(token_path)
            .context("failed to read daemon auth token — is the daemon running?")?;

        #[cfg(unix)]
        {
            let path = match socket_path {
                SocketPath::Unix(p) => p,
                _ => bail!("expected Unix socket path on this platform"),
            };
            let stream = tokio::net::UnixStream::connect(path)
                .await
                .with_context(|| format!("failed to connect to daemon at {}", path.display()))?;
            let conn = pice_daemon::server::unix::UnixConnection::new(stream);
            Ok(Self { conn, token })
        }

        #[cfg(windows)]
        {
            let name = match socket_path {
                SocketPath::Windows(n) => n,
                _ => bail!("expected Windows named pipe path on this platform"),
            };
            let client = tokio::net::windows::named_pipe::ClientOptions::new()
                .open(name)
                .with_context(|| format!("failed to connect to daemon at {name}"))?;
            let (rd, wr) = tokio::io::split(client);
            let framed = pice_daemon::server::framing::JsonLineFramed::new(rd, wr);
            Ok(Self { framed, token })
        }
    }

    /// Send a `daemon/health` RPC and verify the daemon is alive.
    ///
    /// Returns `Ok(())` if the daemon responds with a valid health result.
    /// The connection remains open for subsequent requests.
    pub async fn health_check(&mut self) -> Result<()> {
        let req = DaemonRequest::new(
            0,
            methods::DAEMON_HEALTH,
            &self.token,
            serde_json::json!({}),
        );
        self.write_msg(&req).await?;

        let resp: DaemonResponse = self
            .read_msg()
            .await?
            .ok_or_else(|| anyhow::anyhow!("daemon closed connection during health check"))?;

        if let Some(err) = resp.error {
            bail!("daemon health check failed ({}): {}", err.code, err.message);
        }

        Ok(())
    }

    /// Send a `cli/dispatch` RPC with the given `CommandRequest`.
    ///
    /// Serializes the request into the `params` of a `cli/dispatch`
    /// `DaemonRequest`, sends it, reads the response, and deserializes
    /// the `CommandResponse` from the result.
    pub async fn dispatch(&mut self, req: CommandRequest) -> Result<CommandResponse> {
        let params = serde_json::to_value(&req).context("failed to serialize CommandRequest")?;

        let daemon_req = DaemonRequest::new(1, methods::CLI_DISPATCH, &self.token, params);
        self.write_msg(&daemon_req)
            .await
            .context("failed to send cli/dispatch request")?;

        let daemon_resp: DaemonResponse = self
            .read_msg()
            .await
            .context("failed to read daemon response")?
            .ok_or_else(|| anyhow::anyhow!("daemon closed connection before responding"))?;

        if let Some(err) = daemon_resp.error {
            bail!("daemon error ({}): {}", err.code, err.message);
        }

        let result = daemon_resp
            .result
            .ok_or_else(|| anyhow::anyhow!("daemon returned success with no result"))?;

        serde_json::from_value(result)
            .context("failed to deserialize CommandResponse from daemon result")
    }

    /// Send a `daemon/shutdown` RPC to request orderly daemon shutdown.
    ///
    /// Used by `pice daemon stop` (T24) and test cleanup.
    #[allow(dead_code)] // T24 wires this into `pice daemon stop`.
    pub async fn shutdown(&mut self) -> Result<()> {
        let req = DaemonRequest::new(
            99,
            methods::DAEMON_SHUTDOWN,
            &self.token,
            serde_json::json!({}),
        );
        self.write_msg(&req).await?;

        let resp: DaemonResponse = self
            .read_msg()
            .await?
            .ok_or_else(|| anyhow::anyhow!("daemon closed connection during shutdown"))?;

        if let Some(err) = resp.error {
            bail!("daemon shutdown failed ({}): {}", err.code, err.message);
        }

        Ok(())
    }

    /// Platform-gated write.
    async fn write_msg<T: Serialize>(&mut self, msg: &T) -> Result<()> {
        #[cfg(unix)]
        {
            self.conn.write_message(msg).await
        }
        #[cfg(windows)]
        {
            self.framed.write_message(msg).await
        }
    }

    /// Platform-gated read.
    async fn read_msg<T: DeserializeOwned>(&mut self) -> Result<Option<T>> {
        #[cfg(unix)]
        {
            self.conn.read_message().await
        }
        #[cfg(windows)]
        {
            self.framed.read_message().await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pice_core::cli::StatusRequest;
    use std::time::Duration;

    /// Integration test: start a daemon in a background task with isolated
    /// paths, then use `DaemonClient` to connect, health-check, and dispatch.
    ///
    /// Proves the full adapter → socket → daemon → handler → response path.
    #[cfg(unix)]
    #[tokio::test]
    async fn client_dispatch_roundtrip_via_daemon() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sock_path = dir.path().join("daemon.sock");
        let token_path = dir.path().join("daemon.token");
        let socket_path = SocketPath::Unix(sock_path.clone());

        // Spawn the daemon in a background task.
        let sp = socket_path.clone();
        let tp = token_path.clone();
        let handle = tokio::spawn(pice_daemon::lifecycle::run_with_paths(sp, tp));

        // Wait for the socket to appear.
        for _ in 0..100 {
            if sock_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(sock_path.exists(), "socket should exist after startup");

        // Connect and health-check.
        let mut client = DaemonClient::connect(&socket_path, &token_path)
            .await
            .expect("connect");
        client.health_check().await.expect("health check");

        // Dispatch a status command.
        let req = CommandRequest::Status(StatusRequest { json: false });
        let resp = client.dispatch(req).await.expect("dispatch");
        match resp {
            CommandResponse::Text { content } => {
                assert!(
                    content.contains("stub"),
                    "status should return stub, got: {content}"
                );
            }
            other => panic!("expected Text response, got: {other:?}"),
        }

        // Shutdown the daemon cleanly.
        client.shutdown().await.expect("shutdown");
        drop(client);

        // Wait for daemon to exit.
        let daemon_result = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("daemon should exit within 5s")
            .expect("join handle");
        daemon_result.expect("daemon should exit cleanly");
    }

    /// Verify that connecting to a non-existent socket produces a clear error.
    #[cfg(unix)]
    #[tokio::test]
    async fn connect_to_missing_socket_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sock_path = dir.path().join("no-such.sock");
        let token_path = dir.path().join("daemon.token");

        // Write a fake token so the token-read step doesn't fail first.
        std::fs::write(&token_path, "fake-token-for-test").expect("write token");

        let socket_path = SocketPath::Unix(sock_path);
        let result = DaemonClient::connect(&socket_path, &token_path).await;
        assert!(result.is_err(), "should fail with missing socket");
        let msg = format!("{:#}", result.err().unwrap());
        assert!(
            msg.contains("failed to connect"),
            "error should mention connection failure, got: {msg}"
        );
    }

    /// Verify that a missing token file produces a clear error.
    #[cfg(unix)]
    #[tokio::test]
    async fn connect_with_missing_token_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sock_path = dir.path().join("daemon.sock");
        let token_path = dir.path().join("no-such.token");

        let socket_path = SocketPath::Unix(sock_path);
        let result = DaemonClient::connect(&socket_path, &token_path).await;
        assert!(result.is_err(), "should fail with missing token");
        let msg = format!("{:#}", result.err().unwrap());
        assert!(
            msg.contains("auth token"),
            "error should mention auth token, got: {msg}"
        );
    }
}

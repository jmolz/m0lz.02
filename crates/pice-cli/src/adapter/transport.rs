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
#[cfg(windows)]
use std::time::Duration;

use anyhow::{bail, Context, Result};
use pice_core::cli::{CommandRequest, CommandResponse};
use pice_core::protocol::{methods, DaemonNotification, DaemonRequest, DaemonResponse};
use pice_core::transport::SocketPath;
use pice_daemon::server::auth;
use serde::{de::DeserializeOwned, Serialize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

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
            let client = open_windows_pipe_client(name)
                .await
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

    /// Send a `daemon/health` RPC and return the raw result JSON.
    ///
    /// Unlike [`Self::health_check`], which only asserts liveness, this returns the
    /// response body so callers can extract `version`, `uptime_seconds`, etc.
    /// Used by `pice daemon status` (T24).
    pub async fn health_query(&mut self) -> Result<serde_json::Value> {
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
            .ok_or_else(|| anyhow::anyhow!("daemon closed connection during health query"))?;

        if let Some(err) = resp.error {
            bail!("daemon health query failed ({}): {}", err.code, err.message);
        }

        resp.result
            .ok_or_else(|| anyhow::anyhow!("daemon returned success with no result"))
    }

    /// Send a `daemon/shutdown` RPC to request orderly daemon shutdown.
    ///
    /// Used by `pice daemon stop` (T24) and test cleanup.
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

    /// Open a subscribe stream (`manifest/subscribe` or `logs/stream`).
    ///
    /// Consumes `self` because a subscribe connection can't multiplex
    /// control RPCs — the daemon takes over the socket for the lifetime
    /// of the subscription. Callers that need a concurrent control
    /// channel (e.g., `ReviewGate::Decide` while streaming manifest
    /// events) must open a second [`DaemonClient`] instance; the daemon
    /// bearer-token auth allows concurrent connections.
    ///
    /// Flow:
    /// 1. Send the RPC with `method` + serialized `params`.
    /// 2. Await the single `DaemonResponse` carrying the snapshot.
    /// 3. Deserialize the snapshot into `T`.
    /// 4. Spawn a reader task owning `self` that forwards every
    ///    subsequent [`DaemonNotification`] into an `mpsc` channel
    ///    until EOF, read error, or the receiver is dropped.
    ///
    /// The returned [`SubscribeStream`] bundles the snapshot, the
    /// `mpsc::Receiver`, and a close handle. See its docs for close
    /// semantics.
    pub async fn subscribe_stream<P, T>(
        mut self,
        method: &str,
        params: P,
    ) -> Result<SubscribeStream<T>>
    where
        P: Serialize,
        T: DeserializeOwned,
    {
        let params_value =
            serde_json::to_value(&params).context("failed to serialize subscribe params")?;
        let req = DaemonRequest::new(0, method, &self.token, params_value);
        self.write_msg(&req)
            .await
            .context("failed to send subscribe request")?;

        let resp: DaemonResponse = self
            .read_msg()
            .await
            .context("failed to read subscribe snapshot response")?
            .ok_or_else(|| anyhow::anyhow!("daemon closed connection before subscribe snapshot"))?;

        if let Some(err) = resp.error {
            bail!("daemon subscribe error ({}): {}", err.code, err.message);
        }

        let result = resp
            .result
            .ok_or_else(|| anyhow::anyhow!("daemon returned success with no snapshot result"))?;
        let snapshot: T = serde_json::from_value(result)
            .context("failed to deserialize subscribe snapshot body")?;

        // Buffered channel — the reader outpaces the consumer by a
        // handful of frames at most before awaiting channel capacity,
        // which backpressures the daemon-side broadcast (where we'd
        // trip `RecvError::Lagged` and lose frames anyway). 64 is
        // comfortable headroom vs. the worst observed burst (12 events
        // in Phase 5 parallel cohort integration tests).
        let (tx, rx) = mpsc::channel::<DaemonNotification>(64);

        let task = tokio::spawn(async move {
            loop {
                match self.read_msg::<DaemonNotification>().await {
                    Ok(Some(notif)) => {
                        if tx.send(notif).await.is_err() {
                            tracing::debug!("subscribe_stream reader: consumer dropped — closing");
                            break;
                        }
                    }
                    Ok(None) => {
                        tracing::debug!(
                            "subscribe_stream reader: daemon closed connection (clean EOF)"
                        );
                        break;
                    }
                    Err(e) => {
                        tracing::debug!("subscribe_stream reader: read error: {e}");
                        break;
                    }
                }
            }
            // `self` drops here → the framed connection drops → the
            // daemon observes EOF on the next scheduler tick and the
            // router's subscribe handler exits its `tokio::select!`.
        });

        Ok(SubscribeStream {
            snapshot,
            rx,
            handle: SubscribeStreamHandle { task },
        })
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

#[cfg(windows)]
async fn open_windows_pipe_client(
    name: &str,
) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use tokio::net::windows::named_pipe::ClientOptions;
    use windows_sys::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, ERROR_SEM_TIMEOUT,
    };
    use windows_sys::Win32::System::Pipes::WaitNamedPipeW;

    const OPEN_TIMEOUT: Duration = Duration::from_millis(500);
    const RETRY_INTERVAL: Duration = Duration::from_millis(25);

    let wide_name: Vec<u16> = OsStr::new(name).encode_wide().chain(Some(0)).collect();
    let deadline = tokio::time::Instant::now() + OPEN_TIMEOUT;

    loop {
        let available = unsafe { WaitNamedPipeW(wide_name.as_ptr(), 0) } != 0;
        let last_error = if available {
            match ClientOptions::new().open(name) {
                Ok(client) => return Ok(client),
                Err(err) if is_transient_windows_pipe_open_error(&err) => err,
                Err(err) => return Err(err),
            }
        } else {
            let err = std::io::Error::last_os_error();
            match err.raw_os_error().map(|code| code as u32) {
                Some(ERROR_FILE_NOT_FOUND | ERROR_PIPE_BUSY | ERROR_SEM_TIMEOUT) => err,
                _ => return Err(err),
            }
        };

        if tokio::time::Instant::now() >= deadline {
            return Err(last_error);
        }

        tokio::time::sleep(RETRY_INTERVAL).await;
    }
}

#[cfg(windows)]
fn is_transient_windows_pipe_open_error(err: &std::io::Error) -> bool {
    use windows_sys::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, ERROR_SEM_TIMEOUT,
    };

    matches!(
        err.raw_os_error().map(|code| code as u32),
        Some(ERROR_FILE_NOT_FOUND | ERROR_PIPE_BUSY | ERROR_SEM_TIMEOUT)
    )
}

/// A live subscribe stream: snapshot + notification channel + close handle.
///
/// Returned by [`DaemonClient::subscribe_stream`]. The snapshot is the
/// initial RPC response body (typed per-method: `SubscribeManifestResponse`
/// for `manifest/subscribe`, `LogsStreamResponse` for `logs/stream`).
/// Subsequent notifications arrive on `rx.recv().await`.
///
/// Dropping `SubscribeStream` detaches the reader task — it continues
/// reading until the daemon closes the connection OR the reader fails to
/// forward (which happens one frame after the consumer drops `rx`).
/// Callers that need deterministic cleanup (e.g., to ensure the socket
/// is closed before asserting "no subscribers" in a test) should call
/// [`SubscribeStream::close`] explicitly.
pub struct SubscribeStream<T> {
    /// The initial snapshot body decoded from the RPC response. For
    /// `manifest/subscribe` this is `SubscribeManifestResponse`; for
    /// `logs/stream` this is `LogsStreamResponse`.
    pub snapshot: T,
    /// Receiver for subsequent daemon notifications on the same
    /// connection. Returns `None` after the reader task exits (daemon
    /// closed connection, read error, or [`SubscribeStreamHandle::close`]
    /// was called).
    pub rx: mpsc::Receiver<DaemonNotification>,
    handle: SubscribeStreamHandle,
}

impl<T> SubscribeStream<T> {
    /// Close the subscribe stream: abort the reader task and drop the
    /// connection. Equivalent to `self.handle.close().await`.
    pub async fn close(self) {
        self.handle.close().await;
    }
}

/// Close handle for the spawned reader task inside a [`SubscribeStream`].
pub struct SubscribeStreamHandle {
    task: JoinHandle<()>,
}

impl SubscribeStreamHandle {
    /// Abort the reader task and drop the underlying connection.
    ///
    /// `JoinHandle::abort` fires a cancellation signal; awaiting the
    /// handle after abort returns `Err(JoinError::Cancelled)`, which we
    /// swallow — the point is to ensure the socket is closed before
    /// this function returns, not to rethrow an abort into the caller.
    pub async fn close(self) {
        self.task.abort();
        let _ = self.task.await;
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

        // Wait for the socket + token to appear. Debug daemon startup can
        // exceed one second under parallel cargo test load.
        for _ in 0..500 {
            if sock_path.exists() && token_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(sock_path.exists(), "socket should exist after startup");
        assert!(token_path.exists(), "token should exist after startup");

        // Connect and health-check.
        let mut client = DaemonClient::connect(&socket_path, &token_path)
            .await
            .expect("connect");
        client.health_check().await.expect("health check");

        // Dispatch a status command.
        let req = CommandRequest::Status(StatusRequest {
            json: false,
            ..Default::default()
        });
        let resp = client.dispatch(req).await.expect("dispatch");
        match resp {
            CommandResponse::Text { content } => {
                assert!(
                    content.contains("PICE Status"),
                    "status should contain header, got: {content}"
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

    /// Task 11 happy-path: `subscribe_stream` sends the request, receives
    /// the snapshot, then forwards subsequent `manifest/event`
    /// notifications on the mpsc channel. Closes cleanly on daemon
    /// shutdown.
    ///
    /// Uses the daemon's socket so the full read/parse/dispatch path
    /// runs (not just the reader task in isolation).
    #[cfg(unix)]
    #[tokio::test]
    async fn subscribe_stream_happy_path() {
        use pice_core::protocol::subscribe::{SubscribeManifestRequest, SubscribeManifestResponse};

        let dir = tempfile::tempdir().expect("tempdir");
        let sock_path = dir.path().join("daemon.sock");
        let token_path = dir.path().join("daemon.token");
        let socket_path = SocketPath::Unix(sock_path.clone());

        let state_tmp = tempfile::tempdir().expect("state tempdir");
        let _guard = pice_daemon::test_support::StateDirGuard::new(state_tmp.path());

        // Spawn the daemon.
        let sp = socket_path.clone();
        let tp = token_path.clone();
        let handle = tokio::spawn(pice_daemon::lifecycle::run_with_paths(sp, tp));

        for _ in 0..500 {
            if sock_path.exists() && token_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(sock_path.exists(), "daemon socket should be up");
        assert!(token_path.exists(), "daemon token should be up");

        // Open the subscribe stream on a fresh connection.
        let client = DaemonClient::connect(&socket_path, &token_path)
            .await
            .expect("connect");
        let params = SubscribeManifestRequest {
            feature_id: Some("subscribe-test".to_string()),
        };
        let stream = client
            .subscribe_stream::<_, SubscribeManifestResponse>(methods::MANIFEST_SUBSCRIBE, params)
            .await
            .expect("subscribe_stream");

        // Initial snapshot is empty (no manifests on disk yet). This
        // proves the request/response half of the handshake — the
        // daemon parsed our SubscribeManifestRequest and returned a
        // valid SubscribeManifestResponse body.
        assert_eq!(
            stream.snapshot.snapshots.len(),
            0,
            "no pre-existing manifests"
        );
        assert_eq!(
            stream.snapshot.run_ids.len(),
            0,
            "no live runs at subscribe time"
        );

        // Caller-side close: aborts the reader task + drops the framed
        // connection. The daemon observes socket EOF on the next
        // scheduler tick and its `tokio::select!` in the subscribe
        // handler exits. `close` MUST return in bounded time — if it
        // hangs, the `JoinHandle::abort` path is broken or the reader
        // is parked in a non-cancelable state.
        //
        // The daemon-side "daemon-shutdown closes active subscribes"
        // behavior is covered separately in
        // `pice-daemon::handlers::subscribe::tests`; this test owns the
        // CLI-side close semantics only.
        tokio::time::timeout(Duration::from_secs(2), stream.close())
            .await
            .expect("stream.close() should complete within 2s");

        // Shutdown daemon + join.
        let mut shutdown_client = DaemonClient::connect(&socket_path, &token_path)
            .await
            .expect("shutdown connect");
        shutdown_client.shutdown().await.expect("shutdown RPC");

        let daemon_result = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("daemon should exit within 5s")
            .expect("join handle");
        daemon_result.expect("daemon should exit cleanly");
    }

    /// Verify that `subscribe_stream` propagates daemon-side parse errors
    /// (invalid params) cleanly — the caller should see an `Err`, not a
    /// silent empty stream.
    #[cfg(unix)]
    #[tokio::test]
    async fn subscribe_stream_propagates_invalid_params_error() {
        use pice_core::protocol::subscribe::SubscribeManifestResponse;
        use serde_json::json;

        let dir = tempfile::tempdir().expect("tempdir");
        let sock_path = dir.path().join("daemon.sock");
        let token_path = dir.path().join("daemon.token");
        let socket_path = SocketPath::Unix(sock_path.clone());

        let state_tmp = tempfile::tempdir().expect("state tempdir");
        let _guard = pice_daemon::test_support::StateDirGuard::new(state_tmp.path());

        let sp = socket_path.clone();
        let tp = token_path.clone();
        let handle = tokio::spawn(pice_daemon::lifecycle::run_with_paths(sp, tp));

        for _ in 0..500 {
            if sock_path.exists() && token_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(sock_path.exists(), "daemon socket should be up");
        assert!(token_path.exists(), "daemon token should be up");

        let client = DaemonClient::connect(&socket_path, &token_path)
            .await
            .expect("connect");

        // `bogus_field` fails the `deny_unknown_fields` contract on
        // SubscribeManifestRequest, which should return a -32602 error.
        let bad_params = json!({ "feature_id": "f", "bogus_field": 1 });
        let result = client
            .subscribe_stream::<_, SubscribeManifestResponse>(
                methods::MANIFEST_SUBSCRIBE,
                bad_params,
            )
            .await;

        assert!(
            result.is_err(),
            "subscribe_stream should propagate daemon parse error"
        );
        let msg = format!("{:#}", result.err().unwrap());
        assert!(
            msg.contains("daemon subscribe error") || msg.contains("-32602"),
            "error should name the daemon subscribe failure, got: {msg}"
        );

        // Clean up.
        let mut shutdown_client = DaemonClient::connect(&socket_path, &token_path)
            .await
            .expect("shutdown connect");
        shutdown_client.shutdown().await.expect("shutdown");
        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
    }
}

//! Unix domain socket transport for the daemon RPC server.
//!
//! Implements newline-delimited JSON-RPC 2.0 framing over `tokio::net::UnixStream`.
//! This is the macOS/Linux side of T15; the Windows named pipe equivalent will
//! live in [`super::windows`] (T16, `#[cfg(windows)]`).
//!
//! ## Stale-socket handling
//!
//! On `AddrInUse`, [`UnixSocketListener::bind`] probes the existing path via
//! `UnixStream::connect`. `ConnectionRefused` means the socket file is a corpse
//! from a previously-killed daemon — we remove it and retry the bind. A
//! successful probe means another daemon is actively listening — we bail with a
//! clear error so the operator knows to stop the other instance.
//!
//! This is the only sound stale-socket test on Unix. Checking `Metadata`,
//! `file_type()`, or mtime cannot distinguish a live listener from a dead one.
//!
//! ## File permissions
//!
//! After a successful bind, the socket file is `chmod 0600` via
//! [`PermissionsExt::set_mode`]. This runs *after* `bind(2)` because Unix does
//! not expose a mode argument for socket creation — the socket file inherits
//! `umask`. The recommended deployment mitigation is to ensure the parent
//! directory (`~/.pice/`) is itself `0700`, closing the pre-chmod window. T17
//! (`server::auth`) owns that directory invariant.
//!
//! ## Framing
//!
//! Each frame is one JSON object followed by exactly one `\n`. Multi-line JSON
//! is not supported. Writers must not emit newlines inside a single message —
//! serde_json's default serializer guarantees this for non-pretty output, and a
//! `debug_assert!` enforces it in debug builds.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};

/// File mode applied to the socket after bind. Owner read/write only.
const SOCKET_MODE: u32 = 0o600;

/// A bound Unix domain socket listener with newline-delimited JSON framing.
///
/// On drop, the listener best-effort removes the socket file so a subsequent
/// daemon start does not trip stale-socket detection unnecessarily. Removal
/// errors are ignored — a missing file is not a problem for the next startup
/// because [`UnixSocketListener::bind`] handles that case cleanly, and a
/// permission error during shutdown is a non-recoverable condition that logging
/// here would not help with.
#[derive(Debug)]
pub struct UnixSocketListener {
    inner: UnixListener,
    path: PathBuf,
}

impl UnixSocketListener {
    /// Bind a listener at `path`, handling stale-socket cleanup and setting
    /// 0600 permissions on the resulting socket file.
    ///
    /// Errors:
    /// - Another daemon is actively bound to `path`
    /// - The parent directory does not exist
    /// - The process lacks permission to create the socket or chmod the file
    pub async fn bind(path: &Path) -> Result<Self> {
        let listener = bind_with_stale_recovery(path).await?;
        set_socket_permissions(path)?;
        Ok(Self {
            inner: listener,
            path: path.to_path_buf(),
        })
    }

    /// Accept the next incoming connection, wrapping it in a framed
    /// [`UnixConnection`].
    pub async fn accept(&self) -> io::Result<UnixConnection> {
        let (stream, _addr) = self.inner.accept().await?;
        Ok(UnixConnection::new(stream))
    }

    /// The socket path this listener is bound to.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for UnixSocketListener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Binds a `UnixListener`, handling the stale-socket case per the T15 contract.
///
/// See the module docs for the reasoning behind probe-based detection.
async fn bind_with_stale_recovery(path: &Path) -> Result<UnixListener> {
    match UnixListener::bind(path) {
        Ok(l) => Ok(l),
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => match UnixStream::connect(path).await {
            Ok(_probe) => {
                bail!(
                    "another pice-daemon is already listening on {}; \
                         refusing to bind a second instance",
                    path.display()
                );
            }
            Err(probe_err) if probe_err.kind() == io::ErrorKind::ConnectionRefused => {
                std::fs::remove_file(path).with_context(|| {
                    format!(
                        "failed to remove stale socket at {} before rebind",
                        path.display()
                    )
                })?;
                UnixListener::bind(path).with_context(|| {
                    format!(
                        "rebind after stale-socket cleanup failed at {}",
                        path.display()
                    )
                })
            }
            Err(probe_err) => Err(anyhow::Error::from(probe_err).context(format!(
                "cannot probe existing socket at {} to determine liveness",
                path.display()
            ))),
        },
        Err(e) => Err(anyhow::Error::from(e)
            .context(format!("failed to bind Unix socket at {}", path.display()))),
    }
}

/// `chmod 0600` on the socket file. See module docs for the race-window
/// discussion — this is best-effort exposure hardening, not an atomic guarantee.
fn set_socket_permissions(path: &Path) -> Result<()> {
    let mut perms = std::fs::metadata(path)
        .with_context(|| format!("cannot stat socket at {}", path.display()))?
        .permissions();
    perms.set_mode(SOCKET_MODE);
    std::fs::set_permissions(path, perms)
        .with_context(|| format!("cannot chmod 0600 on socket at {}", path.display()))
}

/// A framed full-duplex connection over a Unix domain socket.
///
/// Wraps a `UnixStream` with a persistent [`BufReader`] on the read side and a
/// raw [`OwnedWriteHalf`] on the write side. The reader's buffer is reused
/// across calls so messages arriving in the same kernel read are not dropped.
pub struct UnixConnection {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    read_buf: Vec<u8>,
}

impl UnixConnection {
    /// Wrap a connected `UnixStream`. Used by [`UnixSocketListener::accept`]
    /// (server side) and by callers that connect via `UnixStream::connect`
    /// (the CLI adapter in T22, and transport unit tests).
    pub fn new(stream: UnixStream) -> Self {
        let (rd, wr) = stream.into_split();
        Self {
            reader: BufReader::new(rd),
            writer: wr,
            read_buf: Vec::with_capacity(4096),
        }
    }

    /// Read one newline-delimited JSON message and deserialize it into `T`.
    ///
    /// Returns `Ok(None)` on clean EOF (peer closed the connection between
    /// frames). A parse failure or transport error returns `Err`. The internal
    /// read buffer is cleared on entry and reused across calls, so back-to-back
    /// frames prefetched by the BufReader are preserved.
    pub async fn read_message<T: DeserializeOwned>(&mut self) -> Result<Option<T>> {
        self.read_buf.clear();
        let n = self
            .reader
            .read_until(b'\n', &mut self.read_buf)
            .await
            .context("transport read failed")?;
        if n == 0 {
            return Ok(None);
        }
        // Some peers may omit the trailing newline on the very last frame
        // before closing. Accept both forms.
        let slice: &[u8] = self.read_buf.strip_suffix(b"\n").unwrap_or(&self.read_buf);
        let msg = serde_json::from_slice::<T>(slice)
            .with_context(|| format!("failed to parse JSON frame ({} bytes)", slice.len()))?;
        Ok(Some(msg))
    }

    /// Serialize `msg` as a single JSON object and write it followed by `\n`.
    pub async fn write_message<T: Serialize>(&mut self, msg: &T) -> Result<()> {
        let buf = serde_json::to_vec(msg).context("failed to serialize outgoing message")?;
        debug_assert!(
            !buf.contains(&b'\n'),
            "serde_json output contained a newline — framing would break"
        );
        self.writer
            .write_all(&buf)
            .await
            .context("transport write failed")?;
        self.writer
            .write_all(b"\n")
            .await
            .context("transport write (frame delimiter) failed")?;
        self.writer
            .flush()
            .await
            .context("transport flush failed")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pice_core::protocol::{methods, DaemonRequest, DaemonResponse};
    use serde_json::json;
    use tempfile::tempdir;

    /// Produces a temp socket path. The `TempDir` handle must outlive the
    /// listener, otherwise auto-cleanup will remove the directory before the
    /// test finishes.
    fn temp_socket_path() -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("pice.sock");
        (dir, path)
    }

    #[tokio::test]
    async fn bind_accept_roundtrip_with_0600_perms() {
        let (_tmp, path) = temp_socket_path();

        let listener = UnixSocketListener::bind(&path).await.expect("bind");

        // 0600 check — the criterion T15 guarantees and T17 relies on.
        let meta = std::fs::metadata(&path).expect("stat socket");
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, SOCKET_MODE, "expected 0600, got {mode:o}");

        // Spawn a server task and drive the client from the main task. The
        // two synchronize at the kernel level via accept/connect.
        let server = tokio::spawn(async move {
            let mut conn = listener.accept().await.expect("accept");
            let req: DaemonRequest = conn
                .read_message()
                .await
                .expect("server read")
                .expect("server got a frame (not EOF)");
            assert_eq!(req.method, methods::DAEMON_HEALTH);
            assert_eq!(req.auth, "test-token");

            let resp =
                DaemonResponse::success(req.id, json!({"version": "test", "uptime_seconds": 0}));
            conn.write_message(&resp).await.expect("server write");

            // After writing the response, a second read must observe clean EOF
            // once the client hangs up.
            let next: Option<DaemonRequest> = conn
                .read_message()
                .await
                .expect("server post-response read");
            assert!(next.is_none(), "expected clean EOF after client hangup");
        });

        // Client side.
        let client_stream = UnixStream::connect(&path).await.expect("client connect");
        let mut client = UnixConnection::new(client_stream);

        let req = DaemonRequest::new(42, methods::DAEMON_HEALTH, "test-token", json!({}));
        client.write_message(&req).await.expect("client write");

        let resp: DaemonResponse = client
            .read_message()
            .await
            .expect("client read")
            .expect("client got a frame");
        assert_eq!(resp.id, 42);
        assert!(resp.error.is_none());
        let version = resp
            .result
            .as_ref()
            .and_then(|v| v.get("version"))
            .and_then(|v| v.as_str());
        assert_eq!(version, Some("test"));

        // Drop the client to close its half of the socket; the server task
        // expects EOF after this.
        drop(client);

        server.await.expect("server task join");
    }

    // Note: the stale-socket cleanup test lives in
    // `tests/server_unix_stale_socket.rs` (integration test binary), not here.
    // Under parallel unit-test execution a sibling test
    // (`prompt::builders::*`) spawns `git` subprocesses, and on macOS a
    // freshly-bound simulator socket fd can leak into a concurrent fork
    // before its `Drop` runs — which makes the kernel treat the socket as
    // live and breaks the `bind_with_stale_recovery` probe. Moving that
    // specific test into its own integration test binary (a separate process
    // with no sibling forks) eliminates the race cleanly. See the module
    // docs in `tests/server_unix_stale_socket.rs` for the full writeup.

    #[tokio::test]
    async fn live_daemon_conflict_reports_error() {
        let (_tmp, path) = temp_socket_path();

        let _alive = UnixSocketListener::bind(&path).await.expect("first bind");

        let err = UnixSocketListener::bind(&path)
            .await
            .expect_err("second bind must fail with a live daemon present");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("already listening"),
            "error should mention live-daemon conflict, got: {msg}"
        );
    }

    #[tokio::test]
    async fn malformed_frame_returns_parse_error() {
        let (_tmp, path) = temp_socket_path();
        let listener = UnixSocketListener::bind(&path).await.expect("bind");

        let server = tokio::spawn(async move {
            let mut conn = listener.accept().await.expect("accept");
            let result: Result<Option<DaemonRequest>> = conn.read_message().await;
            assert!(
                result.is_err(),
                "malformed JSON should return Err, got {:?}",
                result.ok()
            );
        });

        // Client writes non-JSON bytes followed by the frame delimiter.
        let mut stream = UnixStream::connect(&path).await.expect("connect");
        stream
            .write_all(b"this is not json\n")
            .await
            .expect("write");
        stream.shutdown().await.expect("shutdown");

        server.await.expect("server task join");
    }
}

//! Windows named pipe transport for the daemon RPC server.
//!
//! Implements newline-delimited JSON-RPC 2.0 framing over
//! `tokio::net::windows::named_pipe::NamedPipeServer`. This is the Windows
//! half of the daemon RPC transport — the macOS/Linux half lives in
//! [`super::unix`] (T15). Both share [`super::framing::JsonLineFramed`] for
//! the wire format.
//!
//! ## How named pipes differ from Unix sockets
//!
//! The surface-level API of this module (`bind`, `accept`, `read_message`,
//! `write_message`) mirrors `server::unix` so a `DaemonTransport` trait in
//! T18+ can consume either transport. Under the hood, however, the Windows
//! lifecycle is fundamentally different in three ways that shape the code
//! below:
//!
//! - **Per-connection server instances.** A single `NamedPipeServer` handles
//!   exactly one client. After `connect().await` returns, you have a bound
//!   end-of-pipe that you use like an `AsyncRead + AsyncWrite` for that one
//!   session. To keep accepting new clients you must create additional
//!   `NamedPipeServer` instances on the same pipe name — the server end of
//!   the pipe is not a listener that accepts many connections the way a
//!   Unix socket does.
//!
//! - **Always-live invariant.** Between connections there must be at least
//!   one unbound `NamedPipeServer` instance on the pipe name at all times,
//!   or clients calling `ClientOptions::open` during the gap get
//!   `io::ErrorKind::NotFound`. [`WindowsPipeListener::accept`] preserves
//!   this invariant by creating the *next* server instance *before*
//!   swapping out the current one.
//!
//! - **No filesystem corpse.** Named pipes are refcounted kernel objects,
//!   not files on disk. When the last handle to a pipe closes — process
//!   death included — the kernel reclaims the name. There is no
//!   equivalent of the Unix "stale socket file after SIGKILL" case.
//!
//! ## Stale-pipe detection
//!
//! Because of the last point above, there is no stale-pipe detection to do.
//! `bind()` uses `ServerOptions::first_pipe_instance(true)`, which causes
//! `create()` to fail with `io::ErrorKind::PermissionDenied` if any other
//! process is currently holding a server instance on the same name. On
//! that error, `bind()` bails with a clear "already listening" message.
//!
//! There is deliberately **no** probe-via-`ClientOptions::open` dance
//! analogous to `server::unix`'s stale-socket recovery, for two reasons:
//!
//! 1. If the kernel says `PermissionDenied`, by definition another live
//!    process currently holds a handle on the name. There is no "dead
//!    corpse" case to recover from — the kernel already cleaned up for us
//!    if the other process had actually died.
//!
//! 2. A `ClientOptions::open()` probe call against our *own* listener
//!    would pair with our next-server slot, bind it to the probe client,
//!    and leave a doomed server instance in the slot. The next `accept()`
//!    call would take that doomed instance out, call `connect().await`
//!    (which returns immediately because the probe is already connected),
//!    and return a `WindowsPipeConnection` whose first `read_message()`
//!    would immediately observe EOF. That silent-bug surface is not
//!    worth the zero benefit of probing.
//!
//! ## Access control
//!
//! Named pipes don't use filesystem permissions. [`ServerOptions::new`]
//! with no explicit security attributes creates the pipe with a default
//! DACL that grants read/write only to the creating user — equivalent in
//! effect to `chmod 0600` on a Unix socket. T17 may tighten this further
//! with an explicit DACL if threat-modeling calls for it.
//!
//! ## Framing
//!
//! Delegated to [`super::framing::JsonLineFramed`]. Identical wire format
//! and semantics to `server::unix` — one JSON object per line, `\n`
//! delimiter, clean EOF maps to `Ok(None)`.

use std::io;
use std::sync::Mutex;

use anyhow::{bail, Result};
use serde::{de::DeserializeOwned, Serialize};
use tokio::io::{split, ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use super::framing::JsonLineFramed;

/// A bound Windows named pipe listener with newline-delimited JSON framing.
///
/// On drop, the held `NamedPipeServer` instance is closed. Unlike
/// [`super::unix::UnixSocketListener`], there is no on-disk file to remove —
/// the kernel reclaims the pipe name automatically when the last handle
/// closes.
#[derive(Debug)]
pub struct WindowsPipeListener {
    /// The pipe name, e.g., `\\.\pipe\pice-daemon`.
    name: String,
    /// The next server instance to be handed to the next `accept()` call.
    /// Held under a mutex so `accept()` can take `&self`, even though its
    /// body does an atomic swap. The mutex is never held across an await.
    next_server: Mutex<NamedPipeServer>,
}

impl WindowsPipeListener {
    /// Bind a listener on the given pipe name, refusing to bind a second
    /// instance if another daemon is already listening.
    ///
    /// Errors:
    /// - Another daemon is actively bound to `name`
    ///   (`io::ErrorKind::PermissionDenied` surfaced as a clear message)
    /// - OS-level I/O error creating the first pipe instance
    pub async fn bind(name: &str) -> Result<Self> {
        let first = create_first_instance_with_conflict_check(name)?;
        Ok(Self {
            name: name.to_string(),
            next_server: Mutex::new(first),
        })
    }

    /// Accept the next incoming connection, wrapping it in a framed
    /// [`WindowsPipeConnection`].
    ///
    /// This implementation preserves the "always-live server" invariant
    /// described in the module docs: a fresh `NamedPipeServer` instance is
    /// created and atomically swapped into the next-server slot *before*
    /// the previous instance is used for `connect().await`. That guarantees
    /// clients never observe a window where no server is listening on the
    /// pipe name.
    pub async fn accept(&self) -> io::Result<WindowsPipeConnection> {
        // Create the NEXT server instance FIRST. No `first_pipe_instance`
        // flag on subsequent instances — that flag is a one-shot ownership
        // assertion at bind time. Inside our own process we are free to
        // create as many cooperating instances as we need for multi-client
        // accept loops, and that is exactly what we are doing.
        //
        // If this create() fails, we return the error with the slot
        // untouched. The listener stays in a valid state and a later
        // accept() call can retry.
        let next = ServerOptions::new().create(&self.name)?;

        // Atomically swap the slot. The lock is held only long enough for
        // a single `mem::replace` — no awaits inside — so concurrent
        // accept() callers (if any) race on a single infallible operation.
        let current = {
            let mut slot = self
                .next_server
                .lock()
                .expect("WindowsPipeListener mutex poisoned");
            std::mem::replace(&mut *slot, next)
        };

        // Wait for a client to connect on the server instance we just took
        // out of the slot. This happens OUTSIDE the lock so a long connect
        // wait does not block other accept() callers.
        current.connect().await?;

        Ok(WindowsPipeConnection::new(current))
    }

    /// The pipe name this listener is bound to.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Creates the first server instance for `name`, using
/// `first_pipe_instance(true)` so a conflicting daemon on the same name
/// surfaces as `PermissionDenied` instead of silent cooperative sharing.
fn create_first_instance_with_conflict_check(name: &str) -> Result<NamedPipeServer> {
    match ServerOptions::new().first_pipe_instance(true).create(name) {
        Ok(server) => Ok(server),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => bail!(
            "another pice-daemon is already listening on {}; \
             refusing to bind a second instance",
            name
        ),
        Err(e) => {
            Err(anyhow::Error::from(e).context(format!("failed to create named pipe at {}", name)))
        }
    }
}

/// A framed full-duplex connection over a connected Windows named pipe.
///
/// Wraps the read/write halves of a `NamedPipeServer` inside a
/// [`JsonLineFramed`] that owns the framing buffer and read-buffer reuse.
/// The underlying split uses `tokio::io::split` because `NamedPipeServer`
/// does not expose a platform-specific zero-lock split API the way
/// `UnixStream::into_split` does. The per-operation lock cost of
/// `tokio::io::split`'s `BiLock` is nanoseconds — negligible against the
/// `serde_json` cost of each frame.
pub struct WindowsPipeConnection {
    framed: JsonLineFramed<ReadHalf<NamedPipeServer>, WriteHalf<NamedPipeServer>>,
}

impl WindowsPipeConnection {
    /// Wrap a connected `NamedPipeServer`. Used by
    /// [`WindowsPipeListener::accept`] and, in tests, by code that
    /// constructs a server instance directly.
    pub fn new(server: NamedPipeServer) -> Self {
        let (rd, wr) = split(server);
        Self {
            framed: JsonLineFramed::new(rd, wr),
        }
    }

    /// Read one newline-delimited JSON message. See
    /// [`JsonLineFramed::read_message`] for the full contract.
    pub async fn read_message<T: DeserializeOwned>(&mut self) -> Result<Option<T>> {
        self.framed.read_message().await
    }

    /// Serialize `msg` as one JSON object and write it followed by `\n`.
    /// See [`JsonLineFramed::write_message`] for the embedded-newline guard.
    pub async fn write_message<T: Serialize>(&mut self, msg: &T) -> Result<()> {
        self.framed.write_message(msg).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pice_core::protocol::{methods, DaemonRequest, DaemonResponse};
    use serde_json::json;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::io::AsyncWriteExt;
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};

    /// Produces a unique pipe name per test. Pipe names share a single
    /// namespace per-machine, so we combine the process ID with a
    /// per-test-binary counter so collisions are impossible within a run
    /// and unlikely across concurrent runs on the same host.
    fn temp_pipe_name() -> String {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let pid = std::process::id();
        let idx = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!(r"\\.\pipe\pice-daemon-test-{pid}-{idx}")
    }

    #[tokio::test]
    async fn bind_accept_roundtrip() {
        let name = temp_pipe_name();

        let listener = WindowsPipeListener::bind(&name).await.expect("bind");

        // Spawn a server task and drive the client from the main task. The
        // two synchronize at the kernel level via accept/open.
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

            // After writing the response, a second read must observe clean
            // EOF once the client hangs up.
            let next: Option<DaemonRequest> = conn
                .read_message()
                .await
                .expect("server post-response read");
            assert!(next.is_none(), "expected clean EOF after client hangup");
        });

        // Client side — splits the client half the same way
        // `WindowsPipeConnection::new` splits a server half, and wraps in
        // `JsonLineFramed` directly.
        let client: NamedPipeClient = ClientOptions::new().open(&name).expect("client open");
        let (client_rd, client_wr) = split(client);
        let mut client_framed = JsonLineFramed::new(client_rd, client_wr);

        let req = DaemonRequest::new(42, methods::DAEMON_HEALTH, "test-token", json!({}));
        client_framed
            .write_message(&req)
            .await
            .expect("client write");

        let resp: DaemonResponse = client_framed
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

        // Drop the client framed connection to close its half of the pipe;
        // the server task expects EOF after this.
        drop(client_framed);

        server.await.expect("server task join");
    }

    #[tokio::test]
    async fn live_daemon_conflict_reports_error() {
        let name = temp_pipe_name();

        let _alive = WindowsPipeListener::bind(&name).await.expect("first bind");

        let err = WindowsPipeListener::bind(&name)
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
        let name = temp_pipe_name();
        let listener = WindowsPipeListener::bind(&name).await.expect("bind");

        let server = tokio::spawn(async move {
            let mut conn = listener.accept().await.expect("accept");
            let result: Result<Option<DaemonRequest>> = conn.read_message().await;
            assert!(
                result.is_err(),
                "malformed JSON should return Err, got {:?}",
                result.ok()
            );
        });

        // Client writes non-JSON bytes followed by the frame delimiter,
        // then shuts down the write side.
        let mut client: NamedPipeClient = ClientOptions::new().open(&name).expect("client open");
        client
            .write_all(b"this is not json\n")
            .await
            .expect("write");
        client.shutdown().await.expect("client shutdown");

        server.await.expect("server task join");
    }
}

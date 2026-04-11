//! Transport-generic newline-delimited JSON-RPC 2.0 framing.
//!
//! Provides [`JsonLineFramed`], a full-duplex framed connection that speaks
//! the daemon RPC wire format over any `tokio::io::AsyncRead` +
//! `tokio::io::AsyncWrite` pair. Both [`super::unix`] (Unix domain socket,
//! T15) and [`super::windows`] (named pipe, T16) wrap this type; the framing
//! logic itself has no knowledge of the underlying transport.
//!
//! ## Why a separate module
//!
//! Every framing concern — newline delimiter, `serde_json` parse, EOF
//! semantics, embedded-newline debug assertion, read-buffer reuse — is
//! identical across both platforms. Keeping it in one place means a future
//! framing bug gets fixed once, not twice, and the two transport impls stay
//! mechanically interchangeable at the call site.
//!
//! ## Framing contract
//!
//! - Each message is exactly one JSON object followed by one `\n` byte.
//! - Writers MUST NOT emit newlines inside a single serialized message.
//!   `serde_json`'s default (non-pretty) serializer already guarantees this;
//!   a `debug_assert!` in [`JsonLineFramed::write_message`] enforces it in
//!   debug builds as a belt-and-braces check against a future change.
//! - The reader accepts a missing trailing newline on the very last frame
//!   before EOF, to tolerate peers that omit the delimiter during clean
//!   shutdown.
//! - A `read_until` that returns zero bytes is clean EOF and maps to
//!   `Ok(None)`. Any transport error or JSON parse failure maps to `Err`.
//!
//! The read buffer is cleared on each `read_message` call and then reused,
//! so back-to-back frames prefetched into the same kernel read stay resident
//! in the buffered reader and are delivered across successive calls without
//! reallocation.

use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// A framed full-duplex daemon RPC connection over any async read/write pair.
///
/// Type-parameterized so the unix socket impl can plug in
/// `tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf}` (lock-free,
/// platform-specific split) and the windows named-pipe impl can plug in
/// whatever split tokio's `NamedPipeServer` exposes, without this module
/// caring.
pub struct JsonLineFramed<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    reader: BufReader<R>,
    writer: W,
    read_buf: Vec<u8>,
}

impl<R, W> JsonLineFramed<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    /// Wrap a read/write pair with newline-delimited JSON framing.
    ///
    /// Typically called by a transport adapter (e.g.,
    /// `UnixConnection::new`, `WindowsPipeConnection::new`) that has already
    /// performed the platform-specific stream split.
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader: BufReader::new(reader),
            writer,
            read_buf: Vec::with_capacity(4096),
        }
    }

    /// Read one newline-delimited JSON message and deserialize it into `T`.
    ///
    /// Returns `Ok(None)` on clean EOF (peer closed the connection between
    /// frames). A parse failure or transport error returns `Err`. The internal
    /// read buffer is cleared on entry and reused across calls, so back-to-back
    /// frames prefetched by the `BufReader` are preserved.
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

//! Transport-generic `DaemonConnection` trait abstracting `UnixConnection`
//! and `WindowsPipeConnection` behind a common async interface.
//!
//! Phase 7 introduces handlers (`manifest/subscribe`, `logs/stream`) that take
//! over the connection for the lifetime of the subscription — they write an
//! initial snapshot response, then `tokio::select!` between inbound messages
//! (client-hangup detection) and an outbound broadcast/logs receiver, writing
//! notifications on the same connection until one side closes.
//!
//! Rather than templating every subscribe handler on two transport types
//! (three if we add a test-only in-memory transport), this trait gives them a
//! single `&mut dyn DaemonConnection` surface. The methods use CONCRETE
//! request/response/notification types (not generic `T`) so the trait remains
//! object-safe under `async-trait`.
//!
//! ## Why concrete types, not generics
//!
//! `async-trait` cannot express generic methods on object-safe traits
//! (`fn read_message<T>(&mut self)` requires known-at-vtable-time return
//! types). The three concrete message types in `pice_core::protocol` are
//! sufficient for every subscribe handler's wire needs, and specializing now
//! avoids a later refactor to unblock transport-generic code.

use anyhow::Result;
use async_trait::async_trait;
use pice_core::protocol::{DaemonNotification, DaemonRequest, DaemonResponse};

/// Transport-generic daemon connection suitable for subscribe handlers that
/// need to read inbound messages AND write responses + streaming notifications
/// on the same full-duplex channel.
///
/// See [`crate::handlers::subscribe`] for the two production consumers.
#[async_trait]
pub trait DaemonConnection: Send {
    /// Read one newline-delimited `DaemonRequest` from the peer. Returns
    /// `Ok(None)` on clean EOF (peer closed between frames). Parse failures
    /// and transport errors return `Err`.
    async fn read_request(&mut self) -> Result<Option<DaemonRequest>>;

    /// Write a final response. Used by both subscribe handlers to deliver
    /// the initial snapshot.
    async fn write_response(&mut self, resp: &DaemonResponse) -> Result<()>;

    /// Write a streaming notification (no response id — fire-and-forget,
    /// per JSON-RPC 2.0 spec). Used to forward `manifest/event` and
    /// `logs/chunk` frames to the subscribed client.
    async fn write_notification(&mut self, notif: &DaemonNotification) -> Result<()>;
}

#[cfg(unix)]
#[async_trait]
impl DaemonConnection for super::unix::UnixConnection {
    async fn read_request(&mut self) -> Result<Option<DaemonRequest>> {
        self.read_message().await
    }
    async fn write_response(&mut self, resp: &DaemonResponse) -> Result<()> {
        self.write_message(resp).await
    }
    async fn write_notification(&mut self, notif: &DaemonNotification) -> Result<()> {
        self.write_message(notif).await
    }
}

#[cfg(windows)]
#[async_trait]
impl DaemonConnection for super::windows::WindowsPipeConnection {
    async fn read_request(&mut self) -> Result<Option<DaemonRequest>> {
        self.read_message().await
    }
    async fn write_response(&mut self, resp: &DaemonResponse) -> Result<()> {
        self.write_message(resp).await
    }
    async fn write_notification(&mut self, notif: &DaemonNotification) -> Result<()> {
        self.write_message(notif).await
    }
}

#[cfg(test)]
pub mod test_support {
    //! In-memory `DaemonConnection` impl used by unit tests of the subscribe
    //! handlers — no socket, no kernel IO, deterministic wake-ups.

    use super::*;
    use tokio::sync::mpsc;

    /// A bidirectional in-memory connection. The `inbound` channel carries
    /// requests the test "sends" to the handler; the `outbound` channel
    /// collects responses + notifications the handler writes back.
    ///
    /// Drop the inbound sender to simulate a client hangup (next
    /// `read_request` returns `Ok(None)`).
    pub struct MemoryConnection {
        inbound: mpsc::UnboundedReceiver<DaemonRequest>,
        outbound: mpsc::UnboundedSender<WireFrame>,
    }

    /// Every frame written by the handler is tagged so tests can distinguish
    /// the initial snapshot response from subsequent notifications in order.
    #[derive(Debug, Clone)]
    pub enum WireFrame {
        Response(DaemonResponse),
        Notification(DaemonNotification),
    }

    impl MemoryConnection {
        /// Construct a connected pair: `(conn, inbound_tx, outbound_rx)`.
        pub fn new() -> (
            Self,
            mpsc::UnboundedSender<DaemonRequest>,
            mpsc::UnboundedReceiver<WireFrame>,
        ) {
            let (in_tx, in_rx) = mpsc::unbounded_channel();
            let (out_tx, out_rx) = mpsc::unbounded_channel();
            let conn = Self {
                inbound: in_rx,
                outbound: out_tx,
            };
            (conn, in_tx, out_rx)
        }
    }

    #[async_trait]
    impl DaemonConnection for MemoryConnection {
        async fn read_request(&mut self) -> Result<Option<DaemonRequest>> {
            Ok(self.inbound.recv().await)
        }
        async fn write_response(&mut self, resp: &DaemonResponse) -> Result<()> {
            // Send errors mean the test dropped the outbound receiver — we
            // treat that as benign EOF (test shut down mid-handler), not a
            // test failure.
            let _ = self.outbound.send(WireFrame::Response(resp.clone()));
            Ok(())
        }
        async fn write_notification(&mut self, notif: &DaemonNotification) -> Result<()> {
            let _ = self.outbound.send(WireFrame::Notification(notif.clone()));
            Ok(())
        }
    }
}

//! Streaming output sink abstraction for the orchestrator.
//!
//! `StreamSink` severs the historical dependency on `pice-cli::engine::output`
//! that existed when orchestration and terminal rendering lived in the same
//! crate. With the v0.2 daemon split, orchestration logic lives in
//! `pice-daemon` and terminal rendering lives in `pice-cli`; the sink is the
//! boundary between them.
//!
//! ## Two channels
//!
//! - [`StreamSink::send_chunk`] — the allocation-free hot path for model
//!   token streaming. `response/chunk` notifications are forwarded verbatim.
//! - [`StreamSink::send_event`] — a structured side channel for warnings,
//!   progress events, and phase transitions. In daemon mode, each event is
//!   relayed to the CLI adapter over the socket as a `cli/stream-event`
//!   notification; in inline mode, it is dispatched locally.
//!
//! ## Why `Arc<dyn StreamSink>`?
//!
//! The orchestrator's `ProviderHost` stores a notification handler closure
//! (`Box<dyn Fn + Send>`) whose captured state must be `'static`. A borrowed
//! `&dyn StreamSink` cannot survive into that closure. Sharing via `Arc`
//! lets the session runner install a closure that holds its own clone
//! alongside any other sink references the caller retains.
//!
//! The [`SharedSink`] type alias is the standard form passed across the
//! `pice-cli` → `pice-daemon::orchestrator` boundary.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// A sink for streaming output emitted during provider session runs.
///
/// Implementations must be `Send + Sync` because a sink handle is typically
/// cloned into a notification handler closure stored in `ProviderHost` and
/// may be invoked from any task that drives `request()`.
pub trait StreamSink: Send + Sync {
    /// Emit a raw text chunk. The chunk is forwarded verbatim — the sink
    /// contract does not trim, buffer, or split on line boundaries.
    fn send_chunk(&self, text: &str);

    /// Emit a structured side-channel event.
    ///
    /// The default implementation is a no-op so minimal sinks (like
    /// [`NullSink`]) do not need to override it.
    fn send_event(&self, event: StreamEvent) {
        let _ = event;
    }
}

/// Shared-ownership handle for a [`StreamSink`]. Standard form passed across
/// the orchestrator boundary.
pub type SharedSink = Arc<dyn StreamSink>;

/// Structured events emitted by orchestration code during session runs.
///
/// `#[non_exhaustive]` so T19 handlers can extend the enum without breaking
/// external consumers. The starting set is deliberately minimal; variants
/// are added as concrete handler needs emerge.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// Advisory message to surface to the user. Today's inline
    /// `eprintln!("warning: …")` sites migrate here so warnings route
    /// through the sink instead of the daemon's stderr, allowing the CLI
    /// adapter to render them consistently whether in socket or inline mode.
    Notice { level: NoticeLevel, message: String },
}

/// Severity classification for [`StreamEvent::Notice`].
///
/// `#[non_exhaustive]` so T19/T21 can add `Debug`/`Trace` levels when the
/// daemon starts relaying internal tracing output without breaking CLI
/// match arms.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Warn,
    Error,
}

/// A sink that discards every chunk and event.
///
/// Use this when a caller needs the `run_session_and_capture` return value
/// but does not want user-visible output — e.g., `pice commit` building a
/// commit message silently from the captured model response.
pub struct NullSink;

impl StreamSink for NullSink {
    fn send_chunk(&self, _text: &str) {}
    fn send_event(&self, _event: StreamEvent) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Test sink that captures every chunk and event for assertions.
    struct CaptureSink {
        chunks: Mutex<Vec<String>>,
        events: Mutex<Vec<StreamEvent>>,
    }

    impl CaptureSink {
        fn new() -> Self {
            Self {
                chunks: Mutex::new(Vec::new()),
                events: Mutex::new(Vec::new()),
            }
        }
    }

    impl StreamSink for CaptureSink {
        fn send_chunk(&self, text: &str) {
            self.chunks.lock().unwrap().push(text.to_string());
        }

        fn send_event(&self, event: StreamEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[test]
    fn null_sink_discards_chunks_and_events() {
        let sink = NullSink;
        sink.send_chunk("hello");
        sink.send_chunk("world");
        sink.send_event(StreamEvent::Notice {
            level: NoticeLevel::Warn,
            message: "ignored".to_string(),
        });
        // No assertions — the point is that nothing panics and nothing is stored.
    }

    #[test]
    fn shared_sink_is_clonable_into_closure() {
        // Prove the ownership model: an `Arc<dyn StreamSink>` can be cloned
        // into a `'static` closure, which is exactly what the session runner
        // does when installing a notification handler on `ProviderHost`.
        let concrete: Arc<CaptureSink> = Arc::new(CaptureSink::new());
        let sink: SharedSink = Arc::clone(&concrete) as SharedSink;
        let sink_clone = Arc::clone(&sink);

        let handler: Box<dyn Fn(&str) + Send + 'static> =
            Box::new(move |text: &str| sink_clone.send_chunk(text));
        handler("chunk-1");
        handler("chunk-2");
        handler("chunk-3");

        let chunks = concrete.chunks.lock().unwrap();
        assert_eq!(chunks.as_slice(), &["chunk-1", "chunk-2", "chunk-3"]);
    }

    #[test]
    fn capture_sink_records_events() {
        let concrete: Arc<CaptureSink> = Arc::new(CaptureSink::new());
        let sink: SharedSink = Arc::clone(&concrete) as SharedSink;

        sink.send_chunk("alpha");
        sink.send_chunk("beta");
        sink.send_event(StreamEvent::Notice {
            level: NoticeLevel::Error,
            message: "deserialize failed".to_string(),
        });

        assert_eq!(
            concrete.chunks.lock().unwrap().as_slice(),
            &["alpha", "beta"]
        );

        let events = concrete.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::Notice { level, message } => {
                assert_eq!(*level, NoticeLevel::Error);
                assert_eq!(message, "deserialize failed");
            }
        }
    }

    #[test]
    fn stream_event_notice_roundtrips_json() {
        let event = StreamEvent::Notice {
            level: NoticeLevel::Warn,
            message: "evaluate/result deserialize failed".to_string(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert_eq!(
            json,
            r#"{"type":"notice","level":"warn","message":"evaluate/result deserialize failed"}"#
        );

        let decoded: StreamEvent = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            StreamEvent::Notice { level, message } => {
                assert_eq!(level, NoticeLevel::Warn);
                assert_eq!(message, "evaluate/result deserialize failed");
            }
        }
    }

    #[test]
    fn notice_level_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&NoticeLevel::Info).unwrap(),
            r#""info""#
        );
        assert_eq!(
            serde_json::to_string(&NoticeLevel::Warn).unwrap(),
            r#""warn""#
        );
        assert_eq!(
            serde_json::to_string(&NoticeLevel::Error).unwrap(),
            r#""error""#
        );
    }
}

//! In-memory captured-log store with bounded history + live broadcast.
//!
//! Design (Phase 7 Task 5):
//!
//! ```text
//! LogStore = DashMap<FeatureId, LogBuffer>
//! LogBuffer = {
//!     history:   VecDeque<LogChunk>        // bounded at 8 MiB, drop-oldest
//!     bytes:     usize                     // running sum of text.len() for eviction
//!     broadcast: broadcast::Sender<LogChunk>  // cap 64 frames
//! }
//! ```
//!
//! **Invariants:**
//! - `append_chunk` writes a non-terminal `LogChunk` — enqueues to
//!   `history`, evicts from the front until under the byte cap, then
//!   broadcasts.
//! - `append_terminal_frame` is called EXACTLY ONCE per feature at
//!   the `FeatureComplete` / `Cancelled` transition. It enqueues +
//!   broadcasts a `LogChunk { terminal: true, reason: Some(_), ... }`.
//!   Follow subscribers watch for `terminal: true` and close their
//!   loops. A second terminal write is a no-op (defensive).
//! - `snapshot` clones the buffer for the non-follow read path.
//! - `subscribe` returns a `broadcast::Receiver` that begins delivery
//!   at the next `append_*` call (history is NOT replayed — callers
//!   who need history + live interleave must use `snapshot` first
//!   then `subscribe`).
//! - `purge` is NOT yet defined — no consumer until `pice clean`
//!   ships (Phase 5.5). Per the `.claude/rules/rust-core.md`
//!   "no-scaffolding" rule we do not pre-declare it.
//!
//! **Byte-cap semantics:** the cap is ~8 MiB of `text` across all
//! buffered chunks. Eviction is greedy: pop from the front until
//! `bytes <= BUFFER_BYTES_CAP`. Individual oversized chunks (>8 MiB
//! of text in one append) still land — the buffer can transiently
//! exceed the cap when a single chunk is larger than it. Better to
//! surface a huge chunk than silently drop it.

use dashmap::DashMap;
use pice_core::events::LogChunk;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use tokio::sync::{broadcast, Mutex};

/// Broadcast channel capacity for each per-feature log channel. Sized
/// smaller than the event-bus capacity because log chunks are bigger
/// and the live `pice logs --follow` consumer is expected to drain at
/// near-line-rate.
pub const CHANNEL_CAPACITY: usize = 64;

/// Buffered history byte cap per feature. ~8 MiB of `text` across all
/// retained `LogChunk`s. Exceeding the cap evicts oldest chunks until
/// the running sum fits under the bound.
pub const BUFFER_BYTES_CAP: usize = 8 * 1024 * 1024;

/// Shared mutable state for a single feature's log history. Wrapped
/// in a `tokio::sync::Mutex` so `append_*` can hold the lock across
/// the enqueue+evict+broadcast sequence without blocking the tokio
/// runtime. The broadcast `Sender` lives inside the mutex-guarded
/// struct because its `.send()` is a mutation the critical section
/// already has exclusive access to.
#[derive(Debug)]
struct LogBuffer {
    history: VecDeque<LogChunk>,
    bytes: usize,
    broadcast: broadcast::Sender<LogChunk>,
    /// Guards against a second `append_terminal_frame` — we broadcast
    /// the terminal frame exactly once, matching the Task 5 invariant.
    terminal_emitted: bool,
}

impl LogBuffer {
    fn new() -> Self {
        // See `EventBus::new` for why we immediately drop the
        // receiver — external subscribers mint their own via
        // `broadcast::Sender::subscribe`.
        let (broadcast, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self {
            history: VecDeque::new(),
            bytes: 0,
            broadcast,
            terminal_emitted: false,
        }
    }

    /// Evict oldest chunks until `bytes <= BUFFER_BYTES_CAP`. Called
    /// AFTER the new chunk has been pushed so the cap check reflects
    /// the latest state. A single oversize chunk (> cap) is retained
    /// regardless — we prefer transient over-cap to silently dropping
    /// a user's log.
    fn evict_to_cap(&mut self) {
        while self.bytes > BUFFER_BYTES_CAP && self.history.len() > 1 {
            // `pop_front` returns None only when `history` is empty,
            // which the `len() > 1` guard already rules out. Always
            // keeping at least one chunk means an oversized single
            // chunk stays retained instead of being silently dropped.
            if let Some(old) = self.history.pop_front() {
                self.bytes = self.bytes.saturating_sub(old.text.len());
            }
        }
    }
}

/// In-memory, feature-scoped log store. Clone-cheap (holds an
/// `Arc<DashMap>`).
#[derive(Debug, Clone, Default)]
pub struct LogStore {
    per_feature: Arc<DashMap<String, Arc<Mutex<LogBuffer>>>>,
}

impl LogStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up (lazily inserting) the buffer handle for a feature.
    fn buffer_for(&self, feature_id: &str) -> Arc<Mutex<LogBuffer>> {
        self.per_feature
            .entry(feature_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(LogBuffer::new())))
            .clone()
    }

    /// Append a non-terminal log chunk. Enqueues to history, evicts
    /// to the byte cap, then broadcasts to live subscribers. Returns
    /// the constructed `LogChunk` so tests can assert on the
    /// timestamp / etc.
    pub async fn append_chunk(
        &self,
        feature_id: &str,
        run_id: &str,
        layer: &str,
        text: String,
    ) -> LogChunk {
        let chunk = LogChunk {
            feature_id: feature_id.to_string(),
            run_id: run_id.to_string(),
            layer: layer.to_string(),
            text,
            timestamp: now_rfc3339(),
            terminal: false,
            reason: None,
        };

        let buffer = self.buffer_for(feature_id);
        let mut guard = buffer.lock().await;
        guard.bytes = guard.bytes.saturating_add(chunk.text.len());
        guard.history.push_back(chunk.clone());
        guard.evict_to_cap();
        // `send()` errors only when there are no subscribers — that
        // is expected and handled the same way as in the event bus
        // (trace-level log only).
        if let Err(err) = guard.broadcast.send(chunk.clone()) {
            tracing::trace!(
                feature_id,
                ?err,
                "no follow subscribers on log chunk broadcast"
            );
        }
        chunk
    }

    /// Append the terminal frame for a feature. Call exactly once at
    /// `FeatureComplete` / `Cancelled`. A second call is a no-op
    /// (defensive — duplicate terminals would confuse follow
    /// subscribers that close on the first).
    pub async fn append_terminal_frame(
        &self,
        feature_id: &str,
        run_id: &str,
        reason: &str,
    ) -> Option<LogChunk> {
        let chunk = LogChunk {
            feature_id: feature_id.to_string(),
            run_id: run_id.to_string(),
            layer: String::new(),
            text: String::new(),
            timestamp: now_rfc3339(),
            terminal: true,
            reason: Some(reason.to_string()),
        };

        let buffer = self.buffer_for(feature_id);
        let mut guard = buffer.lock().await;
        if guard.terminal_emitted {
            tracing::debug!(
                feature_id,
                "ignoring duplicate terminal frame (already emitted)"
            );
            return None;
        }
        guard.terminal_emitted = true;
        // Terminal frame contributes 0 bytes of `text` but still
        // lands in history so `snapshot` callers observe the
        // end-of-stream sentinel.
        guard.history.push_back(chunk.clone());
        guard.evict_to_cap();
        if let Err(err) = guard.broadcast.send(chunk.clone()) {
            tracing::trace!(
                feature_id,
                ?err,
                "no follow subscribers on terminal frame broadcast"
            );
        }
        Some(chunk)
    }

    /// Return a cloned snapshot of the feature's buffered history,
    /// optionally filtered by `layer`. Returns an empty vec for
    /// unknown features (never panics, never errors — the caller
    /// handles the empty-history case).
    pub async fn snapshot(&self, feature_id: &str, layer_filter: Option<&str>) -> Vec<LogChunk> {
        let Some(entry) = self.per_feature.get(feature_id) else {
            return Vec::new();
        };
        let buffer = entry.value().clone();
        drop(entry); // release DashMap shard lock before .await
        let guard = buffer.lock().await;
        match layer_filter {
            Some(want) => guard
                .history
                .iter()
                .filter(|c| c.layer == want || c.terminal)
                .cloned()
                .collect(),
            None => guard.history.iter().cloned().collect(),
        }
    }

    /// Subscribe to live log chunks for a feature. Receivers begin
    /// delivery at the next `append_*` call — history is NOT
    /// replayed. Callers needing history + live should call
    /// `snapshot` BEFORE `subscribe`, accepting that a chunk could
    /// land in both (deduping on (feature_id, timestamp, text) is
    /// cheap at the receiver).
    pub async fn subscribe(&self, feature_id: &str) -> broadcast::Receiver<LogChunk> {
        let buffer = self.buffer_for(feature_id);
        let guard = buffer.lock().await;
        guard.broadcast.subscribe()
    }

    /// Receiver count on a feature's broadcast channel. Test-only.
    #[cfg(test)]
    pub(crate) async fn subscriber_count(&self, feature_id: &str) -> usize {
        let Some(entry) = self.per_feature.get(feature_id) else {
            return 0;
        };
        let buffer = entry.value().clone();
        drop(entry);
        let guard = buffer.lock().await;
        guard.broadcast.receiver_count()
    }
}

/// [`crate::orchestrator::StreamSink`] implementation that forwards
/// provider response chunks into a feature's captured log stream.
///
/// The sink is intentionally small and clone-free for callers: construct one
/// per background run and wrap it in `Arc<dyn StreamSink>`. `send_chunk` is
/// synchronous while `LogStore::append_chunk` is async, so writes are bridged
/// through `tokio::spawn` and serialized by the store's per-feature mutex.
pub struct LogStoreSink {
    store: LogStore,
    feature_id: String,
    run_id: String,
    layer: String,
    pending: Arc<StdMutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl LogStoreSink {
    pub fn new(
        store: LogStore,
        feature_id: impl Into<String>,
        run_id: impl Into<String>,
        layer: impl Into<String>,
    ) -> Self {
        Self {
            store,
            feature_id: feature_id.into(),
            run_id: run_id.into(),
            layer: layer.into(),
            pending: Arc::new(StdMutex::new(Vec::new())),
        }
    }

    /// Wait for all spawned `append_chunk` tasks to land in the store.
    ///
    /// `send_chunk` is synchronous because it implements [`StreamSink`], but
    /// `LogStore::append_chunk` is async. Background handlers call `flush`
    /// after provider shutdown and before writing the terminal frame so the
    /// terminal frame cannot overtake buffered provider output.
    pub async fn flush(&self) {
        loop {
            let handles = match self.pending.lock() {
                Ok(mut pending) => {
                    if pending.is_empty() {
                        break;
                    }
                    std::mem::take(&mut *pending)
                }
                Err(poisoned) => {
                    let mut pending = poisoned.into_inner();
                    if pending.is_empty() {
                        break;
                    }
                    std::mem::take(&mut *pending)
                }
            };

            for handle in handles {
                if let Err(err) = handle.await {
                    tracing::warn!(?err, "log-store sink append task failed");
                }
            }
        }
    }
}

impl crate::orchestrator::StreamSink for LogStoreSink {
    fn send_chunk(&self, text: &str) {
        let store = self.store.clone();
        let feature = self.feature_id.clone();
        let run = self.run_id.clone();
        let layer = self.layer.clone();
        let text_owned = text.to_string();
        let handle = tokio::spawn(async move {
            store.append_chunk(&feature, &run, &layer, text_owned).await;
        });
        match self.pending.lock() {
            Ok(mut pending) => pending.push(handle),
            Err(poisoned) => {
                tracing::warn!("log-store sink pending-task mutex was poisoned");
                let mut pending = poisoned.into_inner();
                pending.push(handle);
            }
        }
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn append_and_snapshot_roundtrip() {
        let store = LogStore::new();
        store
            .append_chunk("feat-1", "run-1", "backend", "hello".into())
            .await;
        store
            .append_chunk("feat-1", "run-1", "backend", "world".into())
            .await;

        let snap = store.snapshot("feat-1", None).await;
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].text, "hello");
        assert_eq!(snap[1].text, "world");
        assert!(!snap[0].terminal);
    }

    #[tokio::test]
    async fn snapshot_filters_by_layer_but_keeps_terminal() {
        let store = LogStore::new();
        store
            .append_chunk("feat-1", "r", "backend", "b1".into())
            .await;
        store
            .append_chunk("feat-1", "r", "frontend", "f1".into())
            .await;
        store
            .append_terminal_frame("feat-1", "r", "feature-complete")
            .await;

        let backend = store.snapshot("feat-1", Some("backend")).await;
        // backend chunks + terminal frame (which has empty layer but
        // the filter admits it because the consumer must observe
        // end-of-stream regardless of layer).
        assert_eq!(backend.len(), 2);
        assert!(backend[0].text == "b1" || backend[0].terminal);
        assert!(backend.last().unwrap().terminal);
    }

    #[tokio::test]
    async fn snapshot_empty_for_unknown_feature() {
        let store = LogStore::new();
        let snap = store.snapshot("ghost-feature", None).await;
        assert!(snap.is_empty());
    }

    #[tokio::test]
    async fn byte_cap_drops_oldest_chunks() {
        let store = LogStore::new();
        // 2 MiB per chunk, 5 chunks = 10 MiB total — cap is 8 MiB,
        // so after the fifth chunk we expect the oldest to be
        // evicted. Choice of 2 MiB * 5 instead of 5 MiB * 2 lets us
        // observe a mid-buffer eviction point (not the degenerate
        // "evict everything but the newest" case).
        let big = "x".repeat(2 * 1024 * 1024);
        for i in 0..5 {
            store
                .append_chunk("feat-big", "r", "backend", format!("{i}:{big}"))
                .await;
        }
        let snap = store.snapshot("feat-big", None).await;
        // After 5th chunk: ~10 MiB > 8 MiB cap. Per-chunk size is
        // 2 MiB + ~2 bytes (the "{i}:" prefix), so evicting chunk 0
        // leaves ~8.000008 MiB which is still over the cap — evict
        // chunk 1 too, leaving ~6 MiB across chunks 2/3/4.
        assert_eq!(
            snap.len(),
            3,
            "expected two evictions leaving 3 chunks, got {}",
            snap.len()
        );
        assert!(
            snap[0].text.starts_with("2:"),
            "oldest remaining should be index 2, got: {:?}",
            &snap[0].text[..16]
        );
    }

    #[tokio::test]
    async fn oversized_single_chunk_is_retained() {
        let store = LogStore::new();
        // Single chunk larger than the cap — we prefer transient
        // over-cap to silent drop.
        let huge = "y".repeat(10 * 1024 * 1024); // 10 MiB, > 8 MiB cap
        store.append_chunk("feat-huge", "r", "backend", huge).await;
        let snap = store.snapshot("feat-huge", None).await;
        assert_eq!(snap.len(), 1, "oversized single chunk must be retained");
    }

    #[tokio::test]
    async fn subscribe_delivers_subsequent_appends() {
        let store = LogStore::new();
        let mut rx = store.subscribe("feat-sub").await;
        // Subscribe first, then append — subscriber must see it.
        store
            .append_chunk("feat-sub", "r", "backend", "hi".into())
            .await;

        let chunk = rx.recv().await.unwrap();
        assert_eq!(chunk.text, "hi");
        assert!(!chunk.terminal);
    }

    #[tokio::test]
    async fn subscribe_does_not_replay_history() {
        let store = LogStore::new();
        // Append BEFORE subscribing; the pre-subscribe history must
        // NOT arrive on the channel.
        store
            .append_chunk("feat-hist", "r", "backend", "past".into())
            .await;
        let mut rx = store.subscribe("feat-hist").await;
        store
            .append_chunk("feat-hist", "r", "backend", "present".into())
            .await;

        let chunk = rx.recv().await.unwrap();
        assert_eq!(chunk.text, "present");
    }

    #[tokio::test]
    async fn terminal_frame_observed_by_follow_subscriber() {
        let store = LogStore::new();
        let mut rx = store.subscribe("feat-term").await;
        store
            .append_chunk("feat-term", "r", "backend", "mid".into())
            .await;
        store
            .append_terminal_frame("feat-term", "r", "feature-complete")
            .await;

        let first = rx.recv().await.unwrap();
        assert_eq!(first.text, "mid");
        assert!(!first.terminal);
        let second = rx.recv().await.unwrap();
        assert!(second.terminal);
        assert_eq!(second.reason.as_deref(), Some("feature-complete"));
    }

    #[tokio::test]
    async fn duplicate_terminal_frame_is_noop() {
        let store = LogStore::new();
        let first = store
            .append_terminal_frame("feat-dup", "r", "feature-complete")
            .await;
        assert!(first.is_some());
        let second = store
            .append_terminal_frame("feat-dup", "r", "cancelled")
            .await;
        assert!(second.is_none(), "second terminal must be a no-op");
        let snap = store.snapshot("feat-dup", None).await;
        assert_eq!(snap.len(), 1, "only the first terminal frame is retained");
    }

    #[tokio::test]
    async fn subscriber_count_reflects_live_receivers() {
        let store = LogStore::new();
        assert_eq!(store.subscriber_count("feat-cnt").await, 0);
        let rx = store.subscribe("feat-cnt").await;
        assert_eq!(store.subscriber_count("feat-cnt").await, 1);
        drop(rx);
        assert_eq!(store.subscriber_count("feat-cnt").await, 0);
    }

    #[tokio::test]
    async fn cross_feature_isolation() {
        let store = LogStore::new();
        let mut rx_a = store.subscribe("feat-a").await;
        store
            .append_chunk("feat-b", "r", "backend", "b-only".into())
            .await;
        match rx_a.try_recv() {
            Err(broadcast::error::TryRecvError::Empty) => {}
            other => panic!("expected Empty, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn log_store_sink_appends_provider_chunks() {
        let store = LogStore::new();
        let sink = LogStoreSink::new(store.clone(), "feat-sink", "run-sink", "evaluate");

        crate::orchestrator::StreamSink::send_chunk(&sink, "provider output");
        sink.flush().await;

        let snap = store.snapshot("feat-sink", None).await;
        let chunk = snap.first().expect("provider chunk");
        assert_eq!(chunk.feature_id, "feat-sink");
        assert_eq!(chunk.run_id, "run-sink");
        assert_eq!(chunk.layer, "evaluate");
        assert_eq!(chunk.text, "provider output");
    }

    #[tokio::test]
    async fn log_store_sink_flush_keeps_terminal_frame_after_provider_chunks() {
        let store = LogStore::new();
        let sink = LogStoreSink::new(store.clone(), "feat-order", "run-order", "evaluate");

        crate::orchestrator::StreamSink::send_chunk(&sink, "first");
        crate::orchestrator::StreamSink::send_chunk(&sink, "second");
        sink.flush().await;
        store
            .append_terminal_frame("feat-order", "run-order", "passed")
            .await;

        let snap = store.snapshot("feat-order", None).await;
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0].text, "first");
        assert_eq!(snap[1].text, "second");
        assert!(snap[2].terminal, "terminal frame must be last: {snap:?}");
    }
}

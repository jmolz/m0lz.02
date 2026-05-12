//! In-memory `broadcast::Sender` fan-out for manifest state transitions.
//!
//! `EventBus` is the producer side of every `manifest/event`
//! notification. Subscribers (Task 6's `manifest/subscribe` handler)
//! acquire a `broadcast::Receiver` via [`EventBus::subscribe_feature`]
//! or [`EventBus::subscribe_wildcard`] and consume events until the
//! connection closes.
//!
//! **Shape:**
//! - `per_feature: DashMap<String, broadcast::Sender<ManifestEventPayload>>`
//!   — one channel per feature_id. Senders are lazily inserted on the
//!   first `subscribe_feature` or `publish` call for that id. When
//!   Receivers drop, the Sender remains in the DashMap (channel
//!   subscriber count decrements naturally). No `SubscriptionRegistry`
//!   — the plan is explicit about this (see `.claude/plans/phase-7-…`
//!   Task 4 and Task 6). The DashMap grows bounded by unique feature
//!   ids, which is acceptable for a long-lived daemon.
//! - `wildcard: broadcast::Sender<ManifestEventPayload>` — a single
//!   shared channel that receives EVERY published event regardless of
//!   feature. Used by `pice status --follow` (no feature_id) and
//!   dashboard adapters.
//!
//! **Capacity:** both channels use `CHANNEL_CAPACITY = 256` frames. A
//! receiver that falls more than 256 events behind receives
//! `broadcast::error::RecvError::Lagged(n)` on the next `recv()` — the
//! subscribe handler (Task 6) logs `tracing::warn!` and closes the
//! subscription, per the plan's "lagged receivers log warn and drop"
//! rule. The BUS does NOT drop senders on lag; lag detection is a
//! receiver-side concern.
//!
//! **Publish visibility:** `publish` is `pub(crate)` — external crates
//! MUST go through one of the typed `emit_*` helpers. This prevents
//! accidental mis-construction of a `ManifestEventPayload` with a
//! wrong `event` / `data` / timestamp combo.

use dashmap::DashMap;
use pice_core::events::{ManifestEvent, ManifestEventPayload};
use std::sync::Arc;
use tokio::sync::broadcast;

/// Broadcast channel capacity for each per-feature and the wildcard
/// channel. Sized for a comfortable headroom over the ~dozen-events-
/// per-cohort steady state. Receivers lagging beyond this bound
/// surface `RecvError::Lagged(n)` and are closed by the subscribe
/// handler.
pub const CHANNEL_CAPACITY: usize = 256;

/// In-memory event fan-out for manifest state transitions.
///
/// Clone-cheap: holds `Arc`s to the DashMap and wildcard Sender.
#[derive(Debug, Clone)]
pub struct EventBus {
    per_feature: Arc<DashMap<String, broadcast::Sender<ManifestEventPayload>>>,
    wildcard: broadcast::Sender<ManifestEventPayload>,
}

impl EventBus {
    /// Create a new bus with an empty per-feature map and a fresh
    /// wildcard channel.
    pub fn new() -> Self {
        // `broadcast::channel` returns `(Sender, Receiver)`; we drop
        // the receiver immediately because the wildcard Sender retains
        // the channel (Receivers are minted on demand via
        // `subscribe_wildcard`). Without this initial receiver drop
        // the channel's subscriber count starts at 1 (our held
        // receiver) and `.send()` reports 1 delivery even when no
        // external subscriber has called `subscribe()` yet.
        let (wildcard, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self {
            per_feature: Arc::new(DashMap::new()),
            wildcard,
        }
    }

    /// Subscribe to a specific feature's events. Lazily inserts a
    /// Sender into `per_feature` if none exists yet. Wildcard
    /// subscribers receive the SAME events on their own channel — they
    /// must NOT `subscribe_feature` additionally to avoid duplicates.
    pub fn subscribe_feature(&self, feature_id: &str) -> broadcast::Receiver<ManifestEventPayload> {
        // `entry().or_insert_with(...)` handles the race where two
        // subscribers (or a subscriber + a publisher) arrive
        // concurrently — DashMap's entry API serializes the insert.
        let sender = self
            .per_feature
            .entry(feature_id.to_string())
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(CHANNEL_CAPACITY);
                tx
            });
        sender.subscribe()
    }

    /// Subscribe to every feature's events via the wildcard channel.
    /// Used by `pice status --follow` without a feature_id and future
    /// dashboard adapters.
    pub fn subscribe_wildcard(&self) -> broadcast::Receiver<ManifestEventPayload> {
        self.wildcard.subscribe()
    }

    // ─── Typed emit helpers ──────────────────────────────────────────
    //
    // Each helper constructs a `ManifestEventPayload` from the event
    // kind's specific fields + stamps `Utc::now()` then calls the
    // internal `publish`. This is the ONLY sanctioned entry point for
    // external crates (handlers/orchestrator/saver).

    /// Emit `LayerStarted` — a layer transitioned `Pending → InProgress`.
    pub fn emit_layer_started(&self, feature_id: &str, run_id: &str, layer: &str) {
        self.publish(ManifestEventPayload {
            feature_id: feature_id.to_string(),
            run_id: run_id.to_string(),
            event: ManifestEvent::LayerStarted,
            layer: Some(layer.to_string()),
            data: serde_json::Value::Null,
            timestamp: now_rfc3339(),
        });
    }

    /// Emit `PassComplete` — the adaptive loop finished a single pass
    /// for `layer`. `data` carries the pass index + score + cost when
    /// the caller wants to include them.
    pub fn emit_pass_complete(
        &self,
        feature_id: &str,
        run_id: &str,
        layer: &str,
        data: serde_json::Value,
    ) {
        self.publish(ManifestEventPayload {
            feature_id: feature_id.to_string(),
            run_id: run_id.to_string(),
            event: ManifestEvent::PassComplete,
            layer: Some(layer.to_string()),
            data,
            timestamp: now_rfc3339(),
        });
    }

    /// Emit `GateRequested` — a review gate was appended. `data`
    /// carries `{gate_id, trigger_expression}` so subscribers (e.g.
    /// the desktop notifier, dashboard) can render without a separate
    /// manifest fetch.
    pub fn emit_gate_requested(
        &self,
        feature_id: &str,
        run_id: &str,
        layer: &str,
        data: serde_json::Value,
    ) {
        self.publish(ManifestEventPayload {
            feature_id: feature_id.to_string(),
            run_id: run_id.to_string(),
            event: ManifestEvent::GateRequested,
            layer: Some(layer.to_string()),
            data,
            timestamp: now_rfc3339(),
        });
    }

    /// Emit `GateDecided` — a reviewer recorded a decision or a
    /// timeout fired. `data` carries `{gate_id, decision}`.
    pub fn emit_gate_decided(
        &self,
        feature_id: &str,
        run_id: &str,
        layer: &str,
        data: serde_json::Value,
    ) {
        self.publish(ManifestEventPayload {
            feature_id: feature_id.to_string(),
            run_id: run_id.to_string(),
            event: ManifestEvent::GateDecided,
            layer: Some(layer.to_string()),
            data,
            timestamp: now_rfc3339(),
        });
    }

    /// Emit `SeamFinding` — a seam check produced a result (pass /
    /// warn / fail). `data` carries `{boundary, check_id, severity}`.
    pub fn emit_seam_finding(
        &self,
        feature_id: &str,
        run_id: &str,
        layer: &str,
        data: serde_json::Value,
    ) {
        self.publish(ManifestEventPayload {
            feature_id: feature_id.to_string(),
            run_id: run_id.to_string(),
            event: ManifestEvent::SeamFinding,
            layer: Some(layer.to_string()),
            data,
            timestamp: now_rfc3339(),
        });
    }

    /// Emit `LayerComplete` — the adaptive loop halted for `layer` and
    /// the result landed in the manifest. `data` carries the final
    /// status + halted_by.
    pub fn emit_layer_complete(
        &self,
        feature_id: &str,
        run_id: &str,
        layer: &str,
        data: serde_json::Value,
    ) {
        self.publish(ManifestEventPayload {
            feature_id: feature_id.to_string(),
            run_id: run_id.to_string(),
            event: ManifestEvent::LayerComplete,
            layer: Some(layer.to_string()),
            data,
            timestamp: now_rfc3339(),
        });
    }

    /// Emit `FeatureComplete` — the manifest reached a terminal
    /// `overall_status`. Subscribers terminate their streams on this
    /// event (paired with the terminal log frame from `LogStore`).
    pub fn emit_feature_complete(&self, feature_id: &str, run_id: &str, data: serde_json::Value) {
        self.publish(ManifestEventPayload {
            feature_id: feature_id.to_string(),
            run_id: run_id.to_string(),
            event: ManifestEvent::FeatureComplete,
            layer: None,
            data,
            timestamp: now_rfc3339(),
        });
    }

    /// Emit `Cancelled` — the feature was aborted (SIGINT, daemon
    /// shutdown drain, or a panicked orchestrator future). `data`
    /// carries `{reason}`.
    pub fn emit_cancelled(&self, feature_id: &str, run_id: &str, reason: &str) {
        self.publish(ManifestEventPayload {
            feature_id: feature_id.to_string(),
            run_id: run_id.to_string(),
            event: ManifestEvent::Cancelled,
            layer: None,
            data: serde_json::json!({ "reason": reason }),
            timestamp: now_rfc3339(),
        });
    }

    /// Internal fan-out: publish to the per-feature channel (if one
    /// exists) AND the wildcard channel. `pub(crate)` so the saver
    /// impl can reach it; external crates must go through the typed
    /// `emit_*` helpers.
    ///
    /// A `send()` failure on either channel means "no active
    /// subscribers" — that is expected and not an error. Logged at
    /// `trace` so a quiet daemon doesn't spam the log.
    pub(crate) fn publish(&self, payload: ManifestEventPayload) {
        // Per-feature fan-out. We only touch the DashMap if an entry
        // exists — lazily creating one just to publish into would leak
        // empty senders for every one-shot emit.
        if let Some(sender) = self.per_feature.get(&payload.feature_id) {
            if let Err(err) = sender.send(payload.clone()) {
                tracing::trace!(
                    feature_id = %payload.feature_id,
                    ?err,
                    "no per-feature subscribers for manifest event"
                );
            }
        }
        // Wildcard fan-out.
        if let Err(err) = self.wildcard.send(payload) {
            tracing::trace!(?err, "no wildcard subscribers for manifest event");
        }
    }

    /// Receiver count for the wildcard channel — used by tests and
    /// diagnostics.
    pub fn wildcard_receiver_count(&self) -> usize {
        self.wildcard.receiver_count()
    }

    /// Receiver count for one feature channel. Missing feature channels
    /// have zero receivers.
    pub fn feature_receiver_count(&self, feature_id: &str) -> usize {
        self.per_feature
            .get(feature_id)
            .map(|sender| sender.receiver_count())
            .unwrap_or(0)
    }

    /// Receiver count across wildcard and every per-feature channel.
    pub fn total_receiver_count(&self) -> usize {
        self.wildcard_receiver_count()
            + self
                .per_feature
                .iter()
                .map(|entry| entry.receiver_count())
                .sum::<usize>()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_payload(feature_id: &str, event: ManifestEvent) -> ManifestEventPayload {
        ManifestEventPayload {
            feature_id: feature_id.to_string(),
            run_id: "run-abc".to_string(),
            event,
            layer: None,
            data: serde_json::Value::Null,
            timestamp: now_rfc3339(),
        }
    }

    #[tokio::test]
    async fn publish_subscribe_roundtrip_per_feature() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe_feature("feat-1");

        bus.emit_layer_started("feat-1", "run-abc", "backend");

        let evt = rx
            .recv()
            .await
            .expect("subscriber should receive the emitted event");
        assert_eq!(evt.feature_id, "feat-1");
        assert_eq!(evt.event, ManifestEvent::LayerStarted);
        assert_eq!(evt.layer.as_deref(), Some("backend"));
    }

    #[tokio::test]
    async fn wildcard_receives_every_feature() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe_wildcard();

        bus.emit_layer_started("feat-1", "run-1", "backend");
        bus.emit_layer_started("feat-2", "run-2", "frontend");

        let first = rx.recv().await.unwrap();
        let second = rx.recv().await.unwrap();
        let ids: Vec<_> = [first, second]
            .iter()
            .map(|e| e.feature_id.clone())
            .collect();
        assert!(
            ids.contains(&"feat-1".to_string()) && ids.contains(&"feat-2".to_string()),
            "wildcard should observe both features, got {ids:?}"
        );
    }

    #[tokio::test]
    async fn per_feature_does_not_leak_to_other_features() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe_feature("feat-1");

        bus.emit_layer_started("feat-2", "run-2", "frontend");

        // `try_recv` on an empty channel returns Empty (not a feat-2
        // event): proves cross-feature isolation on the per-feature
        // path.
        match rx.try_recv() {
            Err(broadcast::error::TryRecvError::Empty) => {}
            other => panic!("expected Empty, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn lagged_receiver_surfaces_lagged_error() {
        // Forge a lag condition: build a bus with a tiny capacity by
        // calling the constructor directly. We can't lower
        // `CHANNEL_CAPACITY` without changing the constant, so we
        // simulate by publishing > 256 events on a real bus and then
        // catching the first Lagged on the subscriber. This is the
        // contract: the bus itself does NOT panic, drop, or otherwise
        // interfere — the receiver observes Lagged and handles it.
        let bus = EventBus::new();
        let mut rx = bus.subscribe_feature("feat-lag");

        // Flood 1 + CHANNEL_CAPACITY + 1 events without the receiver
        // consuming anything. The extra +1 guarantees overflow.
        for _ in 0..(CHANNEL_CAPACITY + 2) {
            bus.publish(sample_payload("feat-lag", ManifestEvent::LayerStarted));
        }

        let first = rx.recv().await;
        match first {
            Err(broadcast::error::RecvError::Lagged(n)) => {
                assert!(n >= 1, "lag count should be at least 1, got {n}");
            }
            other => panic!("expected RecvError::Lagged, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn subscriber_count_reflects_active_receivers() {
        let bus = EventBus::new();
        assert_eq!(bus.wildcard_receiver_count(), 0);
        let rx1 = bus.subscribe_wildcard();
        let rx2 = bus.subscribe_wildcard();
        assert_eq!(bus.wildcard_receiver_count(), 2);
        drop(rx1);
        assert_eq!(bus.wildcard_receiver_count(), 1);
        drop(rx2);
        assert_eq!(bus.wildcard_receiver_count(), 0);
    }

    #[tokio::test]
    async fn publish_without_subscribers_is_silent() {
        let bus = EventBus::new();
        // No subscribers on either channel — publish should not panic
        // or emit any error level higher than trace.
        bus.emit_feature_complete("feat-quiet", "run-x", serde_json::Value::Null);
        // If we reached here, the silent-publish contract held.
    }

    #[tokio::test]
    async fn all_emit_helpers_produce_matching_event_kind() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe_feature("feat-all");

        bus.emit_layer_started("feat-all", "run-1", "backend");
        bus.emit_pass_complete(
            "feat-all",
            "run-1",
            "backend",
            serde_json::json!({"pass": 1}),
        );
        bus.emit_gate_requested(
            "feat-all",
            "run-1",
            "backend",
            serde_json::json!({"gate_id": "g1"}),
        );
        bus.emit_gate_decided(
            "feat-all",
            "run-1",
            "backend",
            serde_json::json!({"decision": "approve"}),
        );
        bus.emit_seam_finding(
            "feat-all",
            "run-1",
            "backend",
            serde_json::json!({"boundary": "api↔db"}),
        );
        bus.emit_layer_complete(
            "feat-all",
            "run-1",
            "backend",
            serde_json::json!({"status": "passed"}),
        );
        bus.emit_feature_complete("feat-all", "run-1", serde_json::json!({"status": "passed"}));
        bus.emit_cancelled("feat-all", "run-1", "sigterm");

        let expected = [
            ManifestEvent::LayerStarted,
            ManifestEvent::PassComplete,
            ManifestEvent::GateRequested,
            ManifestEvent::GateDecided,
            ManifestEvent::SeamFinding,
            ManifestEvent::LayerComplete,
            ManifestEvent::FeatureComplete,
            ManifestEvent::Cancelled,
        ];
        for want in expected {
            let evt = rx.recv().await.expect("receiver should see each emit");
            assert_eq!(evt.event, want, "event kind ordering mismatch");
        }
    }

    #[test]
    fn now_rfc3339_renders_zulu() {
        let ts = now_rfc3339();
        assert!(ts.ends_with('Z'), "expected zulu suffix, got {ts}");
    }
}

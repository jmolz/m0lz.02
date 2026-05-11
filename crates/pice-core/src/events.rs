//! Phase 7 event types — the wire shape of `manifest/event` notifications
//! and `logs/chunk` notifications that the daemon broadcasts to subscribed
//! CLI adapters over the daemon RPC socket.
//!
//! ## No `Snapshot` pseudo-event
//!
//! `ManifestEvent` intentionally carries **only real state transitions**
//! (8 variants). Initial state is delivered as the normal RPC response body
//! of `manifest/subscribe` (see `crates/pice-core/src/protocol/subscribe.rs`);
//! it is not a pseudo-event. This resolves the Cycle 1 schema hole that
//! forced subscribers to distinguish "snapshot" from "event" on the same
//! channel — the two are now separate: one RPC result, then a stream of
//! notifications.
//!
//! ## Wire shape
//!
//! ```jsonc
//! // manifest/event notification params
//! {
//!   "feature_id": "auth-20260421",
//!   "run_id": "r-1001",
//!   "event": { "event_type": "layer_started" },
//!   "layer": "backend",
//!   "data": { /* event-specific payload */ },
//!   "timestamp": "2026-04-21T10:00:00Z"
//! }
//! ```
//!
//! The `event` field carries a `serde(tag = "event_type")` discriminant so
//! pattern-matching consumers (dashboard, CLI renderer) can route on a
//! single field without inspecting `data`.

use crate::protocol::subscribe::SubscribeManifestResponse;
use serde::{Deserialize, Serialize};

/// Phase 7 NDJSON envelope emitted by `pice status --follow --stream-json`
/// and `pice logs --follow --stream-json`.
///
/// Each line of stdout in `--stream-json` mode is one `StreamJsonFrame`.
/// The envelope is heterogeneous so consumers pattern-match on `kind`:
///
/// - First line: `snapshot` — initial state at subscribe time.
/// - Middle lines: `event` or `log-chunk` — live manifest/log frames.
/// - Last line before exit: `terminal` — exit code signalling stream close.
///
/// Callers that want a homogeneous stream can filter on `kind == "event"`.
/// See `.claude/rules/daemon.md` → "Streaming and JSON mode" for the
/// channel-ownership rule (stdout for frames, stderr for prompts).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum StreamJsonFrame {
    /// The initial subscribe-response snapshot. Emitted exactly once per
    /// stream, as the first line.
    Snapshot { snapshot: SubscribeManifestResponse },
    /// A live manifest event forwarded from the subscribe stream.
    Event { event: ManifestEventPayload },
    /// A live log chunk forwarded from the logs stream.
    LogChunk { chunk: LogChunk },
    /// End-of-stream marker. Carries the process exit code the CLI will
    /// return after the stream closes (0 / 2 / 3 / 4 / 5 per
    /// `ExitJsonStatus::exit_code`). Exactly one terminal frame per stream.
    Terminal { exit_code: i32 },
}

/// Eight-variant enum covering every real manifest state transition emitted
/// during a Stack Loops run. Consumed by `pice status --follow`, the future
/// v0.3 dashboard, and the CI adapter.
///
/// Variants are intentionally zero-field enums: auxiliary data (gate id,
/// pass index, seam boundary) flows through [`ManifestEventPayload::data`]
/// as free-form JSON. Keeping the enum flat avoids schema churn as new
/// auxiliary fields land in later phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum ManifestEvent {
    /// Layer transitioned `Pending → InProgress`. Fires for every layer in
    /// every cohort as the cohort begins executing — not just the first.
    LayerStarted,
    /// An adaptive-loop pass completed and its metrics were persisted.
    /// `data` carries the pass index + confidence delta.
    PassComplete,
    /// A review gate was appended to the layer's pending-gate list.
    /// `data` carries the `gate_id` and any rendering hints.
    GateRequested,
    /// A review gate decision was recorded (approve / reject / skip / timeout).
    /// `data` carries the audit-decision string and reviewer identity.
    GateDecided,
    /// A seam check produced a finding. `data` carries the boundary name,
    /// check id, and severity.
    SeamFinding,
    /// Layer transitioned to a terminal status (`Passed` / `Failed` /
    /// `Skipped` / `PendingReview`). `data` carries the final status.
    LayerComplete,
    /// The feature reached its overall terminal status. Always the last
    /// `ManifestEvent` for a feature run. Followed by a `LogChunk`
    /// carrying `terminal: true` on the logs stream.
    FeatureComplete,
    /// The feature was cancelled mid-run (explicit cancel, panic, or
    /// `drain_on_shutdown`). `data` carries `{"reason": "..."}`.
    Cancelled,
}

impl ManifestEvent {
    /// Returns the serialized discriminant string. Tests pin this against
    /// the serde output to prevent silent rename drift (same pattern as
    /// [`crate::cli::ExitJsonStatus::as_str`]).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LayerStarted => "layer_started",
            Self::PassComplete => "pass_complete",
            Self::GateRequested => "gate_requested",
            Self::GateDecided => "gate_decided",
            Self::SeamFinding => "seam_finding",
            Self::LayerComplete => "layer_complete",
            Self::FeatureComplete => "feature_complete",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Wire envelope carried by every `manifest/event` daemon-RPC notification.
///
/// `run_id` is always populated. The daemon mints it at dispatch time and
/// writes it to the on-disk manifest on the `Queued → InProgress`
/// transition, so consumers can correlate events against persisted state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestEventPayload {
    pub feature_id: String,
    pub run_id: String,
    pub event: ManifestEvent,
    /// Optional — `FeatureComplete` and `Cancelled` are feature-scoped;
    /// every other variant carries the layer name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    /// Event-specific payload. Structure varies by variant; see variant
    /// docs for expected shape.
    #[serde(default)]
    pub data: serde_json::Value,
    /// RFC3339 UTC timestamp (`chrono::Utc::now().to_rfc3339(...)`).
    pub timestamp: String,
}

/// Wire envelope carried by every `logs/chunk` daemon-RPC notification, and
/// by the `logs/stream` initial-snapshot response's `history` vector.
///
/// `terminal: true` marks the end-of-stream frame for follow subscribers.
/// Exactly one terminal frame is emitted per feature (at `FeatureComplete`
/// or `Cancelled`). After observing a terminal frame, follow subscribers
/// close their connection and exit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LogChunk {
    pub feature_id: String,
    pub run_id: String,
    pub layer: String,
    pub text: String,
    pub timestamp: String,
    #[serde(default)]
    pub terminal: bool,
    /// Present only on the terminal frame (carries `overall_status.to_string()`
    /// or a panic reason). `None` on non-terminal chunks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn manifest_event_layer_started_roundtrip() {
        let ev = ManifestEvent::LayerStarted;
        let wire = serde_json::to_string(&ev).unwrap();
        assert_eq!(wire, r#"{"event_type":"layer_started"}"#);
        let back: ManifestEvent = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn manifest_event_all_eight_variants_roundtrip() {
        for v in [
            ManifestEvent::LayerStarted,
            ManifestEvent::PassComplete,
            ManifestEvent::GateRequested,
            ManifestEvent::GateDecided,
            ManifestEvent::SeamFinding,
            ManifestEvent::LayerComplete,
            ManifestEvent::FeatureComplete,
            ManifestEvent::Cancelled,
        ] {
            let wire = serde_json::to_string(&v).unwrap();
            let back: ManifestEvent = serde_json::from_str(&wire).unwrap();
            assert_eq!(back, v, "roundtrip failed for {v:?}");
        }
    }

    /// Lock `ManifestEvent::as_str()` against the serde `rename_all = "snake_case"`
    /// output. Mirrors the `ExitJsonStatus::as_str_matches_serde_kebab_case`
    /// parity test — handlers emit via `as_str()` directly (bypassing serde),
    /// so the two paths can silently drift. This test fails on mismatch,
    /// forcing future variant renames to update BOTH the serde derive AND
    /// the match arm.
    #[test]
    fn manifest_event_kebab_parity() {
        let all = [
            ManifestEvent::LayerStarted,
            ManifestEvent::PassComplete,
            ManifestEvent::GateRequested,
            ManifestEvent::GateDecided,
            ManifestEvent::SeamFinding,
            ManifestEvent::LayerComplete,
            ManifestEvent::FeatureComplete,
            ManifestEvent::Cancelled,
        ];
        for v in &all {
            let wire = serde_json::to_string(v).unwrap();
            // Internally-tagged enum serializes as `{"event_type":"<snake>"}`.
            let expected = format!(r#"{{"event_type":"{}"}}"#, v.as_str());
            assert_eq!(
                wire, expected,
                "{v:?}: serde={wire} vs as_str={expected}; update the match arm or rename"
            );
        }
    }

    #[test]
    fn manifest_event_no_snapshot_variant_exists() {
        // Compile-time-adjacent guard: deserialize of the old `snapshot`
        // discriminant must now fail. Cycle 1 resolution removed the
        // pseudo-event; leaving a legacy wire path would be misleading.
        let bad = r#"{"event_type":"snapshot"}"#;
        let err = serde_json::from_str::<ManifestEvent>(bad).unwrap_err();
        assert!(
            err.to_string().contains("unknown variant") || err.to_string().contains("snapshot"),
            "expected unknown-variant error, got: {err}"
        );
    }

    #[test]
    fn manifest_event_payload_roundtrip_with_layer() {
        let payload = ManifestEventPayload {
            feature_id: "auth-20260421".to_string(),
            run_id: "r-1001".to_string(),
            event: ManifestEvent::LayerStarted,
            layer: Some("backend".to_string()),
            data: json!({"pass_index": 0}),
            timestamp: "2026-04-21T10:00:00Z".to_string(),
        };
        let wire = serde_json::to_string(&payload).unwrap();
        assert!(wire.contains(r#""event_type":"layer_started""#));
        assert!(wire.contains(r#""layer":"backend""#));
        let back: ManifestEventPayload = serde_json::from_str(&wire).unwrap();
        assert_eq!(back.feature_id, payload.feature_id);
        assert_eq!(back.run_id, payload.run_id);
        assert_eq!(back.event, payload.event);
        assert_eq!(back.layer, payload.layer);
        assert_eq!(back.data, payload.data);
        assert_eq!(back.timestamp, payload.timestamp);
    }

    #[test]
    fn manifest_event_payload_roundtrip_without_layer() {
        // FeatureComplete + Cancelled are feature-scoped — layer is omitted.
        let payload = ManifestEventPayload {
            feature_id: "auth-20260421".to_string(),
            run_id: "r-1001".to_string(),
            event: ManifestEvent::FeatureComplete,
            layer: None,
            data: json!({"overall_status": "passed"}),
            timestamp: "2026-04-21T10:04:00Z".to_string(),
        };
        let wire = serde_json::to_string(&payload).unwrap();
        // Optional `layer` is skipped when None.
        assert!(
            !wire.contains(r#""layer":"#),
            "layer should be omitted when None; got {wire}"
        );
        let back: ManifestEventPayload = serde_json::from_str(&wire).unwrap();
        assert!(back.layer.is_none());
    }

    #[test]
    fn manifest_event_payload_rejects_unknown_fields() {
        // deny_unknown_fields: a misspelled field must error, not silently drop.
        let bad = r#"{"feature_id":"f","run_id":"r","event":{"event_type":"layer_started"},"timestamp":"2026","bogusField":1}"#;
        let err = serde_json::from_str::<ManifestEventPayload>(bad).unwrap_err();
        assert!(
            err.to_string().contains("bogusField") || err.to_string().contains("unknown field"),
            "expected unknown-field error, got: {err}"
        );
    }

    #[test]
    fn log_chunk_non_terminal_roundtrip() {
        let chunk = LogChunk {
            feature_id: "auth-20260421".to_string(),
            run_id: "r-1001".to_string(),
            layer: "backend".to_string(),
            text: "compiling foo...\n".to_string(),
            timestamp: "2026-04-21T10:01:23Z".to_string(),
            terminal: false,
            reason: None,
        };
        let wire = serde_json::to_string(&chunk).unwrap();
        // `reason` is omitted when None.
        assert!(
            !wire.contains(r#""reason":"#),
            "reason should be omitted when None; got {wire}"
        );
        let back: LogChunk = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, chunk);
    }

    #[test]
    fn log_chunk_terminal_frame_roundtrip() {
        let chunk = LogChunk {
            feature_id: "auth-20260421".to_string(),
            run_id: "r-1001".to_string(),
            layer: "backend".to_string(),
            text: String::new(),
            timestamp: "2026-04-21T10:04:00Z".to_string(),
            terminal: true,
            reason: Some("passed".to_string()),
        };
        let wire = serde_json::to_string(&chunk).unwrap();
        assert!(wire.contains(r#""terminal":true"#));
        assert!(wire.contains(r#""reason":"passed""#));
        let back: LogChunk = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, chunk);
        assert!(back.terminal);
    }

    #[test]
    fn log_chunk_terminal_defaults_false() {
        // Backwards-compat-style default: omit `terminal` → false.
        let wire = r#"{"feature_id":"f","run_id":"r","layer":"l","text":"hi","timestamp":"2026"}"#;
        let back: LogChunk = serde_json::from_str(wire).unwrap();
        assert!(!back.terminal);
        assert!(back.reason.is_none());
    }

    #[test]
    fn log_chunk_rejects_unknown_fields() {
        let bad = r#"{"feature_id":"f","run_id":"r","layer":"l","text":"x","timestamp":"t","bogusField":1}"#;
        let err = serde_json::from_str::<LogChunk>(bad).unwrap_err();
        assert!(
            err.to_string().contains("bogusField") || err.to_string().contains("unknown field"),
            "expected unknown-field error, got: {err}"
        );
    }

    #[test]
    fn stream_json_frame_snapshot_serializes_with_kind_tag() {
        use crate::protocol::subscribe::SubscribeManifestResponse;
        use std::collections::BTreeMap;
        let frame = StreamJsonFrame::Snapshot {
            snapshot: SubscribeManifestResponse {
                snapshots: vec![],
                run_ids: BTreeMap::new(),
            },
        };
        let wire = serde_json::to_string(&frame).unwrap();
        assert!(wire.starts_with(r#"{"kind":"snapshot""#), "got: {wire}");
        let back: StreamJsonFrame = serde_json::from_str(&wire).unwrap();
        assert!(matches!(back, StreamJsonFrame::Snapshot { .. }));
    }

    #[test]
    fn stream_json_frame_event_roundtrip() {
        let payload = ManifestEventPayload {
            feature_id: "f".to_string(),
            run_id: "r-1".to_string(),
            event: ManifestEvent::LayerStarted,
            layer: Some("backend".to_string()),
            data: json!({}),
            timestamp: "2026-04-21T10:00:00Z".to_string(),
        };
        let frame = StreamJsonFrame::Event { event: payload };
        let wire = serde_json::to_string(&frame).unwrap();
        assert!(wire.starts_with(r#"{"kind":"event""#), "got: {wire}");
        let back: StreamJsonFrame = serde_json::from_str(&wire).unwrap();
        assert!(matches!(back, StreamJsonFrame::Event { .. }));
    }

    #[test]
    fn stream_json_frame_log_chunk_roundtrip() {
        let chunk = LogChunk {
            feature_id: "f".to_string(),
            run_id: "r-1".to_string(),
            layer: "backend".to_string(),
            text: "x\n".to_string(),
            timestamp: "2026-04-21T10:00:00Z".to_string(),
            terminal: false,
            reason: None,
        };
        let frame = StreamJsonFrame::LogChunk { chunk };
        let wire = serde_json::to_string(&frame).unwrap();
        assert!(wire.starts_with(r#"{"kind":"log-chunk""#), "got: {wire}");
        let back: StreamJsonFrame = serde_json::from_str(&wire).unwrap();
        assert!(matches!(back, StreamJsonFrame::LogChunk { .. }));
    }

    #[test]
    fn stream_json_frame_terminal_carries_exit_code() {
        let frame = StreamJsonFrame::Terminal { exit_code: 2 };
        let wire = serde_json::to_string(&frame).unwrap();
        assert_eq!(wire, r#"{"kind":"terminal","exit_code":2}"#);
        let back: StreamJsonFrame = serde_json::from_str(&wire).unwrap();
        match back {
            StreamJsonFrame::Terminal { exit_code } => assert_eq!(exit_code, 2),
            other => panic!("expected Terminal frame, got {other:?}"),
        }
    }
}

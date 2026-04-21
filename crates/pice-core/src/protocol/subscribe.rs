//! Phase 7 typed DTOs for the `manifest/subscribe` and `logs/stream` daemon
//! RPCs. These are router-level methods (NOT `cli/dispatch` variants — see
//! `crates/pice-core/src/protocol/methods.rs`) whose response body carries
//! the initial snapshot and whose subsequent wire traffic is a stream of
//! JSON-RPC notifications on the SAME connection until the CLI closes it.
//!
//! Each request + response has `#[serde(deny_unknown_fields)]` + an inline
//! roundtrip test. Mirrors the Phase 6 review-gate DTO style.

use crate::events::LogChunk;
use crate::layers::manifest::{ManifestStatus, VerificationManifest};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ─── manifest/subscribe ─────────────────────────────────────────────────────

/// Subscribe request. `feature_id: None` subscribes to the wildcard channel
/// (events for every feature currently tracked by the daemon). `Some(id)`
/// subscribes only to that feature's events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SubscribeManifestRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature_id: Option<String>,
}

/// Initial snapshot response. `snapshots` carries every manifest that
/// matches the subscribe filter at subscribe time — a one-element vec for
/// a feature-scoped subscribe, a larger vec for a wildcard subscribe.
///
/// `run_ids` maps `feature_id` → `run_id` for every feature CURRENTLY LIVE
/// in the `FeatureJobManager` at subscribe time. Features present in
/// `snapshots` but absent from `run_ids` are persisted-but-not-running
/// (typical for terminal or `failed-interrupted` manifests). Features
/// present in `run_ids` but absent from `snapshots` are live but
/// pre-manifest-write — the caller should re-subscribe with the feature
/// filter to observe their `LayerStarted` event when the task acquires
/// its global permit and transitions `Queued → InProgress`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscribeManifestResponse {
    pub snapshots: Vec<VerificationManifest>,
    /// Live `feature_id → run_id` lookup at subscribe time. `BTreeMap`
    /// for deterministic serialization order (integration tests assert
    /// stable bytes).
    pub run_ids: BTreeMap<String, String>,
}

// ─── logs/stream ────────────────────────────────────────────────────────────

/// Logs-stream request. `follow: false` is a one-shot history snapshot
/// (equivalent to `cli/dispatch → Logs`); `follow: true` keeps the
/// connection open and streams `LogChunk` notifications until a terminal
/// frame arrives or the CLI closes. `include_history` controls whether the
/// response's `history` vec is populated (defaulting to true lets
/// late-subscribing follow clients see the run-so-far; set to false to
/// stream only future chunks).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LogsStreamRequest {
    pub feature_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    #[serde(default)]
    pub follow: bool,
    #[serde(default = "default_include_history")]
    pub include_history: bool,
}

fn default_include_history() -> bool {
    true
}

/// Logs-stream response. `history` is the snapshot up to subscribe time;
/// if the feature has already completed, the history may include a
/// `LogChunk { terminal: true }` frame — consumers MUST check for this and
/// exit immediately rather than hang waiting for a live terminal frame
/// that will never arrive (Codex Cycle 2 terminal-short-circuit fix).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogsStreamResponse {
    pub history: Vec<LogChunk>,
    pub run_id: String,
}

// ─── status list summary (used by `cli/dispatch` Status { mode: List }) ─────

/// Lightweight per-feature summary. The `status --list` CLI mode renders a
/// table of these; full manifest detail requires `pice status {feature_id}`
/// (which dispatches `Status { mode: Detail }` and gets a full
/// `VerificationManifest` back).
///
/// Distinct from `VerificationManifest` so list RPCs don't pay the full
/// manifest size for every feature — a user with 40 historical features
/// would otherwise receive megabytes on every `pice status` invocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManifestSummary {
    pub feature_id: String,
    pub overall_status: ManifestStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Total count of layers in the manifest (sum of all `LayerResult`
    /// entries regardless of status).
    pub layers_total: usize,
    /// Count of layers whose status is `Passed` / `Failed` / `Skipped` /
    /// `PendingReview` — i.e., non-InProgress, non-Pending.
    pub layers_complete: usize,
    /// Count of layers with `status == Failed`. Separate field (not just
    /// derivable from other fields) so the list renderer can highlight
    /// failure counts without round-tripping full manifest data.
    pub layers_failed: usize,
    /// Count of `gates` with `GateStatus::Pending`. The list renderer
    /// prints a "N pending gates" hint pointing the user to
    /// `pice review-gate --list`.
    pub pending_gates: usize,
}

impl ManifestSummary {
    /// Build a summary from a full manifest. Pure function — no filesystem
    /// or daemon state access. The caller (list handler) populates
    /// `run_id` separately from `FeatureJobManager::run_id_for`.
    pub fn from_manifest(manifest: &VerificationManifest, run_id: Option<String>) -> Self {
        use crate::layers::manifest::{GateStatus, LayerStatus};

        let layers_total = manifest.layers.len();
        let layers_complete = manifest
            .layers
            .iter()
            .filter(|l| {
                matches!(
                    l.status,
                    LayerStatus::Passed
                        | LayerStatus::Failed
                        | LayerStatus::Skipped
                        | LayerStatus::PendingReview
                )
            })
            .count();
        let layers_failed = manifest
            .layers
            .iter()
            .filter(|l| l.status == LayerStatus::Failed)
            .count();
        let pending_gates = manifest
            .gates
            .iter()
            .filter(|g| g.status == GateStatus::Pending)
            .count();

        Self {
            feature_id: manifest.feature_id.clone(),
            overall_status: manifest.overall_status.clone(),
            run_id,
            layers_total,
            layers_complete,
            layers_failed,
            pending_gates,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layers::manifest::VerificationManifest;
    use std::path::Path;

    #[test]
    fn subscribe_manifest_request_roundtrip_with_feature_id() {
        let req = SubscribeManifestRequest {
            feature_id: Some("auth-20260421".to_string()),
        };
        let wire = serde_json::to_string(&req).unwrap();
        assert!(wire.contains(r#""feature_id":"auth-20260421""#));
        let back: SubscribeManifestRequest = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn subscribe_manifest_request_roundtrip_wildcard() {
        let req = SubscribeManifestRequest { feature_id: None };
        let wire = serde_json::to_string(&req).unwrap();
        assert!(!wire.contains(r#""feature_id":"#));
        let back: SubscribeManifestRequest = serde_json::from_str(&wire).unwrap();
        assert!(back.feature_id.is_none());
    }

    #[test]
    fn subscribe_manifest_request_rejects_unknown_fields() {
        let bad = r#"{"feature_id":"f","bogusField":1}"#;
        let err = serde_json::from_str::<SubscribeManifestRequest>(bad).unwrap_err();
        assert!(
            err.to_string().contains("bogusField") || err.to_string().contains("unknown field"),
            "got: {err}"
        );
    }

    #[test]
    fn subscribe_manifest_response_roundtrip() {
        let manifest = VerificationManifest::new("auth-20260421", Path::new("/tmp/project"));
        let mut run_ids = BTreeMap::new();
        run_ids.insert("auth-20260421".to_string(), "r-1001".to_string());
        let resp = SubscribeManifestResponse {
            snapshots: vec![manifest.clone()],
            run_ids,
        };
        let wire = serde_json::to_string(&resp).unwrap();
        let back: SubscribeManifestResponse = serde_json::from_str(&wire).unwrap();
        assert_eq!(back.snapshots.len(), 1);
        assert_eq!(back.snapshots[0].feature_id, "auth-20260421");
        assert_eq!(
            back.run_ids.get("auth-20260421"),
            Some(&"r-1001".to_string())
        );
    }

    #[test]
    fn logs_stream_request_follow_defaults_false() {
        // include_history defaults to true; follow defaults to false.
        let wire = r#"{"feature_id":"f"}"#;
        let back: LogsStreamRequest = serde_json::from_str(wire).unwrap();
        assert_eq!(back.feature_id, "f");
        assert!(back.layer.is_none());
        assert!(!back.follow);
        assert!(back.include_history);
    }

    #[test]
    fn logs_stream_request_full_roundtrip() {
        let req = LogsStreamRequest {
            feature_id: "f".to_string(),
            layer: Some("backend".to_string()),
            follow: true,
            include_history: false,
        };
        let wire = serde_json::to_string(&req).unwrap();
        let back: LogsStreamRequest = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn logs_stream_request_rejects_unknown_fields() {
        let bad = r#"{"feature_id":"f","follow":true,"bogusField":1}"#;
        let err = serde_json::from_str::<LogsStreamRequest>(bad).unwrap_err();
        assert!(
            err.to_string().contains("bogusField") || err.to_string().contains("unknown field"),
            "got: {err}"
        );
    }

    #[test]
    fn logs_stream_response_with_terminal_frame_in_history() {
        // Late-subscribe case: feature already completed, history contains
        // the terminal frame. Consumers short-circuit on this.
        let resp = LogsStreamResponse {
            history: vec![
                LogChunk {
                    feature_id: "f".to_string(),
                    run_id: "r-1".to_string(),
                    layer: "backend".to_string(),
                    text: "compiling...\n".to_string(),
                    timestamp: "2026-04-21T10:00:00Z".to_string(),
                    terminal: false,
                    reason: None,
                },
                LogChunk {
                    feature_id: "f".to_string(),
                    run_id: "r-1".to_string(),
                    layer: "backend".to_string(),
                    text: String::new(),
                    timestamp: "2026-04-21T10:04:00Z".to_string(),
                    terminal: true,
                    reason: Some("passed".to_string()),
                },
            ],
            run_id: "r-1".to_string(),
        };
        let wire = serde_json::to_string(&resp).unwrap();
        let back: LogsStreamResponse = serde_json::from_str(&wire).unwrap();
        assert_eq!(back.history.len(), 2);
        assert!(back.history[1].terminal);
        assert_eq!(back.history[1].reason.as_deref(), Some("passed"));
    }

    #[test]
    fn manifest_summary_from_empty_manifest() {
        let manifest = VerificationManifest::new("f", Path::new("/tmp/project"));
        let summary = ManifestSummary::from_manifest(&manifest, None);
        assert_eq!(summary.feature_id, "f");
        assert_eq!(summary.layers_total, 0);
        assert_eq!(summary.layers_complete, 0);
        assert_eq!(summary.layers_failed, 0);
        assert_eq!(summary.pending_gates, 0);
        assert!(summary.run_id.is_none());
    }

    #[test]
    fn manifest_summary_counts_layer_statuses_correctly() {
        use crate::layers::manifest::{LayerResult, LayerStatus, VerificationManifest};
        let mut manifest = VerificationManifest::new("f", Path::new("/tmp/project"));
        manifest.layers = vec![
            LayerResult {
                name: "a".to_string(),
                status: LayerStatus::Passed,
                passes: vec![],
                seam_checks: vec![],
                halted_by: None,
                final_confidence: None,
                total_cost_usd: None,
                escalation_events: None,
            },
            LayerResult {
                name: "b".to_string(),
                status: LayerStatus::Failed,
                passes: vec![],
                seam_checks: vec![],
                halted_by: None,
                final_confidence: None,
                total_cost_usd: None,
                escalation_events: None,
            },
            LayerResult {
                name: "c".to_string(),
                status: LayerStatus::Pending,
                passes: vec![],
                seam_checks: vec![],
                halted_by: None,
                final_confidence: None,
                total_cost_usd: None,
                escalation_events: None,
            },
            LayerResult {
                name: "d".to_string(),
                status: LayerStatus::Skipped,
                passes: vec![],
                seam_checks: vec![],
                halted_by: None,
                final_confidence: None,
                total_cost_usd: None,
                escalation_events: None,
            },
        ];
        let summary = ManifestSummary::from_manifest(&manifest, Some("r-1".to_string()));
        assert_eq!(summary.layers_total, 4);
        // Passed + Failed + Skipped are complete; Pending is not.
        assert_eq!(summary.layers_complete, 3);
        assert_eq!(summary.layers_failed, 1);
        assert_eq!(summary.run_id.as_deref(), Some("r-1"));
    }

    #[test]
    fn manifest_summary_roundtrip() {
        let s = ManifestSummary {
            feature_id: "f".to_string(),
            overall_status: ManifestStatus::InProgress,
            run_id: Some("r-1".to_string()),
            layers_total: 7,
            layers_complete: 3,
            layers_failed: 0,
            pending_gates: 1,
        };
        let wire = serde_json::to_string(&s).unwrap();
        let back: ManifestSummary = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, s);
    }
}

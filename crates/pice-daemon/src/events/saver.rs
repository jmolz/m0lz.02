//! `ManifestSaver` trait + production `EventEmittingSaver` impl.
//!
//! Every daemon-side manifest state transition must go through
//! `ManifestSaver::save_and_emit(&manifest, path, intent)`. The saver
//! couples two operations that otherwise drift apart:
//!
//! 1. Persist the manifest to disk (crash-safe via
//!    `VerificationManifest::save`).
//! 2. Publish the matching `ManifestEventPayload` to the event bus.
//!
//! The orchestrator call site KNOWS its intent (it just transitioned a
//! layer `Pending → InProgress`, or appended a gate, etc). Rather than
//! inferring the event kind from manifest diffing — which would be
//! slow, lossy, and brittle — the saver asks the caller to supply an
//! explicit [`SaveIntent`]. The saver matches on the intent, builds
//! the right [`ManifestEventPayload`], writes the manifest, then
//! publishes.
//!
//! **Trait design rationale.** We ship a trait (not a concrete impl)
//! so tests can substitute `NullSaver` / `RecordingSaver` without a
//! live `EventBus`. The `.claude/rules/rust-core.md` "Don't ship
//! trait-based scaffolding ahead of a real consumer" rule is honored
//! because `EventEmittingSaver` is wired into the orchestrator in
//! Task 9 — the trait is landing with an immediate production
//! consumer, not speculative scaffolding.

use super::bus::EventBus;
use pice_core::layers::manifest::VerificationManifest;
use std::path::Path;

/// What state transition the caller just completed.
///
/// The saver uses this to build a typed `ManifestEventPayload` — it
/// never inspects the manifest contents to infer the event kind. See
/// Task 4 in `.claude/plans/phase-7-background-execution.md` for the
/// full call-site → intent mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveIntent {
    /// A layer transitioned `Pending → InProgress` (start of cohort
    /// processing). Emits `ManifestEvent::LayerStarted`.
    LayerStarted { layer: String },

    /// The adaptive loop halted for a layer and the `LayerResult`
    /// landed in the manifest. Emits `ManifestEvent::LayerComplete`.
    LayerCompleted { layer: String },

    /// A single adaptive pass finished and its `PassResult` was
    /// appended to the layer. Emits `ManifestEvent::PassComplete`.
    PassRecorded { layer: String, pass_index: usize },

    /// A new review gate was appended to the manifest. Emits
    /// `ManifestEvent::GateRequested` with `{gate_id,
    /// trigger_expression}` in the data payload.
    GateAppended {
        gate_id: String,
        layer: String,
        trigger_expression: String,
    },

    /// A gate decision was recorded (approve / reject / skip / timeout
    /// variants). Emits `ManifestEvent::GateDecided` with `{gate_id,
    /// decision}`.
    GateDecided {
        gate_id: String,
        layer: String,
        decision: String,
    },

    /// A seam check recorded a finding. Emits
    /// `ManifestEvent::SeamFinding` with `{boundary, check_id,
    /// severity}`. Attributed to the `layer` whose seam check was run
    /// (bilateral-active boundaries run the seam once per side — the
    /// saver emits per-side events; the audit-trail layer dedupes).
    SeamRecorded {
        boundary: String,
        check_id: String,
        severity: String,
        layer: String,
    },

    /// The manifest reached a terminal `overall_status`. Emits
    /// `ManifestEvent::FeatureComplete` with `{status}`.
    FeatureCompleted,

    /// The feature was cancelled (SIGINT / daemon drain / panicked
    /// orchestrator future). Emits `ManifestEvent::Cancelled` with
    /// `{reason}`.
    Cancelled { reason: String },
}

/// Abstraction for the `save + emit` pair. Every daemon-side manifest
/// write goes through this trait — the orchestrator never calls
/// `VerificationManifest::save` directly (pinned by Task 9's grep
/// coverage test).
pub trait ManifestSaver: Send + Sync {
    /// Persist `manifest` to `path` (crash-safe) and publish the
    /// matching event to the bus. Errors from the save step propagate;
    /// event publication never fails (bus has no fallible send
    /// semantics — lagged subscribers handle lag receiver-side).
    fn save_and_emit(
        &self,
        manifest: &VerificationManifest,
        path: &Path,
        intent: SaveIntent,
    ) -> anyhow::Result<()>;
}

/// Production saver: writes the manifest via
/// `VerificationManifest::save` then publishes the typed event via
/// [`EventBus`]. Holds a borrow of the bus — callers own the bus (via
/// `DaemonContext`) and hand out `&dyn ManifestSaver` references.
pub struct EventEmittingSaver<'a> {
    bus: &'a EventBus,
}

impl<'a> EventEmittingSaver<'a> {
    /// Wrap a bus reference in the saver.
    pub fn new(bus: &'a EventBus) -> Self {
        Self { bus }
    }

    /// Build the event payload from the intent + manifest metadata
    /// (feature_id, run_id) and publish. Called AFTER the manifest
    /// save succeeded — a failed save must NOT emit a spurious event
    /// (the receiver's next manifest snapshot would disagree with the
    /// event stream).
    fn emit_for_intent(&self, manifest: &VerificationManifest, intent: SaveIntent) {
        let feature_id = manifest.feature_id.as_str();
        // `run_id` is optional on the manifest (Queued manifests
        // preceding the spawn haven't allocated one yet). We fall back
        // to the empty string so the wire type contract holds;
        // subscribers that care about run_id already skip empty-string
        // frames.
        let run_id = manifest.run_id.as_deref().unwrap_or("");

        match intent {
            SaveIntent::LayerStarted { layer } => {
                self.bus.emit_layer_started(feature_id, run_id, &layer);
            }
            SaveIntent::LayerCompleted { layer } => {
                // We include the layer's status + halted_by in the
                // event data so subscribers can render without
                // re-reading the manifest.
                let data = manifest
                    .layers
                    .iter()
                    .find(|l| l.name == layer)
                    .map(|l| {
                        serde_json::json!({
                            "status": l.status,
                            "halted_by": l.halted_by,
                        })
                    })
                    .unwrap_or(serde_json::Value::Null);
                self.bus
                    .emit_layer_complete(feature_id, run_id, &layer, data);
            }
            SaveIntent::PassRecorded { layer, pass_index } => {
                let data = serde_json::json!({ "pass_index": pass_index });
                self.bus
                    .emit_pass_complete(feature_id, run_id, &layer, data);
            }
            SaveIntent::GateAppended {
                gate_id,
                layer,
                trigger_expression,
            } => {
                let data = serde_json::json!({
                    "gate_id": gate_id,
                    "trigger_expression": trigger_expression,
                });
                self.bus
                    .emit_gate_requested(feature_id, run_id, &layer, data);
            }
            SaveIntent::GateDecided {
                gate_id,
                layer,
                decision,
            } => {
                let data = serde_json::json!({
                    "gate_id": gate_id,
                    "decision": decision,
                });
                self.bus.emit_gate_decided(feature_id, run_id, &layer, data);
            }
            SaveIntent::SeamRecorded {
                boundary,
                check_id,
                severity,
                layer,
            } => {
                let data = serde_json::json!({
                    "boundary": boundary,
                    "check_id": check_id,
                    "severity": severity,
                });
                self.bus.emit_seam_finding(feature_id, run_id, &layer, data);
            }
            SaveIntent::FeatureCompleted => {
                let data = serde_json::json!({ "status": manifest.overall_status });
                self.bus.emit_feature_complete(feature_id, run_id, data);
            }
            SaveIntent::Cancelled { reason } => {
                self.bus.emit_cancelled(feature_id, run_id, &reason);
            }
        }
    }
}

impl<'a> ManifestSaver for EventEmittingSaver<'a> {
    fn save_and_emit(
        &self,
        manifest: &VerificationManifest,
        path: &Path,
        intent: SaveIntent,
    ) -> anyhow::Result<()> {
        // Save first — a failed save must NOT emit an event. The
        // inverse ordering would leave subscribers chasing a state the
        // disk never reflected.
        manifest.save(path)?;
        self.emit_for_intent(manifest, intent);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pice_core::events::ManifestEvent;
    use pice_core::layers::manifest::{ManifestStatus, VerificationManifest};
    use tempfile::tempdir;

    fn sample_manifest(feature_id: &str, run_id: Option<&str>) -> VerificationManifest {
        VerificationManifest {
            schema_version: "0.2".to_string(),
            feature_id: feature_id.to_string(),
            project_root_hash: "test-hash".to_string(),
            layers: Vec::new(),
            gates: Vec::new(),
            overall_status: ManifestStatus::InProgress,
            run_id: run_id.map(|s| s.to_string()),
        }
    }

    #[tokio::test]
    async fn trait_save_emits_event() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe_feature("feat-saver");
        let saver = EventEmittingSaver::new(&bus);

        let dir = tempdir().unwrap();
        let path = dir.path().join("manifest.json");
        let manifest = sample_manifest("feat-saver", Some("run-1"));

        saver
            .save_and_emit(
                &manifest,
                &path,
                SaveIntent::LayerStarted {
                    layer: "backend".to_string(),
                },
            )
            .expect("save_and_emit should succeed");

        // File written?
        assert!(path.exists(), "manifest should be on disk after save");

        // Event published?
        let evt = rx.recv().await.unwrap();
        assert_eq!(evt.event, ManifestEvent::LayerStarted);
        assert_eq!(evt.feature_id, "feat-saver");
        assert_eq!(evt.run_id, "run-1");
        assert_eq!(evt.layer.as_deref(), Some("backend"));
    }

    #[tokio::test]
    async fn save_failure_does_not_emit_event() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe_feature("feat-saver");
        let saver = EventEmittingSaver::new(&bus);

        // Point the save at a path whose parent cannot be created
        // (a file-in-place-of-a-directory error). The save step must
        // fail, and crucially the bus must NOT see an event — stale
        // events with no corresponding manifest-on-disk would mislead
        // subscribers.
        let dir = tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"not a dir").unwrap();
        let path = blocker.join("manifest.json");

        let manifest = sample_manifest("feat-saver", Some("run-1"));
        let result = saver.save_and_emit(
            &manifest,
            &path,
            SaveIntent::LayerStarted {
                layer: "backend".to_string(),
            },
        );
        assert!(result.is_err(), "save must fail when parent is a file");

        // No event on the channel.
        match rx.try_recv() {
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {}
            other => panic!("expected no event after failed save, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn intent_maps_to_matching_event_kind() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe_feature("feat-all-intents");
        let saver = EventEmittingSaver::new(&bus);

        let dir = tempdir().unwrap();
        let manifest = sample_manifest("feat-all-intents", Some("run-x"));

        // One save per intent, each at a distinct path so the saves
        // don't fight each other on the tmp fs.
        let cases: Vec<(SaveIntent, ManifestEvent)> = vec![
            (
                SaveIntent::LayerStarted { layer: "a".into() },
                ManifestEvent::LayerStarted,
            ),
            (
                SaveIntent::LayerCompleted { layer: "a".into() },
                ManifestEvent::LayerComplete,
            ),
            (
                SaveIntent::PassRecorded {
                    layer: "a".into(),
                    pass_index: 1,
                },
                ManifestEvent::PassComplete,
            ),
            (
                SaveIntent::GateAppended {
                    gate_id: "g1".into(),
                    layer: "a".into(),
                    trigger_expression: "tier >= 3".into(),
                },
                ManifestEvent::GateRequested,
            ),
            (
                SaveIntent::GateDecided {
                    gate_id: "g1".into(),
                    layer: "a".into(),
                    decision: "approve".into(),
                },
                ManifestEvent::GateDecided,
            ),
            (
                SaveIntent::SeamRecorded {
                    boundary: "api↔db".into(),
                    check_id: "schema_drift".into(),
                    severity: "pass".into(),
                    layer: "a".into(),
                },
                ManifestEvent::SeamFinding,
            ),
            (SaveIntent::FeatureCompleted, ManifestEvent::FeatureComplete),
            (
                SaveIntent::Cancelled {
                    reason: "sigterm".into(),
                },
                ManifestEvent::Cancelled,
            ),
        ];

        for (i, (intent, expected_kind)) in cases.into_iter().enumerate() {
            let path = dir.path().join(format!("manifest-{i}.json"));
            saver.save_and_emit(&manifest, &path, intent).unwrap();
            let evt = rx.recv().await.unwrap();
            assert_eq!(
                evt.event, expected_kind,
                "intent at index {i} produced wrong event kind"
            );
        }
    }
}

//! Phase 7 Criterion 4: manifest notifications are emitted only after the
//! matching manifest write has succeeded.
//!
//! This test uses the production `EventEmittingSaver` plus its diagnostic
//! hooks. The hooks observe the precise interval before save and after save
//! but before publish, proving subscribers cannot receive an event whose
//! manifest state is not yet durable.

use pice_core::events::ManifestEvent;
use pice_core::layers::manifest::{LayerResult, LayerStatus, ManifestStatus, VerificationManifest};
use pice_daemon::events::{
    EventBus, EventEmittingSaver, EventEmittingSaverHooks, ManifestSaver, SaveIntent,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast::error::TryRecvError;

fn sample_manifest(feature_id: &str) -> VerificationManifest {
    VerificationManifest {
        schema_version: "0.2".to_string(),
        feature_id: feature_id.to_string(),
        project_root_hash: "notification-coverage-hash".to_string(),
        layers: Vec::new(),
        gates: Vec::new(),
        overall_status: ManifestStatus::InProgress,
        run_id: Some("run-notify-coverage".to_string()),
    }
}

#[test]
fn event_emitting_saver_publishes_only_after_successful_save() {
    let bus = EventBus::new();
    let rx = Arc::new(Mutex::new(bus.subscribe_feature("feat-notify-order")));
    let dir = tempfile::tempdir().unwrap();
    let current_expected = Arc::new(Mutex::new(None::<(PathBuf, String)>));
    let after_save_count = Arc::new(AtomicUsize::new(0));

    let before_rx = Arc::clone(&rx);
    let before_save = move || {
        let mut rx = before_rx.lock().unwrap();
        assert!(
            matches!(rx.try_recv(), Err(TryRecvError::Empty)),
            "event bus must be silent before the manifest save starts"
        );
    };

    let after_rx = Arc::clone(&rx);
    let after_expected = Arc::clone(&current_expected);
    let after_count = Arc::clone(&after_save_count);
    let after_save_before_emit = move || {
        let (path, expected_halted_by) = after_expected
            .lock()
            .unwrap()
            .clone()
            .expect("expected persisted manifest metadata");
        let persisted = VerificationManifest::load(&path).expect("manifest persisted before emit");
        assert_eq!(
            persisted.layers[0].halted_by.as_deref(),
            Some(expected_halted_by.as_str()),
            "post-save hook must observe the durable state before any event is published"
        );
        let mut rx = after_rx.lock().unwrap();
        assert!(
            matches!(rx.try_recv(), Err(TryRecvError::Empty)),
            "event bus must still be silent after save and before emit"
        );
        after_count.fetch_add(1, Ordering::SeqCst);
    };

    let hooks = EventEmittingSaverHooks {
        before_save: Some(&before_save),
        after_save_before_emit: Some(&after_save_before_emit),
    };
    let saver = EventEmittingSaver::new_with_hooks(&bus, hooks);
    let mut manifest = sample_manifest("feat-notify-order");

    let mut previous_timestamp = None;
    for i in 0..100 {
        let layer = format!("layer-{i:03}");
        let halted_by = format!("persisted-{i:03}");
        manifest.layers.clear();
        manifest.layers.push(LayerResult {
            name: layer.clone(),
            status: LayerStatus::InProgress,
            passes: Vec::new(),
            seam_checks: Vec::new(),
            halted_by: Some(halted_by.clone()),
            final_confidence: None,
            total_cost_usd: None,
            escalation_events: None,
        });
        let path = dir.path().join(format!("manifest-{i:03}.json"));
        *current_expected.lock().unwrap() = Some((path.clone(), halted_by.clone()));

        saver
            .save_and_emit(
                &manifest,
                &path,
                SaveIntent::LayerCompleted {
                    layer: layer.clone(),
                },
            )
            .expect("save_and_emit should persist then publish");
        assert_eq!(
            after_save_count.load(Ordering::SeqCst),
            i + 1,
            "after-save hook must run once per successful save"
        );

        let event = rx
            .lock()
            .unwrap()
            .try_recv()
            .expect("event should be available after save_and_emit returns");
        assert_eq!(event.event, ManifestEvent::LayerComplete);
        assert_eq!(event.layer.as_deref(), Some(layer.as_str()));
        assert_eq!(event.data["halted_by"], halted_by);

        let timestamp = chrono::DateTime::parse_from_rfc3339(&event.timestamp)
            .expect("event timestamp should be RFC3339");
        if let Some(previous) = previous_timestamp {
            assert!(
                timestamp >= previous,
                "event timestamps must be monotonic across rapid saves"
            );
        }
        previous_timestamp = Some(timestamp);
    }
}

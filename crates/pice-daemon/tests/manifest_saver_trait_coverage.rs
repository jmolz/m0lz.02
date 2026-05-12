//! Phase 7 Task 4 + Task 9 — coverage test: every daemon-side manifest
//! state transition goes through [`ManifestSaver::save_and_emit`].
//!
//! The test is split in two:
//!
//! 1. A trait-shape smoke test (`event_emitting_saver_satisfies_trait`)
//!    that locks the public API shape. Ships with Task 4.
//! 2. A grep-based coverage assertion
//!    (`zero_raw_manifest_save_calls_in_orchestrator`) that scans the
//!    daemon source for raw `manifest.save(path)` calls outside of
//!    `events/saver.rs` (where the one sanctioned call lives). This
//!    is the Task 9 landing point — Task 9 replaces every current
//!    orchestrator/handlers call with `saver.save_and_emit(...,
//!    intent)`. It is currently `#[ignore]` so CI stays green until
//!    Task 9 completes the wiring; un-ignoring it is the last Task 9
//!    step (coupled with the orchestrator refactor so enabling the
//!    test and removing the raw calls happen in one commit).
//!
//! See `.claude/plans/phase-7-background-execution.md` → Task 4 +
//! Task 9 for the full rationale.

use pice_core::events::ManifestEvent;
use pice_core::layers::manifest::{ManifestStatus, VerificationManifest};
use pice_daemon::events::{EventBus, EventEmittingSaver, ManifestSaver, SaveIntent};
use tempfile::tempdir;

fn sample_manifest(feature_id: &str) -> VerificationManifest {
    VerificationManifest {
        schema_version: "0.2".to_string(),
        feature_id: feature_id.to_string(),
        project_root_hash: "coverage-test-hash".to_string(),
        layers: Vec::new(),
        gates: Vec::new(),
        overall_status: ManifestStatus::InProgress,
        run_id: Some("run-coverage".to_string()),
    }
}

/// Task 4: `EventEmittingSaver` implements `ManifestSaver` and can be
/// used behind a `&dyn ManifestSaver` reference (Task 9 threads it
/// through the orchestrator as a trait object).
#[tokio::test]
async fn event_emitting_saver_satisfies_trait() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe_feature("feat-trait");
    let saver = EventEmittingSaver::new(&bus);
    let saver_ref: &dyn ManifestSaver = &saver;

    let dir = tempdir().unwrap();
    let path = dir.path().join("feat-trait.manifest.json");
    let manifest = sample_manifest("feat-trait");

    saver_ref
        .save_and_emit(
            &manifest,
            &path,
            SaveIntent::LayerStarted {
                layer: "backend".to_string(),
            },
        )
        .expect("trait object save must succeed");

    // The manifest is on disk AND the bus saw the event.
    assert!(path.exists(), "manifest persisted via trait object");
    let evt = rx.recv().await.unwrap();
    assert_eq!(evt.event, ManifestEvent::LayerStarted);
}

/// Task 9 landing point: the orchestrator + handlers may call the
/// low-level `VerificationManifest::save` only from inside the one
/// sanctioned file (`crates/pice-daemon/src/events/saver.rs`). Every
/// other call site routes through `ManifestSaver::save_and_emit`.
///
/// Implementation: scan the daemon transition-write surface for raw
/// `.save(` calls and fail if any land outside the allow-list. The
/// matcher intentionally catches `rewritten.save(` and similar receiver
/// names, not just the literal `manifest.save(`.
#[test]
fn zero_raw_manifest_save_calls_in_orchestrator() {
    use std::fs;
    use std::path::Path;

    // Allow-list: the single file that is permitted to call
    // `manifest.save(path)` directly.
    let allowed = [Path::new("src/events/saver.rs")];

    let daemon_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let scopes = [
        daemon_src.join("orchestrator"),
        daemon_src.join("handlers/evaluate.rs"),
        daemon_src.join("handlers/review_gate.rs"),
        daemon_src.join("jobs/recovery.rs"),
    ];
    let mut offenders = Vec::<String>::new();

    for scope in scopes {
        walk(&scope, &mut |rel, content| {
            if allowed
                .iter()
                .any(|p| rel.ends_with(p.file_name().unwrap_or_default()))
            {
                return;
            }
            let mut in_test_module = false;
            for (i, line) in content.lines().enumerate() {
                if line.trim() == "#[cfg(test)]" {
                    in_test_module = true;
                }
                if in_test_module {
                    continue;
                }
                if line.contains(".save(") {
                    offenders.push(format!("{}:{}: {}", rel.display(), i + 1, line.trim()));
                }
            }
        });
    }

    assert!(
        offenders.is_empty(),
        "Task 9 invariant violated — raw manifest.save calls found outside the saver:\n{}",
        offenders.join("\n")
    );

    fn walk(dir: &Path, f: &mut dyn FnMut(&Path, &str)) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(&p, f);
            } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
                if let Ok(s) = fs::read_to_string(&p) {
                    f(&p, &s);
                }
            }
        }
    }
}

#[test]
fn zero_direct_cancelled_event_emits_outside_saver() {
    use std::fs;
    use std::path::Path;

    let daemon_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let allowed = [
        daemon_src.join("events/saver.rs"),
        daemon_src.join("events/bus.rs"),
    ];
    let mut offenders = Vec::<String>::new();

    fn walk(dir: &Path, f: &mut dyn FnMut(&Path, &str)) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(&p, f);
            } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
                if let Ok(s) = fs::read_to_string(&p) {
                    f(&p, &s);
                }
            }
        }
    }

    walk(&daemon_src, &mut |path, content| {
        if allowed.iter().any(|allowed_path| path == allowed_path) {
            return;
        }
        let mut in_test_module = false;
        for (i, line) in content.lines().enumerate() {
            if line.trim() == "#[cfg(test)]" {
                in_test_module = true;
            }
            if in_test_module {
                continue;
            }
            if line.contains(".emit_cancelled(") {
                offenders.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
            }
        }
    });

    assert!(
        offenders.is_empty(),
        "terminal Cancelled manifest/events must go through ManifestSaver::save_and_emit:\n{}",
        offenders.join("\n")
    );
}

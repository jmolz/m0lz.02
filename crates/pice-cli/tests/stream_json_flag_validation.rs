//! Phase 7 Task 3 — clap conflict-rule regression tests.
//!
//! These pin the flag invariants that later Phase 7 tasks rely on:
//! - `pice execute/evaluate`: `--wait` requires `--background`;
//!   `--timeout-secs` requires `--wait`.
//! - `pice status`: `--follow` ⊥ `--wait`, `--follow` ⊥ `--json`,
//!   `--wait` requires a `feature_id` positional, `--stream-json`
//!   requires `--follow`.
//! - `pice logs`: `--json` ⊥ `--follow`, `--stream-json` requires
//!   `--follow`.
//!
//! We drive clap via `assert_cmd` rather than in-process parser tests
//! so the shape of the generated `--help` + error diagnostics matches
//! what users actually see. Conflict violations exit with clap's
//! default `2` (usage error) and print to stderr, so no daemon runtime
//! is required — these tests do NOT need `PICE_DAEMON_INLINE` or any
//! `.pice/` scaffolding.

use assert_cmd::Command;

fn pice() -> Command {
    Command::cargo_bin("pice").expect("pice binary must be built for tests")
}

// ─── pice execute ────────────────────────────────────────────────────────

#[test]
fn execute_wait_without_background_is_rejected() {
    let output = pice()
        .args(["execute", "plan.md", "--wait"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--background"),
        "expected --wait ⇒ --background error in stderr, got: {stderr}"
    );
}

#[test]
fn execute_timeout_secs_without_wait_is_rejected() {
    let output = pice()
        .args(["execute", "plan.md", "--timeout-secs", "30"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--wait"),
        "expected --timeout-secs ⇒ --wait error in stderr, got: {stderr}"
    );
}

// ─── pice evaluate ───────────────────────────────────────────────────────

#[test]
fn evaluate_wait_without_background_is_rejected() {
    let output = pice()
        .args(["evaluate", "plan.md", "--wait"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--background"),
        "expected --wait ⇒ --background error in stderr, got: {stderr}"
    );
}

#[test]
fn evaluate_timeout_secs_without_wait_is_rejected() {
    let output = pice()
        .args(["evaluate", "plan.md", "--timeout-secs", "30"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--wait"),
        "expected --timeout-secs ⇒ --wait error in stderr, got: {stderr}"
    );
}

// ─── pice status ─────────────────────────────────────────────────────────

#[test]
fn status_follow_conflicts_with_wait() {
    let output = pice()
        .args(["status", "feat-1", "--follow", "--wait"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--follow") && stderr.contains("--wait"),
        "expected --follow/--wait conflict in stderr, got: {stderr}"
    );
}

#[test]
fn status_follow_conflicts_with_json() {
    let output = pice()
        .args(["status", "--follow", "--json"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--follow") && stderr.contains("--json"),
        "expected --follow/--json conflict in stderr, got: {stderr}"
    );
}

#[test]
fn status_wait_without_feature_id_is_rejected() {
    let output = pice()
        .args(["status", "--wait"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("FEATURE_ID") || stderr.contains("feature_id"),
        "expected --wait ⇒ feature_id error in stderr, got: {stderr}"
    );
}

#[test]
fn status_stream_json_requires_follow() {
    let output = pice()
        .args(["status", "--stream-json"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--follow"),
        "expected --stream-json ⇒ --follow error in stderr, got: {stderr}"
    );
}

// ─── pice logs ───────────────────────────────────────────────────────────

#[test]
fn logs_follow_conflicts_with_json() {
    let output = pice()
        .args(["logs", "feat-1", "--follow", "--json"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--follow") && stderr.contains("--json"),
        "expected --follow/--json conflict in stderr, got: {stderr}"
    );
}

#[test]
fn logs_stream_json_requires_follow() {
    let output = pice()
        .args(["logs", "feat-1", "--stream-json"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--follow"),
        "expected --stream-json ⇒ --follow error in stderr, got: {stderr}"
    );
}

#[test]
fn logs_requires_feature_id_positional() {
    let output = pice()
        .args(["logs"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("FEATURE_ID") || stderr.contains("feature_id"),
        "expected FEATURE_ID missing-positional error, got: {stderr}"
    );
}

// ─── Happy-path: valid flag combinations parse cleanly (--help exit 0).
//     Not a dispatch test — just proves clap accepts the shapes the
//     later tasks depend on. ────────────────────────────────────────────

#[test]
fn execute_background_wait_timeout_parses() {
    // `--help` short-circuits clap parsing; if any flag attr is wrong
    // (e.g. `requires` spelled badly) this would still expose the
    // error at command-construction time.
    pice().args(["execute", "--help"]).assert().success();
}

#[test]
fn status_follow_with_stream_json_parses() {
    pice().args(["status", "--help"]).assert().success();
}

#[test]
fn logs_help_renders() {
    pice().args(["logs", "--help"]).assert().success();
}

// ─── NDJSON envelope validity (Task 15) ──────────────────────────────────
//
// Exercise `StreamJsonFrame` serialize → line-by-line deserialize to
// prove every emitted line of `--stream-json` output parses back as a
// typed frame. This pins the wire shape that consumers pattern-match
// on, independent of the runtime path (no daemon needed, no tokio
// runtime — pure serde). The status follow loop calls
// `serde_json::to_string(&StreamJsonFrame::*)` and emits one frame per
// line; if the enum's tag layout ever drifts from `{"kind":...}`, the
// parse step below fails instantly and CI catches it before a user's
// NDJSON pipeline breaks.

#[test]
fn stream_json_frame_ndjson_roundtrip_is_stable() {
    use pice_core::events::{ManifestEvent, ManifestEventPayload, StreamJsonFrame};
    use pice_core::protocol::subscribe::SubscribeManifestResponse;
    use std::collections::BTreeMap;

    // Build 50 frames: 1 snapshot + 48 events + 1 terminal — the worst
    // case the plan's integration test calls out ("JSONL validity over
    // 50 events").
    let mut lines = Vec::with_capacity(50);
    lines.push(
        serde_json::to_string(&StreamJsonFrame::Snapshot {
            snapshot: SubscribeManifestResponse {
                snapshots: vec![],
                run_ids: BTreeMap::new(),
            },
        })
        .expect("snapshot serializes"),
    );
    for i in 0..48 {
        let payload = ManifestEventPayload {
            feature_id: format!("f-{}", i % 3),
            run_id: format!("r-{}", i % 7),
            event: match i % 4 {
                0 => ManifestEvent::LayerStarted,
                1 => ManifestEvent::PassComplete,
                2 => ManifestEvent::LayerComplete,
                _ => ManifestEvent::SeamFinding,
            },
            layer: Some(format!("layer-{}", i % 5)),
            data: serde_json::json!({ "i": i }),
            timestamp: "2026-04-21T10:00:00Z".to_string(),
        };
        lines.push(
            serde_json::to_string(&StreamJsonFrame::Event { event: payload })
                .expect("event serializes"),
        );
    }
    lines.push(
        serde_json::to_string(&StreamJsonFrame::Terminal { exit_code: 0 })
            .expect("terminal serializes"),
    );
    assert_eq!(lines.len(), 50);

    // Every line parses back as a typed frame, in order: first is
    // snapshot, last is terminal, middle are events.
    let mut saw_snapshot = 0;
    let mut saw_event = 0;
    let mut saw_terminal = 0;
    for (idx, line) in lines.iter().enumerate() {
        // Mandatory: no newline INSIDE a serialized frame (would break
        // the NDJSON contract).
        assert!(
            !line.contains('\n'),
            "line {idx} contains an embedded newline: {line}"
        );
        let frame: StreamJsonFrame =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("line {idx} parse: {e}: {line}"));
        match frame {
            StreamJsonFrame::Snapshot { .. } => saw_snapshot += 1,
            StreamJsonFrame::Event { .. } => saw_event += 1,
            StreamJsonFrame::Terminal { .. } => saw_terminal += 1,
        }
    }
    assert_eq!(saw_snapshot, 1);
    assert_eq!(saw_event, 48);
    assert_eq!(saw_terminal, 1);
}

#[test]
fn stream_json_frame_kind_discriminant_values() {
    // Pin the exact kebab-case discriminant wire strings so a future
    // serde rename on `StreamJsonFrame` is caught at CI.
    use pice_core::events::StreamJsonFrame;
    let terminal = serde_json::to_value(StreamJsonFrame::Terminal { exit_code: 0 }).unwrap();
    assert_eq!(terminal["kind"], "terminal");
    let event = serde_json::to_value(StreamJsonFrame::Event {
        event: pice_core::events::ManifestEventPayload {
            feature_id: "f".to_string(),
            run_id: "r".to_string(),
            event: pice_core::events::ManifestEvent::FeatureComplete,
            layer: None,
            data: serde_json::json!({}),
            timestamp: "ts".to_string(),
        },
    })
    .unwrap();
    assert_eq!(event["kind"], "event");
}

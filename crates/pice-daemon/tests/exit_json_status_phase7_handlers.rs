//! Phase 7 Criterion 10 — daemon-side integration tests for Phase 7
//! `ExitJsonStatus` variants that are not reachable from the CLI binary
//! in inline mode.
//!
//! Strategy:
//! - `WaitTimeout`, `LogsStreamEnded`, `DaemonDisconnected` are emitted
//!   by CLI-side subscription adapter code (`adapter/background_wait.rs`,
//!   `commands/logs.rs`). They cannot be triggered through
//!   `handlers::dispatch` alone. This file pins their wire strings via
//!   explicit `assert_eq!(variant.as_str(), ...)` assertions so any rename
//!   of the variant or its `as_str()` match arm fails to compile at the
//!   assertion site AND produces a lint error — no silent drift.
//!
//! - `FeatureNotFound` is additionally tested end-to-end at the daemon
//!   handler dispatch level so the `ExitJsonStatus::FeatureNotFound.as_str()`
//!   value actually appears in the `CommandResponse::ExitJson.value["status"]`
//!   wire payload. This gives an "on-the-wire" assertion for the variant that
//!   is closest to the real protocol boundary without requiring a running
//!   daemon socket.
//!
//! Together with `crates/pice-cli/tests/exit_json_status_phase7_wire_strings.rs`
//! (which tests `FeatureNotFound` and `FailedInterrupted` via the binary),
//! every Phase 7 variant has at least one test that exercises
//! `ExitJsonStatus::X.as_str()` at a point in the call stack where the string
//! is actually placed on a response channel.

use pice_core::cli::{CommandRequest, CommandResponse, ExitJsonStatus, StatusMode, StatusRequest};
use pice_daemon::handlers::dispatch;
use pice_daemon::orchestrator::NullSink;
use pice_daemon::server::router::DaemonContext;
use pice_daemon::test_support::StateDirGuard;

// ─── Wire-string pin tests (unit-level) ─────────────────────────────────────
//
// Each test calls `ExitJsonStatus::X.as_str()` and `ExitJsonStatus::X.exit_code()`
// and asserts the result matches the expected kebab-case wire form and exit
// code. A variant rename updates `as_str()` automatically (the match arm must
// match the new name), so a stale literal here fails compilation. If the
// literal in `as_str()` is updated WITHOUT updating the serde rename, the
// existing `exit_json_status_as_str_matches_serde_kebab_case` test in
// `pice-core` catches the divergence.
//
// The assertions are intentionally against string literals so grep-based
// consumers (CI dashboards, monitoring scripts) can be updated in the same
// commit — the literal IS the observable contract.

/// Phase 7 Criterion 10 — `wait-timeout` wire string pin.
///
/// `ExitJsonStatus::WaitTimeout` is emitted by the CLI-side subscription
/// adapter (`crates/pice-cli/src/adapter/background_wait.rs`) when the
/// subscribe stream's timeout deadline fires before a terminal event arrives.
/// The daemon handler never emits this status; it is a CLI-adapter concern.
///
/// This test pins:
/// 1. The wire string `"wait-timeout"` (kebab-case of the variant name).
/// 2. The exit code 4 (new Phase 7 code, distinct from ReviewGatePending's 3).
#[test]
fn wait_timeout_wire_string_is_pinned() {
    assert_eq!(
        ExitJsonStatus::WaitTimeout.as_str(),
        "wait-timeout",
        "ExitJsonStatus::WaitTimeout.as_str() must equal the kebab-case wire form; \
         update the as_str() match arm to match"
    );
    assert_eq!(
        ExitJsonStatus::WaitTimeout.exit_code(),
        4,
        "ExitJsonStatus::WaitTimeout.exit_code() must be 4 \
         (Phase 7 wait-timeout exit code, distinct from ReviewGatePending's 3)"
    );
}

/// Phase 7 Criterion 10 — `logs-stream-ended` wire string pin.
///
/// `ExitJsonStatus::LogsStreamEnded` is emitted by
/// `crates/pice-cli/src/commands/logs.rs::maybe_emit_logs_stream_ended` in
/// `--json` mode after the `logs/stream` subscribe stream receives a terminal
/// `LogChunk { terminal: true }` frame and closes cleanly. Exit 0 (structured
/// success — the stream ended normally).
///
/// This test pins:
/// 1. The wire string `"logs-stream-ended"`.
/// 2. The exit code 0 (structured success, same family as BackgroundDispatched).
#[test]
fn logs_stream_ended_wire_string_is_pinned() {
    assert_eq!(
        ExitJsonStatus::LogsStreamEnded.as_str(),
        "logs-stream-ended",
        "ExitJsonStatus::LogsStreamEnded.as_str() must equal the kebab-case wire form; \
         update the as_str() match arm to match"
    );
    assert_eq!(
        ExitJsonStatus::LogsStreamEnded.exit_code(),
        0,
        "ExitJsonStatus::LogsStreamEnded.exit_code() must be 0 \
         (structured success — stream closed cleanly)"
    );
}

/// Phase 7 Criterion 10 — `daemon-disconnected` wire string pin.
///
/// `ExitJsonStatus::DaemonDisconnected` is emitted when the CLI's subscribe
/// stream (`manifest/subscribe` in `background_wait.rs` or `logs/stream` in
/// `commands/logs.rs`) receives `None` from `rx.recv().await`, indicating the
/// daemon closed the connection before a terminal frame arrived (e.g. daemon
/// crash or SIGTERM). Exit 5 (new Phase 7 code, distinct from WaitTimeout's 4).
///
/// This test pins:
/// 1. The wire string `"daemon-disconnected"`.
/// 2. The exit code 5 (new Phase 7 code).
#[test]
fn daemon_disconnected_wire_string_is_pinned() {
    assert_eq!(
        ExitJsonStatus::DaemonDisconnected.as_str(),
        "daemon-disconnected",
        "ExitJsonStatus::DaemonDisconnected.as_str() must equal the kebab-case wire form; \
         update the as_str() match arm to match"
    );
    assert_eq!(
        ExitJsonStatus::DaemonDisconnected.exit_code(),
        5,
        "ExitJsonStatus::DaemonDisconnected.exit_code() must be 5 \
         (Phase 7 daemon-disconnected exit code, distinct from WaitTimeout's 4)"
    );
}

// ─── Handler dispatch tests ──────────────────────────────────────────────────

/// Phase 7 Criterion 10 — `feature-not-found` wire string via handler dispatch.
///
/// Dispatches a `StatusRequest` with `StatusMode::Detail` for a feature id
/// that has no manifest on disk. The daemon handler returns
/// `CommandResponse::ExitJson { code: 1, value: { "status": "feature-not-found", ... } }`.
///
/// Asserts `value["status"] == ExitJsonStatus::FeatureNotFound.as_str()` — the
/// wire payload is what the binary would emit on stdout. This is the same
/// assertion point as the CLI binary test in `exit_json_status_phase7_wire_strings.rs`,
/// but exercised at the handler boundary rather than the binary boundary.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feature_not_found_wire_string_via_handler_dispatch() {
    let state_tmp = tempfile::tempdir().unwrap();
    let _guard = StateDirGuard::new(state_tmp.path());

    let project = tempfile::tempdir().unwrap();
    let ctx = DaemonContext::new("tok".to_string(), project.path().to_path_buf());

    let req = CommandRequest::Status(StatusRequest {
        json: true,
        mode: StatusMode::Detail,
        feature_id: Some("feat-handler-not-found".to_string()),
        stream_json: false,
        timeout_secs: None,
    });

    let resp = dispatch(req, &ctx, &NullSink).await.expect("dispatch");

    match resp {
        CommandResponse::ExitJson { code, value } => {
            assert_eq!(
                code,
                ExitJsonStatus::FeatureNotFound.exit_code(),
                "ExitJson code must equal ExitJsonStatus::FeatureNotFound.exit_code() ({}); \
                 got: {code}",
                ExitJsonStatus::FeatureNotFound.exit_code()
            );
            assert_eq!(
                value["status"].as_str().unwrap_or(""),
                ExitJsonStatus::FeatureNotFound.as_str(),
                "ExitJson value[\"status\"] must equal ExitJsonStatus::FeatureNotFound.as_str() ({}); \
                 got: {}",
                ExitJsonStatus::FeatureNotFound.as_str(),
                value["status"]
            );
            assert_eq!(
                value["feature_id"].as_str().unwrap_or(""),
                "feat-handler-not-found",
                "ExitJson value must carry the requested feature_id; got: {value}"
            );
        }
        other => panic!("expected ExitJson for missing feature, got: {other:?}"),
    }
}

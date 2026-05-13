//! Phase 7 Task 11: CLI-side `--background` / `--wait` plumbing.
//!
//! This module centralizes the logic shared by `pice evaluate --background`
//! and `pice execute --background` so neither command re-implements the
//! handshake with the daemon.
//!
//! ## Flow
//!
//! 1. The caller runs `adapter::dispatch(req)` with `req.background = true`
//!    (and `req.wait` as appropriate). The daemon either returns a
//!    `background-dispatched` `CommandResponse::Json` OR an error response
//!    (inline mode unsupported, feature already running, operational
//!    failure). Error responses pass straight through `render_response`.
//!
//! 2. If `--wait=false`: announce `{feature_id} ({run_id})` on stderr + a
//!    monitor hint, exit 0. `--json` mode renders the dispatched JSON
//!    verbatim via `render_response`.
//!
//! 3. If `--wait=true`: open a SECOND `DaemonClient` connection,
//!    `subscribe_stream(manifest/subscribe, { feature_id })`, and block
//!    until the feature reaches terminal status.
//!    - Short-circuit check: if the initial snapshot already shows a
//!      terminal manifest (`Passed` / `Failed` / `FailedInterrupted` /
//!      `PendingReview`), exit immediately — do NOT hang waiting for a
//!      `FeatureComplete` event that fired before we subscribed (Codex
//!      Cycle 2 terminal-short-circuit fix).
//!    - Otherwise, loop on `rx.recv().await` until `FeatureComplete` or
//!      `Cancelled` arrives, or the timeout expires, or the connection
//!      closes. Exit codes: 0 (Passed) / 2 (Failed|FailedInterrupted) /
//!      3 (PendingReview) / 4 (WaitTimeout) / 5 (DaemonDisconnected).

use anyhow::Result;
use pice_core::cli::{CommandResponse, ExitJsonStatus};
use pice_core::events::{ManifestEvent, ManifestEventPayload};
use pice_core::layers::manifest::{ManifestStatus, VerificationManifest};
use pice_core::protocol::methods::MANIFEST_SUBSCRIBE;
use pice_core::protocol::subscribe::{SubscribeManifestRequest, SubscribeManifestResponse};
use pice_core::protocol::DaemonNotification;
use pice_core::transport::SocketPath;
use pice_daemon::server::auth;
use serde_json::json;
use tokio::time::{sleep_until, Duration, Instant};

use crate::adapter::transport::DaemonClient;
use crate::notifications::{
    self, Dispatcher, NotificationKind, NotificationState, NotificationsConfig,
    NotifyRustDispatcher,
};

/// Outcome of the `--background`/`--wait` handshake. The caller's `run()`
/// returns `Ok(())` after rendering, or `std::process::exit(code)` for
/// non-zero terminal outcomes.
pub enum BackgroundOutcome {
    /// The daemon returned a non-dispatched response — pass it straight
    /// to `render_response` (which may `std::process::exit` itself).
    Passthrough(CommandResponse),
    /// Dispatch-only succeeded (no `--wait`). Caller should exit 0 after
    /// the helper prints the hint (text mode) or render_response emits
    /// the JSON.
    Dispatched {
        resp: CommandResponse,
        feature_id: String,
        run_id: String,
    },
    /// `--wait` completed; the helper has already printed the outcome
    /// and called `std::process::exit(code)`. This variant is
    /// unreachable in practice (the helper exits), but named so the
    /// match on `BackgroundOutcome` is exhaustive.
    #[allow(dead_code)]
    Waited,
}

/// Run the `--background` CLI path. Dispatches the request, branches on
/// `--wait`, and either returns a passthrough/dispatched outcome OR calls
/// `std::process::exit` with a wait-terminal code.
///
/// `kind` is a short descriptor ("evaluate" / "execute") used in the
/// human-readable hint — it lets the two callers share wording without
/// each duplicating the string.
pub async fn run_background(
    req: pice_core::cli::CommandRequest,
    json: bool,
    wait: bool,
    timeout_secs: Option<u64>,
    _kind: &str,
) -> Result<BackgroundOutcome> {
    let resp = crate::adapter::dispatch(req).await?;

    let (feature_id, run_id) = match parse_background_dispatched(&resp) {
        Some(pair) => pair,
        None => return Ok(BackgroundOutcome::Passthrough(resp)),
    };

    if !wait {
        return Ok(BackgroundOutcome::Dispatched {
            resp,
            feature_id,
            run_id,
        });
    }

    // --wait: subscribe on a separate connection and block until terminal.
    let code = wait_until_terminal(&feature_id, &run_id, timeout_secs, json).await?;
    std::process::exit(code);
}

/// Render the outcome produced by [`run_background`]. Called by the
/// evaluate/execute CLI command modules after `run_background` returns.
pub fn render_background_outcome(outcome: BackgroundOutcome, json: bool) -> Result<()> {
    match outcome {
        BackgroundOutcome::Passthrough(resp) => crate::commands::render_response(resp),
        BackgroundOutcome::Dispatched {
            resp,
            feature_id,
            run_id,
        } => {
            if json {
                return crate::commands::render_response(resp);
            }
            // Text-mode: intercept the Json response and print the hint.
            eprintln!("{feature_id} ({run_id})");
            eprintln!(
                "Run `pice status --follow {feature_id}` to monitor or \
                 `pice status --wait {feature_id}` to block until complete."
            );
            Ok(())
        }
        BackgroundOutcome::Waited => Ok(()),
    }
}

/// Extract `(feature_id, run_id)` from a daemon response iff it carries
/// the `background-dispatched` status discriminant.
fn parse_background_dispatched(resp: &CommandResponse) -> Option<(String, String)> {
    let value = match resp {
        CommandResponse::Json { value } => value,
        _ => return None,
    };
    if value.get("status")?.as_str()? != ExitJsonStatus::BackgroundDispatched.as_str() {
        return None;
    }
    let feature_id = value.get("feature_id")?.as_str()?.to_string();
    let run_id = value.get("run_id")?.as_str()?.to_string();
    Some((feature_id, run_id))
}

/// Block on a `manifest/subscribe` stream until a terminal event
/// arrives, the timeout fires, or the connection closes. Returns the
/// process exit code.
async fn wait_until_terminal(
    feature_id: &str,
    run_id: &str,
    timeout_secs: Option<u64>,
    json: bool,
) -> Result<i32> {
    let socket_path = SocketPath::default_from_env();
    let token_path = auth::default_token_path();
    let mut client = match DaemonClient::connect(&socket_path, &token_path).await {
        Ok(client) => client,
        Err(_) => {
            emit_daemon_disconnected(feature_id, run_id, json);
            return Ok(ExitJsonStatus::DaemonDisconnected.exit_code());
        }
    };
    if client.health_check().await.is_err() {
        emit_daemon_disconnected(feature_id, run_id, json);
        return Ok(ExitJsonStatus::DaemonDisconnected.exit_code());
    }
    let params = SubscribeManifestRequest {
        feature_id: Some(feature_id.to_string()),
    };
    let mut stream = match client
        .subscribe_stream::<_, SubscribeManifestResponse>(MANIFEST_SUBSCRIBE, params)
        .await
    {
        Ok(stream) => stream,
        Err(_) => {
            emit_daemon_disconnected(feature_id, run_id, json);
            return Ok(ExitJsonStatus::DaemonDisconnected.exit_code());
        }
    };

    // Short-circuit: feature may have already completed before subscribe.
    // Clone the status out of the snapshot so we can close the stream
    // before emitting output (the stream borrow must end before `close`
    // takes `self` by value).
    let terminal = stream
        .snapshot
        .snapshots
        .iter()
        .find(|m| m.feature_id == feature_id)
        .and_then(terminal_outcome_for_manifest);
    if let Some((wire, code)) = terminal {
        stream.close().await;
        // Task 14: snapshot short-circuit also emits a notification —
        // the user-facing outcome is the same as the rx-loop path.
        let notif_state = NotificationState::new();
        let notif_cfg = NotificationsConfig::default();
        notify_terminal_outcome(
            feature_id,
            &wire,
            &notif_state,
            &notif_cfg,
            &NotifyRustDispatcher,
        );
        emit_wait_outcome(feature_id, run_id, &wire, json);
        return Ok(code);
    }

    // Task 14: fire a desktop notification on the terminal outcome. The
    // dispatcher is a real `notify-rust` instance; failures are swallowed
    // to tracing::debug + terminal BEL fallback per
    // `notifications::notify`. Defaults-only config for now — a future
    // Task 19 pass reads `[notifications]` from the user's config.
    let notif_state = NotificationState::new();
    let notif_cfg = NotificationsConfig::default();
    let notif_dispatcher = NotifyRustDispatcher;

    let deadline = timeout_secs.map(|s| Instant::now() + Duration::from_secs(s));

    loop {
        let timeout_fut = async {
            match deadline {
                Some(d) => sleep_until(d).await,
                None => std::future::pending::<()>().await,
            }
        };

        tokio::select! {
            biased;

            _ = timeout_fut => {
                stream.close().await;
                emit_wait_timeout(feature_id, run_id, timeout_secs, json);
                return Ok(ExitJsonStatus::WaitTimeout.exit_code());
            }

            recv = stream.rx.recv() => {
                match recv {
                    Some(notif) => {
                        if let Some((status_wire, code)) =
                            parse_terminal_notification(&notif)
                        {
                            stream.close().await;
                            // Notify BEFORE printing the outcome so the
                            // desktop alert doesn't race the terminal
                            // render (they race on the same scheduler
                            // tick but the desktop path is slower; firing
                            // first minimizes the visible gap).
                            notify_terminal_outcome(
                                feature_id,
                                &status_wire,
                                &notif_state,
                                &notif_cfg,
                                &notif_dispatcher,
                            );
                            emit_wait_outcome(feature_id, run_id, &status_wire, json);
                            return Ok(code);
                        }
                        // Non-terminal event (LayerStarted, PassComplete, etc.) —
                        // keep looping.
                    }
                    None => {
                        stream.close().await;
                        emit_daemon_disconnected(feature_id, run_id, json);
                        return Ok(ExitJsonStatus::DaemonDisconnected.exit_code());
                    }
                }
            }
        }
    }
}

/// Map a [`ManifestStatus`] to a terminal-exit code, or `None` if the
/// status is not terminal.
fn terminal_exit_code(status: &ManifestStatus) -> Option<i32> {
    match status {
        ManifestStatus::Passed => Some(0),
        ManifestStatus::Failed | ManifestStatus::FailedInterrupted => {
            Some(ExitJsonStatus::EvaluationFailed.exit_code())
        }
        ManifestStatus::PendingReview => Some(ExitJsonStatus::ReviewGatePending.exit_code()),
        ManifestStatus::Pending | ManifestStatus::InProgress | ManifestStatus::Queued => None,
    }
}

fn terminal_outcome_for_manifest(manifest: &VerificationManifest) -> Option<(String, i32)> {
    if manifest
        .layers
        .iter()
        .filter_map(|layer| layer.halted_by.as_deref())
        .any(ExitJsonStatus::is_metrics_persist_failed)
    {
        return Some((
            ExitJsonStatus::MetricsPersistFailed.as_str().to_string(),
            ExitJsonStatus::MetricsPersistFailed.exit_code(),
        ));
    }
    terminal_exit_code(&manifest.overall_status)
        .map(|code| (manifest_status_wire(&manifest.overall_status), code))
}

/// Wire string for a [`ManifestStatus`] — matches `#[serde(rename_all =
/// "kebab-case")]`. Used for the JSON-mode output envelope. Not derived
/// from serde because we want a stable `Display`-style mapping without
/// serializing the enum (which would wrap it in quotes).
fn manifest_status_wire(status: &ManifestStatus) -> String {
    match status {
        ManifestStatus::Pending => "pending",
        ManifestStatus::InProgress => "in-progress",
        ManifestStatus::Passed => "passed",
        ManifestStatus::Failed => "failed",
        ManifestStatus::FailedInterrupted => ExitJsonStatus::FailedInterrupted.as_str(),
        ManifestStatus::PendingReview => "pending-review",
        ManifestStatus::Queued => "queued",
    }
    .to_string()
}

/// Inspect a daemon notification for a terminal `FeatureComplete` /
/// `Cancelled` event. Returns `Some((status_wire, exit_code))` if the
/// event closes the wait, `None` otherwise.
fn parse_terminal_notification(notif: &DaemonNotification) -> Option<(String, i32)> {
    if notif.method != pice_core::protocol::methods::MANIFEST_EVENT {
        return None;
    }
    let payload: ManifestEventPayload = serde_json::from_value(notif.params.clone()).ok()?;
    match payload.event {
        ManifestEvent::FeatureComplete => {
            // `data.overall_status` is the kebab-cased ManifestStatus
            // wire string. Accept the legacy `status` alias so an updated
            // CLI can wait against an older daemon during rolling upgrades.
            // If missing / unrecognized, treat as Failed.
            let status_wire = payload
                .data
                .get("exit_status")
                .or_else(|| payload.data.get("overall_status"))
                .or_else(|| payload.data.get("status"))
                .and_then(|v| v.as_str())
                .unwrap_or("failed")
                .to_string();
            let code = match status_wire.as_str() {
                "passed" => 0,
                "pending-review" => ExitJsonStatus::ReviewGatePending.exit_code(),
                "metrics-persist-failed" => ExitJsonStatus::MetricsPersistFailed.exit_code(),
                _ => ExitJsonStatus::EvaluationFailed.exit_code(),
            };
            Some((status_wire, code))
        }
        ManifestEvent::Cancelled => {
            // `Cancelled` is the feature-level abort event. Map to the
            // same exit family as a contract failure — the feature did
            // not pass the bar.
            Some((
                "cancelled".to_string(),
                ExitJsonStatus::EvaluationFailed.exit_code(),
            ))
        }
        _ => None,
    }
}
/// Map a terminal wire-status to a [`NotificationKind`] + title/body and
/// fire a desktop notification. Handles the three `wait_until_terminal`
/// outcomes (passed / pending-review / failed|cancelled) plus anything
/// else that reaches the terminal branch (mapped to Failure — a defensive
/// default since unknown statuses at this point mean the feature ended
/// without passing).
fn notify_terminal_outcome(
    feature_id: &str,
    status_wire: &str,
    state: &NotificationState,
    cfg: &NotificationsConfig,
    dispatcher: &dyn Dispatcher,
) {
    let (kind, title, body) = match status_wire {
        "passed" => (
            NotificationKind::Complete,
            format!("pice: {feature_id} passed"),
            "evaluation completed successfully.".to_string(),
        ),
        "pending-review" => (
            NotificationKind::Gate,
            format!("pice: {feature_id} — review gate"),
            "the feature is waiting on a reviewer decision.".to_string(),
        ),
        other => (
            NotificationKind::Failure,
            format!("pice: {feature_id} — {other}"),
            "evaluation ended without passing.".to_string(),
        ),
    };
    notifications::notify(state, cfg, kind, feature_id, &title, &body, dispatcher);
}

fn emit_wait_outcome(feature_id: &str, run_id: &str, status_wire: &str, json: bool) {
    if json {
        let value = json!({
            "status": status_wire,
            "feature_id": feature_id,
            "run_id": run_id,
        });
        // Stdout: JSON envelope. The status discriminant matches the
        // manifest's kebab-case wire form (no `--wait-succeeded` vs
        // `--wait-failed` discriminator — the overall status IS the
        // discriminator).
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
        );
    } else {
        // Stderr: one-line human summary. Exit code conveys the outcome;
        // the wire-form status is human-readable enough for the one-line
        // summary.
        eprintln!("feature {feature_id} ({run_id}) — {status_wire}");
    }
}

fn emit_wait_timeout(feature_id: &str, run_id: &str, timeout_secs: Option<u64>, json: bool) {
    let timeout_repr = timeout_secs
        .map(|s| s.to_string())
        .unwrap_or_else(|| "unbounded".to_string());
    if json {
        let value = json!({
            "status": ExitJsonStatus::WaitTimeout.as_str(),
            "feature_id": feature_id,
            "run_id": run_id,
            "timeout_secs": timeout_secs,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
        );
    } else {
        eprintln!(
            "wait timed out after {timeout_repr}s — feature {feature_id} ({run_id}) \
             still running. Re-run `pice status --wait {feature_id}` to resume \
             waiting."
        );
    }
}

fn emit_daemon_disconnected(feature_id: &str, run_id: &str, json: bool) {
    if json {
        let value = json!({
            "status": ExitJsonStatus::DaemonDisconnected.as_str(),
            "feature_id": feature_id,
            "run_id": run_id,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
        );
    } else {
        eprintln!(
            "subscribe connection closed before feature {feature_id} ({run_id}) \
             reached terminal state. Re-run `pice status --wait {feature_id}` \
             to resume."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pice_core::events::{ManifestEvent, ManifestEventPayload};
    use pice_core::protocol::{methods, DaemonNotification};
    use serde_json::json;

    #[test]
    fn parse_background_dispatched_extracts_ids() {
        let resp = CommandResponse::Json {
            value: json!({
                "status": "background-dispatched",
                "feature_id": "feat-x",
                "run_id": "r-1001",
            }),
        };
        let (f, r) = parse_background_dispatched(&resp).expect("dispatched");
        assert_eq!(f, "feat-x");
        assert_eq!(r, "r-1001");
    }

    #[test]
    fn parse_background_dispatched_rejects_other_statuses() {
        let resp = CommandResponse::Json {
            value: json!({
                "status": "feature-already-running",
                "feature_id": "feat-x",
                "run_id": "r-1001",
            }),
        };
        assert!(parse_background_dispatched(&resp).is_none());
    }

    #[test]
    fn parse_background_dispatched_rejects_non_json_variants() {
        let resp = CommandResponse::Text {
            content: "not json".to_string(),
        };
        assert!(parse_background_dispatched(&resp).is_none());
    }

    #[test]
    fn terminal_exit_code_maps_expected_variants() {
        assert_eq!(terminal_exit_code(&ManifestStatus::Passed), Some(0));
        assert_eq!(terminal_exit_code(&ManifestStatus::Failed), Some(2));
        assert_eq!(
            terminal_exit_code(&ManifestStatus::FailedInterrupted),
            Some(2)
        );
        assert_eq!(terminal_exit_code(&ManifestStatus::PendingReview), Some(3));
        assert_eq!(terminal_exit_code(&ManifestStatus::InProgress), None);
        assert_eq!(terminal_exit_code(&ManifestStatus::Queued), None);
        assert_eq!(terminal_exit_code(&ManifestStatus::Pending), None);
    }

    #[test]
    fn terminal_outcome_for_manifest_maps_metrics_persist_failed_to_status_one() {
        let mut manifest = VerificationManifest::new("f", std::path::Path::new("."));
        manifest.overall_status = ManifestStatus::Pending;
        manifest
            .layers
            .push(pice_core::layers::manifest::LayerResult {
                name: "metrics".to_string(),
                status: pice_core::layers::manifest::LayerStatus::Pending,
                passes: Vec::new(),
                seam_checks: Vec::new(),
                halted_by: Some(format!(
                    "{}sqlite locked",
                    ExitJsonStatus::METRICS_PERSIST_FAILED_PREFIX
                )),
                final_confidence: None,
                total_cost_usd: None,
                escalation_events: None,
            });
        assert_eq!(
            terminal_outcome_for_manifest(&manifest),
            Some((
                ExitJsonStatus::MetricsPersistFailed.as_str().to_string(),
                ExitJsonStatus::MetricsPersistFailed.exit_code()
            ))
        );
    }

    #[test]
    fn parse_terminal_notification_feature_complete_passed_returns_zero() {
        let payload = ManifestEventPayload {
            feature_id: "f".to_string(),
            run_id: "r-1".to_string(),
            event: ManifestEvent::FeatureComplete,
            layer: None,
            data: json!({"overall_status": "passed"}),
            timestamp: "2026-04-21T10:00:00Z".to_string(),
        };
        let notif = DaemonNotification::new(
            methods::MANIFEST_EVENT,
            serde_json::to_value(payload).unwrap(),
        );
        let (status, code) = parse_terminal_notification(&notif).expect("terminal");
        assert_eq!(status, "passed");
        assert_eq!(code, 0);
    }

    #[test]
    fn parse_terminal_notification_feature_complete_metrics_failed_returns_one() {
        let payload = ManifestEventPayload {
            feature_id: "f".to_string(),
            run_id: "r-1".to_string(),
            event: ManifestEvent::FeatureComplete,
            layer: None,
            data: json!({
                "overall_status": "failed",
                "exit_status": "metrics-persist-failed"
            }),
            timestamp: "2026-04-21T10:00:00Z".to_string(),
        };
        let notif = DaemonNotification::new(
            methods::MANIFEST_EVENT,
            serde_json::to_value(payload).unwrap(),
        );
        let (status, code) = parse_terminal_notification(&notif).expect("terminal");
        assert_eq!(status, "metrics-persist-failed");
        assert_eq!(code, ExitJsonStatus::MetricsPersistFailed.exit_code());
    }

    #[test]
    fn parse_terminal_notification_feature_complete_failed_returns_two() {
        let payload = ManifestEventPayload {
            feature_id: "f".to_string(),
            run_id: "r-1".to_string(),
            event: ManifestEvent::FeatureComplete,
            layer: None,
            data: json!({"overall_status": "failed"}),
            timestamp: "2026-04-21T10:00:00Z".to_string(),
        };
        let notif = DaemonNotification::new(
            methods::MANIFEST_EVENT,
            serde_json::to_value(payload).unwrap(),
        );
        let (status, code) = parse_terminal_notification(&notif).expect("terminal");
        assert_eq!(status, "failed");
        assert_eq!(code, 2);
    }

    #[test]
    fn parse_terminal_notification_cancelled_returns_two() {
        let payload = ManifestEventPayload {
            feature_id: "f".to_string(),
            run_id: "r-1".to_string(),
            event: ManifestEvent::Cancelled,
            layer: None,
            data: json!({"reason": "shutdown"}),
            timestamp: "2026-04-21T10:00:00Z".to_string(),
        };
        let notif = DaemonNotification::new(
            methods::MANIFEST_EVENT,
            serde_json::to_value(payload).unwrap(),
        );
        let (status, code) = parse_terminal_notification(&notif).expect("terminal");
        assert_eq!(status, "cancelled");
        assert_eq!(code, 2);
    }

    #[test]
    fn parse_terminal_notification_non_terminal_events_ignored() {
        for ev in [
            ManifestEvent::LayerStarted,
            ManifestEvent::LayerComplete,
            ManifestEvent::PassComplete,
            ManifestEvent::GateRequested,
            ManifestEvent::GateDecided,
            ManifestEvent::SeamFinding,
        ] {
            let payload = ManifestEventPayload {
                feature_id: "f".to_string(),
                run_id: "r-1".to_string(),
                event: ev,
                layer: Some("backend".to_string()),
                data: json!({}),
                timestamp: "2026-04-21T10:00:00Z".to_string(),
            };
            let notif = DaemonNotification::new(
                methods::MANIFEST_EVENT,
                serde_json::to_value(payload).unwrap(),
            );
            assert!(
                parse_terminal_notification(&notif).is_none(),
                "{ev:?} should be non-terminal"
            );
        }
    }

    #[test]
    fn manifest_status_wire_matches_serde_kebab_case() {
        // If someone adds a new ManifestStatus variant or changes serde
        // rename_all, the existing variants must keep their kebab-case
        // wire string. This test pins the expected-format mapping.
        for (status, expected) in [
            (ManifestStatus::Pending, "pending"),
            (ManifestStatus::InProgress, "in-progress"),
            (ManifestStatus::Passed, "passed"),
            (ManifestStatus::Failed, "failed"),
            (
                ManifestStatus::FailedInterrupted,
                ExitJsonStatus::FailedInterrupted.as_str(),
            ),
            (ManifestStatus::PendingReview, "pending-review"),
            (ManifestStatus::Queued, "queued"),
        ] {
            assert_eq!(manifest_status_wire(&status), expected, "{status:?}");
        }
    }
}

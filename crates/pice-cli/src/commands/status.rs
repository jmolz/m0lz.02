//! `pice status` — Phase 7 rewrite with four invocation shapes.
//!
//! The CLI discriminates on [`StatusMode`] computed from parsed flags:
//! - `pice status`                           → `StatusMode::List`  (cli/dispatch)
//! - `pice status <feature_id>`              → `StatusMode::Detail` (cli/dispatch)
//! - `pice status --follow [<feature_id>]`   → `StatusMode::Follow` (manifest/subscribe)
//! - `pice status --wait <feature_id>`       → `StatusMode::Wait`   (manifest/subscribe)
//!
//! `List` and `Detail` traverse the daemon's `cli/dispatch` handler and go
//! through [`crate::commands::render_response`]. `Follow` and `Wait` bypass
//! `cli/dispatch` entirely and open a [`crate::adapter::transport::DaemonClient::subscribe_stream`]
//! connection directly. The daemon handler's `Detail` arm returns the full
//! [`VerificationManifest`]; `List` preserves the historical plan-scan
//! response (tests depend on this).
//!
//! Clap-enforced conflict rules:
//! - `--follow` conflicts with `--wait`
//! - `--wait` requires a `feature_id`
//! - `--json` conflicts with `--follow` (single-JSON-object invariant)
//! - `--stream-json` requires `--follow`
//!
//! Exit codes for `Wait`:
//! - 0 `Passed`
//! - 2 `Failed` / `FailedInterrupted`
//! - 3 `PendingReview`
//! - 4 `WaitTimeout`
//! - 5 `DaemonDisconnected`

use anyhow::{Context, Result};
use clap::Args;
use pice_core::cli::{CommandRequest, ExitJsonStatus, StatusMode, StatusRequest};
use pice_core::events::{ManifestEvent, ManifestEventPayload, StreamJsonFrame};
use pice_core::layers::manifest::ManifestStatus;
use pice_core::protocol::methods::{MANIFEST_EVENT, MANIFEST_SUBSCRIBE};
use pice_core::protocol::subscribe::{SubscribeManifestRequest, SubscribeManifestResponse};
use serde_json::json;
use tokio::time::{sleep_until, Duration, Instant};

use crate::adapter::autostart::ensure_daemon_running;

#[derive(Args, Debug, Clone)]
pub struct StatusArgs {
    /// Feature id to inspect. Required for `--wait`; optional for
    /// `--follow` (omit to tail every manifest).
    pub feature_id: Option<String>,

    /// Stream live manifest updates as events arrive. Conflicts with
    /// `--wait` and `--json`.
    #[arg(long, conflicts_with_all = ["wait", "json"])]
    pub follow: bool,

    /// Block until the feature reaches a terminal status. Requires a
    /// `feature_id` positional.
    #[arg(long, requires = "feature_id", conflicts_with = "follow")]
    pub wait: bool,

    /// Max seconds to wait before returning exit 4 (`WaitTimeout`).
    /// Requires `--wait`.
    #[arg(long, value_name = "N", requires = "wait")]
    pub timeout_secs: Option<u64>,

    /// Output as a single JSON object. Conflicts with `--follow` (which
    /// emits an NDJSON stream) — the JSON mode guarantees a single
    /// top-level object.
    #[arg(long)]
    pub json: bool,

    /// Emit heterogeneous `StreamJsonFrame` NDJSON frames. Requires
    /// `--follow`.
    #[arg(long, requires = "follow")]
    pub stream_json: bool,
}

impl StatusArgs {
    /// Compute the [`StatusMode`] from parsed flags.
    pub fn mode(&self) -> StatusMode {
        if self.follow {
            StatusMode::Follow
        } else if self.wait {
            StatusMode::Wait
        } else if self.feature_id.is_some() {
            StatusMode::Detail
        } else {
            StatusMode::List
        }
    }
}

impl TryFrom<StatusArgs> for StatusRequest {
    type Error = anyhow::Error;

    fn try_from(args: StatusArgs) -> Result<Self> {
        let mode = args.mode();
        if mode == StatusMode::Wait && args.feature_id.is_none() {
            anyhow::bail!("--wait requires a feature_id positional");
        }
        Ok(StatusRequest {
            json: args.json,
            mode,
            feature_id: args.feature_id,
            stream_json: args.stream_json,
            timeout_secs: args.timeout_secs,
        })
    }
}

pub async fn run(args: &StatusArgs) -> Result<()> {
    let req: StatusRequest = args
        .clone()
        .try_into()
        .context("invalid pice status arguments")?;
    match req.mode {
        StatusMode::List | StatusMode::Detail => {
            let resp = crate::adapter::dispatch(CommandRequest::Status(req)).await?;
            super::render_response(resp)
        }
        StatusMode::Follow => run_follow(req).await,
        StatusMode::Wait => run_wait(req).await,
    }
}

/// `pice status --follow [<feature_id>]` — stream live manifest updates.
///
/// Opens a fresh [`crate::adapter::transport::DaemonClient`] (the subscribe
/// stream consumes the client — see `transport::DaemonClient::subscribe_stream`
/// docs), renders the initial snapshot, then forwards every subsequent
/// `manifest/event` notification until the stream closes or a
/// `FeatureComplete` / `Cancelled` event arrives for the subscribed feature.
///
/// Short-circuit: if the snapshot already shows a terminal overall status
/// for the subscribed feature, the final frame is rendered and the loop
/// never runs — closes the Codex Cycle 2 hazard (FeatureComplete fired
/// before subscribe → infinite wait).
async fn run_follow(req: StatusRequest) -> Result<()> {
    let feature_id_filter = req.feature_id.clone();
    let stream_json = req.stream_json;
    let client = ensure_daemon_running()
        .await
        .context("failed to open subscribe connection for --follow")?;
    let params = SubscribeManifestRequest {
        feature_id: feature_id_filter.clone(),
    };
    let mut stream = client
        .subscribe_stream::<_, SubscribeManifestResponse>(MANIFEST_SUBSCRIBE, params)
        .await
        .context("failed to open manifest/subscribe stream for --follow")?;

    // Snapshot render.
    if stream_json {
        emit_stream_snapshot(&stream.snapshot)?;
    } else {
        render_follow_snapshot(&stream.snapshot, feature_id_filter.as_deref());
    }

    // Short-circuit: the subscribed feature is already terminal at
    // subscribe time. Render a terminal frame and exit. Wildcard follows
    // (`feature_id_filter == None`) don't short-circuit — they tail every
    // feature.
    if let Some(ref fid) = feature_id_filter {
        let terminal = stream
            .snapshot
            .snapshots
            .iter()
            .find(|m| &m.feature_id == fid)
            .and_then(|m| terminal_exit_code(&m.overall_status));
        if let Some(code) = terminal {
            if stream_json {
                emit_stream_terminal(code)?;
            } else {
                eprintln!("feature {fid} already terminal — follow stream closed");
            }
            stream.close().await;
            if code != 0 {
                std::process::exit(code);
            }
            return Ok(());
        }
    }

    loop {
        match stream.rx.recv().await {
            Some(notif) => {
                if notif.method != MANIFEST_EVENT {
                    continue;
                }
                let Ok(payload) = serde_json::from_value::<ManifestEventPayload>(notif.params)
                else {
                    continue;
                };
                // Filter scoped follows by feature_id (the daemon already
                // filters, but a wildcard subscribe receives everything —
                // defensive match for the stable-CLI guarantee).
                if let Some(ref fid) = feature_id_filter {
                    if &payload.feature_id != fid {
                        continue;
                    }
                }
                if stream_json {
                    emit_stream_event(&payload)?;
                } else {
                    render_follow_event(&payload);
                }
                if let Some((_, code)) = terminal_from_event(&payload) {
                    if stream_json {
                        emit_stream_terminal(code)?;
                    } else {
                        eprintln!("feature {} terminal — closing stream", payload.feature_id);
                    }
                    stream.close().await;
                    if code != 0 {
                        std::process::exit(code);
                    }
                    return Ok(());
                }
            }
            None => {
                if stream_json {
                    emit_stream_terminal(ExitJsonStatus::DaemonDisconnected.exit_code())?;
                }
                eprintln!("subscribe stream closed before terminal event");
                std::process::exit(ExitJsonStatus::DaemonDisconnected.exit_code());
            }
        }
    }
}

/// `pice status --wait <feature_id>` — block until terminal state.
///
/// Semantically identical to `pice evaluate --background --wait`'s
/// `wait_until_terminal`: subscribe, short-circuit on already-terminal,
/// then `select!` between the rx stream and the deadline.
async fn run_wait(req: StatusRequest) -> Result<()> {
    let Some(feature_id) = req.feature_id.clone() else {
        anyhow::bail!("--wait requires a feature_id positional");
    };
    let timeout_secs = req.timeout_secs;
    let json = req.json;

    let client = ensure_daemon_running()
        .await
        .context("failed to open subscribe connection for --wait")?;
    let params = SubscribeManifestRequest {
        feature_id: Some(feature_id.clone()),
    };
    let mut stream = client
        .subscribe_stream::<_, SubscribeManifestResponse>(MANIFEST_SUBSCRIBE, params)
        .await
        .context("failed to open manifest/subscribe stream for --wait")?;

    // Short-circuit on already-terminal snapshot. Clone the status out of
    // the snapshot before closing the stream — the borrow must end before
    // `close` takes `self`.
    let terminal = stream
        .snapshot
        .snapshots
        .iter()
        .find(|m| m.feature_id == feature_id)
        .and_then(|m| terminal_exit_code(&m.overall_status).map(|c| (m.overall_status.clone(), c)));
    if let Some((status, code)) = terminal {
        stream.close().await;
        emit_wait_outcome(&feature_id, &manifest_status_wire(&status), json);
        std::process::exit(code);
    }

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
                emit_wait_timeout(&feature_id, timeout_secs, json);
                std::process::exit(ExitJsonStatus::WaitTimeout.exit_code());
            }
            recv = stream.rx.recv() => {
                match recv {
                    Some(notif) => {
                        if notif.method != MANIFEST_EVENT { continue; }
                        let Ok(payload) = serde_json::from_value::<ManifestEventPayload>(notif.params) else { continue; };
                        if payload.feature_id != feature_id { continue; }
                        if let Some((status_wire, code)) = terminal_from_event(&payload) {
                            stream.close().await;
                            emit_wait_outcome(&feature_id, &status_wire, json);
                            std::process::exit(code);
                        }
                    }
                    None => {
                        emit_daemon_disconnected(&feature_id, json);
                        std::process::exit(ExitJsonStatus::DaemonDisconnected.exit_code());
                    }
                }
            }
        }
    }
}

// ─── Render helpers ─────────────────────────────────────────────────────────

fn render_follow_snapshot(snap: &SubscribeManifestResponse, feature_id: Option<&str>) {
    // Stdout: one-line per feature summary (the visible frame — channel
    // ownership rule: stdout = frames, stderr = prompts).
    let filtered: Vec<_> = match feature_id {
        Some(id) => snap
            .snapshots
            .iter()
            .filter(|m| m.feature_id == id)
            .collect(),
        None => snap.snapshots.iter().collect(),
    };
    if filtered.is_empty() {
        println!("(no manifests matched subscribe filter — waiting for events)");
        return;
    }
    for m in filtered {
        let overall = serde_json::to_value(&m.overall_status)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "?".to_string());
        let layers_done = m
            .layers
            .iter()
            .filter(|l| {
                use pice_core::layers::manifest::LayerStatus;
                matches!(
                    l.status,
                    LayerStatus::Passed
                        | LayerStatus::Failed
                        | LayerStatus::Skipped
                        | LayerStatus::PendingReview
                )
            })
            .count();
        println!(
            "{}  overall={overall}  layers={}/{}",
            m.feature_id,
            layers_done,
            m.layers.len()
        );
    }
}

fn render_follow_event(payload: &ManifestEventPayload) {
    // Stdout: `{feature_id} {event_type} [{layer}]`. Terse by design — the
    // event envelope is the visible frame.
    let layer = payload.layer.as_deref().unwrap_or("-");
    println!(
        "{}  {}  layer={}  ts={}",
        payload.feature_id,
        payload.event.as_str(),
        layer,
        payload.timestamp
    );
}

fn emit_stream_snapshot(snap: &SubscribeManifestResponse) -> Result<()> {
    let frame = StreamJsonFrame::Snapshot {
        snapshot: snap.clone(),
    };
    println!("{}", serde_json::to_string(&frame)?);
    Ok(())
}

fn emit_stream_event(payload: &ManifestEventPayload) -> Result<()> {
    let frame = StreamJsonFrame::Event {
        event: payload.clone(),
    };
    println!("{}", serde_json::to_string(&frame)?);
    Ok(())
}

fn emit_stream_terminal(code: i32) -> Result<()> {
    let frame = StreamJsonFrame::Terminal { exit_code: code };
    println!("{}", serde_json::to_string(&frame)?);
    Ok(())
}

fn emit_wait_outcome(feature_id: &str, status_wire: &str, json: bool) {
    if json {
        let value = json!({
            "status": status_wire,
            "feature_id": feature_id,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
        );
    } else {
        eprintln!("feature {feature_id} — {status_wire}");
    }
}

fn emit_wait_timeout(feature_id: &str, timeout_secs: Option<u64>, json: bool) {
    if json {
        let value = json!({
            "status": ExitJsonStatus::WaitTimeout.as_str(),
            "feature_id": feature_id,
            "timeout_secs": timeout_secs,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
        );
    } else {
        let repr = timeout_secs
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unbounded".to_string());
        eprintln!(
            "wait timed out after {repr}s — feature {feature_id} still running. \
             Re-run `pice status --wait {feature_id}` to resume."
        );
    }
}

fn emit_daemon_disconnected(feature_id: &str, json: bool) {
    if json {
        let value = json!({
            "status": ExitJsonStatus::DaemonDisconnected.as_str(),
            "feature_id": feature_id,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
        );
    } else {
        eprintln!(
            "subscribe connection closed before feature {feature_id} reached \
             terminal state"
        );
    }
}

// ─── Pure helpers (tested in inline tests) ──────────────────────────────────

/// Map a [`ManifestStatus`] to its terminal exit code, or `None` if
/// non-terminal. Mirror of the helper in `adapter::background_wait`.
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

/// Stable kebab-case mapping for [`ManifestStatus`] — mirrors the serde
/// `rename_all = "kebab-case"` output without going through a `Value`
/// roundtrip.
fn manifest_status_wire(status: &ManifestStatus) -> String {
    match status {
        ManifestStatus::Pending => "pending",
        ManifestStatus::InProgress => "in-progress",
        ManifestStatus::Passed => "passed",
        ManifestStatus::Failed => "failed",
        ManifestStatus::FailedInterrupted => "failed-interrupted",
        ManifestStatus::PendingReview => "pending-review",
        ManifestStatus::Queued => "queued",
    }
    .to_string()
}

/// Inspect an event payload for a terminal `FeatureComplete` / `Cancelled`
/// event. Returns `Some((status_wire, exit_code))` on terminal, else
/// `None` — the follow / wait loops call this each recv.
fn terminal_from_event(payload: &ManifestEventPayload) -> Option<(String, i32)> {
    match payload.event {
        ManifestEvent::FeatureComplete => {
            let status_wire = payload
                .data
                .get("overall_status")
                .and_then(|v| v.as_str())
                .unwrap_or("failed")
                .to_string();
            let code = match status_wire.as_str() {
                "passed" => 0,
                "pending-review" => ExitJsonStatus::ReviewGatePending.exit_code(),
                _ => ExitJsonStatus::EvaluationFailed.exit_code(),
            };
            Some((status_wire, code))
        }
        ManifestEvent::Cancelled => Some((
            "cancelled".to_string(),
            ExitJsonStatus::EvaluationFailed.exit_code(),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mode_list_when_no_feature_no_flags() {
        let args = StatusArgs {
            feature_id: None,
            follow: false,
            wait: false,
            timeout_secs: None,
            json: false,
            stream_json: false,
        };
        assert_eq!(args.mode(), StatusMode::List);
    }

    #[test]
    fn mode_detail_when_feature_id_given() {
        let args = StatusArgs {
            feature_id: Some("f".to_string()),
            follow: false,
            wait: false,
            timeout_secs: None,
            json: false,
            stream_json: false,
        };
        assert_eq!(args.mode(), StatusMode::Detail);
    }

    #[test]
    fn mode_follow_takes_precedence_over_detail() {
        let args = StatusArgs {
            feature_id: Some("f".to_string()),
            follow: true,
            wait: false,
            timeout_secs: None,
            json: false,
            stream_json: true,
        };
        assert_eq!(args.mode(), StatusMode::Follow);
    }

    #[test]
    fn mode_wait_when_flag_set() {
        let args = StatusArgs {
            feature_id: Some("f".to_string()),
            follow: false,
            wait: true,
            timeout_secs: Some(30),
            json: false,
            stream_json: false,
        };
        assert_eq!(args.mode(), StatusMode::Wait);
    }

    #[test]
    fn try_from_wait_without_feature_id_fails() {
        // Defense-in-depth: clap enforces `--wait requires feature_id`, but
        // a caller bypassing clap (inline test) must still hit the bail.
        let args = StatusArgs {
            feature_id: None,
            follow: false,
            wait: true,
            timeout_secs: None,
            json: false,
            stream_json: false,
        };
        let result: Result<StatusRequest> = args.try_into();
        assert!(result.is_err());
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
        assert_eq!(terminal_exit_code(&ManifestStatus::Pending), None);
        assert_eq!(terminal_exit_code(&ManifestStatus::InProgress), None);
        assert_eq!(terminal_exit_code(&ManifestStatus::Queued), None);
    }

    #[test]
    fn terminal_from_event_feature_complete_passed_returns_zero() {
        let payload = ManifestEventPayload {
            feature_id: "f".to_string(),
            run_id: "r-1".to_string(),
            event: ManifestEvent::FeatureComplete,
            layer: None,
            data: json!({"overall_status": "passed"}),
            timestamp: "2026-04-21T10:00:00Z".to_string(),
        };
        let (status, code) = terminal_from_event(&payload).expect("terminal");
        assert_eq!(status, "passed");
        assert_eq!(code, 0);
    }

    #[test]
    fn terminal_from_event_feature_complete_pending_review_returns_three() {
        let payload = ManifestEventPayload {
            feature_id: "f".to_string(),
            run_id: "r-1".to_string(),
            event: ManifestEvent::FeatureComplete,
            layer: None,
            data: json!({"overall_status": "pending-review"}),
            timestamp: "2026-04-21T10:00:00Z".to_string(),
        };
        let (status, code) = terminal_from_event(&payload).expect("terminal");
        assert_eq!(status, "pending-review");
        assert_eq!(code, 3);
    }

    #[test]
    fn terminal_from_event_cancelled_returns_two() {
        let payload = ManifestEventPayload {
            feature_id: "f".to_string(),
            run_id: "r-1".to_string(),
            event: ManifestEvent::Cancelled,
            layer: None,
            data: json!({"reason": "shutdown"}),
            timestamp: "2026-04-21T10:00:00Z".to_string(),
        };
        let (status, code) = terminal_from_event(&payload).expect("terminal");
        assert_eq!(status, "cancelled");
        assert_eq!(code, 2);
    }

    #[test]
    fn terminal_from_event_non_terminal_events_ignored() {
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
            assert!(terminal_from_event(&payload).is_none(), "{ev:?}");
        }
    }

    #[test]
    fn manifest_status_wire_mirrors_serde_kebab_case() {
        for (status, expected) in [
            (ManifestStatus::Pending, "pending"),
            (ManifestStatus::InProgress, "in-progress"),
            (ManifestStatus::Passed, "passed"),
            (ManifestStatus::Failed, "failed"),
            (ManifestStatus::FailedInterrupted, "failed-interrupted"),
            (ManifestStatus::PendingReview, "pending-review"),
            (ManifestStatus::Queued, "queued"),
        ] {
            assert_eq!(manifest_status_wire(&status), expected, "{status:?}");
        }
    }
}

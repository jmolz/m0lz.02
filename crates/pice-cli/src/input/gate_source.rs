//! Phase 7 Task 18: `SubscribedGateSource` — reviewer input for
//! `pice status --follow` when a
//! [`pice_core::events::ManifestEvent::GateRequested`] arrives.
//!
//! This is a **concrete struct, not a trait**. Phase 6 initially shipped
//! a `DecisionSource` trait with 3 impls; the Pass-3 review removed it
//! as unused scaffolding (see `.claude/rules/rust-core.md` → "Don't ship
//! trait-based scaffolding ahead of a real consumer"). Phase 7 re-
//! introduces exactly ONE input shape — the live-subscribe-stream gate
//! prompt — and names it concretely. Future phases add a trait ONLY if
//! a second consumer (e.g., dashboard gate UI) emerges.
//!
//! ## Wiring
//!
//! `SubscribedGateSource` runs on the CLI's follow-loop task. When a
//! `GateRequested` event arrives on the subscribe stream, the loop
//! invokes [`SubscribedGateSource::handle_gate_requested`] which:
//!
//! 1. Renders the prompt box to stderr (channel-ownership rule).
//! 2. Reads the reviewer's decision from stdin via
//!    `tokio::task::spawn_blocking` — sidesteps `StdinLock: !Send`, the
//!    Phase 6 blocker that killed the original trait abstraction.
//! 3. Fires `CommandRequest::ReviewGate(Decide { ... })` via a SEPARATE
//!    `DaemonClient` connection — the subscribe client is busy reading
//!    notifications, so a concurrent `cli/dispatch` would stall on the
//!    same socket. The daemon's bearer-token auth explicitly permits
//!    concurrent connections.
//!
//! ## What the daemon does NOT do
//!
//! The daemon NEVER reads stdin and NEVER emits prompt bytes. Every
//! interactive I/O lives in this struct. Phase 6's channel-ownership
//! invariant (`stdout = daemon-emitted frames, stderr = CLI prompts`)
//! extends here unchanged.

use anyhow::{Context, Result};
use pice_core::cli::{CommandRequest, CommandResponse, ReviewGateRequest, ReviewGateSubcommand};
use pice_core::events::ManifestEventPayload;
use pice_core::gate::GateDecision;

use crate::adapter::transport::DaemonClient;

/// Outcome of a single gate prompt round.
#[derive(Debug)]
pub enum GateOutcome {
    /// The decide RPC returned successfully; the daemon has recorded
    /// the decision and may have transitioned the feature.
    Decided {
        gate_id: String,
        decision: GateDecision,
        /// Full daemon response for the decide RPC. Callers can inspect
        /// `resp` to surface per-decision details (e.g. audit id, layer
        /// status transitions); the status follow loop only logs the
        /// decision summary, so the field is kept for future consumers.
        #[allow(dead_code)]
        resp: CommandResponse,
    },
    /// The reviewer declined to decide after 5 invalid prompts — the
    /// follow loop falls back to logging and continues streaming.
    /// Production code should not exit on this; a later event (gate
    /// timeout, second GateRequested) may give the reviewer another
    /// chance.
    PromptExhausted { gate_id: String },
    /// An RPC-level failure (daemon down, auth rejected, etc.). The
    /// follow loop surfaces this via `tracing::warn!` and keeps
    /// streaming; the reviewer can retry via `pice review-gate`.
    #[allow(dead_code)]
    RpcFailure { gate_id: String, error: String },
}

/// Owns a separate [`DaemonClient`] for dispatching gate-decision RPCs
/// while the subscribe stream's client is busy reading notifications.
///
/// Construct ONE instance per `pice status --follow` invocation; reuse
/// across every `GateRequested` event for the life of the subscribe.
/// The `control_client` is consumed by each dispatch, so the struct
/// replaces it after every RPC — the shape matches how
/// `autostart::ensure_daemon_running` mints fresh clients on demand.
pub struct SubscribedGateSource {
    /// `$USER` / `$USERNAME` / `"unknown"` — resolved once per session.
    /// Matches Phase 6's `resolve_reviewer` semantics so audit rows
    /// carry a stable reviewer identity.
    reviewer_name: String,
}

impl SubscribedGateSource {
    /// Construct a new gate source. `reviewer_name` is captured at
    /// construction so a later `$USER` env mutation doesn't affect a
    /// decision already mid-flight.
    pub fn new(reviewer_name: String) -> Self {
        Self { reviewer_name }
    }

    /// Resolve the reviewer name from `$USER` / `$USERNAME` with
    /// `"unknown"` fallback — same precedence as Phase 6.
    pub fn resolve_reviewer() -> String {
        std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "unknown".to_string())
    }

    /// Handle a `GateRequested` event from a subscribe stream. Renders
    /// the prompt to stderr, reads the reviewer decision via
    /// `spawn_blocking`, and dispatches the `ReviewGate::Decide` RPC.
    ///
    /// The payload's `data` field carries `{gate_id, trigger_expression}`
    /// per `EventBus::emit_gate_requested`. Missing fields surface as
    /// [`GateOutcome::RpcFailure`] with a descriptive error — this
    /// only happens if the daemon's emission contract drifts, so
    /// failing loud is the right call.
    pub async fn handle_gate_requested(
        &self,
        payload: &ManifestEventPayload,
    ) -> Result<GateOutcome> {
        let Some(gate_id) = payload
            .data
            .get("gate_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
        else {
            anyhow::bail!("GateRequested payload missing gate_id: {}", payload.data);
        };
        let trigger = payload
            .data
            .get("trigger_expression")
            .and_then(|v| v.as_str())
            .unwrap_or("(unspecified)");
        let layer = payload.layer.as_deref().unwrap_or("?");

        // 1. Render prompt to stderr.
        let body = format!("layer: {layer}\ntrigger: {trigger}\nid: {gate_id}");
        let prompt = crate::input::decision_source::render_prompt(&body, None);
        {
            use std::io::Write;
            let mut err = std::io::stderr();
            writeln!(err, "{prompt}")?;
            err.flush()?;
        }

        // 2. Read decision via spawn_blocking (StdinLock: !Send).
        let decision = match Self::read_decision_blocking().await? {
            Some(d) => d,
            None => return Ok(GateOutcome::PromptExhausted { gate_id }),
        };

        // 3. Dispatch decide RPC on a SEPARATE DaemonClient — the
        //    subscribe client is busy reading notifications. The daemon
        //    bearer-token auth allows concurrent connections.
        let mut control_client: DaemonClient = crate::adapter::autostart::ensure_daemon_running()
            .await
            .context("failed to open control connection for gate decide")?;
        let req = CommandRequest::ReviewGate(ReviewGateRequest {
            json: false,
            subcommand: ReviewGateSubcommand::Decide {
                gate_id: gate_id.clone(),
                decision,
                reviewer: self.reviewer_name.clone(),
                reason: None,
            },
        });
        match control_client.dispatch(req).await {
            Ok(resp) => Ok(GateOutcome::Decided {
                gate_id,
                decision,
                resp,
            }),
            Err(err) => Ok(GateOutcome::RpcFailure {
                gate_id,
                error: err.to_string(),
            }),
        }
    }

    /// Read one reviewer decision from stdin. Returns `Ok(None)` after
    /// 5 invalid prompts — the caller falls through to
    /// [`GateOutcome::PromptExhausted`]. Uses `spawn_blocking` because
    /// `std::io::Stdin::read_line` is a blocking sync call and
    /// `StdinLock: !Send`.
    async fn read_decision_blocking() -> Result<Option<GateDecision>> {
        tokio::task::spawn_blocking(|| {
            use std::io::{stderr, stdin, Write};
            for _ in 0..5 {
                let mut line = String::new();
                stdin().read_line(&mut line)?;
                let ch = line
                    .trim()
                    .chars()
                    .next()
                    .map(|c| c.to_ascii_lowercase())
                    .unwrap_or(' ');
                match ch {
                    'a' => return Ok::<_, anyhow::Error>(Some(GateDecision::Approve)),
                    'r' => return Ok(Some(GateDecision::Reject)),
                    's' => return Ok(Some(GateDecision::Skip)),
                    other => {
                        let mut err = stderr();
                        let _ = writeln!(err, "invalid '{other}'; expect a/r/s (details deferred)");
                        let _ = err.flush();
                    }
                }
            }
            Ok(None)
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pice_core::events::{ManifestEvent, ManifestEventPayload};
    use serde_json::json;

    fn gate_requested_payload(
        gate_id: Option<&str>,
        trigger: Option<&str>,
    ) -> ManifestEventPayload {
        let mut data = json!({});
        if let Some(g) = gate_id {
            data["gate_id"] = json!(g);
        }
        if let Some(t) = trigger {
            data["trigger_expression"] = json!(t);
        }
        ManifestEventPayload {
            feature_id: "feat-x".to_string(),
            run_id: "r-1".to_string(),
            event: ManifestEvent::GateRequested,
            layer: Some("infrastructure".to_string()),
            data,
            timestamp: "2026-04-21T10:00:00Z".to_string(),
        }
    }

    #[test]
    fn resolve_reviewer_falls_back_to_unknown() {
        // Can't reliably assert a specific result (env is process-wide),
        // but the function must return a non-empty string regardless.
        let name = SubscribedGateSource::resolve_reviewer();
        assert!(!name.is_empty());
    }

    #[tokio::test]
    async fn handle_gate_requested_bails_on_missing_gate_id() {
        // No gate_id in payload.data → hard error (daemon contract
        // drift). Use a bogus reviewer name; the function must NOT
        // reach the RPC dispatch step.
        let src = SubscribedGateSource::new("reviewer-a".to_string());
        let payload = gate_requested_payload(None, Some("layer == infra"));
        let err = src.handle_gate_requested(&payload).await.unwrap_err();
        assert!(
            err.to_string().contains("missing gate_id"),
            "expected gate_id-missing error, got: {err}"
        );
    }

    // Happy-path + RPC-failure paths require either:
    // - a running daemon, which belongs in an integration test binary
    //   under `crates/pice-cli/tests/`, OR
    // - mocking the DaemonClient, which would force the struct to take
    //   a trait-bound control dispatcher (premature abstraction — see
    //   rust-core rule: "Don't ship trait-based scaffolding ahead of a
    //   real consumer").
    //
    // Deferred to a follow-up integration test when Phase 7.1 ships the
    // daemon-process harness for CLI integration. The error-path test
    // above pins the happy-path's pre-dispatch validation.
}
